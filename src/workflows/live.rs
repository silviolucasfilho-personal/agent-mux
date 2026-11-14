//! Pure display decoding of captured workflow stdout. JSON events may be
//! split across PTY reads: incomplete JSON is ignored until the next snapshot.
//! Text probed 2026-11-13: agy 1.5.8 emits ACTIVE agent_response word
//! fragments in text_delta, so concatenate within a step before formatting.
//! Claude Code's configured stream-json --verbose emitted complete assistant
//! text blocks. A second probe with --include-partial-messages emitted
//! stream_event/content_block_delta records plus one complete assistant
//! block; the decoder ignores those separate partial records to avoid repeats.
//! That optional flag is not enabled by flow launches.
//! OpenCode 2.0.32 run --format json emitted a complete part.text in the
//! multi-paragraph probe, rather than word deltas.
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
    let mut pending_text = String::new();
    let mut text_step = None;
    for line in cleaned.lines() {
        let line = line.trim();
        if line == CAPTURE_GAP.trim() {
            flush_text(&mut out, &mut pending_text);
            text_step = None;
            continue;
        }
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
                    if string(step, "step_type") == "agent_response" {
                        let key = (
                            string(step, "conversation_id").to_owned(),
                            step["step_index"].as_u64(),
                        );
                        if text_step.as_ref() != Some(&key) {
                            flush_text(&mut out, &mut pending_text);
                            text_step = Some(key);
                        }
                        pending_text.push_str(string(step, "text_delta"));
                        if let Some(end) = pending_text.rfind('\n') {
                            emit(&mut out, LiveKind::Text, &pending_text[..=end]);
                            pending_text.drain(..=end);
                        }
                    } else {
                        flush_text(&mut out, &mut pending_text);
                        text_step = None;
                    }
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
                    flush_text(&mut out, &mut pending_text);
                    let result = &v["result"];
                    if !matches!(string(result, "status"), "SUCCESS" | "") {
                        emit(&mut out, LiveKind::Error, &display(result));
                    } else {
                        final_text(&mut out, string(result, "response"));
                    }
                }
                "error" => {
                    flush_text(&mut out, &mut pending_text);
                    emit(&mut out, LiveKind::Error, &display(&v["error"]));
                }
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
            Harness::Muse => {
                let payload = &v["payload"];
                match string(&v, "payload_type") {
                    "run.output.delta" => {
                        let chunk = payload
                            .get("text")
                            .or_else(|| payload.get("delta"))
                            .and_then(|s| s.as_str())
                            .unwrap_or("");
                        pending_text.push_str(chunk);
                        if let Some(end) = pending_text.rfind('\n') {
                            emit(&mut out, LiveKind::Text, &pending_text[..=end]);
                            pending_text.drain(..=end);
                        }
                    }
                    "runtime.session" => {
                        let ev = &payload["event"];
                        match string(ev, "kind") {
                            "assistant_tool_calls_committed" => {
                                flush_text(&mut out, &mut pending_text);
                                if let Some(calls) = ev["tool_calls"].as_array() {
                                    for call in calls {
                                        let name = call
                                            .get("name")
                                            .or_else(|| call.get("tool_name"))
                                            .and_then(|s| s.as_str())
                                            .unwrap_or("tool");
                                        let raw =
                                            call.get("args").or_else(|| call.get("arguments"));
                                        let parsed = match raw {
                                            Some(serde_json::Value::String(s)) => {
                                                serde_json::from_str(s).unwrap_or_else(|_| {
                                                    serde_json::Value::String(s.clone())
                                                })
                                            }
                                            Some(other) => other.clone(),
                                            None => serde_json::Value::Null,
                                        };
                                        tool(&mut out, name, &parsed);
                                    }
                                }
                            }
                            "tool_result_batch_committed" => {
                                if let Some(results) = ev["results"].as_array() {
                                    for res in results {
                                        let content = res
                                            .get("text")
                                            .or_else(|| res.get("content"))
                                            .unwrap_or(&serde_json::Value::Null);
                                        let text_str = display(content);
                                        let is_err = res["is_error"].as_bool() == Some(true)
                                            || res
                                                .get("text")
                                                .and_then(|t| t.as_str())
                                                .and_then(|s| {
                                                    serde_json::from_str::<serde_json::Value>(s)
                                                        .ok()
                                                })
                                                .is_some_and(|j| {
                                                    j.get("exit_code")
                                                        .and_then(|c| c.as_i64())
                                                        .is_some_and(|c| c != 0)
                                                        || j.get("terminal_status")
                                                            .and_then(|s| s.as_str())
                                                            == Some("failed")
                                                });
                                        emit(
                                            &mut out,
                                            if is_err {
                                                LiveKind::Error
                                            } else {
                                                LiveKind::Outcome
                                            },
                                            &text_str,
                                        );
                                    }
                                }
                            }
                            "assistant_message_committed"
                                if pending_text.is_empty()
                                    && !out.iter().any(|l| l.kind == LiveKind::Text) =>
                            {
                                if let Some(t) = ev.get("text").and_then(|s| s.as_str()) {
                                    emit(&mut out, LiveKind::Text, t);
                                } else if !ev["content"].is_null() {
                                    emit(&mut out, LiveKind::Text, &display(&ev["content"]));
                                }
                            }
                            _ => {}
                        }
                    }
                    "run.terminal.completed" => {
                        flush_text(&mut out, &mut pending_text);
                        final_text(&mut out, string(payload, "text"));
                    }
                    "run.terminal.failed" => {
                        flush_text(&mut out, &mut pending_text);
                        let msg = payload
                            .get("message")
                            .or_else(|| payload.get("reason"))
                            .or_else(|| payload.get("text"))
                            .and_then(|s| s.as_str())
                            .unwrap_or("");
                        if msg.is_empty() {
                            emit(&mut out, LiveKind::Error, &display(payload));
                        } else {
                            emit(&mut out, LiveKind::Error, msg);
                        }
                    }
                    _ => {}
                }
            }
            // Pi's `--mode json` events (probed 2026-11-14): the
            // assistant's committed message carries its text parts
            Harness::Pi => {
                if string(&v, "type") == "message_end" {
                    let m = &v["message"];
                    if string(m, "role") == "assistant" {
                        if string(m, "stopReason") == "error" {
                            emit(&mut out, LiveKind::Error, string(m, "errorMessage"));
                        } else if let Some(parts) = m["content"].as_array() {
                            for p in parts {
                                if string(p, "type") == "text" {
                                    emit(&mut out, LiveKind::Text, string(p, "text"));
                                }
                            }
                        }
                    }
                }
            }
            // Grok's `--output-format json` is one object at the end:
            // no stream to decode, the envelope carries the answer
            Harness::Codex | Harness::Grok => {}
        }
    }
    flush_text(&mut out, &mut pending_text);
    out
}

fn flush_text(out: &mut Vec<LiveLine>, pending: &mut String) {
    emit(out, LiveKind::Text, pending);
    pending.clear();
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
    // Real Claude optional partial-events probe; keep the full assistant too.
    pub const CLAUDE_PARTIAL: &str = r#####"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Rivers begin as"}},"session_id":"fcedb557-2e12-406b-8294-68490d0c3f03","parent_tool_use_id":null,"uuid":"9f0a182d-8500-43c7-a14f-5f21fef076d5","api_message_id":"msg_011Ch1sisQuHMye2CpQS1oHn"}
{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":" small streams f"}},"session_id":"fcedb557-2e12-406b-8294-68490d0c3f03","parent_tool_use_id":null,"uuid":"9fa2ba96-b00a-4be1-b8b3-745f2c299fa8","api_message_id":"msg_011Ch1sisQuHMye2CpQS1oHn"}
{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"ed by r"}},"session_id":"fcedb557-2e12-406b-8294-68490d0c3f03","parent_tool_use_id":null,"uuid":"540b2b58-599d-4943-82b0-72acbf07343d","api_message_id":"msg_011Ch1sisQuHMye2CpQS1oHn"}
{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"ain, snowmelt,"}},"session_id":"fcedb557-2e12-406b-8294-68490d0c3f03","parent_tool_use_id":null,"uuid":"2813946f-0440-4cef-8d90-3190dca20a04","api_message_id":"msg_011Ch1sisQuHMye2CpQS1oHn"}
{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":" or springs, gathering str"}},"session_id":"fcedb557-2e12-406b-8294-68490d0c3f03","parent_tool_use_id":null,"uuid":"5978f532-fb6b-4f2e-8e1f-ce1da8cc9984","api_message_id":"msg_011Ch1sisQuHMye2CpQS1oHn"}
{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"ength as they flow downhill"}},"session_id":"fcedb557-2e12-406b-8294-68490d0c3f03","parent_tool_use_id":null,"uuid":"aee4a122-dd1e-4bde-ba6a-316a96cb8bf2","api_message_id":"msg_011Ch1sisQuHMye2CpQS1oHn"}
{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":" and"}},"session_id":"fcedb557-2e12-406b-8294-68490d0c3f03","parent_tool_use_id":null,"uuid":"ce877930-c784-46a1-85d8-ef150dc72e0e","api_message_id":"msg_011Ch1sisQuHMye2CpQS1oHn"}
{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":" mer"}},"session_id":"fcedb557-2e12-406b-8294-68490d0c3f03","parent_tool_use_id":null,"uuid":"083b034c-5531-4c3a-b7c9-39fa25e5a699","api_message_id":"msg_011Ch1sisQuHMye2CpQS1oHn"}
{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"ge with tributaries."}},"session_id":"fcedb557-2e12-406b-8294-68490d0c3f03","parent_tool_use_id":null,"uuid":"12d06a3a-be0b-4c76-8a68-b8ddff531555","api_message_id":"msg_011Ch1sisQuHMye2CpQS1oHn"}
{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":" Along the way they carve valle"}},"session_id":"fcedb557-2e12-406b-8294-68490d0c3f03","parent_tool_use_id":null,"uuid":"4ae326e6-f21e-4c80-a8c6-d6b506ec1cc4","api_message_id":"msg_011Ch1sisQuHMye2CpQS1oHn"}
{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"ys, car"}},"session_id":"fcedb557-2e12-406b-8294-68490d0c3f03","parent_tool_use_id":null,"uuid":"2326a261-bf87-4e7b-b729-9a12501aa641","api_message_id":"msg_011Ch1sisQuHMye2CpQS1oHn"}
{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"ry sediment, and sust"}},"session_id":"fcedb557-2e12-406b-8294-68490d0c3f03","parent_tool_use_id":null,"uuid":"807ab905-4e1f-4d0b-833a-be6ee6171985","api_message_id":"msg_011Ch1sisQuHMye2CpQS1oHn"}
{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"ain "}},"session_id":"fcedb557-2e12-406b-8294-68490d0c3f03","parent_tool_use_id":null,"uuid":"1cf49438-2ce0-4d9b-a716-af7dad3d4f15","api_message_id":"msg_011Ch1sisQuHMye2CpQS1oHn"}
{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"ecosystems and"}},"session_id":"fcedb557-2e12-406b-8294-68490d0c3f03","parent_tool_use_id":null,"uuid":"75927095-0c60-4d1a-99ae-22a3f7204e9b","api_message_id":"msg_011Ch1sisQuHMye2CpQS1oHn"}
{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":" communities before fin"}},"session_id":"fcedb557-2e12-406b-8294-68490d0c3f03","parent_tool_use_id":null,"uuid":"5b11313f-e14c-4e6a-8d32-5423d7de848d","api_message_id":"msg_011Ch1sisQuHMye2CpQS1oHn"}
{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"ally emptying into l"}},"session_id":"fcedb557-2e12-406b-8294-68490d0c3f03","parent_tool_use_id":null,"uuid":"00a4446f-8f7d-4d87-864d-5a6f6f1e13af","api_message_id":"msg_011Ch1sisQuHMye2CpQS1oHn"}
{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"akes, seas, or o"}},"session_id":"fcedb557-2e12-406b-8294-68490d0c3f03","parent_tool_use_id":null,"uuid":"f6670138-5a0d-4a31-bb7a-391d05a968c9","api_message_id":"msg_011Ch1sisQuHMye2CpQS1oHn"}
{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"ceans."}},"session_id":"fcedb557-2e12-406b-8294-68490d0c3f03","parent_tool_use_id":null,"uuid":"49759cab-756e-4730-bfb7-20ea7dd7dbfd","api_message_id":"msg_011Ch1sisQuHMye2CpQS1oHn"}
{"type":"assistant","message":{"model":"claude-fable-5-2","id":"msg_011Ch1sisQuHMye2CpQS1oHn","type":"message","role":"assistant","content":[{"type":"text","text":"Rivers begin as small streams fed by rain, snowmelt, or springs, gathering strength as they flow downhill and merge with tributaries. Along the way they carve valleys, carry sediment, and sustain ecosystems and communities before finally emptying into lakes, seas, or oceans."}],"container":null,"stop_reason":null,"stop_sequence":null,"stop_details":null,"usage":{"input_tokens":2,"cache_creation_input_tokens":17200,"cache_read_input_tokens":11370,"cache_creation":{"ephemeral_5m_input_tokens":0,"ephemeral_1h_input_tokens":17200},"output_tokens":7,"service_tier":"standard","inference_geo":"global","speed":"standard"},"input_transformations":[],"diagnostics":null,"context_management":null},"parent_tool_use_id":null,"session_id":"fcedb557-2e12-406b-8294-68490d0c3f03","uuid":"ce02dced-def9-4a1a-a078-cf4dbeefe912","timestamp":"2026-11-14T00:29:33.011Z","request_id":"req_011Ch1sisGy7cMi7yf3jhiDM"}
"#####;
    pub const CLAUDE: &str = r###"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"toolu_01G2aSKfJxmjd96HtS2zvCVF","name":"Bash","input":{"command":"printf stream-probe","description":"Print the stream-probe text"},"caller":{"type":"direct"}}]}}
{"type":"user","message":{"content":[{"tool_use_id":"toolu_01G2aSKfJxmjd96HtS2zvCVF","type":"tool_result","content":"stream-probe","is_error":false}]}}
{"type":"assistant","message":{"content":[{"type":"text","text":"probe done"}]}}
{"type":"result","result":"probe done","usage":{"input_tokens":4,"cache_creation_input_tokens":17267,"cache_read_input_tokens":39846,"output_tokens":156,"output_tokens_details":{"thinking_tokens":68},"server_tool_use":{"web_search_requests":0,"web_fetch_requests":0},"service_tier":"standard","cache_creation":{"ephemeral_1h_input_tokens":17267,"ephemeral_5m_input_tokens":0},"inference_geo":"global","iterations":[{"input_tokens":2,"output_tokens":5,"cache_read_input_tokens":28476,"cache_creation_input_tokens":161,"cache_creation":{"ephemeral_5m_input_tokens":0,"ephemeral_1h_input_tokens":161},"type":"message"}],"speed":"standard","fallback_credit":null},"total_cost_usd":0.2905132,"is_error":false}
"###;
    // Real agy 1.5.8 multi-paragraph stream: ACTIVE word fragments then DONE.
    pub const AGY_PARAGRAPHS: &str = r#####"{"event":"init","conversation_id":"fac668ea-3620-4e8e-b9fd-eab383d7ba89","init":{"cwd":"/private/var/folders/tl/0nz3g9w50pz78dv7mrcwc7bc0000gn/T/flow-delta-probe-lp579cpz","tools":["ask_custom_permission","ask_permission","ask_question","browser_click_element","browser_drag_pixel_to_pixel","browser_get_dom","browser_get_network_request","browser_input","browser_list_network_requests","browser_mouse_down","browser_mouse_up","browser_move_mouse","browser_press_key","browser_refresh_page","browser_resize_window","browser_scroll","browser_scroll_dom","browser_select_option","browser_subagent","call_mcp_tool","capture_browser_console_logs","capture_browser_screenshot","click_browser_pixel","command_status","define_subagent","delete_knowledge","execute_browser_javascript","find_by_name","finish","generate_image","grep_search","invoke_subagent","list_browser_pages","list_dir","list_permissions","list_plugin_accounts","list_resources","manage_inbox","manage_subagents","manage_task","multi_replace_file_content","notebook_edit","notebook_execution","open_browser_url","read_browser_page","read_resource","read_url_content","replace_file_content","run_command","run_workflow","schedule","search_marketplace","search_web","sed_file","send_command_input","send_message","view_file","wait","wait_5_seconds","write_to_file"],"permission_mode":"request-review"}}
{"event":"step_update","step_update":{"conversation_id":"fac668ea-3620-4e8e-b9fd-eab383d7ba89","step_index":0,"state":"DONE","step_type":"user_input"}}
{"event":"step_update","step_update":{"conversation_id":"fac668ea-3620-4e8e-b9fd-eab383d7ba89","step_index":1,"state":"DONE","step_type":"unknown","duration_seconds":0.000276}}
{"event":"step_update","step_update":{"conversation_id":"fac668ea-3620-4e8e-b9fd-eab383d7ba89","step_index":2,"state":"ACTIVE","step_type":"agent_response","text_delta":"Rivers typically begin at high elevations as small trickles "}}
{"event":"step_update","step_update":{"conversation_id":"fac668ea-3620-4e8e-b9fd-eab383d7ba89","step_index":2,"state":"ACTIVE","step_type":"agent_response","text_delta":"of water fed"}}
{"event":"step_update","step_update":{"conversation_id":"fac668ea-3620-4e8e-b9fd-eab383d7ba89","step_index":2,"state":"ACTIVE","step_type":"agent_response","text_delta":" by melting"}}
{"event":"step_update","step_update":{"conversation_id":"fac668ea-3620-4e8e-b9fd-eab383d7ba89","step_index":2,"state":"ACTIVE","step_type":"agent_response","text_delta":" snow, glaciers, or underground spri"}}
{"event":"step_update","step_update":{"conversation_id":"fac668ea-3620-4e8e-b9fd-eab383d7ba89","step_index":2,"state":"ACTIVE","step_type":"agent_response","text_delta":"ngs. As rainfall and run"}}
{"event":"step_update","step_update":{"conversation_id":"fac668ea-3620-4e8e-b9fd-eab383d7ba89","step_index":2,"state":"ACTIVE","step_type":"agent_response","text_delta":"off gather i"}}
{"event":"step_update","step_update":{"conversation_id":"fac668ea-3620-4e8e-b9fd-eab383d7ba89","step_index":2,"state":"ACTIVE","step_type":"agent_response","text_delta":"n these upland headwater"}}
{"event":"step_update","step_update":{"conversation_id":"fac668ea-3620-4e8e-b9fd-eab383d7ba89","step_index":2,"state":"ACTIVE","step_type":"agent_response","text_delta":"s, tiny streams merge to"}}
{"event":"step_update","step_update":{"conversation_id":"fac668ea-3620-4e8e-b9fd-eab383d7ba89","step_index":2,"state":"ACTIVE","step_type":"agent_response","text_delta":" form"}}
{"event":"step_update","step_update":{"conversation_id":"fac668ea-3620-4e8e-b9fd-eab383d7ba89","step_index":2,"state":"ACTIVE","step_type":"agent_response","text_delta":" a steady channel.\n\nPulled by gravit"}}
{"event":"step_update","step_update":{"conversation_id":"fac668ea-3620-4e8e-b9fd-eab383d7ba89","step_index":2,"state":"ACTIVE","step_type":"agent_response","text_delta":"y, the flowing water car"}}
{"event":"step_update","step_update":{"conversation_id":"fac668ea-3620-4e8e-b9fd-eab383d7ba89","step_index":2,"state":"ACTIVE","step_type":"agent_response","text_delta":"ves a winding path downh"}}
{"event":"step_update","step_update":{"conversation_id":"fac668ea-3620-4e8e-b9fd-eab383d7ba89","step_index":2,"state":"ACTIVE","step_type":"agent_response","text_delta":"ill through "}}
{"event":"step_update","step_update":{"conversation_id":"fac668ea-3620-4e8e-b9fd-eab383d7ba89","step_index":2,"state":"ACTIVE","step_type":"agent_response","text_delta":"valleys and across plain"}}
{"event":"step_update","step_update":{"conversation_id":"fac668ea-3620-4e8e-b9fd-eab383d7ba89","step_index":2,"state":"ACTIVE","step_type":"agent_response","text_delta":"s"}}
{"event":"step_update","step_update":{"conversation_id":"fac668ea-3620-4e8e-b9fd-eab383d7ba89","step_index":2,"state":"ACTIVE","step_type":"agent_response","text_delta":". Along the journey, the river grows wider and slower as tri"}}
{"event":"step_update","step_update":{"conversation_id":"fac668ea-3620-4e8e-b9fd-eab383d7ba89","step_index":2,"state":"ACTIVE","step_type":"agent_response","text_delta":"butaries joi"}}
{"event":"step_update","step_update":{"conversation_id":"fac668ea-3620-4e8e-b9fd-eab383d7ba89","step_index":2,"state":"ACTIVE","step_type":"agent_response","text_delta":"n the main current and c"}}
{"event":"step_update","step_update":{"conversation_id":"fac668ea-3620-4e8e-b9fd-eab383d7ba89","step_index":2,"state":"ACTIVE","step_type":"agent_response","text_delta":"arry sedimen"}}
{"event":"step_update","step_update":{"conversation_id":"fac668ea-3620-4e8e-b9fd-eab383d7ba89","step_index":2,"state":"ACTIVE","step_type":"agent_response","text_delta":"t downstream.\n\nMost"}}
{"event":"step_update","step_update":{"conversation_id":"fac668ea-3620-4e8e-b9fd-eab383d7ba89","step_index":2,"state":"ACTIVE","step_type":"agent_response","text_delta":" rivers eventually reach a mouth where they empt"}}
{"event":"step_update","step_update":{"conversation_id":"fac668ea-3620-4e8e-b9fd-eab383d7ba89","step_index":2,"state":"ACTIVE","step_type":"agent_response","text_delta":"y their freshwater into "}}
{"event":"step_update","step_update":{"conversation_id":"fac668ea-3620-4e8e-b9fd-eab383d7ba89","step_index":2,"state":"ACTIVE","step_type":"agent_response","text_delta":"an ocean, sea, or large "}}
{"event":"step_update","step_update":{"conversation_id":"fac668ea-3620-4e8e-b9fd-eab383d7ba89","step_index":2,"state":"ACTIVE","step_type":"agent_response","text_delta":"lake. At thi"}}
{"event":"step_update","step_update":{"conversation_id":"fac668ea-3620-4e8e-b9fd-eab383d7ba89","step_index":2,"state":"ACTIVE","step_type":"agent_response","text_delta":"s final meeting"}}
{"event":"step_update","step_update":{"conversation_id":"fac668ea-3620-4e8e-b9fd-eab383d7ba89","step_index":2,"state":"ACTIVE","step_type":"agent_response","text_delta":" point, the slowing current drops it"}}
{"event":"step_update","step_update":{"conversation_id":"fac668ea-3620-4e8e-b9fd-eab383d7ba89","step_index":2,"state":"ACTIVE","step_type":"agent_response","text_delta":"s remaining sediment to "}}
{"event":"step_update","step_update":{"conversation_id":"fac668ea-3620-4e8e-b9fd-eab383d7ba89","step_index":2,"state":"ACTIVE","step_type":"agent_response","text_delta":"build fertil"}}
{"event":"step_update","step_update":{"conversation_id":"fac668ea-3620-4e8e-b9fd-eab383d7ba89","step_index":2,"state":"DONE","step_type":"agent_response","text_delta":"e deltas or mix into coastal estuaries.\n","duration_seconds":13.163624,"usage":{"input_tokens":12703,"output_tokens":675,"thinking_tokens":541,"cache_read_tokens":0,"total_tokens":13378}}}
{"event":"result","result":{"conversation_id":"fac668ea-3620-4e8e-b9fd-eab383d7ba89","status":"SUCCESS","response":"Rivers typically begin at high elevations as small trickles of water fed by melting snow, glaciers, or underground springs. As rainfall and runoff gather in these upland headwaters, tiny streams merge to form a steady channel.\n\nPulled by gravity, the flowing water carves a winding path downhill through valleys and across plains. Along the journey, the river grows wider and slower as tributaries join the main current and carry sediment downstream.\n\nMost rivers eventually reach a mouth where they empty their freshwater into an ocean, sea, or large lake. At this final meeting point, the slowing current drops its remaining sediment to build fertile deltas or mix into coastal estuaries.\n","duration_seconds":13.559053,"num_turns":1,"usage":{"input_tokens":12703,"output_tokens":675,"thinking_tokens":541,"cache_read_tokens":0,"total_tokens":13378}}}
"#####;
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
    fn active_agy_deltas_form_exact_paragraphs() {
        assert_eq!(lines(Harness::Antigravity, fixtures::AGY_PARAGRAPHS.as_bytes()), vec![LiveLine { kind: LiveKind::Text, text: "Rivers typically begin at high elevations as small trickles of water fed by melting snow, glaciers, or underground springs. As rainfall and runoff gather in these upland headwaters, tiny streams merge to form a steady channel.".into() },LiveLine { kind: LiveKind::Text, text: "Pulled by gravity, the flowing water carves a winding path downhill through valleys and across plains. Along the journey, the river grows wider and slower as tributaries join the main current and carry sediment downstream.".into() },LiveLine { kind: LiveKind::Text, text: "Most rivers eventually reach a mouth where they empty their freshwater into an ocean, sea, or large lake. At this final meeting point, the slowing current drops its remaining sediment to build fertile deltas or mix into coastal estuaries.".into() }]);
        let mut cumulative = String::new();
        let mut expected = String::new();
        for record in fixtures::AGY_PARAGRAPHS.lines() {
            cumulative.push_str(record);
            cumulative.push('\n');
            let Ok(v) = serde_json::from_str::<serde_json::Value>(record) else {
                continue;
            };
            if let Some(delta) = v["step_update"]["text_delta"].as_str() {
                expected.push_str(delta);
            }
            if !expected.is_empty() {
                let got = lines(Harness::Antigravity, cumulative.as_bytes());
                assert_eq!(
                    got.iter().map(|l| l.text.as_str()).collect::<Vec<_>>(),
                    expected
                        .lines()
                        .map(str::trim)
                        .filter(|l| !l.is_empty())
                        .collect::<Vec<_>>()
                );
            }
        }
    }

    #[test]
    fn agy_flushes_text_before_tools_and_new_response_steps() {
        let responses: Vec<serde_json::Value> = fixtures::AGY_PARAGRAPHS
            .lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .filter(|v| v["step_update"]["text_delta"].is_string())
            .take(3)
            .collect();
        let tool = fixtures::AGY_DENIED
            .lines()
            .find(|l| {
                l.contains("\"tool_name\":\"run_command\"") && l.contains("\"state\":\"ACTIVE\"")
            })
            .unwrap();
        let stream = format!(
            "{}\n{}\n{tool}\n{}\n",
            responses[0], responses[1], responses[2]
        );
        let decoded = lines(Harness::Antigravity, stream.as_bytes());
        assert_eq!(
            decoded
                .iter()
                .map(|l| (l.kind, l.text.as_str()))
                .collect::<Vec<_>>(),
            vec![
                (
                    LiveKind::Text,
                    "Rivers typically begin at high elevations as small trickles of water fed"
                ),
                (
                    LiveKind::Tool,
                    "▸ run_command {\"CommandLine\":\"printf stream-probe\"}"
                ),
                (LiveKind::Text, "by melting"),
            ]
        );
        let mut next = responses[2].clone();
        next["step_update"]["step_index"] = serde_json::json!(999);
        let stream = format!("{}\n{}\n{next}\n", responses[0], responses[1]);
        assert_eq!(
            lines(Harness::Antigravity, stream.as_bytes())
                .iter()
                .map(|l| l.text.as_str().to_owned())
                .collect::<Vec<_>>(),
            vec![
                "Rivers typically begin at high elevations as small trickles of water fed",
                "by melting"
            ]
        );
    }

    #[test]
    fn claude_optional_partial_events_do_not_duplicate_the_complete_text() {
        assert_eq!(lines(Harness::Claude, fixtures::CLAUDE_PARTIAL.as_bytes()), vec![LiveLine { kind: LiveKind::Text, text: "Rivers begin as small streams fed by rain, snowmelt, or springs, gathering strength as they flow downhill and merge with tributaries. Along the way they carve valleys, carry sediment, and sustain ecosystems and communities before finally em…".into() }]);
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
        for h in [
            Harness::Claude,
            Harness::Antigravity,
            Harness::OpenCode,
            Harness::Muse,
        ] {
            assert!(lines(h, b"{\"unknown\":true}\n{\"type\":\"text\"").is_empty());
        }
    }

    #[test]
    fn muse_exec_json_decodes_deltas_tools_and_terminal() {
        let stream = concat!(
            r#"{"payload_type":"run.output.delta","payload":{"text":"hello "}}"#,
            "\n",
            r#"{"payload_type":"run.output.delta","payload":{"text":"world\n"}}"#,
            "\n",
            r#"{"payload_type":"runtime.session","payload":{"kind":"run","event":{"kind":"assistant_tool_calls_committed","tool_calls":[{"name":"shell","args":"{\"command\":\"ls\"}"}]}}}"#,
            "\n",
            r#"{"payload_type":"runtime.session","payload":{"kind":"run","event":{"kind":"tool_result_batch_committed","results":[{"text":"{\"exit_code\":0}"}]}}}"#,
            "\n",
            r#"{"payload_type":"runtime.session","payload":{"kind":"run","event":{"kind":"tool_result_batch_committed","results":[{"text":"{\"exit_code\":1,\"terminal_status\":\"failed\"}"}]}}}"#,
            "\n",
            r#"{"payload_type":"run.terminal.completed","payload":{"text":"hello world"}}"#,
            "\n",
            r#"{"payload_type":"run.terminal.failed","payload":{"message":"timed out"}}"#,
            "\n",
        );
        let decoded = lines(Harness::Muse, stream.as_bytes());
        assert_eq!(
            decoded
                .iter()
                .map(|l| (l.kind, l.text.as_str()))
                .collect::<Vec<_>>(),
            vec![
                (LiveKind::Text, "hello world"),
                (LiveKind::Tool, r#"▸ shell {"command":"ls"}"#),
                (LiveKind::Outcome, r#"{"exit_code":0}"#),
                (
                    LiveKind::Error,
                    r#"{"exit_code":1,"terminal_status":"failed"}"#
                ),
                (LiveKind::Error, "timed out"),
            ]
        );
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
