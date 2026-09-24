//! The build stamp: `agent-mux --version` from the built binary, and the
//! same facts through the library.

use std::process::Command;

fn version_output(flag: &str) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_agent-mux"))
        .arg(flag)
        .output()
        .expect("the binary runs");
    assert!(out.status.success(), "{flag} exits cleanly");
    assert!(out.stderr.is_empty(), "nothing on stderr: {:?}", out.stderr);
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn every_spelling_of_version_prints_the_same_stamp() {
    let long = version_output("--version");
    assert_eq!(long, version_output("-V"));
    assert_eq!(long, version_output("version"));

    let lines: Vec<&str> = long.lines().collect();
    assert_eq!(
        lines[0],
        format!("agent-mux {}", env!("CARGO_PKG_VERSION")),
        "{long}"
    );
    assert!(lines.iter().any(|l| l.starts_with("built ")), "{long}");
    assert!(lines.iter().any(|l| l.contains("ago,")), "{long}");
    assert!(lines.iter().any(|l| l.ends_with("UTC")), "{long}");
    let branch = lines
        .iter()
        .find(|l| l.starts_with("branch "))
        .expect("the branch line");
    assert!(branch.contains('@'), "{branch}");
    assert!(lines.iter().any(|l| l.starts_with("target ")), "{long}");
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("binary ") && l.contains("agent-mux")),
        "{long}"
    );
}

#[test]
fn the_library_exposes_the_same_stamp() {
    use agent_mux::build_info;
    assert_eq!(build_info::VERSION, env!("CARGO_PKG_VERSION"));
    let built = build_info::built_at().expect("a build timestamp");
    let now = time::OffsetDateTime::now_utc();
    assert!(built <= now, "the build is not in the future");
    assert!(
        (now - built).whole_days() < 3650,
        "the timestamp is a real date"
    );
    let short = build_info::short();
    assert!(short.contains(build_info::VERSION), "{short}");
    assert!(short.contains(build_info::COMMIT), "{short}");
    assert!(short.contains("built "), "{short}");
    // the whole stamp fits the help overlay's 84-column box
    assert!(
        short.chars().count() <= 80,
        "{} chars: {short}",
        short.chars().count()
    );
}

/// The About overlay (`v`) shows the same stamp inside the TUI.
mod overlay {
    use agent_mux::app::{App, Mode};
    use agent_mux::config::Profile;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use std::time::Instant;
    use tokio::sync::mpsc;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn render(app: &App, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|f| agent_mux::ui::draw(f, app, Instant::now()))
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        let mut out = String::new();
        for y in 0..height {
            for x in 0..width {
                out.push_str(buffer[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    #[tokio::test]
    async fn v_opens_about_and_esc_closes_it() {
        let (tx, _rx) = mpsc::channel(32);
        let mut app = App::new(
            vec![Profile {
                name: "Claude Code".into(),
                command: "claude".into(),
                args: vec![],
                default_dir: None,
                tracing: None,
                model: None,
                bypass_approvals: None,
            }],
            None,
            tx,
        );
        app.clipboard_enabled = false;
        app.set_pane_size(30, 110);

        app.handle_key(&key(KeyCode::Char('v')), Instant::now());
        assert!(matches!(app.mode, Mode::About(_)), "v opens About");
        let screen = render(&app, 120, 34);
        assert!(screen.contains("About agent-mux"), "{screen}");
        assert!(
            screen.contains(&format!("agent-mux {}", agent_mux::build_info::VERSION)),
            "{screen}"
        );
        assert!(screen.contains("built"), "{screen}");
        assert!(screen.contains(agent_mux::build_info::COMMIT), "{screen}");
        assert!(screen.contains("branch"), "{screen}");
        // tracing is off in this App, and that is said rather than hidden
        assert!(screen.contains("tracing is off"), "{screen}");
        assert!(screen.contains("[Esc] close"), "{screen}");

        // ? switches to the key reference, which lists the About key itself
        app.handle_key(&key(KeyCode::Char('?')), Instant::now());
        assert!(matches!(app.mode, Mode::Help));
        let help = render(&app, 120, 60);
        assert!(help.contains("about: version, build time"), "{help}");

        app.handle_key(&key(KeyCode::Char('v')), Instant::now());
        app.handle_key(&key(KeyCode::Esc), Instant::now());
        assert!(matches!(app.mode, Mode::Control), "Esc closes About");
    }

    /// Prints the overlay for documentation and eyeballing:
    /// `cargo test --test build_info -- --ignored --nocapture about_preview`.
    #[tokio::test]
    #[ignore = "prints the About overlay instead of asserting"]
    async fn about_preview() {
        let (tx, _rx) = mpsc::channel(8);
        let mut app = App::new(vec![], None, tx);
        app.clipboard_enabled = false;
        app.set_pane_size(32, 106);
        app.open_about();
        println!("{}", render(&app, 114, 36));
    }

    #[tokio::test]
    async fn about_scrolls_on_a_short_terminal() {
        let (tx, _rx) = mpsc::channel(32);
        let mut app = App::new(vec![], None, tx);
        app.clipboard_enabled = false;
        app.set_pane_size(12, 80);
        app.open_about();
        let _ = render(&app, 90, 15); // the renderer reports the viewport
        let Mode::About(state) = &app.mode else {
            panic!("About is open");
        };
        assert!(state.max_scroll() > 0, "the rows do not fit 15 rows");
        app.handle_key(&key(KeyCode::End), Instant::now());
        let Mode::About(state) = &app.mode else {
            panic!()
        };
        let bottom = state.scroll_offset;
        assert_eq!(bottom, state.max_scroll());
        let screen = render(&app, 90, 15);
        assert!(
            screen.contains("Harnesses on PATH") || screen.contains("claude"),
            "{screen}"
        );
        app.handle_key(&key(KeyCode::Home), Instant::now());
        let Mode::About(state) = &app.mode else {
            panic!()
        };
        assert_eq!(state.scroll_offset, 0);
    }
}
