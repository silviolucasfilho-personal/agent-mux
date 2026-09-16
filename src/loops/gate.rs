//! `gate.yaml`: the path denylist, the file cap and the auto-merge
//! allowlist of a workspace, and the decision a set of changed paths
//! gets against them. Evaluation is most-severe-first and short-circuits:
//! denylist → file count → (auto-merge only) allowlist → ok.
//!
//! The file is a small YAML subset (comments, `key: value`, `- item`
//! lists) parsed here without a YAML dependency; unknown keys are ignored
//! so a hand-extended file still loads.
//!
//! Globs are `globset` patterns compiled with `literal_separator(false)`:
//! `*` and `**` span `/`, and dotfiles are ordinary characters, so
//! `**/.env` matches `.env` and `a/.env`, and `**/secrets/**` matches
//! `.hidden/secrets/x` (a dot-directory is never skipped). `**/secrets/**`
//! does not match `.secrets/prod.json`: `.secrets` is not `secrets`.

use globset::{Glob, GlobBuilder, GlobSet, GlobSetBuilder};
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateConfig {
    pub version: u32,
    pub denylist: Vec<String>,
    pub max_files: Option<u32>,
    pub auto_merge_allowlist: Vec<String>,
}

/// The twelve conservative denylist globs the template ships.
pub const DEFAULT_DENYLIST: [&str; 12] = [
    "**/.env",
    "**/.env.*",
    "**/secrets/**",
    "**/auth/**",
    "**/payments/**",
    "**/migrations/**",
    "**/*.pem",
    "**/*.key",
    "**/id_rsa*",
    "**/credentials*",
    "**/.github/workflows/**",
    "**/infra/**",
];

pub fn default_config() -> GateConfig {
    GateConfig {
        version: 1,
        denylist: DEFAULT_DENYLIST.iter().map(|s| s.to_string()).collect(),
        max_files: Some(10),
        auto_merge_allowlist: vec!["docs/**".into(), "**/*.md".into()],
    }
}

/// What a decision was made on. `Merge` behaves like `Commit`; only
/// `AutoMerge` consults the allowlist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Commit,
    Merge,
    AutoMerge,
}

impl Action {
    pub fn parse(s: &str) -> Option<Action> {
        match s.trim() {
            "commit" => Some(Action::Commit),
            "merge" => Some(Action::Merge),
            "auto-merge" | "auto_merge" | "automerge" => Some(Action::AutoMerge),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    Ok,
    Denylist,
    FileCount,
    NotAllowlisted,
}

impl Trigger {
    pub fn as_str(self) -> &'static str {
        match self {
            Trigger::Ok => "ok",
            Trigger::Denylist => "denylist",
            Trigger::FileCount => "file-count",
            Trigger::NotAllowlisted => "not-allowlisted",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    pub allowed: bool,
    pub trigger: Trigger,
    pub reason: String,
    pub matched_paths: Vec<String>,
}

/// A compiled glob list that remembers which pattern matched.
#[derive(Debug, Clone)]
pub struct Matcher {
    set: GlobSet,
    globs: Vec<String>,
}

fn compile(glob: &str) -> Result<Glob, String> {
    GlobBuilder::new(glob)
        .literal_separator(false)
        .build()
        .map_err(|e| format!("invalid glob {glob:?}: {e}"))
}

impl Matcher {
    pub fn new(globs: &[String]) -> Result<Matcher, String> {
        let mut builder = GlobSetBuilder::new();
        for g in globs {
            builder.add(compile(g)?);
        }
        let set = builder
            .build()
            .map_err(|e| format!("cannot build the glob set: {e}"))?;
        Ok(Matcher {
            set,
            globs: globs.to_vec(),
        })
    }

    /// An empty matcher matches nothing.
    pub fn empty() -> Matcher {
        Matcher {
            set: GlobSet::empty(),
            globs: Vec::new(),
        }
    }

    /// The first glob `path` matches, tried as given and with a leading
    /// `./` stripped.
    pub fn first_match(&self, path: &str) -> Option<&str> {
        let candidates = [path, path.strip_prefix("./").unwrap_or(path)];
        for c in candidates {
            if let Some(i) = self.set.matches(c).into_iter().min() {
                return self.globs.get(i).map(String::as_str);
            }
        }
        None
    }

    pub fn is_match(&self, path: &str) -> bool {
        self.first_match(path).is_some()
    }
}

/// A path relative to `root` when it lies under it (forward slashes),
/// else the path as given with a leading `./` stripped.
pub fn relative_to(root: &Path, path: &str) -> String {
    let p = Path::new(path);
    let rel = if p.is_absolute() {
        p.strip_prefix(root).unwrap_or(p).to_path_buf()
    } else {
        p.to_path_buf()
    };
    let s = rel.to_string_lossy().replace('\\', "/");
    s.strip_prefix("./").unwrap_or(&s).to_string()
}

fn strip_comment(line: &str) -> &str {
    // a `#` outside quotes starts a comment
    let mut in_single = false;
    let mut in_double = false;
    for (i, c) in line.char_indices() {
        match c {
            '\'' if !in_double => in_single = !in_single,
            '"' if !in_single => in_double = !in_double,
            '#' if !in_single && !in_double => return &line[..i],
            _ => {}
        }
    }
    line
}

fn unquote(s: &str) -> String {
    let t = s.trim();
    if t.len() >= 2 {
        let b = t.as_bytes();
        if (b[0] == b'"' && b[t.len() - 1] == b'"') || (b[0] == b'\'' && b[t.len() - 1] == b'\'') {
            return t[1..t.len() - 1].to_string();
        }
    }
    t.to_string()
}

/// Parses the YAML subset of `gate.yaml`.
pub fn parse(text: &str) -> Result<GateConfig, String> {
    let mut version: Option<u32> = None;
    let mut denylist: Option<Vec<String>> = None;
    let mut max_files: Option<u32> = None;
    let mut allowlist: Option<Vec<String>> = None;
    // the list currently being filled by `- item` lines
    enum Target {
        None,
        Deny,
        Allow,
        Other,
    }
    let mut target = Target::None;
    for raw in text.lines() {
        let line = strip_comment(raw);
        if line.trim().is_empty() {
            continue;
        }
        let trimmed = line.trim_start();
        if let Some(item) = trimmed
            .strip_prefix("- ")
            .or_else(|| (trimmed == "-").then_some(""))
        {
            let value = unquote(item);
            match target {
                Target::Deny => denylist.get_or_insert_with(Vec::new).push(value),
                Target::Allow => allowlist.get_or_insert_with(Vec::new).push(value),
                Target::Other => {}
                Target::None => {
                    return Err(format!("list item outside a list: {raw:?}"));
                }
            }
            continue;
        }
        let Some((key, value)) = trimmed.split_once(':') else {
            return Err(format!("cannot parse line: {raw:?}"));
        };
        let key = key.trim();
        let value = value.trim();
        match key {
            "version" => {
                version = Some(
                    value
                        .parse()
                        .map_err(|_| format!("\"version\" must be a number, got {value:?}"))?,
                );
                target = Target::None;
            }
            "denylist" => {
                if value.is_empty() {
                    denylist.get_or_insert_with(Vec::new);
                    target = Target::Deny;
                } else if value == "[]" {
                    denylist = Some(Vec::new());
                    target = Target::None;
                } else {
                    return Err("\"denylist\" must be a list of globs".into());
                }
            }
            "autoMergeAllowlist" | "auto_merge_allowlist" => {
                if value.is_empty() {
                    allowlist.get_or_insert_with(Vec::new);
                    target = Target::Allow;
                } else if value == "[]" {
                    allowlist = Some(Vec::new());
                    target = Target::None;
                } else {
                    return Err("\"autoMergeAllowlist\" must be a list of globs".into());
                }
            }
            "maxFiles" | "max_files" => {
                max_files = Some(
                    value
                        .parse()
                        .map_err(|_| format!("\"maxFiles\" must be a number, got {value:?}"))?,
                );
                target = Target::None;
            }
            _ => {
                target = if value.is_empty() {
                    Target::Other
                } else {
                    Target::None
                };
            }
        }
    }
    let Some(denylist) = denylist else {
        return Err("invalid gate config: expected { version: 1, denylist: [...] }: \"denylist\" is missing".into());
    };
    for g in &denylist {
        compile(g)?;
    }
    let allowlist = allowlist.unwrap_or_default();
    for g in &allowlist {
        compile(g)?;
    }
    Ok(GateConfig {
        version: version.unwrap_or(1),
        denylist,
        max_files,
        auto_merge_allowlist: allowlist,
    })
}

pub fn load(path: &Path) -> Result<GateConfig, String> {
    let text = std::fs::read_to_string(path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            format!("Gate config not found: {}", path.display())
        } else {
            format!("cannot read {}: {e}", path.display())
        }
    })?;
    parse(&text).map_err(|e| format!("{}: {e}", path.display()))
}

impl GateConfig {
    pub fn deny_matcher(&self) -> Matcher {
        Matcher::new(&self.denylist).unwrap_or_else(|_| Matcher::empty())
    }

    pub fn allow_matcher(&self) -> Matcher {
        Matcher::new(&self.auto_merge_allowlist).unwrap_or_else(|_| Matcher::empty())
    }

    /// The denylist glob `path` matches, if any.
    pub fn denied(&self, path: &str) -> Option<&str> {
        let m = self.deny_matcher();
        let g = m.first_match(path)?;
        self.denylist
            .iter()
            .find(|d| d.as_str() == g)
            .map(String::as_str)
    }

    /// The decision for `paths` under `action`.
    pub fn check(&self, action: Action, paths: &[String]) -> Decision {
        let deny = self.deny_matcher();
        let hits: Vec<String> = paths.iter().filter(|p| deny.is_match(p)).cloned().collect();
        if !hits.is_empty() {
            return Decision {
                allowed: false,
                trigger: Trigger::Denylist,
                reason: format!(
                    "{} path(s) match the denylist: {}. Escalating for human review.",
                    hits.len(),
                    hits.join(", ")
                ),
                matched_paths: hits,
            };
        }
        if let Some(max) = self.max_files
            && paths.len() as u64 > u64::from(max)
        {
            return Decision {
                allowed: false,
                trigger: Trigger::FileCount,
                reason: format!(
                    "{} changed file(s) exceeds the max-files threshold ({max}). Escalating for human review.",
                    paths.len()
                ),
                matched_paths: paths.to_vec(),
            };
        }
        if action == Action::AutoMerge {
            let allow = self.allow_matcher();
            let outside: Vec<String> = paths
                .iter()
                .filter(|p| !allow.is_match(p))
                .cloned()
                .collect();
            if !outside.is_empty() {
                return Decision {
                    allowed: false,
                    trigger: Trigger::NotAllowlisted,
                    reason: format!(
                        "{} path(s) are outside the auto-merge allowlist: {}. Escalating for human review.",
                        outside.len(),
                        outside.join(", ")
                    ),
                    matched_paths: outside,
                };
            }
        }
        Decision {
            allowed: true,
            trigger: Trigger::Ok,
            reason: "Within policy — cleared to proceed.".into(),
            matched_paths: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEMPLATE: &str = r#"# Machine-readable gate for loop runs.
version: 1

denylist:
  - "**/.env"          # dotenv files
  - "**/.env.*"
  - '**/secrets/**'
  - **/auth/**
  - "**/payments/**"
  - "**/migrations/**"

maxFiles: 10

autoMergeAllowlist:
  - "docs/**"
  - "**/*.md"
"#;

    fn owned(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_the_template_subset() {
        let g = parse(TEMPLATE).unwrap();
        assert_eq!(g.version, 1);
        assert_eq!(g.denylist.len(), 6);
        assert_eq!(g.denylist[3], "**/auth/**", "unquoted items");
        assert_eq!(g.max_files, Some(10));
        assert_eq!(g.auto_merge_allowlist, owned(&["docs/**", "**/*.md"]));
        // unknown keys and nested lists under them are ignored
        let g = parse("version: 1\nnotes:\n  - x\ndenylist: []\n").unwrap();
        assert!(g.denylist.is_empty() && g.max_files.is_none());
    }

    #[test]
    fn rejects_bad_shapes() {
        assert!(parse("version: 1\n").unwrap_err().contains("denylist"));
        assert!(
            parse("denylist: nope\n")
                .unwrap_err()
                .contains("list of globs")
        );
        assert!(
            parse("denylist:\n  - a\nmaxFiles: ten\n")
                .unwrap_err()
                .contains("maxFiles")
        );
        assert!(parse("- a\n").unwrap_err().contains("outside a list"));
        let missing = load(Path::new("/nowhere/gate.yaml")).unwrap_err();
        assert!(missing.starts_with("Gate config not found:"), "{missing}");
    }

    #[test]
    fn default_config_has_twelve_globs() {
        let d = default_config();
        assert_eq!(d.denylist.len(), 12);
        assert_eq!(d.max_files, Some(10));
        Matcher::new(&d.denylist).unwrap();
    }

    #[test]
    fn globs_span_separators_and_dotfiles() {
        let d = default_config();
        for hit in [
            ".env",
            "a/.env",
            "./a/.env",
            "a/b/.env.local",
            "secrets/a",
            "x/secrets/prod.json",
            ".hidden/secrets/x",
            "src/auth/login.rs",
            "db/migrations/001.sql",
            "certs/server.pem",
            "home/.ssh/id_rsa.pub",
            "config/credentials.json",
            ".github/workflows/ci.yml",
            "infra/main.tf",
        ] {
            assert!(d.denied(hit).is_some(), "{hit} must be denied");
        }
        assert_eq!(d.denied("a/.env"), Some("**/.env"));
        assert_eq!(d.denied("x/secrets/prod.json"), Some("**/secrets/**"));
        for miss in [
            "docs/README.md",
            "src/main.rs",
            ".secrets/prod.json", // `.secrets` is not `secrets`
            "environment.md",
            "src/author.rs",
        ] {
            assert!(d.denied(miss).is_none(), "{miss} must stay allowed");
        }
    }

    #[test]
    fn decisions_follow_the_severity_order() {
        let d = default_config();
        let ok = d.check(Action::Commit, &owned(&["docs/README.md", "src/lib.rs"]));
        assert!(ok.allowed && ok.trigger == Trigger::Ok);
        let deny = d.check(Action::Commit, &owned(&["src/lib.rs", "prod/.env"]));
        assert!(!deny.allowed && deny.trigger == Trigger::Denylist);
        assert_eq!(deny.matched_paths, owned(&["prod/.env"]));
        assert!(deny.reason.contains("1 path(s) match the denylist"));
        let many: Vec<String> = (0..11).map(|i| format!("docs/drill-{i}.md")).collect();
        let count = d.check(Action::Merge, &many);
        assert_eq!(count.trigger, Trigger::FileCount);
        assert_eq!(count.matched_paths.len(), 11);
        // the denylist wins over the count
        let mut both = many.clone();
        both.push("a/.env".into());
        assert_eq!(d.check(Action::Merge, &both).trigger, Trigger::Denylist);
        // auto-merge consults the allowlist; merge does not
        let bin = owned(&["src/drill-not-allowlisted.bin"]);
        assert!(d.check(Action::Merge, &bin).allowed);
        let am = d.check(Action::AutoMerge, &bin);
        assert_eq!(am.trigger, Trigger::NotAllowlisted);
        assert!(
            d.check(Action::AutoMerge, &owned(&["docs/a.md", "x/y.md"]))
                .allowed
        );
        // no allowlist blocks every auto-merge
        let no_allow = GateConfig {
            auto_merge_allowlist: vec![],
            ..default_config()
        };
        assert!(
            !no_allow
                .check(Action::AutoMerge, &owned(&["docs/a.md"]))
                .allowed
        );
    }

    #[test]
    fn relative_paths() {
        let root = Path::new("/ws");
        assert_eq!(relative_to(root, "/ws/src/a.rs"), "src/a.rs");
        assert_eq!(relative_to(root, "./docs/x.md"), "docs/x.md");
        assert_eq!(relative_to(root, "/elsewhere/.env"), "/elsewhere/.env");
        assert_eq!(Action::parse("auto-merge"), Some(Action::AutoMerge));
        assert_eq!(Trigger::FileCount.as_str(), "file-count");
    }
}
