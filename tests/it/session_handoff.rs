//! `f` on a session continues it in a new one: the same Claude Code or
//! Codex forks the conversation; another harness reads its whole
//! transcript, handed over as a Markdown file.

use agent_mux::app::{App, Mode};
use agent_mux::config::Profile;
use agent_mux::handoff::Continue;
use agent_mux::harness::Harness;
use agent_mux::session::Session;
use agent_mux::tracing::store::model::{SessionRow, StoreOp};
use agent_mux::tracing::store::{OpenOptions, open_rw};
use std::path::Path;
use tokio::sync::mpsc;

fn profile(name: &str) -> Profile {
    Profile {
        name: name.into(),
        command: "sh".into(),
        args: vec!["-c".into(), "sleep 30".into()],
        default_dir: None,
        tracing: None,
        model: None,
        bypass_approvals: None,
    }
}

const TRANSCRIPT: &str = concat!(
    r#"{"type":"user","message":{"role":"user","content":"rename the parser module"},"timestamp":"2026-09-28T10:00:00Z"}"#,
    "\n",
    r#"{"type":"assistant","message":{"id":"m1","role":"assistant","model":"claude-opus-5-5","content":[{"type":"text","text":"Renamed it to lexer and fixed the imports."}]},"timestamp":"2026-09-28T10:00:05Z"}"#,
    "\n"
);

/// An App with one Claude Code session that resumed conversation `abc`,
/// and a store that knows that conversation's transcript file.
fn app_with_a_claude_session(dir: &Path) -> App {
    let transcript = dir.join("abc.jsonl");
    std::fs::write(&transcript, TRANSCRIPT).unwrap();
    let db = dir.join("traces.db");
    let mut store = open_rw(
        &db,
        OpenOptions {
            prices: agent_mux::tracing::pricing::PriceTable::builtin(),
            run_id: "run-1".into(),
            retention_days: 0,
            agent_mux_version: "test".into(),
        },
    )
    .unwrap();
    store
        .apply(&[StoreOp::Session(SessionRow {
            key: "claude:abc".into(),
            provider: "claude".into(),
            session_id: "abc".into(),
            user_id: None,
            cwd: Some(dir.display().to_string()),
            project_slug: None,
            transcript_path: Some(transcript.display().to_string()),
            title: None,
            seen_ns: 1,
            extra: None,
        })])
        .unwrap();
    drop(store);
    let (tx, _rx) = mpsc::channel(32);
    let mut claude = profile("Claude Code");
    claude.command = "claude".into();
    let mut codex = profile("Codex");
    codex.command = "codex".into();
    let mut app = App::new(vec![claude, codex], None, tx.clone());
    app.clipboard_enabled = false;
    app.runtime_dir = Some(dir.join("runtime"));
    app.library_root = Some(dir.join("library"));
    app.trace_db_path = Some(db);
    let mut s = Session::spawn(
        7,
        profile("Claude Code"),
        dir.to_path_buf(),
        10,
        80,
        tx,
        &[],
        &[],
    )
    .unwrap();
    s.profile.command = "claude".into();
    s.conversation = Some("abc".into());
    app.sessions.push(s);
    app
}

#[test]
fn the_same_harness_forks_and_another_reads_the_transcript() {
    let temp = tempfile::tempdir().unwrap();
    let mut app = app_with_a_claude_session(temp.path());

    // f: the New session dialog continues the session, on its profile
    app.open_handoff_dialog(0);
    let Mode::NewSession(d) = &app.mode else {
        panic!("{:?}", app.mode)
    };
    assert_eq!(d.handoff, Some(7));
    assert_eq!(d.profile_idx, 0);

    // Claude Code → Claude Code: a fork of the real conversation
    let same = app
        .session_handoff(7, Some(Harness::Claude), temp.path())
        .unwrap();
    assert_eq!(same, Continue::Fork("abc".into()));

    // Claude Code → Codex: the whole transcript, read first
    let other = app
        .session_handoff(7, Some(Harness::Codex), temp.path())
        .unwrap();
    let Continue::Transcript(args) = other else {
        panic!("{other:?}")
    };
    assert_eq!(args.len(), 1, "the message is Codex's positional prompt");
    let file = temp.path().join("runtime/handoffs/claude-abc.md");
    assert!(args[0].contains(&file.display().to_string()), "{}", args[0]);
    let md = std::fs::read_to_string(&file).unwrap();
    assert!(md.contains("rename the parser module"), "{md}");
    assert!(md.contains("Renamed it to lexer"), "{md}");

    // into Antigravity: its interactive prompt flag
    let agy = app
        .session_handoff(7, Some(Harness::Antigravity), temp.path())
        .unwrap();
    let Continue::Transcript(args) = agy else {
        panic!()
    };
    assert_eq!(args[0], "--prompt-interactive");
}

#[test]
fn a_session_with_no_conversation_yet_cannot_be_continued() {
    let temp = tempfile::tempdir().unwrap();
    let mut app = app_with_a_claude_session(temp.path());
    app.sessions[0].conversation = None;
    let e = app
        .session_handoff(7, Some(Harness::Codex), temp.path())
        .unwrap_err();
    // tracing off and no transcript in its folder yet: nothing to continue
    assert!(e.contains("has it had a turn"), "{e}");
}

#[test]
fn f_on_a_session_row_opens_the_dialog_that_continues_it() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::{Terminal, backend::TestBackend};
    let temp = tempfile::tempdir().unwrap();
    let mut app = app_with_a_claude_session(temp.path());
    app.select_first_row_of(agent_mux::app::SidebarSection::Active);
    app.handle_key(
        &KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE),
        std::time::Instant::now(),
    );
    let Mode::NewSession(d) = &app.mode else {
        panic!("{:?}", app.mode)
    };
    assert_eq!(d.handoff, Some(7));
    let mut t = Terminal::new(TestBackend::new(120, 40)).unwrap();
    t.draw(|f| agent_mux::ui::draw(f, &app, std::time::Instant::now()))
        .unwrap();
    let buf = t.backend().buffer().clone();
    let text: String = buf.content().iter().map(|c| c.symbol()).collect();
    assert!(
        text.contains("Continue Claude Code in a new session"),
        "{text}"
    );
}

/// A saved continued session comes back linked to where it came from, and
/// without the first message it sent: a restart does not send it again.
#[test]
fn a_restart_keeps_the_link_and_does_not_resend_the_handover() {
    let temp = tempfile::tempdir().unwrap();
    let mut app = app_with_a_claude_session(temp.path());
    app.sessions_file = Some(temp.path().join("sessions.json"));
    let s = &mut app.sessions[0];
    s.profile.args = vec!["--model".into(), "m".into(), "read the handover".into()];
    s.opening_args = vec!["read the handover".into()];
    s.continued_from = Some("Codex · api".into());
    let mut t = ratatui::Terminal::new(ratatui::backend::TestBackend::new(140, 30)).unwrap();
    t.draw(|f| agent_mux::ui::draw(f, &app, std::time::Instant::now()))
        .unwrap();
    let text: String = t
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|c| c.symbol())
        .collect();
    assert!(
        text.contains("↳ Codex · api"),
        "the sidebar names it: {text}"
    );
    assert!(
        text.contains("↳ continues Codex · api"),
        "so does the title: {text}"
    );
    app.save_active_sessions().unwrap();
    let saved = agent_mux::persistence::load_saved_sessions(&temp.path().join("sessions.json"));
    assert_eq!(saved[0].profile.args, ["--model", "m"]);
    assert_eq!(saved[0].continued_from.as_deref(), Some("Codex · api"));
}
