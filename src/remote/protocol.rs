//! The remote control's wire protocol.
//!
//! Text frames are JSON control messages tagged with `t`; binary frames
//! carry terminal bytes behind a five-byte header. Session ids on the wire
//! are always `Session.id` (stable for a run), never a vector index.

use crate::tracing::analysis::model::LiveSession;
use serde::{Deserialize, Serialize};

/// Server -> client: append these bytes to the session's terminal.
pub const KIND_OUTPUT: u8 = 0x01;
/// Server -> client: reset the terminal, then write these bytes.
pub const KIND_RESYNC: u8 = 0x02;
/// Client -> server: type these bytes into the session.
pub const KIND_INPUT: u8 = 0x10;

/// `[kind][session: u32 BE][payload]`.
pub fn encode_binary(kind: u8, session: usize, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(5 + payload.len());
    out.push(kind);
    out.extend_from_slice(&(session as u32).to_be_bytes());
    out.extend_from_slice(payload);
    out
}

pub fn decode_binary(frame: &[u8]) -> Option<(u8, usize, &[u8])> {
    if frame.len() < 5 {
        return None;
    }
    let session = u32::from_be_bytes([frame[1], frame[2], frame[3], frame[4]]) as usize;
    Some((frame[0], session, &frame[5..]))
}

/// Every `t` a client may send. Mirrored by `web/app.js`; the asset test
/// holds the two in step.
pub const CLIENT_FRAMES: &[&str] = &[
    "hello",
    "select",
    "resync",
    "key",
    "paste",
    "catalog",
    "launch",
    "kill",
    "launch_skill",
    "start_loop",
    "start_workflow",
    "ping",
];

/// Every `t` the server may send.
pub const SERVER_FRAMES: &[&str] = &[
    "hello", "sessions", "catalog", "exit", "result", "notice", "pong",
];

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ClientMsg {
    Hello {
        #[serde(default)]
        focus: Option<usize>,
    },
    Select {
        s: usize,
    },
    Resync {
        s: usize,
    },
    Key {
        s: usize,
        key: String,
        #[serde(default)]
        ch: Option<String>,
        #[serde(default)]
        mods: Vec<String>,
    },
    Paste {
        s: usize,
        text: String,
    },
    Catalog,
    Launch {
        #[serde(default)]
        id: Option<String>,
        profile: String,
        dir: String,
    },
    Kill {
        #[serde(default)]
        id: Option<String>,
        s: usize,
    },
    LaunchSkill {
        #[serde(default)]
        id: Option<String>,
        skill: String,
        #[serde(default)]
        harness: Option<String>,
    },
    StartLoop {
        #[serde(default)]
        id: Option<String>,
        #[serde(rename = "loop")]
        loop_id: String,
    },
    StartWorkflow {
        #[serde(default)]
        id: Option<String>,
        workflow: String,
        workspace: String,
        #[serde(default)]
        harness: Option<String>,
        #[serde(default)]
        args: serde_json::Value,
    },
    Ping,
}

/// One session as the remote sees it: everything the live snapshot carries,
/// plus the few things only the App knows.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RemoteSession {
    #[serde(flatten)]
    pub live: LiveSession,
    /// The profile name -- what the sidebar shows.
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skill_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProfileInfo {
    pub name: String,
    pub command: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_dir: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SkillInfo {
    pub id: String,
    pub name: String,
    pub harnesses: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LoopInfo {
    pub id: String,
    pub workspace: String,
    pub pattern: String,
    pub enabled: bool,
    pub paused: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WorkflowInfo {
    pub name: String,
    pub source: String,
    pub valid: bool,
    pub args: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ServerMsg {
    Hello {
        version: String,
        run_id: String,
        cols: u16,
        rows: u16,
        allow_kill: bool,
        allow_launch: bool,
        client: u64,
    },
    Sessions {
        #[serde(skip_serializing_if = "Option::is_none")]
        selected: Option<usize>,
        attached: bool,
        cols: u16,
        rows: u16,
        sessions: Vec<RemoteSession>,
    },
    Catalog {
        profiles: Vec<ProfileInfo>,
        skills: Vec<SkillInfo>,
        loops: Vec<LoopInfo>,
        workflows: Vec<WorkflowInfo>,
    },
    Exit {
        s: usize,
        #[serde(skip_serializing_if = "Option::is_none")]
        code: Option<u32>,
    },
    Result {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        ok: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        s: Option<usize>,
        #[serde(skip_serializing_if = "Option::is_none")]
        run: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    Notice {
        level: String,
        text: String,
    },
    Pong,
}

impl ServerMsg {
    pub fn to_json(&self) -> String {
        // Every variant is plain data; a failure here is a bug, not a
        // runtime condition, and the client would rather see an error frame.
        serde_json::to_string(self).unwrap_or_else(|e| {
            format!(r#"{{"t":"notice","level":"error","text":"encode failed: {e}"}}"#)
        })
    }
}

/// A helper-key button's key, encoded server-side so the one key table in
/// `crate::keys` stays the only one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamedKey {
    pub code: crossterm::event::KeyCode,
    pub mods: crossterm::event::KeyModifiers,
}

impl NamedKey {
    /// Parses a client `key` (+ `ch` for `"char"`) and its modifier list.
    pub fn parse(key: &str, ch: Option<&str>, mods: &[String]) -> Option<NamedKey> {
        use crossterm::event::{KeyCode, KeyModifiers};
        let code = match key {
            "enter" => KeyCode::Enter,
            "esc" => KeyCode::Esc,
            "tab" => KeyCode::Tab,
            "backtab" => KeyCode::BackTab,
            "backspace" => KeyCode::Backspace,
            "delete" => KeyCode::Delete,
            "insert" => KeyCode::Insert,
            "up" => KeyCode::Up,
            "down" => KeyCode::Down,
            "left" => KeyCode::Left,
            "right" => KeyCode::Right,
            "home" => KeyCode::Home,
            "end" => KeyCode::End,
            "pageup" => KeyCode::PageUp,
            "pagedown" => KeyCode::PageDown,
            "char" => KeyCode::Char(ch?.chars().next()?),
            other => {
                let n: u8 = other.strip_prefix('f')?.parse().ok()?;
                if !(1..=12).contains(&n) {
                    return None;
                }
                KeyCode::F(n)
            }
        };
        let mut m = KeyModifiers::NONE;
        for name in mods {
            match name.as_str() {
                "ctrl" => m |= KeyModifiers::CONTROL,
                "alt" => m |= KeyModifiers::ALT,
                "shift" => m |= KeyModifiers::SHIFT,
                _ => return None,
            }
        }
        Some(NamedKey { code, mods: m })
    }

    pub fn to_key_event(&self) -> crossterm::event::KeyEvent {
        crossterm::event::KeyEvent::new(self.code, self.mods)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyModifiers};

    #[test]
    fn binary_header_round_trips() {
        let frame = encode_binary(KIND_OUTPUT, 7, b"hello");
        assert_eq!(frame.len(), 10);
        let (kind, session, payload) = decode_binary(&frame).unwrap();
        assert_eq!((kind, session, payload), (KIND_OUTPUT, 7, &b"hello"[..]));
        // A large id still fits the u32 field.
        let (_, big, _) = decode_binary(&encode_binary(KIND_RESYNC, 70_000, b"")).unwrap();
        assert_eq!(big, 70_000);
        assert!(decode_binary(&[1, 2, 3]).is_none());
    }

    #[test]
    fn every_client_frame_deserializes() {
        let cases: &[(&str, ClientMsg)] = &[
            (r#"{"t":"hello"}"#, ClientMsg::Hello { focus: None }),
            (
                r#"{"t":"hello","focus":3}"#,
                ClientMsg::Hello { focus: Some(3) },
            ),
            (r#"{"t":"select","s":2}"#, ClientMsg::Select { s: 2 }),
            (r#"{"t":"resync","s":0}"#, ClientMsg::Resync { s: 0 }),
            (
                r#"{"t":"key","s":1,"key":"up","mods":["ctrl"]}"#,
                ClientMsg::Key {
                    s: 1,
                    key: "up".into(),
                    ch: None,
                    mods: vec!["ctrl".into()],
                },
            ),
            (
                r#"{"t":"paste","s":1,"text":"x"}"#,
                ClientMsg::Paste {
                    s: 1,
                    text: "x".into(),
                },
            ),
            (r#"{"t":"catalog"}"#, ClientMsg::Catalog),
            (
                r#"{"t":"launch","profile":"p","dir":"/d"}"#,
                ClientMsg::Launch {
                    id: None,
                    profile: "p".into(),
                    dir: "/d".into(),
                },
            ),
            (
                r#"{"t":"kill","id":"r1","s":4}"#,
                ClientMsg::Kill {
                    id: Some("r1".into()),
                    s: 4,
                },
            ),
            (
                r#"{"t":"launch_skill","skill":"heimdall"}"#,
                ClientMsg::LaunchSkill {
                    id: None,
                    skill: "heimdall".into(),
                    harness: None,
                },
            ),
            (
                r#"{"t":"start_loop","loop":"triage"}"#,
                ClientMsg::StartLoop {
                    id: None,
                    loop_id: "triage".into(),
                },
            ),
            (r#"{"t":"ping"}"#, ClientMsg::Ping),
        ];
        for (json, expected) in cases {
            let parsed: ClientMsg = serde_json::from_str(json).expect(json);
            assert_eq!(&parsed, expected, "{json}");
        }
        let wf: ClientMsg =
            serde_json::from_str(r#"{"t":"start_workflow","workflow":"w","workspace":"/w"}"#)
                .unwrap();
        assert!(matches!(wf, ClientMsg::StartWorkflow { .. }));
    }

    #[test]
    fn client_frames_constant_lists_every_variant() {
        // Every name in the constant must actually parse, so the client can
        // trust the list the asset test checks it against.
        for name in CLIENT_FRAMES {
            let json = match *name {
                "select" | "resync" => format!(r#"{{"t":"{name}","s":1}}"#),
                "key" => r#"{"t":"key","s":1,"key":"esc"}"#.to_string(),
                "paste" => r#"{"t":"paste","s":1,"text":""}"#.to_string(),
                "launch" => r#"{"t":"launch","profile":"p","dir":"/"}"#.to_string(),
                "kill" => r#"{"t":"kill","s":1}"#.to_string(),
                "launch_skill" => r#"{"t":"launch_skill","skill":"s"}"#.to_string(),
                "start_loop" => r#"{"t":"start_loop","loop":"l"}"#.to_string(),
                "start_workflow" => {
                    r#"{"t":"start_workflow","workflow":"w","workspace":"/"}"#.to_string()
                }
                _ => format!(r#"{{"t":"{name}"}}"#),
            };
            serde_json::from_str::<ClientMsg>(&json).unwrap_or_else(|e| panic!("{name}: {e}"));
        }
    }

    #[test]
    fn server_messages_serialize_with_their_tag() {
        let msgs = [
            ServerMsg::Hello {
                version: "0.1.0".into(),
                run_id: "r".into(),
                cols: 80,
                rows: 24,
                allow_kill: true,
                allow_launch: false,
                client: 1,
            },
            ServerMsg::Exit {
                s: 2,
                code: Some(0),
            },
            ServerMsg::Result {
                id: None,
                ok: false,
                s: None,
                run: None,
                error: Some("nope".into()),
            },
            ServerMsg::Notice {
                level: "warn".into(),
                text: "t".into(),
            },
            ServerMsg::Pong,
        ];
        for m in &msgs {
            let v: serde_json::Value = serde_json::from_str(&m.to_json()).unwrap();
            let tag = v["t"].as_str().unwrap().to_string();
            assert!(SERVER_FRAMES.contains(&tag.as_str()), "{tag} not listed");
        }
        // Absent options stay off the wire rather than serializing as null.
        let json = ServerMsg::Exit { s: 1, code: None }.to_json();
        assert!(!json.contains("code"), "{json}");
    }

    #[test]
    fn remote_session_flattens_the_live_session() {
        use crate::tracing::analysis::model::RuntimeState;
        let msg = ServerMsg::Sessions {
            selected: Some(1),
            attached: false,
            cols: 80,
            rows: 24,
            sessions: vec![RemoteSession {
                live: LiveSession {
                    run_id: "r".into(),
                    launch_id: "l".into(),
                    session_id: 1,
                    session_key: None,
                    provider: Some("claude".into()),
                    cwd: std::path::PathBuf::from("/repo"),
                    state: RuntimeState::Working,
                    updated_at_ns: 5,
                    active_tools: vec![],
                },
                name: "claude-main".into(),
                exit_code: None,
                skill_id: None,
            }],
        };
        let v: serde_json::Value = serde_json::from_str(&msg.to_json()).unwrap();
        let s = &v["sessions"][0];
        assert_eq!(s["session_id"], 1);
        assert_eq!(s["state"], "working");
        assert_eq!(s["name"], "claude-main");
        assert_eq!(s["cwd"], "/repo");
    }

    #[test]
    fn named_keys_map_onto_the_crossterm_table() {
        let k = NamedKey::parse("up", None, &["ctrl".into()]).unwrap();
        assert_eq!(k.code, KeyCode::Up);
        assert_eq!(k.mods, KeyModifiers::CONTROL);
        assert_eq!(
            NamedKey::parse("char", Some("c"), &["ctrl".into()])
                .unwrap()
                .code,
            KeyCode::Char('c')
        );
        assert_eq!(
            NamedKey::parse("f7", None, &[]).unwrap().code,
            KeyCode::F(7)
        );
        assert_eq!(
            NamedKey::parse("esc", None, &[]).unwrap().code,
            KeyCode::Esc
        );
        assert!(NamedKey::parse("char", None, &[]).is_none());
        assert!(NamedKey::parse("f13", None, &[]).is_none());
        assert!(NamedKey::parse("nope", None, &[]).is_none());
        assert!(NamedKey::parse("up", None, &["meta".into()]).is_none());
    }

    #[test]
    fn named_ctrl_c_encodes_to_the_interrupt_byte() {
        let k = NamedKey::parse("char", Some("c"), &["ctrl".into()]).unwrap();
        let bytes = crate::keys::encode_key_with_mode(&k.to_key_event(), false).unwrap();
        assert_eq!(bytes, vec![0x03]);
    }

    #[test]
    fn named_arrows_follow_the_application_cursor_mode() {
        let k = NamedKey::parse("up", None, &[]).unwrap();
        let normal = crate::keys::encode_key_with_mode(&k.to_key_event(), false).unwrap();
        let app = crate::keys::encode_key_with_mode(&k.to_key_event(), true).unwrap();
        assert_eq!(normal, b"\x1b[A".to_vec());
        assert_eq!(app, b"\x1bOA".to_vec());
    }
}
