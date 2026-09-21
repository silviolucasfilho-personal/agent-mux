//! The loop context snapshot: `<runtime>/loops/<run_id>.json`, the facts a
//! run reads on turn one (`$AGENT_MUX_LOOP_CONTEXT`). Built by the App
//! from the store and the workspace files; written before launch; swept
//! with the briefings after 24 h.

use crate::loops::Level;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct RunInfo {
    pub id: String,
    pub pattern: String,
    pub level_configured: Level,
    pub level_effective: Level,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct WorktreeInfo {
    pub path: String,
    pub branch: String,
    pub base: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Files {
    pub state: String,
    pub run_log: String,
    pub constraints: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ledger: Option<String>,
    pub gate: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Budget {
    pub runs_today: i64,
    pub max_runs_per_day: u32,
    pub tokens_today: i64,
    pub max_tokens_per_day: u64,
    pub percent: u32,
    /// `normal` | `report-only`
    pub mode: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Breaker {
    pub applicable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default)]
    pub iterations: usize,
    #[serde(default)]
    pub consecutive_failures: usize,
    /// One more attempt of this kind would trip the breaker; the run is
    /// capped at L1 (`run.level_reason` says so).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub near_trip: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Gate {
    pub denylist: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_files: Option<u32>,
    #[serde(default)]
    pub auto_merge_allowlist: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Readiness {
    pub score: u32,
    pub level: String,
    #[serde(default)]
    pub findings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct RunSummary {
    pub id: String,
    pub outcome: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub items_found: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actions_taken: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub escalations: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ContextDoc {
    pub schema_version: u32,
    pub as_of: String,
    pub run: RunInfo,
    pub workspace: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<WorktreeInfo>,
    pub files: Files,
    pub budget: Budget,
    pub breaker: Breaker,
    pub gate: Gate,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub readiness: Option<Readiness>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_run: Option<RunSummary>,
    #[serde(default)]
    pub recent_runs: Vec<RunSummary>,
    pub inbox_waiting: usize,
    pub kill_switch: bool,
    /// The human gates of the pattern, for the skill's escalation rule.
    #[serde(default)]
    pub human_gates: Vec<String>,
}

pub fn contexts_dir(runtime_dir: &Path) -> PathBuf {
    runtime_dir.join("loops")
}

/// The snapshot path for a run id (`:` becomes `-`).
pub fn context_path(runtime_dir: &Path, run_id: &str) -> PathBuf {
    contexts_dir(runtime_dir).join(format!("{}.json", run_id.replace(':', "-")))
}

/// Writes the snapshot privately (0600 on Unix) and returns its path.
pub fn write(runtime_dir: &Path, run_id: &str, doc: &ContextDoc) -> std::io::Result<PathBuf> {
    let dir = contexts_dir(runtime_dir);
    std::fs::create_dir_all(&dir)?;
    let path = context_path(runtime_dir, run_id);
    let json = serde_json::to_string_pretty(doc)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let temp = path.with_extension("tmp");
    std::fs::write(&temp, json)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(&temp, &path)?;
    Ok(path)
}

pub fn read(path: &Path) -> Option<ContextDoc> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

/// Removes snapshots older than `max_age`; returns how many.
pub fn sweep(runtime_dir: &Path, max_age: std::time::Duration) -> usize {
    let Ok(entries) = std::fs::read_dir(contexts_dir(runtime_dir)) else {
        return 0;
    };
    let mut n = 0;
    for e in entries.flatten() {
        let p = e.path();
        let old = std::fs::metadata(&p)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|m| m.elapsed().ok())
            .is_some_and(|age| age > max_age);
        if old && std::fs::remove_file(&p).is_ok() {
            n += 1;
        }
    }
    n
}

/// The context of a run recomputed from the store, the registry and the
/// workspace files: what `agent_mux_get_loop_context` answers. `run_id`
/// picks a run; otherwise the newest run of the workspace's first loop.
pub fn live(
    db: &Path,
    workspace: Option<&Path>,
    run_id: Option<&str>,
) -> Result<ContextDoc, String> {
    use crate::loops::{patterns, registry, store as lstore};
    let conn = crate::tracing::store::open_ro(db)?;
    let reg = registry::registry_path()
        .map(|p| registry::load(&p))
        .unwrap_or_default();
    let run = match run_id {
        Some(id) => lstore::get_run(&conn, id).map_err(|e| e.to_string())?,
        None => None,
    };
    let entry = match (&run, workspace) {
        (Some(r), _) => reg.find(&r.loop_id).cloned().or_else(|| {
            reg.loops
                .iter()
                .find(|l| l.workspace.to_string_lossy() == r.workspace)
                .cloned()
        }),
        (None, Some(ws)) => reg.for_workspace(ws).first().map(|l| (*l).clone()),
        (None, None) => None,
    }
    .ok_or_else(|| match run_id {
        Some(id) => format!("no loop registered for run {id}"),
        None => "no loop registered for this workspace".to_string(),
    })?;
    let run = match run {
        Some(r) => Some(r),
        None => lstore::recent_runs(&conn, &entry.id, 1)
            .map_err(|e| e.to_string())?
            .into_iter()
            .next(),
    };
    let pattern = patterns::find(&entry.pattern).ok_or("unknown pattern")?;
    let now = crate::loops::now();
    let spend = lstore::spend_since(
        &conn,
        &entry.id,
        crate::loops::to_ns(crate::loops::utc_midnight(now)),
    )
    .unwrap_or_default();
    let percent = if entry.max_tokens_per_day == 0 {
        0
    } else {
        ((spend.tokens.max(0) as u128 * 100) / entry.max_tokens_per_day as u128) as u32
    };
    let breaker = if pattern.breaker {
        match crate::loops::breaker::load(&entry.workspace.join(crate::loops::LEDGER_JSON)) {
            Ok(l) => {
                let v = crate::loops::breaker::check(&l, &Default::default());
                Breaker {
                    applicable: true,
                    status: Some(if v.tripped() {
                        "tripped".into()
                    } else {
                        "ok".into()
                    }),
                    reason: Some(v.reason),
                    near_trip: v.near_trip.map(|t| t.as_str().to_string()),
                    iterations: v.iterations,
                    consecutive_failures: l
                        .attempts
                        .iter()
                        .rev()
                        .take_while(|a| a.outcome == crate::loops::breaker::AttemptOutcome::Failure)
                        .count(),
                }
            }
            Err(_) => Breaker {
                applicable: true,
                status: Some("no ledger".into()),
                ..Default::default()
            },
        }
    } else {
        Breaker::default()
    };
    let gate = crate::loops::gate::load(&entry.workspace.join(crate::loops::GATE_YAML))
        .unwrap_or_else(|_| crate::loops::gate::default_config());
    let activity = lstore::activity_count(
        &conn,
        &entry.workspace.to_string_lossy(),
        crate::loops::to_ns(now - time::Duration::days(14)),
    )
    .unwrap_or(0);
    let audit = crate::loops::readiness::audit(&entry.workspace, activity);
    let recent = lstore::recent_runs(&conn, &entry.id, 5).unwrap_or_default();
    let inbox_waiting = lstore::inbox(&conn).map(|v| v.len()).unwrap_or(0);
    let kill_switch = reg.pause_all
        || [pattern.state_file.as_str(), crate::loops::LOOP_MD]
            .iter()
            .any(|f| {
                std::fs::read_to_string(entry.workspace.join(f))
                    .is_ok_and(|t| crate::loops::run::kill_switch_active(&t))
            });
    let summary = |r: &lstore::LoopRun| RunSummary {
        id: r.id.clone(),
        outcome: r.outcome.as_str().into(),
        items_found: r.items_found,
        actions_taken: r.actions_taken,
        escalations: r.escalations,
        tokens: r.tokens,
    };
    let (run_info, worktree, previous) = match &run {
        Some(r) => (
            RunInfo {
                id: r.id.clone(),
                pattern: r.pattern.clone(),
                level_configured: r.level,
                level_effective: r.effective_level,
                level_reason: r.detail_str("level_reason").map(str::to_string),
            },
            r.worktree.as_ref().map(|p| WorktreeInfo {
                path: p.clone(),
                branch: r.branch.clone().unwrap_or_default(),
                base: String::new(),
            }),
            recent.iter().find(|x| x.id != r.id).map(summary),
        ),
        None => (
            RunInfo {
                id: String::new(),
                pattern: entry.pattern.clone(),
                level_configured: entry.level,
                level_effective: entry.level,
                level_reason: None,
            },
            None,
            recent.first().map(summary),
        ),
    };
    Ok(ContextDoc {
        schema_version: SCHEMA_VERSION,
        as_of: crate::loops::format_timestamp(now),
        run: run_info,
        workspace: entry.workspace.to_string_lossy().into_owned(),
        worktree,
        files: Files {
            state: pattern.state_file.clone(),
            run_log: crate::loops::RUN_LOG_MD.into(),
            constraints: crate::loops::CONSTRAINTS_MD.into(),
            ledger: pattern
                .breaker
                .then(|| crate::loops::LEDGER_JSON.to_string()),
            gate: crate::loops::GATE_YAML.into(),
        },
        budget: Budget {
            runs_today: spend.runs,
            max_runs_per_day: entry.max_runs_per_day,
            tokens_today: spend.tokens,
            max_tokens_per_day: entry.max_tokens_per_day,
            percent,
            mode: if percent >= 100 {
                "blocked".into()
            } else if percent >= 80 {
                "report-only".into()
            } else {
                "normal".into()
            },
        },
        breaker,
        gate: Gate {
            denylist: gate.denylist.clone(),
            max_files: gate.max_files,
            auto_merge_allowlist: gate.auto_merge_allowlist.clone(),
        },
        readiness: Some(Readiness {
            score: audit.score,
            level: audit.level_str().into(),
            findings: audit
                .findings
                .iter()
                .filter(|f| f.level != crate::loops::readiness::FindingLevel::Ok)
                .take(5)
                .map(|f| format!("{} {}", f.level.glyph(), f.message))
                .collect(),
        }),
        previous_run: previous,
        recent_runs: recent.iter().map(summary).collect(),
        inbox_waiting,
        kill_switch,
        human_gates: pattern.human_gates.clone(),
    })
}

/// `trace doctor`'s loops section: (ok, label, detail) lines.
pub fn doctor_lines(
    db: &Path,
    settings: &crate::config::LoopRunnerSettings,
) -> Vec<(bool, String, String)> {
    use crate::loops::{patterns, registry, scaffold};
    let mut out = Vec::new();
    let path = registry::registry_path();
    let reg = path.as_ref().map(|p| registry::load(p)).unwrap_or_default();
    out.push((
        true,
        "registry".into(),
        format!(
            "{} ({} loop(s){})",
            path.as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "no home".into()),
            reg.loops.len(),
            if reg.pause_all { ", PAUSED" } else { "" }
        ),
    ));
    out.push((
        settings.enabled,
        "scheduler".into(),
        format!(
            "{} · max {} concurrent · timeout {} s · catch-up {}",
            if settings.enabled {
                "on"
            } else {
                "off ([loops] enabled = false)"
            },
            settings.max_concurrent,
            settings.run_timeout_s,
            if settings.catch_up_once {
                "once"
            } else {
                "skip"
            }
        ),
    ));
    let exe = crate::tracing::hooks::register::current_exe();
    out.push((
        exe.is_some(),
        "claude guard".into(),
        if exe.is_some() {
            "per-launch PreToolUse with --loop (ceiling L3)".into()
        } else {
            "binary path not absolute: loops run at L1".into()
        },
    ));
    let home = crate::skill::install::home_dir();
    let codex = crate::tracing::hooks::install::codex_status(&home, exe.as_deref());
    out.push((
        codex.installed && !codex.stale,
        "codex guard".into(),
        if codex.installed && !codex.stale {
            "installed hooks.json (ceiling L3)".into()
        } else {
            format!("{} — Codex loops run at L1", codex.note)
        },
    ));
    let git = std::process::Command::new("git")
        .arg("--version")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
    out.push((
        git.is_some(),
        "git".into(),
        git.unwrap_or_else(|| "not found: L2+ worktrees unavailable".into()),
    ));
    match crate::tracing::store::open_ro(db) {
        Ok(conn) => {
            let inbox = crate::loops::store::inbox(&conn)
                .map(|v| v.len())
                .unwrap_or(0);
            out.push((
                true,
                "store".into(),
                format!("loop_runs readable · {inbox} in the inbox"),
            ));
        }
        Err(e) => out.push((false, "store".into(), e)),
    }
    for l in &reg.loops {
        let Some(p) = patterns::find(&l.pattern) else {
            out.push((false, l.pattern.clone(), "unknown pattern".into()));
            continue;
        };
        let files = scaffold::contract_files(&l.workspace, p);
        let missing: Vec<&str> = files
            .iter()
            .filter(|f| !f.present)
            .map(|f| f.name.as_str())
            .collect();
        let stale = files.iter().any(|f| f.stale);
        let ok = missing.is_empty() && !stale && l.workspace.is_dir();
        let detail = if !l.workspace.is_dir() {
            "workspace missing".to_string()
        } else if !missing.is_empty() {
            format!("missing {}", missing.join(", "))
        } else if stale {
            "state file stale (Last run older than 14 days)".into()
        } else {
            format!(
                "every {} at {}{}",
                crate::loops::format_interval(l.interval_s),
                l.level.as_str(),
                if l.paused() { " (paused)" } else { "" }
            )
        };
        out.push((
            ok,
            format!("{} @ {}", l.pattern, l.workspace_name()),
            detail,
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_read_and_sweep() {
        let temp = tempfile::tempdir().unwrap();
        let doc = ContextDoc {
            schema_version: SCHEMA_VERSION,
            as_of: "2026-09-16T08:00:00Z".into(),
            run: RunInfo {
                id: "2026-09-16T08:00:00Z".into(),
                pattern: "daily-triage".into(),
                level_configured: Level::L2,
                level_effective: Level::L1,
                level_reason: Some("tokens today at 84% of the cap".into()),
            },
            workspace: "/w".into(),
            budget: Budget {
                percent: 84,
                mode: "report-only".into(),
                ..Default::default()
            },
            ..Default::default()
        };
        let path = write(temp.path(), "2026-09-16T08:00:00Z", &doc).unwrap();
        assert!(path.ends_with("loops/2026-09-16T08-00-00Z.json"));
        let back = read(&path).unwrap();
        assert_eq!(back, doc);
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(v["run"]["level_effective"], "L1");
        assert!(v.get("worktree").is_none(), "absent, not null");
        assert_eq!(sweep(temp.path(), std::time::Duration::from_secs(3600)), 0);
        assert_eq!(sweep(temp.path(), std::time::Duration::ZERO), 1);
    }
}
