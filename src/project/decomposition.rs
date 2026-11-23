//! `decomposition.md`: the draft of a Feature's Stories and Tasks (or an
//! RFC's Tasks) that a person reviews before any issue exists.
//!
//! The port of spec-wave's `lib/decomposition-doc.mjs` and
//! `lib/dependency-map.mjs` (read on 2026-11-18). The format has to be
//! readable, editable by hand and reversible: `render(parse(render(x)))`
//! equals `render(x)`, or a person's edit would vanish at the apply with
//! nobody noticing. The grammar (v1):
//!
//! ```text
//! # Decomposição — <the issue's title>
//! <!-- spec-wave:decomposition v1 issue=360 kind=stories -->
//!
//! ## Story 1 — <short title>
//!
//! **User story:** Como <perfil>, quero <objetivo>, para <benefício>
//! **Depende de:** Story 1, #412            (or "—" for none)
//! **Repos:** api, web                      (multi-repo only)
//! **Issue:** #523                          (written by the apply)
//!
//! <free body>
//!
//! ### Task 1.1 — <short title>
//!
//! <free body>
//! ```
//!
//! `kind=tasks` (an RFC) has no Stories: its Tasks are `## Task N — …`.
//! Only those three headings are structure; `## Backend` inside a body is
//! body. Position decides, not the number written: a Story inserted in the
//! middle without renumbering works.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The version this code writes and the highest it reads.
pub const DOC_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Task {
    pub title: String,
    pub body: String,
    /// The issue the apply created for it.
    pub issue: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Story {
    pub title: String,
    pub user_story: String,
    pub body: String,
    /// Earlier Stories of this document it depends on, as 0-based indexes.
    pub depends_on: Vec<usize>,
    /// Issues outside this document it depends on.
    pub depends_on_issues: Vec<u64>,
    /// Code repositories it touches, as slugs.
    pub repos: Vec<String>,
    pub issue: Option<u64>,
    pub tasks: Vec<Task>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A Feature: Stories, each with its Tasks.
    Stories,
    /// An RFC: Tasks alone.
    Tasks,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Stories => "stories",
            Kind::Tasks => "tasks",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Doc {
    pub kind: Kind,
    /// The issue's title, as the heading carries it.
    pub title: String,
    pub issue: Option<u64>,
    /// When the apply created the issues; `None` is a proposal.
    pub applied_at: Option<String>,
    /// Text between the marker and the first section.
    pub preamble: String,
    pub stories: Vec<Story>,
    /// The Tasks of a `kind=tasks` document.
    pub tasks: Vec<Task>,
}

fn invalid(reason: impl AsRef<str>) -> String {
    format!("decomposition.md inválido: {}.", reason.as_ref())
}

/// Tracks code fences as CommonMark does, so a `## Story 2` inside a
/// fenced block is body. The same tracker runs in the parser and in the
/// escaping of the renderer.
#[derive(Default)]
pub(crate) struct Fences {
    open: Option<(char, usize)>,
}

impl Fences {
    /// Whether a block was opened and never closed.
    pub(crate) fn is_open(&self) -> bool {
        self.open.is_some()
    }

    /// Whether `line` is inside a fenced block, the fence lines included.
    pub(crate) fn inside(&mut self, line: &str) -> bool {
        let indent = line.len() - line.trim_start_matches(' ').len();
        let rest = &line[indent..];
        let fence = rest.chars().next().filter(|c| *c == '`' || *c == '~');
        if indent <= 3
            && let Some(c) = fence
        {
            let len = rest.chars().take_while(|x| *x == c).count();
            let info = &rest[len..];
            if len >= 3 {
                match self.open {
                    None => {
                        if !(c == '`' && info.contains('`')) {
                            self.open = Some((c, len));
                            return true;
                        }
                    }
                    Some((oc, olen)) => {
                        if c == oc && len >= olen && info.trim().is_empty() {
                            self.open = None;
                            return true;
                        }
                    }
                }
            }
        }
        self.open.is_some()
    }

    fn marker(&self) -> Option<String> {
        self.open.map(|(c, n)| c.to_string().repeat(n))
    }
}

/// `## Story 3 — title`, `## Task 2 — title` or `### Task 1.2 — title`:
/// which, the number as written, and the title.
#[derive(Debug, PartialEq, Eq)]
enum Heading {
    Story,
    RfcTask,
    Task,
}

fn heading(line: &str) -> Option<(Heading, String)> {
    let (hashes, rest) = if let Some(r) = line.strip_prefix("###") {
        (3, r)
    } else {
        (2, line.strip_prefix("##")?)
    };
    if rest.starts_with('#') || !rest.starts_with([' ', '\t']) {
        return None;
    }
    let rest = rest.trim_start_matches([' ', '\t']);
    let lower = rest.to_lowercase();
    let (word, kind) = if lower.starts_with("story") && hashes == 2 {
        ("story", Heading::Story)
    } else if lower.starts_with("task") {
        (
            "task",
            if hashes == 2 {
                Heading::RfcTask
            } else {
                Heading::Task
            },
        )
    } else {
        return None;
    };
    let after = &rest[word.len()..];
    if !after.starts_with([' ', '\t']) {
        return None;
    }
    let after = after.trim_start_matches([' ', '\t']);
    let digits = after
        .char_indices()
        .take_while(|(_, c)| c.is_ascii_digit() || (*c == '.' && kind == Heading::Task))
        .last()
        .map(|(i, c)| i + c.len_utf8())?;
    let number = &after[..digits];
    if !number.starts_with(|c: char| c.is_ascii_digit()) || number.ends_with('.') {
        return None;
    }
    let mut title = after[digits..].trim_start_matches([' ', '\t']);
    if let Some(c) = title.chars().next()
        && "—–:-".contains(c)
    {
        title = title[c.len_utf8()..].trim_start_matches([' ', '\t']);
    }
    Some((kind, title.to_string()))
}

/// A line that would be read as structure: it gets a backslash when it
/// is body.
fn structural(line: &str) -> bool {
    heading(line.trim_start_matches('\\')).is_some()
}

fn flatten(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn trim_blank_edges<'a>(lines: &[&'a str]) -> Vec<&'a str> {
    let start = lines
        .iter()
        .position(|l| !l.trim().is_empty())
        .unwrap_or(lines.len());
    let end = lines
        .iter()
        .rposition(|l| !l.trim().is_empty())
        .map_or(start, |i| i + 1);
    lines[start..end].to_vec()
}

/// A body as the file holds it: blank edges trimmed, an open fence
/// closed, and a line that looks like structure escaped.
fn render_body(value: &str) -> String {
    let normal = value.replace("\r\n", "\n").replace('\r', "\n");
    let lines: Vec<&str> = normal.split('\n').collect();
    let lines = trim_blank_edges(&lines);
    if lines.is_empty() {
        return String::new();
    }
    let mut text = lines.join("\n");
    let mut scan = Fences::default();
    for l in text.split('\n') {
        scan.inside(l);
    }
    if let Some(m) = scan.marker() {
        text.push('\n');
        text.push_str(&m);
    }
    let mut scan = Fences::default();
    text.split('\n')
        .map(|l| {
            if scan.inside(l) || !structural(l) {
                l.to_string()
            } else {
                format!("\\{l}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn read_body(lines: &[&str]) -> String {
    let mut scan = Fences::default();
    trim_blank_edges(lines)
        .into_iter()
        .map(|l| {
            if !scan.inside(l) && l.starts_with('\\') && structural(l) {
                &l[1..]
            } else {
                l
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn repo_slug(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 39
        && s.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

pub fn render(doc: &Doc) -> String {
    let mut attrs = vec![format!("v{DOC_VERSION}")];
    if let Some(n) = doc.issue {
        attrs.push(format!("issue={n}"));
    }
    attrs.push(format!("kind={}", doc.kind.as_str()));
    if let Some(at) = &doc.applied_at {
        attrs.push(format!("applied={at}"));
    }
    let title = flatten(&doc.title);
    let head = if title.is_empty() {
        "# Decomposição".to_string()
    } else {
        format!("# Decomposição — {title}")
    };
    let mut blocks = vec![format!(
        "{head}\n<!-- spec-wave:decomposition {} -->",
        attrs.join(" ")
    )];
    let note = render_body(&doc.preamble);
    if !note.is_empty() {
        blocks.push(note);
    }
    let name = |t: &str| {
        let t = flatten(t);
        if t.is_empty() {
            "(sem título)".to_string()
        } else {
            t
        }
    };
    let push_task = |blocks: &mut Vec<String>, heading: String, task: &Task| {
        blocks.push(format!("{heading} — {}", name(&task.title)));
        if let Some(n) = task.issue {
            blocks.push(format!("**Issue:** #{n}"));
        }
        let body = render_body(&task.body);
        if !body.is_empty() {
            blocks.push(body);
        }
    };
    match doc.kind {
        Kind::Tasks => {
            for (i, t) in doc.tasks.iter().enumerate() {
                push_task(&mut blocks, format!("## Task {}", i + 1), t);
            }
        }
        Kind::Stories => {
            for (i, s) in doc.stories.iter().enumerate() {
                blocks.push(format!("## Story {} — {}", i + 1, name(&s.title)));
                let mut deps: Vec<usize> =
                    s.depends_on.iter().copied().filter(|d| *d < i).collect();
                deps.sort_unstable();
                deps.dedup();
                let mut outside = s.depends_on_issues.clone();
                outside.sort_unstable();
                outside.dedup();
                let refs: Vec<String> = deps
                    .iter()
                    .map(|d| format!("Story {}", d + 1))
                    .chain(outside.iter().map(|n| format!("#{n}")))
                    .collect();
                let us = flatten(&s.user_story);
                let mut fields = vec![
                    format!("**User story:** {}", if us.is_empty() { "—" } else { &us }),
                    format!(
                        "**Depende de:** {}",
                        if refs.is_empty() {
                            "—".to_string()
                        } else {
                            refs.join(", ")
                        }
                    ),
                ];
                let mut repos: Vec<String> = s
                    .repos
                    .iter()
                    .map(|r| r.trim().to_lowercase())
                    .filter(|r| repo_slug(r))
                    .collect();
                repos.sort();
                repos.dedup();
                if !repos.is_empty() {
                    fields.push(format!("**Repos:** {}", repos.join(", ")));
                }
                if let Some(n) = s.issue {
                    fields.push(format!("**Issue:** #{n}"));
                }
                blocks.push(fields.join("\n"));
                let body = render_body(&s.body);
                if !body.is_empty() {
                    blocks.push(body);
                }
                for (j, t) in s.tasks.iter().enumerate() {
                    push_task(&mut blocks, format!("### Task {}.{}", i + 1, j + 1), t);
                }
            }
        }
    }
    format!("{}\n", blocks.join("\n\n"))
}

/// The value of a `**Name:** value` line, when `line` is that field.
/// The colon may sit inside or outside the emphasis, or be missing.
fn field<'a>(line: &'a str, names: &[&str]) -> Option<&'a str> {
    let rest = line.strip_prefix("**")?;
    let end = rest.find("**")?;
    let label = rest[..end].trim_end_matches(':').trim().to_lowercase();
    if !names.contains(&label.as_str()) {
        return None;
    }
    Some(rest[end + 2..].trim_start_matches(':').trim())
}

fn empty_value(v: &str) -> bool {
    matches!(
        v.trim().to_lowercase().as_str(),
        "" | "—" | "–" | "-" | "nenhuma" | "nenhum" | "none" | "n/a"
    )
}

fn issue_value(v: &str) -> Option<u64> {
    v.trim().trim_start_matches('#').parse().ok()
}

/// A `**Depende de:**` value: earlier Stories (`Story 1`) and issues
/// (`#412`), and nothing else but separators.
fn parse_depends(
    raw: Option<&str>,
    index: usize,
    anchor: &str,
) -> Result<(Vec<usize>, Vec<u64>), String> {
    let Some(value) = raw.map(str::trim).filter(|v| !empty_value(v)) else {
        return Ok((vec![], vec![]));
    };
    let lower = value.to_lowercase();
    let mut siblings: Vec<usize> = Vec::new();
    let mut issues: Vec<u64> = Vec::new();
    let mut rest = String::new();
    let bytes: Vec<char> = lower.chars().collect();
    let mut i = 0;
    while i < bytes.len() {
        let tail: String = bytes[i..].iter().collect();
        if let Some(after) = tail.strip_prefix("story") {
            let skipped = after.len() - after.trim_start_matches([' ', '\t']).len();
            let digits: String = after[skipped..]
                .chars()
                .take_while(char::is_ascii_digit)
                .collect();
            if !digits.is_empty() {
                let n: usize = digits.parse().unwrap_or(0);
                if n < 1 || n > index {
                    return Err(invalid(format!(
                        "{anchor} depende de \"Story {n}\", que não é uma Story anterior a ela — dependências entre irmãs só apontam para trás (para outra Feature, use \"#<issue>\")"
                    )));
                }
                if !siblings.contains(&(n - 1)) {
                    siblings.push(n - 1);
                }
                i += 5 + skipped + digits.len();
                continue;
            }
        }
        if bytes[i] == '#' {
            let digits: String = bytes[i + 1..]
                .iter()
                .take_while(|c| c.is_ascii_digit())
                .collect();
            if !digits.is_empty() {
                if let Ok(n) = digits.parse::<u64>()
                    && n > 0
                    && !issues.contains(&n)
                {
                    issues.push(n);
                }
                i += 1 + digits.len();
                continue;
            }
        }
        rest.push(bytes[i]);
        i += 1;
    }
    if siblings.is_empty() && issues.is_empty() {
        return Err(invalid(format!(
            "não entendi \"**Depende de:** {value}\" em {anchor} — use \"Story 1\", \"#412\" ou \"—\""
        )));
    }
    let leftover: Vec<&str> = rest
        .split(|c: char| c.is_whitespace() || ",;+&/–—-".contains(c))
        .filter(|w| !w.is_empty() && *w != "e" && *w != "and")
        .collect();
    if !leftover.is_empty() {
        return Err(invalid(format!(
            "não entendi \"**Depende de:** {value}\" em {anchor} — sobrou \"{}\". Use só referências (\"Story 1, #412\") ou \"—\"",
            leftover.join(" ")
        )));
    }
    siblings.sort_unstable();
    issues.sort_unstable();
    Ok((siblings, issues))
}

fn parse_repos(raw: Option<&str>, anchor: &str) -> Result<Vec<String>, String> {
    let Some(value) = raw.map(str::trim).filter(|v| !empty_value(v)) else {
        return Ok(vec![]);
    };
    let mut out: Vec<String> = Vec::new();
    for slug in value
        .split(|c: char| c == ',' || c == ';' || c.is_whitespace())
        .filter(|s| !s.is_empty())
        .map(|s| s.trim_matches('`'))
    {
        if !repo_slug(slug) {
            return Err(invalid(format!(
                "não entendi \"**Repos:** {value}\" em {anchor} — \"{slug}\" não é um slug de repositório (minúsculas, [a-z0-9-], até 39 caracteres). Use os slugs de `repositories[]` do .spec-wave.json, separados por vírgula, ou \"—\""
            )));
        }
        if !out.iter().any(|s| s == slug) {
            out.push(slug.to_string());
        }
    }
    out.sort();
    Ok(out)
}

fn split_task(lines: &[&str]) -> (Option<u64>, String) {
    let first = lines
        .iter()
        .position(|l| !l.trim().is_empty())
        .unwrap_or(lines.len());
    match lines
        .get(first)
        .and_then(|l| field(l, &["issue"]))
        .and_then(issue_value)
    {
        Some(n) => (Some(n), read_body(&lines[first + 1..])),
        None => (None, read_body(&lines[first..])),
    }
}

fn require_title(title: &str, anchor: &str, hashes: &str) -> Result<String, String> {
    let t = flatten(title);
    if t.is_empty() {
        return Err(invalid(format!(
            "{anchor} está sem título — use \"{hashes} {anchor} — título curto\""
        )));
    }
    Ok(t)
}

pub fn parse(markdown: &str) -> Result<Doc, String> {
    let text = markdown.replace("\r\n", "\n").replace('\r', "\n");
    if text.trim().is_empty() {
        return Err(invalid("o arquivo está vazio"));
    }
    struct Section<'a> {
        kind: Heading,
        title: String,
        lines: Vec<&'a str>,
    }
    let mut scan = Fences::default();
    let mut sections: Vec<Section> = Vec::new();
    let mut preamble: Vec<&str> = Vec::new();
    // the marker's version, issue, kind and `applied=`
    type Marker = (u32, Option<u64>, Option<Kind>, Option<String>);
    let mut marker: Option<Marker> = None;
    let mut title = String::new();
    let mut saw_h1 = false;
    for line in text.split('\n') {
        if !scan.inside(line) {
            if let Some((kind, t)) = heading(line) {
                sections.push(Section {
                    kind,
                    title: t,
                    lines: Vec::new(),
                });
                continue;
            }
            if sections.is_empty() {
                let t = line.trim();
                if marker.is_none()
                    && let Some(inner) = t.strip_prefix("<!--").and_then(|x| x.strip_suffix("-->"))
                    && let Some(attrs) = inner.trim().strip_prefix("spec-wave:decomposition")
                {
                    let mut version = DOC_VERSION;
                    let (mut issue, mut kind, mut applied) = (None, None, None);
                    for w in attrs.split_whitespace() {
                        if let Some(v) = w.strip_prefix('v').and_then(|v| v.parse().ok()) {
                            version = v;
                        } else if let Some(v) = w.strip_prefix("issue=") {
                            issue = v.parse().ok();
                        } else if let Some(v) = w.strip_prefix("kind=") {
                            kind = match v.to_lowercase().as_str() {
                                "stories" => Some(Kind::Stories),
                                "tasks" => Some(Kind::Tasks),
                                _ => None,
                            };
                        } else if let Some(v) = w.strip_prefix("applied=") {
                            applied = Some(v.to_string());
                        }
                    }
                    marker = Some((version, issue, kind, applied));
                    continue;
                }
                if !saw_h1
                    && let Some(h1) = line.strip_prefix("# ").or_else(|| line.strip_prefix("#\t"))
                {
                    let flat = flatten(h1);
                    let lower = flat.to_lowercase();
                    title = match lower.strip_prefix("decomposição") {
                        Some(_) => {
                            let rest = flat["Decomposição".len()..].trim_start();
                            let rest = rest.strip_prefix(['—', '–', ':', '-']).unwrap_or(rest);
                            rest.trim().to_string()
                        }
                        None => flat,
                    };
                    saw_h1 = true;
                    continue;
                }
            }
        }
        match sections.last_mut() {
            Some(s) => s.lines.push(line),
            None => preamble.push(line),
        }
    }
    if scan.open.is_some() {
        return Err(invalid(
            "há um bloco de código (```) aberto e nunca fechado — feche-o para que os títulos seguintes voltem a ser reconhecidos",
        ));
    }
    let Some((version, issue, kind, applied_at)) = marker else {
        return Err(invalid(format!(
            "não encontrei o marcador `<!-- spec-wave:decomposition v{DOC_VERSION} … -->` — o arquivo não parece um decomposition.md do spec-wave"
        )));
    };
    if version > DOC_VERSION {
        return Err(invalid(format!(
            "está no formato v{version} e este agent-mux entende até v{DOC_VERSION}"
        )));
    }
    let kind = kind.unwrap_or(if sections.iter().any(|s| s.kind == Heading::Story) {
        Kind::Stories
    } else {
        Kind::Tasks
    });
    let mut doc = Doc {
        kind,
        title,
        issue,
        applied_at,
        preamble: read_body(&preamble),
        stories: Vec::new(),
        tasks: Vec::new(),
    };
    if kind == Kind::Tasks {
        if let Some(alien) = sections.iter().find(|s| s.kind != Heading::RfcTask) {
            return Err(invalid(format!(
                "em um documento kind=tasks só cabem títulos \"## Task N — …\"; encontrei \"{}\"",
                if alien.kind == Heading::Story {
                    "## Story"
                } else {
                    "### Task"
                }
            )));
        }
        for (i, s) in sections.iter().enumerate() {
            let anchor = format!("Task {}", i + 1);
            let (issue, body) = split_task(&s.lines);
            doc.tasks.push(Task {
                title: require_title(&s.title, &anchor, "##")?,
                body,
                issue,
            });
        }
        if doc.tasks.is_empty() {
            return Err(invalid("não encontrei nenhuma Task (\"## Task 1 — …\")"));
        }
        return Ok(doc);
    }
    for s in &sections {
        match s.kind {
            Heading::Story => {
                let index = doc.stories.len();
                let anchor = format!("Story {}", index + 1);
                // the fields sit in the first block of the section
                let mut i = s
                    .lines
                    .iter()
                    .position(|l| !l.trim().is_empty())
                    .unwrap_or(s.lines.len());
                let (mut us, mut dep, mut rep, mut iss): (
                    Option<&str>,
                    Option<&str>,
                    Option<&str>,
                    Option<u64>,
                ) = (None, None, None, None);
                let twice =
                    |name: &str| invalid(format!("{anchor} tem duas linhas \"**{name}:**\""));
                while i < s.lines.len() && !s.lines[i].trim().is_empty() {
                    let line = s.lines[i];
                    if let Some(v) = field(line, &["user story"]) {
                        if us.is_some() {
                            return Err(twice("User story"));
                        }
                        us = Some(v);
                    } else if let Some(v) = field(line, &["depende de"]) {
                        if dep.is_some() {
                            return Err(twice("Depende de"));
                        }
                        dep = Some(v);
                    } else if let Some(v) = field(
                        line,
                        &[
                            "repos",
                            "repo",
                            "repositórios",
                            "repositorios",
                            "repositório",
                            "repositorio",
                        ],
                    ) {
                        if rep.is_some() {
                            return Err(twice("Repos"));
                        }
                        rep = Some(v);
                    } else if let Some(n) = field(line, &["issue"]).and_then(issue_value) {
                        if iss.is_some() {
                            return Err(twice("Issue"));
                        }
                        iss = Some(n);
                    } else {
                        break;
                    }
                    i += 1;
                }
                let (depends_on, depends_on_issues) = parse_depends(dep, index, &anchor)?;
                doc.stories.push(Story {
                    title: require_title(&s.title, &anchor, "##")?,
                    user_story: us.filter(|v| !empty_value(v)).unwrap_or("").to_string(),
                    body: read_body(&s.lines[i..]),
                    depends_on,
                    depends_on_issues,
                    repos: parse_repos(rep, &anchor)?,
                    issue: iss,
                    tasks: Vec::new(),
                });
            }
            Heading::RfcTask => {
                return Err(invalid(
                    "\"## Task N\" aparece em um documento de Stories — Tasks de Story usam \"### Task N.M — …\"",
                ));
            }
            Heading::Task => {
                let n = doc.stories.len();
                let Some(story) = doc.stories.last_mut() else {
                    return Err(invalid(format!(
                        "a Task \"{}\" aparece antes de qualquer \"## Story N — …\"",
                        flatten(&s.title)
                    )));
                };
                let anchor = format!("Task {n}.{}", story.tasks.len() + 1);
                let (issue, body) = split_task(&s.lines);
                story.tasks.push(Task {
                    title: require_title(&s.title, &anchor, "###")?,
                    body,
                    issue,
                });
            }
        }
    }
    if doc.stories.is_empty() {
        return Err(invalid("não encontrei nenhuma Story (\"## Story 1 — …\")"));
    }
    Ok(doc)
}

/// A draft from a session's answer: `{"stories": [...]}` for a Feature,
/// `{"tasks": [...]}` for an RFC. A Story with no `dependsOn` depends on
/// the one before it, as spec-wave reads the model's answer.
pub fn from_model(result: &Value, kind: Kind, title: &str, issue: u64) -> Result<Doc, String> {
    let text = |v: &Value, k: &str| {
        v.get(k)
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string()
    };
    let task_of = |v: &Value| Task {
        title: text(v, "title"),
        body: text(v, "body"),
        issue: None,
    };
    let mut doc = Doc {
        kind,
        title: title.to_string(),
        issue: Some(issue),
        applied_at: None,
        preamble: String::new(),
        stories: Vec::new(),
        tasks: Vec::new(),
    };
    match kind {
        Kind::Tasks => {
            let list = result
                .get("tasks")
                .and_then(Value::as_array)
                .ok_or("the session answered no `tasks` list")?;
            doc.tasks = list.iter().map(task_of).collect();
            if doc.tasks.is_empty() || doc.tasks.iter().any(|t| t.title.is_empty()) {
                return Err("the draft has no Task, or a Task with no title".into());
            }
        }
        Kind::Stories => {
            let list = result
                .get("stories")
                .and_then(Value::as_array)
                .ok_or("the session answered no `stories` list")?;
            for (i, s) in list.iter().enumerate() {
                let depends_on = match s.get("dependsOn").and_then(Value::as_array) {
                    Some(d) => {
                        let mut v: Vec<usize> = d
                            .iter()
                            .filter_map(Value::as_u64)
                            .map(|n| n as usize)
                            .filter(|n| *n < i)
                            .collect();
                        v.sort_unstable();
                        v.dedup();
                        v
                    }
                    None if i > 0 => vec![i - 1],
                    None => vec![],
                };
                let repos = s
                    .get("repos")
                    .and_then(Value::as_array)
                    .map(|r| {
                        r.iter()
                            .filter_map(Value::as_str)
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default();
                let tasks: Vec<Task> = s
                    .get("tasks")
                    .and_then(Value::as_array)
                    .map(|t| t.iter().map(task_of).collect())
                    .unwrap_or_default();
                if text(s, "title").is_empty() {
                    return Err(format!("Story {} of the draft has no title", i + 1));
                }
                if tasks.is_empty() || tasks.iter().any(|t| t.title.is_empty()) {
                    return Err(format!(
                        "Story {} of the draft has no Task, or a Task with no title",
                        i + 1
                    ));
                }
                doc.stories.push(Story {
                    title: text(s, "title"),
                    user_story: text(s, "userStory"),
                    body: text(s, "body"),
                    depends_on,
                    depends_on_issues: Vec::new(),
                    repos,
                    issue: None,
                    tasks,
                });
            }
            if doc.stories.is_empty() {
                return Err("the draft has no Story".into());
            }
        }
    }
    Ok(doc)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MapStory {
    pub number: u64,
    pub title: String,
    #[serde(rename = "dependsOn")]
    pub depends_on: Vec<u64>,
    pub tasks: Vec<u64>,
}

/// `dependency-map.json` (v1): the Stories of a Feature with what each
/// depends on, written by the apply. The field order and the integer
/// numbers are part of the format: spec-wave's UI reads it strictly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DependencyMap {
    pub version: u32,
    pub feature: u64,
    #[serde(rename = "generatedAt")]
    pub generated_at: String,
    pub source: String,
    pub stories: Vec<MapStory>,
    pub order: Vec<u64>,
    pub cycle: Vec<u64>,
}

/// The map of an applied document; `None` when a Story has no issue yet.
pub fn dependency_map(doc: &Doc, feature: u64, generated_at: &str) -> Option<DependencyMap> {
    if doc.kind != Kind::Stories || doc.stories.iter().any(|s| s.issue.is_none()) {
        return None;
    }
    let stories: Vec<MapStory> = doc
        .stories
        .iter()
        .map(|s| {
            let number = s.issue.unwrap_or(0);
            let mut deps: Vec<u64> = s
                .depends_on
                .iter()
                .filter_map(|i| doc.stories.get(*i).and_then(|d| d.issue))
                .chain(s.depends_on_issues.iter().copied())
                .filter(|n| *n != number)
                .collect();
            deps.sort_unstable();
            deps.dedup();
            MapStory {
                number,
                title: s.title.clone(),
                depends_on: deps,
                tasks: s.tasks.iter().filter_map(|t| t.issue).collect(),
            }
        })
        .collect();
    let nodes: Vec<super::order::Node> = stories
        .iter()
        .map(|s| super::order::Node {
            number: s.number,
            depends_on: s.depends_on.clone(),
        })
        .collect();
    let ordered = super::order::order_nodes(&nodes);
    Some(DependencyMap {
        version: 1,
        feature,
        generated_at: generated_at.to_string(),
        source: "apply".to_string(),
        stories,
        order: ordered.order,
        cycle: ordered.cycle,
    })
}

/// The file's text: two-space JSON and a final newline, as spec-wave
/// writes it.
pub fn dependency_map_json(map: &DependencyMap) -> String {
    format!(
        "{}\n",
        serde_json::to_string_pretty(map).unwrap_or_default()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample() -> Doc {
        Doc {
            kind: Kind::Stories,
            title: "[FEATURE] Cadastro de Pedidos".into(),
            issue: Some(360),
            applied_at: None,
            preamble: String::new(),
            stories: vec![
                Story {
                    title: "visualizar meus pedidos".into(),
                    user_story: "Como cliente, quero visualizar meus pedidos, para acompanhar entregas".into(),
                    body: "Descrição complementar.".into(),
                    tasks: vec![Task {
                        title: "criar endpoint GET /pedidos".into(),
                        body: "Corpo técnico.".into(),
                        issue: None,
                    }],
                    ..Default::default()
                },
                Story {
                    title: "cancelar um pedido".into(),
                    user_story: "Como cliente, quero cancelar".into(),
                    body: "## Backend\n\n```\n## Story 9 — dentro de código\n```\n## Story 7 — parece título".into(),
                    depends_on: vec![0],
                    depends_on_issues: vec![412],
                    repos: vec!["web".into(), "api".into()],
                    tasks: vec![Task {
                        title: "criar endpoint DELETE".into(),
                        body: String::new(),
                        issue: None,
                    }],
                    ..Default::default()
                },
            ],
            tasks: vec![],
        }
    }

    #[test]
    fn the_file_is_spec_waves_format() {
        let text = render(&sample());
        assert!(text.starts_with(
            "# Decomposição — [FEATURE] Cadastro de Pedidos\n<!-- spec-wave:decomposition v1 issue=360 kind=stories -->\n\n## Story 1 — visualizar meus pedidos\n\n**User story:** Como cliente, quero visualizar meus pedidos, para acompanhar entregas\n**Depende de:** —\n\nDescrição complementar.\n\n### Task 1.1 — criar endpoint GET /pedidos\n\nCorpo técnico.\n\n## Story 2 — cancelar um pedido\n\n**User story:** Como cliente, quero cancelar\n**Depende de:** Story 1, #412\n**Repos:** api, web\n"
        ), "{text}");
        // a body line that looks like a section is escaped; one in code is not
        assert!(text.contains("\n\\## Story 7 — parece título\n"));
        assert!(text.contains("```\n## Story 9 — dentro de código\n```"));
        // `## Backend` is body, never structure
        assert!(text.contains("\n## Backend\n"));
        assert!(text.ends_with("### Task 2.1 — criar endpoint DELETE\n"));
    }

    #[test]
    fn rendering_is_reversible() {
        let doc = sample();
        let text = render(&doc);
        let back = parse(&text).unwrap();
        // the repos come back sorted, as the renderer wrote them
        let mut expected = doc.clone();
        expected.stories[1].repos = vec!["api".into(), "web".into()];
        assert_eq!(back, expected);
        assert_eq!(render(&back), text);
    }

    #[test]
    fn position_decides_not_the_number_written() {
        let text = "# Decomposição — x\n<!-- spec-wave:decomposition v1 issue=1 kind=stories -->\n\n## Story 1 — a\n\n**Depende de:** —\n\n### Task 1.1 — t\n\n## Story 9 — inserida sem renumerar\n\n**Depende de:** Story 1\n\n### Task 9.4 — u\n\n## Story 2: c\n\n**Depende de**: story 2 e #7\n\n### Task 2.1 - v\n";
        let doc = parse(text).unwrap();
        assert_eq!(doc.stories.len(), 3);
        assert_eq!(doc.stories[1].title, "inserida sem renumerar");
        assert_eq!(doc.stories[1].depends_on, vec![0]);
        assert_eq!(doc.stories[2].depends_on, vec![1]);
        assert_eq!(doc.stories[2].depends_on_issues, vec![7]);
        assert_eq!(doc.stories[2].tasks[0].title, "v");
        // written back, the numbers are the positions
        assert!(render(&doc).contains("## Story 2 — inserida sem renumerar"));
        assert!(render(&doc).contains("### Task 2.1 — u"));
    }

    #[test]
    fn what_a_person_may_get_wrong_is_an_error_not_a_guess() {
        let head = "# Decomposição — x\n<!-- spec-wave:decomposition v1 kind=stories -->\n\n";
        let bad = |body: &str| parse(&format!("{head}{body}")).unwrap_err();
        // a dependency forward, or on itself
        assert!(
            bad("## Story 1 — a\n\n**Depende de:** Story 2\n\n### Task 1.1 — t\n")
                .contains("só apontam para trás")
        );
        assert!(
            bad("## Story 1 — a\n\n**Depende de:** Story 1\n").contains("não é uma Story anterior")
        );
        // prose in the dependency line
        assert!(
            bad("## Story 1 — a\n\n**Depende de:** #12 no sistema legado\n").contains("sobrou")
        );
        assert!(bad("## Story 1 — a\n\n**Depende de:** a anterior\n").contains("não entendi"));
        assert!(bad("## Story 1 —\n").contains("sem título"));
        assert!(bad("### Task 1.1 — t\n").contains("antes de qualquer"));
        assert!(bad("## Story 1 — a\n\n**Repos:** API_x\n").contains("slug"));
        assert!(bad("## Story 1 — a\n\n```\naberto\n").contains("nunca fechado"));
        assert!(
            parse("# Decomposição\n\n## Story 1 — a\n")
                .unwrap_err()
                .contains("marcador")
        );
        assert!(
            parse("# x\n<!-- spec-wave:decomposition v2 kind=stories -->\n\n## Story 1 — a\n")
                .unwrap_err()
                .contains("v2")
        );
        assert!(parse(" \n").unwrap_err().contains("vazio"));
        assert!(parse(head).unwrap_err().contains("nenhuma Story"));
    }

    #[test]
    fn an_rfc_has_tasks_alone() {
        let doc = Doc {
            kind: Kind::Tasks,
            title: "[RFC] Novo fluxo".into(),
            issue: Some(5),
            applied_at: Some("2026-01-01T00:00:00.000Z".into()),
            preamble: String::new(),
            stories: vec![],
            tasks: vec![Task {
                title: "migrar".into(),
                body: "corpo".into(),
                issue: Some(40),
            }],
        };
        let text = render(&doc);
        assert_eq!(
            text,
            "# Decomposição — [RFC] Novo fluxo\n<!-- spec-wave:decomposition v1 issue=5 kind=tasks applied=2026-01-01T00:00:00.000Z -->\n\n## Task 1 — migrar\n\n**Issue:** #40\n\ncorpo\n"
        );
        assert_eq!(parse(&text).unwrap(), doc);
        assert!(
            parse(&text.replace("## Task 1", "## Story 1"))
                .unwrap_err()
                .contains("kind=tasks")
        );
    }

    #[test]
    fn a_models_draft_becomes_a_document() {
        let doc = from_model(
            &json!({"stories": [
                {"title": "a", "userStory": "Como x", "body": "b", "dependsOn": [], "tasks": [{"title": "t1", "body": "c"}]},
                {"title": "b", "repos": ["api"], "tasks": [{"title": "t2", "body": ""}]},
                {"title": "c", "dependsOn": [0, 5, 2], "tasks": [{"title": "t3", "body": ""}]}
            ]}),
            Kind::Stories,
            "[FEATURE] x",
            9,
        )
        .unwrap();
        assert_eq!(doc.stories[0].depends_on, Vec::<usize>::new());
        // no `dependsOn`: after the one before
        assert_eq!(doc.stories[1].depends_on, vec![0]);
        // only earlier Stories count
        assert_eq!(doc.stories[2].depends_on, vec![0]);
        assert_eq!(doc.stories[1].repos, vec!["api"]);
        assert!(parse(&render(&doc)).is_ok());
        assert!(from_model(&json!({"stories": []}), Kind::Stories, "", 1).is_err());
        assert!(
            from_model(
                &json!({"stories": [{"title": "a", "tasks": []}]}),
                Kind::Stories,
                "",
                1
            )
            .is_err()
        );
        assert!(from_model(&json!({}), Kind::Tasks, "", 1).is_err());
    }

    #[test]
    fn the_dependency_map_is_byte_for_byte_spec_waves() {
        let mut doc = sample();
        assert_eq!(
            dependency_map(&doc, 42, "t"),
            None,
            "a proposal has no numbers"
        );
        doc.stories[0].title = "A".into();
        doc.stories[0].issue = Some(10);
        doc.stories[0].tasks[0].issue = Some(20);
        doc.stories[1].title = "B".into();
        doc.stories[1].issue = Some(11);
        doc.stories[1].depends_on_issues.clear();
        let map = dependency_map(&doc, 42, "2026-01-01T00:00:00.000Z").unwrap();
        assert_eq!(
            dependency_map_json(&map),
            [
                "{",
                "  \"version\": 1,",
                "  \"feature\": 42,",
                "  \"generatedAt\": \"2026-01-01T00:00:00.000Z\",",
                "  \"source\": \"apply\",",
                "  \"stories\": [",
                "    {",
                "      \"number\": 10,",
                "      \"title\": \"A\",",
                "      \"dependsOn\": [],",
                "      \"tasks\": [",
                "        20",
                "      ]",
                "    },",
                "    {",
                "      \"number\": 11,",
                "      \"title\": \"B\",",
                "      \"dependsOn\": [",
                "        10",
                "      ],",
                "      \"tasks\": []",
                "    }",
                "  ],",
                "  \"order\": [",
                "    10,",
                "    11",
                "  ],",
                "  \"cycle\": []",
                "}",
                "",
            ]
            .join("\n")
        );
    }
}
