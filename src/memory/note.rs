//! One note: a Markdown file with a small YAML frontmatter
//! (`docs/memory.md` has the shape). The reader is tolerant: scalars,
//! quoted scalars and one level of nested maps (`source:`, and the
//! `metadata:` map Claude Code's own memory files use), flattened to
//! `source.harness`-style keys.

use super::paths;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    User,
    Feedback,
    Decision,
    Project,
    Reference,
    Handoff,
}

impl Kind {
    pub const ALL: [Kind; 6] = [
        Kind::User,
        Kind::Feedback,
        Kind::Decision,
        Kind::Project,
        Kind::Reference,
        Kind::Handoff,
    ];

    pub fn parse(s: &str) -> Option<Kind> {
        Kind::ALL
            .into_iter()
            .find(|k| k.as_str() == s.trim().to_ascii_lowercase())
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Kind::User => "user",
            Kind::Feedback => "feedback",
            Kind::Decision => "decision",
            Kind::Project => "project",
            Kind::Reference => "reference",
            Kind::Handoff => "handoff",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confidence {
    /// The user said it.
    Stated,
    /// The model inferred it.
    Observed,
    /// Mirrored from a harness's own memory.
    Imported,
}

impl Confidence {
    pub fn parse(s: &str) -> Option<Confidence> {
        match s.trim().to_ascii_lowercase().as_str() {
            "stated" => Some(Confidence::Stated),
            "observed" => Some(Confidence::Observed),
            "imported" => Some(Confidence::Imported),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Confidence::Stated => "stated",
            Confidence::Observed => "observed",
            Confidence::Imported => "imported",
        }
    }
}

/// Who wrote a note.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Source {
    /// `claude`, `codex`, `agy`, `agent-mux`, `keeper`, `user`.
    pub harness: String,
    /// The agent-mux launch id (`$AGENT_MUX_SESSION_ID`).
    pub launch: Option<String>,
    /// The harness's conversation id, when the writer knows it.
    pub session: Option<String>,
    /// RFC 3339.
    pub when: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Note {
    pub name: String,
    pub description: String,
    pub kind: Kind,
    pub source: Source,
    pub confidence: Confidence,
    pub pinned: bool,
    /// `YYYY-MM-DD`: past this day the note is moved to `notes/.expired/`.
    pub expires_at: Option<String>,
    pub body: String,
}

/// Whether `name` is a valid note name (and file stem).
pub fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
        && name.len() <= 80
}

/// A note name from free text: lower case, words joined by `-`.
pub fn slug(text: &str) -> String {
    let mut out = String::new();
    for c in text.trim().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    let out = out.trim_end_matches('-');
    out.chars()
        .take(80)
        .collect::<String>()
        .trim_end_matches('-')
        .to_string()
}

fn is_date(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b.iter()
            .enumerate()
            .all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit())
}

fn unquote(v: &str) -> String {
    let v = v.trim();
    if v.len() >= 2 && v.starts_with('"') && v.ends_with('"') {
        let inner = &v[1..v.len() - 1];
        let mut out = String::new();
        let mut esc = false;
        for c in inner.chars() {
            if esc {
                out.push(match c {
                    'n' => '\n',
                    't' => '\t',
                    other => other,
                });
                esc = false;
            } else if c == '\\' {
                esc = true;
            } else {
                out.push(c);
            }
        }
        return out;
    }
    if v.len() >= 2 && v.starts_with('\'') && v.ends_with('\'') {
        return v[1..v.len() - 1].replace("''", "'");
    }
    v.to_string()
}

/// The frontmatter as flat keys (`source.harness`) and the body.
pub fn split(text: &str) -> Option<(Vec<(String, String)>, String)> {
    let text = text.trim_start_matches('\u{feff}');
    let mut lines = text.split_inclusive('\n');
    if lines.next()?.trim_end() != "---" {
        return None;
    }
    let mut fields = Vec::new();
    let mut parent: Option<String> = None;
    let mut consumed = text.split_inclusive('\n').next()?.len();
    let mut closed = false;
    for raw in lines {
        consumed += raw.len();
        let line = raw.trim_end_matches(['\n', '\r']);
        if line.trim_end() == "---" {
            closed = true;
            break;
        }
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        let nested = line.starts_with(' ') || line.starts_with('\t');
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        let key = k.trim().to_string();
        let value = strip_comment(v);
        if nested {
            if let Some(p) = &parent {
                fields.push((format!("{p}.{key}"), unquote(value)));
            }
            continue;
        }
        if value.is_empty() {
            parent = Some(key);
        } else {
            parent = None;
            fields.push((key, unquote(value)));
        }
    }
    if !closed {
        return None;
    }
    let body = text
        .get(consumed..)
        .unwrap_or("")
        .trim_start_matches(['\n', '\r']);
    Some((fields, body.to_string()))
}

/// `value   # comment` → `value`, leaving quoted values alone.
fn strip_comment(v: &str) -> &str {
    let v = v.trim();
    if v.starts_with('"') || v.starts_with('\'') {
        return v;
    }
    match v.find(" #") {
        Some(i) => v[..i].trim_end(),
        None => v,
    }
}

/// Reads a note. `stem` is the file name without `.md`, used when the
/// frontmatter has no `name`.
pub fn parse(text: &str, stem: &str) -> Result<Note, String> {
    let (fields, body) = split(text).ok_or("no `---` frontmatter block")?;
    let get = |k: &str| {
        fields
            .iter()
            .find(|(key, _)| key == k)
            .map(|(_, v)| v.as_str())
    };
    let name = get("name").unwrap_or(stem).trim().to_string();
    if !valid_name(&name) {
        return Err(format!(
            "name {name:?} must be lower case letters, digits, - and _"
        ));
    }
    let description = get("description").unwrap_or("").trim().to_string();
    if description.is_empty() {
        return Err("description is missing".into());
    }
    // Claude Code's own files say `metadata: type:` (or `type:`)
    let claude_shaped = get("source.harness").is_none()
        && (get("metadata.type").is_some() || get("type").is_some());
    let kind_text = get("kind")
        .or_else(|| get("metadata.type"))
        .or_else(|| get("type"))
        .unwrap_or("project");
    let kind = Kind::parse(kind_text).ok_or_else(|| {
        format!(
            "kind {kind_text:?} is not one of {}",
            Kind::ALL.map(|k| k.as_str()).join(", ")
        )
    })?;
    let confidence = match get("confidence") {
        Some(c) => Confidence::parse(c)
            .ok_or_else(|| format!("confidence {c:?} is not stated, observed or imported"))?,
        None if claude_shaped => Confidence::Imported,
        None => Confidence::Observed,
    };
    let pinned = match get("pinned") {
        None => false,
        Some(v) => match v.trim() {
            "true" | "yes" => true,
            "false" | "no" | "" => false,
            other => return Err(format!("pinned {other:?} is not true or false")),
        },
    };
    let expires_at = match get("expires_at").map(str::trim).filter(|v| !v.is_empty()) {
        None => None,
        Some(d) if is_date(d) => Some(d.to_string()),
        Some(d) => return Err(format!("expires_at {d:?} is not YYYY-MM-DD")),
    };
    let opt = |k: &str| {
        get(k)
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(String::from)
    };
    let source = Source {
        harness: opt("source.harness")
            .unwrap_or_else(|| if claude_shaped { "claude" } else { "unknown" }.to_string()),
        launch: opt("source.launch"),
        session: opt("source.session").or_else(|| opt("metadata.originSessionId")),
        when: opt("source.when"),
    };
    Ok(Note {
        name,
        description,
        kind,
        source,
        confidence,
        pinned,
        expires_at,
        body,
    })
}

/// A scalar as YAML that `split` reads back: plain when it is safe,
/// double-quoted otherwise.
fn scalar(v: &str) -> String {
    let one_line = v.replace(['\n', '\r'], " ");
    let plain = !one_line.is_empty()
        && !one_line.starts_with([
            '"', '\'', '[', '{', '#', '&', '*', '!', '|', '>', '%', '@', '`', '-', ' ',
        ])
        && !one_line.ends_with(' ')
        && !one_line.contains(" #")
        && !one_line.contains(": ");
    if plain {
        one_line
    } else {
        format!(
            "\"{}\"",
            one_line.replace('\\', "\\\\").replace('"', "\\\"")
        )
    }
}

/// The note as its file.
pub fn render(n: &Note) -> String {
    let mut out = String::from("---\n");
    out.push_str(&format!("name: {}\n", n.name));
    out.push_str(&format!("description: {}\n", scalar(&n.description)));
    out.push_str(&format!("kind: {}\n", n.kind.as_str()));
    out.push_str("source:\n");
    out.push_str(&format!("  harness: {}\n", scalar(&n.source.harness)));
    for (k, v) in [
        ("launch", &n.source.launch),
        ("session", &n.source.session),
        ("when", &n.source.when),
    ] {
        if let Some(v) = v {
            out.push_str(&format!("  {k}: {}\n", scalar(v)));
        }
    }
    out.push_str(&format!("confidence: {}\n", n.confidence.as_str()));
    if n.pinned {
        out.push_str("pinned: true\n");
    }
    if let Some(d) = &n.expires_at {
        out.push_str(&format!("expires_at: {d}\n"));
    }
    out.push_str("---\n\n");
    out.push_str(n.body.trim_end());
    out.push('\n');
    out
}

/// Writes `notes/<name>.md` atomically; returns its path.
pub fn write(dir: &Path, n: &Note) -> std::io::Result<PathBuf> {
    let path = paths::note_path(dir, &n.name);
    paths::write_atomic(&path, &render(n))?;
    Ok(path)
}

/// Reads `notes/<name>.md`.
pub fn read(dir: &Path, name: &str) -> Result<Note, String> {
    let path = paths::note_path(dir, name);
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    parse(&text, name)
}

/// Every `notes/*.md` file (no subfolders), sorted by name.
pub fn files(dir: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(paths::notes_dir(dir))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && p.extension().is_some_and(|e| e == "md")
                && !p
                    .file_name()
                    .is_some_and(|f| f.to_string_lossy().starts_with('.'))
        })
        .collect();
    out.sort();
    out
}

/// Every note in the folder, with the files that would not parse.
pub fn load_all(dir: &Path) -> (Vec<Note>, Vec<(PathBuf, String)>) {
    let mut notes = Vec::new();
    let mut problems = Vec::new();
    for path in files(dir) {
        let stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        match std::fs::read_to_string(&path)
            .map_err(|e| e.to_string())
            .and_then(|t| parse(&t, &stem))
        {
            Ok(n) if n.name != stem => problems.push((
                path,
                format!("name {:?} does not match the file name {stem:?}", n.name),
            )),
            Ok(n) => notes.push(n),
            Err(e) => problems.push((path, e)),
        }
    }
    (notes, problems)
}

/// Moves `notes/<name>.md` into `notes/<sub>/` (`.removed`, `.expired`);
/// nothing is ever deleted. Returns the new path.
pub fn shelve(dir: &Path, name: &str, sub: &str) -> std::io::Result<PathBuf> {
    shelve_path(&paths::note_path(dir, name), sub)
}

/// `shelve` for a note file given by path.
pub fn shelve_path(path: &Path, sub: &str) -> std::io::Result<PathBuf> {
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("a note path has a folder"))?;
    let target_dir = parent.join(sub);
    std::fs::create_dir_all(&target_dir)?;
    let file = path
        .file_name()
        .ok_or_else(|| std::io::Error::other("a note path has a file name"))?;
    let mut target = target_dir.join(file);
    if target.exists() {
        let stem = path.file_stem().unwrap_or_default().to_string_lossy();
        let stamp = time::OffsetDateTime::now_utc().unix_timestamp();
        target = target_dir.join(format!("{stem}.{stamp}.md"));
    }
    std::fs::rename(path, &target)?;
    Ok(target)
}

/// A new note's text, for `n` in the Configuration view and
/// `agent-mux memory add` without a body.
pub fn template(name: &str, kind: Kind) -> String {
    render(&Note {
        name: name.to_string(),
        description: "one line: what this note says, used to decide when to read it".into(),
        kind,
        source: Source {
            harness: "user".into(),
            when: Some(super::now_rfc3339()),
            ..Default::default()
        },
        confidence: Confidence::Stated,
        pinned: false,
        expires_at: None,
        body: "The fact.\n\n**Why:** \n\n**How to apply:** ".into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note() -> Note {
        Note {
            name: "vt100-grid".into(),
            description: "a 1-column grid panics: give the pty a winsize".into(),
            kind: Kind::Project,
            source: Source {
                harness: "claude".into(),
                launch: Some("l-1".into()),
                session: Some("546af05a".into()),
                when: Some("2026-10-10T12:30:00Z".into()),
            },
            confidence: Confidence::Observed,
            pinned: true,
            expires_at: Some("2026-11-01".into()),
            body: "Body with [[link]].\n".into(),
        }
    }

    #[test]
    fn a_note_round_trips_through_its_file() {
        let n = note();
        let text = render(&n);
        assert!(text.starts_with("---\nname: vt100-grid\n"), "{text}");
        assert!(text.contains("  harness: claude\n  launch: l-1\n"));
        assert_eq!(parse(&text, "vt100-grid").unwrap(), n);
        let mut odd = note();
        odd.description = "\"quoted\": with # and colon: yes".into();
        assert_eq!(parse(&render(&odd), "vt100-grid").unwrap(), odd);
    }

    #[test]
    fn a_claude_memory_file_reads_as_imported() {
        let text = "---\nname: work-on-master\ndescription: commit on master\nmetadata:\n  type: feedback\n  originSessionId: abc\n---\n\nBody\n";
        let n = parse(text, "work-on-master").unwrap();
        assert_eq!(n.kind, Kind::Feedback);
        assert_eq!(n.confidence, Confidence::Imported);
        assert_eq!(n.source.harness, "claude");
        assert_eq!(n.source.session.as_deref(), Some("abc"));
        assert_eq!(n.body, "Body\n");
    }

    #[test]
    fn bad_notes_say_what_is_wrong() {
        assert!(parse("no frontmatter", "x").is_err());
        assert!(
            parse("---\nname: x\n---\n", "x")
                .unwrap_err()
                .contains("description")
        );
        assert!(
            parse("---\nname: X Y\ndescription: d\n---\n", "x")
                .unwrap_err()
                .contains("name")
        );
        assert!(
            parse("---\ndescription: d\nkind: gossip\n---\n", "x")
                .unwrap_err()
                .contains("kind")
        );
        assert!(
            parse("---\ndescription: d\nexpires_at: soon\n---\n", "x")
                .unwrap_err()
                .contains("expires_at")
        );
        let n = parse("---\ndescription: d  # why\n---\nb", "from-stem").unwrap();
        assert_eq!(
            (n.name.as_str(), n.description.as_str()),
            ("from-stem", "d")
        );
    }

    #[test]
    fn names_and_slugs() {
        assert!(valid_name("a-b_1") && !valid_name("-a") && !valid_name("A") && !valid_name(""));
        assert_eq!(
            slug("  We commit on Master, no PRs! "),
            "we-commit-on-master-no-prs"
        );
    }

    #[test]
    fn write_load_and_shelve() {
        let d = tempfile::tempdir().unwrap();
        paths::ensure(d.path()).unwrap();
        write(d.path(), &note()).unwrap();
        std::fs::write(d.path().join("notes/broken.md"), "nope").unwrap();
        std::fs::write(d.path().join("notes/other.md"), render(&note())).unwrap();
        let (notes, problems) = load_all(d.path());
        assert_eq!(notes.len(), 1);
        assert_eq!(problems.len(), 2, "{problems:?}");
        let moved = shelve(d.path(), "vt100-grid", paths::REMOVED).unwrap();
        assert!(moved.ends_with("notes/.removed/vt100-grid.md"));
        assert!(load_all(d.path()).0.is_empty());
        assert!(parse(&template("t", Kind::User), "t").is_ok());
    }
}
