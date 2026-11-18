//! `agent-mux project …`: a repository configured with spec-wave, read as
//! a Project. Phase 1: every command reads.

use super::config::{self, ProjectConfig};
use super::contract::stage_plain;
use super::next::{self, Decision};
use super::{DocState, Item, ItemType, Snapshot, doctor, order, snapshot, stage, store};
use crate::factory::deliver::Tools;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

const USAGE: &str = "agent-mux project — a spec-wave repository's work items, read from GitHub

  agent-mux project doctor [--json]          what can be read, and what to fix
  agent-mux project sync                     read the Project from GitHub into the cache
  agent-mux project ls [--type T] [--stage S] [--milestone M] [--all] [--json]
  agent-mux project show <n> [--json]        one item: stage, documents, pull requests, next step
  agent-mux project next <n> [--json]        the item's next step and why; runs nothing
  agent-mux project order [<feature>] [--milestone M] [--json]
        the Stories of a Feature in execution order; with no Feature, every one with work

  every command: [--dir DIR] [--refresh] [--cached]

The folder (or one above it) must hold .spec-wave.json. In a code repository
the Project shown is its hub's. A read is cached for `cache.ttlSec` of that
file (600 s when unset): --refresh reads again, --cached never does.
Nothing here writes to GitHub or to .spec-wave.json. docs/project.md has the rest.";

const VALUE_FLAGS: &[&str] = &["--dir", "--type", "--stage", "--milestone"];

struct Args {
    values: Vec<(String, String)>,
    flags: Vec<String>,
    positional: Vec<String>,
}

impl Args {
    fn parse(raw: &[String]) -> Args {
        let mut a = Args {
            values: Vec::new(),
            flags: Vec::new(),
            positional: Vec::new(),
        };
        let mut i = 0;
        while i < raw.len() {
            let t = &raw[i];
            if VALUE_FLAGS.contains(&t.as_str())
                && let Some(v) = raw.get(i + 1)
            {
                a.values.push((t.clone(), v.clone()));
                i += 2;
                continue;
            }
            if let Some((k, v)) = t.split_once('=')
                && VALUE_FLAGS.contains(&k)
            {
                a.values.push((k.to_string(), v.to_string()));
            } else if t.starts_with("--") {
                a.flags.push(t.clone());
            } else {
                a.positional.push(t.clone());
            }
            i += 1;
        }
        a
    }

    fn value(&self, k: &str) -> Option<&str> {
        self.values
            .iter()
            .find(|(f, _)| f == k)
            .map(|(_, v)| v.as_str())
    }

    fn flag(&self, k: &str) -> bool {
        self.flags.iter().any(|f| f == k)
    }
}

/// How fresh a snapshot must be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fresh {
    /// The cache when it is younger than the Project's TTL, else a read.
    Auto,
    /// Always read GitHub.
    Refresh,
    /// Never read GitHub.
    Cached,
}

/// The trace store's path, or why there is none.
pub fn db_path() -> Result<PathBuf, String> {
    let cfg = crate::config::load().map_err(|e| e.to_string())?;
    crate::config::resolve_tracing(cfg.tracing.as_ref(), &|k| std::env::var(k).ok())
        .map(|r| r.db_path)
        .ok_or_else(|| "tracing is disabled; a Project is cached in the trace store".to_string())
}

/// A read-write connection to the store, created when it is not there.
pub fn open_store(db: &Path) -> Result<rusqlite::Connection, String> {
    if !db.is_file() {
        if let Some(dir) = db.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        crate::tracing::store::open_rw(db, crate::tracing::store::OpenOptions::default())?;
    }
    crate::tracing::store::open_aux(db)
}

/// The cache's lifetime for a Project, in seconds: `cache.ttlSec`, 600
/// when unset, 0 turns it off.
pub fn ttl_secs(cfg: &ProjectConfig) -> i64 {
    cfg.pointer("/cache/ttlSec")
        .and_then(Value::as_i64)
        .unwrap_or(600)
        .max(0)
}

/// A snapshot of the Project: from the cache when `fresh` allows it,
/// else from GitHub, saving what was read.
pub fn obtain(
    conn: &rusqlite::Connection,
    tools: &Tools,
    cfg: &ProjectConfig,
    fresh: Fresh,
) -> Result<Snapshot, String> {
    let hub = cfg.hub_repo().slug();
    let cached = store::load(conn, &hub)?;
    let age_ok = |s: &Snapshot| {
        let age = (super::now_ns() - s.synced_ns) / 1_000_000_000;
        age >= 0 && age < ttl_secs(cfg)
    };
    match (fresh, cached) {
        (Fresh::Cached, Some(s)) => Ok(s),
        (Fresh::Cached, None) => Err(format!(
            "{hub} was never read: run `agent-mux project sync`"
        )),
        (Fresh::Auto, Some(s)) if age_ok(&s) => Ok(s),
        _ => {
            let snap = snapshot::load(tools, cfg)?;
            store::save(conn, &snap)?;
            Ok(snap)
        }
    }
}

struct Env {
    cfg: ProjectConfig,
    conn: rusqlite::Connection,
    tools: Tools,
    fresh: Fresh,
}

fn env(a: &Args) -> anyhow::Result<Env> {
    let dir = match a.value("--dir") {
        Some(d) => PathBuf::from(d),
        None => std::env::current_dir()?,
    };
    let dir = dir
        .canonicalize()
        .map_err(|e| anyhow::anyhow!("{}: {e}", dir.display()))?;
    let cfg = config::load(&dir).map_err(|e| anyhow::anyhow!(e))?;
    let db = db_path().map_err(|e| anyhow::anyhow!(e))?;
    let conn = open_store(&db).map_err(|e| anyhow::anyhow!(e))?;
    let fresh = if a.flag("--refresh") {
        Fresh::Refresh
    } else if a.flag("--cached") {
        Fresh::Cached
    } else {
        Fresh::Auto
    };
    Ok(Env {
        cfg,
        conn,
        tools: Tools::default(),
        fresh,
    })
}

fn snap(e: &Env) -> anyhow::Result<Snapshot> {
    obtain(&e.conn, &e.tools, &e.cfg, e.fresh).map_err(|e| anyhow::anyhow!(e))
}

fn number(a: &Args) -> anyhow::Result<u64> {
    let raw = a
        .positional
        .first()
        .ok_or_else(|| anyhow::anyhow!("which issue? give its number, as 12 or #12"))?;
    raw.trim_start_matches('#')
        .parse()
        .map_err(|_| anyhow::anyhow!("\"{raw}\" is not an issue number"))
}

pub fn run(args: &[String]) -> anyhow::Result<()> {
    let rest = args.get(1..).unwrap_or_default();
    let a = Args::parse(rest);
    match args.first().map(String::as_str) {
        Some("doctor") => cmd_doctor(&a),
        Some("sync") => cmd_sync(&a),
        Some("ls") | Some("list") => cmd_ls(&a),
        Some("show") => cmd_show(&a),
        Some("next") => cmd_next(&a),
        Some("order") => cmd_order(&a),
        None | Some("help") | Some("--help") | Some("-h") => {
            println!("{USAGE}");
            Ok(())
        }
        Some(other) => {
            eprintln!("unknown subcommand: project {other}\n\n{USAGE}");
            std::process::exit(2);
        }
    }
}

fn cmd_doctor(a: &Args) -> anyhow::Result<()> {
    let e = env(a)?;
    // the doctor always reads: a cache would hide what it is asked about
    let read = snapshot::load(&e.tools, &e.cfg);
    if let Ok(s) = &read {
        let _ = store::save(&e.conn, s);
    }
    let probe = doctor::probe(&e.tools, &e.cfg);
    let checks = doctor::checks(&e.cfg, &read, &probe);
    if a.flag("--json") {
        println!("{}", serde_json::to_string_pretty(&checks)?);
    } else {
        println!("Project {}", e.cfg.hub_repo().slug());
        for c in &checks {
            println!("  {} {:<20} {}", c.status.mark(), c.name, c.detail);
        }
    }
    if doctor::failed(&checks) {
        std::process::exit(1);
    }
    Ok(())
}

fn cmd_sync(a: &Args) -> anyhow::Result<()> {
    let mut e = env(a)?;
    e.fresh = Fresh::Refresh;
    let s = snap(&e)?;
    let open = s.items.iter().filter(|i| i.open).count();
    println!(
        "{}: {} issues ({open} open), {} milestones{}",
        s.hub,
        s.items.len(),
        s.milestones.len(),
        if s.board_error.is_some() {
            ", board not read"
        } else {
            ""
        }
    );
    for w in &s.warnings {
        println!("  ! {w}");
    }
    Ok(())
}

fn cmd_ls(a: &Args) -> anyhow::Result<()> {
    let e = env(a)?;
    let s = snap(&e)?;
    let want_type = match a.value("--type") {
        Some(t) => Some(
            ItemType::parse(t).ok_or_else(|| anyhow::anyhow!("\"{t}\" is not a work item type"))?,
        ),
        None => None,
    };
    let want_stage = match a.value("--stage") {
        Some(st) => Some(stage::resolve(st).map_err(|e| anyhow::anyhow!(e))?),
        None => None,
    };
    let milestone = a.value("--milestone");
    let rows: Vec<&Item> = s
        .items
        .iter()
        .filter(|i| a.flag("--all") || i.open)
        .filter(|i| want_type.is_none_or(|t| i.item_type == Some(t)))
        .filter(|i| want_stage.is_none_or(|st| stage::shown(i).stage == st))
        .filter(|i| milestone.is_none_or(|m| i.milestone.as_deref() == Some(m)))
        .collect();
    if a.flag("--json") {
        let list: Vec<Value> = rows.iter().map(|i| item_json(i, &s)).collect();
        println!("{}", serde_json::to_string_pretty(&list)?);
        return Ok(());
    }
    for i in &rows {
        println!("{}", row_line(i));
    }
    println!("{} item(s) of {}", rows.len(), s.hub);
    Ok(())
}

/// One list row: number, type, stage, priority, milestone, title.
pub fn row_line(i: &Item) -> String {
    let shown = stage::shown(i);
    format!(
        "#{:<5} {} {:<16}{} {:<3} {:<10} {}",
        i.number,
        i.item_type.map_or("?", ItemType::glyph),
        clip(stage_plain(&shown.stage), 16),
        if shown.derived { "~" } else { " " },
        i.priority.as_deref().unwrap_or("-"),
        clip(i.milestone.as_deref().unwrap_or("-"), 10),
        i.name(),
    )
}

fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let head: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{head}…")
    }
}

fn decision_json(d: &Decision) -> Value {
    json!({
        "action": d.action.as_str(),
        "reason": d.reason,
        "runnable": d.runnable(),
        "blocked": d.blocked.as_ref().map(|b| json!({
            "code": b.code, "message": b.message, "unblock": b.unblock })),
    })
}

fn item_json(i: &Item, s: &Snapshot) -> Value {
    let shown = stage::shown(i);
    let mut v = serde_json::to_value(i).unwrap_or(Value::Null);
    v["shown_stage"] = json!(shown.stage);
    v["stage_derived"] = json!(shown.derived);
    v["next"] = decision_json(&next::describe(i, s));
    v
}

fn cmd_next(a: &Args) -> anyhow::Result<()> {
    let e = env(a)?;
    let n = number(a)?;
    let s = snap(&e)?;
    let item = s.item(n).ok_or_else(|| {
        anyhow::anyhow!(
            "issue #{n} is not in the Project (closed long ago, or not in {})",
            s.hub
        )
    })?;
    let d = next::describe(item, &s);
    if a.flag("--json") {
        println!("{}", serde_json::to_string_pretty(&decision_json(&d))?);
        return Ok(());
    }
    println!("#{n} {}", item.title);
    for line in decision_lines(&d) {
        println!("  {line}");
    }
    Ok(())
}

/// A decision as lines: the step, why, what stops it and how to go on.
pub fn decision_lines(d: &Decision) -> Vec<String> {
    let mut out = Vec::new();
    match d.action {
        next::Action::None => out.push(format!("next: {}", d.reason)),
        action => {
            out.push(format!("next: {}", action.as_str()));
            out.push(format!("why:  {}", d.reason));
        }
    }
    if let Some(b) = &d.blocked {
        out.push(format!("blocked ({}): {}", b.code, b.message));
        out.push(format!("then: {}", b.unblock));
    }
    out
}

/// The card of an item, as lines.
pub fn card_lines(i: &Item, s: &Snapshot) -> Vec<String> {
    let mut out = Vec::new();
    let shown = stage::shown(i);
    let t = i.item_type;
    out.push(format!(
        "{} #{}  {}",
        t.map_or("(no type)", ItemType::as_str),
        i.number,
        i.name()
    ));
    out.push(format!(
        "stage     {}{}{}",
        shown.stage,
        if shown.derived {
            "  (derived: not on the board)"
        } else {
            ""
        },
        i.status
            .as_deref()
            .map(|st| format!("  · {st}"))
            .unwrap_or_default()
    ));
    if !i.open {
        out.push("state     closed".to_string());
    }
    if let Some(t) = t
        && !shown.derived
        && !stage::in_track(t, &shown.stage)
    {
        out.push(format!(
            "          ! {} is not a stage a {} goes through",
            stage_plain(&shown.stage),
            t.as_str()
        ));
    }
    if stage::kind(&shown.stage) != stage::Kind::Canonical {
        out.push(
            "          ! this column is not a stage of the flow; the item is never moved from it"
                .to_string(),
        );
    }
    out.push(format!(
        "priority  {}",
        i.priority.as_deref().unwrap_or("-")
    ));
    if let Some(m) = &i.milestone {
        out.push(format!("milestone {m}"));
    }
    for (k, v) in &i.fields {
        if k != super::contract::FIELD_TYPE && k != super::contract::FIELD_PRIORITY {
            out.push(format!("{:<9} {v}", k.to_lowercase()));
        }
    }
    if !i.assignees.is_empty() {
        out.push(format!("assignees {}", i.assignees.join(", ")));
    }
    if !i.repos.is_empty() {
        out.push(format!("repos     {}", i.repos.join(", ")));
    }
    // the chain of parents, nearest first
    let mut chain = Vec::new();
    let mut cur = i.parent;
    while let Some(p) = cur {
        if chain.len() >= 8 {
            break;
        }
        match s.item(p) {
            Some(parent) => {
                chain.push(format!(
                    "#{} {} {}",
                    parent.number,
                    parent.item_type.map_or("?", ItemType::as_str),
                    parent.name()
                ));
                cur = parent.parent;
            }
            None => {
                chain.push(format!("#{p}"));
                cur = None;
            }
        }
    }
    for (k, line) in chain.iter().enumerate() {
        out.push(format!(
            "{} {line}",
            if k == 0 { "parent   " } else { "         " }
        ));
    }
    for (k, d) in i.depends_on.iter().enumerate() {
        let state = match s.item(*d) {
            Some(dep) if !dep.open => "done".to_string(),
            Some(dep) => stage_plain(&stage::shown(dep).stage).to_string(),
            None => "closed or outside the Project".to_string(),
        };
        out.push(format!(
            "{} #{d}  {state}",
            if k == 0 { "depends  " } else { "         " }
        ));
    }
    let labels: Vec<&str> = i
        .labels
        .iter()
        .filter(|l| l.starts_with("spec-wave:"))
        .map(String::as_str)
        .collect();
    if !labels.is_empty() {
        out.push(format!("labels    {}", labels.join(", ")));
    }
    if !i.docs.is_empty() {
        out.push(String::new());
        out.push("documents".to_string());
        for (name, d) in &i.docs {
            let extra = match (d.state, d.pr, &d.branch) {
                (DocState::Pr, Some(pr), _) => format!("  pull request #{pr}"),
                (DocState::Branch, _, Some(b)) => format!("  branch {b}"),
                _ => String::new(),
            };
            out.push(format!(
                "  {:<14} {:<8} {}{extra}",
                name,
                d.state.as_str(),
                d.path
            ));
        }
    }
    if !i.pulls.is_empty() {
        out.push(String::new());
        out.push("pull requests".to_string());
        for p in &i.pulls {
            out.push(format!(
                "  {}#{}  {}{}{}  {} → {}",
                p.repo,
                p.number,
                p.state.to_lowercase(),
                if p.draft { " (draft)" } else { "" },
                if p.review.is_empty() {
                    String::new()
                } else {
                    format!(" · {}", p.review.to_lowercase().replace('_', " "))
                },
                p.head,
                p.base
            ));
        }
    }
    let kids = s.children_of(i.number);
    if !kids.is_empty() {
        out.push(String::new());
        out.push(format!("children ({})", kids.len()));
        for k in kids {
            out.push(format!("  {}", row_line(k)));
        }
    }
    out.push(String::new());
    out.extend(decision_lines(&next::describe(i, s)));
    out
}

fn cmd_show(a: &Args) -> anyhow::Result<()> {
    let e = env(a)?;
    let n = number(a)?;
    let s = snap(&e)?;
    let item = s.item(n).ok_or_else(|| {
        anyhow::anyhow!(
            "issue #{n} is not in the Project (closed long ago, or not in {})",
            s.hub
        )
    })?;
    if a.flag("--json") {
        println!("{}", serde_json::to_string_pretty(&item_json(item, &s))?);
        return Ok(());
    }
    for line in card_lines(item, &s) {
        println!("{line}");
    }
    Ok(())
}

/// A Feature's order, as lines.
pub fn order_lines(o: &order::FeatureOrder) -> Vec<String> {
    let mut out = vec![format!(
        "Feature #{} {}{}",
        o.feature,
        o.title,
        o.milestone
            .as_deref()
            .map(|m| format!("  [{m}]"))
            .unwrap_or_default()
    )];
    for (k, r) in o.rows.iter().enumerate() {
        let deps = if r.depends_on.is_empty() {
            String::new()
        } else {
            format!(
                "  ← {}",
                r.depends_on
                    .iter()
                    .map(|d| format!("#{d}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        out.push(format!(
            "  {:>2}. #{:<5} {:<16} {}/{} tasks  {}{deps}",
            k + 1,
            r.number,
            clip(stage_plain(&r.stage), 16),
            r.tasks_done,
            r.tasks,
            r.title
        ));
    }
    if o.rows.is_empty() {
        out.push("  (no Stories)".to_string());
    }
    for x in &o.external {
        out.push(format!(
            "  blocked from outside: #{} waits on #{} ({})",
            x.story,
            x.on,
            match (&x.stage, x.open) {
                (_, Some(false)) => "done".to_string(),
                (Some(st), _) => stage_plain(st).to_string(),
                _ => "closed or outside the Project".to_string(),
            }
        ));
    }
    for w in &o.warnings {
        out.push(format!("  ! {w}"));
    }
    out
}

fn cmd_order(a: &Args) -> anyhow::Result<()> {
    let e = env(a)?;
    let s = snap(&e)?;
    let orders = match a.positional.first() {
        Some(_) => vec![order::feature_order(&s, number(a)?).map_err(|e| anyhow::anyhow!(e))?],
        None => order::map(&s, a.value("--milestone")),
    };
    if a.flag("--json") {
        println!("{}", serde_json::to_string_pretty(&orders)?);
        return Ok(());
    }
    if orders.is_empty() {
        println!("no open Feature has Stories");
    }
    for (k, o) in orders.iter().enumerate() {
        if k > 0 {
            println!();
        }
        for line in order_lines(o) {
            println!("{line}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::snapshot::tests as fixture;

    #[test]
    fn flags_values_and_numbers_are_parsed() {
        let raw: Vec<String> = ["#12", "--type", "story", "--stage=ready", "--json"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let a = Args::parse(&raw);
        assert_eq!(number(&a).unwrap(), 12);
        assert_eq!(a.value("--type"), Some("story"));
        assert_eq!(a.value("--stage"), Some("ready"));
        assert!(a.flag("--json"));
        assert!(number(&Args::parse(&["x".to_string()])).is_err());
    }

    #[test]
    fn a_card_says_stage_documents_and_the_next_step() {
        let s = snapshot::build(&fixture::raw(), &fixture::cfg(), 0);
        let card = card_lines(s.item(1).unwrap(), &s).join("\n");
        assert!(card.contains("Feature #1  Cadastro de Pedidos"));
        assert!(card.contains("stage     ✅ Ready  · Todo"));
        assert!(card.contains("spec           base     docs/features/cadastro-de-pedidos/spec.md"));
        assert!(card.contains("decomposition  branch"));
        assert!(card.contains("children (2)"));
        assert!(card.contains("blocked (pr-pending)"));
        let bug = card_lines(s.item(4).unwrap(), &s).join("\n");
        assert!(bug.contains("(derived: not on the board)"));
        assert!(bug.contains("pull request #40"));
    }

    #[test]
    fn the_cache_is_used_while_it_is_young() {
        let dir = tempfile::tempdir().unwrap();
        let conn = open_store(&dir.path().join("t.db")).unwrap();
        let cfg = fixture::cfg();
        // a `gh` that cannot run: any read of GitHub fails
        let tools = Tools {
            gh: "/nonexistent/gh".into(),
        };
        assert!(
            obtain(&conn, &tools, &cfg, Fresh::Cached)
                .unwrap_err()
                .contains("never read")
        );
        let mut s = snapshot::build(&fixture::raw(), &cfg, crate::project::now_ns());
        store::save(&conn, &s).unwrap();
        assert_eq!(obtain(&conn, &tools, &cfg, Fresh::Auto).unwrap(), s);
        assert!(obtain(&conn, &tools, &cfg, Fresh::Refresh).is_err());
        // an old cache is not served by Auto, and still is by Cached
        s.synced_ns -= 3600 * 1_000_000_000;
        store::save(&conn, &s).unwrap();
        assert!(obtain(&conn, &tools, &cfg, Fresh::Auto).is_err());
        assert_eq!(obtain(&conn, &tools, &cfg, Fresh::Cached).unwrap(), s);
    }
}
