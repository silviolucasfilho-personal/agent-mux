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

/// Marks a bounded capture's missing middle; it is not a harness event.
pub(crate) const CAPTURE_GAP: &str = "\n[workflow output omitted]\n";

/// Decode the default session (for OpenCode, the first session id seen).
pub fn lines(harness: Harness, stdout: &[u8]) -> Vec<LiveLine> {
    lines_for(harness, stdout, None)
}

/// Decode a full stdout snapshot, optionally selecting an OpenCode session.
/// Long display lines are abbreviated after 240 characters. Partial JSON
/// records and capture gaps produce no display noise.
pub fn lines_for(harness: Harness, stdout: &[u8], session: Option<&str>) -> Vec<LiveLine> {
    let cleaned = super::harness::clean_terminal(stdout);
    if harness == Harness::Codex {
        return codex_lines(&cleaned);
    }
    let mut out = Vec::new();
    let mut selected = session.map(str::to_owned);
    let mut tools = std::collections::HashSet::new();
    for line in cleaned.lines() {
        let line = line.trim();
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if harness == Harness::OpenCode {
            let id = v.get("sessionID").and_then(|id| id.as_str());
            if selected.is_none() {
                selected = id.map(str::to_owned);
            }
            if selected.as_deref() != id {
                continue;
            }
        }
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
                    // 2.0.32 emitted tool_use only on completion in the shell
                    // probe. Show the invocation too, unless a prior event did.
                    let id = part
                        .get("id")
                        .or_else(|| part.get("partID"))
                        .and_then(|id| id.as_str())
                        .map(str::to_owned)
                        .unwrap_or_else(|| format!("{}:{}", string(part, "tool"), state["input"]));
                    if tools.insert(id) {
                        tool(&mut out, string(part, "tool"), &state["input"]);
                    }
                    match string(state, "status") {
                        "completed" => {
                            emit(&mut out, LiveKind::Outcome, &display(&state["output"]))
                        }
                        "error" => emit(&mut out, LiveKind::Error, &display(&state["error"])),
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

/// Codex's readable exec format has sections, not standalone status lines.
fn codex_lines(cleaned: &str) -> Vec<LiveLine> {
    #[derive(Clone, Copy, PartialEq)]
    enum Section {
        Text,
        User,
        Command,
        Output,
        Footer,
    }
    let mut section = Section::Text;
    let mut banner = false;
    let mut separators = 0;
    let mut out = Vec::new();
    for line in cleaned.lines().map(str::trim) {
        if line == CAPTURE_GAP.trim() {
            continue;
        }
        if line.starts_with("Reading additional input from stdin") || line.starts_with("hook:") {
            continue;
        }
        if line.starts_with("OpenAI Codex ") {
            banner = true;
            separators = 0;
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
        if section == Section::Footer {
            continue;
        }
        match line {
            "user" => {
                section = Section::User;
                continue;
            }
            "codex" | "thinking" => {
                section = Section::Text;
                continue;
            }
            "exec" => {
                section = Section::Command;
                continue;
            }
            "tokens used" => {
                section = Section::Footer;
                continue;
            }
            _ => {}
        }
        if section == Section::User {
            continue;
        }
        if matches!(section, Section::Command | Section::Output)
            && (line.starts_with("succeeded in ") || line.starts_with("exited "))
        {
            section = Section::Output;
            emit(&mut out, LiveKind::Outcome, line);
        } else if line.starts_with("ERROR") || line.starts_with("error:") {
            emit(&mut out, LiveKind::Error, line);
        } else {
            match section {
                Section::Command => emit(&mut out, LiveKind::Tool, &format!("▸ {line}")),
                Section::Output => emit(&mut out, LiveKind::Outcome, line),
                Section::Text => emit(&mut out, LiveKind::Text, line),
                Section::User | Section::Footer => {}
            }
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
    // Real shell runs, 2026-11-13, Codex 0.167.0 and OpenCode 2.0.32.
    pub const CODEX_TOOL: &str = r####"Reading additional input from stdin...
OpenAI Codex v0.167.0
--------
workdir: /private/var/folders/tl/0nz3g9w50pz78dv7mrcwc7bc0000gn/T/flow-fix-probe-g8z6fbt4
model: gpt-6.1-sol
provider: openai
approval: never
sandbox: read-only
reasoning effort: low
reasoning summaries: none
session id: 01a1d74f-da45-7761-8227-8a8eb4389191
--------
user
Run printf stream-fix-probe in the shell, then reply exactly: The command succeeded in producing output.
hook: SessionStart
hook: SessionStart Completed
hook: UserPromptSubmit
hook: UserPromptSubmit Completed
codex
I’ll run the command now.

hook: PreToolUse
hook: PreToolUse Completed
exec
/bin/zsh -lc 'printf stream-fix-probe' in /private/var/folders/tl/0nz3g9w50pz78dv7mrcwc7bc0000gn/T/flow-fix-probe-g8z6fbt4
 succeeded in 0ms:
stream-fix-probe
hook: PostToolUse
hook: PostToolUse Completed
codex
The command succeeded in producing output.
hook: Stop
hook: Stop Completed
tokens used
6,344
The command succeeded in producing output.
"####;
    pub const OPENCODE: &str = r####"{"type":"step_start","timestamp":1794613708059,"sessionID":"ses_e28b000a2ffeX8Li6xYZ53eVQd","part":{"id":"prt_1d750051b001qoP6Jun1XyP0Wp","sessionID":"ses_e28b000a2ffeX8Li6xYZ53eVQd","messageID":"msg_1d74fffb0001qMcZapB3gEmkRN","type":"step-start","snapshot":"f498856f00a6f8351765b80a807e46f2e6f86d1d"}}
{"type":"tool_use","timestamp":1794613709105,"sessionID":"ses_e28b000a2ffeX8Li6xYZ53eVQd","part":{"partID":"prt_1d75008980011dwov50J1TH13X","sessionID":"ses_e28b000a2ffeX8Li6xYZ53eVQd","messageID":"msg_1d74fffb0001qMcZapB3gEmkRN","type":"tool","id":"call_598319d05a684ea6b89d3809","tool":"shell","state":{"status":"completed","input":{"command":"printf stream-fix-probe"},"output":"stream-fix-probe","title":"shell","metadata":{"metadata":{"status":"completed","truncated":false,"exit":0},"content":[{"type":"text","text":"stream-fix-probe"}],"providerCall":{"executed":false},"providerResult":{"executed":false},"rawInput":"{\"command\":\"printf stream-fix-probe\"}"},"time":{"start":1794613708952,"end":1794613709105}}}}
{"type":"step_finish","timestamp":1794613709131,"sessionID":"ses_e28b000a2ffeX8Li6xYZ53eVQd","part":{"id":"prt_1d750094b0010MzkUp9AWhiLu6","sessionID":"ses_e28b000a2ffeX8Li6xYZ53eVQd","messageID":"msg_1d74fffb0001qMcZapB3gEmkRN","type":"step-finish","reason":"tool-calls","snapshot":"f498856f00a6f8351765b80a807e46f2e6f86d1d","cost":0,"tokens":{"input":117,"output":43,"reasoning":137,"cache":{"read":32000,"write":0}}}}
{"type":"step_start","timestamp":1794613710658,"sessionID":"ses_e28b000a2ffeX8Li6xYZ53eVQd","part":{"id":"prt_1d7500f42001cpEgDRB2EtYG3r","sessionID":"ses_e28b000a2ffeX8Li6xYZ53eVQd","messageID":"msg_1d7500954001cNTUdFCE41z7lI","type":"step-start","snapshot":"f498856f00a6f8351765b80a807e46f2e6f86d1d"}}
{"type":"text","timestamp":1794613710682,"sessionID":"ses_e28b000a2ffeX8Li6xYZ53eVQd","part":{"id":"prt_1d7500f470010zLyxE1O99iM6g","sessionID":"ses_e28b000a2ffeX8Li6xYZ53eVQd","messageID":"msg_1d7500954001cNTUdFCE41z7lI","type":"text","text":"probe done.","time":{"start":1794613710663,"end":1794613710682}}}
"####;
    // Captured records replayed with the other session interleaved.
    pub const OPENCODE_MIXED: &str = r####"{"type":"step_start","timestamp":1794613708059,"sessionID":"ses_e28b000a2ffeX8Li6xYZ53eVQd","part":{"id":"prt_1d750051b001qoP6Jun1XyP0Wp","sessionID":"ses_e28b000a2ffeX8Li6xYZ53eVQd","messageID":"msg_1d74fffb0001qMcZapB3gEmkRN","type":"step-start","snapshot":"f498856f00a6f8351765b80a807e46f2e6f86d1d"}}
{"type":"text","timestamp":1794611474790,"sessionID":"ses_e28d2a8e6ffeyLar9G7hsraPxm","part":{"id":"prt_1d72dea04001bKahxfvEESROnh","sessionID":"ses_e28d2a8e6ffeyLar9G7hsraPxm","messageID":"msg_1d72dd3d9001nLhkyFCg5IBs8Z","type":"text","text":"Ran it. Results:\n\n**codex — exit 0**\n`codex-last.md` contains exactly:\n```\nprobe done\n```\nIts jsonl (571 B) shows the SessionStart/UserPromptSubmit/Stop hooks firing and `tokens used 6,291` — clean, streamed end to end.\n\n**opencode — exit 0, but the capture is not usable**\n`opencode.jsonl` (3.0 KB, 24 events) breaks down as:\n- 1 event for the probe's own session `ses_e28d27db…` — a lone `step_start`, no text, no `step_finish`\n- 23 events for `ses_e28d2a8e…`, which is *this* session (my `shell`/`read` calls, the `ls` of the probe dir, the failed read of a bad path)\n\nSo `/Users/sifilho/.opencode/bin/opencode run --format json` here is emitting the shared server's global event stream rather than just the probe session's, and it never printed an assistant text event for its own session. No `probe done` text is recoverable from that file.\n\nNet: codex probe passed; opencode probe needs a different isolation (e.g. `--session`, a separate `OPENCODE_CONFIG_DIR`/server, or `--standalone`) before its `--format json` output can be trusted as a per-run stream.","time":{"start":1794611472900,"end":1794611474790}}}
{"type":"tool_use","timestamp":1794613709105,"sessionID":"ses_e28b000a2ffeX8Li6xYZ53eVQd","part":{"partID":"prt_1d75008980011dwov50J1TH13X","sessionID":"ses_e28b000a2ffeX8Li6xYZ53eVQd","messageID":"msg_1d74fffb0001qMcZapB3gEmkRN","type":"tool","id":"call_598319d05a684ea6b89d3809","tool":"shell","state":{"status":"completed","input":{"command":"printf stream-fix-probe"},"output":"stream-fix-probe","title":"shell","metadata":{"metadata":{"status":"completed","truncated":false,"exit":0},"content":[{"type":"text","text":"stream-fix-probe"}],"providerCall":{"executed":false},"providerResult":{"executed":false},"rawInput":"{\"command\":\"printf stream-fix-probe\"}"},"time":{"start":1794613708952,"end":1794613709105}}}}
{"type":"step_finish","timestamp":1794613709131,"sessionID":"ses_e28b000a2ffeX8Li6xYZ53eVQd","part":{"id":"prt_1d750094b0010MzkUp9AWhiLu6","sessionID":"ses_e28b000a2ffeX8Li6xYZ53eVQd","messageID":"msg_1d74fffb0001qMcZapB3gEmkRN","type":"step-finish","reason":"tool-calls","snapshot":"f498856f00a6f8351765b80a807e46f2e6f86d1d","cost":0,"tokens":{"input":117,"output":43,"reasoning":137,"cache":{"read":32000,"write":0}}}}
{"type":"step_start","timestamp":1794613710658,"sessionID":"ses_e28b000a2ffeX8Li6xYZ53eVQd","part":{"id":"prt_1d7500f42001cpEgDRB2EtYG3r","sessionID":"ses_e28b000a2ffeX8Li6xYZ53eVQd","messageID":"msg_1d7500954001cNTUdFCE41z7lI","type":"step-start","snapshot":"f498856f00a6f8351765b80a807e46f2e6f86d1d"}}
{"type":"text","timestamp":1794613710682,"sessionID":"ses_e28b000a2ffeX8Li6xYZ53eVQd","part":{"id":"prt_1d7500f470010zLyxE1O99iM6g","sessionID":"ses_e28b000a2ffeX8Li6xYZ53eVQd","messageID":"msg_1d7500954001cNTUdFCE41z7lI","type":"text","text":"probe done.","time":{"start":1794613710663,"end":1794613710682}}}
"####;
    pub const OPENCODE_TWO_SESSIONS: &str = r####"{"type":"step_start","timestamp":1794611448402,"sessionID":"ses_e28d27db9ffeuatDpz8XRPMY1o","part":{"id":"prt_1d72d8a52001HP3ZDOVUGLdEtD","sessionID":"ses_e28d27db9ffeuatDpz8XRPMY1o","messageID":"msg_1d72d827e001w1X2XuidQSl04g","type":"step-start","snapshot":"795b152f2839ee9889305dc048ffbd03d7a29fb0"}}
{"type":"text","timestamp":1794611474790,"sessionID":"ses_e28d2a8e6ffeyLar9G7hsraPxm","part":{"id":"prt_1d72dea04001bKahxfvEESROnh","sessionID":"ses_e28d2a8e6ffeyLar9G7hsraPxm","messageID":"msg_1d72dd3d9001nLhkyFCg5IBs8Z","type":"text","text":"Ran it. Results:\n\n**codex — exit 0**\n`codex-last.md` contains exactly:\n```\nprobe done\n```\nIts jsonl (571 B) shows the SessionStart/UserPromptSubmit/Stop hooks firing and `tokens used 6,291` — clean, streamed end to end.\n\n**opencode — exit 0, but the capture is not usable**\n`opencode.jsonl` (3.0 KB, 24 events) breaks down as:\n- 1 event for the probe's own session `ses_e28d27db…` — a lone `step_start`, no text, no `step_finish`\n- 23 events for `ses_e28d2a8e…`, which is *this* session (my `shell`/`read` calls, the `ls` of the probe dir, the failed read of a bad path)\n\nSo `/Users/sifilho/.opencode/bin/opencode run --format json` here is emitting the shared server's global event stream rather than just the probe session's, and it never printed an assistant text event for its own session. No `probe done` text is recoverable from that file.\n\nNet: codex probe passed; opencode probe needs a different isolation (e.g. `--session`, a separate `OPENCODE_CONFIG_DIR`/server, or `--standalone`) before its `--format json` output can be trusted as a per-run stream.","time":{"start":1794611472900,"end":1794611474790}}}
"####;
    pub const CODEX_COMMAND: &str = r####"▸ /bin/zsh -lc 'printf stream-fix-probe' in /private/var/folders/tl/0nz3g9w50pz78dv7mrcwc7bc0000gn/T/flow-fix-probe-g8z6fbt4"####;
    pub const OPENCODE_SESSION: &str = r####"ses_e28b000a2ffeX8Li6xYZ53eVQd"####;
    pub const OTHER_SESSION: &str = r####"ses_e28d2a8e6ffeyLar9G7hsraPxm"####;
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
        assert_eq!(
            decoded,
            vec![LiveLine {
                kind: LiveKind::Text,
                text: "probe done".into()
            }]
        );
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
        let complete = fixtures::OPENCODE.as_bytes();
        let event = fixtures::OPENCODE
            .lines()
            .find(|line| {
                serde_json::from_str::<serde_json::Value>(line)
                    .ok()
                    .and_then(|v| v.get("type").and_then(|t| t.as_str()).map(str::to_owned))
                    .as_deref()
                    == Some("text")
            })
            .unwrap();
        let mut partial = complete.to_vec();
        let split = event.len() / 2;
        partial.extend_from_slice(&event.as_bytes()[..split]);
        assert_eq!(
            lines(Harness::OpenCode, &partial),
            lines(Harness::OpenCode, complete)
        );
        partial.extend_from_slice(&event.as_bytes()[split..]);
        assert_eq!(
            lines(Harness::OpenCode, &partial).last().unwrap().text,
            "probe done."
        );
        let command_capture = fixtures::CODEX_TOOL
            .split("\n succeeded in ")
            .next()
            .unwrap();
        let command = lines(Harness::Codex, command_capture.as_bytes());
        assert_eq!(command.last().unwrap().text, fixtures::CODEX_COMMAND);
    }

    #[test]
    fn partial_json_and_unknown_events_are_ignored() {
        for h in [Harness::Claude, Harness::Antigravity, Harness::OpenCode] {
            assert!(lines(h, b"{\"unknown\":true}\n{\"type\":\"text\"").is_empty());
        }
    }
    #[test]
    fn captured_codex_tool_and_agent_sections() {
        let decoded = lines(Harness::Codex, fixtures::CODEX_TOOL.as_bytes());
        assert_eq!(
            decoded
                .iter()
                .map(|l| (l.kind, l.text.as_str()))
                .collect::<Vec<_>>(),
            vec![
                (LiveKind::Text, "I’ll run the command now."),
                (LiveKind::Tool, fixtures::CODEX_COMMAND),
                (LiveKind::Outcome, "succeeded in 0ms:"),
                (LiveKind::Outcome, "stream-fix-probe"),
                (LiveKind::Text, "The command succeeded in producing output."),
            ]
        );
        let gapped = fixtures::CODEX_TOOL
            .replace("\nuser\n", &format!("\nuser\n{CAPTURE_GAP}"))
            .replace("\ntokens used\n", &format!("\ntokens used\n{CAPTURE_GAP}"));
        assert_eq!(lines(Harness::Codex, gapped.as_bytes()), decoded);
        let pty = fixtures::CODEX_TOOL.replace("codex\n", "\x1b[32mcodex\x1b[0m\r\n");
        assert_eq!(lines(Harness::Codex, pty.as_bytes()), decoded);
    }
    #[test]
    fn captured_opencode_completion_and_session_filter() {
        let expected = vec![
            LiveLine {
                kind: LiveKind::Tool,
                text: "▸ shell {\"command\":\"printf stream-fix-probe\"}".into(),
            },
            LiveLine {
                kind: LiveKind::Outcome,
                text: "stream-fix-probe".into(),
            },
            LiveLine {
                kind: LiveKind::Text,
                text: "probe done.".into(),
            },
        ];
        assert_eq!(
            lines(Harness::OpenCode, fixtures::OPENCODE.as_bytes()),
            expected
        );
        assert_eq!(
            lines_for(
                Harness::OpenCode,
                fixtures::OPENCODE_MIXED.as_bytes(),
                Some(fixtures::OPENCODE_SESSION)
            ),
            expected
        );
        assert_eq!(
            lines(Harness::OpenCode, fixtures::OPENCODE_MIXED.as_bytes()),
            expected
        );
        // The first event belongs to the probe; all subsequent events belong
        // to a different real live session. Never display that other session.
        assert!(
            lines(
                Harness::OpenCode,
                fixtures::OPENCODE_TWO_SESSIONS.as_bytes()
            )
            .is_empty()
        );
        assert!(
            !lines_for(
                Harness::OpenCode,
                fixtures::OPENCODE_TWO_SESSIONS.as_bytes(),
                Some(fixtures::OTHER_SESSION)
            )
            .is_empty()
        );
    }
}
