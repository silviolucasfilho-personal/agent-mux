use agent_mux::app::{App, Focus, Mode};
use agent_mux::keymap::ActionId;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::time::Instant;
use tokio::sync::mpsc;

fn app() -> App {
    let (tx, _) = mpsc::channel(64);
    App::new(vec![], None, tx)
}
fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

#[test]
fn command_availability_tracks_current_context() {
    let mut a = app();
    assert!(!a.command_availability(ActionId::ReturnToSession).enabled());
    assert!(!a.command_availability(ActionId::Save).enabled());
    a.sidebar_hidden = true;
    a.execute_command(ActionId::FocusAgents, Instant::now());
    assert!(!a.sidebar_hidden);
    assert_eq!(a.focus, Focus::List);
    a.execute_command(ActionId::FocusAgents, Instant::now());
    assert!(!a.sidebar_hidden);
    assert!(matches!(a.mode, Mode::Main));
}

#[test]
fn commands_open_on_every_screen_and_preserve_search() {
    let mut a = app();
    a.search = Some(agent_mux::search::SearchState::new());
    a.search.as_mut().unwrap().query = "preserve this".into();
    a.handle_key(&key(KeyCode::F(1)), Instant::now());
    assert!(matches!(a.mode, Mode::Main));
    assert!(a.command_overlay.is_some());
    a.handle_key(&key(KeyCode::Esc), Instant::now());
    assert!(a.command_overlay.is_none());
    assert_eq!(a.search.as_ref().unwrap().query, "preserve this");
    a.search = None;
    a.execute_command(ActionId::Runs, Instant::now());
    a.handle_key(&key(KeyCode::F(1)), Instant::now());
    assert!(a.command_overlay.is_some());
    assert!(matches!(a.mode, Mode::RunsView(_)));
    a.handle_key(&key(KeyCode::Esc), Instant::now());
    assert!(matches!(a.mode, Mode::RunsView(_)));
}

#[test]
fn disabled_command_cannot_execute() {
    let mut a = app();
    a.execute_command(ActionId::ReturnToSession, Instant::now());
    assert!(matches!(a.mode, Mode::Main));
    assert!(a.notice.as_ref().unwrap().text.contains("No session"));
}

#[test]
fn dirty_navigation_cancel_preserves_editor() {
    let mut a = app();
    a.new_persona_editor("blank", "keyboard-test");
    assert!(matches!(a.mode, Mode::AgentEditor(_)));
    a.execute_command(ActionId::FocusAgents, Instant::now());
    assert!(
        matches!(a.mode, Mode::AgentEditor(_)),
        "dirty editor must not be discarded"
    );
    assert!(a.command_overlay.is_some());
    a.handle_key(&key(KeyCode::Esc), Instant::now());
    assert!(matches!(a.mode, Mode::AgentEditor(_)));
    assert!(a.command_overlay.is_none());
    a.execute_command(ActionId::FocusAgents, Instant::now());
    a.handle_key(&key(KeyCode::Char('y')), Instant::now());
    assert!(matches!(a.mode, Mode::Main));
    assert_eq!(a.focus, Focus::List);
}

#[test]
fn hidden_sidebar_transfers_focus() {
    let mut a = app();
    a.focus = Focus::List;
    a.execute_command(ActionId::ToggleSidebar, Instant::now());
    assert!(a.sidebar_hidden);
    assert_eq!(a.focus, Focus::Pane);
}

#[test]
fn tab_moves_focus_not_tabs() {
    use agent_mux::app::agent_editor::Tab;
    let mut a = app();
    a.new_persona_editor("blank", "keyboard-test");
    if let Mode::AgentEditor(st) = &mut a.mode {
        st.set_tab(Tab::Who);
        st.field = 0;
    }
    a.handle_key(&key(KeyCode::Tab), Instant::now());
    let Mode::AgentEditor(st) = &a.mode else {
        panic!("editor closed")
    };
    assert_eq!(st.tab, Tab::Who);
    assert_eq!(st.field, 1);
    a.handle_key(&key(KeyCode::Char('5')), Instant::now());
    let Mode::AgentEditor(st) = &a.mode else {
        panic!("editor closed")
    };
    assert_eq!(st.tab, Tab::Review);
}

#[test]
fn multiline_enter_inserts_singleline_enter_submits() {
    use agent_mux::app::agent_editor::{EdField, Tab};
    use agent_mux::app::flow_builder::AgentField;
    let mut a = app();
    a.new_persona_editor("blank", "keyboard-test");
    if let Mode::AgentEditor(st) = &mut a.mode {
        st.set_tab(Tab::What);
        st.field = st
            .fields()
            .iter()
            .position(|f| *f == EdField::Agent(AgentField::Instructions))
            .unwrap();
        st.editing = true;
        if let agent_mux::app::agent_editor::Body::Persona(p) = &mut st.body {
            p.task_field = None;
            p.form.field = AgentField::Instructions;
        }
        st.text_mut().unwrap().set("hello");
    }
    a.handle_key(&key(KeyCode::Enter), Instant::now());
    let Mode::AgentEditor(st) = &mut a.mode else {
        panic!("editor closed")
    };
    assert!(st.editing);
    assert!(st.text_mut().unwrap().text.contains('\n'));
    a.handle_key(&key(KeyCode::Esc), Instant::now());
    let Mode::AgentEditor(st) = &mut a.mode else {
        panic!("editor closed")
    };
    assert!(!st.editing);
    st.set_tab(Tab::Who);
    st.field = 0;
    st.editing = true;
    a.handle_key(&key(KeyCode::Enter), Instant::now());
    let Mode::AgentEditor(st) = &a.mode else {
        panic!("editor closed")
    };
    assert!(!st.editing);
}

#[test]
fn editor_arrows_do_not_scroll_session_or_interpret_modified_letters() {
    let mut a = app();
    a.execute_command(ActionId::Runs, Instant::now());
    let Mode::RunsView(st) = &a.mode else {
        panic!("runs not open")
    };
    let selected = st.selected;
    a.handle_key(
        &KeyEvent::new(KeyCode::Char('d'), KeyModifiers::ALT),
        Instant::now(),
    );
    let Mode::RunsView(st) = &a.mode else {
        panic!("modified key navigated")
    };
    assert_eq!(st.selected, selected);
    assert!(!st.confirm_discard);
}

#[test]
fn probe_does_not_execute_actions_or_verify_collapsed_ctrl() {
    let mut a = app();
    a.execute_command(ActionId::TestShortcuts, Instant::now());
    assert!(a.shortcut_probe.is_some());
    a.handle_key(&key(KeyCode::F(1)), Instant::now());
    assert!(a.command_overlay.is_none());
    a.handle_key(
        &KeyEvent::new(KeyCode::Char('l'), KeyModifiers::CONTROL),
        Instant::now(),
    );
    assert!(a.shortcuts.preferred_label(ActionId::FocusAgents).is_none());
    a.handle_key(
        &KeyEvent::new(
            KeyCode::Char('l'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        ),
        Instant::now(),
    );
    assert_eq!(
        a.shortcuts
            .preferred_label(ActionId::FocusAgents)
            .as_deref(),
        Some("Ctrl+Shift+L")
    );
    let hidden = a.sidebar_hidden;
    a.handle_key(
        &KeyEvent::new(
            KeyCode::Char('b'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        ),
        Instant::now(),
    );
    assert_eq!(a.sidebar_hidden, hidden);
    a.handle_key(&key(KeyCode::Esc), Instant::now());
    assert!(a.shortcut_probe.is_none());
    assert_eq!(
        a.shortcuts
            .preferred_label(ActionId::FocusAgents)
            .as_deref(),
        Some("Ctrl+Shift+L")
    );
}

fn screen(a: &App, width: u16) -> String {
    let backend = ratatui::backend::TestBackend::new(width, 24);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal
        .draw(|f| agent_mux::ui::draw(f, a, Instant::now()))
        .unwrap();
    let b = terminal.backend().buffer();
    (0..24)
        .map(|y| (0..width).map(|x| b[(x, y)].symbol()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn footer_uses_current_context_and_narrow_footer_keeps_commands_and_focus() {
    let mut a = app();
    let text = screen(&a, 40);
    assert!(text.contains("F1 Commands"));
    assert!(text.contains("Keyboard focus"));
    a.execute_command(ActionId::Runs, Instant::now());
    let text = screen(&a, 100);
    assert!(text.contains("F1 Commands"));
    assert!(!text.lines().last().unwrap().contains("sidebar"));
}

#[test]
fn footer_click_executes_same_action() {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    let mut a = app();
    screen(&a, 80);
    a.handle_mouse(
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 2,
            row: 23,
            modifiers: KeyModifiers::NONE,
        },
        Instant::now(),
    );
    assert!(a.command_overlay.is_some());
    assert!(matches!(a.mode, Mode::Main));
}

#[test]
fn compose_task_enter_inserts_a_newline() {
    use agent_mux::app::workflows_view::WorkflowDialogState;
    let mut a = app();
    a.mode = Mode::WorkflowDialog(Box::new(WorkflowDialogState::for_plan(&[], vec![])));
    a.handle_key(&key(KeyCode::Enter), Instant::now());
    let Mode::WorkflowDialog(d) = &a.mode else {
        panic!("task Enter submitted the dialog")
    };
    assert_eq!(d.task.text, "\n");
}

#[test]
fn empty_selection_does_not_offer_item_actions() {
    let a = app();
    for id in [
        ActionId::Edit,
        ActionId::Delete,
        ActionId::Run,
        ActionId::Stop,
    ] {
        assert!(
            !a.command_availability(id).enabled(),
            "empty selection offered {id:?}"
        );
    }
}

#[test]
fn commands_save_editor_without_typing_an_s() {
    let temp = tempfile::tempdir().unwrap();
    let mut a = app();
    a.library_root = Some(temp.path().to_path_buf());
    a.new_persona_editor("blank", "keyboard-save");
    let Mode::AgentEditor(st) = &mut a.mode else {
        panic!("editor")
    };
    st.set_tab(agent_mux::app::agent_editor::Tab::Who);
    st.editing = true;
    st.text_mut().unwrap().set("keyboard-save");
    a.execute_command(ActionId::Save, Instant::now());
    let Mode::AgentEditor(st) = &mut a.mode else {
        panic!("save closed editor")
    };
    assert!(!st.dirty);
    assert!(!st.editing);
    let file = temp.path().join("agents/keyboard-save.toml");
    let text = std::fs::read_to_string(file).unwrap();
    assert!(text.contains("keyboard-save"));
    assert!(!text.contains("keyboard-saves"));
}

#[test]
fn accepted_navigation_releases_pane_search() {
    let mut a = app();
    a.search = Some(agent_mux::search::SearchState::new());
    a.execute_command(ActionId::Runs, Instant::now());
    assert!(matches!(a.mode, Mode::RunsView(_)));
    assert!(a.search.is_none());
}

#[test]
fn pasted_commands_do_not_reach_underlying_screen() {
    let mut a = app();
    a.focus = Focus::List;
    a.handle_key(&key(KeyCode::F(1)), Instant::now());
    a.handle_paste("open runs");
    assert_eq!(a.focus, Focus::List);
    let Some(agent_mux::app::commands::Overlay::Commands(m)) = &a.command_overlay else {
        panic!("commands")
    };
    assert_eq!(m.query, "open runs");
}

#[test]
fn configuration_keeps_focus_in_a_24_column_footer() {
    let mut a = app();
    a.execute_command(ActionId::Configuration, Instant::now());
    let text = screen(&a, 24);
    let footer = text.lines().last().unwrap();
    assert!(footer.contains("[F1]"), "{footer}");
    assert!(footer.contains("Configuration"), "{footer}");
}

#[test]
fn search_query_stays_visible_in_footer() {
    let mut a = app();
    a.search = Some(agent_mux::search::SearchState::new());
    a.search.as_mut().unwrap().query = "needle".into();
    assert!(screen(&a, 100).contains("needle"));
}

#[test]
fn unsupported_ctrl_letter_does_not_become_a_plain_verb() {
    let mut a = app();
    a.focus = Focus::List;
    a.handle_key(
        &KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL),
        Instant::now(),
    );
    assert!(matches!(a.mode, Mode::Main));
}

#[test]
fn destructive_confirmation_requires_y() {
    let mut a = app();
    a.mode = Mode::ConfirmQuit;
    a.handle_key(&key(KeyCode::Enter), Instant::now());
    assert!(!a.should_quit);
    assert!(matches!(a.mode, Mode::ConfirmQuit));
    a.handle_key(&key(KeyCode::Char('y')), Instant::now());
    assert!(a.should_quit);
}

#[test]
fn help_rows_use_the_shared_theme_sections() {
    use agent_mux::app::commands::{HELP_INTRO, HelpRow, help_row_count, help_rows};
    use agent_mux::keymap::{ACTIONS, ActionId, ActionTheme};

    let rows = help_rows();
    assert_eq!(help_row_count(), rows.len());
    let (intro, mut rest) = rows.split_at(HELP_INTRO.len());
    for (row, line) in intro.iter().zip(HELP_INTRO) {
        match row {
            HelpRow::Intro(got) => assert_eq!(*got, line),
            other => panic!("intro line expected, got {other:?}"),
        }
    }

    // The sections are the Commands overlay's, in the same order, with every
    // action under its own theme and registration order inside it.
    let mut listed: Vec<ActionId> = Vec::new();
    for theme in ActionTheme::ALL.iter().copied() {
        let want: Vec<ActionId> = ACTIONS
            .iter()
            .filter(|action| action.theme == theme)
            .map(|action| action.id)
            .collect();
        if want.is_empty() {
            continue;
        }
        match rest.first() {
            Some(HelpRow::Header(got)) => assert_eq!(*got, theme),
            other => panic!("{theme:?} header expected, got {other:?}"),
        }
        rest = &rest[1..];
        for id in want {
            match rest.first() {
                Some(HelpRow::Action(action)) => assert_eq!(action.id, id),
                other => panic!("{id:?} expected under {theme:?}, got {other:?}"),
            }
            listed.push(id);
            rest = &rest[1..];
        }
    }
    assert!(rest.is_empty(), "unexpected trailing rows: {rest:?}");
    assert_eq!(listed.len(), ACTIONS.len());
    for action in ACTIONS {
        assert_eq!(
            listed.iter().filter(|id| **id == action.id).count(),
            1,
            "{:?} listed once",
            action.id
        );
    }
}

fn help_selection(a: &App) -> usize {
    let Some(agent_mux::app::commands::Overlay::Help(selected)) = &a.command_overlay else {
        panic!("help not open")
    };
    *selected
}

#[test]
fn help_scrolls_its_whole_row_list_and_stops_at_the_edges() {
    use agent_mux::app::commands::help_row_count;
    use agent_mux::keymap::{ACTIONS, ActionTheme};

    let mut a = app();
    a.execute_command(ActionId::Help, Instant::now());
    let last = help_row_count() - 1;

    // Intro lines come first; the cursor cannot scroll above them.
    assert_eq!(help_selection(&a), 0);
    let text = screen(&a, 100);
    assert!(text.contains("F1 Commands"), "{text}");
    assert!(text.contains(" Navigation "), "{text}");
    a.handle_key(&key(KeyCode::Up), Instant::now());
    a.handle_key(&key(KeyCode::PageUp), Instant::now());
    assert_eq!(help_selection(&a), 0);

    // End reaches the last row and Down/PageDown stop there.
    a.handle_key(&key(KeyCode::End), Instant::now());
    assert_eq!(help_selection(&a), last);
    let text = screen(&a, 100);
    assert!(text.contains("Quit agent-mux"), "{text}");
    assert!(text.contains(" App "), "{text}");
    a.handle_key(&key(KeyCode::Down), Instant::now());
    a.handle_key(&key(KeyCode::PageDown), Instant::now());
    assert_eq!(help_selection(&a), last);

    // Paging down from Home reaches the same last row.
    a.handle_key(&key(KeyCode::Home), Instant::now());
    assert_eq!(help_selection(&a), 0);
    for _ in 0..last / 10 + 2 {
        a.handle_key(&key(KeyCode::PageDown), Instant::now());
    }
    assert_eq!(help_selection(&a), last);

    // Every header is drawn, in ActionTheme::ALL order, as the list scrolls.
    // Help is a reader, so a header may scroll under the cursor.
    let want: Vec<ActionTheme> = ActionTheme::ALL
        .iter()
        .copied()
        .filter(|theme| ACTIONS.iter().any(|action| action.theme == *theme))
        .collect();
    let mut seen: Vec<ActionTheme> = Vec::new();
    a.handle_key(&key(KeyCode::Home), Instant::now());
    for step in 0..=last {
        let text = screen(&a, 100);
        for theme in want.iter().copied() {
            if text.contains(&format!(" {} ", theme.title())) && !seen.contains(&theme) {
                seen.push(theme);
            }
        }
        let selected = help_selection(&a);
        assert_eq!(selected, step.min(last));
        a.handle_key(&key(KeyCode::Down), Instant::now());
    }
    assert_eq!(seen, want);
    assert_eq!(help_selection(&a), last);

    a.handle_key(&key(KeyCode::Esc), Instant::now());
    assert!(a.command_overlay.is_none());
}

#[test]
fn overlays_preserve_each_screen_and_editor_buffers() {
    let mut a = app();
    for action in [
        ActionId::Runs,
        ActionId::Traces,
        ActionId::Skills,
        ActionId::Configuration,
        ActionId::Inbox,
        ActionId::History,
    ] {
        a.execute_command(action, Instant::now());
        let before = std::mem::discriminant(&a.mode);
        for overlay in [ActionId::Commands, ActionId::Help] {
            a.execute_command(overlay, Instant::now());
            a.handle_key(&key(KeyCode::Esc), Instant::now());
            assert_eq!(std::mem::discriminant(&a.mode), before, "{action:?}");
        }
    }
    a.mode = Mode::TraceBrowser(Box::new(agent_mux::app::TraceBrowserState::new(None, None)));
    a.handle_key(&key(KeyCode::Char('/')), Instant::now());
    a.handle_key(&key(KeyCode::Char('a')), Instant::now());
    a.handle_key(&key(KeyCode::F(1)), Instant::now());
    a.handle_key(&key(KeyCode::Esc), Instant::now());
    let Mode::TraceBrowser(b) = &a.mode else {
        panic!("traces")
    };
    assert_eq!(b.search_input.as_deref(), Some("a"));
    assert!(screen(&a, 100).contains("Search: a"));
}

#[test]
fn probe_skip_cancel_preserves_only_observed_preferences() {
    let mut a = app();
    a.execute_command(ActionId::TestShortcuts, Instant::now());
    a.handle_key(&key(KeyCode::F(1)), Instant::now());
    a.handle_key(&key(KeyCode::Char(' ')), Instant::now());
    a.handle_key(&key(KeyCode::Esc), Instant::now());
    assert!(a.shortcut_probe.is_none());
    assert_eq!(
        a.shortcuts.preferred_label(ActionId::Commands).as_deref(),
        Some("F1")
    );
    assert!(a.shortcuts.preferred_label(ActionId::FocusAgents).is_none());
    assert!(
        a.shortcuts
            .preferred_label(ActionId::ToggleSidebar)
            .is_none()
    );
}

#[tokio::test]
async fn return_restores_origin_and_rechecks_removed_sessions() {
    use agent_mux::session::Session;
    let (tx, _rx) = mpsc::channel(256);
    let mut a = App::new(vec![], None, tx.clone());
    for id in [11, 22] {
        a.sessions.push(
            Session::spawn(
                id,
                super::scroll_ux::shell_profile(),
                std::env::temp_dir(),
                24,
                80,
                tx.clone(),
                &[],
                &[],
            )
            .unwrap(),
        );
    }
    a.selected = 0;
    a.focus = Focus::Pane;
    a.execute_command(ActionId::Runs, Instant::now());
    a.selected = 1;
    a.execute_command(ActionId::ReturnToSession, Instant::now());
    assert_eq!(a.sessions[a.selected].id, 11);
    assert_eq!(a.focus, Focus::Pane);
    a.execute_command(ActionId::Commands, Instant::now());
    a.kill_all();
    a.sessions.clear();
    assert!(!a.command_availability(ActionId::ReturnToSession).enabled());
    a.execute_command(ActionId::ReturnToSession, Instant::now());
    assert!(a.command_overlay.is_some());
}

#[test]
fn save_is_unavailable_in_traces_and_never_sets_verdict() {
    let mut a = app();
    a.mode = Mode::TraceBrowser(Box::new(agent_mux::app::TraceBrowserState::new(None, None)));
    assert!(!a.command_availability(ActionId::Save).enabled());
    a.handle_key(&key(KeyCode::Char('s')), Instant::now());
    assert!(matches!(a.mode, Mode::TraceBrowser(_)));
    assert!(
        a.notice
            .as_ref()
            .unwrap()
            .text
            .to_lowercase()
            .contains("unavailable")
    );
}

#[test]
fn paging_aliases_still_work_in_application_readers() {
    use agent_mux::app::markdown_view::MarkdownView;
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("long.md");
    std::fs::write(
        &path,
        (0..200)
            .map(|i| format!("line {i}\n\n"))
            .collect::<String>(),
    )
    .unwrap();
    let mut a = app();
    a.mode = Mode::Markdown(Box::new(MarkdownView::open(&path).unwrap()));
    screen(&a, 80);
    a.handle_key(
        &KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL),
        Instant::now(),
    );
    let Mode::Markdown(v) = &a.mode else {
        panic!("Markdown")
    };
    assert!(v.scroll > 0);
    a.handle_key(
        &KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL),
        Instant::now(),
    );
    let Mode::Markdown(v) = &a.mode else {
        panic!("Markdown")
    };
    assert_eq!(v.scroll, 0);
    a.execute_command(ActionId::Commands, Instant::now());
    a.handle_key(&key(KeyCode::Esc), Instant::now());
    assert!(matches!(a.mode, Mode::Markdown(_)));
    a.open_runs_view();
    let Mode::RunsView(v) = &mut a.mode else {
        panic!("Runs")
    };
    v.max_scroll.set(100);
    a.handle_key(
        &KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL),
        Instant::now(),
    );
    let Mode::RunsView(v) = &a.mode else {
        panic!("Runs")
    };
    assert_eq!(v.scroll, 10);
}

#[test]
fn commands_list_each_shortcut_beside_its_command() {
    let mut a = app();
    a.handle_key(&key(KeyCode::F(1)), Instant::now());
    let text = screen(&a, 100);
    let row = |label: &str| {
        text.lines()
            .find(|l| l.contains(label))
            .unwrap_or_else(|| panic!("no {label} row in\n{text}"))
            .to_string()
    };
    let chord = |mac: &str, other: &str| {
        if agent_mux::keymap::MACOS {
            mac.to_string()
        } else {
            other.to_string()
        }
    };
    // a candidate chord shows before Test shortcuts, right-aligned
    let focus = row("Focus Agents");
    assert!(
        focus
            .trim_end_matches(['│', ' '])
            .ends_with(&chord("⌘L", "Ctrl+Shift+L")),
        "{focus}"
    );
    assert!(row("Open traces").contains("F3"));
    // an unavailable command still shows its key
    assert!(
        row("Return to session").contains(&chord("⌘Enter", "Ctrl+Shift+Enter")),
        "{text}"
    );
    // an observed shortcut replaces the candidate
    let observed = a.command_shortcut(ActionId::FocusAgents);
    assert_eq!(observed, Some((chord("⌘L", "Ctrl+Shift+L"), false)));
}

fn highlighted_row(a: &App) -> String {
    screen(a, 100)
        .lines()
        .find(|line| line.contains('›'))
        .unwrap_or_default()
        .to_string()
}

#[test]
fn commands_group_by_theme_and_hide_empty_sections() {
    use agent_mux::app::commands::{Overlay, group_commands};
    use agent_mux::keymap::{ACTIONS, ActionTheme};

    let mut a = app();
    a.handle_key(&key(KeyCode::F(1)), Instant::now());

    // Every action appears exactly once, sections follow the theme order and
    // none is empty when the query is empty.
    let sections = group_commands(&a.filtered_commands());
    let got: Vec<ActionTheme> = sections.iter().map(|s| s.theme).collect();
    let want: Vec<ActionTheme> = ActionTheme::ALL
        .iter()
        .copied()
        .filter(|theme| ACTIONS.iter().any(|action| action.theme == *theme))
        .collect();
    assert_eq!(got, want);
    assert!(sections.iter().all(|section| !section.actions.is_empty()));
    assert_eq!(
        sections.iter().map(|s| s.actions.len()).sum::<usize>(),
        ACTIONS.len()
    );
    let Some(Overlay::Commands(menu)) = &a.command_overlay else {
        panic!("commands not open")
    };
    assert_eq!(menu.selected, 0);
    assert_eq!(a.filtered_commands()[0].id, ActionId::FocusAgents);

    // A search keeps matches under their headers and drops empty sections.
    for c in "open".chars() {
        a.handle_key(&key(KeyCode::Char(c)), Instant::now());
    }
    let matches = a.filtered_commands();
    let sections = group_commands(&matches);
    let got: Vec<ActionTheme> = sections.iter().map(|s| s.theme).collect();
    let want: Vec<ActionTheme> = ActionTheme::ALL
        .iter()
        .copied()
        .filter(|theme| matches.iter().any(|action| action.theme == *theme))
        .collect();
    assert_eq!(got, want);
    assert!(got.contains(&ActionTheme::Screens));
    assert!(got.contains(&ActionTheme::Tracing));
    assert!(!got.contains(&ActionTheme::Navigation));
    assert!(!got.contains(&ActionTheme::Sessions));

    // Nothing matches: the overlay says so and offers nothing to select.
    for _ in 0.."open".len() {
        a.handle_key(&key(KeyCode::Backspace), Instant::now());
    }
    for c in "zzzzzz".chars() {
        a.handle_key(&key(KeyCode::Char(c)), Instant::now());
    }
    assert!(a.filtered_commands().is_empty());
    assert!(group_commands(&a.filtered_commands()).is_empty());
    let text = screen(&a, 100);
    assert!(text.contains("No matching commands"), "{text}");
    assert!(highlighted_row(&a).is_empty(), "nothing to highlight");
}

#[test]
fn arrows_and_enter_walk_actions_not_section_headers() {
    use agent_mux::keymap::ActionTheme;

    let mut a = app();
    a.handle_key(&key(KeyCode::F(1)), Instant::now());
    let headers: Vec<&str> = ActionTheme::ALL.iter().map(|theme| theme.title()).collect();

    // Down moves one action at a time; the highlight never lands on a header,
    // even where a section boundary sits between two actions.
    let labels: Vec<&'static str> = a.filtered_commands().iter().map(|a| a.label).collect();
    for (step, label) in labels.iter().enumerate().take(10) {
        let row = highlighted_row(&a);
        assert!(row.contains(label), "step {step}: {row:?}");
        assert!(
            !headers
                .iter()
                .any(|header| row.contains(&format!(" {header} "))),
            "step {step} highlighted a header: {row:?}"
        );
        a.handle_key(&key(KeyCode::Down), Instant::now());
    }

    // Enter runs the action under the highlight in section order, not the
    // registration order of ACTIONS.
    let mut a = app();
    a.handle_key(&key(KeyCode::F(1)), Instant::now());
    for c in "open".chars() {
        a.handle_key(&key(KeyCode::Char(c)), Instant::now());
    }
    let items = a.filtered_commands();
    assert_eq!(items[0].id, ActionId::Skills);
    assert_eq!(items[1].id, ActionId::Configuration);
    a.handle_key(&key(KeyCode::Down), Instant::now());
    let row = highlighted_row(&a);
    assert!(row.contains("Open configuration"), "{row:?}");
    a.handle_key(&key(KeyCode::Enter), Instant::now());
    assert!(a.command_overlay.is_none());
    assert!(
        matches!(a.mode, Mode::ConfigView(_)),
        "Enter ran the wrong section entry: {:?}",
        a.mode
    );
}
