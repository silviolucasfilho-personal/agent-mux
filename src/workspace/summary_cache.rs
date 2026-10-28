//! On-disk cache (`~/.agent-mux/workspaces/<sha256>.json`) and prompt builder
//! for optional LLM-enriched folder summaries.

use super::{ProjectSummary, SummarySource, TechStack};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedSummary {
    pub path: String,
    pub generated_at: String,
    pub generated_unix_s: u64,
    pub harness: String,
    pub purpose: String,
    #[serde(default)]
    pub architecture: Option<String>,
    #[serde(default)]
    pub entry_points: Vec<String>,
}

/// Directory where workspace LLM summaries are stored.
pub fn cache_dir(library_root: &Path) -> PathBuf {
    library_root.join("workspaces")
}

/// Deterministic filename for a folder path.
pub fn cache_file(library_root: &Path, folder: &Path) -> PathBuf {
    let canonical = folder
        .canonicalize()
        .unwrap_or_else(|_| folder.to_path_buf());
    let mut hasher = Sha256::new();
    hasher.update(canonical.to_string_lossy().as_bytes());
    let digest = hasher.finalize();
    let hex: String = digest.iter().take(12).map(|b| format!("{b:02x}")).collect();
    cache_dir(library_root).join(format!("{hex}.json"))
}

/// Newest modification time (unix seconds) among key project files in `folder`.
pub fn newest_marker_mtime(folder: &Path) -> u64 {
    const MARKERS: &[&str] = &[
        "Cargo.toml",
        "package.json",
        "pyproject.toml",
        "go.mod",
        "README.md",
        "AGENTS.md",
        "CLAUDE.md",
    ];
    MARKERS
        .iter()
        .filter_map(|name| fs::metadata(folder.join(name)).ok())
        .filter_map(|m| m.modified().ok())
        .filter_map(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .max()
        .unwrap_or(0)
}

/// Loads a cached LLM summary for `folder` if present.
pub fn load(library_root: &Path, folder: &Path) -> Option<ProjectSummary> {
    let path = cache_file(library_root, folder);
    let raw = fs::read_to_string(path).ok()?;
    let cached: CachedSummary = serde_json::from_str(&raw).ok()?;
    let stale = newest_marker_mtime(folder) > cached.generated_unix_s;
    Some(ProjectSummary {
        text: cached.purpose,
        source: SummarySource::CachedLlm {
            harness: cached.harness,
            generated_at: cached.generated_at,
            stale,
        },
        architecture: cached.architecture,
        entry_points: cached.entry_points,
    })
}

/// Writes a cached LLM summary for `folder`.
pub fn save(
    library_root: &Path,
    folder: &Path,
    summary: &CachedSummary,
) -> Result<PathBuf, String> {
    let file = cache_file(library_root, folder);
    if let Some(parent) = file.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    let json = serde_json::to_string_pretty(summary).map_err(|e| e.to_string())?;
    fs::write(&file, json).map_err(|e| format!("write {}: {e}", file.display()))?;
    Ok(file)
}

/// Builds a concise prompt for a read-only headless harness invocation to
/// summarize `folder`.
pub fn build_prompt(folder: &Path, stack: &TechStack) -> String {
    format!(
        "Inspect the project in `{}` (detected stack: {}) and reply with ONLY a JSON object \
         (no surrounding prose, or inside a ```json代码块) with these exact keys:\n\
         - \"purpose\": 1-2 sentences explaining what this folder/project does.\n\
         - \"architecture\": 1 sentence describing its main components or structure.\n\
         - \"entry_points\": array of up to 4 relative file paths that are the main entry points.\n\
         Do not modify any files.",
        folder.display(),
        stack.detail_line()
    )
}

/// Extracts the JSON object from an LLM response (raw JSON or fenced block).
pub fn parse_llm_response(
    folder: &Path,
    harness: &str,
    generated_at: &str,
    generated_unix_s: u64,
    output: &str,
) -> Result<CachedSummary, String> {
    #[derive(Deserialize)]
    struct RawReply {
        purpose: String,
        #[serde(default)]
        architecture: Option<String>,
        #[serde(default)]
        entry_points: Vec<String>,
    }

    let trimmed = output.trim();
    let candidate = extract_json_object(trimmed).unwrap_or(trimmed);
    if let Ok(reply) = serde_json::from_str::<RawReply>(candidate) {
        return Ok(CachedSummary {
            path: folder.display().to_string(),
            generated_at: generated_at.to_string(),
            generated_unix_s,
            harness: harness.to_string(),
            purpose: reply.purpose.trim().to_string(),
            architecture: reply
                .architecture
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty()),
            entry_points: reply.entry_points,
        });
    }

    let fallback = trimmed
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with("```"))
        .collect::<Vec<_>>()
        .join(" ");
    if fallback.is_empty() {
        return Err("summarizer returned empty output".into());
    }
    Ok(CachedSummary {
        path: folder.display().to_string(),
        generated_at: generated_at.to_string(),
        generated_unix_s,
        harness: harness.to_string(),
        purpose: fallback.chars().take(320).collect(),
        architecture: None,
        entry_points: Vec::new(),
    })
}

fn extract_json_object(s: &str) -> Option<&str> {
    let start = s.find('{')?;
    let end = s.rfind('}')?;
    (end > start).then_some(&s[start..=end])
}
