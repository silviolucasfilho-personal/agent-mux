//! The state file a loop skill rewrites every run, parsed into the report
//! the Loops view, the sidebar card and the CLI show.
//!
//! The shape is the contract every loop skill is already told to keep
//! (`loops/templates/STATE.md`, the `## State file` section of each
//! `loops/skills/*/SKILL.md`): a `Last run:` line, three sections, one
//! bullet per item, indented `Loop action:` and `Human decision:` lines
//! under it, and a `Run log:` / `Fingerprint:` footer. Parsing is lenient
//! by house rule: a line that does not fit its shape is kept verbatim in
//! `Item::raw`, so a skill that drifts degrades to plain text instead of
//! losing the item.

use serde::{Deserialize, Serialize};

/// What a section means for the reader. Derived from its heading, so a
/// renamed heading still lands in the right bucket.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SectionKind {
    /// `## High Priority (loop is acting or waiting on human)`
    NeedsYou,
    /// `## Watch List`
    Watching,
    /// `## Recent Noise (ignored this run)`
    Ignored,
    Other,
}

impl SectionKind {
    fn of(heading: &str) -> SectionKind {
        let h = heading.to_ascii_lowercase();
        if h.contains("high priority") || h.contains("needs you") || h.contains("blocked") {
            SectionKind::NeedsYou
        } else if h.contains("watch") {
            SectionKind::Watching
        } else if h.contains("noise") || h.contains("ignored") {
            SectionKind::Ignored
        } else {
            SectionKind::Other
        }
    }

    /// The word the view puts above the group.
    pub fn label(self) -> &'static str {
        match self {
            SectionKind::NeedsYou => "Needs you",
            SectionKind::Watching => "Watching",
            SectionKind::Ignored => "Ignored",
            SectionKind::Other => "Other",
        }
    }
}

/// One bullet of a section.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Item {
    /// `#2238`, `PR-14`, `deps/serde` — whatever the skill leads with.
    pub id: Option<String>,
    /// The text before the ` — `.
    pub title: String,
    /// The text after the ` — `: the status fragment.
    pub status: Option<String>,
    pub loop_action: Option<String>,
    pub human_decision: Option<String>,
    /// Continuation lines that are neither of the two known keys.
    pub notes: Vec<String>,
    /// `- [x]` when the skill used a checkbox.
    pub done: bool,
    /// The bullet verbatim, for a line that did not parse.
    pub raw: String,
}

impl Item {
    /// `- (none)`, `- (no open pull requests)`: the skill's way of saying
    /// the bucket is empty. Counted as no item.
    pub fn is_placeholder(&self) -> bool {
        let t = self
            .title
            .trim_start_matches(['(', '['].as_ref())
            .to_ascii_lowercase();
        self.id.is_none() && (t.starts_with("none") || t.starts_with("no ") || t == "n/a")
    }

    /// `#2238 chore(spec-wave): …`, for one line of a list.
    pub fn headline(&self) -> String {
        match &self.id {
            Some(id) => format!("{id} {}", self.title),
            None => self.title.clone(),
        }
    }

    /// What this item is keyed by across runs.
    pub fn key(&self) -> String {
        self.id.clone().unwrap_or_else(|| self.title.clone())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Section {
    pub name: String,
    pub kind: SectionKind,
    pub items: Vec<Item>,
    /// Lines of the section that are not bullets (a skill's trailing note).
    pub notes: Vec<String>,
}

impl Section {
    /// The items that are not empty-bucket placeholders.
    pub fn real(&self) -> impl Iterator<Item = &Item> {
        self.items.iter().filter(|i| !i.is_placeholder())
    }

    pub fn count(&self) -> usize {
        self.real().count()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct StateReport {
    pub title: Option<String>,
    pub last_run: Option<String>,
    pub sections: Vec<Section>,
    /// The footer's `Run log:` line.
    pub run_log: Option<String>,
    /// The footer's `Fingerprint:` line; an unchanged one is a quiet run.
    pub fingerprint: Option<String>,
}

impl StateReport {
    pub fn of_kind(&self, kind: SectionKind) -> Option<&Section> {
        self.sections.iter().find(|s| s.kind == kind)
    }

    pub fn items_of(&self, kind: SectionKind) -> Vec<&Item> {
        self.of_kind(kind)
            .map(|s| s.real().collect())
            .unwrap_or_default()
    }

    pub fn needs_you(&self) -> Vec<&Item> {
        self.items_of(SectionKind::NeedsYou)
    }

    pub fn watching(&self) -> Vec<&Item> {
        self.items_of(SectionKind::Watching)
    }

    pub fn ignored(&self) -> Vec<&Item> {
        self.items_of(SectionKind::Ignored)
    }

    pub fn total(&self) -> usize {
        self.sections.iter().map(|s| s.count()).sum()
    }

    /// Every real item, whichever section it sits in.
    pub fn items(&self) -> impl Iterator<Item = (&Section, &Item)> {
        self.sections
            .iter()
            .flat_map(|s| s.real().map(move |i| (s, i)))
    }

    pub fn find(&self, key: &str) -> Option<(&Section, &Item)> {
        self.items().find(|(_, i)| i.key() == key)
    }

    pub fn is_empty(&self) -> bool {
        self.sections.is_empty() && self.last_run.is_none()
    }
}

/// Reads a state file. Never fails: an unparsable file yields an empty
/// report, which the views render as "no report from this run".
pub fn parse(text: &str) -> StateReport {
    let mut out = StateReport::default();
    let mut section: Option<Section> = None;
    let mut in_footer = false;
    for line in text.lines() {
        let trimmed = line.trim_end();
        let indented = line.starts_with(' ') || line.starts_with('\t');
        let body = trimmed.trim();
        if body.is_empty() {
            continue;
        }
        if body == "---" || body == "***" {
            in_footer = true;
            continue;
        }
        if let Some(rest) = body.strip_prefix("## ") {
            if let Some(s) = section.take() {
                out.sections.push(s);
            }
            section = Some(Section {
                name: rest.trim().to_string(),
                kind: SectionKind::of(rest),
                items: Vec::new(),
                notes: Vec::new(),
            });
            continue;
        }
        if let Some(rest) = body.strip_prefix("# ") {
            if out.title.is_none() {
                out.title = Some(rest.trim().to_string());
            }
            continue;
        }
        if let Some(rest) = strip_key(body, "Last run:") {
            out.last_run = Some(rest.to_string());
            continue;
        }
        if let Some(rest) = strip_key(body, "Run log:") {
            out.run_log = Some(rest.to_string());
            continue;
        }
        if let Some(rest) = strip_key(body, "Fingerprint:") {
            out.fingerprint = Some(rest.to_string());
            continue;
        }
        if in_footer {
            continue;
        }
        let Some(sec) = section.as_mut() else {
            continue;
        };
        if !indented && let Some(bullet) = strip_bullet(body) {
            sec.items.push(parse_item(bullet, body));
            continue;
        }
        // A continuation line belongs to the bullet above it.
        if let Some(item) = sec.items.last_mut() {
            if let Some(rest) = strip_key(body, "Loop action:") {
                item.loop_action = Some(rest.to_string());
            } else if let Some(rest) = strip_key(body, "Human decision:") {
                item.human_decision = Some(rest.to_string());
            } else if indented {
                item.notes.push(body.to_string());
            } else {
                sec.notes.push(body.to_string());
            }
        } else {
            sec.notes.push(body.to_string());
        }
    }
    if let Some(s) = section.take() {
        out.sections.push(s);
    }
    out
}

/// `Loop action: x` → `x`, case-insensitively; `**Loop action:** x` too.
/// Byte-boundary safe: the line may hold any UTF-8, so the key is compared
/// through `get`, never through a split at a byte offset.
fn strip_key<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let clean = line.trim_start_matches(['*', '_', '-', ' ']);
    if let Some(head) = clean.get(..key.len())
        && head.eq_ignore_ascii_case(key)
    {
        return Some(clean[key.len()..].trim());
    }
    // `**Loop action:**` puts the emphasis markers inside the key
    if !clean.starts_with('*') {
        return None;
    }
    let bare = clean.replace('*', "");
    if bare.get(..key.len())?.eq_ignore_ascii_case(key) {
        let idx = clean.find(':')? + 1;
        return Some(clean[idx..].trim_start_matches(['*', ' ']).trim());
    }
    None
}

fn strip_bullet(line: &str) -> Option<&str> {
    let rest = line
        .strip_prefix("- ")
        .or_else(|| line.strip_prefix("* "))
        .or_else(|| line.strip_prefix("+ "))?;
    Some(rest.trim_start())
}

fn parse_item(bullet: &str, raw: &str) -> Item {
    let mut rest = bullet;
    let mut done = false;
    if let Some(r) = rest
        .strip_prefix("[x] ")
        .or_else(|| rest.strip_prefix("[X] "))
    {
        done = true;
        rest = r.trim_start();
    } else if let Some(r) = rest.strip_prefix("[ ] ") {
        rest = r.trim_start();
    }
    let (id, rest) = split_id(rest);
    // The em dash is the documented separator; the double hyphen is what a
    // keyboard produces, so both are accepted.
    let (title, status) = match rest.find(" \u{2014} ").or_else(|| rest.find(" -- ")) {
        Some(i) => {
            let sep = if rest[i..].starts_with(" \u{2014} ") {
                " \u{2014} ".len()
            } else {
                " -- ".len()
            };
            (
                rest[..i].trim().to_string(),
                Some(rest[i + sep..].trim().to_string()),
            )
        }
        None => (rest.trim().to_string(), None),
    };
    Item {
        id,
        title,
        status,
        loop_action: None,
        human_decision: None,
        notes: Vec::new(),
        done,
        raw: raw.to_string(),
    }
}

/// `#2238 title` → (`#2238`, `title`). Also `PR-14`, `serde@1.0` and a
/// bare `[#12]`; anything else has no id.
fn split_id(text: &str) -> (Option<String>, &str) {
    let first = text.split_whitespace().next().unwrap_or("");
    let bare = first.trim_matches(['[', ']', '(', ')', ':', ',']);
    let is_id = bare.starts_with('#')
        || (bare.len() > 2
            && bare
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_uppercase() || c == '@')
            && bare.chars().any(|c| c.is_ascii_digit())
            && bare.chars().all(|c| {
                c.is_ascii_alphanumeric()
                    || c == '-'
                    || c == '_'
                    || c == '/'
                    || c == '@'
                    || c == '.'
            }));
    if !is_id {
        return (None, text);
    }
    let rest = text[first.len()..].trim_start();
    (Some(bare.to_string()), rest)
}

/// What changed between two runs' reports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Delta {
    pub new: Vec<String>,
    pub gone: Vec<String>,
    /// `(key, from section, to section)`
    pub moved: Vec<(String, String, String)>,
    /// `(key, before, after)` of the status fragment
    pub changed: Vec<(String, String, String)>,
}

impl Delta {
    pub fn is_empty(&self) -> bool {
        self.new.is_empty()
            && self.gone.is_empty()
            && self.moved.is_empty()
            && self.changed.is_empty()
    }

    /// One line for a run card, the notice and the sidebar.
    pub fn summary(&self) -> String {
        if self.is_empty() {
            return "nothing moved".into();
        }
        let mut parts: Vec<String> = Vec::new();
        for (k, before, after) in self.changed.iter().take(2) {
            parts.push(format!("{k} {} → {}", short(before), short(after)));
        }
        for (k, _, to) in self.moved.iter().take(2) {
            parts.push(format!("{k} moved to {to}"));
        }
        if !self.new.is_empty() {
            parts.push(format!("{} new", self.new.len()));
        }
        if !self.gone.is_empty() {
            parts.push(format!("{} gone", self.gone.len()));
        }
        parts.join(" · ")
    }
}

fn short(s: &str) -> String {
    let first: String = s
        .split(['·', ';', ','])
        .next()
        .unwrap_or(s)
        .trim()
        .chars()
        .take(28)
        .collect();
    first
}

/// The changes from `prev` to `cur`, keyed by item id (or title).
pub fn delta(prev: &StateReport, cur: &StateReport) -> Delta {
    let mut out = Delta::default();
    for (sec, item) in cur.items() {
        let key = item.key();
        match prev.find(&key) {
            None => out.new.push(key),
            Some((psec, pitem)) => {
                if psec.kind != sec.kind {
                    out.moved.push((
                        key.clone(),
                        psec.kind.label().into(),
                        sec.kind.label().into(),
                    ));
                }
                let before = pitem.status.clone().unwrap_or_default();
                let after = item.status.clone().unwrap_or_default();
                if before != after && !after.is_empty() {
                    out.changed.push((key, before, after));
                }
            }
        }
    }
    for (_, item) in prev.items() {
        let key = item.key();
        if cur.find(&key).is_none() {
            out.gone.push(key);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Snapshots
//
// The state file is rewritten in place every run, so yesterday's report is
// gone the moment today's run ends. agent-mux keeps a copy per run under
// the runtime directory: it is what the Report tab of an older run shows,
// what the "since last run" line is computed against, and what a per-item
// timeline is read from. Kept for 30 days, like the run log.

use std::path::{Path, PathBuf};

pub const KEEP_DAYS: i64 = 30;

pub fn snapshots_dir(runtime_dir: &Path, loop_id: &str) -> PathBuf {
    runtime_dir.join("loops").join("state").join(loop_id)
}

pub fn snapshot_path(runtime_dir: &Path, loop_id: &str, run_id: &str) -> PathBuf {
    // Run ids are RFC 3339 timestamps: replacing the colons keeps the name
    // portable and still sorts oldest-first.
    snapshots_dir(runtime_dir, loop_id).join(format!("{}.md", run_id.replace(':', "-")))
}

/// Writes this run's copy of the state file and prunes old ones.
pub fn write_snapshot(
    runtime_dir: &Path,
    loop_id: &str,
    run_id: &str,
    text: &str,
) -> std::io::Result<PathBuf> {
    let dir = snapshots_dir(runtime_dir, loop_id);
    std::fs::create_dir_all(&dir)?;
    let path = snapshot_path(runtime_dir, loop_id, run_id);
    let temp = path.with_extension("tmp");
    std::fs::write(&temp, text)?;
    std::fs::rename(&temp, &path)?;
    prune(runtime_dir, loop_id, KEEP_DAYS);
    Ok(path)
}

pub fn read_snapshot(runtime_dir: &Path, loop_id: &str, run_id: &str) -> Option<String> {
    std::fs::read_to_string(snapshot_path(runtime_dir, loop_id, run_id)).ok()
}

/// Every snapshot of a loop, oldest first, as `(run id, path)`.
pub fn snapshots(runtime_dir: &Path, loop_id: &str) -> Vec<(String, PathBuf)> {
    let mut out: Vec<(String, PathBuf)> = std::fs::read_dir(snapshots_dir(runtime_dir, loop_id))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            let stem = path.file_stem()?.to_str()?;
            if path.extension().and_then(|x| x.to_str()) != Some("md") {
                return None;
            }
            Some((restore_run_id(stem), path))
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// The snapshot of the run before `run_id`, which is what "since last run"
/// compares against.
pub fn previous_snapshot(runtime_dir: &Path, loop_id: &str, run_id: &str) -> Option<String> {
    let all = snapshots(runtime_dir, loop_id);
    let prev = all.iter().rev().find(|(id, _)| id.as_str() < run_id)?;
    std::fs::read_to_string(&prev.1).ok()
}

/// `2026-09-17T17-36-53Z` → `2026-09-17T17:36:53Z`.
fn restore_run_id(stem: &str) -> String {
    match stem.split_once('T') {
        Some((day, rest)) => format!("{day}T{}", rest.replace('-', ":")),
        None => stem.to_string(),
    }
}

fn prune(runtime_dir: &Path, loop_id: &str, days: i64) {
    let cutoff = crate::loops::format_timestamp(crate::loops::now() - time::Duration::days(days));
    for (id, path) in snapshots(runtime_dir, loop_id) {
        if id < cutoff {
            let _ = std::fs::remove_file(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PR: &str = r#"# PR Babysitter — findon

Last run: 2026-09-17T17:36:53Z

## High Priority (loop is acting or waiting on human)

- [ ] #2238 chore(spec-wave): atualiza arquivos para v0.34.2 — conflicts (CONFLICTING, DIRTY); touches `.github/workflows/**`
  Loop action: reported only, unchanged since 2026-09-13.
  Human decision: rebase spec-wave/update-v0.34.2 on develop, then merge.
- [ ] #1919 chore: back-merge main para develop — BLOCKED: required `verify` check never reported
  Loop action: reported only. Never pushes or merges.
  Human decision: merge with ruleset bypass, or close.

## Watch List

- #2237 chore(deps-dev): bump vitest from 4.1.10 to 4.1.11 — CLEAN, verify green, no review yet

Note: the six CLEAN docs PRs match the allowlist but this skill never merges.

## Recent Noise (ignored this run)

- (no drafts open)

---
Run log: 2026-09-17T17:36:53Z | 8 findings | 0 actions | 2 escalations
Fingerprint: 1919|2026-09-16T19:47:13Z|BLOCKED||3 2238|2026-09-13T19:18:37Z|DIRTY||4
"#;

    #[test]
    fn a_pr_babysitter_state_file_parses_into_its_three_sections() {
        let r = parse(PR);
        assert_eq!(r.title.as_deref(), Some("PR Babysitter — findon"));
        assert_eq!(r.last_run.as_deref(), Some("2026-09-17T17:36:53Z"));
        assert_eq!(r.sections.len(), 3);
        let needs = r.needs_you();
        assert_eq!(needs.len(), 2);
        assert_eq!(needs[0].id.as_deref(), Some("#2238"));
        assert!(needs[0].title.starts_with("chore(spec-wave)"));
        assert!(needs[0].status.as_deref().unwrap().starts_with("conflicts"));
        assert!(
            needs[0]
                .human_decision
                .as_deref()
                .unwrap()
                .starts_with("rebase")
        );
        assert!(
            needs[0]
                .loop_action
                .as_deref()
                .unwrap()
                .starts_with("reported only")
        );
        assert_eq!(r.watching().len(), 1);
        // the placeholder bullet is not an item, and the section note is kept
        assert_eq!(r.ignored().len(), 0);
        assert_eq!(
            r.of_kind(SectionKind::Watching).unwrap().notes.len(),
            1,
            "the trailing note stays with its section"
        );
        assert!(r.run_log.as_deref().unwrap().contains("8 findings"));
        assert!(r.fingerprint.as_deref().unwrap().starts_with("1919|"));
        assert_eq!(r.total(), 3);
    }

    #[test]
    fn a_bullet_that_does_not_fit_the_shape_keeps_its_line() {
        let r = parse("## Watch List\n\n- just a sentence with no id and no dash\n");
        let items = r.watching();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, None);
        assert_eq!(items[0].title, "just a sentence with no id and no dash");
        assert_eq!(items[0].status, None);
        assert!(items[0].raw.starts_with("- just a sentence"));
    }

    #[test]
    fn an_unparsable_file_is_an_empty_report() {
        let r = parse("nothing here at all");
        assert!(r.is_empty());
        assert_eq!(r.total(), 0);
    }

    #[test]
    fn the_delta_names_what_moved_since_the_previous_run() {
        let prev = parse(PR);
        let cur = parse(&PR.replace(
            "CLEAN, verify green, no review yet",
            "CONFLICTING after a push",
        ));
        let d = delta(&prev, &cur);
        assert!(d.new.is_empty() && d.gone.is_empty());
        assert_eq!(d.changed.len(), 1);
        assert_eq!(d.changed[0].0, "#2237");
        assert!(d.summary().contains("#2237"), "{}", d.summary());

        // an item that leaves the file is gone; one that arrives is new
        let shorter = parse(&PR.replace(
            "- [ ] #1919 chore: back-merge main para develop — BLOCKED: required `verify` check never reported\n  Loop action: reported only. Never pushes or merges.\n  Human decision: merge with ruleset bypass, or close.\n",
            "",
        ));
        let d = delta(&prev, &shorter);
        assert_eq!(d.gone, vec!["#1919".to_string()]);
        assert!(d.summary().contains("1 gone"), "{}", d.summary());
    }

    #[test]
    fn an_item_that_changes_section_is_reported_as_moved() {
        let prev = parse("## Watch List\n\n- #7 a thing — CLEAN\n");
        let cur = parse(
            "## High Priority (loop is acting or waiting on human)\n\n- #7 a thing — CLEAN\n",
        );
        let d = delta(&prev, &cur);
        assert_eq!(d.moved.len(), 1);
        assert_eq!(
            d.moved[0],
            ("#7".into(), "Watching".into(), "Needs you".into())
        );
    }

    #[test]
    fn snapshots_are_kept_per_run_and_the_previous_one_is_found() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        write_snapshot(root, "abc", "2026-09-17T17:21:00Z", "# one\n").unwrap();
        write_snapshot(root, "abc", "2026-09-17T17:36:53Z", "# two\n").unwrap();
        assert_eq!(
            read_snapshot(root, "abc", "2026-09-17T17:36:53Z").as_deref(),
            Some("# two\n")
        );
        let ids: Vec<String> = snapshots(root, "abc").into_iter().map(|(i, _)| i).collect();
        assert_eq!(
            ids,
            vec![
                "2026-09-17T17:21:00Z".to_string(),
                "2026-09-17T17:36:53Z".to_string()
            ]
        );
        assert_eq!(
            previous_snapshot(root, "abc", "2026-09-17T17:36:53Z").as_deref(),
            Some("# one\n")
        );
        assert_eq!(previous_snapshot(root, "abc", "2026-09-17T17:21:00Z"), None);
        // a loop with no snapshots is not an error
        assert!(snapshots(root, "nope").is_empty());
    }
}
