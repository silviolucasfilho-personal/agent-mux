//! The Configuration view (`C`) as the user drives it: grouping, navigation
//! over headers, the detail pane, editing through the editor request,
//! reset, new items, the push into loop workspaces, and the settings
//! reload.

use agent_mux::app::{App, ConfigPane, ConfigRow, Mode, Pending};
use agent_mux::assets::{Kind, Source};
use agent_mux::config::Profile;
use agent_mux::loops::registry;
use agent_mux::loops::{Level, patterns};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use std::path::{Path, PathBuf};
use std::time::Instant;
use tokio::sync::mpsc;

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn profile(name: &str) -> Profile {
    Profile {
        name: name.into(),
        command: "sh".into(),
        args: vec![],
        default_dir: None,
        tracing: None,
        model: None,
        bypass_approvals: None,
    }
}

struct Fixture {
    _temp: tempfile::TempDir,
    library: PathBuf,
    app: App,
}

fn fixture() -> Fixture {
    let temp = tempfile::tempdir().unwrap();
    let library = temp.path().join("library");
    std::fs::create_dir_all(&library).unwrap();
    let (tx, _rx) = mpsc::channel(32);
    let mut app = App::new(vec![profile("Claude Code")], None, tx);
    app.clipboard_enabled = false;
    app.set_pane_size(30, 120);
    app.library_root = Some(library.clone());
    app.skills_dir = Some(library.join("skills"));
    app.loops_file = Some(temp.path().join("loops.json"));
    app.runtime_dir = Some(temp.path().join("runtime"));
    app.editor = Some("true".into());
    app.load_loop_registry();
    Fixture {
        _temp: temp,
        library,
        app,
    }
}

fn screen(app: &App) -> String {
    let backend = TestBackend::new(120, 30);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|f| agent_mux::ui::draw(f, app, Instant::now()))
        .unwrap();
    let buffer = terminal.backend().buffer().clone();
    let mut text = String::new();
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            text.push_str(buffer[(x, y)].symbol());
        }
        text.push('\n');
    }
    text
}

fn press(app: &mut App, code: KeyCode) {
    app.handle_key(&key(code), Instant::now());
}

fn type_text(app: &mut App, text: &str) {
    for c in text.chars() {
        press(app, KeyCode::Char(c));
    }
}

fn view(app: &App) -> &agent_mux::app::ConfigViewState {
    match &app.mode {
        Mode::ConfigView(v) => v,
        other => panic!("not in the Configuration view: {other:?}"),
    }
}

fn select(app: &mut App, query: &str) {
    let id = view(app).catalog.find(query).unwrap().id.clone();
    let pos = view(app)
        .rows
        .iter()
        .position(|r| matches!(r, ConfigRow::Item(i) if view(app).catalog.assets[*i].id == id))
        .unwrap();
    // walk from the top so navigation itself is exercised
    press(app, KeyCode::Home);
    while view(app).selected != 0 {
        press(app, KeyCode::Up);
        if view(app).selected == 1 {
            break;
        }
    }
    while view(app).selected < pos {
        press(app, KeyCode::Down);
    }
    assert_eq!(view(app).selected_asset().unwrap().id, id);
}

fn register_loop(f: &mut Fixture, ws: &Path) {
    std::fs::create_dir_all(ws).unwrap();
    let p = patterns::find("daily-triage").unwrap();
    let entry = registry::new_entry(
        ws,
        p,
        "claude",
        "Claude Code",
        p.default_interval_s,
        Level::L1,
        agent_mux::loops::now(),
    );
    f.app.loop_registry.add(entry);
    f.app.save_loop_registry().unwrap();
}

#[test]
fn c_opens_the_view_grouped_by_kind_and_esc_unwinds() {
    let mut f = fixture();
    press(&mut f.app, KeyCode::Char('C'));
    let v = view(&f.app);
    assert_eq!(v.item_count(), 26);
    assert!(matches!(v.rows[0], ConfigRow::Header(Kind::Prompts)));
    assert_eq!(v.selected, 1, "the first item, never a header");
    assert_eq!(v.selected_asset().unwrap().id, "prompts.toml");
    let headers: Vec<Kind> = v
        .rows
        .iter()
        .filter_map(|r| match r {
            ConfigRow::Header(k) => Some(*k),
            _ => None,
        })
        .collect();
    assert_eq!(headers, Kind::ALL.to_vec());

    let text = screen(&f.app);
    assert!(text.contains("Configuration (26)"), "{text}");
    for h in [
        "Prompts",
        "Settings",
        "Skills",
        "Loop patterns",
        "Loop skills",
    ] {
        assert!(text.contains(h), "{h}\n{text}");
    }
    assert!(
        text.contains("built-in (compiled into agent-mux)"),
        "{text}"
    );
    assert!(text.contains("[Enter/e] edit"), "{text}");
    assert!(
        text.contains("{invocation}"),
        "the effective prompts text is shown\n{text}"
    );

    // down skips the Settings header straight to profiles.toml
    press(&mut f.app, KeyCode::Down);
    assert_eq!(view(&f.app).selected_asset().unwrap().id, "profiles.toml");
    press(&mut f.app, KeyCode::Up);
    press(&mut f.app, KeyCode::Up);
    assert_eq!(
        view(&f.app).selected_asset().unwrap().id,
        "prompts.toml",
        "no wrap"
    );

    press(&mut f.app, KeyCode::Right);
    assert_eq!(view(&f.app).focus, ConfigPane::Detail);
    press(&mut f.app, KeyCode::Down);
    assert_eq!(view(&f.app).scroll_offset, 1);
    press(&mut f.app, KeyCode::Esc);
    assert_eq!(view(&f.app).focus, ConfigPane::List);
    press(&mut f.app, KeyCode::Esc);
    assert!(matches!(f.app.mode, Mode::Control));
}

#[test]
fn enter_creates_the_override_and_requests_the_editor_then_the_edit_is_applied() {
    let mut f = fixture();
    press(&mut f.app, KeyCode::Char('C'));
    select(&mut f.app, "loop-rules");
    assert_eq!(
        view(&f.app).selected_asset().unwrap().source,
        Source::Builtin
    );
    press(&mut f.app, KeyCode::Enter);
    let req = f.app.take_editor_request().expect("an editor request");
    assert_eq!(req.path, f.library.join("loops/skills/loop-rules/SKILL.md"));
    assert_eq!(req.asset_id, "loops/skills/loop-rules/SKILL.md");
    assert_eq!(req.command, vec!["true".to_string()]);
    assert!(req.path.is_file(), "the built-in text was copied");
    assert!(f.app.take_editor_request().is_none(), "taken once");

    // the user edits the file; the view learns about it on return
    std::fs::write(
        &req.path,
        "---\nname: loop-rules\ndescription: mine\n---\nmy rules\n",
    )
    .unwrap();
    f.app.editor_finished(req, Ok(()));
    let v = view(&f.app);
    let a = v.selected_asset().unwrap();
    assert_eq!(a.id, "loops/skills/loop-rules/SKILL.md", "selection kept");
    assert_eq!(a.source, Source::Override);
    assert!(a.valid());
    assert!(
        f.app.notice.as_ref().unwrap().text.contains("saved"),
        "{:?}",
        f.app.notice
    );
    let text = screen(&f.app);
    assert!(text.contains("my rules"), "{text}");
    assert!(text.contains("override"), "{text}");

    // a broken edit stays on disk and is reported in the row and the notice
    let req = {
        press(&mut f.app, KeyCode::Char('e'));
        f.app.take_editor_request().unwrap()
    };
    std::fs::write(&req.path, "---\nname: other\n---\n").unwrap();
    f.app.editor_finished(req, Ok(()));
    let a = view(&f.app).selected_asset().unwrap();
    assert!(!a.valid());
    assert!(
        f.app
            .notice
            .as_ref()
            .unwrap()
            .text
            .contains("frontmatter name")
    );
    let text = screen(&f.app);
    assert!(text.contains("problem(s)"), "{text}");

    // an editor failure is reported and nothing is reverted
    press(&mut f.app, KeyCode::Enter);
    let req = f.app.take_editor_request().unwrap();
    f.app.editor_finished(req, Err("vi exited with 1".into()));
    assert!(
        f.app
            .notice
            .as_ref()
            .unwrap()
            .text
            .contains("editor: vi exited")
    );
    assert!(f.library.join("loops/skills/loop-rules/SKILL.md").is_file());
}

#[test]
fn reset_asks_first_and_restores_the_builtin_text() {
    let mut f = fixture();
    press(&mut f.app, KeyCode::Char('C'));
    select(&mut f.app, "loop-fix");
    press(&mut f.app, KeyCode::Char('R'));
    assert_eq!(view(&f.app).pending, Pending::None, "nothing to reset");
    assert!(f.app.notice.as_ref().unwrap().text.contains("already uses"));

    press(&mut f.app, KeyCode::Enter);
    let req = f.app.take_editor_request().unwrap();
    f.app.editor_finished(req, Ok(()));
    assert_eq!(
        view(&f.app).selected_asset().unwrap().source,
        Source::Override
    );

    press(&mut f.app, KeyCode::Char('R'));
    assert_eq!(view(&f.app).pending, Pending::Reset);
    assert!(
        screen(&f.app).contains("Reset loops/skills/loop-fix/SKILL.md to the built-in text? [y/n]")
    );
    press(&mut f.app, KeyCode::Char('n'));
    assert_eq!(view(&f.app).pending, Pending::None);
    assert_eq!(
        view(&f.app).selected_asset().unwrap().source,
        Source::Override
    );
    press(&mut f.app, KeyCode::Char('R'));
    press(&mut f.app, KeyCode::Char('y'));
    assert_eq!(
        view(&f.app).selected_asset().unwrap().source,
        Source::Builtin
    );
    assert!(!f.library.join("loops/skills/loop-fix").exists());
    assert!(f.app.notice.as_ref().unwrap().text.contains("reset"));

    select(&mut f.app, "profiles.toml");
    press(&mut f.app, KeyCode::Char('R'));
    assert_eq!(view(&f.app).pending, Pending::None);
    assert!(
        f.app
            .notice
            .as_ref()
            .unwrap()
            .text
            .contains("never deleted")
    );
}

#[test]
fn n_creates_a_new_loop_skill_and_opens_it() {
    let mut f = fixture();
    press(&mut f.app, KeyCode::Char('C'));
    select(&mut f.app, "prompts.toml");
    press(&mut f.app, KeyCode::Char('n'));
    assert_eq!(view(&f.app).pending, Pending::None);
    assert!(
        f.app
            .notice
            .as_ref()
            .unwrap()
            .text
            .contains("n creates skills")
    );

    select(&mut f.app, "loop-triage");
    press(&mut f.app, KeyCode::Char('n'));
    assert!(matches!(
        view(&f.app).pending,
        Pending::NewName {
            kind: Kind::LoopSkill,
            ..
        }
    ));
    type_text(&mut f.app, "loop-docsx");
    press(&mut f.app, KeyCode::Backspace);
    assert!(screen(&f.app).contains("New loop skill name: loop-docs_"));
    press(&mut f.app, KeyCode::Enter);
    let req = f.app.take_editor_request().expect("opens the new file");
    assert_eq!(req.path, f.library.join("loops/skills/loop-docs/SKILL.md"));
    let v = view(&f.app);
    assert_eq!(v.pending, Pending::None);
    let a = v.selected_asset().unwrap();
    assert_eq!(
        a.id, "loops/skills/loop-docs/SKILL.md",
        "selected after creation"
    );
    assert_eq!(a.source, Source::User);
    assert!(a.valid(), "{:?}", a.problems);
    assert_eq!(v.item_count(), 27);
    f.app.editor_finished(req, Ok(()));

    // a bad name is refused and the footer question closes
    press(&mut f.app, KeyCode::Char('n'));
    type_text(&mut f.app, "Bad Name");
    press(&mut f.app, KeyCode::Enter);
    assert!(
        f.app
            .notice
            .as_ref()
            .unwrap()
            .text
            .contains("not a valid name")
    );
    assert!(f.app.take_editor_request().is_none());

    // Esc cancels the question without leaving the view
    press(&mut f.app, KeyCode::Char('n'));
    press(&mut f.app, KeyCode::Esc);
    assert_eq!(view(&f.app).pending, Pending::None);
    assert!(matches!(f.app.mode, Mode::ConfigView(_)));

    // a new skill package appears in the Agents sidebar list too
    select(&mut f.app, "skills/heimdall/SKILL.md");
    press(&mut f.app, KeyCode::Char('n'));
    type_text(&mut f.app, "my-notes");
    press(&mut f.app, KeyCode::Enter);
    let req = f.app.take_editor_request().unwrap();
    f.app.editor_finished(req, Ok(()));
    assert!(
        f.app.skills.iter().any(|s| s.id == "my-notes"),
        "reload_skills ran"
    );
}

#[test]
fn u_pushes_edited_loop_skills_into_registered_workspaces() {
    let mut f = fixture();
    let ws = f.library.parent().unwrap().join("proj");
    register_loop(&mut f, &ws);
    let skills = ws.join(".claude/skills");
    std::fs::create_dir_all(skills.join("loop-rules")).unwrap();
    std::fs::write(skills.join("loop-rules/SKILL.md"), "old\n").unwrap();

    press(&mut f.app, KeyCode::Char('C'));
    select(&mut f.app, "loop-rules");
    let v = view(&f.app);
    assert_eq!(v.copies.len(), 1);
    assert_eq!(v.stale_copies(), 1);
    let text = screen(&f.app);
    assert!(
        text.contains("1 workspace copy(ies), 1 differ or missing (u pushes)"),
        "{text}"
    );
    assert!(text.contains("differs"), "{text}");

    press(&mut f.app, KeyCode::Char('u'));
    assert_eq!(view(&f.app).pending, Pending::Push);
    press(&mut f.app, KeyCode::Char('y'));
    assert_eq!(view(&f.app).pending, Pending::None);
    assert!(
        f.app.notice.as_ref().unwrap().text.contains("pushed"),
        "{:?}",
        f.app.notice
    );
    assert!(
        std::fs::read_to_string(skills.join("loop-rules/SKILL.md"))
            .unwrap()
            .starts_with("---\nname: loop-rules"),
        "the workspace copy now holds the effective text"
    );
    assert!(
        skills.join("loop-triage/SKILL.md").is_file(),
        "every listed skill"
    );
    assert!(
        !ws.join(".claude/agents/loop-verifier.md").exists(),
        "daily-triage has no verifier"
    );
    assert_eq!(view(&f.app).stale_copies(), 0);
    assert!(screen(&f.app).contains("all current"));
}

#[test]
fn editing_settings_reloads_profiles_and_the_editor() {
    let mut f = fixture();
    let settings = f.library.join("profiles.toml");
    f.app.config_path = Some(settings.clone());
    std::fs::write(
        &settings,
        "[[profiles]]\nname = \"Claude Code\"\ncommand = \"claude\"\n",
    )
    .unwrap();
    press(&mut f.app, KeyCode::Char('C'));
    select(&mut f.app, "profiles.toml");
    assert_eq!(view(&f.app).selected_asset().unwrap().source, Source::User);
    press(&mut f.app, KeyCode::Enter);
    let req = f.app.take_editor_request().unwrap();
    assert_eq!(req.path, settings);
    std::fs::write(
        &settings,
        "editor = \"code --wait\"\n\n[[profiles]]\nname = \"Mine\"\ncommand = \"sh\"\n\n[[profiles]]\nname = \"Other\"\ncommand = \"sh\"\n\n[loops]\nmax_concurrent = 3\n",
    )
    .unwrap();
    f.app.editor_finished(req, Ok(()));
    assert_eq!(f.app.profiles.len(), 2);
    assert_eq!(f.app.profiles[0].name, "Mine");
    assert_eq!(f.app.editor.as_deref(), Some("code --wait"));
    assert_eq!(f.app.loops.max_concurrent, 3);
    press(&mut f.app, KeyCode::Enter);
    assert_eq!(
        f.app.take_editor_request().unwrap().command,
        vec!["code".to_string(), "--wait".to_string()]
    );

    // a settings file that no longer parses is reported and the old
    // profiles stay
    press(&mut f.app, KeyCode::Enter);
    let req = f.app.take_editor_request().unwrap();
    std::fs::write(&settings, "editor = [\n").unwrap();
    f.app.editor_finished(req, Ok(()));
    assert_eq!(f.app.profiles.len(), 2);
    assert!(
        f.app
            .notice
            .as_ref()
            .unwrap()
            .text
            .contains("profiles.toml")
    );
    assert!(!view(&f.app).selected_asset().unwrap().valid());
}

#[test]
fn the_help_and_hints_mention_the_view() {
    let mut f = fixture();
    let text = screen(&f.app);
    assert!(text.contains("[C] config"), "{text}");
    press(&mut f.app, KeyCode::Char('?'));
    let text = screen(&f.app);
    assert!(text.contains("configuration: edit every prompt"), "{text}");
}
