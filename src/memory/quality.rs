//! Keeping the memory useful as it grows: search past the index, and
//! duplicates found in Rust and merged by one model run on request.
//!
//! - `search`: every note of the workspace, the user's, and the imported
//!   mirrors, ranked by where the words match (name, description, body).
//!   The index lists at most `index_lines` notes; search finds the rest.
//! - `duplicates`: pairs of notes in one folder whose words overlap
//!   (Jaccard over their name and description, or over the whole note),
//!   grouped. `agent-mux memory check` reports them; nothing is spent.
//! - `tidy`: one headless run (`[memory] tidy` prompt) merges each group
//!   into one note; agent-mux writes it and moves the originals to
//!   `notes/.merged/`, never deleting them.

use super::{index, log, note, paths};
use crate::harness::Harness;
use crate::prompts::Prompts;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Where merged-away notes go.
pub const MERGED: &str = ".merged";

/// Words too common to tell two notes apart.
const STOP: &[&str] = &[
    "the", "and", "for", "are", "but", "not", "you", "all", "any", "can", "had", "her", "was",
    "one", "our", "out", "has", "have", "his", "how", "its", "may", "new", "now", "old", "see",
    "two", "way", "who", "did", "get", "let", "use", "that", "this", "with", "from", "they",
    "will", "would", "there", "their", "what", "when", "which", "where", "while", "into", "than",
    "then", "them", "these", "those", "been", "being", "were", "should", "could", "must", "only",
    "also", "each", "some", "such", "very", "more", "most", "other", "about", "after", "before",
    "over", "under", "again", "why", "apply", "note", "notes", "session", "sessions",
];

fn words(text: &str) -> BTreeSet<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .map(|w| w.to_lowercase())
        .filter(|w| w.chars().count() >= 3 && !STOP.contains(&w.as_str()))
        .collect()
}

fn jaccard(a: &BTreeSet<String>, b: &BTreeSet<String>) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let inter = a.intersection(b).count() as f64;
    let union = a.union(b).count() as f64;
    inter / union
}

// ------------------------------------------------------------ search

/// One search result.
#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    /// `workspace`, `user` or `imported`.
    pub place: &'static str,
    pub path: PathBuf,
    /// The index line (a note) or the mirror's line that matched.
    pub line: String,
    /// The first body line that matched, when the body did.
    pub snippet: Option<String>,
    pub score: u32,
}

fn score(terms: &[String], name: &str, description: &str, body: &str) -> (u32, Option<String>) {
    let (name, description) = (name.to_lowercase(), description.to_lowercase());
    let mut total = 0;
    let mut snippet = None;
    for t in terms {
        let mut s = 0;
        if name.contains(t.as_str()) {
            s += 6;
        }
        if description.contains(t.as_str()) {
            s += 3;
        }
        if let Some(line) = body.lines().find(|l| l.to_lowercase().contains(t.as_str())) {
            s += 1;
            if snippet.is_none() {
                snippet = Some(line.trim().chars().take(160).collect());
            }
        }
        // every word must match somewhere
        if s == 0 {
            return (0, None);
        }
        total += s;
    }
    (total, snippet)
}

/// The notes of `dir` (and `user`, and `dir`'s imported mirrors) that
/// match every word of `query`, best first.
pub fn search(dir: &Path, user: Option<&Path>, query: &str) -> Vec<Hit> {
    let terms: Vec<String> = query.split_whitespace().map(|w| w.to_lowercase()).collect();
    if terms.is_empty() {
        return Vec::new();
    }
    let mut hits = Vec::new();
    for (place, d) in [("workspace", Some(dir)), ("user", user)] {
        let Some(d) = d else { continue };
        let (notes, _) = note::load_all(d);
        for n in notes {
            let (s, snippet) = score(&terms, &n.name, &n.description, &n.body);
            if s > 0 {
                hits.push(Hit {
                    place,
                    path: paths::note_path(d, &n.name),
                    line: index::line(&n),
                    snippet,
                    score: s,
                });
            }
        }
    }
    // the mirrors: each line of them is one entry
    if let Ok(entries) = std::fs::read_dir(dir.join(paths::IMPORTS)) {
        for p in entries.flatten().map(|e| e.path()) {
            let Ok(text) = std::fs::read_to_string(&p) else {
                continue;
            };
            for line in text.lines().filter(|l| l.starts_with("- ")) {
                let (s, _) = score(&terms, "", line, "");
                if s > 0 {
                    hits.push(Hit {
                        place: "imported",
                        path: p.clone(),
                        line: line.to_string(),
                        snippet: None,
                        score: s,
                    });
                }
            }
        }
    }
    hits.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.line.cmp(&b.line)));
    hits
}

// ------------------------------------------------------------ duplicates

/// Overlap of name and description that makes two notes look alike.
const SAME_GIST: f64 = 0.5;
/// Overlap of the whole note that does.
const SAME_TEXT: f64 = 0.6;

/// The groups of notes in `dir` that look like one fact saved more than
/// once, each sorted by name; groups sorted too.
pub fn duplicates(dir: &Path) -> Vec<Vec<String>> {
    let (notes, _) = note::load_all(dir);
    let gist: Vec<BTreeSet<String>> = notes
        .iter()
        .map(|n| {
            words(&format!(
                "{} {}",
                n.name.replace(['-', '_'], " "),
                n.description
            ))
        })
        .collect();
    let all: Vec<BTreeSet<String>> = notes
        .iter()
        .map(|n| {
            words(&format!(
                "{} {} {}",
                n.name.replace(['-', '_'], " "),
                n.description,
                n.body
            ))
        })
        .collect();
    // union-find over the alike pairs
    let mut parent: Vec<usize> = (0..notes.len()).collect();
    fn root(p: &mut [usize], i: usize) -> usize {
        let mut i = i;
        while p[i] != i {
            p[i] = p[p[i]];
            i = p[i];
        }
        i
    }
    for i in 0..notes.len() {
        for j in i + 1..notes.len() {
            if jaccard(&gist[i], &gist[j]) >= SAME_GIST || jaccard(&all[i], &all[j]) >= SAME_TEXT {
                let (a, b) = (root(&mut parent, i), root(&mut parent, j));
                parent[a] = b;
            }
        }
    }
    let mut groups: std::collections::BTreeMap<usize, Vec<String>> = Default::default();
    for (i, n) in notes.iter().enumerate() {
        let r = root(&mut parent, i);
        groups.entry(r).or_default().push(n.name.clone());
    }
    let mut out: Vec<Vec<String>> = groups
        .into_values()
        .filter(|g| g.len() > 1)
        .map(|mut g| {
            g.sort();
            g
        })
        .collect();
    out.sort();
    out
}

// ------------------------------------------------------------ tidy

/// The harnesses `tidy` can run, in the order it tries them.
const TIDY_HARNESSES: [Harness; 3] = [Harness::Claude, Harness::Codex, Harness::Antigravity];

fn on_path(program: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(program).is_file()))
}

/// The harness `tidy` runs: the one asked for, else the first installed.
pub fn tidy_harness(asked: Option<Harness>) -> Option<Harness> {
    asked.or_else(|| TIDY_HARNESSES.into_iter().find(|h| on_path(h.as_str())))
}

/// The groups as the `[memory] tidy` prompt shows them: each note's file.
pub fn groups_text(dir: &Path, groups: &[Vec<String>]) -> String {
    let mut out = String::new();
    for (i, g) in groups.iter().enumerate() {
        out.push_str(&format!("### Group {}\n\n", i + 1));
        for name in g {
            let text = std::fs::read_to_string(paths::note_path(dir, name)).unwrap_or_default();
            out.push_str(&format!("```note {name}\n{}\n```\n\n", text.trim()));
        }
    }
    out
}

/// A fresh headless run of `harness` with `prompt`, no tools, nothing
/// saved; its answer.
fn ask(harness: Harness, prompt: &str, cwd: &Path) -> Result<String, String> {
    let tmp = tempfile::tempdir().map_err(|e| e.to_string())?;
    let last = tmp.path().join("last.txt");
    let (program, args): (&str, Vec<String>) = match harness {
        Harness::Claude => (
            "claude",
            vec![
                "-p".into(),
                prompt.into(),
                "--tools".into(),
                "".into(),
                "--no-session-persistence".into(),
            ],
        ),
        Harness::Codex => (
            "codex",
            vec![
                "exec".into(),
                "--skip-git-repo-check".into(),
                "--ephemeral".into(),
                "-c".into(),
                "sandbox_mode=\"read-only\"".into(),
                "-o".into(),
                last.to_string_lossy().into_owned(),
                prompt.into(),
            ],
        ),
        Harness::Antigravity => (
            "agy",
            vec!["-p".into(), prompt.into(), "--mode".into(), "plan".into()],
        ),
    };
    let out_file = tmp.path().join("out.txt");
    let out = std::fs::File::create(&out_file).map_err(|e| e.to_string())?;
    let mut child = std::process::Command::new(program)
        .args(&args)
        .current_dir(cwd)
        .stdin(std::process::Stdio::null())
        .stdout(out)
        .stderr(std::process::Stdio::null())
        .env_remove(super::ENV_DIR)
        .env_remove(super::ENV_INDEX)
        .env_remove(super::ENV_USER)
        .spawn()
        .map_err(|e| format!("{program}: {e}"))?;
    let deadline = Instant::now() + super::save::TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                return Err("stopped: it ran too long".into());
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(200)),
            Err(e) => return Err(e.to_string()),
        }
    }
    let file = if harness == Harness::Codex {
        &last
    } else {
        &out_file
    };
    std::fs::read_to_string(file).map_err(|e| e.to_string())
}

/// What one tidy did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Tidied {
    /// (the merged note, the notes it replaced)
    pub merged: Vec<(String, Vec<String>)>,
    pub rejected: Vec<String>,
}

/// Writes the merged notes of `answer` (`mux-note` blocks, each with a
/// `replaces:` line naming the notes it merges) and moves the notes they
/// replace to `notes/.merged/`. Only names of a group in `groups` may
/// be replaced; a pin on any of them stays on the merged note.
pub fn apply_tidy(dir: &Path, groups: &[Vec<String>], answer: &str, harness: &str) -> Tidied {
    let allowed: BTreeSet<&str> = groups.iter().flatten().map(String::as_str).collect();
    let mut out = Tidied::default();
    for block in super::save::note_blocks(answer) {
        let replaces: Vec<String> = note::split(&block)
            .and_then(|(fields, _)| {
                fields
                    .into_iter()
                    .find(|(k, _)| k == "replaces")
                    .map(|(_, v)| v)
            })
            .map(|v| {
                v.trim_matches(['[', ']'])
                    .split(',')
                    .map(|s| s.trim().trim_matches(['"', '\'']).to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        if replaces.len() < 2 || replaces.iter().any(|r| !allowed.contains(r.as_str())) {
            out.rejected.push(format!(
                "a block replaces {replaces:?}, not a group's notes"
            ));
            continue;
        }
        let mut n = match note::parse(&block, "") {
            Ok(n) => n,
            Err(e) => {
                out.rejected.push(e);
                continue;
            }
        };
        let olds: Vec<note::Note> = replaces
            .iter()
            .filter_map(|r| note::read(dir, r).ok())
            .collect();
        if olds.len() != replaces.len() {
            out.rejected.push(format!("{replaces:?}: a note is gone"));
            continue;
        }
        n.pinned |= olds.iter().any(|o| o.pinned);
        if olds
            .iter()
            .any(|o| o.confidence == note::Confidence::Stated)
        {
            n.confidence = note::Confidence::Stated;
        }
        n.source = note::Source {
            harness: harness.to_string(),
            launch: None,
            session: None,
            when: Some(super::now_rfc3339()),
        };
        // the originals first, so a merged note may keep one's name
        for r in &replaces {
            let _ = note::shelve(dir, r, MERGED);
        }
        match note::write(dir, &n) {
            Ok(_) => {
                log::append(
                    dir,
                    harness,
                    None,
                    "merged",
                    &format!("{} → notes/{}.md", replaces.join(" + "), n.name),
                );
                out.merged.push((n.name, replaces));
            }
            Err(e) => out.rejected.push(format!("{}: {e}", n.name)),
        }
    }
    out
}

/// The whole tidy of `dir`: the groups, one run, the merge. `cwd` is
/// where the harness runs.
pub fn tidy(
    dir: &Path,
    harness: Harness,
    prompts: &Prompts,
    index_lines: usize,
    cwd: &Path,
) -> Result<Tidied, String> {
    let groups = duplicates(dir);
    if groups.is_empty() {
        return Ok(Tidied::default());
    }
    let prompt = prompts
        .memory_tidy
        .replace("{groups}", &groups_text(dir, &groups));
    let answer = ask(harness, &prompt, cwd)?;
    let done = apply_tidy(dir, &groups, &answer, harness.as_str());
    let _ = index::rebuild(dir, index_lines, &prompts.memory_index_footer);
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn put(dir: &Path, name: &str, desc: &str, body: &str, extra: &str) {
        paths::ensure(dir).unwrap();
        std::fs::write(
            paths::note_path(dir, name),
            format!(
                "---\nname: {name}\ndescription: {desc}\nkind: feedback\n{extra}---\n\n{body}\n"
            ),
        )
        .unwrap();
    }

    #[test]
    fn search_ranks_name_over_description_over_body_and_needs_every_word() {
        let d = tempfile::tempdir().unwrap();
        let (dir, user) = (d.path().join("ws"), d.path().join("_user"));
        put(
            &dir,
            "postgres-version",
            "database is PostgreSQL 17",
            "chosen for replication",
            "",
        );
        put(
            &dir,
            "ci-cache",
            "the CI cache",
            "it pins postgres 16 images",
            "",
        );
        put(&user, "prefers-tabs", "indent with tabs", "everywhere", "");
        std::fs::create_dir_all(dir.join(paths::IMPORTS)).unwrap();
        std::fs::write(
            dir.join(paths::IMPORTS).join("claude.md"),
            "<!-- summary: x -->\n- [Postgres upgrade](/c/pg.md) — moved to postgres 17\n",
        )
        .unwrap();
        let hits = search(&dir, Some(&user), "postgres");
        let names: Vec<&str> = hits.iter().map(|h| h.line.as_str()).collect();
        assert!(names[0].contains("[postgres-version]"), "{names:?}");
        assert!(hits.iter().any(|h| h.place == "imported"), "{names:?}");
        let body = hits.iter().find(|h| h.line.contains("[ci-cache]")).unwrap();
        assert_eq!(body.snippet.as_deref(), Some("it pins postgres 16 images"));
        assert_eq!(search(&dir, Some(&user), "tabs")[0].place, "user");
        assert!(
            search(&dir, Some(&user), "postgres tabs").is_empty(),
            "every word"
        );
        assert!(search(&dir, None, "  ").is_empty());
    }

    #[test]
    fn alike_notes_are_grouped_and_different_ones_are_not() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path();
        put(
            dir,
            "never-push-to-master",
            "never push to master; work on a branch",
            "b",
            "",
        );
        put(
            dir,
            "no-push-master",
            "do not push to master, use a branch",
            "b",
            "",
        );
        put(
            dir,
            "branch-before-push",
            "push only from a branch, never master",
            "b",
            "",
        );
        put(
            dir,
            "staging-reset",
            "the staging database resets every night",
            "b",
            "",
        );
        assert_eq!(
            duplicates(dir),
            vec![vec![
                "branch-before-push".to_string(),
                "never-push-to-master".into(),
                "no-push-master".into()
            ]]
        );
    }

    #[test]
    fn tidy_writes_the_merged_note_and_moves_the_originals_aside() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path();
        put(
            dir,
            "a",
            "never push to master",
            "a body",
            "confidence: stated\npinned: true\n",
        );
        put(dir, "b", "do not push to master", "b body", "");
        put(dir, "c", "unrelated", "c", "");
        let groups = vec![vec!["a".to_string(), "b".into()]];
        let answer = "```mux-note\n---\nname: no-push-to-master\ndescription: never push to master; work on a branch\nkind: feedback\nconfidence: observed\nreplaces: a, b\n---\n\nmerged body\n```\n\
```mux-note\n---\nname: sneaky\ndescription: x\nreplaces: a, c\n---\nx\n```\n";
        let t = apply_tidy(dir, &groups, answer, "claude");
        assert_eq!(
            t.merged,
            vec![(
                "no-push-to-master".to_string(),
                vec!["a".to_string(), "b".into()]
            )]
        );
        assert_eq!(t.rejected.len(), 1, "c is not in a group");
        let n = note::read(dir, "no-push-to-master").unwrap();
        assert!(n.pinned && n.confidence == note::Confidence::Stated);
        assert!(!paths::note_path(dir, "a").exists());
        assert!(paths::notes_dir(dir).join(MERGED).join("a.md").is_file());
        assert!(paths::note_path(dir, "c").exists());
        let log = std::fs::read_to_string(dir.join(paths::LOG)).unwrap();
        assert!(
            log.contains("merged a + b → notes/no-push-to-master.md"),
            "{log}"
        );
    }
}
