//! Experiments: a named task run under several variants, each run an
//! ordinary traced launch carrying two labels. The registry, the headless
//! runner, per-variant summaries, and side-by-side comparison of turns.

use crate::config::Profile;
use crate::harness::{Harness, LaunchOptions, compose};
use crate::tracing::loops::{LoopMetrics, loop_metrics};
use crate::tracing::store::query::{self, ObservationView, TraceStat};
use rusqlite::{Connection, OptionalExtension, params};
use std::path::PathBuf;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Pass,
    Fail,
    /// No check command, or the run could not be judged.
    Unknown,
}

impl Outcome {
    pub fn as_str(&self) -> &'static str {
        match self {
            Outcome::Pass => "pass",
            Outcome::Fail => "fail",
            Outcome::Unknown => "unknown",
        }
    }

    pub fn parse(s: &str) -> Outcome {
        match s {
            "pass" => Outcome::Pass,
            "fail" => Outcome::Fail,
            _ => Outcome::Unknown,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Experiment {
    pub id: String,
    pub name: String,
    pub prompt: String,
    pub cwd: Option<String>,
    pub check_cmd: Option<String>,
    pub created_ns: i64,
    pub runs: i64,
    pub variants: i64,
}

fn now_ns() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0)
}

/// Finds or creates the experiment by name; a repeat with a different
/// prompt or check updates them, since the name is what runs are grouped by.
pub fn upsert_experiment(
    conn: &Connection,
    name: &str,
    prompt: &str,
    cwd: Option<&str>,
    check_cmd: Option<&str>,
) -> rusqlite::Result<String> {
    let id = crate::tracing::ids::span_id_hex(&format!("amx1|experiment|{name}"));
    conn.execute(
        "INSERT INTO experiments (id, name, prompt, cwd, check_cmd, created_ns)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(name) DO UPDATE SET
           prompt = COALESCE(NULLIF(excluded.prompt, ''), prompt),
           cwd = COALESCE(excluded.cwd, cwd),
           check_cmd = COALESCE(excluded.check_cmd, check_cmd)",
        params![id, name, prompt, cwd, check_cmd, now_ns()],
    )?;
    Ok(id)
}

pub fn record_run(
    conn: &Connection,
    launch_id: &str,
    experiment_id: &str,
    variant: &str,
    outcome: Outcome,
    detail: &serde_json::Value,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO experiment_runs (launch_id, experiment_id, variant, outcome, detail, recorded_ns)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(launch_id) DO UPDATE SET
           outcome = excluded.outcome, detail = excluded.detail, recorded_ns = excluded.recorded_ns",
        params![
            launch_id,
            experiment_id,
            variant,
            outcome.as_str(),
            detail.to_string(),
            now_ns()
        ],
    )?;
    Ok(())
}

pub fn list_experiments(conn: &Connection) -> rusqlite::Result<Vec<Experiment>> {
    let mut stmt = conn.prepare(
        "SELECT e.id, e.name, e.prompt, e.cwd, e.check_cmd, e.created_ns,
                (SELECT COUNT(*) FROM experiment_runs r WHERE r.experiment_id = e.id),
                (SELECT COUNT(DISTINCT r.variant) FROM experiment_runs r WHERE r.experiment_id = e.id)
         FROM experiments e ORDER BY e.created_ns DESC",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(Experiment {
            id: r.get(0)?,
            name: r.get(1)?,
            prompt: r.get(2)?,
            cwd: r.get(3)?,
            check_cmd: r.get(4)?,
            created_ns: r.get(5)?,
            runs: r.get(6)?,
            variants: r.get(7)?,
        })
    })?;
    rows.collect()
}

/// One variant of one experiment, over all its runs.
#[derive(Debug, Clone, PartialEq)]
pub struct VariantSummary {
    pub variant: String,
    pub runs: i64,
    pub passes: i64,
    pub fails: i64,
    pub unknown: i64,
    pub mean_cost: Option<f64>,
    pub p50_cost: Option<f64>,
    pub mean_turns: f64,
    pub mean_wall_ms: Option<f64>,
    /// Mean of the launch-level scores people gave these runs, if any.
    pub mean_score: Option<f64>,
}

impl VariantSummary {
    pub fn pass_rate(&self) -> Option<f64> {
        let judged = self.passes + self.fails;
        (judged > 0).then(|| self.passes as f64 / judged as f64)
    }
}

pub fn experiment_summary(conn: &Connection, name: &str) -> rusqlite::Result<Vec<VariantSummary>> {
    struct Sample {
        outcome: Outcome,
        wall_ms: Option<i64>,
        turns: i64,
        cost: Option<f64>,
        score: Option<f64>,
    }
    let mut stmt = conn.prepare(
        "SELECT r.variant, r.outcome, l.started_ns, l.ended_ns,
                (SELECT COUNT(*) FROM traces t WHERE t.launch_id = r.launch_id),
                (SELECT SUM(ts.total_cost_usd) FROM trace_stats ts WHERE ts.launch_id = r.launch_id),
                (SELECT AVG(s.value) FROM scores s
                  WHERE (s.target = 'launch' AND s.target_id = r.launch_id)
                     OR (s.target = 'trace' AND s.target_id IN
                         (SELECT id FROM traces WHERE launch_id = r.launch_id)))
         FROM experiment_runs r
         JOIN launches l ON l.id = r.launch_id
         JOIN experiments e ON e.id = r.experiment_id
         WHERE e.name = ?1
         ORDER BY r.variant, l.started_ns",
    )?;
    let mut by_variant: Vec<(String, Vec<Sample>)> = Vec::new();
    for row in stmt.query_map(params![name], |r| {
        let started: i64 = r.get(2)?;
        let ended: Option<i64> = r.get(3)?;
        Ok((
            r.get::<_, String>(0)?,
            Sample {
                outcome: Outcome::parse(&r.get::<_, String>(1)?),
                wall_ms: ended.map(|e| (e - started).max(0) / 1_000_000),
                turns: r.get(4)?,
                cost: r.get(5)?,
                score: r.get(6)?,
            },
        ))
    })? {
        let (variant, sample) = row?;
        match by_variant.iter_mut().find(|(v, _)| *v == variant) {
            Some((_, samples)) => samples.push(sample),
            None => by_variant.push((variant, vec![sample])),
        }
    }
    fn mean(values: impl Iterator<Item = f64>) -> Option<f64> {
        let v: Vec<f64> = values.collect();
        (!v.is_empty()).then(|| v.iter().sum::<f64>() / v.len() as f64)
    }
    Ok(by_variant
        .into_iter()
        .map(|(variant, samples)| {
            let mut costs: Vec<f64> = samples.iter().filter_map(|s| s.cost).collect();
            costs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            VariantSummary {
                variant,
                runs: samples.len() as i64,
                passes: samples
                    .iter()
                    .filter(|s| s.outcome == Outcome::Pass)
                    .count() as i64,
                fails: samples
                    .iter()
                    .filter(|s| s.outcome == Outcome::Fail)
                    .count() as i64,
                unknown: samples
                    .iter()
                    .filter(|s| s.outcome == Outcome::Unknown)
                    .count() as i64,
                mean_cost: mean(costs.iter().copied()),
                p50_cost: (!costs.is_empty()).then(|| costs[(costs.len() - 1) / 2]),
                mean_turns: samples.iter().map(|s| s.turns as f64).sum::<f64>()
                    / samples.len().max(1) as f64,
                mean_wall_ms: mean(samples.iter().filter_map(|s| s.wall_ms.map(|w| w as f64))),
                mean_score: mean(samples.iter().filter_map(|s| s.score)),
            }
        })
        .collect())
}

/// What an interactive launch links itself to when the dialog names an
/// experiment: the same row the runner writes, judged `Unknown` because
/// nobody ran a check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExperimentLink {
    pub experiment: String,
    pub variant: String,
    /// The one-shot prompt, or blank for a conversation.
    pub prompt: String,
}

/// Records an interactive launch as a run of its experiment. Called when
/// the session ends, so the launch row is already in the store.
pub fn link_launch(
    db_path: &std::path::Path,
    launch_id: &str,
    link: &ExperimentLink,
    cwd: &std::path::Path,
    exit_code: Option<u32>,
) -> Result<(), String> {
    let conn = crate::tracing::store::open_aux(db_path)?;
    let id = upsert_experiment(
        &conn,
        &link.experiment,
        &link.prompt,
        Some(&cwd.to_string_lossy()),
        None,
    )
    .map_err(|e| e.to_string())?;
    let detail = serde_json::json!({ "exit_code": exit_code, "interactive": true });
    record_run(
        &conn,
        launch_id,
        &id,
        &link.variant,
        Outcome::Unknown,
        &detail,
    )
    .map_err(|e| e.to_string())
}

/// The per-variant table `trace experiments <name>` and the runner print.
pub fn summary_lines(rows: &[VariantSummary]) -> Vec<String> {
    use crate::tracing::cli::{fmt_cost, fmt_ms};
    let mut out = vec![format!(
        "{:<20} {:>4} {:>5} {:>5} {:>5} {:>9} {:>9} {:>6} {:>8} {:>6}",
        "variant", "runs", "pass", "fail", "?", "mean$", "p50$", "turns", "wall", "score"
    )];
    for v in rows {
        out.push(format!(
            "{:<20} {:>4} {:>5} {:>5} {:>5} {:>9} {:>9} {:>6.1} {:>8} {:>6}",
            v.variant.chars().take(20).collect::<String>(),
            v.runs,
            v.passes,
            v.fails,
            v.unknown,
            fmt_cost(v.mean_cost),
            fmt_cost(v.p50_cost),
            v.mean_turns,
            v.mean_wall_ms
                .map(|w| fmt_ms(w as i64))
                .unwrap_or_else(|| "-".into()),
            v.mean_score
                .map(|s| format!("{s:.2}"))
                .unwrap_or_else(|| "-".into()),
        ));
    }
    out
}

// ---------------------------------------------------------------- compare

/// One side of a comparison: a turn, or a whole session folded together.
#[derive(Debug, Clone)]
pub struct Side {
    pub label: String,
    pub metrics: LoopMetrics,
    /// Tool names in time order — the loop as a path.
    pub tools: Vec<String>,
    pub turns: i64,
    pub cost: Option<f64>,
    pub tokens: Option<i64>,
}

fn fold(turns: &[(TraceStat, Vec<ObservationView>)]) -> (LoopMetrics, Vec<String>) {
    let mut total = LoopMetrics::default();
    let mut tools = Vec::new();
    for (i, (t, obs)) in turns.iter().enumerate() {
        let m = loop_metrics(t, obs);
        total.tool_calls += m.tool_calls;
        total.tool_errors += m.tool_errors;
        total.declined += m.declined;
        total.retries += m.retries;
        total.model_ms += m.model_ms;
        total.tool_ms += m.tool_ms;
        total.idle_ms += m.idle_ms;
        total.compactions += m.compactions;
        total.subagents += m.subagents;
        total.subagent_tokens += m.subagent_tokens;
        total.subagent_cost += m.subagent_cost;
        if i == 0 {
            total.context_first = m.context_first;
        }
        if m.context_last.is_some() {
            total.context_last = m.context_last;
        }
        for (name, n) in m.retried_tools {
            match total.retried_tools.iter_mut().find(|(t, _)| *t == name) {
                Some((_, c)) => *c += n,
                None => total.retried_tools.push((name, n)),
            }
        }
        let mut ordered: Vec<&ObservationView> =
            obs.iter().filter(|o| o.obs_type == "tool").collect();
        ordered.sort_by_key(|o| o.start_ns);
        tools.extend(ordered.iter().map(|o| o.name.clone()));
    }
    let mut names: Vec<&str> = tools.iter().map(String::as_str).collect();
    names.sort_unstable();
    names.dedup();
    total.distinct_tools = names.len() as i64;
    if turns.len() == 1 {
        total.cache_ratio = loop_metrics(&turns[0].0, &turns[0].1).cache_ratio;
    }
    (total, tools)
}

/// Resolves a needle to a turn (trace id prefix) or, failing that, a
/// session, folding every turn of the session together.
pub fn resolve_side(conn: &Connection, needle: &str) -> rusqlite::Result<Option<Side>> {
    if let Some(t) = query::find_trace(conn, needle)? {
        let obs = query::list_observations(conn, &t.id)?;
        let cost = t.total_cost_usd;
        let tokens = t.total_tokens;
        let label = format!("turn #{} {}", t.ordinal, &t.id[..8.min(t.id.len())]);
        let (metrics, tools) = fold(&[(t, obs)]);
        return Ok(Some(Side {
            label,
            metrics,
            tools,
            turns: 1,
            cost,
            tokens,
        }));
    }
    let Some(session) = query::find_session(conn, needle)? else {
        return Ok(None);
    };
    let turns = query::list_traces(conn, &session.key)?;
    let mut pairs = Vec::new();
    for t in turns {
        let obs = query::list_observations(conn, &t.id)?;
        pairs.push((t, obs));
    }
    // `None` when nothing is priced or counted, not a zero
    let priced: Vec<f64> = pairs.iter().filter_map(|(t, _)| t.total_cost_usd).collect();
    let counted: Vec<i64> = pairs.iter().filter_map(|(t, _)| t.total_tokens).collect();
    let cost = (!priced.is_empty()).then(|| priced.iter().sum::<f64>());
    let tokens = (!counted.is_empty()).then(|| counted.iter().sum::<i64>());
    let n = pairs.len() as i64;
    let (metrics, tools) = fold(&pairs);
    Ok(Some(Side {
        label: format!(
            "session {} ({n} turns)",
            &session.session_id[..8.min(session.session_id.len())]
        ),
        metrics,
        tools,
        turns: n,
        cost,
        tokens,
    }))
}

/// A line diff of two tool sequences: `' '` common, `'-'` only in the
/// first, `'+'` only in the second. Longest common subsequence, which is
/// fine at the size of a turn's tool list.
pub fn diff(a: &[String], b: &[String]) -> Vec<(char, String)> {
    let (n, m) = (a.len(), b.len());
    let mut lcs = vec![vec![0usize; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i][j] = if a[i] == b[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }
    let mut out = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if a[i] == b[j] {
            out.push((' ', a[i].clone()));
            i += 1;
            j += 1;
        } else if lcs[i + 1][j] >= lcs[i][j + 1] {
            out.push(('-', a[i].clone()));
            i += 1;
        } else {
            out.push(('+', b[j].clone()));
            j += 1;
        }
    }
    out.extend(a[i..].iter().map(|s| ('-', s.clone())));
    out.extend(b[j..].iter().map(|s| ('+', s.clone())));
    out
}

// ----------------------------------------------------------------- runner

/// What one `agent-mux run` invocation asks for.
#[derive(Debug, Clone, PartialEq)]
pub struct RunSpec {
    pub experiment: String,
    pub variant: String,
    pub prompt: String,
    pub profile: Option<String>,
    pub harness: Harness,
    pub model: Option<String>,
    pub bypass: bool,
    pub cwd: PathBuf,
    pub check: Option<String>,
    pub repeat: u32,
    pub max_wait: Duration,
    /// Budget guard for each run (`--max-cost`, `--max-turns`).
    pub max_cost_usd: Option<f64>,
    pub max_turns: Option<u32>,
}

/// The profile a run launches: the named one, else the first whose
/// command is the harness, else a bare `<harness>` command — with the
/// run's own options composed in.
pub fn profile_for(spec: &RunSpec, profiles: &[Profile]) -> Profile {
    let base = spec
        .profile
        .as_ref()
        .and_then(|name| profiles.iter().find(|p| p.name.eq_ignore_ascii_case(name)))
        .or_else(|| {
            profiles
                .iter()
                .find(|p| Harness::detect(&p.command) == Some(spec.harness))
        })
        .cloned()
        .unwrap_or_else(|| Profile {
            name: spec.harness.as_str().to_string(),
            command: spec.harness.as_str().to_string(),
            args: vec![],
            default_dir: None,
            tracing: None,
            model: None,
            bypass_approvals: None,
        });
    let options = LaunchOptions {
        model: spec.model.clone().or_else(|| base.model.clone()),
        bypass_approvals: spec.bypass || base.bypass_approvals.unwrap_or(false),
        resume: Default::default(),
        one_shot: Some(spec.prompt.clone()),
    };
    let harness = Harness::detect(&base.command).unwrap_or(spec.harness);
    let mut profile = base;
    profile.args = compose(&profile.args, &options.render(harness));
    profile
}

/// The outcome of the check command in the run's directory: pass on
/// exit 0, fail otherwise, unknown when there is no check.
pub fn judge(check: Option<&str>, cwd: &std::path::Path) -> (Outcome, Option<i32>) {
    let Some(cmd) = check else {
        return (Outcome::Unknown, None);
    };
    #[cfg(windows)]
    let status = std::process::Command::new("cmd")
        .args(["/C", cmd])
        .current_dir(cwd)
        .status();
    #[cfg(not(windows))]
    let status = std::process::Command::new("sh")
        .args(["-c", cmd])
        .current_dir(cwd)
        .status();
    match status {
        Ok(s) if s.success() => (Outcome::Pass, s.code()),
        Ok(s) => (Outcome::Fail, s.code()),
        Err(_) => (Outcome::Fail, None),
    }
}

/// What one run produced, for the record and the terminal.
#[derive(Debug, Clone, PartialEq)]
pub struct RunResult {
    pub launch_id: String,
    pub exit_code: Option<u32>,
    pub outcome: Outcome,
    pub check_code: Option<i32>,
    pub wall: Duration,
    pub timed_out: bool,
}

/// Runs one launch headlessly through the same app path the dialog
/// uses — plan, trace, spawn, pump the PTY until exit — then shuts the
/// trace runtime down so every row is flushed before the check runs.
pub async fn run_once(
    resolved: &crate::config::ResolvedTracing,
    profiles: Vec<Profile>,
    profile: Profile,
    spec: &RunSpec,
) -> anyhow::Result<RunResult> {
    use crate::events::AppEvent;
    use crate::status::Status;
    let (tx, mut rx) = tokio::sync::mpsc::channel(1024);
    let runtime = crate::tracing::TraceRuntime::new(resolved.clone(), tx.clone())
        .map_err(|e| anyhow::anyhow!("trace store: {e}"))?;
    let mut app = crate::app::App::new(profiles, Some(runtime), tx);
    app.clipboard_enabled = false;
    // A Claude one-shot skips the planner's id injection (the hook channel
    // usually announces it); the runner owns the launch, so it pins the
    // session itself and correlates whether or not hooks are on.
    let mut profile = profile;
    if Harness::detect(&profile.command) == Some(Harness::Claude)
        && !profile
            .args
            .iter()
            .any(|a| a == "--session-id" || a.starts_with("--session-id="))
    {
        profile
            .args
            .extend(["--session-id".to_string(), uuid::Uuid::new_v4().to_string()]);
    }
    if spec.max_cost_usd.is_some() || spec.max_turns.is_some() {
        let mut t = profile.tracing.take().unwrap_or_default();
        t.max_cost_usd = spec.max_cost_usd.or(t.max_cost_usd);
        t.max_turns = spec.max_turns.or(t.max_turns);
        profile.tracing = Some(t);
    }
    let idx = app.launch(profile, spec.cwd.clone())?;
    let launch_id = app.sessions[idx]
        .trace
        .as_ref()
        .map(|t| t.launch_id.clone())
        .ok_or_else(|| anyhow::anyhow!("the profile is untraced: a run needs its launch row"))?;
    let started = Instant::now();
    let mut timed_out = false;
    let exit_code = loop {
        if let Status::Exited(code) = app.sessions[idx].status(Instant::now()) {
            break code;
        }
        if started.elapsed() > spec.max_wait {
            timed_out = true;
            break None;
        }
        match tokio::time::timeout(Duration::from_millis(200), rx.recv()).await {
            Ok(Some(AppEvent::PtyOutput { id, bytes })) => {
                app.handle_pty_output(id, &bytes, Instant::now())
            }
            Ok(Some(AppEvent::PtyExit { id })) => app.handle_pty_exit(id),
            Ok(Some(AppEvent::TraceStats { launch_id, stats })) => {
                app.handle_trace_stats(&launch_id, stats)
            }
            Ok(Some(_)) | Err(_) => {}
            Ok(None) => break None,
        }
    };
    let wall = started.elapsed();
    app.kill_all();
    if let Some(rt) = app.take_tracing() {
        rt.shutdown(Duration::from_secs(5)).await;
    }
    let (outcome, check_code) = if timed_out {
        (Outcome::Fail, None)
    } else {
        judge(spec.check.as_deref(), &spec.cwd)
    };
    Ok(RunResult {
        launch_id,
        exit_code,
        outcome,
        check_code,
        wall,
        timed_out,
    })
}

/// The last thing the model said in a launch, from the store.
pub fn final_message(conn: &Connection, launch_id: &str) -> rusqlite::Result<Option<String>> {
    conn.query_row(
        "SELECT output FROM traces WHERE launch_id = ?1 ORDER BY ordinal DESC LIMIT 1",
        params![launch_id],
        |r| r.get::<_, Option<String>>(0),
    )
    .optional()
    .map(|o| o.flatten())
}

/// `agent-mux run …`: parses the arguments, runs the variant `--repeat`
/// times, records each run, and prints the experiment's summary.
pub async fn run_cli(raw: &[String]) -> anyhow::Result<()> {
    if raw.iter().any(|a| a == "-h" || a == "--help") {
        println!("{RUN_USAGE}");
        return Ok(());
    }
    let spec = parse_run_args(raw)?;
    let cfg = crate::config::load()?;
    let resolved = crate::config::resolve_tracing(cfg.tracing.as_ref(), &|k| std::env::var(k).ok())
        .ok_or_else(|| anyhow::anyhow!("tracing is disabled: a run needs the trace store"))?;
    let profiles = cfg.profiles.clone();
    let profile = profile_for(&spec, &profiles);
    println!(
        "experiment {} · variant {} · {} run(s)\n  {} {}\n  in {}",
        spec.experiment,
        spec.variant,
        spec.repeat,
        profile.command,
        profile.args.join(" "),
        spec.cwd.display()
    );
    let mut experiment_id: Option<String> = None;
    for i in 1..=spec.repeat {
        let result = run_once(&resolved, profiles.clone(), profile.clone(), &spec).await?;
        // the store exists now (the run created it if it did not)
        let conn =
            crate::tracing::store::open_aux(&resolved.db_path).map_err(|e| anyhow::anyhow!(e))?;
        let id = match &experiment_id {
            Some(id) => id.clone(),
            None => {
                let id = upsert_experiment(
                    &conn,
                    &spec.experiment,
                    &spec.prompt,
                    Some(&spec.cwd.to_string_lossy()),
                    spec.check.as_deref(),
                )?;
                experiment_id = Some(id.clone());
                id
            }
        };
        let message = final_message(&conn, &result.launch_id)?;
        let detail = serde_json::json!({
            "exit_code": result.exit_code,
            "check_code": result.check_code,
            "wall_ms": result.wall.as_millis() as u64,
            "timed_out": result.timed_out,
            "final_message": message.as_deref().map(|m| m.chars().take(2000).collect::<String>()),
        });
        record_run(
            &conn,
            &result.launch_id,
            &id,
            &spec.variant,
            result.outcome,
            &detail,
        )?;
        println!(
            "  run {i}/{}: exit {} · {} · {} · launch {}",
            spec.repeat,
            result
                .exit_code
                .map(|c| c.to_string())
                .unwrap_or_else(|| if result.timed_out {
                    "timeout".into()
                } else {
                    "?".into()
                }),
            match result.outcome {
                Outcome::Pass => "check passed".to_string(),
                Outcome::Fail => format!(
                    "check failed{}",
                    result
                        .check_code
                        .map(|c| format!(" ({c})"))
                        .unwrap_or_default()
                ),
                Outcome::Unknown => "no check".to_string(),
            },
            crate::tracing::cli::fmt_ms(result.wall.as_millis() as i64),
            &result.launch_id[..8.min(result.launch_id.len())]
        );
    }
    let conn =
        crate::tracing::store::open_aux(&resolved.db_path).map_err(|e| anyhow::anyhow!(e))?;
    println!();
    for line in summary_lines(&experiment_summary(&conn, &spec.experiment)?) {
        println!("{line}");
    }
    Ok(())
}

pub const RUN_USAGE: &str =
    "usage: agent-mux run --experiment <name> --variant <label> --prompt <text>
            [--harness claude|codex|agy] [--profile <name>] [--model <id>] [--bypass]
            [--cwd <dir>] [--check <command>] [--repeat <n>] [--timeout <secs>]
            [--max-cost <usd>] [--max-turns <n>]

Runs the prompt non-interactively through the named harness (or profile) as a
traced launch labelled with the experiment and variant, judges it with --check
(pass on exit 0) in --cwd, records the run, and prints the experiment's
per-variant summary. Variants that edit files need separate checkouts: the
runner does not prepare the directory.";

pub fn parse_run_args(raw: &[String]) -> anyhow::Result<RunSpec> {
    let mut values: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let mut bypass = false;
    let mut i = 0;
    while i < raw.len() {
        let a = &raw[i];
        match a.as_str() {
            "--bypass" => bypass = true,
            "-h" | "--help" => anyhow::bail!("{RUN_USAGE}"),
            _ => {
                let Some(key) = a.strip_prefix("--") else {
                    anyhow::bail!("unexpected argument {a:?}\n{RUN_USAGE}");
                };
                if let Some((k, v)) = key.split_once('=') {
                    values.insert(k.to_string(), v.to_string());
                } else {
                    let v = raw
                        .get(i + 1)
                        .ok_or_else(|| anyhow::anyhow!("--{key} needs a value\n{RUN_USAGE}"))?;
                    values.insert(key.to_string(), v.clone());
                    i += 1;
                }
            }
        }
        i += 1;
    }
    let required = |k: &str| {
        values
            .get(k)
            .cloned()
            .filter(|v| !v.trim().is_empty())
            .ok_or_else(|| anyhow::anyhow!("--{k} is required\n{RUN_USAGE}"))
    };
    let harness = match values.get("harness").map(|s| s.to_ascii_lowercase()) {
        None => Harness::Claude,
        Some(h) => Harness::detect(&h)
            .or_else(|| (h == "antigravity").then_some(Harness::Antigravity))
            .ok_or_else(|| anyhow::anyhow!("unknown harness {h:?}: claude, codex or agy"))?,
    };
    let cwd = values
        .get("cwd")
        .map(PathBuf::from)
        .unwrap_or(std::env::current_dir()?);
    Ok(RunSpec {
        max_cost_usd: values
            .get("max-cost")
            .map(|v| v.parse::<f64>())
            .transpose()?
            .filter(|c| *c > 0.0),
        max_turns: values
            .get("max-turns")
            .map(|v| v.parse::<u32>())
            .transpose()?
            .filter(|n| *n > 0),
        experiment: required("experiment")?,
        variant: required("variant")?,
        prompt: required("prompt")?,
        profile: values.get("profile").cloned(),
        harness,
        model: values
            .get("model")
            .cloned()
            .filter(|m| !m.trim().is_empty()),
        bypass,
        cwd,
        check: values
            .get("check")
            .cloned()
            .filter(|c| !c.trim().is_empty()),
        repeat: values
            .get("repeat")
            .map(|r| r.parse::<u32>())
            .transpose()?
            .unwrap_or(1)
            .max(1),
        max_wait: Duration::from_secs(
            values
                .get("timeout")
                .map(|t| t.parse::<u64>())
                .transpose()?
                .unwrap_or(600),
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_sequences_diff_as_paths() {
        let a: Vec<String> = ["Read", "Edit", "Bash", "Bash"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let b: Vec<String> = ["Read", "Grep", "Edit", "Bash"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let d = diff(&a, &b);
        let rendered: Vec<String> = d.iter().map(|(c, s)| format!("{c}{s}")).collect();
        assert_eq!(rendered, vec![" Read", "+Grep", " Edit", " Bash", "-Bash"]);
        assert!(diff(&[], &[]).is_empty());
        assert_eq!(diff(&a, &a).iter().filter(|(c, _)| *c != ' ').count(), 0);
    }

    #[test]
    fn a_run_profile_composes_the_prompt_and_options_for_its_harness() {
        let spec = RunSpec {
            max_cost_usd: None,
            max_turns: None,
            experiment: "e".into(),
            variant: "v".into(),
            prompt: "fix the failing test".into(),
            profile: None,
            harness: Harness::Codex,
            model: Some("gpt-5.6".into()),
            bypass: true,
            cwd: PathBuf::from("/tmp"),
            check: None,
            repeat: 1,
            max_wait: Duration::from_secs(1),
        };
        let p = profile_for(&spec, &[]);
        assert_eq!(p.command, "codex");
        assert_eq!(
            p.args,
            vec![
                "exec",
                "--model",
                "gpt-5.6",
                "--yolo",
                "fix the failing test"
            ]
        );
        // a named profile wins and keeps its own arguments in the middle
        let named = Profile {
            name: "My Claude".into(),
            command: "claude".into(),
            args: vec!["--verbose".into()],
            default_dir: None,
            tracing: None,
            model: Some("claude-opus-5".into()),
            bypass_approvals: None,
        };
        let spec = RunSpec {
            profile: Some("my claude".into()),
            model: None,
            bypass: false,
            ..spec
        };
        let p = profile_for(&spec, &[named]);
        assert_eq!(p.command, "claude");
        assert_eq!(
            p.args,
            vec![
                "--verbose",
                "--model",
                "claude-opus-5",
                "-p",
                "fix the failing test"
            ],
            "the profile's model default applies when the run names none"
        );
    }

    #[test]
    fn the_check_command_decides_the_outcome() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(judge(None, dir.path()), (Outcome::Unknown, None));
        #[cfg(not(windows))]
        {
            assert_eq!(judge(Some("true"), dir.path()).0, Outcome::Pass);
            let (o, code) = judge(Some("exit 3"), dir.path());
            assert_eq!((o, code), (Outcome::Fail, Some(3)));
            // the check runs in the run's own directory
            std::fs::write(dir.path().join("marker"), "x").unwrap();
            assert_eq!(judge(Some("test -f marker"), dir.path()).0, Outcome::Pass);
        }
    }
}
