//! Reading the memory: ai-memory's search and pages, for the Memory
//! screen. Both run the ai-memory binary in the project's folder, since
//! ai-memory names the project from the folder it is asked in (its
//! nearest `.ai-memory.toml`, else the repository's remote path, else the
//! folder's name).
//!
//! As probed on ai-memory 2.6.7:
//!
//! - `ai-memory search <query> --json -n <limit>` prints
//!   `[{"path","title","snippet","rank"}]`, the snippet's matches wrapped
//!   in `<mark>…</mark>`.
//! - `ai-memory read-page --path <wiki path> --json` prints
//!   `{"path","workspace","project","title","body","frontmatter"}`.
//!
//! The wiki on disk is kept in folders named by ids, so a page is read
//! through `read-page` and written to a file the Markdown viewer opens.

use serde::Deserialize;
use std::path::{Path, PathBuf};

/// One search result.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Hit {
    pub path: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub snippet: String,
}

impl Hit {
    /// The snippet on one line, its `<mark>`s gone.
    pub fn snippet_line(&self) -> String {
        self.snippet
            .replace("<mark>", "")
            .replace("</mark>", "")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    }
}

#[derive(Debug, Deserialize)]
struct Page {
    path: String,
    #[serde(default)]
    project: Option<String>,
    #[serde(default)]
    body: String,
    #[serde(default)]
    frontmatter: Option<serde_json::Value>,
}

fn run(bin: &Path, dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = std::process::Command::new(bin)
        .args(args)
        .current_dir(dir)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("{}: {e}", bin.display()))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let last = err
            .lines()
            .rev()
            .find_map(super::install::tidy)
            .unwrap_or_else(|| out.status.to_string());
        return Err(last.trim().to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The project's pages matching `query`, best first.
pub fn search(bin: &Path, dir: &Path, query: &str, limit: usize) -> Result<Vec<Hit>, String> {
    let limit = limit.to_string();
    let text = run(bin, dir, &["search", query, "--json", "-n", &limit])?;
    parse_hits(&text)
}

pub fn parse_hits(text: &str) -> Result<Vec<Hit>, String> {
    serde_json::from_str(text.trim()).map_err(|e| format!("ai-memory search: {e}"))
}

/// Writes the page at `path` to a Markdown file under `out_dir` (its
/// frontmatter as YAML front matter) and returns the file.
pub fn read_page(bin: &Path, dir: &Path, path: &str, out_dir: &Path) -> Result<PathBuf, String> {
    let text = run(bin, dir, &["read-page", "--path", path, "--json"])?;
    let (name, markdown) = render_page(&text)?;
    std::fs::create_dir_all(out_dir).map_err(|e| format!("{}: {e}", out_dir.display()))?;
    let file = out_dir.join(name);
    std::fs::write(&file, markdown).map_err(|e| format!("{}: {e}", file.display()))?;
    Ok(file)
}

/// A page's file name and its Markdown, from `read-page --json`.
pub fn render_page(json: &str) -> Result<(String, String), String> {
    let page: Page =
        serde_json::from_str(json.trim()).map_err(|e| format!("ai-memory read-page: {e}"))?;
    let mut md = String::new();
    if let Some(serde_json::Value::Object(fm)) = &page.frontmatter
        && !fm.is_empty()
    {
        md.push_str("---\n");
        md.push_str(&format!("memory: {}\n", page.path));
        if let Some(p) = &page.project {
            md.push_str(&format!("project: {p}\n"));
        }
        for (k, v) in fm {
            let v = match v {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            md.push_str(&format!("{k}: {v}\n"));
        }
        md.push_str("---\n\n");
    }
    md.push_str(&page.body);
    if !md.ends_with('\n') {
        md.push('\n');
    }
    let stem: String = page
        .path
        .trim_end_matches(".md")
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    Ok((
        format!("{}-{stem}.md", page.project.as_deref().unwrap_or("memory")),
        md,
    ))
}

/// The opening message of a session asked to answer from the memory.
pub fn ask_prompt(question: &str) -> String {
    format!(
        "Answer from this project's ai-memory: search it with the memory_query tool (and read the pages it finds) before answering, and say which pages the answer comes from. If the memory has nothing on it, say so. Question: {}",
        question.trim()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_results_parse_and_lose_their_marks() {
        let hits = parse_hits(
            r##"[{"path":"notes/deploy.md","title":"Deploy process","snippet":"# <mark>Deploy</mark> process\nWe <mark>deploy</mark> with fly.io","rank":-2.1e-6}]"##,
        )
        .unwrap();
        assert_eq!(hits[0].path, "notes/deploy.md");
        assert_eq!(hits[0].title.as_deref(), Some("Deploy process"));
        assert_eq!(
            hits[0].snippet_line(),
            "# Deploy process We deploy with fly.io"
        );
        assert!(parse_hits("[]").unwrap().is_empty());
        assert!(parse_hits("oops").is_err());
    }

    #[test]
    fn a_page_becomes_markdown_with_its_frontmatter() {
        let (name, md) = render_page(
            r##"{"path":"notes/deploy.md","workspace":"default","project":"proj","title":null,"body":"# Deploy process\nWe deploy.","frontmatter":{"kind":"rule","tier":"semantic"}}"##,
        )
        .unwrap();
        assert_eq!(name, "proj-notes_deploy.md");
        assert!(md.starts_with("---\nmemory: notes/deploy.md\nproject: proj\nkind: rule\ntier: semantic\n---\n\n# Deploy process"));
        assert!(md.ends_with("We deploy.\n"));
    }

    #[test]
    fn the_question_asks_for_the_memory_first() {
        let p = ask_prompt("  how do we deploy? ");
        assert!(p.contains("memory_query"));
        assert!(p.ends_with("Question: how do we deploy?"));
    }
}
