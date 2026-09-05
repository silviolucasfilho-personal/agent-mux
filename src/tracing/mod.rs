//! Local session tracing into a SQLite store.
//!
//! Spec: `docs/superpowers/specs/2026-09-03-sqlite-trace-store-design.md`.
//!
//! `TraceRuntime` is constructed once at startup when the `[tracing]`
//! section resolves and the store opens. Per launch, `plan_launch` merges
//! the profile's overrides and decides correlation (Claude: injected
//! `--session-id`; Codex/Antigravity: transcript watch; resumes: known
//! ids). `start_session` records the launch row and spawns the per-session
//! pipeline task, which correlates, tails the live transcript, assembles
//! turns, and feeds the one writer thread. Everything is fail-open: no
//! failure here may break, block, or slow a session.

pub mod agy_usage;
pub mod cli;
pub mod correlate;
pub mod experiments;
pub mod hooks;
pub mod ids;
pub mod inventory;
pub mod langfuse;
pub mod loops;
pub mod map;
pub mod pricing;
pub mod store;
pub mod tail;
pub mod usage;
pub mod view;

use crate::config::{Backend, ContentMode, Profile, ResolvedTracing};
use crate::events::AppEvent;
use crate::transcript::Provider;
use correlate::{Adopted, ClaimRegistry, CorrelationSpec};
use hooks::feed::HookFeed;
use langfuse::ExporterHandle;
use map::{MapSettings, SessionEnd, TurnAssembler};
use pricing::PriceTable;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};
use store::model::StoreOp;
use store::writer::{WriterConfig, WriterHandle, spawn_writer};
use tail::{TailPoll, Tailer};
use tokio::sync::watch;

/// Session phase as the App reports it into the pipeline. Duplicate
/// `mark_exited` calls (the documented duplicate-PtyExit case) just re-send
/// the same value — naturally idempotent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Running,
    Exited(Option<u32>),
    Stopped,
}

/// Held by `Session`; dropping the Session closes the phase channel, which
/// the pipeline treats like an exit.
#[derive(Debug)]
pub struct SessionTraceHandle {
    phase: watch::Sender<Phase>,
    /// The launch this handle traces — the join key for live stats.
    pub launch_id: String,
    /// Where the launch's rows go (drives the badge glyph).
    pub backend: Backend,
}

impl SessionTraceHandle {
    pub fn mark_exited(&self, exit_code: Option<u32>) {
        let _ = self.phase.send(Phase::Exited(exit_code));
    }

    pub fn mark_stopped(&self) {
        let _ = self.phase.send(Phase::Stopped);
    }
}

/// Everything decided before spawn for one launch.
pub struct LaunchPlan {
    pub launch_id: String,
    pub extra_args: Vec<String>,
    pub extra_env: Vec<(String, String)>,
    provider: Provider,
    content_mode: ContentMode,
    correlation: CorrelationSpec,
    /// "deterministic" | "watched" | "none"
    correlation_label: &'static str,
    /// Session id already known at spawn (Claude injection/resume, agy
    /// `--conversation`).
    known_session_id: Option<String>,
    /// True when this launch had `--session-id` injected (drives the
    /// fast-failure hint).
    injected: bool,
    /// True when attached to an already-running session (the `t` toggle).
    attached: bool,
    /// True when a per-launch hook registration was added to the args.
    hooks_registered: bool,
    /// Where this launch's rows go.
    pub backend: Backend,
    /// The backend the profile or dialog asked for when it could not be
    /// honored (Langfuse not configured): recorded on the launch row.
    pub backend_requested: Option<Backend>,
    profile_name: String,
    dir: PathBuf,
}

pub struct TraceRuntime {
    settings: ResolvedTracing,
    claims: Arc<ClaimRegistry>,
    writer: WriterHandle,
    /// The Langfuse sink, spawned only when credentials resolve.
    exporter: Option<ExporterHandle>,
    run_id: String,
    shutdown_tx: watch::Sender<bool>,
    pipelines: Vec<tokio::task::JoinHandle<()>>,
    status_tx: tokio::sync::mpsc::Sender<AppEvent>,
    /// Profiles whose `--session-id` injection got disabled for the run
    /// after repeated fast failures.
    injection_disabled: Arc<Mutex<HashSet<String>>>,
    /// Consecutive injected-launch fast failures per profile.
    fast_failures: Arc<Mutex<std::collections::HashMap<String, u32>>>,
    /// Once-per-run latch for the dropped-ops warning, shared between the
    /// writer's status sink and the pipelines' queue-full paths.
    drop_warned: Arc<std::sync::atomic::AtomicBool>,
}

/// Which sinks an op reaches under `backend`: (local store, Langfuse).
/// Launch rows always stay local.
pub fn sink_targets(op: &StoreOp, backend: Backend) -> (bool, bool) {
    match op {
        StoreOp::Launch(_) => (true, false),
        _ => (backend.local(), backend.langfuse()),
    }
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

fn now_nanos() -> i128 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos() as i128)
        .unwrap_or(0)
}

fn now_ns() -> i64 {
    store::now_ns()
}

/// Extracts the value of `flag` from an args list — both the two-token form
/// (`--resume <id>`) and, for long flags, the equals form (`--resume=<id>`).
fn arg_value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    if let Some(at) = args.iter().position(|a| a == flag) {
        let value = args.get(at + 1)?;
        return (!value.starts_with('-')).then_some(value.as_str());
    }
    if flag.starts_with("--") {
        return args
            .iter()
            .find_map(|a| a.strip_prefix(flag).and_then(|rest| rest.strip_prefix('=')))
            .filter(|v| !v.is_empty());
    }
    None
}

/// True when `arg` is `flag` itself or its `--flag=value` form.
fn matches_flag(arg: &str, flag: &str) -> bool {
    arg == flag
        || (flag.starts_with("--")
            && arg
                .strip_prefix(flag)
                .is_some_and(|rest| rest.starts_with('=')))
}

fn basename(command: &str) -> &str {
    Path::new(command)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(command)
}

/// The price table in effect for a resolved configuration.
pub fn price_table(settings: &ResolvedTracing) -> PriceTable {
    PriceTable::builtin().with_overrides(&settings.models)
}

impl TraceRuntime {
    /// Opens the store and starts the writer thread. `Err` carries a
    /// status-bar message; the caller runs untraced.
    pub fn new(
        settings: ResolvedTracing,
        status_tx: tokio::sync::mpsc::Sender<AppEvent>,
    ) -> Result<TraceRuntime, String> {
        let run_id = uuid::Uuid::new_v4().to_string();
        let store = store::open_rw(
            &settings.db_path,
            store::OpenOptions {
                prices: price_table(&settings),
                run_id: run_id.clone(),
                retention_days: settings.retention_days,
                agent_mux_version: env!("CARGO_PKG_VERSION").to_string(),
            },
        )?;
        let status_for_writer = status_tx.clone();
        let drop_warned = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let drop_warned_writer = Arc::clone(&drop_warned);
        let stats_tx = status_tx.clone();
        let mut last_stats: std::collections::HashMap<String, Instant> =
            std::collections::HashMap::new();
        let writer = spawn_writer(
            store,
            WriterConfig::new(settings.flush_interval_ms),
            Box::new(move |class, message| {
                if class == "dropped" && drop_warned_writer.swap(true, Ordering::Relaxed) {
                    return;
                }
                let _ = status_for_writer.try_send(AppEvent::TraceStatus(message));
            }),
            Some(Box::new(move |store, launches| {
                // live badges: at most one stats event per launch per second
                for launch_id in launches {
                    let due = last_stats
                        .get(launch_id)
                        .is_none_or(|t| t.elapsed() >= Duration::from_secs(1));
                    if !due {
                        continue;
                    }
                    if let Ok(stats) = store::query::launch_stats(store.conn(), launch_id) {
                        last_stats.insert(launch_id.clone(), Instant::now());
                        let _ = stats_tx.try_send(AppEvent::TraceStats {
                            launch_id: launch_id.clone(),
                            stats,
                        });
                    }
                }
            })),
        );
        let exporter = settings.langfuse.as_ref().map(|lf| {
            let status_for_exporter = status_tx.clone();
            let stats_tx = status_tx.clone();
            let mut last_stats: std::collections::HashMap<String, Instant> =
                std::collections::HashMap::new();
            langfuse::spawn_exporter(
                langfuse::ExporterConfig::new(lf),
                langfuse::map::MapCtx::from_settings(&settings),
                Box::new(move |_class, message| {
                    let _ = status_for_exporter.try_send(AppEvent::TraceStatus(message));
                }),
                Box::new(move |launch_id, stats| {
                    let due = last_stats
                        .get(launch_id)
                        .is_none_or(|t| t.elapsed() >= Duration::from_secs(1));
                    if due {
                        last_stats.insert(launch_id.to_string(), Instant::now());
                        let _ = stats_tx.try_send(AppEvent::TraceStats {
                            launch_id: launch_id.to_string(),
                            stats,
                        });
                    }
                }),
            )
        });
        Ok(TraceRuntime {
            settings,
            claims: Arc::new(Mutex::new(HashSet::new())),
            writer,
            exporter,
            run_id,
            shutdown_tx: watch::Sender::new(false),
            pipelines: Vec::new(),
            status_tx,
            injection_disabled: Arc::new(Mutex::new(HashSet::new())),
            fast_failures: Arc::new(Mutex::new(std::collections::HashMap::new())),
            drop_warned,
        })
    }

    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    pub fn db_path(&self) -> &Path {
        &self.settings.db_path
    }

    pub fn home(&self) -> &Path {
        &self.settings.home
    }

    /// True when Langfuse credentials resolved and the exporter is running.
    pub fn langfuse_configured(&self) -> bool {
        self.exporter.is_some()
    }

    /// The configured default destination.
    pub fn default_backend(&self) -> Backend {
        self.settings.backend
    }

    /// The destination for a launch: the profile's choice, else the global
    /// default, downgraded to local (with a one-line notice) when Langfuse
    /// is not configured. Returns the backend and, when downgraded, what
    /// was asked for.
    fn resolve_backend(&self, profile: &Profile) -> (Backend, Option<Backend>) {
        let wanted = profile
            .tracing
            .as_ref()
            .and_then(|o| o.backend.as_deref())
            .and_then(Backend::parse)
            .unwrap_or(self.settings.backend);
        if wanted.langfuse() && self.exporter.is_none() {
            let _ = self.status_tx.try_send(AppEvent::TraceStatus(format!(
                "tracing: Langfuse is not configured — '{}' is traced locally \
                 (set [tracing.langfuse] or $LANGFUSE_* keys; see `agent-mux trace doctor`)",
                profile.name
            )));
            return (Backend::Local, Some(wanted));
        }
        (wanted, None)
    }

    pub fn claude_dir(&self) -> Option<PathBuf> {
        self.settings
            .claude_dir
            .clone()
            .or_else(|| std::env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from))
            .or_else(|| home_dir().map(|h| h.join(".claude")))
    }

    fn codex_dir(&self) -> Option<PathBuf> {
        self.settings
            .codex_dir
            .clone()
            .or_else(|| home_dir().map(|h| h.join(".codex")))
    }

    fn antigravity_root(&self) -> Option<PathBuf> {
        self.settings
            .antigravity_dir
            .clone()
            .or_else(crate::history::default_antigravity_root)
    }

    /// Decides how (and whether) to trace one launch. `None` = fully
    /// untraced: no extras, no markers, no pipeline, no launch row.
    pub fn plan_launch(&self, profile: &Profile, dir: &Path) -> Option<LaunchPlan> {
        self.plan_launch_opt(profile, dir, false)
    }

    /// Explicit/dynamic trace plan (e.g. user toggled tracing on via keybinding `t`).
    /// Ignores `enabled = false` in profile override, but respects `provider = "none"`.
    /// Attaches to an already-running session (no extra CLI args injected).
    pub fn plan_attach(&self, profile: &Profile, dir: &Path) -> Option<LaunchPlan> {
        let over = profile.tracing.as_ref();
        let provider = match over.and_then(|o| o.provider.as_deref()) {
            Some("claude") => Provider::Claude,
            Some("codex") => Provider::Codex,
            Some("antigravity") => Provider::Antigravity,
            Some("none") => return None,
            Some(_) => return None,
            None => match basename(&profile.command) {
                "claude" => Provider::Claude,
                "codex" => Provider::Codex,
                "agy" => Provider::Antigravity,
                _ => return None,
            },
        };
        let content_mode = match over.and_then(|o| o.content_mode.as_deref()) {
            Some("full") => ContentMode::Full,
            Some("metadata") => ContentMode::Metadata,
            _ => self.settings.content_mode,
        };
        let launch_id = uuid::Uuid::new_v4().to_string();
        let mut known_session_id: Option<String> = None;
        let t0 = SystemTime::now()
            .checked_sub(Duration::from_secs(3600))
            .unwrap_or(SystemTime::UNIX_EPOCH);

        let (correlation, correlation_label) = match provider {
            Provider::Claude => {
                let claude_dir = self.claude_dir()?;
                let projects_dir = claude_dir.join("projects");
                let args = &profile.args;
                if let Some(id) = arg_value(args, "--resume")
                    .or_else(|| arg_value(args, "-r"))
                    .or_else(|| arg_value(args, "--session-id"))
                {
                    known_session_id = Some(id.to_string());
                    let expected_path = projects_dir
                        .join(crate::history::project_slug(dir))
                        .join(format!("{id}.jsonl"));
                    (
                        CorrelationSpec::KnownClaude {
                            session_id: id.to_string(),
                            expected_path,
                            projects_dir,
                            resume: true,
                        },
                        "deterministic",
                    )
                } else {
                    (
                        CorrelationSpec::WatchClaude {
                            projects_dir,
                            cwd: dir.to_path_buf(),
                            t0,
                        },
                        "watched",
                    )
                }
            }
            Provider::Codex => {
                let sessions_dir = self.codex_dir()?.join("sessions");
                (
                    CorrelationSpec::WatchCodex {
                        sessions_dir,
                        cwd: dir.to_path_buf(),
                        t0,
                    },
                    "watched",
                )
            }
            Provider::Antigravity => {
                let root = self.antigravity_root()?;
                if let Some(id) = arg_value(&profile.args, "--conversation") {
                    known_session_id = Some(id.to_string());
                    (
                        CorrelationSpec::KnownAntigravity {
                            conversation_id: id.to_string(),
                            root,
                        },
                        "deterministic",
                    )
                } else {
                    (
                        CorrelationSpec::WatchAntigravity {
                            root,
                            cwd: dir.to_path_buf(),
                            t0,
                            initial: None,
                        },
                        "watched",
                    )
                }
            }
        };

        let (backend, backend_requested) = self.resolve_backend(profile);
        Some(LaunchPlan {
            extra_env: Vec::new(),
            launch_id,
            extra_args: Vec::new(),
            provider,
            content_mode,
            correlation,
            correlation_label,
            known_session_id,
            injected: false,
            attached: true,
            hooks_registered: false,
            backend,
            backend_requested,
            profile_name: profile.name.clone(),
            dir: dir.to_path_buf(),
        })
    }

    pub fn plan_launch_opt(
        &self,
        profile: &Profile,
        dir: &Path,
        force_enabled: bool,
    ) -> Option<LaunchPlan> {
        let over = profile.tracing.as_ref();
        if !force_enabled && !over.and_then(|o| o.enabled).unwrap_or(true) {
            return None;
        }
        let provider = match over.and_then(|o| o.provider.as_deref()) {
            Some("claude") => Provider::Claude,
            Some("codex") => Provider::Codex,
            Some("antigravity") => Provider::Antigravity,
            Some("none") => return None,
            Some(_) => return None, // unknown override: fail safe, untraced
            None => match basename(&profile.command) {
                "claude" => Provider::Claude,
                "codex" => Provider::Codex,
                "agy" => Provider::Antigravity,
                _ => return None,
            },
        };
        let content_mode = match over.and_then(|o| o.content_mode.as_deref()) {
            Some("full") => ContentMode::Full,
            Some("metadata") => ContentMode::Metadata,
            _ => self.settings.content_mode,
        };
        let launch_id = uuid::Uuid::new_v4().to_string();
        let mut extra_args: Vec<String> = Vec::new();
        let mut injected = false;
        let mut known_session_id: Option<String> = None;

        let (correlation, correlation_label) = match provider {
            Provider::Claude => {
                let claude_dir = self.claude_dir()?;
                let projects_dir = claude_dir.join("projects");
                let expected = |id: &str| {
                    projects_dir
                        .join(crate::history::project_slug(dir))
                        .join(format!("{id}.jsonl"))
                };
                let args = &profile.args;
                let skip = args.iter().any(|a| {
                    ["--resume", "--session-id", "--continue", "--print"]
                        .iter()
                        .any(|flag| matches_flag(a, flag))
                        || matches!(a.as_str(), "-r" | "-c" | "-p")
                });
                let inject_allowed = over.and_then(|o| o.inject_session_id).unwrap_or(true)
                    && !self
                        .injection_disabled
                        .lock()
                        .map(|s| s.contains(&profile.name))
                        .unwrap_or(false);
                if let Some(id) = arg_value(args, "--resume")
                    .or_else(|| arg_value(args, "-r"))
                    .or_else(|| arg_value(args, "--session-id"))
                {
                    known_session_id = Some(id.to_string());
                    (
                        CorrelationSpec::KnownClaude {
                            session_id: id.to_string(),
                            expected_path: expected(id),
                            projects_dir,
                            resume: true,
                        },
                        "deterministic",
                    )
                } else if skip || !inject_allowed {
                    (CorrelationSpec::None, "none")
                } else {
                    let id = uuid::Uuid::new_v4().to_string();
                    extra_args = vec!["--session-id".into(), id.clone()];
                    injected = true;
                    known_session_id = Some(id.clone());
                    let expected_path = expected(&id);
                    (
                        CorrelationSpec::KnownClaude {
                            session_id: id,
                            expected_path,
                            projects_dir,
                            resume: false,
                        },
                        "deterministic",
                    )
                }
            }
            Provider::Codex => {
                let sessions_dir = self.codex_dir()?.join("sessions");
                (
                    CorrelationSpec::WatchCodex {
                        sessions_dir,
                        cwd: dir.to_path_buf(),
                        t0: SystemTime::now(),
                    },
                    "watched",
                )
            }
            Provider::Antigravity => {
                let root = self.antigravity_root()?;
                if let Some(id) = arg_value(&profile.args, "--conversation") {
                    known_session_id = Some(id.to_string());
                    (
                        CorrelationSpec::KnownAntigravity {
                            conversation_id: id.to_string(),
                            root,
                        },
                        "deterministic",
                    )
                } else {
                    (
                        CorrelationSpec::WatchAntigravity {
                            root,
                            cwd: dir.to_path_buf(),
                            t0: SystemTime::now(),
                            initial: None,
                        },
                        "watched",
                    )
                }
            }
        };

        // Per-launch hook registrations: nothing is written anywhere, and a
        // profile that already failed fast twice gets none (an old binary
        // rejecting unknown flags looks exactly like that).
        let hooks_wanted = self.settings.hooks.registers()
            && over.and_then(|o| o.hooks.as_deref()) != Some("off")
            && !self
                .injection_disabled
                .lock()
                .map(|s| s.contains(&profile.name))
                .unwrap_or(false);
        let mut hooks_registered = false;
        let mut extra_env = vec![
            ("AGENT_MUX".to_string(), "1".to_string()),
            ("AGENT_MUX_SESSION_ID".to_string(), launch_id.clone()),
        ];
        if hooks_wanted && let Some(exe) = hooks::register::current_exe() {
            extra_env.push(("AGENT_MUX_EXE".into(), exe.to_string_lossy().into_owned()));
            let reg = hooks::register::Registration {
                exe,
                home: self.settings.home.clone(),
                content_mode,
            };
            match provider {
                Provider::Claude => {
                    let user_files = [
                        self.settings.home.join(".claude").join("settings.json"),
                        dir.join(".claude").join("settings.json"),
                        dir.join(".claude").join("settings.local.json"),
                    ];
                    extra_args.push("--settings".into());
                    extra_args.push(hooks::register::claude_settings_json(&reg, &user_files));
                    hooks_registered = true;
                }
                Provider::Codex => {
                    let chain = hooks::register::codex_user_notify(&self.settings.home);
                    extra_args.push("-c".into());
                    extra_args.push(hooks::register::codex_notify_override(
                        &reg,
                        &launch_id,
                        chain.as_deref(),
                    ));
                    hooks_registered = true;
                }
                // agy loads hooks only from customization roots: see
                // `trace hooks install agy`
                Provider::Antigravity => {}
            }
        }

        let (backend, backend_requested) = self.resolve_backend(profile);
        Some(LaunchPlan {
            extra_env,
            launch_id,
            extra_args,
            provider,
            content_mode,
            correlation,
            correlation_label,
            known_session_id,
            injected,
            attached: false,
            hooks_registered,
            backend,
            backend_requested,
            profile_name: profile.name.clone(),
            dir: dir.to_path_buf(),
        })
    }

    fn map_settings(
        &self,
        plan: &LaunchPlan,
        agent_mux_session: usize,
        started_ns: i64,
    ) -> MapSettings {
        MapSettings {
            provider: plan.provider,
            content_mode: plan.content_mode,
            content_max_bytes: self.settings.content_max_bytes,
            redact_literals: self.settings.redact_literals.clone(),
            user_id: self.settings.user_id.clone(),
            release: self.settings.release.clone(),
            tags: self.settings.tags.clone(),
            environment: self.settings.environment.clone(),
            profile_name: plan.profile_name.clone(),
            cwd: plan.dir.to_string_lossy().into_owned(),
            project_slug: crate::history::project_slug(&plan.dir),
            agent_mux_session,
            launch_id: plan.launch_id.clone(),
            run_id: self.run_id.clone(),
            correlation_plan: if plan.hooks_registered {
                format!("announced+{}", plan.correlation_label)
            } else {
                plan.correlation_label.to_string()
            },
            injected: plan.injected,
            attached: plan.attached,
            started_ns,
        }
    }

    fn send(&self, op: StoreOp) {
        if self.writer.tx.try_send(op).is_err() {
            self.writer.dropped.fetch_add(1, Ordering::Relaxed);
            self.warn_dropped();
        }
    }

    /// Sends one op to the sinks `backend` selects (launch rows always go
    /// to the store).
    fn send_routed(&self, op: StoreOp, backend: Backend) {
        let (local, remote) = sink_targets(&op, backend);
        if remote && let Some(exporter) = &self.exporter {
            if local {
                if exporter.tx.try_send(op.clone()).is_err() {
                    exporter.dropped.fetch_add(1, Ordering::Relaxed);
                }
            } else if exporter.tx.try_send(op).is_err() {
                exporter.dropped.fetch_add(1, Ordering::Relaxed);
                return;
            } else {
                return;
            }
        }
        if local {
            self.send(op);
        }
    }

    /// Records the launch row and spawns the pipeline. Must be called from
    /// within the tokio runtime (all spawn sites are).
    pub fn start_session(
        &mut self,
        agent_mux_session: usize,
        plan: LaunchPlan,
    ) -> SessionTraceHandle {
        let started_ns = now_ns();
        let map_settings = self.map_settings(&plan, agent_mux_session, started_ns);
        if let Some(id) = &plan.known_session_id {
            let seed = TurnAssembler::new(
                map_settings.clone(),
                Some(id.clone()),
                plan.correlation_label,
            );
            self.send_routed(StoreOp::Session(seed.session_row(started_ns)), plan.backend);
        }
        let mut launch = map::launch_started(&map_settings, plan.known_session_id.as_deref());
        let mut meta = serde_json::Map::new();
        meta.insert(
            "backend".into(),
            serde_json::Value::from(plan.backend.as_str()),
        );
        if let Some(wanted) = plan.backend_requested {
            meta.insert(
                "backend_requested".into(),
                serde_json::Value::from(wanted.as_str()),
            );
        }
        launch.metadata = Some(serde_json::Value::Object(meta));
        self.send(StoreOp::Launch(launch));
        let phase_tx = watch::Sender::new(Phase::Running);
        let phase_rx = phase_tx.subscribe();
        let launch_id = plan.launch_id.clone();
        let backend = plan.backend;
        let known_resume = matches!(
            &plan.correlation,
            CorrelationSpec::KnownClaude { resume: true, .. }
                | CorrelationSpec::KnownAntigravity { .. }
        );
        let codex_sessions_dir = match &plan.correlation {
            CorrelationSpec::WatchCodex { sessions_dir, .. } => Some(sessions_dir.clone()),
            _ => None,
        };
        let ctx = PipelineCtx {
            db_path: self.settings.db_path.clone(),
            known_resume,
            codex_sessions_dir,
            claims: Arc::clone(&self.claims),
            op_tx: self.writer.tx.clone(),
            dropped: Arc::clone(&self.writer.dropped),
            backend: plan.backend,
            export_tx: self.exporter.as_ref().map(|e| e.tx.clone()),
            export_dropped: self.exporter.as_ref().map(|e| Arc::clone(&e.dropped)),
            status_tx: self.status_tx.clone(),
            injection_disabled: Arc::clone(&self.injection_disabled),
            fast_failures: Arc::clone(&self.fast_failures),
            drop_warned: Arc::clone(&self.drop_warned),
            map_settings,
            correlation: plan.correlation,
            correlation_label: plan.correlation_label,
            known_session_id: plan.known_session_id,
            injected: plan.injected || plan.hooks_registered,
            profile_name: plan.profile_name,
            provider: plan.provider,
            poll_interval: Duration::from_millis(self.settings.poll_interval_ms.max(50)),
            backfill_max_bytes: self.settings.backfill_max_bytes,
        };
        let shutdown_rx = self.shutdown_tx.subscribe();
        self.pipelines
            .push(tokio::spawn(run_pipeline(ctx, phase_rx, shutdown_rx)));
        SessionTraceHandle {
            phase: phase_tx,
            launch_id,
            backend,
        }
    }

    fn warn_dropped(&self) {
        if !self.drop_warned.swap(true, Ordering::Relaxed) {
            let _ = self.status_tx.try_send(AppEvent::TraceStatus(
                "tracing: some trace rows were dropped (store errors/backpressure)".into(),
            ));
        }
    }

    /// Bounded shutdown after `App::kill_all`: signal every pipeline (the
    /// main loop is gone, so this watch is their only exit signal), wait
    /// ~half the deadline for them, then drain the writer with the rest.
    pub async fn shutdown(self, deadline: Duration) {
        let TraceRuntime {
            writer,
            exporter,
            shutdown_tx,
            pipelines,
            ..
        } = self;
        let _ = shutdown_tx.send(true);
        let started = Instant::now();
        let half = deadline / 2;
        let join_all = async {
            for handle in pipelines {
                let _ = handle.await;
            }
        };
        let _ = tokio::time::timeout(half, join_all).await;
        let remaining = deadline.saturating_sub(started.elapsed());
        let writer_done = tokio::task::spawn_blocking(move || writer.finish(remaining));
        let exporter_done =
            exporter.map(|e| tokio::task::spawn_blocking(move || e.finish(remaining)));
        let _ = writer_done.await;
        if let Some(done) = exporter_done {
            let _ = done.await;
        }
    }
}

struct PipelineCtx {
    /// The store, for the hook feed's read-only connection.
    db_path: PathBuf,
    /// The launch resumes existing history (prime, never re-emit).
    known_resume: bool,
    /// Codex: where to look a hook-announced thread id up.
    codex_sessions_dir: Option<PathBuf>,
    claims: Arc<ClaimRegistry>,
    op_tx: std::sync::mpsc::SyncSender<StoreOp>,
    dropped: Arc<AtomicU64>,
    /// Where this launch's rows go.
    backend: Backend,
    /// The Langfuse sink, when configured (`backend` decides its use).
    export_tx: Option<std::sync::mpsc::SyncSender<StoreOp>>,
    export_dropped: Option<Arc<AtomicU64>>,
    status_tx: tokio::sync::mpsc::Sender<AppEvent>,
    injection_disabled: Arc<Mutex<HashSet<String>>>,
    fast_failures: Arc<Mutex<std::collections::HashMap<String, u32>>>,
    drop_warned: Arc<std::sync::atomic::AtomicBool>,
    map_settings: MapSettings,
    correlation: CorrelationSpec,
    correlation_label: &'static str,
    known_session_id: Option<String>,
    injected: bool,
    profile_name: String,
    provider: Provider,
    poll_interval: Duration,
    backfill_max_bytes: u64,
}

struct Pipeline {
    ctx: PipelineCtx,
    assembler: TurnAssembler,
    tailer: Option<Tailer>,
    adopted: Option<Adopted>,
    /// RAII: releases the transcript claim even if this pipeline panics.
    claim_guard: Option<correlate::ClaimGuard>,
    /// Antigravity: the per-request usage agy keeps outside the transcript.
    agy_usage: Option<agy_usage::AgyUsageReader>,
    /// The hook channel for this launch.
    hook_feed: HookFeed,
    hook_events: u64,
    parse_errors: u64,
    started: Instant,
}

impl Pipeline {
    fn send_ops(&self, ops: Vec<StoreOp>) {
        for op in ops {
            let (local, remote) = sink_targets(&op, self.ctx.backend);
            if remote
                && let Some(tx) = &self.ctx.export_tx
                && tx.try_send(op.clone()).is_err()
                && let Some(counter) = &self.ctx.export_dropped
            {
                counter.fetch_add(1, Ordering::Relaxed);
            }
            if !local {
                continue;
            }
            if self.ctx.op_tx.try_send(op).is_err() {
                self.ctx.dropped.fetch_add(1, Ordering::Relaxed);
                if !self.ctx.drop_warned.swap(true, Ordering::Relaxed) {
                    let _ = self.ctx.status_tx.try_send(AppEvent::TraceStatus(
                        "tracing: some trace rows were dropped (store errors/backpressure)".into(),
                    ));
                }
            }
        }
    }

    fn feed_line(&mut self, line: &str) {
        if line.trim().is_empty() {
            return;
        }
        let events = crate::transcript::parse_line(self.ctx.provider, line);
        if events.is_empty() && serde_json::from_str::<serde::de::IgnoredAny>(line).is_err() {
            self.parse_errors += 1;
            return;
        }
        let recv = now_nanos();
        let mut ops = Vec::new();
        for event in events {
            ops.extend(self.assembler.feed(event, recv));
        }
        self.send_ops(ops);
    }

    /// Prime (no emission) an existing transcript for Known resumes; always
    /// leaves the tailer positioned so only NEW content is exported.
    fn adopt(&mut self, adopted: Adopted) {
        self.claim_guard = Some(correlate::ClaimGuard::new(
            Arc::clone(&self.ctx.claims),
            self.ctx.provider,
            &adopted.session_id,
        ));
        self.assembler
            .set_session_id(&adopted.session_id, adopted.correlation);
        self.assembler
            .set_transcript_path(&adopted.path.to_string_lossy());
        self.hook_feed.set_session(&adopted.session_id);
        if adopted.resume_prime {
            match Tailer::prime(&adopted.path, self.ctx.backfill_max_bytes) {
                Ok((tailer, lines, truncated)) => {
                    self.assembler.set_emitting(false);
                    if truncated {
                        self.assembler.mark_backfill_truncated();
                    }
                    let recv = now_nanos();
                    for line in &lines {
                        for event in crate::transcript::parse_line(self.ctx.provider, line) {
                            let _ = self.assembler.feed(event, recv);
                        }
                    }
                    self.assembler.set_emitting(true);
                    self.tailer = Some(tailer);
                }
                Err(_) => {
                    self.tailer = Some(Tailer::new(&adopted.path));
                }
            }
        } else {
            self.tailer = Some(Tailer::new(&adopted.path));
        }
        if self.ctx.provider == Provider::Antigravity
            && let Some(db) = agy_usage::conversation_db_for(&adopted.path, &adopted.session_id)
        {
            let mut reader = agy_usage::AgyUsageReader::new(db);
            if adopted.resume_prime {
                reader.skip_existing();
            }
            self.agy_usage = Some(reader);
        }
        // adoption facts: the session row and the launch's outcome
        let now = now_ns();
        self.send_ops(vec![
            StoreOp::Session(self.assembler.session_row(now)),
            StoreOp::Launch(map::launch_adopted(
                &self.ctx.map_settings,
                &adopted.session_id,
                adopted.correlation,
            )),
        ]);
        self.adopted = Some(adopted);
    }

    /// Antigravity side channel: attach any new per-request usage records.
    fn poll_agy_usage(&mut self) {
        let Some(reader) = self.agy_usage.as_mut() else {
            return;
        };
        let records = reader.poll();
        if records.is_empty() {
            return;
        }
        let mut ops = Vec::new();
        for record in &records {
            ops.extend(self.assembler.attach_step_usage(record));
        }
        self.send_ops(ops);
    }

    /// A hook that named this launch's session (and transcript) wins over
    /// every watch heuristic. `resume_prime` follows the CLI's own word:
    /// a `resume` start, or a launch that already knew it resumes, primes
    /// the existing content instead of re-recording it.
    fn try_announced(&mut self) -> Option<Adopted> {
        let ann = self.hook_feed.announcement()?;
        let path = match ann.transcript_path.as_deref() {
            Some(p) => PathBuf::from(p),
            None if self.ctx.provider == Provider::Codex => correlate::codex_rollout_by_thread(
                self.ctx.codex_sessions_dir.as_deref()?,
                &ann.session_id,
            )?,
            None => return None,
        };
        if !path.is_file() {
            return None;
        }
        if !correlate::try_claim(&self.ctx.claims, self.ctx.provider, &ann.session_id) {
            return None;
        }
        Some(Adopted {
            session_id: ann.session_id,
            path,
            correlation: "announced",
            resume_prime: self.ctx.known_resume || ann.source.as_deref() == Some("resume"),
        })
    }

    /// Hook rows since the last tick: launch-level facts go straight to the
    /// launch row, everything else to the assembler.
    fn poll_hooks(&mut self) {
        let events = self.hook_feed.poll();
        if events.is_empty() {
            return;
        }
        self.hook_events += events.len() as u64;
        let mut ops = Vec::new();
        for ev in &events {
            match ev.event.as_str() {
                "SessionStart" => {
                    let meta = serde_json::json!({
                        "hooks": true,
                        "session_start_source": ev.payload.get("source").cloned().unwrap_or(serde_json::Value::Null),
                    });
                    ops.push(StoreOp::Launch(map::launch_metadata(
                        &self.ctx.map_settings,
                        self.assembler.session_id(),
                        meta,
                    )));
                }
                "SessionEnd" => {
                    let meta = serde_json::json!({
                        "session_end_reason": ev.payload.get("reason").cloned().unwrap_or(serde_json::Value::Null),
                    });
                    ops.push(StoreOp::Launch(map::launch_metadata(
                        &self.ctx.map_settings,
                        self.assembler.session_id(),
                        meta,
                    )));
                }
                _ => ops.extend(self.assembler.attach_hook_event(ev)),
            }
        }
        self.send_ops(ops);
    }

    fn tick(&mut self) {
        if self.adopted.is_none() {
            if let Some(adopted) = self.try_announced() {
                self.adopt(adopted);
            } else if let Some(adopted) =
                correlate::poll(&mut self.ctx.correlation, self.started, &self.ctx.claims)
            {
                self.adopt(adopted);
            }
        }
        self.tick_tail();
        self.poll_agy_usage();
        self.poll_hooks();
    }

    fn tick_tail(&mut self) {
        let Some(tailer) = self.tailer.as_mut() else {
            return;
        };
        match tailer.poll() {
            TailPoll::NoChange => {}
            TailPoll::Lines(lines) => {
                for line in lines {
                    self.feed_line(&line);
                }
            }
            TailPoll::Truncated => {
                // Unexpected rewrite: the file's contents CHANGED, so the
                // old assembler's ordinals no longer describe it. Rebuild a
                // FRESH assembler and salt this run's trace ids.
                if let Some(adopted) = &self.adopted {
                    let path = adopted.path.clone();
                    let session_id = adopted.session_id.clone();
                    let correlation = adopted.correlation;
                    let mut fresh = TurnAssembler::new(
                        self.ctx.map_settings.clone(),
                        Some(session_id),
                        correlation,
                    );
                    fresh.set_transcript_path(&path.to_string_lossy());
                    fresh.mark_backfill_truncated();
                    self.assembler = fresh;
                    match Tailer::prime(&path, self.ctx.backfill_max_bytes) {
                        Ok((new_tailer, lines, _truncated)) => {
                            self.assembler.set_emitting(false);
                            let recv = now_nanos();
                            for line in &lines {
                                for event in crate::transcript::parse_line(self.ctx.provider, line)
                                {
                                    let _ = self.assembler.feed(event, recv);
                                }
                            }
                            self.assembler.set_emitting(true);
                            self.tailer = Some(new_tailer);
                        }
                        Err(_) => self.tailer = Some(Tailer::new(&path)),
                    }
                }
            }
        }
    }

    fn finalize(&mut self, termination: &'static str, exit_code: Option<u32>) {
        if let Some(line) = self.tailer.as_mut().and_then(|t| t.take_remainder()) {
            self.feed_line(&line);
        }
        self.poll_agy_usage();
        self.poll_hooks();
        let mut ops = self.assembler.finalize();
        let end = SessionEnd {
            termination,
            exit_code,
            correlation: self
                .adopted
                .as_ref()
                .map(|a| a.correlation.to_string())
                .unwrap_or_else(|| {
                    if self.assembler.session_id().is_some() {
                        self.ctx.correlation_label.to_string()
                    } else {
                        "none".to_string()
                    }
                }),
            session_id: self.assembler.session_id().map(|s| s.to_string()),
            parse_errors: self.parse_errors,
            dropped_ops: self.ctx.dropped.load(Ordering::Relaxed),
            cost: self.assembler.cost_snapshot(),
        };
        let mut ended = map::launch_ended(&self.ctx.map_settings, &end, now_ns());
        if self.hook_events > 0 {
            ended.metadata = Some(serde_json::json!({ "hook_events": self.hook_events }));
        }
        ops.push(StoreOp::Launch(ended));
        self.send_ops(ops);
        self.claim_guard = None;
        // Fast-failure tracking for injected --session-id launches: two
        // consecutive immediate nonzero exits disable injection for the
        // profile for the rest of the run.
        if self.ctx.injected && termination == "exit" {
            let fast_failure = self.adopted.is_none()
                && exit_code.is_some_and(|c| c != 0)
                && self.started.elapsed() < Duration::from_secs(5);
            if fast_failure {
                let strikes = self
                    .ctx
                    .fast_failures
                    .lock()
                    .map(|mut m| {
                        let entry = m.entry(self.ctx.profile_name.clone()).or_insert(0);
                        *entry += 1;
                        *entry
                    })
                    .unwrap_or(0);
                if strikes == 1 {
                    let _ = self.ctx.status_tx.try_send(AppEvent::TraceStatus(format!(
                        "tracing: '{}' exited immediately with injected launch flags — \
                         old CLI? (set inject_session_id = false / hooks = \"off\", or run `agent-mux trace doctor`)",
                        self.ctx.profile_name
                    )));
                } else if strikes >= 2 {
                    let newly = self
                        .ctx
                        .injection_disabled
                        .lock()
                        .map(|mut s| s.insert(self.ctx.profile_name.clone()))
                        .unwrap_or(false);
                    if newly {
                        let _ = self.ctx.status_tx.try_send(AppEvent::TraceStatus(format!(
                            "tracing: launch-flag injection (--session-id, hooks) disabled for '{}' \
                             this run after repeated immediate exits",
                            self.ctx.profile_name
                        )));
                    }
                }
            } else if let Ok(mut m) = self.ctx.fast_failures.lock() {
                m.remove(&self.ctx.profile_name);
            }
        }
    }
}

async fn run_pipeline(
    ctx: PipelineCtx,
    mut phase_rx: watch::Receiver<Phase>,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    let assembler = TurnAssembler::new(
        ctx.map_settings.clone(),
        ctx.known_session_id.clone(),
        if ctx.known_session_id.is_some() {
            ctx.correlation_label
        } else {
            "none"
        },
    );
    let poll_interval = ctx.poll_interval;
    let hook_feed = HookFeed::new(&ctx.db_path, ctx.provider, &ctx.map_settings.launch_id);
    let mut pipeline = Pipeline {
        ctx,
        assembler,
        tailer: None,
        adopted: None,
        claim_guard: None,
        agy_usage: None,
        hook_feed,
        hook_events: 0,
        parse_errors: 0,
        started: Instant::now(),
    };
    loop {
        pipeline.tick();
        if *shutdown_rx.borrow() {
            // app quit: kill_all already ran and the main loop is gone. A
            // session that had already exited keeps its real outcome — the
            // quit must not overwrite an exit the App reported earlier.
            pipeline.tick();
            match *phase_rx.borrow() {
                Phase::Exited(code) => pipeline.finalize("exit", code),
                Phase::Stopped => pipeline.finalize("stopped", None),
                Phase::Running => pipeline.finalize("app_quit", None),
            }
            return;
        }
        let phase = *phase_rx.borrow();
        let sender_gone = phase_rx.has_changed().is_err();
        if matches!(phase, Phase::Stopped) {
            pipeline.tick();
            pipeline.finalize("stopped", None);
            return;
        }
        if let Phase::Exited(code) = phase {
            // Grace sweep: the CLI's final flushes can trail the PTY exit.
            for _ in 0..3 {
                tokio::select! {
                    _ = tokio::time::sleep(poll_interval.min(Duration::from_millis(350))) => {}
                    _ = shutdown_rx.changed() => {
                        pipeline.tick();
                        let code = match *phase_rx.borrow() {
                            Phase::Exited(late @ Some(_)) => late,
                            _ => code,
                        };
                        pipeline.finalize("exit", code);
                        return;
                    }
                }
                pipeline.tick();
            }
            let code = match *phase_rx.borrow() {
                Phase::Exited(late @ Some(_)) => late,
                _ => code,
            };
            pipeline.finalize("exit", code);
            return;
        }
        if sender_gone {
            pipeline.tick();
            pipeline.finalize("exit", None);
            return;
        }
        tokio::select! {
            _ = tokio::time::sleep(poll_interval) => {}
            _ = phase_rx.changed() => {}
            _ = shutdown_rx.changed() => {}
        }
    }
}

#[cfg(test)]
mod plan_tests {
    use super::*;
    use crate::config::{ProfileTracing, TracingConfig, resolve_tracing};

    /// A runtime with per-launch hook registration off, so the
    /// correlation-only assertions below stay exact.
    fn runtime(dir: &Path) -> TraceRuntime {
        runtime_with(dir, Some("off"), dir)
    }

    /// The default (hooks = auto) with `home` as the user's home.
    fn runtime_hooks(dir: &Path, home: &Path) -> TraceRuntime {
        runtime_with(dir, None, home)
    }

    fn runtime_with(dir: &Path, hooks: Option<&str>, home: &Path) -> TraceRuntime {
        let lf = TracingConfig {
            db_path: Some(dir.join("t.db").to_string_lossy().into_owned()),
            claude_dir: Some("/tmp/fake-claude".into()),
            codex_dir: Some("/tmp/fake-codex".into()),
            antigravity_dir: Some("/tmp/fake-agy".into()),
            hooks: hooks.map(str::to_string),
            ..Default::default()
        };
        let home = home.to_string_lossy().into_owned();
        let resolved =
            resolve_tracing(Some(&lf), &|k| (k == "HOME").then(|| home.clone())).unwrap();
        let (tx, _rx) = tokio::sync::mpsc::channel(16);
        TraceRuntime::new(resolved, tx).unwrap()
    }

    fn settings_arg(plan: &LaunchPlan) -> Option<serde_json::Value> {
        let i = plan.extra_args.iter().position(|a| a == "--settings")?;
        serde_json::from_str(&plan.extra_args[i + 1]).ok()
    }

    #[test]
    fn claude_launches_register_hooks_via_inline_settings() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        std::fs::write(
            home.join(".claude").join("settings.json"),
            r#"{"hooks":{"PostToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"lint.sh"}]}]}}"#,
        )
        .unwrap();
        let rt = runtime_hooks(dir.path(), &home);
        // a fresh session: --session-id first, then --settings
        let plan = rt
            .plan_launch(&profile("Claude Code", "claude", &[]), Path::new("/tmp"))
            .unwrap();
        assert_eq!(plan.extra_args[0], "--session-id");
        assert_eq!(plan.extra_args[2], "--settings");
        assert!(plan.hooks_registered && plan.injected);
        let v = settings_arg(&plan).expect("valid JSON");
        let hooks = v["hooks"].as_object().unwrap();
        for (event, _, _) in hooks::register::CLAUDE_EVENTS {
            assert!(hooks.contains_key(*event), "{event}");
        }
        let post = hooks["PostToolUse"].as_array().unwrap();
        assert_eq!(post.len(), 2, "user hook merged behind ours");
        assert_eq!(post[1]["matcher"], "Bash");
        let ours = &post[0]["hooks"][0];
        assert_eq!(
            ours["command"].as_str(),
            Some(&*std::env::current_exe().unwrap().to_string_lossy())
        );
        assert_eq!(ours["args"][4].as_str(), Some(&*home.to_string_lossy()));
        assert_eq!(ours["args"][6], rt.settings.content_mode.as_str());
        assert_eq!(ours["async"], true);
        assert_eq!(hooks["SessionEnd"][0]["hooks"][0].get("async"), None);
        assert!(plan.extra_env.iter().any(|(k, _)| k == "AGENT_MUX_EXE"));
        // launches the planner cannot correlate still announce themselves
        for args in [
            vec!["-r"],
            vec!["--continue"],
            vec!["-p", "hi"],
            vec!["--resume", "abc"],
        ] {
            let plan = rt
                .plan_launch(&profile("Claude Code", "claude", &args), Path::new("/tmp"))
                .unwrap();
            assert_eq!(plan.extra_args[0], "--settings", "{args:?}");
            assert!(settings_arg(&plan).is_some(), "{args:?}");
            assert!(plan.hooks_registered);
        }
        // the plan label records both channels
        let settings = rt.map_settings(&plan, 1, 0);
        assert_eq!(settings.correlation_plan, "announced+deterministic");
    }

    #[test]
    fn backend_follows_profile_then_global_and_downgrades_without_langfuse() {
        let dir = tempfile::tempdir().unwrap();
        // no credentials: every request for Langfuse runs local, noted
        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        let lf = TracingConfig {
            db_path: Some(dir.path().join("t.db").to_string_lossy().into_owned()),
            backend: Some("both".into()),
            hooks: Some("off".into()),
            ..Default::default()
        };
        let resolved = resolve_tracing(Some(&lf), &|_| None).unwrap();
        let rt = TraceRuntime::new(resolved, tx).unwrap();
        assert!(!rt.langfuse_configured());
        let plan = rt
            .plan_launch(&profile("Claude Code", "claude", &[]), Path::new("/tmp"))
            .unwrap();
        assert_eq!(plan.backend, Backend::Local);
        assert_eq!(plan.backend_requested, Some(Backend::Both));
        match rx.try_recv() {
            Ok(AppEvent::TraceStatus(msg)) => {
                assert!(msg.contains("Langfuse is not configured"), "{msg}")
            }
            other => panic!("{other:?}"),
        }
        // with credentials: the profile wins over the global default
        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        let env = |k: &str| match k {
            "LANGFUSE_PUBLIC_KEY" => Some("pk".to_string()),
            "LANGFUSE_SECRET_KEY" => Some("sk".to_string()),
            "LANGFUSE_HOST" => Some("http://127.0.0.1:9".to_string()),
            _ => None,
        };
        let resolved = resolve_tracing(Some(&lf), &env).unwrap();
        let rt = TraceRuntime::new(resolved, tx).unwrap();
        assert!(rt.langfuse_configured());
        assert_eq!(rt.default_backend(), Backend::Both);
        let plan = rt
            .plan_launch(&profile("Claude Code", "claude", &[]), Path::new("/tmp"))
            .unwrap();
        assert_eq!(plan.backend, Backend::Both);
        assert!(plan.backend_requested.is_none());
        let mut p = profile("Claude Code", "claude", &[]);
        p.tracing = Some(ProfileTracing {
            backend: Some("langfuse".into()),
            ..Default::default()
        });
        assert_eq!(
            rt.plan_launch(&p, Path::new("/tmp")).unwrap().backend,
            Backend::Langfuse
        );
        let attached = rt.plan_attach(&p, Path::new("/tmp")).unwrap();
        assert_eq!(attached.backend, Backend::Langfuse);
        assert!(rx.try_recv().is_err(), "no notice when honored");
        // routing: launch rows stay local whatever the backend
        let launch = StoreOp::Launch(map::launch_started(&rt.map_settings(&plan, 1, 0), None));
        assert_eq!(sink_targets(&launch, Backend::Langfuse), (true, false));
        let seed = TurnAssembler::new(rt.map_settings(&plan, 1, 0), Some("s".into()), "x");
        let session = StoreOp::Session(seed.session_row(0));
        assert_eq!(sink_targets(&session, Backend::Langfuse), (false, true));
        assert_eq!(sink_targets(&session, Backend::Both), (true, true));
        assert_eq!(sink_targets(&session, Backend::Local), (true, false));
    }

    #[test]
    fn hook_registration_honors_profile_and_global_off() {
        let dir = tempfile::tempdir().unwrap();
        let rt = runtime_hooks(dir.path(), dir.path());
        let mut p = profile("Claude Code", "claude", &[]);
        p.tracing = Some(ProfileTracing {
            hooks: Some("off".into()),
            ..Default::default()
        });
        let plan = rt.plan_launch(&p, Path::new("/tmp")).unwrap();
        assert!(!plan.hooks_registered);
        assert!(!plan.extra_args.iter().any(|a| a == "--settings"));
        assert_eq!(
            rt.map_settings(&plan, 1, 0).correlation_plan,
            "deterministic"
        );
        let off = runtime(dir.path());
        let plan = off
            .plan_launch(&profile("Codex", "codex", &[]), Path::new("/tmp"))
            .unwrap();
        assert!(plan.extra_args.is_empty() && !plan.hooks_registered);
        // `installed` registers like auto
        let installed = runtime_with(dir.path(), Some("installed"), dir.path());
        let plan = installed
            .plan_launch(&profile("Claude Code", "claude", &[]), Path::new("/tmp"))
            .unwrap();
        assert!(plan.hooks_registered);
    }

    #[test]
    fn codex_launches_register_notify_and_chain_the_users_program() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        std::fs::create_dir_all(home.join(".codex")).unwrap();
        let rt = runtime_hooks(dir.path(), &home);
        let plan = rt
            .plan_launch(
                &profile("Codex", "codex", &["--model", "gpt-5"]),
                Path::new("/tmp"),
            )
            .unwrap();
        assert_eq!(plan.extra_args[0], "-c");
        let doc: toml::Value = toml::from_str(&plan.extra_args[1]).expect("valid TOML");
        let argv: Vec<&str> = doc["notify"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(argv[1..4], ["trace", "hook", "codex-notify"]);
        assert_eq!(argv[5], &*home.to_string_lossy());
        assert_eq!(argv[7], plan.launch_id);
        assert_eq!(argv.len(), 8, "nothing to chain");
        assert!(plan.hooks_registered);
        assert_eq!(
            rt.map_settings(&plan, 1, 0).correlation_plan,
            "announced+watched"
        );
        // an existing notify is chained, not displaced
        std::fs::write(
            home.join(".codex").join("config.toml"),
            "notify = [\"python3\", \"/x/notify.py\"]\n",
        )
        .unwrap();
        let plan = rt
            .plan_launch(&profile("Codex", "codex", &[]), Path::new("/tmp"))
            .unwrap();
        let doc: toml::Value = toml::from_str(&plan.extra_args[1]).unwrap();
        let argv = doc["notify"].as_array().unwrap();
        assert_eq!(argv[8].as_str(), Some("--chain"));
        assert_eq!(argv[9].as_str(), Some(r#"["python3","/x/notify.py"]"#));
        // agy has no per-launch channel
        let agy = rt
            .plan_launch(&profile("Antigravity", "agy", &[]), Path::new("/tmp"))
            .unwrap();
        assert!(agy.extra_args.is_empty() && !agy.hooks_registered);
    }

    fn profile(name: &str, command: &str, args: &[&str]) -> Profile {
        Profile {
            name: name.into(),
            command: command.into(),
            args: args.iter().map(|s| s.to_string()).collect(),
            default_dir: None,
            tracing: None,
            model: None,
            bypass_approvals: None,
        }
    }

    #[test]
    fn claude_new_session_injects_session_id_and_markers() {
        let dir = tempfile::tempdir().unwrap();
        let rt = runtime(dir.path());
        let plan = rt
            .plan_launch(&profile("Claude Code", "claude", &[]), Path::new("/tmp"))
            .unwrap();
        assert_eq!(plan.extra_args.len(), 2);
        assert_eq!(plan.extra_args[0], "--session-id");
        let id = plan.extra_args[1].clone();
        assert!(uuid::Uuid::parse_str(&id).is_ok(), "injected id is a uuid");
        assert!(plan.injected);
        assert!(!plan.attached);
        assert_eq!(plan.known_session_id.as_deref(), Some(id.as_str()));
        assert_eq!(plan.correlation_label, "deterministic");
        assert!(matches!(
            &plan.correlation,
            CorrelationSpec::KnownClaude { resume: false, .. }
        ));
        assert!(
            plan.extra_env
                .iter()
                .any(|(k, v)| k == "AGENT_MUX" && v == "1")
        );
        assert!(
            plan.extra_env
                .iter()
                .any(|(k, v)| k == "AGENT_MUX_SESSION_ID" && *v == plan.launch_id)
        );
        let plan2 = rt
            .plan_launch(
                &profile("C", "/usr/local/bin/claude", &[]),
                Path::new("/tmp"),
            )
            .unwrap();
        assert!(plan2.injected);
    }

    #[test]
    fn claude_explicit_resume_id_is_known_without_injection() {
        let dir = tempfile::tempdir().unwrap();
        let rt = runtime(dir.path());
        let plan = rt
            .plan_launch(
                &profile("Claude Code", "claude", &["--resume", "abc-123"]),
                Path::new("/tmp"),
            )
            .unwrap();
        assert!(plan.extra_args.is_empty());
        assert!(!plan.injected);
        assert_eq!(plan.known_session_id.as_deref(), Some("abc-123"));
        assert!(matches!(
            &plan.correlation,
            CorrelationSpec::KnownClaude { session_id, resume: true, .. } if session_id == "abc-123"
        ));
        let plan2 = rt
            .plan_launch(
                &profile("Claude Code", "claude", &["--session-id", "def-456"]),
                Path::new("/tmp"),
            )
            .unwrap();
        assert!(plan2.extra_args.is_empty());
        assert_eq!(plan2.known_session_id.as_deref(), Some("def-456"));
    }

    #[test]
    fn claude_skip_flags_without_id_are_launch_only() {
        let dir = tempfile::tempdir().unwrap();
        let rt = runtime(dir.path());
        for args in [
            vec!["-r"],
            vec!["--continue"],
            vec!["-c"],
            vec!["-p"],
            vec!["--print"],
        ] {
            let plan = rt
                .plan_launch(&profile("Claude Code", "claude", &args), Path::new("/tmp"))
                .unwrap();
            assert!(plan.extra_args.is_empty(), "no injection for {args:?}");
            assert!(plan.known_session_id.is_none());
            assert!(
                matches!(plan.correlation, CorrelationSpec::None),
                "{args:?}"
            );
            assert_eq!(plan.correlation_label, "none");
        }
    }

    #[test]
    fn inject_session_id_false_is_an_escape_hatch() {
        let dir = tempfile::tempdir().unwrap();
        let rt = runtime(dir.path());
        let mut p = profile("Claude Code", "claude", &[]);
        p.tracing = Some(ProfileTracing {
            inject_session_id: Some(false),
            ..Default::default()
        });
        let plan = rt.plan_launch(&p, Path::new("/tmp")).unwrap();
        assert!(plan.extra_args.is_empty());
        assert!(!plan.injected);
        assert!(matches!(plan.correlation, CorrelationSpec::None));
    }

    #[test]
    fn disabled_profile_none_provider_and_unknown_command_are_untraced() {
        let dir = tempfile::tempdir().unwrap();
        let rt = runtime(dir.path());
        let mut disabled = profile("X", "claude", &[]);
        disabled.tracing = Some(ProfileTracing {
            enabled: Some(false),
            ..Default::default()
        });
        assert!(rt.plan_launch(&disabled, Path::new("/tmp")).is_none());
        let mut none = profile("X", "claude", &[]);
        none.tracing = Some(ProfileTracing {
            provider: Some("none".into()),
            ..Default::default()
        });
        assert!(rt.plan_launch(&none, Path::new("/tmp")).is_none());
        assert!(
            rt.plan_launch(&profile("Shell", "bash", &[]), Path::new("/tmp"))
                .is_none()
        );
    }

    #[test]
    fn provider_override_forces_kind_for_wrapper_commands() {
        let dir = tempfile::tempdir().unwrap();
        let rt = runtime(dir.path());
        let mut wrapper = profile("My Wrapper", "my-agent-wrapper", &[]);
        wrapper.tracing = Some(ProfileTracing {
            provider: Some("codex".into()),
            ..Default::default()
        });
        let plan = rt.plan_launch(&wrapper, Path::new("/tmp")).unwrap();
        assert_eq!(plan.provider, Provider::Codex);
        assert!(matches!(
            plan.correlation,
            CorrelationSpec::WatchCodex { .. }
        ));
        assert_eq!(plan.correlation_label, "watched");
        assert!(plan.extra_args.is_empty());
    }

    #[test]
    fn codex_and_antigravity_watch_agy_resume_is_known() {
        let dir = tempfile::tempdir().unwrap();
        let rt = runtime(dir.path());
        let codex = rt
            .plan_launch(&profile("Codex", "codex", &[]), Path::new("/tmp"))
            .unwrap();
        assert!(matches!(
            codex.correlation,
            CorrelationSpec::WatchCodex { .. }
        ));
        let agy = rt
            .plan_launch(&profile("Antigravity", "agy", &[]), Path::new("/tmp"))
            .unwrap();
        assert!(matches!(
            agy.correlation,
            CorrelationSpec::WatchAntigravity { .. }
        ));
        let resume = rt
            .plan_launch(
                &profile("Antigravity", "agy", &["--conversation", "conv-9"]),
                Path::new("/tmp"),
            )
            .unwrap();
        assert!(matches!(
            &resume.correlation,
            CorrelationSpec::KnownAntigravity { conversation_id, .. } if conversation_id == "conv-9"
        ));
        assert_eq!(resume.known_session_id.as_deref(), Some("conv-9"));
        assert_eq!(resume.correlation_label, "deterministic");
    }

    #[test]
    fn content_mode_override_applies_per_profile() {
        let dir = tempfile::tempdir().unwrap();
        let rt = runtime(dir.path());
        let plain = rt
            .plan_launch(&profile("Claude Code", "claude", &[]), Path::new("/tmp"))
            .unwrap();
        assert_eq!(plain.content_mode, ContentMode::Full, "full is the default");
        let mut meta = profile("Claude Code", "claude", &[]);
        meta.tracing = Some(ProfileTracing {
            content_mode: Some("metadata".into()),
            ..Default::default()
        });
        let plan = rt.plan_launch(&meta, Path::new("/tmp")).unwrap();
        assert_eq!(plan.content_mode, ContentMode::Metadata);
    }

    #[test]
    fn plan_attach_watches_running_sessions_without_extra_args() {
        let dir = tempfile::tempdir().unwrap();
        let rt = runtime(dir.path());
        let claude = rt
            .plan_attach(&profile("Claude Code", "claude", &[]), Path::new("/tmp"))
            .unwrap();
        assert!(claude.extra_args.is_empty());
        assert!(!claude.injected);
        assert!(claude.attached);
        assert!(matches!(
            claude.correlation,
            CorrelationSpec::WatchClaude { .. }
        ));
        assert_eq!(claude.correlation_label, "watched");
        let agy = rt
            .plan_attach(&profile("Antigravity", "agy", &[]), Path::new("/tmp"))
            .unwrap();
        assert!(agy.extra_args.is_empty());
        assert!(matches!(
            agy.correlation,
            CorrelationSpec::WatchAntigravity { .. }
        ));
    }

    #[test]
    fn unopenable_store_yields_an_error_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let blocker = dir.path().join("not-a-dir");
        std::fs::write(&blocker, b"x").unwrap();
        let lf = TracingConfig {
            db_path: Some(blocker.join("t.db").to_string_lossy().into_owned()),
            ..Default::default()
        };
        let resolved = resolve_tracing(Some(&lf), &|_| None).unwrap();
        let (tx, _rx) = tokio::sync::mpsc::channel(16);
        assert!(TraceRuntime::new(resolved, tx).is_err());
    }
}
