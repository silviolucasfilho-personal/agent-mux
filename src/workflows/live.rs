//! Pure display decoding of captured workflow stdout. JSON events may be
//! split across PTY reads: incomplete JSON is ignored until the next snapshot.
use crate::harness::Harness;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiveKind {
    Text,
    Tool,
    Outcome,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveLine {
    pub kind: LiveKind,
    pub text: String,
}

/// Decode a full stdout snapshot. Text lines are bounded to 240 characters;
/// unknown events and incomplete JSON records produce no display noise.
pub fn lines(harness: Harness, stdout: &[u8]) -> Vec<LiveLine> {
    let cleaned = super::harness::clean_terminal(stdout);
    let mut out = Vec::new();
    let mut banner = false;
    let mut separators = 0;
    let mut codex_kind = LiveKind::Text;
    for line in cleaned.lines() {
        let line = line.trim();
        if harness == Harness::Codex {
            if line.starts_with("Reading additional input from stdin") {
                continue;
            }
            if line.starts_with("OpenAI Codex ") {
                banner = true;
                continue;
            }
            if banner {
                if line.starts_with("--------") {
                    separators += 1;
                }
                if separators == 2 {
                    banner = false;
                }
                continue;
            }
            if line.starts_with("hook:") || line.starts_with("tokens used") {
                continue;
            }
            match line {
                "exec" => {
                    codex_kind = LiveKind::Tool;
                    continue;
                }
                "codex" | "thinking" | "user" => {
                    codex_kind = LiveKind::Text;
                    continue;
                }
                _ => {}
            }
            let kind = if line.contains("exited ") || line.contains("succeeded in ") {
                if codex_kind == LiveKind::Tool {
                    let command = line
                        .split(" succeeded in ")
                        .next()
                        .unwrap_or(line)
                        .split(" exited ")
                        .next()
                        .unwrap_or(line);
                    emit(&mut out, LiveKind::Tool, &format!("▸ {command}"));
                }
                codex_kind = LiveKind::Outcome;
                LiveKind::Outcome
            } else if line.starts_with("ERROR") || line.starts_with("error:") {
                LiveKind::Error
            } else {
                codex_kind
            };
            if kind == LiveKind::Tool {
                emit(&mut out, kind, &format!("▸ {line}"));
            } else {
                emit(&mut out, kind, line);
            }
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        match harness {
            Harness::Claude => match string(&v, "type") {
                "assistant" | "user" => {
                    if let Some(parts) = v["message"]["content"].as_array() {
                        for part in parts {
                            match string(part, "type") {
                                "text" => emit(&mut out, LiveKind::Text, string(part, "text")),
                                "tool_use" => tool(&mut out, string(part, "name"), &part["input"]),
                                "tool_result" => emit(
                                    &mut out,
                                    if part["is_error"].as_bool() == Some(true) {
                                        LiveKind::Error
                                    } else {
                                        LiveKind::Outcome
                                    },
                                    &display(&part["content"]),
                                ),
                                _ => {}
                            }
                        }
                    }
                }
                "result" => {
                    if v["is_error"].as_bool() == Some(true) {
                        emit(&mut out, LiveKind::Error, &display(&v["errors"]));
                        emit(&mut out, LiveKind::Error, string(&v, "result"));
                    } else {
                        final_text(&mut out, string(&v, "result"));
                    }
                }
                "error" => emit(&mut out, LiveKind::Error, &display(&v["error"])),
                _ => {}
            },
            Harness::Antigravity => match string(&v, "event") {
                "step_update" => {
                    let step = &v["step_update"];
                    emit(&mut out, LiveKind::Text, string(step, "text_delta"));
                    if string(step, "step_type") == "tool" {
                        let info = &step["tool_info"];
                        match string(step, "state") {
                            "ACTIVE" => {
                                tool(&mut out, string(step, "tool_name"), &info["parameters"])
                            }
                            "DONE" => emit(&mut out, LiveKind::Outcome, &display(&info["output"])),
                            "ERROR" => emit(&mut out, LiveKind::Error, &display(&info["error"])),
                            _ => {}
                        }
                    }
                }
                "result" => {
                    let result = &v["result"];
                    if !matches!(string(result, "status"), "SUCCESS" | "") {
                        emit(&mut out, LiveKind::Error, &display(result));
                    } else {
                        final_text(&mut out, string(result, "response"));
                    }
                }
                "error" => emit(&mut out, LiveKind::Error, &display(&v["error"])),
                _ => {}
            },
            Harness::OpenCode => match string(&v, "type") {
                "text" => emit(&mut out, LiveKind::Text, string(&v["part"], "text")),
                "tool_use" => {
                    let part = &v["part"];
                    let state = &part["state"];
                    match string(state, "status") {
                        "completed" => {
                            emit(&mut out, LiveKind::Outcome, &display(&state["output"]))
                        }
                        "error" => emit(&mut out, LiveKind::Error, &display(&state["error"])),
                        "pending" | "running" | "" => {
                            tool(&mut out, string(part, "tool"), &state["input"])
                        }
                        _ => {}
                    }
                }
                "error" => emit(&mut out, LiveKind::Error, &display(&v["error"])),
                _ => {}
            },
            Harness::Codex | Harness::Grok | Harness::Pi => {}
        }
    }
    out
}

fn string<'a>(value: &'a serde_json::Value, key: &str) -> &'a str {
    value.get(key).and_then(|v| v.as_str()).unwrap_or("")
}

fn display(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Null => String::new(),
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(parts) => parts
            .iter()
            .map(|p| p.as_str().unwrap_or_else(|| string(p, "text")))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => value.to_string(),
    }
}

fn emit(out: &mut Vec<LiveLine>, kind: LiveKind, text: &str) {
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let mut text: String = line.chars().filter(|c| !c.is_control()).take(240).collect();
        if line.chars().count() > 240 {
            text.push('…');
        }
        out.push(LiveLine { kind, text });
    }
}

fn tool(out: &mut Vec<LiveLine>, name: &str, args: &serde_json::Value) {
    emit(out, LiveKind::Tool, &format!("▸ {name} {}", display(args)));
}

fn final_text(out: &mut Vec<LiveLine>, text: &str) {
    // A final envelope repeats text already emitted as assistant messages.
    if !out.iter().any(|line| line.kind == LiveKind::Text) {
        emit(out, LiveKind::Text, text);
    }
}

#[cfg(test)]
pub(crate) mod fixtures {
    pub const CODEX: &str = r###"Reading additional input from stdin...
OpenAI Codex v0.167.0
--------
workdir: /private/var/folders/tl/0nz3g9w50pz78dv7mrcwc7bc0000gn/T/flow-stream-probe-_zlyjrxh
model: gpt-6.1-sol
provider: openai
approval: never
sandbox: read-only
reasoning effort: low
reasoning summaries: none
session id: 01a1d72d-69b2-7183-a61b-40a79839cd8f
--------
user
Reply exactly probe done. Do not use tools.
hook: SessionStart
hook: SessionStart Completed
hook: UserPromptSubmit
hook: UserPromptSubmit Completed
codex
probe done
hook: Stop
hook: Stop Completed
tokens used
6,291
probe done
"###;
    // Captured 2026-11-13 in a temporary folder. Claude's volatile message
    // metadata and thinking signatures omitted; event payloads preserved.
    pub const CLAUDE: &str = r###"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"toolu_01G2aSKfJxmjd96HtS2zvCVF","name":"Bash","input":{"command":"printf stream-probe","description":"Print the stream-probe text"},"caller":{"type":"direct"}}]}}
{"type":"user","message":{"content":[{"tool_use_id":"toolu_01G2aSKfJxmjd96HtS2zvCVF","type":"tool_result","content":"stream-probe","is_error":false}]}}
{"type":"assistant","message":{"content":[{"type":"text","text":"probe done"}]}}
{"type":"result","result":"probe done","usage":{"input_tokens":4,"cache_creation_input_tokens":17267,"cache_read_input_tokens":39846,"output_tokens":156,"output_tokens_details":{"thinking_tokens":68},"server_tool_use":{"web_search_requests":0,"web_fetch_requests":0},"service_tier":"standard","cache_creation":{"ephemeral_1h_input_tokens":17267,"ephemeral_5m_input_tokens":0},"inference_geo":"global","iterations":[{"input_tokens":2,"output_tokens":5,"cache_read_input_tokens":28476,"cache_creation_input_tokens":161,"cache_creation":{"ephemeral_5m_input_tokens":0,"ephemeral_1h_input_tokens":161},"type":"message"}],"speed":"standard","fallback_credit":null},"total_cost_usd":0.2905132,"is_error":false}
"###;
    pub const AGY: &str = r###"{"event":"step_update","step_update":{"conversation_id":"3b25ce7b-4d8b-4bda-8e3b-4cc85b8f10fa","step_index":0,"state":"DONE","step_type":"user_input"}}
{"event":"step_update","step_update":{"conversation_id":"3b25ce7b-4d8b-4bda-8e3b-4cc85b8f10fa","step_index":1,"state":"DONE","step_type":"unknown","duration_seconds":0.00043}}
{"event":"step_update","step_update":{"conversation_id":"3b25ce7b-4d8b-4bda-8e3b-4cc85b8f10fa","step_index":2,"state":"DONE","step_type":"agent_response","text_delta":"probe done\n","duration_seconds":5.166668,"usage":{"input_tokens":12634,"output_tokens":83,"thinking_tokens":77,"cache_read_tokens":0,"total_tokens":12717}}}
{"event":"result","result":{"conversation_id":"3b25ce7b-4d8b-4bda-8e3b-4cc85b8f10fa","status":"SUCCESS","response":"probe done\n","duration_seconds":5.898309,"num_turns":1,"usage":{"input_tokens":12634,"output_tokens":83,"thinking_tokens":77,"cache_read_tokens":0,"total_tokens":12717}}}
"###;
    pub const AGY_DENIED: &str = r###"{"event":"step_update","step_update":{"conversation_id":"0d34a62a-d736-41b9-9865-57e7d938f0e7","step_index":0,"state":"DONE","step_type":"user_input"}}
{"event":"step_update","step_update":{"conversation_id":"0d34a62a-d736-41b9-9865-57e7d938f0e7","step_index":1,"state":"DONE","step_type":"unknown","duration_seconds":0.000246}}
{"event":"step_update","step_update":{"conversation_id":"0d34a62a-d736-41b9-9865-57e7d938f0e7","step_index":2,"state":"DONE","step_type":"agent_response","duration_seconds":10.918656,"usage":{"input_tokens":13327,"output_tokens":255,"thinking_tokens":202,"cache_read_tokens":0,"total_tokens":13582}}}
{"event":"step_update","step_update":{"conversation_id":"0d34a62a-d736-41b9-9865-57e7d938f0e7","step_index":3,"state":"ACTIVE","step_type":"tool","tool_name":"exit_plan_mode","tool_info":{"name":"exit_plan_mode","parameters":{"plan":"1. Run `printf stream-probe` using the `run_command` tool.\n2. Reply with the exact text `probe done`."}}}}
{"event":"step_update","step_update":{"conversation_id":"0d34a62a-d736-41b9-9865-57e7d938f0e7","step_index":3,"state":"DONE","step_type":"tool","tool_name":"exit_plan_mode","duration_seconds":1.650394,"tool_info":{"name":"exit_plan_mode","parameters":{"plan":"1. Run `printf stream-probe` using the `run_command` tool.\n2. Reply with the exact text `probe done`."},"output":"Created At: 2026-11-13T20:07:27-03:00\nCompleted At: 2026-11-13T20:07:29-03:00\nPlan mode is off."}}}
{"event":"step_update","step_update":{"conversation_id":"0d34a62a-d736-41b9-9865-57e7d938f0e7","step_index":4,"state":"DONE","step_type":"system_message"}}
{"event":"step_update","step_update":{"conversation_id":"0d34a62a-d736-41b9-9865-57e7d938f0e7","step_index":5,"state":"DONE","step_type":"agent_response","duration_seconds":2.054618,"usage":{"input_tokens":13153,"output_tokens":62,"thinking_tokens":16,"cache_read_tokens":0,"total_tokens":13215}}}
{"event":"step_update","step_update":{"conversation_id":"0d34a62a-d736-41b9-9865-57e7d938f0e7","step_index":6,"state":"ACTIVE","step_type":"tool","tool_name":"run_command","tool_info":{"name":"run_command","parameters":{"CommandLine":"printf stream-probe"}}}}
{"event":"step_update","step_update":{"conversation_id":"0d34a62a-d736-41b9-9865-57e7d938f0e7","step_index":6,"state":"ERROR","step_type":"tool","tool_name":"run_command","duration_seconds":1.9060350000000001,"tool_info":{"name":"run_command","parameters":{"CommandLine":"printf stream-probe"},"error":{"type":"TOOL_ERROR","message":"permission check failed for unsandboxed \"printf stream-probe\": user denied permission to run command:\nprintf stream-probe\nDo not attempt to circumvent this denial by rephrasing the command, using alternative tools/scripts (e.g. python, sh, curl), or accessing the same target resource. Proceed without performing this action."}}}}
{"event":"result","result":{"conversation_id":"0d34a62a-d736-41b9-9865-57e7d938f0e7","status":"SUCCESS","response":"","duration_seconds":16.754072,"num_turns":1,"usage":{"input_tokens":26480,"output_tokens":317,"thinking_tokens":218,"cache_read_tokens":0,"total_tokens":26797},"denied_actions":[{"action":"command","display_name":"Bash"}]}}
"###;
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn captured_codex_plain_lines() {
        let decoded = lines(Harness::Codex, fixtures::CODEX.as_bytes());
        assert!(decoded.iter().any(|l| l.text == "probe done"));
        assert!(!decoded.iter().any(|l| l.text.contains("hook:")
            || l.text.contains("workdir:")
            || l.text.contains("OpenAI Codex")));
    }
    #[test]
    fn captured_claude_and_agy_events() {
        let c = lines(Harness::Claude, fixtures::CLAUDE.as_bytes());
        assert!(
            c.iter()
                .any(|l| l.kind == LiveKind::Tool && l.text.contains("printf stream-probe"))
        );
        assert!(
            c.iter()
                .any(|l| l.kind == LiveKind::Outcome && l.text == "stream-probe")
        );
        assert_eq!(c.iter().filter(|l| l.text == "probe done").count(), 1);
        let a = lines(Harness::Antigravity, fixtures::AGY.as_bytes());
        assert_eq!(a.iter().filter(|l| l.text == "probe done").count(), 1);
        let denied = lines(Harness::Antigravity, fixtures::AGY_DENIED.as_bytes());
        assert!(
            denied
                .iter()
                .any(|l| l.kind == LiveKind::Tool && l.text.contains("run_command"))
        );
        assert!(
            denied
                .iter()
                .any(|l| l.kind == LiveKind::Error && l.text.contains("permission check failed"))
        );
    }
    #[test]
    fn partial_capture_preserves_complete_events() {
        let complete = br#"{"type":"text","part":{"text":"ready"}}
"#;
        let mut partial = complete.to_vec();
        partial.extend_from_slice(b"{\"type\":\"text\",\"part\":{\"text\":\"later");
        assert_eq!(
            lines(Harness::OpenCode, &partial),
            lines(Harness::OpenCode, complete)
        );
        partial.extend_from_slice(b"\"}}\n");
        assert_eq!(
            lines(Harness::OpenCode, &partial).last().unwrap().text,
            "later"
        );
        let command = lines(Harness::Codex, b"exec\nprintf hello");
        assert_eq!(command[0].text, "▸ printf hello");
    }

    #[test]
    fn partial_json_and_unknown_events_are_ignored() {
        for h in [Harness::Claude, Harness::Antigravity, Harness::OpenCode] {
            assert!(lines(h, b"{\"unknown\":true}\n{\"type\":\"text\"").is_empty());
        }
    }
    #[test]
    fn codex_plain_output_and_pty_cleanup() {
        let out = b"OpenAI Codex v0.167.0\r\n--------\nworkdir: /tmp\nmodel: gpt-5\n--------\nuser\nprompt\nhook: startup\nexec\nprintf hi succeeded in 10ms:\nhi\n\x1b[32mcodex\x1b[0m\nanswer";
        let decoded = lines(Harness::Codex, out);
        assert!(!decoded.iter().any(|l| l.text.contains("workdir")
            || l.text.contains("hook:")
            || l.text.contains("OpenAI")));
        assert!(decoded.iter().any(|l| l.text == "answer"));
        assert!(decoded.iter().any(|l| l.kind == LiveKind::Tool));
    }
    #[test]
    fn opencode_events() {
        let out = br#"{"type":"step_start","part":{"type":"step-start"}}
{"type":"tool_use","part":{"tool":"bash","state":{"status":"running","input":{"command":"pwd"}}}}
{"type":"tool_use","part":{"tool":"bash","state":{"status":"completed","output":"/tmp"}}}
{"type":"text","part":{"text":"done"}}
{"type":"error","error":{"message":"failed"}}
"#;
        let decoded = lines(Harness::OpenCode, out);
        assert_eq!(
            decoded.iter().map(|l| l.kind).collect::<Vec<_>>(),
            vec![
                LiveKind::Tool,
                LiveKind::Outcome,
                LiveKind::Text,
                LiveKind::Error
            ]
        );
        assert_eq!(decoded[2].text, "done");
    }
}
