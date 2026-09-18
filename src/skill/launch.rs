//! Turning a skill and a harness into a spawnable profile: the harness's
//! own command line, the skill invocation as the opening prompt, and the
//! environment the skill's instructions rely on.

use super::render::invocation;
use super::{Hydration, SkillDefinition};
use crate::config::Profile;
use crate::harness::Harness;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// The built-in sentence appended to the opening prompt when a briefing
/// snapshot is written for the launch; `[skill] hydration_hint` in the
/// library's prompts.toml replaces it. It names the environment variables
/// rather than a path so a restored session (whose prompt is saved) still
/// resolves.
pub const HYDRATION_HINT: &str = "Read the briefing snapshot at $AGENT_MUX_BRIEFING (taken at $AGENT_MUX_BRIEFING_AS_OF, schema in $AGENT_MUX_BRIEFING_SCHEMA) before running any command; answer your default report from it and use tools only for newer or narrower questions.";

/// The result of writing a launch's briefing snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hydrated {
    pub path: PathBuf,
    pub as_of: String,
    pub schema_version: u32,
    /// False when the file holds a typed error envelope instead of data.
    pub ok: bool,
    pub error: Option<String>,
}

/// Where launch snapshots live under the runtime directory.
pub fn briefings_dir(runtime_dir: &Path) -> PathBuf {
    runtime_dir.join("briefings")
}

fn now_rfc3339() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

/// Computes every snapshot the package asks for and writes it to
/// `<runtime_dir>/briefings/<key>.json` (temp file + rename, owner-only).
/// A failure never blocks the launch: the file then holds
/// `{"schema_version":1,"as_of":…,"error":{"code","message"}}` or
/// `{"schema_version":2,"as_of":…,"error":{"code","message"}}`.
/// Returns `None` when the package asks for nothing.
pub fn hydrate(
    def: &SkillDefinition,
    trace_db: Option<&Path>,
    cwd: &Path,
    home: &Path,
    runtime_dir: &Path,
    key: &str,
) -> Option<Hydrated> {
    let wants_briefing = def.hydrate.contains(&Hydration::Briefing);
    let wants_dossier = def.hydrate.contains(&Hydration::Dossier);
    if !wants_briefing && !wants_dossier {
        return None;
    }

    let schema_version = if wants_dossier { 2 } else { 1 };
    let now = now_rfc3339();
    let error_envelope = |code: &str, message: &str, schema: u32| {
        serde_json::json!({
            "schema_version": schema,
            "as_of": now,
            "error": { "code": code, "message": message },
        })
    };
    let (value, ok, error, as_of) = match trace_db {
        None => {
            let m = "tracing is off; there is no trace store to brief from";
            (
                error_envelope("DB_UNAVAILABLE", m, schema_version),
                false,
                Some(m.to_string()),
                now.clone(),
            )
        }
        Some(db) => {
            if wants_dossier {
                match crate::tracing::store::open_ro(db) {
                    Err(err) => (
                        error_envelope("DB_UNAVAILABLE", &err, 2),
                        false,
                        Some(err),
                        now.clone(),
                    ),
                    Ok(conn) => {
                        let as_of_dt = time::OffsetDateTime::now_utc();
                        let as_of_str = as_of_dt
                            .format(&time::format_description::well_known::Rfc3339)
                            .unwrap_or_else(|_| now.clone());
                        let now_ns = as_of_dt.unix_timestamp_nanos() as i64;
                        let snap_dir = if runtime_dir.join("snapshots").exists() {
                            runtime_dir.join("snapshots")
                        } else if runtime_dir.exists() {
                            runtime_dir.to_path_buf()
                        } else {
                            crate::tracing::analysis::default_snapshot_dir()
                        };
                        let (live_sessions, live_available) =
                            match crate::tracing::analysis::read_snapshots(&snap_dir, now_ns) {
                                Ok(snaps) => {
                                    let avail = !snaps.is_empty();
                                    (
                                        snaps.into_iter().flat_map(|s| s.sessions).collect::<Vec<_>>(),
                                        avail,
                                    )
                                }
                                Err(_) => (Vec::new(), false),
                            };
                        let inputs = crate::tracing::analysis::dossier::DossierInputs {
                            conn: &conn,
                            db_path: db,
                            workspace: cwd,
                            home,
                            live_sessions: &live_sessions,
                            live_snapshot_available: live_available,
                            as_of: as_of_dt,
                        };
                        match crate::tracing::analysis::dossier::build_dossier(
                            inputs,
                            crate::tracing::analysis::dossier::DossierConfig::default(),
                        ) {
                            Ok(dossier) => (
                                serde_json::to_value(&dossier).unwrap_or(serde_json::Value::Null),
                                true,
                                None,
                                as_of_str,
                            ),
                            Err(e) => {
                                let code = match &e {
                                    crate::tracing::analysis::dossier::DossierBuildError::TooLarge(_) => {
                                        "DOSSIER_TOO_LARGE"
                                    }
                                    crate::tracing::analysis::dossier::DossierBuildError::SchemaUnsupported(_) => {
                                        "SCHEMA_UNSUPPORTED"
                                    }
                                    _ => "BUILD_FAILED",
                                };
                                (
                                    error_envelope(code, &e.to_string(), 2),
                                    false,
                                    Some(e.to_string()),
                                    as_of_str,
                                )
                            }
                        }
                    }
                }
            } else {
                use crate::tracing::analysis::{
                    BriefingArgs, Request, Scope, ServiceConfig, TraceService,
                };
                let scope = Scope::workspace(cwd).unwrap_or_else(|_| Scope::all_workspaces());
                let outcome = TraceService::new(ServiceConfig::new(db.to_path_buf(), scope))
                    .and_then(|svc| svc.execute(Request::Briefing(BriefingArgs::default())));
                match outcome {
                    Ok(envelope) => {
                        let as_of = envelope.as_of.clone();
                        (
                            serde_json::to_value(&envelope).unwrap_or(serde_json::Value::Null),
                            true,
                            None,
                            as_of,
                        )
                    }
                    Err(e) => (
                        error_envelope(e.code(), &e.to_string(), 1),
                        false,
                        Some(e.to_string()),
                        now.clone(),
                    ),
                }
            }
        }
    };
    let dir = briefings_dir(runtime_dir);
    let path = dir.join(format!("{key}.json"));
    if let Err(e) = write_private(&dir, &path, &value) {
        return Some(Hydrated {
            path,
            as_of,
            schema_version,
            ok: false,
            error: Some(format!("cannot write briefing snapshot: {e}")),
        });
    }
    Some(Hydrated {
        path,
        as_of,
        schema_version,
        ok,
        error,
    })
}

fn write_private(dir: &Path, path: &Path, value: &serde_json::Value) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    }
    let tmp = dir.join(format!(
        ".{}.tmp-{}",
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("snapshot"),
        std::process::id()
    ));
    let text = serde_json::to_string_pretty(value).unwrap_or_else(|_| "{}".into());
    std::fs::write(&tmp, text)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(&tmp, path)
}

/// Removes snapshot files older than `max_age`; returns how many.
pub fn sweep_briefings(runtime_dir: &Path, max_age: Duration) -> usize {
    let dir = briefings_dir(runtime_dir);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return 0;
    };
    let now = SystemTime::now();
    let mut removed = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        let stale = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|m| now.duration_since(m).ok())
            .is_some_and(|age| age > max_age);
        if stale && std::fs::remove_file(&path).is_ok() {
            removed += 1;
        }
    }
    removed
}

#[derive(Debug, Clone)]
pub struct SkillLaunch {
    pub profile: Profile,
    pub cwd: PathBuf,
    pub env: Vec<(String, String)>,
}

#[derive(Debug)]
pub enum LaunchError {
    UnsupportedHarness(Harness),
}

impl std::fmt::Display for LaunchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LaunchError::UnsupportedHarness(h) => {
                write!(f, "harness '{h}' is not supported by this skill")
            }
        }
    }
}

impl std::error::Error for LaunchError {}

/// The first message of the session: the harness's invocation syntax for
/// the skill, followed by the package's startup prompt.
pub fn opening_prompt(def: &SkillDefinition, harness: Harness) -> String {
    match &def.startup_prompt {
        Some(p) => format!("{} {}", invocation(&def.id, harness), p.trim()),
        None => invocation(&def.id, harness),
    }
}

/// Session name shown in the sidebar and recorded on the trace launch.
pub fn session_name(def: &SkillDefinition, harness: Harness) -> String {
    format!("{} ({})", def.name, harness.as_str())
}

/// Composes the argv for `harness`, verified against the installed CLIs:
/// Claude Code and Codex take the prompt as the last positional argument,
/// Antigravity through `--prompt-interactive`. Model and permission
/// settings come from the base profile.
pub fn build_skill_launch(
    def: &SkillDefinition,
    harness: Harness,
    base: &Profile,
    cwd: &Path,
) -> Result<SkillLaunch, LaunchError> {
    build_skill_launch_with_db(def, harness, base, cwd, None)
}

/// Like [`build_skill_launch`], but hands the skill the trace store at
/// `trace_db` (the one the running TUI writes, which honours a configured
/// `[tracing] db_path`). `None` falls back to the environment/home default.
pub fn build_skill_launch_with_db(
    def: &SkillDefinition,
    harness: Harness,
    base: &Profile,
    cwd: &Path,
    trace_db: Option<&Path>,
) -> Result<SkillLaunch, LaunchError> {
    build_skill_launch_full(def, harness, base, cwd, trace_db, !def.hydrate.is_empty())
}

/// Like [`build_skill_launch_with_db`]; `hydrated` appends the snapshot
/// pointer sentence to the opening prompt (the app passes whether it will
/// actually write one).
pub fn build_skill_launch_full(
    def: &SkillDefinition,
    harness: Harness,
    base: &Profile,
    cwd: &Path,
    trace_db: Option<&Path>,
    hydrated: bool,
) -> Result<SkillLaunch, LaunchError> {
    if !def.harnesses.contains(&harness) {
        return Err(LaunchError::UnsupportedHarness(harness));
    }
    let mut prompt = opening_prompt(def, harness);
    if hydrated {
        prompt.push(' ');
        prompt.push_str(&crate::prompts::Prompts::current().hydration_hint);
    }
    let mut args: Vec<String> = Vec::new();
    if let Some(m) = base
        .model
        .as_deref()
        .map(str::trim)
        .filter(|m| !m.is_empty())
    {
        args.push("--model".into());
        args.push(m.to_string());
    }
    // `auto_approve` in skill.toml is the package's own demand, so it wins
    // over a base profile that leaves approvals on: the flags below are the
    // three CLIs' spellings of the same thing (checked against `--help` on
    // claude 2.1.274, codex 0.154.0 and agy 1.2.4).
    let bypass = def.auto_approve || base.bypass_approvals.unwrap_or(false);
    match harness {
        Harness::Claude => {
            if bypass {
                args.push("--dangerously-skip-permissions".into());
            }
            args.push(prompt);
        }
        Harness::Codex => {
            if bypass {
                args.push("--yolo".into());
            }
            args.push(prompt);
        }
        Harness::Antigravity => {
            if bypass {
                args.push("--dangerously-skip-permissions".into());
            }
            args.push("--prompt-interactive".into());
            args.push(prompt);
        }
    }
    let mut profile = base.clone();
    profile.name = session_name(def, harness);
    // A configured profile whose command already resolves to this harness
    // (a wrapper path such as `~/bin/claude`) is kept; otherwise the bare
    // executable name is looked up on PATH.
    if Harness::detect(&base.command) != Some(harness) {
        profile.command = harness.as_str().to_string();
    }
    profile.args = args;

    let executable = std::env::var("AGENT_MUX_BIN")
        .map(PathBuf::from)
        .or_else(|_| std::env::current_exe())
        .unwrap_or_else(|_| PathBuf::from("agent-mux"));
    let env = vec![
        ("AGENT_MUX_SKILL_ID".to_string(), def.id.clone()),
        (
            "AGENT_MUX_BIN".to_string(),
            executable.to_string_lossy().into_owned(),
        ),
        (
            "AGENT_MUX_TRACE_DB".to_string(),
            trace_db
                .map(Path::to_path_buf)
                .unwrap_or_else(crate::tracing::analysis::default_trace_db_path)
                .to_string_lossy()
                .into_owned(),
        ),
    ];
    Ok(SkillLaunch {
        profile,
        cwd: base
            .default_dir
            .as_ref()
            .map(PathBuf::from)
            .filter(|d| d.is_dir())
            .unwrap_or_else(|| cwd.to_path_buf()),
        env,
    })
}
