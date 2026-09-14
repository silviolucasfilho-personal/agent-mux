//! Text slicing, command extraction, and tool evidence normalization.

/// Character-safe string snippet with an ellipsis suffix if truncated.
pub fn snippet(text: &str, max_chars: usize) -> String {
    let mut iter = text.chars();
    let mut out: String = iter.by_ref().take(max_chars).collect();
    if iter.next().is_some() {
        out.push('…');
    }
    out
}

/// Normalizes shell commands extracted from tool inputs (`command`, `cmd`, `CommandLine`).
pub fn extract_command(tool_name: &str, input_raw: &str) -> Option<String> {
    let lower_tool = tool_name.to_lowercase();
    let is_shell_tool = lower_tool.contains("command")
        || lower_tool.contains("bash")
        || lower_tool.contains("exec")
        || lower_tool.contains("shell")
        || lower_tool.contains("terminal")
        || lower_tool == "cmd";

    if !is_shell_tool {
        return None;
    }

    if let Ok(val) = serde_json::from_str::<serde_json::Value>(input_raw) {
        if let Some(cmd) = val.get("command").and_then(|v| v.as_str()) {
            return Some(cmd.trim().to_string());
        }
        if let Some(cmd) = val.get("CommandLine").and_then(|v| v.as_str()) {
            return Some(cmd.trim().to_string());
        }
        if let Some(cmd) = val.get("cmd").and_then(|v| v.as_str()) {
            return Some(cmd.trim().to_string());
        }
    }

    let trimmed = input_raw.trim();
    if !trimmed.is_empty() && !trimmed.starts_with('{') {
        Some(trimmed.to_string())
    } else {
        None
    }
}

/// Normalizes relative file paths extracted from edit/write tool inputs.
/// Preserves directory components (e.g. `src/a/config.rs` vs `src/b/config.rs`).
pub fn extract_target_file(tool_name: &str, input_raw: &str) -> Option<String> {
    let lower_tool = tool_name.to_lowercase();
    let is_file_tool = lower_tool.contains("write")
        || lower_tool.contains("edit")
        || lower_tool.contains("file")
        || lower_tool.contains("patch");

    if !is_file_tool {
        return None;
    }

    if let Ok(val) = serde_json::from_str::<serde_json::Value>(input_raw) {
        for key in ["TargetFile", "file_path", "path", "target_file", "filename", "filepath"] {
            if let Some(p) = val.get(key).and_then(|v| v.as_str()) {
                let trimmed = p.trim();
                if !trimmed.is_empty() {
                    return Some(trimmed.to_string());
                }
            }
        }
    }

    None
}
