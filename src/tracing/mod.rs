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
pub mod ids;
pub mod map;
pub mod pricing;
pub mod store;
pub mod tail;
pub mod usage;

use crate::config::{ContentMode, Profile, ResolvedTracing};
use crate::events::AppEvent;
use crate::transcript::Provider;
use correlate::{Adopted, ClaimRegistry, CorrelationSpec};
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
    profile_name: String,
    dir: PathBuf,
}

pub struct TraceRuntime {
    settings: ResolvedTracing,
    claims: Arc<ClaimRegistry>,
    writer: WriterHandle,
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
        Ok(TraceRuntime {
            settings,
            claims: Arc::new(Mutex::new(HashSet::new())),
            writer,
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

        Some(LaunchPlan {
            extra_env: vec![
                ("AGENT_MUX".into(), "1".into()),
                ("AGENT_MUX_SESSION_ID".into(), launch_id.clone()),
            ],
            launch_id,
            extra_args,
            provider,
            content_mode,
            correlation,
            correlation_label,
            known_session_id,
            injected,
            attached: false,
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
            correlation_plan: plan.correlation_label.to_string(),
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
            self.send(StoreOp::Session(seed.session_row(started_ns)));
        }
        self.send(StoreOp::Launch(map::launch_started(
            &map_settings,
            plan.known_session_id.as_deref(),
        )));
        let phase_tx = watch::Sender::new(Phase::Running);
        let phase_rx = phase_tx.subscribe();
        let launch_id = plan.launch_id.clone();
        let ctx = PipelineCtx {
            claims: Arc::clone(&self.claims),
            op_tx: self.writer.tx.clone(),
            dropped: Arc::clone(&self.writer.dropped),
            status_tx: self.status_tx.clone(),
            injection_disabled: Arc::clone(&self.injection_disabled),
            fast_failures: Arc::clone(&self.fast_failures),
            drop_warned: Arc::clone(&self.drop_warned),
            map_settings,
            correlation: plan.correlation,
            correlation_label: plan.correlation_label,
            known_session_id: plan.known_session_id,
            injected: plan.injected,
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
        let _ = tokio::task::spawn_blocking(move || writer.finish(remaining)).await;
    }
}

struct PipelineCtx {
    claims: Arc<ClaimRegistry>,
    op_tx: std::sync::mpsc::SyncSender<StoreOp>,
    dropped: Arc<AtomicU64>,
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
    parse_errors: u64,
    started: Instant,
}

impl Pipeline {
    fn send_ops(&self, ops: Vec<StoreOp>) {
        for op in ops {
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

    fn tick(&mut self) {
        if self.adopted.is_none()
            && let Some(adopted) =
                correlate::poll(&mut self.ctx.correlation, self.started, &self.ctx.claims)
        {
            self.adopt(adopted);
        }
        self.tick_tail();
        self.poll_agy_usage();
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
        ops.push(StoreOp::Launch(map::launch_ended(
            &self.ctx.map_settings,
            &end,
            now_ns(),
        )));
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
                        "tracing: '{}' exited immediately with an injected --session-id — \
                         old claude? (set inject_session_id = false, or run `agent-mux trace doctor`)",
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
                            "tracing: --session-id injection disabled for '{}' this run \
                             after repeated immediate exits",
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
    let mut pipeline = Pipeline {
        ctx,
        assembler,
        tailer: None,
        adopted: None,
        claim_guard: None,
        agy_usage: None,
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

    fn runtime(dir: &Path) -> TraceRuntime {
        let lf = TracingConfig {
            db_path: Some(dir.join("t.db").to_string_lossy().into_owned()),
            claude_dir: Some("/tmp/fake-claude".into()),
            codex_dir: Some("/tmp/fake-codex".into()),
            antigravity_dir: Some("/tmp/fake-agy".into()),
            ..Default::default()
        };
        let resolved = resolve_tracing(Some(&lf), &|_| None).unwrap();
        let (tx, _rx) = tokio::sync::mpsc::channel(16);
        TraceRuntime::new(resolved, tx).unwrap()
    }

    fn profile(name: &str, command: &str, args: &[&str]) -> Profile {
        Profile {
            name: name.into(),
            command: command.into(),
            args: args.iter().map(|s| s.to_string()).collect(),
            default_dir: None,
            tracing: None,
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
