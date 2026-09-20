//! Per-harness print-mode command lines and envelope parsing for workflow
//! sessions. Probed against the installed CLIs on 2026-09-17: Claude Code
//! 2.1.274 (`-p`, `--output-format json`, `--max-budget-usd`), Codex CLI
//! 0.154.0 (`exec`, `-o/--output-last-message FILE`, `--skip-git-repo-check`)
//! and Antigravity 1.2.4 (`-p` is `--print`; `--output-format json`,
//! `--print-timeout`, `--mode accept-edits`). Native schema flags exist on
//! all three and are deliberately not used: the fenced `workflow-result`
//! block keeps a result independent of the harness. agy `--sandbox` is not
//! passed: its terminal restrictions are unverified against step skills.

use crate::harness::Harness;
use std::path::Path;

/// The arguments a workflow session adds to the profile's command line,
/// inserted before the trailing prompt.
pub fn extra_args(
    harness: Harness,
    usd_cap: Option<f64>,
    timeout_s: u64,
    out_file: &Path,
) -> Vec<String> {
    let mut v = Vec::new();
    match harness {
        Harness::Claude => {
            v.push("--output-format".into());
            v.push("json".into());
            if let Some(cap) = usd_cap {
                v.push("--max-budget-usd".into());
                v.push(format!("{cap}"));
            }
        }
        Harness::Codex => {
            v.push("-o".into());
            v.push(out_file.to_string_lossy().into_owned());
            v.push("--skip-git-repo-check".into());
        }
        Harness::Antigravity => {
            v.push("--output-format".into());
            v.push("json".into());
            v.push("--print-timeout".into());
            v.push(format!("{timeout_s}s"));
            v.push("--mode".into());
            v.push("accept-edits".into());
        }
    }
    v
}

/// Reasoning effort for the harnesses that take one. Codex reads it from
/// its config (`-c model_reasoning_effort="high"`, accepted by codex
/// 0.154.0); Claude Code 2.1.277 and agy 1.2.6 have no such flag, so a
/// step that asks for one on them gets `None` and a run note.
pub fn effort_args(harness: Harness, effort: &str) -> Option<Vec<String>> {
    match harness {
        Harness::Codex => Some(vec![
            "-c".into(),
            format!("model_reasoning_effort=\"{}\"", effort.replace('"', "")),
        ]),
        Harness::Claude | Harness::Antigravity => None,
    }
}

/// Inserts `extra` before the trailing prompt of a composed command line
/// (`-p <prompt>` on Claude and Antigravity, the positional prompt on
/// Codex).
pub fn insert_before_prompt(args: &mut Vec<String>, harness: Harness, extra: Vec<String>) {
    let tail = match harness {
        Harness::Claude | Harness::Antigravity => 2,
        Harness::Codex => 1,
    };
    let at = args.len().saturating_sub(tail);
    for (i, a) in extra.into_iter().enumerate() {
        args.insert(at + i, a);
    }
}

/// What the envelope of a print-mode run says.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Envelope {
    pub text: Option<String>,
    pub tokens: Option<u64>,
    pub cost_usd: Option<f64>,
}

/// Strips carriage returns and ANSI CSI/OSC sequences from PTY output.
pub fn clean_terminal(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {}
            '\u{1b}' => match chars.peek() {
                Some('[') => {
                    chars.next();
                    for d in chars.by_ref() {
                        if ('@'..='~').contains(&d) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    chars.next();
                    let mut prev = '\0';
                    for d in chars.by_ref() {
                        if d == '\u{7}' || (prev == '\u{1b}' && d == '\\') {
                            break;
                        }
                        prev = d;
                    }
                }
                _ => {}
            },
            other => out.push(other),
        }
    }
    out
}

/// The last JSON object on its own line(s) of the cleaned output. Print
/// modes emit the envelope last; earlier lines may be the harness's own
/// progress text.
fn last_json_object(text: &str) -> Option<serde_json::Value> {
    let lines: Vec<&str> = text.lines().collect();
    // single-line envelopes first
    for line in lines.iter().rev() {
        let t = line.trim();
        if t.starts_with('{')
            && t.ends_with('}')
            && let Ok(v) = serde_json::from_str::<serde_json::Value>(t)
            && v.is_object()
        {
            return Some(v);
        }
    }
    // a pretty-printed envelope: from the last `{` line to the end
    if let Some(start) = lines.iter().rposition(|l| l.trim() == "{") {
        let candidate = lines[start..].join("\n");
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(candidate.trim())
            && v.is_object()
        {
            return Some(v);
        }
    }
    None
}

fn text_of(v: &serde_json::Value) -> Option<String> {
    for key in [
        "result", "response", "text", "output", "message", "content", "final",
    ] {
        match v.get(key) {
            Some(serde_json::Value::String(s)) if !s.trim().is_empty() => return Some(s.clone()),
            Some(serde_json::Value::Array(parts)) => {
                let joined: Vec<String> = parts
                    .iter()
                    .filter_map(|p| {
                        p.as_str()
                            .map(str::to_string)
                            .or_else(|| p.get("text").and_then(|t| t.as_str()).map(str::to_string))
                    })
                    .collect();
                if !joined.is_empty() {
                    return Some(joined.join("\n"));
                }
            }
            Some(serde_json::Value::Object(o)) => {
                if let Some(serde_json::Value::String(s)) = o.get("text") {
                    return Some(s.clone());
                }
            }
            _ => {}
        }
    }
    None
}

/// Reads the envelope of a finished session: the JSON on stdout (Claude
/// Code, Antigravity) or the last-message file (Codex).
pub fn envelope(harness: Harness, stdout: &[u8], out_file: &Path) -> Envelope {
    match harness {
        Harness::Codex => Envelope {
            text: std::fs::read_to_string(out_file)
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty()),
            ..Default::default()
        },
        Harness::Claude | Harness::Antigravity => {
            let cleaned = clean_terminal(stdout);
            let Some(v) = last_json_object(&cleaned) else {
                return Envelope::default();
            };
            let tokens = v
                .get("usage")
                .and_then(|u| {
                    let i = u.get("input_tokens").and_then(|x| x.as_u64()).unwrap_or(0);
                    let o = u.get("output_tokens").and_then(|x| x.as_u64()).unwrap_or(0);
                    (i + o > 0).then_some(i + o)
                })
                .or_else(|| v.get("total_tokens").and_then(|x| x.as_u64()));
            Envelope {
                text: text_of(&v),
                tokens,
                cost_usd: v
                    .get("total_cost_usd")
                    .or_else(|| v.get("cost_usd"))
                    .and_then(|c| c.as_f64()),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extra_args_per_harness_and_insertion() {
        let out = Path::new("/tmp/last.md");
        let mut claude = vec!["--model".into(), "m".into(), "-p".into(), "hi".into()];
        insert_before_prompt(
            &mut claude,
            Harness::Claude,
            extra_args(Harness::Claude, Some(1.5), 900, out),
        );
        assert_eq!(
            claude,
            vec![
                "--model",
                "m",
                "--output-format",
                "json",
                "--max-budget-usd",
                "1.5",
                "-p",
                "hi"
            ]
        );
        let mut codex = vec!["exec".into(), "--yolo".into(), "hi".into()];
        insert_before_prompt(
            &mut codex,
            Harness::Codex,
            extra_args(Harness::Codex, None, 900, out),
        );
        assert_eq!(
            codex,
            vec![
                "exec",
                "--yolo",
                "-o",
                "/tmp/last.md",
                "--skip-git-repo-check",
                "hi"
            ]
        );
        let mut agy = vec!["-p".into(), "hi".into()];
        insert_before_prompt(
            &mut agy,
            Harness::Antigravity,
            extra_args(Harness::Antigravity, None, 60, out),
        );
        assert_eq!(
            agy,
            vec![
                "--output-format",
                "json",
                "--print-timeout",
                "60s",
                "--mode",
                "accept-edits",
                "-p",
                "hi"
            ]
        );
    }

    #[test]
    fn envelopes_are_parsed_from_cleaned_output() {
        let stdout = b"\x1b[32mworking\x1b[0m\r\n{\"type\":\"result\",\"result\":\"done\\n```workflow-result\\n{}\\n```\",\"total_cost_usd\":0.02,\"usage\":{\"input_tokens\":10,\"output_tokens\":5}}\r\n";
        let e = envelope(Harness::Claude, stdout, Path::new("/nonexistent"));
        assert!(e.text.as_deref().unwrap().starts_with("done"));
        assert_eq!(e.tokens, Some(15));
        assert_eq!(e.cost_usd, Some(0.02));
        let pretty = b"{\n  \"response\": \"agy says hi\"\n}\n";
        assert_eq!(
            envelope(Harness::Antigravity, pretty, Path::new("/x"))
                .text
                .as_deref(),
            Some("agy says hi")
        );
        let parts = b"{\"content\":[{\"type\":\"text\",\"text\":\"a\"},{\"text\":\"b\"}]}\n";
        assert_eq!(
            envelope(Harness::Antigravity, parts, Path::new("/x"))
                .text
                .as_deref(),
            Some("a\nb")
        );
        assert_eq!(
            envelope(Harness::Claude, b"plain text only\n", Path::new("/x")),
            Envelope::default()
        );
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("last.md");
        std::fs::write(&f, " codex answer \n").unwrap();
        assert_eq!(
            envelope(Harness::Codex, b"", &f).text.as_deref(),
            Some("codex answer")
        );
        assert_eq!(clean_terminal(b"a\x1b]0;title\x07b\x1b[1;31mc"), "abc");
    }
}
