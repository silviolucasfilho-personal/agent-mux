//! Scheduled agents: an agent file with `[task]` and `[schedule]` is a
//! loop (agent-first spec, phase 3). The file holds the settings; the loop
//! registry (`loops.json`) keeps the run state (next run, last run,
//! pause, breaker) and a complete copy of the settings, so `agent-mux
//! loop …` and older builds keep working.
//!
//! - `load_registry` reads the registry and lays every library agent's
//!   settings over it (`overlay`); an agent without an entry gets one.
//! - `write_entry` writes a loop's settings into its agent file, keeping
//!   what the file says that a loop has no field for (instructions,
//!   backends, other tools).
//! - `migrate` plans (and with `write` performs) an agent file for every
//!   registry entry that has none. Nothing is deleted; `loops.json` is
//!   copied to `loops.json.bak` before it changes.

use super::{AgentSpec, Catalog, Source, Tool};
use crate::loops::registry::{self, LoopEntry, Registry};
use crate::loops::{Level, patterns};
use std::path::{Path, PathBuf};
use time::OffsetDateTime;

/// The level an agent's tools give its runs: with `edit` it works in a
/// worktree, without it it reports.
pub fn level_of(spec: &AgentSpec) -> Level {
    Level::from_edits(spec.can(&Tool::Edit))
}

fn expand_home(p: &str) -> PathBuf {
    match p.strip_prefix("~/") {
        Some(rest) => crate::skill::install::home_dir().join(rest),
        None => PathBuf::from(p),
    }
}

/// `~/code/api` for a path under the home directory.
fn tilde(p: &Path) -> String {
    let home = crate::skill::install::home_dir();
    match p.strip_prefix(&home) {
        Ok(rest) if !home.as_os_str().is_empty() => format!("~/{}", rest.display()),
        _ => p.display().to_string(),
    }
}

/// The registry at `path` with the library's scheduled agents laid over
/// it; problems name the agents that were skipped and why.
pub fn load_registry(path: &Path, library_root: &Path) -> (Registry, Vec<String>) {
    let mut reg = registry::load(path);
    let problems = overlay(&mut reg, library_root, crate::loops::now());
    (reg, problems)
}

/// Applies every library agent with `[task]` and `[schedule]` to `reg`.
pub fn overlay(reg: &mut Registry, library_root: &Path, now: OffsetDateTime) -> Vec<String> {
    let catalog = Catalog::load(library_root, None);
    let mut problems = Vec::new();
    for e in &catalog.entries {
        if !matches!(e.source, Source::Library(_)) {
            continue;
        }
        let Some(spec) = &e.spec else { continue };
        let (Some(task), Some(sched)) = (&spec.task, &spec.schedule) else {
            continue;
        };
        let Some(pattern) = patterns::find(&task.pattern) else {
            problems.push(format!(
                "agent {}: [task] pattern {:?} is not in the library",
                e.name, task.pattern
            ));
            continue;
        };
        let at = reg
            .loops
            .iter()
            .position(|l| l.agent.as_deref() == Some(e.name.as_str()));
        let i = match at {
            Some(i) => i,
            None => {
                let mut entry = registry::new_entry(
                    &expand_home(&sched.workspace),
                    pattern,
                    sched.harness.as_deref().unwrap_or("claude"),
                    &sched.profile,
                    sched.interval_s,
                    level_of(spec),
                    now,
                );
                if reg.find(&e.name).is_none() {
                    entry.id = e.name.clone();
                }
                entry.agent = Some(e.name.clone());
                reg.add(entry);
                reg.loops.len() - 1
            }
        };
        apply(&mut reg.loops[i], spec, pattern, now);
    }
    problems
}

/// The agent's settings on its registry entry; the state stays.
fn apply(
    entry: &mut LoopEntry,
    spec: &AgentSpec,
    pattern: &crate::loops::Pattern,
    now: OffsetDateTime,
) {
    let (Some(task), Some(sched)) = (&spec.task, &spec.schedule) else {
        return;
    };
    entry.workspace = expand_home(&sched.workspace);
    entry.pattern = pattern.id.clone();
    entry.profile = sched.profile.clone();
    if let Some(h) = &sched.harness {
        entry.harness = h.clone();
    }
    entry.model = spec.model_for(&entry.harness).unwrap_or_default();
    entry.verifier_model = task.verifier_model.clone().unwrap_or_default();
    if entry.interval_s != sched.interval_s {
        entry.interval_s = sched.interval_s;
        entry.set_next_run(now + time::Duration::seconds(sched.interval_s as i64));
    }
    entry.level = level_of(spec);
    let limits = spec.limits.clone().unwrap_or_default();
    entry.max_runs_per_day = limits.runs_per_day.unwrap_or(pattern.max_runs_per_day);
    entry.max_tokens_per_day = limits.tokens_per_day.unwrap_or(pattern.max_tokens_per_day);
    entry.max_cost_usd_per_run = limits.usd_per_run;
}

/// Where the library keeps agent `name`.
pub fn file_of(library_root: &Path, name: &str) -> PathBuf {
    super::library_dir(library_root).join(format!("{name}.toml"))
}

/// The agent file's text for `entry`, laid over `existing` (the file as
/// it stands, or empty for a new one).
pub fn render_entry(name: &str, entry: &LoopEntry, existing: &str) -> Result<String, String> {
    let mut t: toml::Table = if existing.trim().is_empty() {
        toml::Table::new()
    } else {
        existing.parse().map_err(|e| format!("{name}.toml: {e}"))?
    };
    let pattern = patterns::find(&entry.pattern);
    let s = |v: &str| toml::Value::String(v.to_string());
    t.insert("name".into(), s(name));
    if !t.contains_key("description") {
        let what = pattern
            .map(|p| p.name.clone())
            .unwrap_or(entry.pattern.clone());
        t.insert(
            "description".into(),
            s(&format!("{what} in {}", entry.workspace_name())),
        );
    }
    // the tools say what a run may change: edit or not
    let mut tools: Vec<String> = match t.get("tools").and_then(|v| v.as_array()) {
        Some(a) => a
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect(),
        None => vec!["read".into(), "shell".into()],
    };
    tools.retain(|x| x != "edit");
    if entry.level.edits() {
        tools.insert(1.min(tools.len()), "edit".into());
    }
    t.insert(
        "tools".into(),
        toml::Value::Array(tools.iter().map(|x| s(x)).collect()),
    );
    if entry.model.is_empty() {
        t.remove("model");
    } else {
        t.insert("model".into(), s(&entry.model));
    }
    let mut task = toml::Table::new();
    task.insert("pattern".into(), s(&entry.pattern));
    if !entry.verifier_model.is_empty() {
        task.insert("verifier_model".into(), s(&entry.verifier_model));
    }
    t.insert("task".into(), toml::Value::Table(task));
    let mut sched = toml::Table::new();
    sched.insert(
        "every".into(),
        s(&crate::loops::format_interval(entry.interval_s)),
    );
    sched.insert("workspace".into(), s(&tilde(&entry.workspace)));
    sched.insert("profile".into(), s(&entry.profile));
    sched.insert("harness".into(), s(&entry.harness));
    t.insert("schedule".into(), toml::Value::Table(sched));
    let mut limits = toml::Table::new();
    limits.insert(
        "runs_per_day".into(),
        toml::Value::Integer(entry.max_runs_per_day as i64),
    );
    limits.insert(
        "tokens_per_day".into(),
        toml::Value::Integer(entry.max_tokens_per_day as i64),
    );
    if let Some(c) = entry.max_cost_usd_per_run {
        limits.insert("usd_per_run".into(), toml::Value::Float(c));
    }
    t.insert("limits".into(), toml::Value::Table(limits));
    let text = ordered(t)?;
    AgentSpec::parse(&text).map_err(|p| format!("{name}.toml: {}", p.join("; ")))?;
    Ok(text)
}

/// The table as TOML with the agent's keys first and its tables in the
/// order the guide explains them (a `toml::Table` sorts its keys).
fn ordered(mut t: toml::Table) -> Result<String, String> {
    const FIRST: [&str; 9] = [
        "name",
        "description",
        "instructions",
        "tools",
        "model",
        "effort",
        "task",
        "schedule",
        "limits",
    ];
    let mut keys: Vec<String> = FIRST
        .iter()
        .filter(|k| t.contains_key(**k))
        .map(|k| k.to_string())
        .collect();
    let rest: Vec<String> = t.keys().filter(|k| !keys.contains(k)).cloned().collect();
    // plain values before any table, or TOML would read them into it
    let (tables, values): (Vec<String>, Vec<String>) = keys
        .drain(..)
        .chain(rest)
        .partition(|k| t.get(k).is_some_and(|v| v.is_table()));
    let mut out = String::new();
    for k in values.into_iter().chain(tables) {
        if let Some(v) = t.remove(&k) {
            let mut one = toml::Table::new();
            one.insert(k, v);
            let part = toml::to_string(&one).map_err(|e| e.to_string())?;
            if part.starts_with('[') && !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&part);
        }
    }
    Ok(out)
}

/// Writes `entry`'s settings into its agent file (`entry.agent`).
pub fn write_entry(library_root: &Path, entry: &LoopEntry) -> Result<PathBuf, String> {
    let name = entry.agent.as_deref().ok_or("the loop has no agent file")?;
    let path = file_of(library_root, name);
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let text = render_entry(name, entry, &existing)?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(path)
}

/// Deletes the agent file of a loop that is removed, so the overlay does
/// not bring it back.
pub fn remove_file(library_root: &Path, entry: &LoopEntry) -> std::io::Result<Option<PathBuf>> {
    let Some(name) = &entry.agent else {
        return Ok(None);
    };
    let path = file_of(library_root, name);
    if path.exists() {
        std::fs::remove_file(&path)?;
        return Ok(Some(path));
    }
    Ok(None)
}

/// A free agent name for a loop of `pattern` in `workspace`: the pattern,
/// else the pattern and the workspace's folder, else numbered.
pub fn free_name(library_root: &Path, entry: &LoopEntry, taken: &[String]) -> String {
    let clean = |s: &str| -> String {
        let mut out: String = s
            .to_ascii_lowercase()
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '_' {
                    c
                } else {
                    '-'
                }
            })
            .collect();
        while out.contains("--") {
            out = out.replace("--", "-");
        }
        out.trim_matches('-').to_string()
    };
    let base = clean(&entry.pattern);
    let base = if base.starts_with(|c: char| c.is_ascii_lowercase()) {
        base
    } else {
        format!("loop-{base}")
    };
    let used = |n: &str| {
        taken.iter().any(|t| t == n)
            || file_of(library_root, n).exists()
            || super::builtin(n).is_some()
    };
    let ws = clean(&entry.workspace_name());
    let candidates = std::iter::once(base.clone())
        .chain((!ws.is_empty()).then(|| format!("{base}-{ws}")))
        .chain((2..).map(|i| format!("{base}-{i}")));
    for c in candidates {
        if !used(&c) {
            return c;
        }
    }
    unreachable!("the numbered names never run out")
}

/// One registry entry and the agent file it becomes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Move {
    pub loop_id: String,
    pub pattern: String,
    pub workspace: PathBuf,
    pub name: String,
    pub path: PathBuf,
}

/// What `migrate` did or would do.
#[derive(Debug, Default)]
pub struct Migration {
    pub moves: Vec<Move>,
    /// Entries left as they are, and why.
    pub skipped: Vec<(String, String)>,
    /// The copy of `loops.json` taken before it changed.
    pub backup: Option<PathBuf>,
}

/// Plans an agent file for every registry entry without one; with
/// `write`, writes them, links the entries and saves the registry (after
/// copying it to `loops.json.bak`).
pub fn migrate(
    registry_path: &Path,
    library_root: &Path,
    write: bool,
) -> Result<Migration, String> {
    let mut reg = registry::load(registry_path);
    let mut out = Migration::default();
    let mut taken: Vec<String> = Vec::new();
    for entry in &reg.loops {
        if entry.agent.is_some() {
            continue;
        }
        if patterns::find(&entry.pattern).is_none() {
            out.skipped.push((
                entry.id.clone(),
                format!("pattern {:?} is not in the library", entry.pattern),
            ));
            continue;
        }
        let name = free_name(library_root, entry, &taken);
        taken.push(name.clone());
        out.moves.push(Move {
            loop_id: entry.id.clone(),
            pattern: entry.pattern.clone(),
            workspace: entry.workspace.clone(),
            path: file_of(library_root, &name),
            name,
        });
    }
    if !write || out.moves.is_empty() {
        return Ok(out);
    }
    if registry_path.exists() {
        let bak = registry_path.with_extension("json.bak");
        std::fs::copy(registry_path, &bak).map_err(|e| format!("{}: {e}", bak.display()))?;
        out.backup = Some(bak);
    }
    for m in &out.moves {
        if let Some(e) = reg.find_mut(&m.loop_id) {
            e.agent = Some(m.name.clone());
            write_entry(library_root, e)?;
        }
    }
    registry::save(registry_path, &reg).map_err(|e| format!("{}: {e}", registry_path.display()))?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(ws: &Path, level: Level) -> LoopEntry {
        let p = patterns::find("daily-triage").unwrap();
        let mut e = registry::new_entry(ws, p, "codex", "Codex", 3600, level, crate::loops::now());
        e.model = "gpt-5.5".into();
        e.max_cost_usd_per_run = Some(0.5);
        e
    }

    #[test]
    fn a_rendered_entry_parses_and_says_what_it_may_change() {
        let temp = tempfile::tempdir().unwrap();
        let text = render_entry("triage", &entry(temp.path(), Level::L2), "").unwrap();
        let spec = AgentSpec::parse(&text).unwrap();
        assert_eq!(spec.task.as_ref().unwrap().pattern, "daily-triage");
        let s = spec.schedule.as_ref().unwrap();
        assert_eq!((s.interval_s, s.profile.as_str()), (3600, "Codex"));
        assert_eq!(s.harness.as_deref(), Some("codex"));
        assert_eq!(level_of(&spec), Level::L2, "{text}");
        assert_eq!(spec.model.as_deref(), Some("gpt-5.5"));
        assert_eq!(spec.limits.as_ref().unwrap().usd_per_run, Some(0.5));

        // re-rendered over itself with editing off: edit goes, the rest stays
        let edited = format!("{text}\n[backends.codex]\neffort = \"high\"\n");
        let again = render_entry("triage", &entry(temp.path(), Level::L1), &edited).unwrap();
        let spec = AgentSpec::parse(&again).unwrap();
        assert_eq!(level_of(&spec), Level::L1);
        assert_eq!(spec.effort_for("codex").as_deref(), Some("high"), "{again}");
    }

    #[test]
    fn a_schedule_needs_a_task_and_a_valid_interval() {
        let base = "name = \"x\"\ndescription = \"d\"\n";
        let e = AgentSpec::parse(&format!(
            "{base}instructions = \"i\"\n[schedule]\nevery = \"1h\"\nworkspace = \"/w\"\n"
        ))
        .unwrap_err();
        assert!(e.join(" ").contains("needs a [task]"), "{e:?}");
        let e = AgentSpec::parse(&format!(
            "{base}[task]\npattern = \"daily-triage\"\n[schedule]\nevery = \"1m\"\nworkspace = \"/w\"\n"
        ))
        .unwrap_err();
        assert!(e.join(" ").contains("at least 5m"), "{e:?}");
        // a task agent needs no instructions
        assert!(AgentSpec::parse(&format!("{base}[task]\npattern = \"daily-triage\"\n")).is_ok());
    }

    #[test]
    fn migrate_plans_then_writes_and_the_overlay_reads_it_back() {
        let temp = tempfile::tempdir().unwrap();
        let lib = temp.path().join("library");
        let path = temp.path().join("loops.json");
        let ws = temp.path().join("api");
        std::fs::create_dir_all(&ws).unwrap();
        let mut reg = Registry::default();
        let a = entry(&ws, Level::L1);
        let mut b = entry(&ws, Level::L2);
        b.interval_s = 7200;
        let (a_id, b_id) = (a.id.clone(), b.id.clone());
        reg.add(a);
        reg.add(b);
        registry::save(&path, &reg).unwrap();

        // a dry run names the files and changes nothing
        let plan = migrate(&path, &lib, false).unwrap();
        let names: Vec<&str> = plan.moves.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, ["daily-triage", "daily-triage-api"]);
        assert!(!lib.join("agents").exists());
        assert!(
            registry::load(&path)
                .loops
                .iter()
                .all(|l| l.agent.is_none())
        );

        let done = migrate(&path, &lib, true).unwrap();
        assert!(done.backup.as_ref().unwrap().exists());
        assert!(lib.join("agents/daily-triage.toml").exists());
        let reg = registry::load(&path);
        assert_eq!(
            reg.find(&a_id).unwrap().agent.as_deref(),
            Some("daily-triage")
        );
        // a second run has nothing to do
        assert!(migrate(&path, &lib, true).unwrap().moves.is_empty());

        // the file is the source of the settings now
        let file = lib.join("agents/daily-triage-api.toml");
        let text = std::fs::read_to_string(&file)
            .unwrap()
            .replace("every = \"2h\"", "every = \"30m\"");
        std::fs::write(&file, text).unwrap();
        let (reg, problems) = load_registry(&path, &lib);
        assert!(problems.is_empty(), "{problems:?}");
        let b = reg.find(&b_id).unwrap();
        assert_eq!(b.interval_s, 1800);
        assert_eq!(b.level, Level::L2);
        assert_eq!(reg.loops.len(), 2, "no entry was added twice");

        // an agent file alone makes a loop
        std::fs::write(
            lib.join("agents/nightly.toml"),
            format!(
                "name = \"nightly\"\ndescription = \"d\"\ntools = [\"read\"]\n[task]\npattern = \"daily-triage\"\n[schedule]\nevery = \"1d\"\nworkspace = \"{}\"\nprofile = \"Claude Code\"\n",
                ws.display()
            ),
        )
        .unwrap();
        let (reg, _) = load_registry(&path, &lib);
        let n = reg.find("nightly").unwrap();
        assert_eq!(
            (n.agent.as_deref(), n.interval_s),
            (Some("nightly"), 86_400)
        );
        assert_eq!(n.level, Level::L1);

        // removing the file's loop removes the file
        assert!(remove_file(&lib, n).unwrap().is_some());
        let (reg, _) = load_registry(&path, &lib);
        assert!(reg.find("nightly").is_none());
    }
}
