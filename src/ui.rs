use crate::app::about::{AboutRow, AboutState};
use crate::app::loops::{LoopDialogState, LoopField, LoopStatus};
use crate::app::loops_view::{LoopRow, LoopsPane, LoopsTab, LoopsViewState};
use crate::app::workflows_view::{
    DialogField as WfField, DialogPurpose, RunRow, ViewPane, ViewPending, WorkflowDialogState,
    WorkflowsViewState,
};
use crate::app::{
    App, BrowserPane, DialogContentMode, DialogField, DialogState, HistoryPane, HistoryState,
    InstallLabel, Mode, NoticeLevel, SidebarSection, SkillLauncherField, SkillRow, SkillsPane,
    SkillsTab, SkillsViewState, TraceBrowserState,
};
use crate::app::{ConfigPane, ConfigRow, ConfigViewState, Pending};
use crate::status::Status;
use crate::tracing::cli::{fmt_cost, fmt_ms, fmt_time, fmt_tokens};
use crate::tracing::store::query::LaunchStats;
use crate::tracing::view::{self as trace_view, Bar};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph};
use std::time::Instant;
use tui_term::widget::PseudoTerminal;

pub const SIDEBAR_WIDTH: u16 = 30;

#[derive(Clone, Copy)]
struct PlatformKeys {
    page_scroll: &'static str,
    word_navigation: &'static str,
}

const fn platform_keys_for(macos: bool) -> PlatformKeys {
    if macos {
        PlatformKeys {
            page_scroll: "Fn+↑/↓ (PgUp/PgDn)",
            word_navigation: "Option+←/→",
        }
    } else {
        PlatformKeys {
            page_scroll: "PgUp/PgDn",
            word_navigation: "Ctrl+←/→",
        }
    }
}

fn platform_keys() -> PlatformKeys {
    platform_keys_for(cfg!(target_os = "macos"))
}

/// First visible index of the sidebar's session list: 0 until the selected
/// row would fall below the window, then scrolled just far enough to keep
/// it on the last row. Pure — the mouse handler uses the same math to map a
/// clicked row back to a session index.
pub fn sidebar_window(selected: usize, len: usize, visible: usize) -> usize {
    if visible == 0 || len <= visible {
        0
    } else if selected >= visible {
        (selected + 1 - visible).min(len - visible)
    } else {
        0
    }
}

/// Splits the sidebar into active sessions, the three development
/// sections, and a compact history section. A development section with
/// nothing in it shrinks to its title and a one-line hint, and the rows it
/// frees go to the others.
pub fn sidebar_areas(
    total_height: u16,
    agent_count: usize,
    loop_count: usize,
    workflow_count: usize,
) -> (Rect, Rect, Rect, Rect, Rect) {
    let side_area = Rect::new(0, 0, SIDEBAR_WIDTH, total_height.saturating_sub(1));
    // Active keeps its quarter. History is capped at four rows; on a short
    // terminal it yields rows until Agents, Loops and Workflows can each keep
    // two. The three development sections share every remaining row.
    let active_rows = side_area.height / 4;
    let rest = side_area.height.saturating_sub(active_rows);
    let history_rows = if rest == 0 {
        0
    } else {
        rest.saturating_sub(6).clamp(1, 4)
    };
    let development_rows = rest.saturating_sub(history_rows);
    let empty = [agent_count == 0, loop_count == 0, workflow_count == 0];
    let open = empty.iter().filter(|e| !**e).count() as u16;
    // an empty section keeps a border and one line, never more than an
    // equal share would give it
    let closed_rows = (development_rows / 3).min(3);
    let [agent_rows, loop_rows, workflow_rows] = if open == 0 || open == 3 {
        let shared_rows = development_rows / 3;
        let remainder = development_rows % 3;
        [
            shared_rows + u16::from(remainder > 0),
            shared_rows + u16::from(remainder > 1),
            shared_rows,
        ]
    } else {
        let spare = development_rows.saturating_sub(closed_rows * (3 - open));
        let share = spare / open;
        let mut remainder = spare % open;
        empty.map(|e| {
            if e {
                closed_rows
            } else {
                let extra = u16::from(remainder > 0);
                remainder = remainder.saturating_sub(1);
                share + extra
            }
        })
    };
    let [active, skills, loops, workflows, history] = Layout::vertical([
        Constraint::Length(active_rows),
        Constraint::Length(agent_rows),
        Constraint::Length(loop_rows),
        Constraint::Length(workflow_rows),
        Constraint::Length(history_rows),
    ])
    .areas(side_area);
    (active, skills, loops, workflows, history)
}

/// Char-boundary-safe truncation with an ellipsis. Byte slicing here
/// panicked on non-ASCII titles (emoji, accents).
fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() > max {
        let keep: String = s.chars().take(max.saturating_sub(3)).collect();
        format!("{keep}...")
    } else {
        s.to_string()
    }
}

/// Keep the project end of a long working directory visible.
fn truncate_path_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let suffix: String = s
        .chars()
        .rev()
        .take(max.saturating_sub(1))
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("…{suffix}")
}

pub fn status_label_style(status: Status) -> (String, Style) {
    match status {
        Status::Working => ("working".into(), Style::default().fg(Color::Green)),
        Status::Idle => ("idle".into(), Style::default().fg(Color::DarkGray)),
        Status::NeedsAttention => (
            "attention".into(),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Status::Exited(Some(code)) => (format!("exit {code}"), Style::default().fg(Color::Red)),
        Status::Exited(None) => ("exited".into(), Style::default().fg(Color::Red)),
    }
}

/// (rows, cols) inside the main pane's borders, given the full terminal
/// rect and sidebar visibility. Keep in sync with the Layout in draw():
/// 1 status-bar row at the bottom, optional SIDEBAR_WIDTH columns on the left,
/// 1-cell border all around the main pane.
pub fn main_pane_inner_dims(total: Rect, sidebar_hidden: bool) -> (u16, u16) {
    let sidebar_w = if sidebar_hidden { 0 } else { SIDEBAR_WIDTH };
    let rows = total.height.saturating_sub(1).saturating_sub(2).max(1);
    let cols = total
        .width
        .saturating_sub(sidebar_w)
        .saturating_sub(2)
        .max(1);
    (rows, cols)
}

/// (rows, cols) inside the main pane's borders, assuming the sidebar is visible.
pub fn main_pane_inner(total: Rect) -> (u16, u16) {
    main_pane_inner_dims(total, false)
}

/// (x, y) of the main pane's interior top-left cell based on sidebar visibility.
pub fn pane_origin(sidebar_hidden: bool) -> (u16, u16) {
    if sidebar_hidden {
        (1, 1)
    } else {
        (SIDEBAR_WIDTH + 1, 1)
    }
}

/// (x, y) of the main pane's interior top-left cell when the sidebar is visible.
pub const PANE_ORIGIN: (u16, u16) = (SIDEBAR_WIDTH + 1, 1);

/// Translate absolute terminal coordinates into pane-local (col, row),
/// accounting for whether the sidebar is currently hidden.
pub fn pane_local_with_sidebar(
    col: u16,
    row: u16,
    pane: (u16, u16),
    sidebar_hidden: bool,
) -> Option<(u16, u16)> {
    let (rows, cols) = pane;
    let (x0, y0) = pane_origin(sidebar_hidden);
    if col >= x0 && col < x0 + cols && row >= y0 && row < y0 + rows {
        Some((col - x0, row - y0))
    } else {
        None
    }
}

/// Translate absolute terminal coordinates into pane-local (col, row).
/// `pane` is App's pane_size, i.e. (rows, cols). None = outside the pane
/// interior (border cells count as outside).
pub fn pane_local(col: u16, row: u16, pane: (u16, u16)) -> Option<(u16, u16)> {
    pane_local_with_sidebar(col, row, pane, false)
}

/// Like `pane_local_with_sidebar`, but clamps out-of-range terminal coordinates into the
/// pane interior instead of returning `None`.
pub fn pane_clamped_with_sidebar(
    col: u16,
    row: u16,
    pane: (u16, u16),
    sidebar_hidden: bool,
) -> (u16, u16) {
    let (rows, cols) = pane;
    let (x0, y0) = pane_origin(sidebar_hidden);
    let max_x = x0 + cols.saturating_sub(1);
    let max_y = y0 + rows.saturating_sub(1);
    let cx = col.clamp(x0, max_x);
    let cy = row.clamp(y0, max_y);
    (cx - x0, cy - y0)
}

/// Like `pane_local`, but clamps out-of-range terminal coordinates into the
/// pane interior instead of returning `None`. Used to finalize a drag whose
/// terminating event (e.g. the mouse-up) landed outside the pane -- the
/// drag still needs a definite final position rather than being stranded
/// with `dragging: true` forever.
pub fn pane_clamped(col: u16, row: u16, pane: (u16, u16)) -> (u16, u16) {
    pane_clamped_with_sidebar(col, row, pane, false)
}

/// Applies REVERSED to every cell of `inner` whose grid-absolute position
/// falls inside the selection. Pure over the buffer; testable headlessly.
pub fn apply_selection_highlight(
    buf: &mut ratatui::buffer::Buffer,
    inner: Rect,
    sel: &crate::selection::Selection,
    scrollback_len: usize,
    offset: usize,
) {
    for v in 0..inner.height {
        let row = crate::selection::abs_row(scrollback_len, offset, v);
        for c in 0..inner.width {
            if sel.contains(row, c) {
                let cell = &mut buf[(inner.x + c, inner.y + v)];
                let style = cell.style().add_modifier(Modifier::REVERSED);
                cell.set_style(style);
            }
        }
    }
}

/// Highlight search matches: all matches yellow, the current one white +
/// bold. Same visual-coordinate mapping as the selection pass.
pub fn apply_search_highlight(
    buf: &mut ratatui::buffer::Buffer,
    inner: Rect,
    matches: &[crate::search::Match],
    current: usize,
    scrollback_len: usize,
    offset: usize,
) {
    for (i, m) in matches.iter().enumerate() {
        // abs = len - offset + v  =>  v = row + offset - len
        let Some(v) = (m.row + offset).checked_sub(scrollback_len) else {
            continue; // above the current view
        };
        if v >= usize::from(inner.height) {
            continue; // below the current view
        }
        let style = if i == current {
            Style::default()
                .bg(Color::White)
                .fg(Color::Black)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().bg(Color::Yellow).fg(Color::Black)
        };
        for c in m.col_start..=m.col_end.min(inner.width.saturating_sub(1)) {
            buf[(inner.x + c, inner.y + v as u16)].set_style(style);
        }
    }
}

pub fn draw(f: &mut Frame, app: &App, now: Instant) {
    let [body, bar] = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(f.area());
    if app.sidebar_hidden {
        draw_main(f, body, app, now);
    } else {
        let [side, main] =
            Layout::horizontal([Constraint::Length(SIDEBAR_WIDTH), Constraint::Min(0)]).areas(body);
        draw_sidebar(f, side, app, now);
        draw_main(f, main, app, now);
    }
    draw_status_bar(f, bar, app);

    match &app.mode {
        Mode::NewSession(dialog) => draw_new_session_dialog(f, dialog, app),
        Mode::SessionHistory(history) => draw_session_history(f, history, app),
        Mode::TraceBrowser(browser) => draw_trace_browser(f, browser),
        Mode::ConfirmKill => draw_confirm(f, "Kill this session? [y/n]"),
        Mode::ConfirmQuit => draw_confirm(f, "Sessions are still working. Quit anyway? [y/n]"),
        Mode::Help => draw_help(f),
        Mode::SkillsView(view) => draw_skills_view(f, view, app),
        Mode::SkillLauncher(launcher) => draw_skill_launcher(f, launcher, app),
        Mode::About(state) => draw_about(f, state),
        Mode::LoopsView(view) => draw_loops_view(f, view, app),
        Mode::NewLoop(dialog) => draw_loop_dialog(f, dialog, app),
        Mode::ConfigView(view) => draw_config_view(f, view),
        Mode::WorkflowDialog(dialog) => draw_workflow_dialog(f, dialog),
        Mode::WorkflowsView(view) => draw_workflows_view(f, view, app),
        Mode::Inbox(state) => draw_inbox(f, state),
        Mode::ConfirmRemoveLoop => draw_confirm(
            f,
            "Remove this loop from the registry? Its files in the workspace stay. [y/n]",
        ),
        _ => {}
    }
}

fn draw_sidebar(f: &mut Frame, area: Rect, app: &App, now: Instant) {
    let (active_area, agents_area, loops_area, workflows_area, history_area) = sidebar_areas(
        area.height,
        app.skills.len(),
        app.loop_registry.loops.len(),
        app.workflow_section_lines().len(),
    );

    draw_active_sidebar(f, active_area, app, now);
    draw_agents_sidebar(f, agents_area, app);
    draw_loops_sidebar(f, loops_area, app);
    draw_workflows_sidebar(f, workflows_area, app);
    draw_history_sidebar(f, history_area, app);
}

fn draw_workflows_sidebar(f: &mut Frame, area: Rect, app: &App) {
    use crate::app::workflows::{SectionLine, WorkflowRow};
    let is_focused =
        app.sidebar_section == SidebarSection::Workflows && matches!(app.mode, Mode::Control);
    let n = app.workflow_list.len();
    let live = app.live_workflow_runs.len();
    let title = if live > 0 {
        format!("Workflows · {live} running")
    } else {
        format!("Workflows [{n}]")
    };
    let border_style = if is_focused {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let title_style = if is_focused {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Gray)
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(Span::styled(format!(" {title} "), title_style));
    let rows = app.workflow_rows();
    if rows.is_empty() {
        let hint = Paragraph::new("[c] compose one for a task")
            .style(Style::default().fg(Color::DarkGray))
            .block(block);
        f.render_widget(hint, area);
        return;
    }
    let lines = app.workflow_section_lines();
    let visible = usize::from(area.height.saturating_sub(2));
    let start = app.workflow_section_start(&lines, visible);
    let end = (start + visible.max(1)).min(lines.len());
    let inner_width = usize::from(area.width.saturating_sub(2));
    let items: Vec<ListItem> = lines[start..end]
        .iter()
        .map(|line| {
            let i = match line {
                SectionLine::Header(h) => {
                    return ListItem::new(Line::styled(
                        h.to_string(),
                        Style::default().fg(Color::DarkGray),
                    ));
                }
                SectionLine::Row(i) => *i,
            };
            let is_selected = i == app.selected_workflow;
            let marker = if is_selected && is_focused {
                "> "
            } else if is_selected {
                "* "
            } else {
                "  "
            };
            let (glyph, color, name, right) = match &rows[i] {
                WorkflowRow::Live(id) => {
                    let r = app.live_workflow_runs.iter().find(|r| &r.run_id == id);
                    (
                        "▶",
                        Color::Cyan,
                        r.map(|r| r.name.clone()).unwrap_or_default(),
                        r.map(|r| {
                            format!("{}/{}", r.state.records.len(), r.state.sessions_started)
                        })
                        .unwrap_or_default(),
                    )
                }
                WorkflowRow::Planned(id) => {
                    let p = app.planned_workflows.iter().find(|p| &p.id == id);
                    let ok = p.is_some_and(|p| p.valid());
                    (
                        if ok { "⏸" } else { "!" },
                        if ok { Color::Cyan } else { Color::Yellow },
                        p.map(|p| p.name.clone()).unwrap_or_default(),
                        "plan".to_string(),
                    )
                }
                WorkflowRow::Recent(id) => {
                    let r = app.recent_workflow_runs.iter().find(|r| &r.run_id == id);
                    let ok = r.is_some_and(|r| r.status == "finished" && r.error.is_none());
                    (
                        if ok { "✓" } else { "!" },
                        if ok { Color::Green } else { Color::Yellow },
                        r.map(|r| r.name.clone()).unwrap_or_default(),
                        r.map(|r| {
                            if ok {
                                "done".to_string()
                            } else {
                                r.status.clone()
                            }
                        })
                        .unwrap_or_default(),
                    )
                }
                WorkflowRow::Doc(d) => {
                    let e = &app.workflow_list[*d];
                    if e.valid() {
                        (" ", Color::DarkGray, e.name.clone(), String::new())
                    } else {
                        ("!", Color::Yellow, e.name.clone(), "invalid".to_string())
                    }
                }
            };
            let right = truncate_chars(&right, 8);
            let name_width = inner_width
                .saturating_sub(4 + right.chars().count() + 1)
                .max(6);
            let line = Line::from(vec![
                Span::raw(marker),
                Span::styled(format!("{glyph} "), Style::default().fg(color)),
                Span::styled(
                    format!("{:<w$}", truncate_chars(&name, name_width), w = name_width),
                    if is_selected {
                        Style::default().fg(Color::Cyan)
                    } else {
                        Style::default().fg(Color::White)
                    },
                ),
                Span::styled(format!(" {right}"), Style::default().fg(color)),
            ]);
            let item = ListItem::new(line);
            if is_selected && is_focused {
                item.style(Style::default().add_modifier(Modifier::REVERSED))
            } else {
                item
            }
        })
        .collect();
    f.render_widget(List::new(items).block(block), area);
}

fn draw_loops_sidebar(f: &mut Frame, area: Rect, app: &App) {
    let is_focused =
        app.sidebar_section == SidebarSection::Loops && matches!(app.mode, Mode::Control);
    let n = app.loop_registry.loops.len();
    let title = if app.loop_registry.pause_all {
        format!("Loops [{}] PAUSED", n)
    } else if n == 0 {
        "Loops [0]".to_string()
    } else {
        format!("Loops [{}/{}]", (app.selected_loop + 1).min(n), n)
    };
    let border_style = if is_focused {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let title_style = if app.loop_registry.pause_all {
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    } else if is_focused {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Gray)
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(Span::styled(format!(" {title} "), title_style));
    if n == 0 {
        let hint = Paragraph::new("[a] add a loop")
            .style(Style::default().fg(Color::DarkGray))
            .block(block);
        f.render_widget(hint, area);
        return;
    }
    let visible = usize::from(area.height.saturating_sub(2));
    let start = sidebar_window(app.selected_loop, n, visible);
    let end = (start + visible.max(1)).min(n);
    // Marker, glyph, then the pattern and the workspace sharing the rest
    // with the right label: two loops of one pattern differ by workspace.
    let name_width = usize::from(area.width.saturating_sub(2))
        .saturating_sub(9)
        .max(6);
    let items: Vec<ListItem> = app.loop_registry.loops[start..end]
        .iter()
        .enumerate()
        .map(|(offset, entry)| {
            let i = start + offset;
            let is_selected = i == app.selected_loop;
            let marker = if is_selected && is_focused {
                "> "
            } else if is_selected {
                "* "
            } else {
                "  "
            };
            let (status, right) = app.loop_row(entry);
            let pattern = truncate_chars(&entry.pattern, name_width);
            let room = name_width.saturating_sub(pattern.chars().count() + 1);
            let workspace = if room >= 5 {
                format!(" {}", truncate_chars(&entry.workspace_name(), room))
            } else {
                String::new()
            };
            let line = Line::from(vec![
                Span::raw(marker),
                Span::styled(
                    format!("{} ", status.glyph()),
                    Style::default().fg(status.color()),
                ),
                Span::styled(
                    pattern.clone(),
                    if is_selected {
                        Style::default().fg(Color::Cyan)
                    } else {
                        Style::default().fg(Color::White)
                    },
                ),
                Span::styled(
                    format!(
                        "{:<w$}",
                        workspace,
                        w = name_width - pattern.chars().count()
                    ),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(format!(" {right:>4}"), Style::default().fg(status.color())),
            ]);
            let item = ListItem::new(line);
            if is_selected && is_focused {
                item.style(Style::default().add_modifier(Modifier::REVERSED))
            } else {
                item
            }
        })
        .collect();
    f.render_widget(List::new(items).block(block), area);
}

fn draw_active_sidebar(f: &mut Frame, area: Rect, app: &App, now: Instant) {
    let is_focused =
        app.sidebar_section == SidebarSection::Active && matches!(app.mode, Mode::Control);
    // The tree, and the cursor's place in it: with a run folded away the
    // session index and the drawn position are no longer the same number.
    let rows = app.active_rows();
    let drawn = crate::tree::visible_items(&rows);
    let title = if app.sessions.is_empty() {
        "Active [0]".to_string()
    } else {
        let at = drawn
            .iter()
            .position(|i| *i == app.selected)
            .map(|n| n + 1)
            .unwrap_or(app.selected + 1);
        format!("Active [{at}/{}]", app.sessions.len())
    };
    let border_style = if is_focused {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(Span::styled(
            format!(" {title} "),
            if is_focused {
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Gray)
            },
        ));

    if app.sessions.is_empty() {
        let hint = Paragraph::new("no sessions\n\n[n] new session").block(block);
        f.render_widget(hint, area);
        return;
    }

    // The cursor sits on a row, not on a session index, so the scroll
    // window has to follow the row.
    let cursor = crate::tree::row_of(&rows, app.selected).unwrap_or(0);
    let visible = usize::from(area.height.saturating_sub(2));
    let start = sidebar_window(cursor, rows.len(), visible);
    let end = (start + visible.max(1)).min(rows.len());
    // Numbering counts drawn sessions, so the digit beside a row is the
    // digit that selects it.
    let numbers: std::collections::HashMap<usize, usize> = drawn
        .into_iter()
        .enumerate()
        .map(|(n, index)| (index, n))
        .collect();
    let items: Vec<ListItem> = rows[start..end]
        .iter()
        .enumerate()
        .map(|(offset, row)| {
            session_row(
                app,
                row,
                start + offset == cursor,
                is_focused,
                &numbers,
                now,
            )
        })
        .collect();
    f.render_widget(List::new(items).block(block), area);
}

/// One row of the Active tree: a loop or workflow header, or a session
/// under it.
fn session_row<'a>(
    app: &'a App,
    row: &crate::tree::Row,
    is_cursor: bool,
    is_focused: bool,
    numbers: &std::collections::HashMap<usize, usize>,
    now: Instant,
) -> ListItem<'a> {
    let marker = if is_cursor { "> " } else { "  " };
    let spans = match row {
        crate::tree::Row::Group {
            kind,
            title,
            detail,
            members,
            collapsed,
            ..
        } => {
            // A folded header answers for the family it hides: how many
            // calls, and how many of them are still running.
            let running = members
                .iter()
                .filter_map(|i| app.sessions.get(*i))
                .filter(|s| !matches!(s.status(now), Status::Exited(_)))
                .count();
            let mut spans = vec![
                Span::raw(marker.to_string()),
                Span::styled(
                    if *collapsed { "▸ " } else { "▾ " }.to_string(),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(
                    format!("{} ", kind.glyph()),
                    Style::default().fg(Color::Magenta),
                ),
                Span::styled(
                    truncate_chars(title, 14),
                    Style::default()
                        .fg(Color::Magenta)
                        .add_modifier(Modifier::BOLD),
                ),
                if running > 0 {
                    Span::styled(
                        format!(" {running}/{}▶", members.len()),
                        Style::default().fg(Color::Green),
                    )
                } else {
                    Span::styled(
                        format!(" {}", members.len()),
                        Style::default().fg(Color::DarkGray),
                    )
                },
            ];
            spans.push(Span::styled(
                format!(" {}", truncate_chars(detail, 10)),
                Style::default().fg(Color::DarkGray),
            ));
            spans
        }
        crate::tree::Row::Item { index, depth, last } => {
            let Some(s) = app.sessions.get(*index) else {
                return ListItem::new(Line::raw(String::new()));
            };
            let (label, style) = status_label_style(s.status(now));
            let num = match numbers.get(index) {
                Some(n) if *n < 9 => format!("{} ", n + 1),
                _ => "  ".into(),
            };
            // A session the tree owns says what it is in the run (a
            // workflow step, a loop run); a loose one keeps its profile.
            let name = match (&s.group, depth) {
                (Some(g), 1) if !g.label.is_empty() => g.label.clone(),
                _ => s.profile.name.clone(),
            };
            let mut spans = vec![Span::raw(marker.to_string())];
            if *depth > 0 {
                spans.push(Span::styled(
                    if *last { "└ " } else { "├ " }.to_string(),
                    Style::default().fg(Color::DarkGray),
                ));
            }
            spans.push(Span::styled(num, Style::default().fg(Color::DarkGray)));
            spans.push(Span::raw(format!("{} ", truncate_chars(&name, 12))));
            spans.push(Span::styled(format!("[{label}]"), style));
            if s.trace.is_some() {
                spans.push(Span::styled(
                    format!(
                        " {}",
                        trace_badge(s.trace_stats.as_ref(), false, session_backend(s))
                    ),
                    Style::default().fg(Color::Cyan),
                ));
            }
            spans
        }
    };
    let item = ListItem::new(Line::from(spans));
    if is_cursor && is_focused {
        item.style(Style::default().add_modifier(Modifier::REVERSED))
    } else if is_cursor {
        item.style(Style::default().fg(Color::Cyan))
    } else {
        item
    }
}

fn draw_agents_sidebar(f: &mut Frame, area: Rect, app: &App) {
    let is_focused =
        app.sidebar_section == SidebarSection::Agents && matches!(app.mode, Mode::Control);
    let border_style = if is_focused {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let title = if app.skills.is_empty() {
        "Agents [0]".to_string()
    } else {
        let current = (app.selected_agent + 1).min(app.skills.len());
        format!("Agents [{current}/{}]", app.skills.len())
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(Span::styled(
            format!(" {title} "),
            if is_focused {
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Gray)
            },
        ));

    if app.skills.is_empty() {
        let hint = Paragraph::new("no skills found\n\n~/.agent-mux/skills/").block(block);
        f.render_widget(hint, area);
        return;
    }

    let visible = usize::from(area.height.saturating_sub(2));
    let start = sidebar_window(app.selected_agent, app.skills.len(), visible);
    let end = (start + visible.max(1)).min(app.skills.len());
    let items: Vec<ListItem> = app.skills[start..end]
        .iter()
        .enumerate()
        .map(|(offset, agent)| {
            let i = start + offset;
            let is_selected = i == app.selected_agent;
            let marker = if is_selected && is_focused {
                "> "
            } else if is_selected {
                "* "
            } else {
                "  "
            };

            let running_harness = app.running_skill_harness(&agent.id);

            let icon_str = agent.icon.as_deref().unwrap_or("⚡");
            let mut spans = vec![
                Span::raw(marker),
                Span::styled(format!("{icon_str} "), Style::default().fg(Color::Yellow)),
                Span::styled(
                    &agent.name,
                    if is_selected && is_focused {
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD)
                    } else if is_selected {
                        Style::default().fg(Color::Cyan)
                    } else {
                        Style::default().fg(Color::White)
                    },
                ),
            ];
            if let Some(h) = running_harness {
                spans.push(Span::styled(
                    format!(" [{}]", h.as_str()),
                    Style::default().fg(Color::Green),
                ));
            }

            let line = Line::from(spans);
            let item = ListItem::new(line);
            if is_selected && is_focused {
                item.style(Style::default().add_modifier(Modifier::REVERSED))
            } else if is_selected {
                item.style(Style::default().fg(Color::Cyan))
            } else {
                item
            }
        })
        .collect();
    f.render_widget(List::new(items).block(block), area);
}

fn draw_history_sidebar(f: &mut Frame, area: Rect, app: &App) {
    let is_focused =
        app.sidebar_section == SidebarSection::History && matches!(app.mode, Mode::Control);
    let title = if app.history_sessions.is_empty() {
        "History [0]".to_string()
    } else {
        format!(
            "History [{}/{}]",
            app.selected_history + 1,
            app.history_sessions.len()
        )
    };
    let border_style = if is_focused {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(Span::styled(
            format!(" {title} "),
            if is_focused {
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Gray)
            },
        ));

    if app.history_sessions.is_empty() {
        let hint = Paragraph::new("no history\nsessions found\n\n[a] all projects").block(block);
        f.render_widget(hint, area);
        return;
    }

    let visible = usize::from(area.height.saturating_sub(2));
    let start = sidebar_window(app.selected_history, app.history_sessions.len(), visible);
    let end = (start + visible.max(1)).min(app.history_sessions.len());
    let items: Vec<ListItem> = app.history_sessions[start..end]
        .iter()
        .enumerate()
        .map(|(offset, s)| {
            let i = start + offset;
            let is_selected = i == app.selected_history;
            let marker = if is_selected { "> " } else { "  " };
            let (provider_badge, provider_style) = match s.provider {
                crate::history::AgentProvider::Claude => ("C", Style::default().fg(Color::Magenta)),
                crate::history::AgentProvider::Antigravity => {
                    ("A", Style::default().fg(Color::Blue))
                }
            };
            let max_title_len = (area.width as usize).saturating_sub(8).max(5);
            let title = if s.title.chars().count() > max_title_len {
                let mut truncated: String = s
                    .title
                    .chars()
                    .take(max_title_len.saturating_sub(1))
                    .collect();
                truncated.push('…');
                truncated
            } else {
                s.title.clone()
            };
            let line = Line::from(vec![
                Span::raw(marker.to_string()),
                Span::styled(format!("[{provider_badge}] "), provider_style),
                Span::raw(title),
            ]);
            let item = ListItem::new(line);
            if is_selected && is_focused {
                item.style(Style::default().add_modifier(Modifier::REVERSED))
            } else if is_selected {
                item.style(Style::default().fg(Color::Cyan))
            } else {
                item
            }
        })
        .collect();
    f.render_widget(List::new(items).block(block), area);
}

fn draw_main(f: &mut Frame, area: Rect, app: &App, now: Instant) {
    if !app.sidebar_hidden
        && app.sidebar_section == SidebarSection::Agents
        && matches!(app.mode, Mode::Control)
    {
        if let Some(agent) = app.selected_agent() {
            if agent.capabilities.iter().any(|c| c == "trace.read") {
                draw_trace_briefing_preview(f, area, agent, app, now);
            } else {
                draw_generic_agent_preview(f, area, agent, app, now);
            }
        }
        return;
    }
    if !app.sidebar_hidden
        && app.sidebar_section == SidebarSection::Loops
        && matches!(app.mode, Mode::Control)
    {
        draw_loop_preview(f, area, app);
        return;
    }
    if !app.sidebar_hidden
        && app.sidebar_section == SidebarSection::Workflows
        && matches!(app.mode, Mode::Control)
    {
        draw_workflow_preview(f, area, app);
        return;
    }
    if ((!app.sidebar_hidden
        && app.sidebar_section == SidebarSection::History
        && matches!(app.mode, Mode::Control))
        || app.sessions.is_empty())
        && let Some(hist) = app.history_sessions.get(app.selected_history)
    {
        draw_history_preview(f, area, hist, !app.sessions.is_empty());
        return;
    }
    let Some(session) = app.sessions.get(app.selected) else {
        let block = Block::default().borders(Borders::ALL).title("agent-mux");
        f.render_widget(
            Paragraph::new("no sessions — press [n] to create one").block(block),
            area,
        );
        return;
    };
    let (label, _) = status_label_style(session.status(now));
    let (scrollback_len, scroll_offset) = session.scroll_view();
    let scroll_tag = if scroll_offset > 0 {
        format!("[SCROLL ↑ {scroll_offset}/{scrollback_len}] ")
    } else {
        String::new()
    };
    let trace_tag = if session.trace.is_some() {
        format!(
            "{} ",
            trace_badge(session.trace_stats.as_ref(), true, session_backend(session))
        )
    } else {
        String::new()
    };
    let title = format!(
        " {} — {} [{label}] {trace_tag}{scroll_tag}",
        session.profile.name,
        session.dir.display()
    );
    let block = Block::default().borders(Borders::ALL).title(title);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let cursor = {
        let screen = session.parser.screen();
        f.render_widget(PseudoTerminal::new(screen), inner);
        // real cursor while attached AND live: a scrolled view is history,
        // the cursor belongs to the bottom of the buffer
        (matches!(app.mode, Mode::Attached) && !screen.hide_cursor() && scroll_offset == 0)
            .then(|| screen.cursor_position())
    };
    if let Some((row, col)) = cursor
        && inner.width > 0
        && inner.height > 0
    {
        let col = col.min(inner.width.saturating_sub(1));
        let row = row.min(inner.height.saturating_sub(1));
        f.set_cursor_position((inner.x + col, inner.y + row));
    }
    if let Some(sel) = app.displayed_selection() {
        let (len, offset) = session.scroll_view();
        apply_selection_highlight(f.buffer_mut(), inner, sel, len, offset);
    }
    if let Some(st) = &app.search {
        let (len, offset) = session.scroll_view();
        apply_search_highlight(f.buffer_mut(), inner, &st.matches, st.current, len, offset);
    }
}

fn draw_history_preview(
    f: &mut Frame,
    area: Rect,
    summary: &crate::history::SessionSummary,
    has_active: bool,
) {
    let provider_name = match summary.provider {
        crate::history::AgentProvider::Claude => "Claude Code",
        crate::history::AgentProvider::Antigravity => "Google Antigravity",
    };
    let title = format!(" History Session: {} ", summary.title);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(title);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let mut lines = vec![
        Line::from(vec![
            Span::styled("Title:        ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                &summary.title,
                Style::default().add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            Span::styled("Provider:     ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                provider_name,
                match summary.provider {
                    crate::history::AgentProvider::Claude => Style::default().fg(Color::Magenta),
                    crate::history::AgentProvider::Antigravity => Style::default().fg(Color::Blue),
                },
            ),
        ]),
        Line::from(vec![
            Span::styled("Session ID:   ", Style::default().fg(Color::DarkGray)),
            Span::styled(&summary.session_id, Style::default().fg(Color::Yellow)),
        ]),
        Line::from(vec![
            Span::styled("Directory:    ", Style::default().fg(Color::DarkGray)),
            Span::raw(
                summary
                    .cwd
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "Unknown".into()),
            ),
        ]),
        Line::from(vec![
            Span::styled("Turns:        ", Style::default().fg(Color::DarkGray)),
            Span::raw(format!("{}", summary.turn_count)),
            Span::styled("    Modified: ", Style::default().fg(Color::DarkGray)),
            Span::raw(&summary.timestamp_str),
        ]),
        Line::from(vec![
            Span::styled("Transcript:   ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                summary.file_path.display().to_string(),
                Style::default().fg(Color::DarkGray),
            ),
        ]),
        Line::raw(""),
        Line::from(vec![
            Span::styled("Actions:      ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                "[Enter] / [r] ",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("Restart / resume this session in agent-mux"),
        ]),
        Line::from(vec![
            Span::styled("              ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                "[Tab]         ",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("Switch to active sessions"),
        ]),
        Line::from(vec![
            Span::styled("              ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                "[a]           ",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("Toggle current project / all projects"),
        ]),
    ];
    if has_active {
        lines.push(Line::raw(""));
        lines.push(Line::from(vec![Span::styled(
            "Tip: Active sessions are running in the top panel. Press [Tab] to view terminal.",
            Style::default().fg(Color::DarkGray),
        )]));
    }
    f.render_widget(Paragraph::new(lines), inner);
}

fn draw_generic_agent_preview(
    f: &mut Frame,
    area: Rect,
    agent: &crate::skill::SkillDefinition,
    app: &App,
    _now: Instant,
) {
    let icon_str = agent.icon.as_deref().unwrap_or("⚡");
    let title = format!(" {icon_str} {} — Skill [{}] ", agent.name, agent.id);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .title(Span::styled(
            title,
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let [info_area, instructions_area, footer_area] = Layout::vertical([
        Constraint::Length(5),
        Constraint::Min(6),
        Constraint::Length(1),
    ])
    .areas(inner);

    // 1. Info block
    let harnesses_str = if agent.harnesses.is_empty() {
        "claude, codex, agy (all supported)".to_string()
    } else {
        agent
            .harnesses
            .iter()
            .map(|h| h.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    };
    let origin_str = if agent.is_builtin {
        "Built-in (compiled into agent-mux)".to_string()
    } else if let Some(ref p) = agent.dir {
        p.to_string_lossy().into_owned()
    } else {
        "Custom skill".to_string()
    };
    let info_lines = vec![
        Line::from(vec![
            Span::styled("Skill ID:    ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                &agent.id,
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("   Origin:     ", Style::default().fg(Color::DarkGray)),
            Span::styled(origin_str, Style::default().fg(Color::Yellow)),
        ]),
        Line::from(vec![
            Span::styled("Harnesses:   ", Style::default().fg(Color::DarkGray)),
            Span::styled(harnesses_str, Style::default().fg(Color::Green)),
            Span::styled("   Default:    ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                agent.default_harness.as_str(),
                Style::default().fg(Color::Cyan),
            ),
        ]),
        Line::from(vec![
            Span::styled("Description: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                if agent.description.is_empty() {
                    "No description provided."
                } else {
                    &agent.description
                },
                Style::default().fg(Color::Gray),
            ),
        ]),
    ];
    f.render_widget(Paragraph::new(info_lines), info_area);

    // 2. Instructions block
    let inst_block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray))
        .title(Span::styled(
            " SKILL.md ",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));
    let inst_inner = inst_block.inner(instructions_area);
    f.render_widget(inst_block, instructions_area);

    let inst_lines: Vec<Line> = agent
        .body
        .lines()
        .map(|line| {
            if line.starts_with("# ") {
                Line::styled(
                    line,
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                )
            } else if line.starts_with("## ") {
                Line::styled(
                    line,
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                )
            } else if line.starts_with("### ") {
                Line::styled(
                    line,
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                )
            } else if line.starts_with("- ") || line.starts_with("* ") {
                Line::styled(line, Style::default().fg(Color::White))
            } else if line.starts_with("```") {
                Line::styled(line, Style::default().fg(Color::DarkGray))
            } else {
                Line::styled(line, Style::default().fg(Color::Gray))
            }
        })
        .collect();
    f.render_widget(Paragraph::new(inst_lines), inst_inner);

    // 3. Footer
    let is_running = app.running_skill_session(&agent.id).is_some();
    let enter_action = if is_running {
        format!("Attach to {} Session", agent.name)
    } else {
        format!("Launch {} Harness", agent.name)
    };
    let footer = Line::from(vec![
        Span::styled(
            "[Enter] ",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!("{enter_action}   ")),
        Span::styled(
            "[Tab] ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("Switch Section   "),
        Span::styled(
            "[b] ",
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("Toggle Sidebar"),
    ]);
    f.render_widget(Paragraph::new(footer), footer_area);
}

fn draw_trace_briefing_preview(
    f: &mut Frame,
    area: Rect,
    agent: &crate::skill::SkillDefinition,
    app: &App,
    now: Instant,
) {
    let icon_str = agent.icon.as_deref().unwrap_or("⚡");
    let title = format!(
        " {icon_str} {} — Executive Briefing & Telemetry [{}] ",
        agent.name, agent.id
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .title(Span::styled(
            title,
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let [summary_area, sessions_area, footer_area] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(8),
        Constraint::Length(1),
    ])
    .areas(inner);

    let briefing = app.cached_briefing.as_ref();

    // 1. Summary line
    let summary_lines = if let Some(b) = briefing {
        let db_path = app
            .trace_db_path
            .clone()
            .unwrap_or_else(crate::tracing::analysis::default_trace_db_path);
        let toks = b.total_tokens.unwrap_or(0);
        let cost = b.total_cost_usd.unwrap_or(0.0);
        let mut lines = vec![Line::from(vec![
            Span::styled("Trace Store: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                db_path.display().to_string(),
                Style::default().fg(Color::Yellow),
            ),
            Span::styled("  |  Scope: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                b.scope_workspace.display().to_string(),
                Style::default().fg(Color::White),
            ),
            Span::styled("  |  Sessions: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("{}", b.total_sessions),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("  |  Turns: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("{}", b.total_turns),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("  |  Tools: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("{}", b.total_tools),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("  |  Tokens: ", Style::default().fg(Color::DarkGray)),
            Span::styled(format!("{toks}"), Style::default().fg(Color::Yellow)),
            Span::styled("  |  Cost: ", Style::default().fg(Color::DarkGray)),
            Span::styled(format!("${cost:.3}"), Style::default().fg(Color::Yellow)),
        ])];

        let as_of_text = if let Some(t) = app.cached_briefing_as_of {
            let secs = now.saturating_duration_since(t).as_secs();
            format!("Refreshed {secs}s ago")
        } else {
            "Cached".to_string()
        };

        let mut status_spans = vec![
            Span::styled("Cache Status: ", Style::default().fg(Color::DarkGray)),
            Span::styled(as_of_text, Style::default().fg(Color::Green)),
        ];
        if let Some(ref warn) = app.cached_briefing_warning {
            status_spans.push(Span::styled(
                format!("  ⚠️ Refresh warning: {warn}"),
                Style::default().fg(Color::Red),
            ));
        }
        lines.push(Line::from(status_spans));
        lines
    } else {
        let mut spans = vec![Span::styled(
            "Telemetry briefing pending initial refresh...",
            Style::default().fg(Color::DarkGray),
        )];
        if let Some(ref warn) = app.cached_briefing_warning {
            spans.push(Span::styled(
                format!("  ⚠️ Error: {warn}"),
                Style::default().fg(Color::Red),
            ));
        }
        vec![
            Line::from(spans),
            Line::styled(
                "Press [Enter] to launch this skill or wait for background trace query.",
                Style::default().fg(Color::DarkGray),
            ),
        ]
    };
    f.render_widget(Paragraph::new(summary_lines), summary_area);

    // 2. Session Cards
    let card_count = briefing.map_or(0, |b| b.cards.len());
    let sessions_block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray))
        .title(Span::styled(
            format!(" Executive Briefing & Session Clues ({card_count}) "),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));
    let sess_inner = sessions_block.inner(sessions_area);
    f.render_widget(sessions_block, sessions_area);

    if card_count == 0 {
        let empty_msg = Paragraph::new(
            "No active or historical sessions recorded yet in trace database.\nPress [Enter] to launch this skill or [n] to create a new session.",
        )
        .style(Style::default().fg(Color::DarkGray));
        f.render_widget(empty_msg, sess_inner);
    } else if let Some(b) = briefing {
        let items: Vec<ListItem> = b
            .cards
            .iter()
            .map(|card| {
                let status_style = match card.runtime_state {
                    crate::tracing::analysis::RuntimeState::Working => Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                    crate::tracing::analysis::RuntimeState::WaitingForUser => {
                        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
                    }
                    crate::tracing::analysis::RuntimeState::Idle => {
                        Style::default().fg(Color::Yellow)
                    }
                    crate::tracing::analysis::RuntimeState::Exited => {
                        Style::default().fg(Color::DarkGray)
                    }
                    crate::tracing::analysis::RuntimeState::Disconnected
                    | crate::tracing::analysis::RuntimeState::Unknown => {
                        Style::default().fg(Color::DarkGray)
                    }
                };

                let id_label = card
                    .launch_id
                    .as_deref()
                    .or(card.session_key.as_deref())
                    .unwrap_or("unknown");
                let provider_label = card.provider.as_deref().unwrap_or("agent");
                let toks = card.total_tokens.unwrap_or(0);
                let cost = card.total_cost_usd.unwrap_or(0.0);

                let mut lines = vec![Line::from(vec![
                    Span::styled(
                        format!("Session [{id_label}] "),
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        format!("({provider_label}) "),
                        Style::default().fg(Color::Magenta),
                    ),
                    Span::styled(format!("[{:?}] ", card.runtime_state), status_style),
                    Span::styled(
                        format!("— {}", card.cwd.display()),
                        Style::default().fg(Color::DarkGray),
                    ),
                ])];

                if let Some(ref act) = card.current_activity.value
                    && !act.is_empty()
                {
                    lines.push(Line::from(vec![
                        Span::styled(
                            "  ⚡ Right Now:  ",
                            Style::default()
                                .fg(Color::Yellow)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(
                            act,
                            Style::default()
                                .fg(Color::White)
                                .add_modifier(Modifier::BOLD),
                        ),
                    ]));
                }

                if let Some(ref goal) = card.initial_goal.value
                    && !goal.is_empty()
                {
                    lines.push(Line::from(vec![
                        Span::styled("  🎯 Goal:       ", Style::default().fg(Color::Cyan)),
                        Span::styled(goal, Style::default().fg(Color::Gray)),
                    ]));
                }

                if !card.files_modified.is_empty() {
                    let files_str = card
                        .files_modified
                        .iter()
                        .filter_map(|e| e.value.as_deref())
                        .collect::<Vec<_>>()
                        .join(", ");
                    if !files_str.is_empty() {
                        lines.push(Line::from(vec![
                            Span::styled(
                                "  📝 Files:      ",
                                Style::default().fg(Color::LightCyan),
                            ),
                            Span::styled(files_str, Style::default().fg(Color::LightCyan)),
                        ]));
                    }
                }

                if !card.recent_commands.is_empty() {
                    let cmds_str = card
                        .recent_commands
                        .iter()
                        .filter_map(|e| e.value.as_deref())
                        .collect::<Vec<_>>()
                        .join("  ·  ");
                    if !cmds_str.is_empty() {
                        lines.push(Line::from(vec![
                            Span::styled(
                                "  💻 Commands:   ",
                                Style::default().fg(Color::LightGreen),
                            ),
                            Span::styled(cmds_str, Style::default().fg(Color::LightGreen)),
                        ]));
                    }
                }

                if let Some(ref out) = card.last_assistant_output
                    && let Some(ref text) = out.value
                    && !text.is_empty()
                {
                    lines.push(Line::from(vec![
                        Span::styled("  💬 Last Out:   ", Style::default().fg(Color::DarkGray)),
                        Span::styled(format!("\"{text}\""), Style::default().fg(Color::DarkGray)),
                    ]));
                }

                lines.push(Line::from(vec![
                    Span::styled("  📊 Metrics:    ", Style::default().fg(Color::DarkGray)),
                    Span::styled(
                        format!("{} turns", card.completed_turns),
                        Style::default().fg(Color::Yellow),
                    ),
                    Span::raw(" | "),
                    Span::styled(
                        format!("{} tools", card.total_tools),
                        Style::default().fg(Color::Yellow),
                    ),
                    Span::raw(" | "),
                    Span::styled(format!("{toks} tokens"), Style::default().fg(Color::Yellow)),
                    Span::raw(" | "),
                    Span::styled(format!("${cost:.3}"), Style::default().fg(Color::Yellow)),
                ]));

                lines.push(Line::raw("")); // Spacer between sessions
                ListItem::new(lines)
            })
            .collect();
        f.render_widget(List::new(items), sess_inner);
    }

    // 3. Footer hints
    let footer = Line::from(vec![
        Span::styled(
            "[Enter] ",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!("Launch {}   ", agent.name)),
        Span::styled(
            "[Tab] ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("Switch Section   "),
        Span::styled(
            "[b] ",
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("Toggle Sidebar"),
    ]);
    f.render_widget(Paragraph::new(footer), footer_area);
}

fn draw_skill_launcher(f: &mut Frame, state: &crate::app::SkillLauncherState, app: &App) {
    let width = 68.min(f.area().width.saturating_sub(4)).max(48);
    let height = 24.min(f.area().height.saturating_sub(2)).max(16);
    let area = centered(f.area(), width, height);
    f.render_widget(Clear, area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .title(Span::styled(
            format!(" Launch {} [{}] ", state.skill_name, state.skill_id),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let [header_area, workspace_area, list_area, info_area, hint_area] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(5),
        Constraint::Length(4),
        Constraint::Min(4),
        Constraint::Length(2),
    ])
    .areas(inner);

    let header_text = Paragraph::new(vec![
        Line::styled(
            format!("Select an AI harness to run {}:", state.skill_name),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Line::raw(""),
    ]);
    f.render_widget(header_text, header_area);

    let workspace_lines = dir_picker_lines(
        "Workspace: ",
        &state.workspace,
        &state.dir_picker,
        state.field == SkillLauncherField::Workspace,
        2,
    );
    f.render_widget(Paragraph::new(workspace_lines), workspace_area);

    let harnesses = if state.harnesses.is_empty() {
        crate::harness::Harness::ALL.as_slice()
    } else {
        state.harnesses.as_slice()
    };
    let items: Vec<ListItem> = harnesses
        .iter()
        .enumerate()
        .map(|(idx, h)| {
            let is_selected = idx == state.selected;
            let marker = if is_selected { "> " } else { "  " };
            let num = idx + 1;
            let is_running = app.sessions.iter().any(|s| {
                s.skill_id.as_deref() == Some(&state.skill_id)
                    && crate::harness::Harness::detect(&s.profile.command) == Some(*h)
                    && matches!(s.status(Instant::now()), Status::Working | Status::Idle)
            });
            let running_tag = if is_running { " [active - attach]" } else { "" };
            let line = Line::from(vec![
                Span::raw(marker),
                Span::styled(format!("[{num}] "), Style::default().fg(Color::DarkGray)),
                Span::styled(
                    h.display_name(),
                    if is_selected {
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(Color::White)
                    },
                ),
                Span::styled(running_tag, Style::default().fg(Color::Green)),
            ]);
            let item = ListItem::new(line);
            if is_selected && state.field == SkillLauncherField::Harness {
                item.style(Style::default().add_modifier(Modifier::REVERSED))
            } else {
                item
            }
        })
        .collect();
    f.render_widget(List::new(items), list_area);

    let info_lines = if let Some(agent) = app.skills.iter().find(|a| a.id == state.skill_id) {
        let desc = if agent.description.is_empty() {
            "Skill loaded from ~/.agent-mux/skills/"
        } else {
            &agent.description
        };
        let caps = if agent.capabilities.is_empty() {
            "none declared".to_string()
        } else {
            agent.capabilities.join(", ")
        };
        vec![
            Line::styled(
                format!("{} Description:", agent.name),
                Style::default().fg(Color::Yellow),
            ),
            Line::raw(desc),
            Line::from(vec![
                Span::styled("Capabilities: ", Style::default().fg(Color::DarkGray)),
                Span::styled(caps, Style::default().fg(Color::Cyan)),
            ]),
            Line::styled(
                "Installed into the harness skill directory before launch.",
                Style::default().fg(Color::DarkGray),
            ),
        ]
    } else {
        vec![
            Line::styled(
                format!("{} Capabilities:", state.skill_name),
                Style::default().fg(Color::Yellow),
            ),
            Line::raw("• Harness session opened with the skill invoked"),
        ]
    };
    f.render_widget(Paragraph::new(info_lines), info_area);

    let hints = Line::from(vec![
        Span::styled(
            "[Enter] ",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("Launch/Attach   "),
        Span::styled("[Tab] ", Style::default().fg(Color::Cyan)),
        Span::raw("Workspace/Harness   "),
        Span::styled("[1-3 / c,x,a] ", Style::default().fg(Color::Cyan)),
        Span::raw("Select   "),
        Span::styled("[Esc] ", Style::default().fg(Color::DarkGray)),
        Span::raw("Cancel"),
    ]);
    f.render_widget(Paragraph::new(hints), hint_area);
}

/// A hint line cut to `width` columns by dropping whole hints rather than
/// letters: the first piece (the section's lead) and the last (`[?] help`,
/// `[Esc] close`) stay, and the hints before the last go first.
pub(crate) fn fit_hints(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }
    let mut starts: Vec<usize> = vec![0];
    for (i, _) in text.match_indices('[') {
        if i > 0 && text[..i].ends_with(' ') && !text[..i].trim().is_empty() {
            starts.push(i);
        }
    }
    let mut pieces: Vec<&str> = starts
        .iter()
        .enumerate()
        .map(|(n, &a)| &text[a..starts.get(n + 1).copied().unwrap_or(text.len())])
        .collect();
    let len = |p: &[&str]| p.iter().map(|s| s.chars().count()).sum::<usize>();
    while len(&pieces) > width && pieces.len() > 2 {
        pieces.remove(pieces.len() - 2);
    }
    pieces.concat()
}

fn draw_status_bar(f: &mut Frame, area: Rect, app: &App) {
    let fit = |hints: &str| fit_hints(hints, usize::from(area.width));
    let text = if let Some(st) = &app.search {
        let count = if st.matches.is_empty() {
            if st.query.is_empty() {
                String::new()
            } else {
                "no matches".into()
            }
        } else {
            format!("{}/{}", st.current + 1, st.matches.len())
        };
        Line::raw(format!(
            "Search: {}  {count}  [Enter] next  [Shift+Enter] prev  [Esc] close",
            st.query
        ))
    } else if let Some(notice) = &app.notice {
        let color = match notice.level {
            NoticeLevel::Info => Color::Cyan,
            NoticeLevel::Warn => Color::Yellow,
            NoticeLevel::Error => Color::Red,
        };
        Line::styled(notice.text.clone(), Style::default().fg(color))
    } else {
        match app.mode {
            Mode::Attached => Line::raw(
                "ATTACHED — Ctrl+Q detach · Ctrl+Shift+B toggle sidebar · Shift+↑/↓ scroll · Ctrl+Shift+C/V copy/paste · Ctrl+Shift+F search",
            ),
            Mode::Control => {
                if app.sidebar_hidden {
                    Line::raw(fit(
                        "[b] sidebar  [Tab] select  [Enter] attach  [n] new  [l] logs  [S] skills  [C] config  [t/T] trace  [?] help  [q] quit",
                    ))
                } else {
                    match app.sidebar_section {
                        SidebarSection::Active => Line::raw(fit(
                            "[b] side [Enter] attach [n] new [S] skills [C] config [X] clear exited [t/T] trace [?] help [q] quit",
                        )),
                        SidebarSection::Agents => Line::raw(fit(
                            "[b] sidebar  [Enter/h] launch agent  [Tab] loops  [n] new  [S] skills  [C] config  [?] help  [q] quit",
                        )),
                        SidebarSection::Loops => {
                            if app.loop_registry.pause_all {
                                Line::styled(
                                    fit(
                                        "‖ LOOPS PAUSED  [K] resume all  [Enter] details  [r] run now  [p] pause  [a] add  [e] edit  [x] remove",
                                    ),
                                    Style::default().fg(Color::Yellow),
                                )
                            } else {
                                Line::raw(fit(
                                    "[b] sidebar  [Enter] details  [r] run now  [p] pause  [a] add  [e] edit  [x] remove  [K] kill  [?] help",
                                ))
                            }
                        }
                        SidebarSection::Workflows => Line::raw(fit(
                            "[b] sidebar  [Enter] run / view  [c] compose  [e] edit  [x] cancel  [W] view  [K] kill  [?] help",
                        )),
                        SidebarSection::History => Line::raw(fit(
                            "[b] sidebar  [Enter/r] restart  [a] all  [n] new  [l] logs  [S] skills  [C] config  [?] help  [q] quit",
                        )),
                    }
                }
            }
            _ => Line::raw(fit(
                "[b] sidebar  [Enter] attach  [n] new  [l] logs  [S] skills  [C] config  [t/T] trace  [?] help  [q] quit",
            )),
        }
    };
    // What waits on a human leads the control-mode hints, so the inbox is
    // seen without opening it.
    let pending =
        if matches!(app.mode, Mode::Control) && app.search.is_none() && app.notice.is_none() {
            app.inbox_count()
        } else {
            0
        };
    let text = if pending > 0 {
        let badge = format!("● {pending} need you [I]  ");
        let rest: String = text.spans.iter().map(|s| s.content.as_ref()).collect();
        let rest = fit_hints(
            &rest,
            usize::from(area.width).saturating_sub(badge.chars().count()),
        );
        Line::from(vec![
            Span::styled(
                badge,
                Style::default()
                    .fg(Color::Magenta)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(rest, text.style),
        ])
    } else {
        text
    };
    f.render_widget(Paragraph::new(text), area);
    if let Some(st) = &app.search {
        let cursor_x =
            (area.x + 8 + st.query.len() as u16).min(area.x + area.width.saturating_sub(1));
        f.set_cursor_position((cursor_x, area.y));
    }
}

/// Full keybinding reference — the one place every chord (including the
/// otherwise invisible Ctrl+Shift ones) is written down in the UI.
fn draw_help(f: &mut Frame) {
    let platform_keys = platform_keys();
    let key_style = Style::default().fg(Color::Cyan);
    let dim = Style::default().fg(Color::DarkGray);
    let head = Style::default()
        .fg(Color::Yellow)
        .add_modifier(Modifier::BOLD);
    let row = |key: &str, desc: &str| {
        Line::from(vec![
            Span::styled(format!("  {key:<18}"), key_style),
            Span::raw(desc.to_string()),
        ])
    };
    let lines: Vec<Line> = vec![
        Line::styled("Control mode", head),
        row("b", "toggle sidebar (hide / full harness)"),
        row("j/k, ↑/↓", "select session"),
        row(
            "1-9, space",
            "jump to session N · fold the session's loop or workflow",
        ),
        row("Tab", "cycle active / agents / loops / history sections"),
        row(
            "Enter",
            "attach (active), launch agent (agents), or restart (history)",
        ),
        row("h", "launch / attach the selected agent (agents)"),
        row("S", "skills view: every skill, where installed, when used"),
        row(
            "C",
            "configuration: edit every prompt, skill, loop pattern, loop skill, agent and template",
        ),
        row("E / K", "loops view / kill switch: pause every loop"),
        row(
            "W / I",
            "workflows view · inbox: what waits on you, loops and workflows",
        ),
        row("v", "about: version, build time, paths and this session"),
        row(
            "n",
            "new session (pick the trace backend: SQLite, Langfuse, both)",
        ),
        row("l", "browse past session logs"),
        row("t / T", "toggle tracing / browse local traces"),
        row(
            "● ◆ ◈",
            "badge glyphs: traced locally, to Langfuse, to both",
        ),
        row("x / r", "kill session / respawn or restart"),
        row("X", "clear all exited sessions"),
        row("q", "quit"),
        Line::raw(""),
        Line::styled("Attached mode", head),
        row("Ctrl+Shift+B", "toggle sidebar / full-screen harness"),
        row("Ctrl+Q", "detach back to control mode"),
        row("Ctrl+Q Ctrl+Q", "send a literal Ctrl+Q to the agent"),
        Line::raw(""),
        Line::styled("Scrollback, selection & search", head),
        row("Shift+↑/↓", "scroll three lines"),
        row(platform_keys.page_scroll, "scroll one page"),
        row(platform_keys.word_navigation, "move by word in text fields"),
        row("Shift+Home/End", "jump to top / back to live"),
        row("mouse", "wheel to scroll, drag to select text"),
        row("Ctrl+Shift+C/V", "copy selection / paste"),
        row(
            "Ctrl+Shift+F",
            "search scrollback (plain Ctrl+F in control mode)",
        ),
        Line::raw(""),
        Line::styled("Session logs", head),
        row("Tab, ←/→", "switch pane"),
        row("a", "toggle this project / all projects"),
        row("r or Enter", "resume the selected session"),
        Line::raw(""),
        Line::styled("Workflows section", head),
        row(
            "Enter / c / e / x",
            "run (or view when live) · compose for a task · edit · cancel",
        ),
        Line::styled("Loops section", head),
        row(
            "Enter",
            "details (Loops view) · r run now · p pause / resume",
        ),
        row("a / e / x", "add a loop · edit it · remove it (files stay)"),
        row(
            "Loops view",
            "Tab or 1-3: Report · History · Setup · T traces",
        ),
        Line::raw(""),
        Line::styled("Skills view", head),
        row("Tab, ←/→", "next tab / focus the list or the detail pane"),
        row("1-3, c/x/a", "filter to one harness (again to clear)"),
        row("r", "rescan packages, install state and the store"),
        row("T", "open the selected execution's traces"),
        Line::raw(""),
        Line::styled("Trace browser", head),
        row("Tab, ←/→", "sessions → turns → detail"),
        row("Enter", "drill in / expand an observation"),
        row("v", "detail view: list → tree → timeline → loop"),
        row(
            "Space",
            "fold a loop or workflow run (sessions) / a subtree (tree view)",
        ),
        row("/", "full-text search (full mode content)"),
        row("a", "toggle this project / all projects"),
        row("s", "verdict: good → bad → cleared (also sent to Langfuse)"),
        row("r", "resume the selected session"),
        Line::raw(""),
        Line::styled("  [Esc] or [?] to close", dim),
        Line::styled(
            format!("  {}", crate::build_info::short()),
            Style::default().fg(Color::DarkGray),
        ),
    ];
    let height = (lines.len() as u16 + 2).min(f.area().height.saturating_sub(2));
    let width = 84.min(f.area().width.saturating_sub(4)).max(40);
    let area = centered(f.area(), width, height);
    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Keyboard & mouse reference ");
    f.render_widget(Paragraph::new(lines).block(block), area);
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let w = width.min(area.width);
    let h = height.min(area.height);
    Rect::new(
        area.x + (area.width - w) / 2,
        area.y + (area.height - h) / 2,
        w,
        h,
    )
}

fn draw_new_session_dialog(f: &mut Frame, dialog: &DialogState, app: &App) {
    let width = 72.min(f.area().width.saturating_sub(4)).max(40);
    // the launch options add six rows when the profile runs a known CLI
    let options_rows = if dialog.harness.is_some() { 6 } else { 0 }
        + if dialog.fields().contains(&DialogField::MaxCost) {
            4
        } else {
            0
        }
        + if dialog.fields().contains(&DialogField::Experiment) {
            4
        } else {
            0
        };
    let height = (app.profiles.len() as u16 + 20 + options_rows)
        .min(f.area().height.saturating_sub(2))
        .max(18);
    let area = centered(f.area(), width, height);
    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" New session ");
    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::raw("Profile:"));
    for (i, p) in app.profiles.iter().enumerate() {
        let marker = if i == dialog.profile_idx { "> " } else { "  " };
        let style = if i == dialog.profile_idx && matches!(dialog.field, DialogField::Profile) {
            Style::default().add_modifier(Modifier::REVERSED)
        } else {
            Style::default()
        };
        lines.push(Line::styled(format!("{marker}{}", p.name), style));
    }
    lines.push(Line::raw(""));
    // the subfolder list is the flexible part: on a short terminal it
    // shrinks so the fields below it stay on screen
    let fixed_rows = app.profiles.len()
        + 12
        + if dialog.harness.is_some() { 6 } else { 0 }
        + if dialog.fields().contains(&DialogField::MaxCost) {
            4
        } else {
            0
        }
        + if dialog.fields().contains(&DialogField::Experiment) {
            4
        } else {
            0
        };
    let max_visible = usize::from(height).saturating_sub(fixed_rows).clamp(1, 4);
    lines.extend(dir_picker_lines(
        "Directory: ",
        &dialog.dir,
        &dialog.dir_picker,
        matches!(dialog.field, DialogField::Dir),
        max_visible,
    ));
    lines.push(Line::raw(""));

    // Tracing Option
    let tracing_style = if matches!(dialog.field, DialogField::Tracing) {
        Style::default().add_modifier(Modifier::REVERSED)
    } else {
        Style::default()
    };
    let (trace_box, trace_color) = if dialog.tracing_enabled {
        ("[●] Enabled", Color::Cyan)
    } else {
        ("[○] Disabled", Color::DarkGray)
    };
    lines.push(Line::from(vec![
        Span::raw("Tracing:          "),
        Span::styled(trace_box, tracing_style.fg(trace_color)),
        Span::styled(" (Space to toggle)", Style::default().fg(Color::DarkGray)),
    ]));

    // Backend Option
    let backend_style = if matches!(dialog.field, DialogField::Backend) {
        Style::default().add_modifier(Modifier::REVERSED)
    } else {
        Style::default()
    };
    let backend_label = match dialog.backend {
        crate::config::Backend::Local => "[Local SQLite]",
        crate::config::Backend::Langfuse => "[Langfuse]",
        crate::config::Backend::Both => "[Both] SQLite + Langfuse",
    };
    let backend_hint = if dialog.langfuse_available {
        " (Space to cycle)"
    } else {
        " (Langfuse: not configured — see `agent-mux trace doctor`)"
    };
    lines.push(Line::from(vec![
        Span::raw("Backend:          "),
        Span::styled(backend_label, backend_style.fg(Color::Yellow)),
        Span::styled(backend_hint, Style::default().fg(Color::DarkGray)),
    ]));

    // Content Mode Option
    let mode_style = if matches!(dialog.field, DialogField::ContentMode) {
        Style::default().add_modifier(Modifier::REVERSED)
    } else {
        Style::default()
    };
    let mode_desc = match dialog.content_mode {
        DialogContentMode::Full => "[Full] Prompts, tool I/O, skills & subagents",
        DialogContentMode::Metadata => "[Metadata] Privacy-safe, token counts & timings only",
    };
    lines.push(Line::from(vec![
        Span::raw("Content Mode:     "),
        Span::styled(mode_desc, mode_style.fg(Color::Yellow)),
        Span::styled(" (Space to toggle)", Style::default().fg(Color::DarkGray)),
    ]));

    // Launch options, only for a command we know how to pass them to
    if let Some(harness) = dialog.harness {
        let focused = |f: DialogField| {
            if dialog.field == f {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            }
        };
        let dim = Style::default().fg(Color::DarkGray);
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            format!("── {} options ──", harness.as_str()),
            Style::default().fg(Color::DarkGray),
        ));

        let model = if dialog.model.is_empty() {
            "(the CLI's default)".to_string()
        } else {
            dialog.model.clone()
        };
        let model_style = if dialog.model.is_empty() {
            focused(DialogField::Model).fg(Color::DarkGray)
        } else {
            focused(DialogField::Model).fg(Color::Yellow)
        };
        lines.push(Line::from(vec![
            Span::raw("Model:            "),
            Span::styled(model, model_style),
            Span::styled(" (--model, blank = unset)", dim),
        ]));

        let (approvals, approvals_color) = if dialog.bypass_approvals {
            ("[!] bypass", Color::Red)
        } else {
            ("[●] normal", Color::Green)
        };
        lines.push(Line::from(vec![
            Span::raw("Approvals:        "),
            Span::styled(
                approvals,
                focused(DialogField::Approvals).fg(approvals_color),
            ),
            Span::styled(
                if dialog.bypass_approvals {
                    match harness {
                        crate::harness::Harness::Codex => " (--yolo)",
                        _ => " (--dangerously-skip-permissions)",
                    }
                } else {
                    " (Space to toggle)"
                },
                dim,
            ),
        ]));

        lines.push(Line::from(vec![
            Span::raw("Resume:           "),
            Span::styled(
                if dialog.resume_last {
                    "[Last session]"
                } else {
                    "[Off]"
                },
                focused(DialogField::Resume).fg(Color::Yellow),
            ),
            Span::styled(
                if dialog.resume_last {
                    match harness {
                        crate::harness::Harness::Codex => " (resume --last)",
                        _ => " (--continue)",
                    }
                } else {
                    " (Space to toggle)"
                },
                dim,
            ),
        ]));

        let one_shot = if dialog.one_shot.is_empty() {
            "(interactive)".to_string()
        } else {
            truncate_chars(&dialog.one_shot, 44)
        };
        let one_shot_style = if dialog.one_shot.is_empty() {
            focused(DialogField::OneShot).fg(Color::DarkGray)
        } else {
            focused(DialogField::OneShot).fg(Color::Yellow)
        };
        lines.push(Line::from(vec![
            Span::raw("One-shot prompt:  "),
            Span::styled(one_shot, one_shot_style),
            Span::styled(
                match harness {
                    crate::harness::Harness::Codex => " (codex exec)",
                    _ => " (-p, blank = unset)",
                },
                dim,
            ),
        ]));
    }

    // Budget guard, when the CLI's PreToolUse hook can enforce one
    if dialog.fields().contains(&DialogField::MaxCost) {
        let focused = |f: DialogField| {
            if dialog.field == f {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            }
        };
        let dim = Style::default().fg(Color::DarkGray);
        lines.push(Line::raw(""));
        lines.push(Line::styled("── budget guard ──", dim));
        let text = |value: &str, field: DialogField| {
            if value.is_empty() {
                ("(none)".to_string(), focused(field).fg(Color::DarkGray))
            } else {
                (value.to_string(), focused(field).fg(Color::Yellow))
            }
        };
        let (cost, cost_style) = text(&dialog.max_cost, DialogField::MaxCost);
        lines.push(Line::from(vec![
            Span::raw("Max cost (USD):   "),
            Span::styled(cost, cost_style),
            Span::styled(" (blocks the next tool call past it)", dim),
        ]));
        let (turns, turns_style) = text(&dialog.max_turns, DialogField::MaxTurns);
        lines.push(Line::from(vec![
            Span::raw("Max turns:        "),
            Span::styled(turns, turns_style),
        ]));
    }

    // Experiment link, when a store is there to record it
    if dialog.fields().contains(&DialogField::Experiment) {
        let focused = |f: DialogField| {
            if dialog.field == f {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            }
        };
        let dim = Style::default().fg(Color::DarkGray);
        lines.push(Line::raw(""));
        lines.push(Line::styled("── experiment ──", dim));
        let (experiment, experiment_style) = if dialog.experiment.is_empty() {
            (
                "(none)".to_string(),
                focused(DialogField::Experiment).fg(Color::DarkGray),
            )
        } else {
            (
                truncate_chars(&dialog.experiment, 32),
                focused(DialogField::Experiment).fg(Color::Yellow),
            )
        };
        lines.push(Line::from(vec![
            Span::raw("Experiment:       "),
            Span::styled(experiment, experiment_style),
            Span::styled(" (records this launch as a run)", dim),
        ]));
        let (variant, variant_style) = if dialog.variant.is_empty() {
            (
                "interactive".to_string(),
                focused(DialogField::Variant).fg(Color::DarkGray),
            )
        } else {
            (
                truncate_chars(&dialog.variant, 32),
                focused(DialogField::Variant).fg(Color::Yellow),
            )
        };
        lines.push(Line::from(vec![
            Span::raw("Variant:          "),
            Span::styled(variant, variant_style),
        ]));
    }

    if let Some(err) = &dialog.error {
        lines.push(Line::styled(err.clone(), Style::default().fg(Color::Red)));
    }
    lines.push(Line::raw(""));
    lines.push(Line::raw(
        "[Tab] switch field  [Space] toggle  [Enter] launch  [Esc] cancel",
    ));
    f.render_widget(Paragraph::new(lines).block(block), area);
}

fn draw_session_history(f: &mut Frame, history: &HistoryState, _app: &App) {
    let width = (f.area().width * 95 / 100).clamp(60, 140);
    let height = (f.area().height * 90 / 100).clamp(18, 45);
    let area = centered(f.area(), width, height);
    f.render_widget(Clear, area);

    let [body, footer] = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(area);
    let [left, right] =
        Layout::horizontal([Constraint::Percentage(38), Constraint::Min(0)]).areas(body);

    // Left pane: Session List
    let left_border_style = if matches!(history.focused_pane, HistoryPane::SessionsList) {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let scope_label = if history.all_projects {
        "all projects"
    } else {
        "this project"
    };
    let left_title = format!(
        " Past Sessions ({}) [a: {scope_label}] ",
        history.sessions.len()
    );
    let left_block = Block::default()
        .borders(Borders::ALL)
        .border_style(left_border_style)
        .title(left_title);

    if history.sessions.is_empty() {
        let hint =
            Paragraph::new("\n  No past sessions found.\n\n  Press [a] to search all projects.")
                .style(Style::default().fg(Color::DarkGray))
                .block(left_block);
        f.render_widget(hint, left);
    } else {
        let items: Vec<ListItem> = history
            .sessions
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let is_sel = i == history.selected_session_idx;
                let marker = if is_sel { "> " } else { "  " };
                let badge = match s.provider {
                    crate::history::AgentProvider::Claude => {
                        Span::styled("[Claude] ", Style::default().fg(Color::LightMagenta))
                    }
                    crate::history::AgentProvider::Antigravity => {
                        Span::styled("[AGY]    ", Style::default().fg(Color::LightCyan))
                    }
                };
                let title_str = truncate_chars(&s.title, 18);
                let line = Line::from(vec![
                    Span::raw(marker),
                    badge,
                    Span::raw(format!("{:<16} ", s.timestamp_str)),
                    Span::styled(
                        title_str,
                        Style::default().fg(if is_sel { Color::Yellow } else { Color::White }),
                    ),
                ]);
                let item = ListItem::new(line);
                if is_sel && matches!(history.focused_pane, HistoryPane::SessionsList) {
                    item.style(Style::default().add_modifier(Modifier::REVERSED))
                } else if is_sel {
                    item.style(Style::default().fg(Color::Yellow))
                } else {
                    item
                }
            })
            .collect();
        f.render_widget(List::new(items).block(left_block), left);
    }

    // Right pane: Log Details
    let right_border_style = if matches!(history.focused_pane, HistoryPane::LogDetail) {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let right_title = if let Some(s) = history.sessions.get(history.selected_session_idx) {
        let id_short: String = s.session_id.chars().take(8).collect();
        let scroll_tag = if history.scroll_offset > 0 {
            format!(
                " [↑ {}/{}] ",
                history.scroll_offset,
                history.log_lines.len()
            )
        } else {
            String::new()
        };
        let provider_tag = match s.provider {
            crate::history::AgentProvider::Claude => "Claude",
            crate::history::AgentProvider::Antigravity => "Antigravity",
        };
        format!(
            " Log [{provider_tag}]: {} ({id_short}){scroll_tag} ",
            s.title
        )
    } else {
        " Log ".to_string()
    };
    let right_block = Block::default()
        .borders(Borders::ALL)
        .border_style(right_border_style)
        .title(right_title);

    let inner_right = right_block.inner(right);
    f.render_widget(right_block, right);
    // hand the real viewport height back so scrolling clamps to a full page
    history.viewport_rows.set(usize::from(inner_right.height));

    if let Some(err) = &history.error {
        let p = Paragraph::new(Line::styled(err.clone(), Style::default().fg(Color::Red)));
        f.render_widget(p, inner_right);
    } else if history.log_lines.is_empty() {
        let p = Paragraph::new(Line::styled(
            "  (No log output for this session)",
            Style::default().fg(Color::DarkGray),
        ));
        f.render_widget(p, inner_right);
    } else {
        let start = history
            .scroll_offset
            .min(history.log_lines.len().saturating_sub(1));
        let visible: Vec<Line> = history.log_lines[start..].to_vec();
        let p = Paragraph::new(visible);
        f.render_widget(p, inner_right);
    }

    // Footer line
    let footer_text = Line::styled(
        " [Tab] switch pane  [↑/↓/PgUp/PgDn] select/scroll  [a] toggle all projects  [r/Enter] resume  [Esc] close",
        Style::default().fg(Color::Black).bg(Color::Cyan),
    );
    f.render_widget(Paragraph::new(footer_text), footer);
}

/// `[● TRACE]` until the first live rollup arrives, then turns + cost (or
/// tokens when nothing is priced); the verbose form adds the running tool.
/// The backend a live session's launch chose (local when untraced).
fn session_backend(session: &crate::session::Session) -> crate::config::Backend {
    session
        .trace
        .as_ref()
        .map(|t| t.backend)
        .unwrap_or_default()
}

pub fn trace_badge(
    stats: Option<&LaunchStats>,
    verbose: bool,
    backend: crate::config::Backend,
) -> String {
    let glyph = match backend {
        crate::config::Backend::Local => "●",
        crate::config::Backend::Langfuse => "◆",
        crate::config::Backend::Both => "◈",
    };
    let Some(stats) = stats else {
        return format!("[{glyph} TRACE]");
    };
    let money = match (stats.cost_usd, stats.total_tokens) {
        (Some(c), _) if c > 0.0 => fmt_cost(Some(c)),
        (_, Some(t)) if t > 0 => format!("{} tok", fmt_tokens(Some(t))),
        _ => String::new(),
    };
    let mut badge = format!("[{glyph} {}t", stats.turns);
    if !money.is_empty() {
        badge.push(' ');
        badge.push_str(&money);
    }
    if verbose && let Some(tool) = &stats.running_tool {
        badge.push_str(" ▸ ");
        badge.push_str(&truncate_chars(tool, 24));
    }
    badge.push(']');
    badge
}

fn pane_border(focused: bool) -> Style {
    if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    }
}

fn provider_badge(provider: &str) -> Span<'static> {
    match provider {
        "claude" => Span::styled("[C] ", Style::default().fg(Color::LightMagenta)),
        "codex" => Span::styled("[X] ", Style::default().fg(Color::LightGreen)),
        "antigravity" => Span::styled("[A] ", Style::default().fg(Color::LightCyan)),
        _ => Span::raw("[?] "),
    }
}

fn draw_trace_browser(f: &mut Frame, browser: &TraceBrowserState) {
    let width = (f.area().width * 96 / 100).clamp(60, 200);
    let height = (f.area().height * 92 / 100).clamp(18, 60);
    let area = centered(f.area(), width, height);
    f.render_widget(Clear, area);
    let [body, footer] = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(area);
    let [left, middle, right] = Layout::horizontal([
        Constraint::Percentage(26),
        Constraint::Percentage(40),
        Constraint::Min(0),
    ])
    .areas(body);

    // Sessions
    let scope = if browser.all_projects {
        "all projects"
    } else {
        "this project"
    };
    let left_block = Block::default()
        .borders(Borders::ALL)
        .border_style(pane_border(browser.focused == BrowserPane::Sessions))
        .title(format!(
            " Sessions ({}) [a: {scope}] ",
            browser.sessions.len()
        ));
    if let Some(err) = &browser.error {
        let p = Paragraph::new(vec![
            Line::raw(""),
            Line::styled(format!("  {err}"), Style::default().fg(Color::Red)),
            Line::raw(""),
            Line::styled(
                "  Traces are written while sessions run; see `agent-mux trace doctor`.",
                Style::default().fg(Color::DarkGray),
            ),
        ])
        .block(left_block);
        f.render_widget(p, left);
    } else if browser.sessions.is_empty() {
        let p = Paragraph::new("\n  No traced sessions yet.\n\n  Press [a] to show all projects, or\n  `agent-mux trace import --discover`\n  to backfill past transcripts.")
            .style(Style::default().fg(Color::DarkGray))
            .block(left_block);
        f.render_widget(p, left);
    } else {
        // The same tree the Active sidebar draws, over traced sessions:
        // each loop and each workflow run is a parent over its calls.
        let rows = browser.session_rows();
        let cursor = crate::tree::row_of(&rows, browser.selected_session).unwrap_or(0);
        let visible = usize::from(left.height.saturating_sub(2));
        let start = sidebar_window(cursor, rows.len(), visible);
        let end = (start + visible.max(1)).min(rows.len());
        let items: Vec<ListItem> = rows[start..end]
            .iter()
            .enumerate()
            .map(|(offset, row)| traced_session_row(browser, row, start + offset == cursor))
            .collect();
        f.render_widget(List::new(items).block(left_block), left);
    }

    // Turns
    let middle_title = match &browser.search_query {
        Some(q) => format!(
            " Search: {} ({}) ",
            truncate_chars(q, 24),
            browser.turns.len()
        ),
        None => match browser.sessions.get(browser.selected_session) {
            Some(s) => {
                let directory = s
                    .cwd
                    .as_deref()
                    .map(|cwd| format!(" · cwd {}", truncate_path_chars(cwd, 28)))
                    .unwrap_or_default();
                format!(
                    " Turns ({})  {} tok  {}{} ",
                    browser.turns.len(),
                    fmt_tokens(s.total_tokens),
                    fmt_cost(s.total_cost_usd),
                    directory
                )
            }
            None => " Turns ".to_string(),
        },
    };
    let middle_block = Block::default()
        .borders(Borders::ALL)
        .border_style(pane_border(browser.focused == BrowserPane::Turns))
        .title(middle_title);
    if browser.turns.is_empty() {
        let p = Paragraph::new(Line::styled(
            "  (no turns)",
            Style::default().fg(Color::DarkGray),
        ))
        .block(middle_block);
        f.render_widget(p, middle);
    } else {
        let visible = usize::from(middle.height.saturating_sub(2));
        let start = sidebar_window(browser.selected_turn, browser.turns.len(), visible);
        let end = (start + visible.max(1)).min(browser.turns.len());
        let items: Vec<ListItem> = browser.turns[start..end]
            .iter()
            .enumerate()
            .map(|(offset, t)| {
                let i = start + offset;
                let is_sel = i == browser.selected_turn;
                let status_style = match t.status.as_str() {
                    "open" => Style::default().fg(Color::Green),
                    "aborted" => Style::default().fg(Color::Red),
                    _ => Style::default().fg(Color::DarkGray),
                };
                let errors = if t.error_count > 0 {
                    Span::styled(
                        format!(" {}!", t.error_count),
                        Style::default().fg(Color::Red),
                    )
                } else {
                    Span::raw("")
                };
                let name = t
                    .name
                    .split_once(": ")
                    .map(|(_, rest)| rest.to_string())
                    .unwrap_or_else(|| t.name.clone());
                let line = Line::from(vec![
                    Span::raw(if is_sel { "> " } else { "  " }),
                    Span::styled(
                        format!("#{:<3}", t.ordinal),
                        Style::default().fg(Color::DarkGray),
                    ),
                    Span::styled(format!("{:<7} ", t.status), status_style),
                    Span::styled(
                        format!(
                            "{:>6} {:>7} {:>2}🔧",
                            fmt_ms(t.latency_ms),
                            fmt_cost(t.total_cost_usd),
                            t.tool_count
                        ),
                        Style::default().fg(Color::Yellow),
                    ),
                    // the loop line: retries, only when there were any
                    Span::styled(
                        if t.retries > 0 {
                            format!(" {}↻", t.retries)
                        } else {
                            String::new()
                        },
                        Style::default().fg(Color::Magenta),
                    ),
                    // loop warnings and the guard, then the verdict
                    Span::styled(
                        if crate::tracing::loops::warning_kinds(&t.metadata).is_empty() {
                            String::new()
                        } else {
                            " ⚠".to_string()
                        },
                        Style::default().fg(Color::Magenta),
                    ),
                    match browser.scores.get(&t.id) {
                        Some(v) if *v >= 0.5 => {
                            Span::styled(" ✓", Style::default().fg(Color::Green))
                        }
                        Some(_) => Span::styled(" ✗", Style::default().fg(Color::Red)),
                        None => Span::raw(""),
                    },
                    errors,
                    Span::raw(format!(" {}", truncate_chars(&name, 48))),
                ]);
                let item = ListItem::new(line);
                if is_sel && browser.focused == BrowserPane::Turns {
                    item.style(Style::default().add_modifier(Modifier::REVERSED))
                } else if is_sel {
                    item.style(Style::default().fg(Color::Yellow))
                } else {
                    item
                }
            })
            .collect();
        f.render_widget(List::new(items).block(middle_block), middle);
    }

    // Detail
    let right_title = match browser.turns.get(browser.selected_turn) {
        Some(t) if browser.expanded => format!(
            " Turn #{} · observation {}/{} [Esc] back ",
            t.ordinal,
            browser.selected_observation + 1,
            browser.observations.len()
        ),
        Some(t) => format!(
            " Turn #{} · {} · {} obs · {} · {} tok · {} ",
            t.ordinal,
            browser.detail_view.label(),
            browser.observations.len(),
            fmt_ms(t.latency_ms),
            fmt_tokens(t.total_tokens),
            fmt_cost(t.total_cost_usd)
        ),
        None => " Detail ".to_string(),
    };
    let right_block = Block::default()
        .borders(Borders::ALL)
        .border_style(pane_border(browser.focused == BrowserPane::Detail))
        .title(right_title);
    let inner_right = right_block.inner(right);
    f.render_widget(right_block, right);
    browser.viewport_rows.set(usize::from(inner_right.height));
    if browser.expanded {
        let start = browser
            .scroll_offset
            .min(browser.detail_lines.len().saturating_sub(1));
        let visible: Vec<Line> = browser.detail_lines[start..].to_vec();
        f.render_widget(Paragraph::new(visible), inner_right);
    } else if browser.observations.is_empty() {
        let hint = match browser.turns.get(browser.selected_turn) {
            Some(t) if t.input.is_some() => {
                let mut lines = vec![Line::styled(
                    "── input ──",
                    Style::default().fg(Color::Yellow),
                )];
                for l in t.input.as_deref().unwrap_or("").lines().take(50) {
                    lines.push(Line::raw(l.to_string()));
                }
                lines
            }
            _ => vec![Line::styled(
                "  (no observations)",
                Style::default().fg(Color::DarkGray),
            )],
        };
        f.render_widget(Paragraph::new(hint), inner_right);
    } else if browser.detail_view == crate::app::DetailView::Tree {
        draw_observation_tree(f, browser, inner_right);
    } else if browser.detail_view == crate::app::DetailView::Timeline {
        draw_observation_timeline(f, browser, inner_right);
    } else if browser.detail_view == crate::app::DetailView::Loop {
        draw_loop_view(f, browser, inner_right);
    } else if browser.detail_view == crate::app::DetailView::Summary {
        draw_turn_summary(f, browser, inner_right);
    } else {
        let visible = usize::from(inner_right.height);
        let start = sidebar_window(
            browser.selected_observation,
            browser.observations.len(),
            visible,
        );
        let end = (start + visible.max(1)).min(browser.observations.len());
        let items: Vec<ListItem> = browser.observations[start..end]
            .iter()
            .enumerate()
            .map(|(offset, o)| {
                let i = start + offset;
                let is_sel = i == browser.selected_observation;
                let glyph = match (o.obs_type.as_str(), o.end_ns.is_some()) {
                    ("generation", _) => "💬",
                    ("agent", _) => "🤖",
                    (_, false) => "⏳",
                    _ => "🔧",
                };
                let duration = o
                    .end_ns
                    .map(|e| fmt_ms((e - o.start_ns) / 1_000_000))
                    .unwrap_or_else(|| "running".into());
                let level_style = match o.level.as_str() {
                    "ERROR" => Style::default().fg(Color::Red),
                    "WARNING" => Style::default().fg(Color::Yellow),
                    _ => Style::default().fg(Color::DarkGray),
                };
                let line = Line::from(vec![
                    Span::raw(if is_sel { "> " } else { "  " }),
                    Span::styled(
                        format!("{} ", &fmt_time(o.start_ns)[11..]),
                        Style::default().fg(Color::DarkGray),
                    ),
                    Span::raw(format!("{glyph} ")),
                    Span::raw(truncate_chars(&obs_display_name(o), 28)),
                    Span::styled(
                        format!(
                            " {:>7} {:>6} {:>7}",
                            duration,
                            fmt_tokens(o.total_tokens),
                            fmt_cost(o.total_cost_usd)
                        ),
                        level_style,
                    ),
                ]);
                let item = ListItem::new(line);
                if is_sel && browser.focused == BrowserPane::Detail {
                    item.style(Style::default().add_modifier(Modifier::REVERSED))
                } else {
                    item
                }
            })
            .collect();
        f.render_widget(List::new(items), inner_right);
    }

    let footer_text = match &browser.search_input {
        Some(input) => Line::styled(
            format!(" Search: {input}▏  [Enter] run  [Esc] cancel"),
            Style::default().fg(Color::Black).bg(Color::Yellow),
        ),
        None => Line::styled(
            " [Tab] pane  [↑/↓] select  [Enter] drill  [v] view  [space] fold run/subtree  [/] search  [s] score  [a] all  [r] resume  [Esc] close",
            Style::default().fg(Color::Black).bg(Color::Cyan),
        ),
    };
    f.render_widget(Paragraph::new(footer_text), footer);
}

/// One row of the trace browser's Sessions tree: a loop or workflow
/// header, or a traced session under it.
fn traced_session_row<'a>(
    browser: &'a TraceBrowserState,
    row: &crate::tree::Row,
    is_sel: bool,
) -> ListItem<'a> {
    let marker = if is_sel { "> " } else { "  " };
    let spans = match row {
        crate::tree::Row::Group {
            kind,
            title,
            detail,
            members,
            collapsed,
            ..
        } => {
            // A header carries the family's totals, so a folded run still
            // says what it cost.
            let (mut cost, mut turns, mut live, mut seen) = (0.0f64, 0i64, 0i64, 0i64);
            for s in members.iter().filter_map(|i| browser.sessions.get(*i)) {
                cost += s.total_cost_usd.unwrap_or(0.0);
                turns += s.turn_count;
                live += s.open_turns;
                seen = seen.max(s.last_seen_ns);
            }
            vec![
                Span::raw(marker.to_string()),
                Span::styled(
                    if *collapsed { "▸" } else { "▾" }.to_string(),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(
                    format!("{} ", kind.glyph()),
                    Style::default().fg(Color::Magenta),
                ),
                Span::styled(
                    format!("{} ", &fmt_time(seen)[5..16]),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(
                    format!("{turns}t {} ", fmt_cost(Some(cost))),
                    Style::default().fg(Color::Yellow),
                ),
                Span::styled(
                    if live > 0 { "● " } else { "" }.to_string(),
                    Style::default().fg(Color::Green),
                ),
                Span::styled(
                    truncate_chars(title, 22),
                    Style::default()
                        .fg(Color::Magenta)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!(" {}·{}", members.len(), truncate_chars(detail, 12)),
                    Style::default().fg(Color::DarkGray),
                ),
            ]
        }
        crate::tree::Row::Item { index, depth, last } => {
            let Some(s) = browser.sessions.get(*index) else {
                return ListItem::new(Line::raw(String::new()));
            };
            // Under a header the step or run label says more than the
            // transcript title, which is the same prompt for every step.
            let title = match browser.groups.get(&s.key) {
                Some(g) if *depth > 0 && !g.label.is_empty() => g.label.clone(),
                _ => s
                    .title
                    .clone()
                    .or_else(|| s.cwd.clone())
                    .unwrap_or_else(|| s.session_id.clone()),
            };
            let mut spans = vec![Span::raw(marker.to_string())];
            // Under a header the date is the header's, repeated on every
            // child; the columns go to the step's own name instead.
            if *depth > 0 {
                spans.push(Span::styled(
                    if *last { " └ " } else { " ├ " }.to_string(),
                    Style::default().fg(Color::DarkGray),
                ));
            }
            spans.push(provider_badge(&s.provider));
            if *depth == 0 {
                spans.push(Span::styled(
                    format!("{} ", &fmt_time(s.last_seen_ns)[5..16]),
                    Style::default().fg(Color::DarkGray),
                ));
            }
            spans.push(Span::styled(
                format!("{}t {} ", s.turn_count, fmt_cost(s.total_cost_usd)),
                Style::default().fg(Color::Yellow),
            ));
            spans.push(Span::styled(
                if s.open_turns > 0 { "● " } else { "" }.to_string(),
                Style::default().fg(Color::Green),
            ));
            spans.push(Span::raw(truncate_chars(
                &title,
                if *depth > 0 { 34 } else { 40 },
            )));
            spans
        }
    };
    let item = ListItem::new(Line::from(spans));
    if is_sel && browser.focused == BrowserPane::Sessions {
        item.style(Style::default().add_modifier(Modifier::REVERSED))
    } else if is_sel {
        item.style(Style::default().fg(Color::Yellow))
    } else {
        item
    }
}

/// The loop's numbers for the selected turn, on one screen.
fn draw_loop_view(f: &mut Frame, browser: &TraceBrowserState, area: Rect) {
    let Some(turn) = browser.turns.get(browser.selected_turn) else {
        return;
    };
    let m = crate::tracing::loops::loop_metrics(turn, &browser.observations);
    let dim = Style::default().fg(Color::DarkGray);
    let head = |s: &str| Line::styled(format!("── {s} ──"), Style::default().fg(Color::Yellow));
    let kv = |k: &str, v: String| {
        Line::from(vec![Span::styled(format!("  {k:<14}"), dim), Span::raw(v)])
    };
    let pct = |part: i64, whole: i64| {
        if whole > 0 {
            format!("{:>3}%", part * 100 / whole)
        } else {
            "  -%".to_string()
        }
    };
    let mut lines = vec![head("calls")];
    lines.push(kv(
        "tool calls",
        format!("{} ({} distinct)", m.tool_calls, m.distinct_tools),
    ));
    lines.push(kv(
        "retries",
        if m.retried_tools.is_empty() {
            "0".to_string()
        } else {
            let named: Vec<String> = m
                .retried_tools
                .iter()
                .map(|(n, c)| format!("{n} ×{c}"))
                .collect();
            format!("{}  ({})", m.retries, named.join(", "))
        },
    ));
    lines.push(kv("errors", m.tool_errors.to_string()));
    lines.push(kv("declined", m.declined.to_string()));

    lines.push(Line::raw(""));
    lines.push(head("time"));
    let total = m.model_ms + m.tool_ms + m.idle_ms;
    let track = usize::from(area.width).saturating_sub(20).clamp(10, 60);
    let cells = |part: i64| {
        if total > 0 {
            ((part as f64 / total as f64) * track as f64).round() as usize
        } else {
            0
        }
    };
    let (mc, tc) = (cells(m.model_ms), cells(m.tool_ms));
    let ic = track.saturating_sub(mc + tc);
    lines.push(Line::from(vec![
        Span::raw("  "),
        Span::styled("█".repeat(mc), Style::default().fg(Color::Blue)),
        Span::styled("▓".repeat(tc), Style::default()),
        Span::styled("░".repeat(ic), dim),
    ]));
    lines.push(kv(
        "model",
        format!("{:>8} {}", fmt_ms(m.model_ms), pct(m.model_ms, total)),
    ));
    lines.push(kv(
        "tools",
        format!("{:>8} {}", fmt_ms(m.tool_ms), pct(m.tool_ms, total)),
    ));
    lines.push(kv(
        "idle",
        format!("{:>8} {}", fmt_ms(m.idle_ms), pct(m.idle_ms, total)),
    ));

    lines.push(Line::raw(""));
    lines.push(head("context"));
    lines.push(kv(
        "first → last",
        match (m.context_first, m.context_last) {
            (Some(a), Some(b)) => format!(
                "{} → {}  ({}{})",
                fmt_tokens(Some(a)),
                fmt_tokens(Some(b)),
                if b >= a { "+" } else { "" },
                fmt_tokens(Some(b - a))
            ),
            _ => "no usage reported".to_string(),
        },
    ));
    lines.push(kv(
        "cache ratio",
        m.cache_ratio
            .map(|r| format!("{:.0}%", r * 100.0))
            .unwrap_or_else(|| "-".to_string()),
    ));
    lines.push(kv("compactions", m.compactions.to_string()));
    let warnings = crate::tracing::loops::warnings_of(&turn.metadata);
    if !warnings.is_empty() {
        lines.push(Line::raw(""));
        lines.push(head("warnings"));
        for (kind, detail) in warnings {
            lines.push(Line::from(vec![
                Span::styled(
                    format!("  ⚠ {kind:<13}"),
                    Style::default().fg(Color::Magenta),
                ),
                Span::raw(detail),
            ]));
        }
    }

    lines.push(Line::raw(""));
    lines.push(head("subagents"));
    lines.push(kv(
        "invocations",
        format!(
            "{}  ·  {} tok  ·  {}",
            m.subagents,
            fmt_tokens(Some(m.subagent_tokens).filter(|t| *t > 0)),
            fmt_cost(Some(m.subagent_cost).filter(|c| *c > 0.0))
        ),
    ));
    f.render_widget(Paragraph::new(lines), area);
}

/// The turn's bill on one screen: tokens by kind and cost two to a row,
/// then every tool with its calls and errors. Tools past the pane's last
/// row fold into one row that carries what it hides.
fn draw_turn_summary(f: &mut Frame, browser: &TraceBrowserState, area: Rect) {
    let Some(turn) = browser.turns.get(browser.selected_turn) else {
        return;
    };
    let s = trace_view::turn_summary(turn, &browser.observations);
    let dim = Style::default().fg(Color::DarkGray);
    let red = Style::default().fg(Color::Red);
    let head = |s: String| Line::styled(format!("── {s} ──"), Style::default().fg(Color::Yellow));
    let plural = |n: usize, what: &str| format!("{n} {what}{}", if n == 1 { "" } else { "s" });
    let errors = |n: usize| {
        if n > 0 {
            format!(" · {}", plural(n, "error"))
        } else {
            String::new()
        }
    };
    let cell = |k: &str, v: String| {
        vec![
            Span::styled(format!("  {k:<12}"), dim),
            Span::raw(format!("{v:>8}")),
        ]
    };

    let mut tokens_head = format!("tokens · {}", plural(s.generations, "generation"));
    if s.unpriced_generations > 0 {
        tokens_head.push_str(&format!(" · {} unpriced", s.unpriced_generations));
    }
    let mut lines = vec![head(tokens_head)];
    let cells = [
        ("input", fmt_tokens(s.input_tokens)),
        ("output", fmt_tokens(s.output_tokens)),
        ("cache read", fmt_tokens(s.cache_read_tokens)),
        ("cache write", fmt_tokens(s.cache_write_tokens)),
        ("total", fmt_tokens(s.total_tokens)),
        ("cost", fmt_cost(s.cost_usd)),
    ];
    // two cells to a row when both fit, so the tools keep the rows
    let per_row = if area.width >= 44 { 2 } else { 1 };
    for row in cells.chunks(per_row) {
        lines.push(Line::from(
            row.iter()
                .flat_map(|(k, v)| cell(k, v.clone()))
                .collect::<Vec<_>>(),
        ));
    }
    lines.push(head(format!(
        "tools · {} · {} distinct{}",
        plural(s.tool_calls, "call"),
        s.tools.len(),
        errors(s.tool_errors)
    )));

    let rows = usize::from(area.height).saturating_sub(lines.len());
    let (shown, folded) = trace_view::fit_tools(&s.tools, rows);
    let width = s
        .tools
        .iter()
        .map(|t| t.name.chars().count())
        .max()
        .unwrap_or(0)
        .clamp(12, 40);
    for t in shown {
        let mut spans = vec![
            Span::raw(format!("  {:<width$} ", truncate_chars(&t.name, width))),
            Span::styled(
                format!("×{:<4}", t.calls),
                Style::default().fg(Color::Yellow),
            ),
        ];
        if t.errors > 0 {
            spans.push(Span::styled(format!("✗{}", t.errors), red));
        }
        lines.push(Line::from(spans));
    }
    if let Some(rest) = folded {
        lines.push(Line::styled(
            format!(
                "  … +{} more · {}{}",
                rest.tools,
                plural(rest.calls, "call"),
                errors(rest.errors)
            ),
            dim,
        ));
    }
    f.render_widget(Paragraph::new(lines), area);
}

/// Colour for an observation row, shared by both new views.
fn obs_style(o: &crate::tracing::store::query::ObservationView) -> Style {
    if o.level == "ERROR" {
        return Style::default().fg(Color::Red);
    }
    match o.level.as_str() {
        "WARNING" => Style::default().fg(Color::Yellow),
        _ => match o.obs_type.as_str() {
            "generation" => Style::default().fg(Color::Blue),
            "agent" => Style::default().fg(Color::Cyan),
            _ => Style::default(),
        },
    }
}

fn obs_glyph(o: &crate::tracing::store::query::ObservationView) -> &'static str {
    match (o.obs_type.as_str(), o.end_ns.is_some()) {
        ("generation", _) => "💬",
        ("agent", _) => "🤖",
        (_, false) => "⏳",
        _ => "🔧",
    }
}

/// A Task/Agent invocation and the transcript produced by that subagent are
/// separate observations. Name them distinctly so they do not look like a
/// duplicate event in the browser.
fn obs_display_name(o: &crate::tracing::store::query::ObservationView) -> String {
    if o.obs_type != "agent" {
        return o.name.clone();
    }
    match o.name.strip_prefix("agent: ") {
        Some(name) => format!("launch agent: {name}"),
        None => format!("subagent: {}", o.name),
    }
}

/// The hierarchy: connectors from the depth sequence, folded subtrees
/// reporting what they hide.
fn draw_observation_tree(f: &mut Frame, browser: &TraceBrowserState, area: Rect) {
    let tree = crate::tracing::store::query::nest_observations(browser.observations.clone());
    let rows = trace_view::tree_rows(&tree, &browser.collapsed);
    let selected = rows
        .iter()
        .position(|r| r.obs.id == browser.observations[browser.selected_observation].id)
        .unwrap_or(0);
    let height = usize::from(area.height);
    let start = sidebar_window(selected, rows.len(), height);
    let end = (start + height.max(1)).min(rows.len());
    let width = usize::from(area.width);
    let items: Vec<ListItem> = rows[start..end]
        .iter()
        .enumerate()
        .map(|(offset, r)| {
            let i = start + offset;
            let is_sel = i == selected;
            let fold = if !r.has_children {
                "  "
            } else if r.collapsed {
                "▸ "
            } else {
                "▾ "
            };
            let metrics = if r.collapsed {
                format!(
                    " +{} {:>6} {:>7}",
                    r.hidden,
                    fmt_tokens(Some(r.subtree.tokens).filter(|t| *t > 0)),
                    fmt_cost(Some(r.subtree.cost).filter(|c| *c > 0.0))
                )
            } else {
                format!(
                    " {:>7} {:>6} {:>7}",
                    r.obs
                        .end_ns
                        .map(|e| fmt_ms((e - r.obs.start_ns) / 1_000_000))
                        .unwrap_or_else(|| "running".into()),
                    fmt_tokens(r.obs.total_tokens),
                    fmt_cost(r.obs.total_cost_usd)
                )
            };
            // the name takes whatever the connectors and metrics leave,
            // padded so the metrics stay in one column down the pane
            let budget = width
                .saturating_sub(r.prefix.chars().count() + metrics.chars().count() + 6)
                .max(8);
            let name = truncate_chars(&obs_display_name(r.obs), budget);
            let line = Line::from(vec![
                Span::raw(if is_sel { ">" } else { " " }),
                Span::styled(fold.to_string(), Style::default().fg(Color::DarkGray)),
                Span::styled(r.prefix.clone(), Style::default().fg(Color::DarkGray)),
                Span::raw(format!("{} ", obs_glyph(r.obs))),
                Span::styled(format!("{name:<budget$}"), obs_style(r.obs)),
                Span::styled(metrics, Style::default().fg(Color::DarkGray)),
            ]);
            let item = ListItem::new(line);
            if is_sel && browser.focused == BrowserPane::Detail {
                item.style(Style::default().add_modifier(Modifier::REVERSED))
            } else {
                item
            }
        })
        .collect();
    f.render_widget(List::new(items), area);
}

/// Time, proportional to the turn: an axis, then one bar per observation.
fn draw_observation_timeline(f: &mut Frame, browser: &TraceBrowserState, area: Rect) {
    let turn = browser.turns.get(browser.selected_turn);
    let now_ns = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0);
    let window = trace_view::window(
        turn.map(|t| t.start_ns).unwrap_or(0),
        turn.and_then(|t| t.end_ns),
        &browser.observations,
        now_ns,
    );
    // label column, then the track
    let label_cols = usize::from(area.width).saturating_sub(4).clamp(6, 22);
    let track_cols = usize::from(area.width).saturating_sub(label_cols + 1);
    let mut lines: Vec<Line> = vec![Line::from(vec![
        Span::raw(" ".repeat(label_cols + 1)),
        Span::styled(
            trace_view::axis(&window, track_cols),
            Style::default().fg(Color::DarkGray),
        ),
    ])];
    let height = usize::from(area.height).saturating_sub(1);
    let start = sidebar_window(
        browser.selected_observation,
        browser.observations.len(),
        height,
    );
    let end = (start + height.max(1)).min(browser.observations.len());
    for (i, o) in browser.observations[start..end].iter().enumerate() {
        let i = start + i;
        let is_sel = i == browser.selected_observation;
        let Bar {
            offset,
            width,
            running,
            instant,
        } = trace_view::bar(o.start_ns, o.end_ns, &window, track_cols);
        let indent = "  ".repeat(o.depth.min(3));
        let label = format!(
            "{}{}{}",
            if is_sel { ">" } else { " " },
            indent,
            truncate_chars(
                &obs_display_name(o),
                label_cols.saturating_sub(indent.len() + 1),
            )
        );
        let bar_body = if instant {
            "▏".to_string()
        } else if running {
            format!("{}▶", "█".repeat(width.saturating_sub(1)))
        } else {
            "█".repeat(width)
        };
        let mut spans = vec![
            Span::styled(format!("{label:<label_cols$} "), obs_style(o)),
            Span::styled("░".repeat(offset), Style::default().fg(Color::DarkGray)),
            Span::styled(bar_body, obs_style(o)),
        ];
        if is_sel && browser.focused == BrowserPane::Detail {
            spans = spans
                .into_iter()
                .map(|s| {
                    let style = s.style.add_modifier(Modifier::REVERSED);
                    s.style(style)
                })
                .collect();
        }
        lines.push(Line::from(spans));
    }
    f.render_widget(Paragraph::new(lines), area);
}

/// The Skills view: skills grouped by harness on the left, the selected
/// row's details, executions or briefing on the right.
fn draw_skills_view(f: &mut Frame, view: &SkillsViewState, app: &App) {
    let width = (f.area().width * 96 / 100).clamp(60, 200);
    let height = (f.area().height * 92 / 100).clamp(18, 60);
    let area = centered(f.area(), width, height);
    f.render_widget(Clear, area);
    let [body, footer] = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(area);
    let [left, right] =
        Layout::horizontal([Constraint::Percentage(34), Constraint::Min(0)]).areas(body);

    // Left: skills grouped by harness
    let selectable = view
        .rows
        .iter()
        .filter(|r| !matches!(r, SkillRow::Header(_)))
        .count();
    let filter = view.harness_filter.map(|h| h.as_str()).unwrap_or("all");
    let left_block = Block::default()
        .borders(Borders::ALL)
        .border_style(pane_border(view.focus == SkillsPane::Skills))
        .title(format!(" Skills ({selectable}) [harness: {filter}] "));
    if selectable == 0 {
        let p = Paragraph::new("\n  no skills found\n\n  ~/.agent-mux/skills/<id>/SKILL.md")
            .style(Style::default().fg(Color::DarkGray))
            .block(left_block);
        f.render_widget(p, left);
    } else {
        let visible = usize::from(left.height.saturating_sub(2));
        let start = sidebar_window(view.selected, view.rows.len(), visible);
        let end = (start + visible.max(1)).min(view.rows.len());
        let name_width = usize::from(left.width.saturating_sub(4))
            .saturating_sub(20)
            .max(8);
        let items: Vec<ListItem> = view.rows[start..end]
            .iter()
            .enumerate()
            .map(|(offset, row)| {
                let i = start + offset;
                let is_sel = i == view.selected;
                let marker = if is_sel { "> " } else { "  " };
                let line = match row {
                    SkillRow::Header(h) => Line::styled(
                        h.display_name().to_string(),
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    ),
                    SkillRow::Package { index, harness } => {
                        let Some(def) = view.packages.get(*index) else {
                            return ListItem::new(Line::raw(""));
                        };
                        let icon = def.icon.as_deref().unwrap_or("⚡");
                        let (state, style) = match app.running_skill_harness(&def.id) {
                            Some(h) if h == *harness => (
                                format!("running [{}]", h.as_str()),
                                Style::default().fg(Color::Green),
                            ),
                            _ => match view.install.get(&(def.id.clone(), *harness)) {
                                Some(st) => {
                                    let label = InstallLabel::of(st);
                                    let color = match label {
                                        InstallLabel::Current => Color::Green,
                                        InstallLabel::Stale | InstallLabel::NotManaged => {
                                            Color::Yellow
                                        }
                                        InstallLabel::NotInstalled => Color::DarkGray,
                                    };
                                    (label.text().to_string(), Style::default().fg(color))
                                }
                                None => ("—".to_string(), Style::default().fg(Color::DarkGray)),
                            },
                        };
                        Line::from(vec![
                            Span::raw(marker),
                            Span::styled(format!("{icon} "), Style::default().fg(Color::Yellow)),
                            Span::raw(format!(
                                "{:<w$} ",
                                truncate_chars(&def.name, name_width),
                                w = name_width
                            )),
                            Span::styled(state, style),
                        ])
                    }
                    SkillRow::Native { index } => {
                        let Some(def) = view.native.get(*index) else {
                            return ListItem::new(Line::raw(""));
                        };
                        Line::from(vec![
                            Span::raw(marker),
                            Span::styled("⚙ ", Style::default().fg(Color::DarkGray)),
                            Span::raw(format!(
                                "{:<w$} ",
                                truncate_chars(&def.name, name_width),
                                w = name_width
                            )),
                            Span::styled(
                                format!("native · {}", def.scope.label()),
                                Style::default().fg(Color::DarkGray),
                            ),
                        ])
                    }
                };
                let item = ListItem::new(line);
                if is_sel && view.focus == SkillsPane::Skills {
                    item.style(Style::default().add_modifier(Modifier::REVERSED))
                } else if is_sel {
                    item.style(Style::default().fg(Color::Cyan))
                } else {
                    item
                }
            })
            .collect();
        f.render_widget(List::new(items).block(left_block), left);
    }

    // Right: tabs
    let selected_name = match view.selected_row() {
        Some(SkillRow::Package { index, harness }) => view
            .packages
            .get(*index)
            .map(|p| format!("{} · {}", p.id, harness.display_name()))
            .unwrap_or_default(),
        Some(SkillRow::Native { index }) => view
            .native
            .get(*index)
            .map(|d| format!("{} · {}", d.name, d.harness.display_name()))
            .unwrap_or_default(),
        _ => String::new(),
    };
    let mut title_spans = vec![Span::raw(" ")];
    for (i, tab) in view.tabs().into_iter().enumerate() {
        if i > 0 {
            title_spans.push(Span::raw(" | "));
        }
        title_spans.push(if tab == view.tab {
            Span::styled(
                tab.label(),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            Span::styled(tab.label(), Style::default().fg(Color::DarkGray))
        });
    }
    if !selected_name.is_empty() {
        title_spans.push(Span::raw(format!(" · {selected_name} ")));
    } else {
        title_spans.push(Span::raw(" "));
    }
    let right_block = Block::default()
        .borders(Borders::ALL)
        .border_style(pane_border(view.focus == SkillsPane::Detail))
        .title(Line::from(title_spans));
    let inner = right_block.inner(right);
    view.viewport_rows.set(usize::from(inner.height));

    match view.tab {
        SkillsTab::Details => {
            f.render_widget(right_block, right);
            let lines: Vec<Line> = view
                .detail_lines
                .iter()
                .skip(view.scroll_offset)
                .cloned()
                .collect();
            f.render_widget(Paragraph::new(lines), inner);
        }
        SkillsTab::Executions => {
            f.render_widget(right_block, right);
            draw_skill_executions(f, inner, view);
        }
    }

    let footer_text = Line::styled(
        " [e] edit  [v] check  [l] run  [Tab] details/runs  [↑/↓] select  [1-3] harness  [T] trace  [Esc] close",
        Style::default().fg(Color::Black).bg(Color::Cyan),
    );
    f.render_widget(Paragraph::new(footer_text), footer);
}

/// The Configuration view: items grouped by kind on the left, the selected
/// item's source, status, uses and effective text on the right.
fn draw_config_view(f: &mut Frame, view: &ConfigViewState) {
    let width = (f.area().width * 96 / 100).clamp(60, 200);
    let height = (f.area().height * 92 / 100).clamp(18, 60);
    let area = centered(f.area(), width, height);
    f.render_widget(Clear, area);
    let [body, footer] = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(area);
    let [left, right] =
        Layout::horizontal([Constraint::Percentage(38), Constraint::Min(0)]).areas(body);

    let left_block = Block::default()
        .borders(Borders::ALL)
        .border_style(pane_border(view.focus == ConfigPane::List))
        .title(format!(
            " Configuration ({})  {} ",
            view.item_count(),
            view.root.display()
        ));
    let visible = usize::from(left.height.saturating_sub(2));
    let start = sidebar_window(view.selected, view.rows.len(), visible);
    let end = (start + visible.max(1)).min(view.rows.len());
    let name_width = usize::from(left.width.saturating_sub(4))
        .saturating_sub(10)
        .max(8);
    let items: Vec<ListItem> = view.rows[start..end]
        .iter()
        .enumerate()
        .map(|(offset, row)| {
            let i = start + offset;
            let is_sel = i == view.selected;
            let line = match row {
                ConfigRow::Header(kind) => Line::styled(
                    kind.title().to_string(),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
                ConfigRow::Item(idx) => {
                    let Some(asset) = view.catalog.assets.get(*idx) else {
                        return ListItem::new(Line::raw(""));
                    };
                    let marker = if is_sel { "> " } else { "  " };
                    // the id without its group prefix keeps rows short
                    let short = asset
                        .id
                        .strip_prefix("loops/skills/")
                        .or_else(|| asset.id.strip_prefix("loops/agents/"))
                        .or_else(|| asset.id.strip_prefix("loops/templates/"))
                        .or_else(|| asset.id.strip_prefix("loops/"))
                        .or_else(|| asset.id.strip_prefix("skills/"))
                        .unwrap_or(&asset.id);
                    let mut name: String = short.chars().take(name_width).collect();
                    if short.chars().count() > name_width {
                        name.pop();
                        name.push('…');
                    }
                    let (label, style) = ConfigViewState::source_label(asset);
                    Line::from(vec![
                        Span::raw(format!("{marker}{name:<name_width$} ")),
                        Span::styled(label, style),
                    ])
                }
            };
            let item = ListItem::new(line);
            if is_sel && view.focus == ConfigPane::List {
                item.style(Style::default().add_modifier(Modifier::REVERSED))
            } else if is_sel {
                item.style(Style::default().fg(Color::Cyan))
            } else {
                item
            }
        })
        .collect();
    f.render_widget(List::new(items).block(left_block), left);

    let title = view
        .selected_asset()
        .map(|a| format!(" {} ", a.id))
        .unwrap_or_else(|| " Configuration ".into());
    let right_block = Block::default()
        .borders(Borders::ALL)
        .border_style(pane_border(view.focus == ConfigPane::Detail))
        .title(title);
    let inner = right_block.inner(right);
    view.viewport_rows.set(usize::from(inner.height));
    f.render_widget(right_block, right);
    let lines: Vec<Line> = view
        .detail_lines
        .iter()
        .skip(view.scroll_offset)
        .take(usize::from(inner.height))
        .cloned()
        .collect();
    f.render_widget(Paragraph::new(lines), inner);

    let footer_style = if matches!(view.pending, Pending::None) {
        Style::default().fg(Color::Black).bg(Color::Cyan)
    } else {
        Style::default().fg(Color::Black).bg(Color::Yellow)
    };
    f.render_widget(
        Paragraph::new(Line::styled(
            fit_hints(&view.footer(), usize::from(footer.width)),
            footer_style,
        )),
        footer,
    );
}

/// The Executions tab: launches of the skill on this harness, then the
/// turns (on any harness) that loaded it.
fn draw_skill_executions(f: &mut Frame, area: Rect, view: &SkillsViewState) {
    let dim = Style::default().fg(Color::DarkGray);
    let head = Style::default()
        .fg(Color::Yellow)
        .add_modifier(Modifier::BOLD);
    // (line, execution index it stands for)
    let mut lines: Vec<(Line<'static>, Option<usize>)> = Vec::new();
    if view.launches.is_empty() && view.turns.is_empty() {
        match &view.error {
            Some(e) => {
                lines.push((
                    Line::styled(format!("  {e}"), Style::default().fg(Color::Red)),
                    None,
                ));
            }
            None => {
                let harness = view
                    .selected_row()
                    .and_then(|r| r.harness(&view.native))
                    .map(|h| h.display_name())
                    .unwrap_or("this harness");
                lines.push((
                    Line::styled(format!("  no recorded executions on {harness}"), dim),
                    None,
                ));
                lines.push((Line::raw(""), None));
                lines.push((
                    Line::styled(
                        "  run it with l, or `agent-mux trace import --discover` for past sessions",
                        dim,
                    ),
                    None,
                ));
            }
        }
    }
    let harness_name = view
        .selected_row()
        .and_then(|r| r.harness(&view.native))
        .map(|h| h.display_name())
        .unwrap_or("harness");
    if !view.launches.is_empty() {
        lines.push((
            Line::styled(
                format!(
                    "Sessions launched on {harness_name} ({})",
                    view.launches.len()
                ),
                head,
            ),
            None,
        ));
        for (i, l) in view.launches.iter().enumerate() {
            let status = if l.live {
                Span::styled("● live   ", Style::default().fg(Color::Green))
            } else {
                let text = match (l.termination.as_deref(), l.exit_code) {
                    (_, Some(code)) => format!("exit {code}"),
                    (Some(t), None) => t.to_string(),
                    (None, None) => "ended".to_string(),
                };
                Span::styled(format!("{text:<9}"), dim)
            };
            let matched = if l.by_id { "" } else { " (matched by name)" };
            lines.push((
                Line::from(vec![
                    Span::raw("  "),
                    Span::raw(format!("{} ", &fmt_time(l.started_ns)[..16])),
                    provider_badge(&l.provider),
                    status,
                    Span::styled(
                        format!(" {}t {:>7} ", l.turns, fmt_cost(l.total_cost_usd)),
                        Style::default().fg(Color::Yellow),
                    ),
                    Span::raw(truncate_path_chars(&l.cwd, 36)),
                    Span::styled(matched.to_string(), dim),
                ]),
                Some(i),
            ));
        }
        lines.push((Line::raw(""), None));
    }
    if !view.turns.is_empty() {
        lines.push((
            Line::styled(
                format!("Turns that loaded it, any harness ({})", view.turns.len()),
                head,
            ),
            None,
        ));
        for (i, t) in view.turns.iter().enumerate() {
            let name = t
                .stat
                .name
                .split_once(": ")
                .map(|(_, rest)| rest)
                .unwrap_or(&t.stat.name);
            let attributed = if t.attributed {
                Span::styled("attributed ", Style::default().fg(Color::Green))
            } else {
                Span::styled("loaded     ", dim)
            };
            lines.push((
                Line::from(vec![
                    Span::raw("  "),
                    Span::raw(format!("{} ", &fmt_time(t.stat.start_ns)[..16])),
                    Span::styled(
                        format!("#{:<3} ", t.stat.ordinal),
                        Style::default().fg(Color::Cyan),
                    ),
                    Span::styled(
                        format!(
                            "{:>6} {:>7} ",
                            fmt_ms(t.stat.latency_ms),
                            fmt_cost(t.stat.total_cost_usd)
                        ),
                        Style::default().fg(Color::Yellow),
                    ),
                    attributed,
                    Span::raw(truncate_chars(name, 40)),
                ]),
                Some(view.launches.len() + i),
            ));
        }
    }
    let selected_line = lines
        .iter()
        .position(|(_, idx)| *idx == Some(view.selected_execution))
        .unwrap_or(0);
    let visible = usize::from(area.height).max(1);
    let start = sidebar_window(selected_line, lines.len(), visible);
    let end = (start + visible).min(lines.len());
    let items: Vec<ListItem> = lines[start..end]
        .iter()
        .map(|(line, idx)| {
            let item = ListItem::new(line.clone());
            if *idx == Some(view.selected_execution) && view.focus == SkillsPane::Detail {
                item.style(Style::default().add_modifier(Modifier::REVERSED))
            } else if *idx == Some(view.selected_execution) && idx.is_some() {
                item.style(Style::default().fg(Color::Cyan))
            } else {
                item
            }
        })
        .collect();
    f.render_widget(List::new(items), area);
}

/// The Loops section preview: one card for the selected loop.
/// Wraps a preview's rows at `width`: a `key  value` row (a first span
/// that is the indented, padded key, then the value) hangs under the
/// value, any other line under its own leading spaces.
fn hang(lines: Vec<Line<'static>>, width: usize) -> Vec<Line<'static>> {
    lines
        .iter()
        .flat_map(|l| {
            let first = l.spans.first().map(|s| s.content.as_ref()).unwrap_or("");
            let indent = if l.spans.len() > 1 && first.starts_with(' ') && first.ends_with(' ') {
                first.chars().count()
            } else {
                l.spans
                    .iter()
                    .flat_map(|s| s.content.chars())
                    .take_while(|c| *c == ' ')
                    .count()
            };
            crate::app::workflows_view::wrap_line_hanging(l, width, indent)
        })
        .collect()
}

fn draw_loop_preview(f: &mut Frame, area: Rect, app: &App) {
    let dim = Style::default().fg(Color::DarkGray);
    let key = Style::default().fg(Color::DarkGray);
    let Some(entry) = app.selected_loop() else {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan))
            .title(" Loops ");
        let text = "\n  No loops yet.\n\n  A loop is a scheduled, bounded agent run against one workspace: it reads a state\n  file, triages, at most proposes one fix in a worktree, updates the state file and stops.\n\n  [a] add a loop (pattern, harness, cadence, what it may do)   [E] Loops view   [?] help\n\n  Start with report only for a week. Let it propose fixes when the Setup tab says it is ready.";
        f.render_widget(Paragraph::new(text).block(block), area);
        return;
    };
    let profile = if entry.profile.is_empty() {
        entry.harness.clone()
    } else {
        format!("{} ({})", entry.profile, entry.harness)
    };
    let title = format!(
        " {} · {} · {} ",
        entry.pattern,
        entry.workspace_name(),
        profile
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .title(Span::styled(
            title,
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(area);
    f.render_widget(block, area);
    // ` key         value`: the value starts at column KEY, and a row that
    // wraps hangs under it.
    const KEY: usize = 13;
    let row = |k: &str, v: String| {
        Line::from(vec![
            Span::styled(format!(" {k:<w$}", w = KEY - 1), key),
            Span::raw(v),
        ])
    };
    let width = usize::from(inner.width);
    let mut lines: Vec<Line> = Vec::new();
    let card = app.loop_cards.get(&entry.id);
    let (status, _) = app.loop_row(entry);
    let wall = crate::loops::now();

    // 1. The answer: what the loop is doing or found, and when.
    let last = card.and_then(|c| c.last_run.as_ref());
    let (word, why) = match status {
        LoopStatus::Running => ("RUNNING".to_string(), String::new()),
        LoopStatus::Paused => {
            let why = if app.loop_registry.pause_all {
                "every loop is paused ([K] resumes)".to_string()
            } else if card.is_some_and(|c| c.kill_switch_in_files) {
                "loop-pause-all is in the workspace files".to_string()
            } else {
                entry
                    .paused_reason
                    .clone()
                    .unwrap_or_else(|| "[p] resumes".into())
            };
            ("PAUSED".to_string(), why)
        }
        LoopStatus::NeedsHuman => (
            "NEEDS YOU".to_string(),
            format!("{} waiting · [I] inbox", card.map(|c| c.inbox).unwrap_or(0)),
        ),
        LoopStatus::Failed => (
            last.map(|r| r.outcome.word())
                .unwrap_or("failed")
                .to_uppercase(),
            last.and_then(|r| r.detail_str("reason"))
                .unwrap_or_default()
                .to_string(),
        ),
        LoopStatus::Scheduled => match last {
            Some(r) => (r.outcome.word().to_uppercase(), String::new()),
            None => ("NO RUNS YET".to_string(), "[r] runs it now".to_string()),
        },
    };
    let mut when: Vec<String> = Vec::new();
    if let Some(r) = last {
        let ago = (wall - crate::loops::from_ns(r.started_ns.unwrap_or(r.scheduled_ns)))
            .whole_seconds()
            .max(0) as u64;
        when.push(format!(
            "ran {} ago",
            crate::app::loops::short_duration(ago)
        ));
    }
    if status != LoopStatus::Paused {
        match card.and_then(|c| c.next_in_s) {
            Some(s) if s <= 0 => when.push("due now".into()),
            Some(s) => when.push(format!(
                "next in {}",
                crate::app::loops::short_duration(s as u64)
            )),
            None => {}
        }
    }
    let head = format!(" {} {word}", status.glyph());
    let tail = when.join(" · ");
    let mut spans = vec![Span::styled(
        head.clone(),
        Style::default()
            .fg(status.color())
            .add_modifier(Modifier::BOLD),
    )];
    let mut used = head.chars().count();
    if !why.is_empty() {
        let text = format!("  {why}");
        used += text.chars().count();
        spans.push(Span::raw(text));
    }
    if !tail.is_empty() {
        let pad = width.saturating_sub(used + tail.chars().count() + 1).max(2);
        spans.push(Span::styled(format!("{}{tail}", " ".repeat(pad)), dim));
    }
    lines.push(Line::from(spans));

    let Some(c) = card else {
        lines.push(Line::styled(" loading…", dim));
        f.render_widget(Paragraph::new(hang(lines, width)), inner);
        return;
    };
    // what the last run found, in its own words, and what moved
    if let Some(r) = last {
        let mut facts: Vec<String> = Vec::new();
        if let Some(n) = r.items_found {
            facts.push(format!("{n} found"));
        }
        if let Some(n) = r.escalations.filter(|n| *n > 0) {
            facts.push(format!("{n} for you"));
        }
        if let Some(t) = r.tokens.filter(|t| *t > 0) {
            facts.push(format!(
                "{} tokens",
                crate::loops::format_tokens(t.max(0) as u64)
            ));
            facts.push(crate::workflows::report::Cost::of(r.cost_usd, &r.harness).text());
        }
        if let Some(d) = r.duration_s() {
            facts.push(crate::workflows::report::format_duration(d));
        }
        if let Some(s) = r.detail_str("summary") {
            lines.push(Line::raw(format!("   {s}")));
        }
        if let Some(d) = crate::app::loops::run_delta_summary(r) {
            lines.push(Line::from(vec![
                Span::styled("   Since last run  ", dim),
                Span::raw(d),
            ]));
        }
        if !facts.is_empty() {
            lines.push(Line::styled(format!("   {}", facts.join(" · ")), dim));
        }
    }
    lines.push(Line::raw(""));

    // 2. What the loop may do, and what stands between it and more.
    let (allowed, capped) = c.allowed(entry.level);
    let mut allowed_line = allowed.can().to_string();
    if entry.bypass_score && allowed < crate::loops::Level::L3 {
        allowed_line.push_str(" · score bypassed");
    }
    if let Some(why) = &capped {
        allowed_line.push_str(&format!(
            " — set to {}; held back: {why}",
            entry.level.can()
        ));
    }
    lines.push(row("Allowed to", allowed_line));
    if capped.is_none()
        && let Some((up, missing)) = &c.step_up
    {
        if missing.is_empty() {
            lines.push(Line::styled(
                format!("{}ready to {} · [e] edit", " ".repeat(KEY), up.can()),
                dim,
            ));
        } else {
            lines.push(Line::styled(
                format!("{}to {}: {}", " ".repeat(KEY), up.can(), missing.join("; ")),
                dim,
            ));
        }
    }
    if status != LoopStatus::Paused
        && let Some(why) = c.blocked()
    {
        lines.push(Line::from(vec![
            Span::styled(format!(" {:<w$}", "Next run", w = KEY - 1), key),
            Span::styled(
                format!("will not start: {why}"),
                Style::default().fg(Color::Yellow),
            ),
        ]));
    }

    // 3. Today's spend against the caps, and the last few runs.
    lines.push(row(
        "Today",
        format!(
            "{} of {} runs · {} of {} tokens{} · {}",
            c.runs_today,
            entry.max_runs_per_day,
            crate::loops::format_tokens(c.tokens_today.max(0) as u64),
            crate::loops::format_tokens(entry.max_tokens_per_day),
            if c.percent >= 80 {
                format!(" ({} %)", c.percent)
            } else {
                String::new()
            },
            crate::workflows::report::Cost::of(
                (c.cost_today > 0.0).then_some(c.cost_today),
                &entry.harness
            )
            .text()
        ),
    ));
    // One glyph per run, newest on the right, with the tally beside it.
    let glyphs: String = c
        .recent
        .iter()
        .rev()
        .map(|r| match r.outcome {
            crate::loops::Outcome::Escalated => '▮',
            crate::loops::Outcome::FixProposed => '▰',
            crate::loops::Outcome::ReportOnly => '▪',
            crate::loops::Outcome::Failed => '✗',
            _ => '▯',
        })
        .collect();
    let mut tally: std::collections::BTreeMap<&str, usize> = Default::default();
    for r in &c.recent {
        *tally.entry(r.outcome.word()).or_default() += 1;
    }
    if !tally.is_empty() {
        let recent: Vec<String> = tally
            .iter()
            .map(|(word, n)| format!("{n} {word}"))
            .collect();
        lines.push(row("Recent", format!("{glyphs}  {}", recent.join(" · "))));
    }
    lines.push(row(
        "Every",
        format!(
            "{} · {}",
            crate::loops::format_interval(entry.interval_s),
            truncate_path_chars(
                &entry.workspace.to_string_lossy(),
                width.saturating_sub(24).max(12)
            )
        ),
    ));

    // 4. Setup, folded to one line: the Setup tab has the detail.
    let mut setup: Vec<String> = Vec::new();
    let missing = c.files.iter().filter(|f| !f.present).count();
    let stale = c.files.iter().filter(|f| f.present && f.stale).count();
    if missing > 0 {
        setup.push(format!("{missing} of {} files missing", c.files.len()));
    }
    if stale > 0 {
        setup.push(format!("{stale} stale"));
    }
    if let Some(b) = c
        .breaker
        .as_deref()
        .filter(|b| !b.starts_with("ok") && *b != "no ledger yet")
    {
        setup.push(format!("breaker {b}"));
    }
    if let Some(e) = &c.store_error {
        setup.push(e.clone());
    }
    if !setup.is_empty() {
        lines.push(Line::from(vec![
            Span::styled(format!(" {:<w$}", "Setup", w = KEY - 1), key),
            Span::styled(
                format!("{} · [E] Setup tab", setup.join(" · ")),
                Style::default().fg(Color::Yellow),
            ),
        ]));
    }
    lines.push(Line::raw(""));
    lines.push(Line::styled(
        fit_hints(
            " [Enter] details  [r] run now  [p] pause  [e] edit  [x] remove  [K] kill switch  [E] loops view",
            width,
        ),
        dim,
    ));
    f.render_widget(Paragraph::new(hang(lines, width)), inner);
}

/// The directory picker shared by the New session dialog (Directory) and
/// the loop dialog (Workspace): the path line, a hint or the search line,
/// up to `max_visible` subfolder rows and an overflow count.
fn dir_picker_lines(
    label: &str,
    path: &str,
    picker: &crate::app::dir_picker::DirPicker,
    active: bool,
    max_visible: usize,
) -> Vec<Line<'static>> {
    let dim = Style::default().fg(Color::DarkGray);
    let mut lines: Vec<Line> = Vec::new();
    let path_style = if active && !picker.in_list() {
        Style::default().add_modifier(Modifier::REVERSED)
    } else {
        Style::default()
    };
    lines.push(Line::from(vec![
        Span::raw(label.to_string()),
        Span::styled(path.to_string(), path_style),
    ]));
    let rows = picker.rows();
    if picker.query.is_empty() {
        lines.push(Line::styled(
            "  Select subfolder: ↑/↓ · → open · ← up · Enter pick · type to search",
            dim,
        ));
    } else {
        lines.push(Line::from(vec![
            Span::styled("  Search: ", dim),
            Span::styled(
                picker.query.clone(),
                if active {
                    Style::default().fg(Color::Yellow)
                } else {
                    dim
                },
            ),
            Span::styled(
                format!(
                    "  ({} match{}, {} levels deep, Esc clears)",
                    rows.len(),
                    if rows.len() == 1 { "" } else { "es" },
                    crate::app::dir_picker::SEARCH_DEPTH
                ),
                dim,
            ),
        ]));
    }
    if rows.is_empty() {
        lines.push(Line::styled(
            if picker.query.is_empty() {
                "    (no subdirectories)"
            } else {
                "    (no matches)"
            },
            dim,
        ));
        return lines;
    }
    let max_visible = max_visible.max(1);
    let selected = picker.selected.unwrap_or(0).min(rows.len() - 1);
    let start = if selected >= max_visible {
        selected + 1 - max_visible
    } else {
        0
    };
    let end = (start + max_visible).min(rows.len());
    for (idx, entry) in rows[start..end].iter().enumerate() {
        let actual_idx = start + idx;
        let is_sel = picker.selected == Some(actual_idx);
        let marker = if is_sel { "> " } else { "  " };
        let style = if is_sel && active {
            Style::default().add_modifier(Modifier::REVERSED)
        } else if entry == ".." {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default().fg(Color::Blue)
        };
        let display_name = if entry == ".." {
            ".. (parent directory)".to_string()
        } else {
            format!("{entry}/")
        };
        lines.push(Line::styled(format!("    {marker}{display_name}"), style));
    }
    if rows.len() > max_visible {
        lines.push(Line::styled(
            format!("      ... ({} total directories)", rows.len()),
            dim,
        ));
    }
    lines
}

fn draw_loop_dialog(f: &mut Frame, dialog: &LoopDialogState, _app: &App) {
    let width = 84.min(f.area().width.saturating_sub(4)).max(40);
    let height = 30.min(f.area().height.saturating_sub(2)).max(16);
    let area = centered(f.area(), width, height);
    f.render_widget(Clear, area);
    let title = if dialog.editing.is_some() {
        " Edit loop "
    } else {
        " Add loop "
    };
    let block = Block::default().borders(Borders::ALL).title(title);
    let dim = Style::default().fg(Color::DarkGray);
    let sel = |on: bool| {
        if on {
            Style::default().add_modifier(Modifier::REVERSED)
        } else {
            Style::default()
        }
    };
    let pattern = dialog.pattern();
    let mut lines: Vec<Line> = Vec::new();
    let field = |label: &str, value: String, on: bool| {
        Line::from(vec![
            Span::styled(format!("{label:<12}"), dim),
            Span::styled(value, sel(on)),
        ])
    };
    // the subfolder list shrinks on a short terminal so the fields below
    // it stay on screen: 22 rows are fixed (border, fields, notes, footer)
    let max_visible = usize::from(height).saturating_sub(22).clamp(1, 4);
    lines.extend(dir_picker_lines(
        &format!("{:<12}", "Workspace"),
        &dialog.workspace,
        &dialog.dir_picker,
        dialog.field == LoopField::Workspace,
        max_visible,
    ));
    lines.push(field(
        "Pattern",
        pattern
            .map(|p| format!("{} — {}", p.id, p.name))
            .unwrap_or_default(),
        dialog.field == LoopField::Pattern,
    ));
    if let Some(p) = pattern {
        lines.push(Line::styled(
            format!(
                "            {} · starts: {} · risk {} · cost {}",
                truncate_chars(&p.goal, 48),
                p.week_one_level.short(),
                p.risk,
                p.token_cost
            ),
            dim,
        ));
    }
    let profile_text = if dialog.no_profiles {
        "no Claude Code or Codex profile in profiles.toml".to_string()
    } else {
        dialog
            .profiles
            .get(dialog.profile_idx)
            .map(|(n, h)| format!("{n} ({})", h.as_str()))
            .unwrap_or_default()
    };
    lines.push(field(
        "Profile",
        profile_text,
        dialog.field == LoopField::Profile,
    ));
    lines.push(Line::styled(
        "            Antigravity: not supported for loops yet (see docs/loops.md)",
        dim,
    ));
    lines.push(field(
        "Model",
        format!("{} ", dialog.model),
        dialog.field == LoopField::Model,
    ));
    lines.push(Line::styled(
        "            the run's model; blank keeps the profile's",
        dim,
    ));
    lines.push(field(
        "Verifier",
        format!("{} ", dialog.verifier_model),
        dialog.field == LoopField::VerifierModel,
    ));
    lines.push(Line::styled(
        if dialog.pattern().is_some_and(|p| p.verifier) {
            "            the loop-verifier sub-agent's model; blank inherits the run's"
        } else {
            "            this pattern runs no verifier"
        },
        dim,
    ));
    lines.push(field(
        "Every",
        format!("{} ", dialog.every),
        dialog.field == LoopField::Every,
    ));
    let level_line: Vec<Span> = {
        let mut spans = vec![Span::styled(format!("{:<12}", "Allowed to"), dim)];
        for (i, l) in [
            crate::loops::Level::L1,
            crate::loops::Level::L2,
            crate::loops::Level::L3,
        ]
        .into_iter()
        .enumerate()
        {
            let blocked = dialog.level_notes[i].is_some();
            let on = dialog.level == l;
            let style = if on && dialog.field == LoopField::Level {
                Style::default().add_modifier(Modifier::REVERSED)
            } else if on {
                Style::default().fg(Color::Cyan)
            } else if blocked {
                dim
            } else {
                Style::default()
            };
            spans.push(Span::styled(
                format!("[{}{}] ", l.short(), if blocked { " ✗" } else { "" }),
                style,
            ));
        }
        spans
    };
    lines.push(Line::from(level_line));
    if let Some(note) = &dialog.level_notes[match dialog.level {
        crate::loops::Level::L1 => 0,
        crate::loops::Level::L2 => 1,
        crate::loops::Level::L3 => 2,
    }] {
        lines.push(Line::styled(
            format!("            ✗ {note}"),
            Style::default().fg(Color::Yellow),
        ));
    } else {
        lines.push(Line::styled(
            format!("            a run may {}", dialog.level.can()),
            dim,
        ));
    }
    lines.push(field(
        "Runs/day",
        format!("{} ", dialog.max_runs),
        dialog.field == LoopField::MaxRuns,
    ));
    lines.push(field(
        "Tokens/day",
        format!("{} ", dialog.max_tokens),
        dialog.field == LoopField::MaxTokens,
    ));
    lines.push(field(
        "USD/run",
        format!(
            "{} ",
            if dialog.max_cost.is_empty() {
                "(no cap)".to_string()
            } else {
                dialog.max_cost.clone()
            }
        ),
        dialog.field == LoopField::MaxCost,
    ));
    lines.push(field(
        "Score",
        if dialog.bypass_score {
            "[x] may run below the readiness score (report only, propose fixes)".into()
        } else {
            "[ ] needs the readiness score".into()
        },
        dialog.field == LoopField::BypassScore,
    ));
    lines.push(field(
        "Scaffold",
        if dialog.scaffold {
            "[x] write missing skills and contract files (never overwrites)".into()
        } else {
            "[ ] register only".into()
        },
        dialog.field == LoopField::Scaffold,
    ));
    lines.push(Line::raw(""));
    lines.push(Line::styled(format!("  {}", dialog.audit_note), dim));
    if let Some(e) = &dialog.error {
        lines.push(Line::styled(
            format!("  {e}"),
            Style::default().fg(Color::Red),
        ));
    }
    lines.push(Line::raw(""));
    lines.push(Line::styled(
        "  [Tab/↑/↓] field  [←/→ / Space] choose  [↓ type] search folders  [Enter] save  [Esc] cancel",
        dim,
    ));
    f.render_widget(Paragraph::new(lines).block(block), area);
}

fn draw_loops_view(f: &mut Frame, view: &LoopsViewState, app: &App) {
    // The whole screen, footer over the status bar: a box inset over the
    // sidebar left fragments of it showing down both edges.
    let area = f.area();
    f.render_widget(Clear, area);
    let [body, footer] = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(area);
    let [left, right] =
        Layout::horizontal([Constraint::Percentage(30), Constraint::Min(0)]).areas(body);

    let title = if view.pause_all {
        format!(" Loops ({}) PAUSED ", view.loops.len())
    } else {
        format!(" Loops ({}) ", view.loops.len())
    };
    let left_block = Block::default()
        .borders(Borders::ALL)
        .border_style(pane_border(view.focus == LoopsPane::Loops))
        .title(title);
    if view.loops.is_empty() {
        let p = Paragraph::new("\n  no loops yet\n\n  [a] in the Loops section")
            .style(Style::default().fg(Color::DarkGray))
            .block(left_block);
        f.render_widget(p, left);
    } else {
        let visible = usize::from(left.height.saturating_sub(2));
        let start = sidebar_window(view.selected, view.rows.len(), visible);
        let end = (start + visible.max(1)).min(view.rows.len());
        let items: Vec<ListItem> = view.rows[start..end]
            .iter()
            .enumerate()
            .map(|(offset, row)| {
                let i = start + offset;
                let is_sel = i == view.selected;
                let marker = if is_sel { "> " } else { "  " };
                let line = match row {
                    LoopRow::Header(ws) => Line::styled(
                        truncate_path_chars(ws, usize::from(left.width.saturating_sub(3))),
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    ),
                    LoopRow::Loop(idx) => {
                        let Some(entry) = view.loops.get(*idx) else {
                            return ListItem::new(Line::raw(""));
                        };
                        let (status, right) = app.loop_row(entry);
                        Line::from(vec![
                            Span::raw(marker),
                            Span::styled(
                                format!("{} ", status.glyph()),
                                Style::default().fg(status.color()),
                            ),
                            Span::raw(format!("{} ", entry.pattern)),
                            Span::styled(right.to_string(), Style::default().fg(Color::DarkGray)),
                        ])
                    }
                };
                let item = ListItem::new(line);
                if is_sel && view.focus == LoopsPane::Loops {
                    item.style(Style::default().add_modifier(Modifier::REVERSED))
                } else if is_sel {
                    item.style(Style::default().fg(Color::Cyan))
                } else {
                    item
                }
            })
            .collect();
        f.render_widget(List::new(items).block(left_block), left);
    }

    let mut title_spans = vec![Span::raw(" ")];
    for (i, tab) in LoopsTab::ALL.into_iter().enumerate() {
        if i > 0 {
            title_spans.push(Span::raw(" | "));
        }
        let label = tab.label().to_string();
        title_spans.push(if tab == view.tab {
            Span::styled(
                label,
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            Span::styled(label, Style::default().fg(Color::DarkGray))
        });
    }
    if let Some(l) = view.selected_loop() {
        title_spans.push(Span::raw(format!(
            " · {} @ {} ",
            l.pattern,
            l.workspace_name()
        )));
    } else {
        title_spans.push(Span::raw(" "));
    }
    let right_block = Block::default()
        .borders(Borders::ALL)
        .border_style(pane_border(view.focus == LoopsPane::Detail))
        .title(Line::from(title_spans));
    let inner = right_block.inner(right);
    view.viewport_rows.set(usize::from(inner.height));
    f.render_widget(right_block, right);
    let lines: Vec<Line> = view
        .detail_lines
        .iter()
        .skip(view.scroll_offset)
        .cloned()
        .collect();
    f.render_widget(Paragraph::new(lines), inner);

    let footer_hints = match view.tab {
        LoopsTab::Report => {
            " [Tab/1-3] tab  [↑/↓] earlier run  [2] history  [r] run now  [T] traces  [Esc] close"
        }
        LoopsTab::History => {
            " [Tab/1-3] tab  [←/→] pane  [↑/↓] select  [1] its report  [Enter] attach/traces  [r] run now  [Esc] close"
        }
        _ => {
            " [Tab/1-3] tab  [←/→] pane  [↑/↓] scroll  [r] run now  [p] pause  [R] reload  [I] inbox  [Esc] close"
        }
    };
    let footer_text = Line::styled(
        fit_hints(footer_hints, usize::from(footer.width)),
        Style::default().fg(Color::Black).bg(Color::Cyan),
    );
    f.render_widget(Paragraph::new(footer_text), footer);
}

/// The About overlay (`v`): the rows `app::about` gathered, one per line.
fn draw_about(f: &mut Frame, state: &AboutState) {
    let label = Style::default().fg(Color::DarkGray);
    let head = Style::default()
        .fg(Color::Yellow)
        .add_modifier(Modifier::BOLD);
    let lines: Vec<Line> = state
        .rows
        .iter()
        .map(|row| match row {
            AboutRow::Heading(text) => Line::styled(format!("  {text}"), head),
            AboutRow::Field(name, value) => Line::from(vec![
                Span::styled(format!("    {name:<9} "), label),
                Span::raw(value.clone()),
            ]),
            AboutRow::Continuation(text) => Line::styled(
                format!("              {text}"),
                Style::default().fg(Color::Gray),
            ),
            AboutRow::Note(text) => Line::styled(format!("  {text}"), Style::default()),
            AboutRow::Blank => Line::raw(""),
        })
        .collect();
    let widest = state
        .rows
        .iter()
        .map(|row| match row {
            AboutRow::Heading(t) | AboutRow::Note(t) => t.chars().count() + 2,
            AboutRow::Field(n, v) => n.chars().count() + v.chars().count() + 6,
            AboutRow::Continuation(t) => t.chars().count() + 14,
            AboutRow::Blank => 0,
        })
        .max()
        .unwrap_or(40);
    let width = (widest as u16 + 4)
        .min(f.area().width.saturating_sub(4))
        .max(40);
    let height = (lines.len() as u16 + 3).min(f.area().height.saturating_sub(2));
    let area = centered(f.area(), width, height);
    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .title(" About agent-mux ");
    let inner = block.inner(area);
    f.render_widget(block, area);
    let [body, footer] = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(inner);
    state.viewport_rows.set(usize::from(body.height));
    let shown: Vec<Line> = lines
        .into_iter()
        .skip(state.scroll_offset.min(state.max_scroll()))
        .collect();
    f.render_widget(Paragraph::new(shown), body);
    let more = if state.max_scroll() > 0 {
        "  [↑/↓] scroll"
    } else {
        ""
    };
    f.render_widget(
        Paragraph::new(Line::styled(
            format!("  [Esc] close  [?] keys{more}"),
            label,
        )),
        footer,
    );
}

fn draw_confirm(f: &mut Frame, message: &str) {
    let width = (message.chars().count() as u16 + 4).min(f.area().width);
    let area = centered(f.area(), width, 3);
    f.render_widget(Clear, area);
    let block = Block::default().borders(Borders::ALL);
    f.render_widget(Paragraph::new(message).block(block), area);
}

/// The Workflows section preview: one card for the selected document.
fn draw_workflow_preview(f: &mut Frame, area: Rect, app: &App) {
    use crate::app::workflows::WorkflowRow;
    let dim = Style::default().fg(Color::DarkGray);
    let key = Style::default().fg(Color::DarkGray);
    let Some(selected) = app.selected_workflow_row() else {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan))
            .title(" Workflows ");
        let text = "\n  No workflows.\n\n  A workflow runs several harness sessions with one focused goal each and\n  composes their answers: fan-out, verification votes, tournaments, loops until done.\n\n  [c] compose one for a task   [W] Workflows view   [?] help";
        f.render_widget(Paragraph::new(text).block(block), area);
        return;
    };
    let row = |k: &str, v: String| {
        Line::from(vec![Span::styled(format!("  {k:<11}"), key), Span::raw(v)])
    };
    let block_with = |title: String, color: Color| {
        Block::default()
            .borders(Borders::ALL)
            .border_style(
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )
            .title(Span::styled(title, Style::default().fg(color)))
    };
    let mut lines: Vec<Line> = Vec::new();
    let block = match &selected {
        WorkflowRow::Live(id) => {
            let Some(r) = app.live_workflow_runs.iter().find(|r| &r.run_id == id) else {
                return;
            };
            lines.push(Line::styled(
                format!("  ▶ RUNNING   {}", r.progress()),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ));
            lines.push(Line::styled(
                format!(
                    "  on {} · {} · started {}",
                    r.harness.as_str(),
                    r.workspace.display(),
                    crate::workflows::report::format_when(r.started_ns)
                ),
                dim,
            ));
            lines.push(Line::raw(""));
            for (step, state) in r.state.step_states() {
                lines.push(row(&step, state.to_string()));
            }
            lines.push(Line::raw(""));
            lines.push(Line::styled("  [Enter] open the run   [x] cancel", dim));
            block_with(format!(" {} · running ", r.name), Color::Cyan)
        }
        WorkflowRow::Recent(id) => {
            let Some(r) = app.recent_workflow_runs.iter().find(|r| &r.run_id == id) else {
                return;
            };
            let ok = r.status == "finished" && r.error.is_none();
            lines.push(Line::styled(
                format!(
                    "  {} {}",
                    if ok { "✓" } else { "!" },
                    r.status.to_uppercase()
                ),
                Style::default()
                    .fg(if ok { Color::Green } else { Color::Yellow })
                    .add_modifier(Modifier::BOLD),
            ));
            lines.push(Line::styled(
                format!(
                    "  {} sessions · {} tokens · ${:.2} · on {} · {}",
                    r.sessions,
                    crate::loops::format_tokens(r.tokens),
                    r.cost_usd,
                    r.harness,
                    crate::workflows::report::format_when(crate::loops::to_ns(r.ended_at))
                ),
                dim,
            ));
            if let Some(e) = &r.error {
                lines.push(Line::styled(
                    format!("  {e}"),
                    Style::default().fg(Color::Red),
                ));
            }
            for n in r.notes.iter().take(4) {
                lines.push(Line::styled(format!("  · {n}"), dim));
            }
            lines.push(Line::raw(""));
            lines.push(Line::styled(
                "  [Enter] open the report   [W] every run",
                dim,
            ));
            block_with(
                format!(" {} · {} ", r.name, r.status),
                if ok { Color::Green } else { Color::Yellow },
            )
        }
        WorkflowRow::Planned(id) => {
            let Some(p) = app.planned_workflows.iter().find(|p| &p.id == id) else {
                return;
            };
            lines.push(Line::styled(
                if p.valid() {
                    "  ⏸ PLAN READY   the planner wrote a workflow; nothing has run yet"
                } else {
                    "  ! PLAN HAS PROBLEMS   fix it with [e] in the view, or discard it"
                },
                Style::default()
                    .fg(if p.valid() {
                        Color::Cyan
                    } else {
                        Color::Yellow
                    })
                    .add_modifier(Modifier::BOLD),
            ));
            lines.push(Line::raw(""));
            lines.push(row("Task", p.task.clone()));
            lines.push(row("Workflow", p.name.clone()));
            lines.push(row("Workspace", p.workspace.to_string_lossy().into_owned()));
            for problem in p.problems.iter().take(4) {
                lines.push(Line::styled(
                    format!("  ! {problem}"),
                    Style::default().fg(Color::Red),
                ));
            }
            lines.push(Line::raw(""));
            lines.push(Line::styled(
                "  [Enter] review it: run, edit, save or discard",
                dim,
            ));
            block_with(format!(" {} · plan ", p.name), Color::Cyan)
        }
        WorkflowRow::Doc(_) => {
            let Some(entry) = app.selected_workflow_entry() else {
                return;
            };
            let (glyph, _) = app.workflow_row(entry);
            match &entry.doc {
                Some(doc) => {
                    lines.push(Line::raw(format!("  {}", doc.description)));
                    if let Some(w) = &doc.when_to_use {
                        lines.push(Line::styled(format!("  {w}"), dim));
                    }
                    lines.push(Line::raw(""));
                    for (n, s) in doc.steps.iter().enumerate() {
                        lines.push(Line::from(vec![
                            Span::styled(
                                format!("  {:>2} {:<14} ", n + 1, s.id),
                                Style::default().fg(Color::White),
                            ),
                            Span::raw(crate::app::workflows_view::describe_step(s)),
                        ]));
                        // which skill does the work and where it runs,
                        // when the document says more than "the run's"
                        let mut how: Vec<String> = Vec::new();
                        match &s.actor {
                            Some(crate::workflows::document::Actor::Skill(name)) => {
                                how.push(name.clone())
                            }
                            Some(crate::workflows::document::Actor::Prompt(_)) => {
                                how.push("inline prompt".into())
                            }
                            None => {}
                        }
                        let step_runner = crate::workflows::document::Workflow::runner_of(s);
                        if !step_runner.is_empty() {
                            how.push(format!("on {}", step_runner.label()));
                        }
                        if let Some(v) = &s.verify
                            && !v.runner.is_empty()
                        {
                            how.push(format!("checkers on {}", v.runner.label()));
                        }
                        if !s.judge_runner.is_empty() {
                            how.push(format!("judge on {}", s.judge_runner.label()));
                        }
                        if !how.is_empty() {
                            lines.push(Line::styled(
                                format!("{}{}", " ".repeat(20), how.join(" · ")),
                                dim,
                            ));
                        }
                    }
                    lines.push(Line::raw(""));
                    for (k, a) in &doc.args {
                        lines.push(row(
                            if a.required { "Needs" } else { "Takes" },
                            format!("{k} — {}", a.description.clone().unwrap_or_default()),
                        ));
                    }
                    lines.push(row(
                        "Typical",
                        crate::app::workflows_view::estimate_text(doc),
                    ));
                    lines.push(row(
                        "Runs on",
                        match &doc.harness {
                            crate::workflows::document::HarnessFilter::Any => {
                                "any CLI you have a profile for".to_string()
                            }
                            crate::workflows::document::HarnessFilter::Only(l) => l.join(", "),
                        },
                    ));
                }
                None => {
                    for p in &entry.problems {
                        lines.push(Line::styled(
                            format!("  ! {p}"),
                            Style::default().fg(Color::Red),
                        ));
                    }
                }
            }
            if let Some(r) = app
                .recent_workflow_runs
                .iter()
                .find(|r| r.name == entry.name)
            {
                lines.push(row(
                    "Last run",
                    format!(
                        "{} · {} sessions · {} tokens · ${:.2}",
                        r.status,
                        r.sessions,
                        crate::loops::format_tokens(r.tokens),
                        r.cost_usd
                    ),
                ));
            }
            lines.push(Line::raw(""));
            lines.push(Line::styled(
                "  [Enter] run   [c] compose your own   [e] edit   [W] view",
                dim,
            ));
            block_with(
                format!(" {} · {} ", entry.name, entry.source.label()),
                glyph.color(),
            )
        }
    };
    let inner = block.inner(area);
    f.render_widget(block, area);
    f.render_widget(Paragraph::new(hang(lines, usize::from(inner.width))), inner);
}

/// A dialog text field over several rows: the label, then the wrapped
/// text with the cursor on it. An unfocused field shows one line. The
/// width drawn with is written back so `↑`/`↓` move by visual row.
fn text_area_lines(
    label: &str,
    ta: &crate::app::text_area::TextArea,
    active: bool,
    rows: usize,
    width: usize,
) -> Vec<Line<'static>> {
    const LABEL: usize = 12;
    let dim = Style::default().fg(Color::DarkGray);
    // a long label (`arg replacement`) must not eat into the text
    let label = truncate_chars(label, LABEL - 1);
    let content = width.saturating_sub(LABEL).max(8);
    ta.width.set(content);
    let mut out: Vec<Line> = Vec::new();
    if !active {
        let text = ta.preview(content);
        out.push(Line::from(vec![
            Span::styled(format!("{label:<LABEL$}"), dim),
            if text.is_empty() {
                Span::styled("(empty)", dim)
            } else {
                Span::raw(text)
            },
        ]));
        return out;
    }
    let visual = ta.rows();
    let (cur_row, cur_col) = ta.cursor_at(&visual);
    let rows = rows.max(1);
    let start = cur_row
        .saturating_sub(rows - 1)
        .min(visual.len().saturating_sub(1));
    let end = (start + rows).min(visual.len());
    for (n, row) in visual[start..end].iter().enumerate() {
        let i = start + n;
        let text = &ta.text[row.start..row.end];
        let head = if i == start {
            Span::styled(format!("{label:<LABEL$}"), dim)
        } else {
            Span::raw(" ".repeat(LABEL))
        };
        let mut spans = vec![head];
        if i == cur_row {
            // the cursor cell is reversed; at the end of a row it is a space
            let before: String = text.chars().take(cur_col).collect();
            let at: String = text.chars().skip(cur_col).take(1).collect();
            let after: String = text.chars().skip(cur_col + 1).collect();
            spans.push(Span::raw(before));
            spans.push(Span::styled(
                if at.is_empty() { " ".to_string() } else { at },
                Style::default().add_modifier(Modifier::REVERSED),
            ));
            if !after.is_empty() {
                spans.push(Span::raw(after));
            }
        } else {
            spans.push(Span::raw(text.to_string()));
        }
        out.push(Line::from(spans));
    }
    if end < visual.len() {
        out.push(Line::styled(
            format!("{}… {} more line(s)", " ".repeat(LABEL), visual.len() - end),
            dim,
        ));
    }
    out
}

fn draw_workflow_dialog(f: &mut Frame, dialog: &WorkflowDialogState) {
    let width = 92.min(f.area().width.saturating_sub(4)).max(40);
    let height = 34.min(f.area().height.saturating_sub(2)).max(14);
    let area = centered(f.area(), width, height);
    f.render_widget(Clear, area);
    let inner_width = usize::from(width.saturating_sub(2));
    let title = match &dialog.purpose {
        DialogPurpose::Run { name } => format!(" Run {name} "),
        DialogPurpose::Plan => " Compose a workflow ".to_string(),
    };
    let block = Block::default().borders(Borders::ALL).title(title);
    let dim = Style::default().fg(Color::DarkGray);
    let sel = |on: bool| {
        if on {
            Style::default().add_modifier(Modifier::REVERSED)
        } else {
            Style::default()
        }
    };
    let field = |label: &str, value: String, on: bool| {
        Line::from(vec![
            Span::styled(format!("{label:<12}"), dim),
            Span::styled(value, sel(on)),
        ])
    };
    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::styled(format!("  {}", dialog.description), dim));
    if dialog.purpose == DialogPurpose::Plan {
        lines.extend(text_area_lines(
            "Task",
            &dialog.task,
            dialog.field == WfField::Task,
            dialog.rows_for(WfField::Task),
            inner_width,
        ));
        if dialog.field == WfField::Task {
            lines.push(Line::styled(
                "            what the composed workflow should do; Alt+Enter newline, Ctrl+E editor, Ctrl+V paste",
                dim,
            ));
        }
    }
    // A run's dialog asks what the run is for first; a plan's, the task.
    let args_lines = |lines: &mut Vec<Line<'static>>| {
        for (i, name) in dialog.arg_names.iter().enumerate() {
            if let Some(ta) = dialog.args.get(i) {
                // the name alone: the help line under it says what it is
                lines.extend(text_area_lines(
                    name,
                    ta,
                    dialog.field == WfField::Arg(i),
                    dialog.rows_for(WfField::Arg(i)),
                    inner_width,
                ));
            }
            if let Some(h) = dialog.arg_help.get(i)
                && !h.is_empty()
            {
                lines.push(Line::styled(format!("            {h}"), dim));
            }
        }
    };
    args_lines(&mut lines);
    // the folder list only while the field is focused: otherwise one line
    if dialog.field == WfField::Workspace {
        let max_visible = usize::from(height).saturating_sub(18).clamp(1, 4);
        lines.extend(dir_picker_lines(
            &format!("{:<12}", "Workspace"),
            &dialog.workspace,
            &dialog.dir_picker,
            true,
            max_visible,
        ));
    } else {
        lines.push(field("Workspace", dialog.workspace.clone(), false));
    }
    let profile_text = if dialog.profiles.is_empty() {
        "no profile for this workflow's harnesses in profiles.toml".to_string()
    } else {
        dialog
            .profiles
            .get(dialog.profile_idx)
            .map(|(n, h)| format!("{n} ({})", h.as_str()))
            .unwrap_or_default()
    };
    lines.push(field(
        "Profile",
        profile_text,
        dialog.field == WfField::Profile,
    ));
    if dialog.purpose == DialogPurpose::Plan {
        lines.push(field(
            "Budget",
            format!("{} tokens ", dialog.budget.text),
            dialog.field == WfField::Budget,
        ));
    } else {
        lines.push(Line::from(vec![
            Span::styled(format!("{:<12}", "More"), dim),
            Span::styled(
                if dialog.more { "▾ " } else { "▸ " }.to_string(),
                sel(dialog.field == WfField::More),
            ),
            Span::styled(dialog.more_summary(), dim),
        ]));
        if dialog.more {
            lines.push(field(
                "  Budget",
                format!("{} tokens ", dialog.budget.text),
                dialog.field == WfField::Budget,
            ));
            lines.push(field(
                "  Max cost",
                format!("{} USD ", dialog.max_cost.text),
                dialog.field == WfField::MaxCost,
            ));
            lines.push(field(
                "  Isolation",
                match dialog.isolation {
                    crate::workflows::document::Isolation::None => "none".into(),
                    crate::workflows::document::Isolation::Worktree => "worktree".into(),
                },
                dialog.field == WfField::Isolation,
            ));
            // one row per step: the harness it runs on for this run
            let id_width = dialog.step_ids.iter().map(|s| s.len()).max().unwrap_or(0);
            for (i, id) in dialog.step_ids.iter().enumerate() {
                let choice = match dialog.step_harness.get(i).copied().flatten() {
                    Some(h) => format!("{} · chosen for this run", h.as_str()),
                    None => dialog.step_default_label(i),
                };
                lines.push(field(
                    if i == 0 { "  Steps" } else { "" },
                    format!("{id:<id_width$}  {choice}"),
                    dialog.field == WfField::StepHarness(i),
                ));
            }
            if matches!(dialog.field, WfField::StepHarness(_)) {
                lines.push(Line::styled(
                    "            ←/→ or Space: the CLI this step runs on, for this run only",
                    dim,
                ));
            }
        } else if dialog.field == WfField::More {
            lines.push(Line::styled(
                "            Space or →: budget, cost cap, isolation and each step's CLI",
                dim,
            ));
        }
        if !dialog.estimate.is_empty() {
            lines.push(Line::styled(format!("  {}", dialog.estimate), dim));
        }
    }
    lines.push(Line::raw(""));
    if let Some(e) = &dialog.error {
        lines.push(Line::styled(
            format!("  {e}"),
            Style::default().fg(Color::Red),
        ));
    }
    lines.push(Line::styled(
        "  [Enter] start  [Tab] field  [Alt+Enter] newline  [Ctrl+E] editor  [Ctrl+V] paste  [Esc] cancel",
        dim,
    ));
    f.render_widget(Paragraph::new(lines).block(block), area);
}

/// The inbox (`I`): what waits on a human, grouped by where it came
/// from, with the selected item in full on the right.
fn draw_inbox(f: &mut Frame, state: &crate::app::inbox::InboxState) {
    use crate::app::inbox::InboxItem;
    let area = f.area();
    f.render_widget(Clear, area);
    let [body, footer] = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(area);
    let [left, right] =
        Layout::horizontal([Constraint::Percentage(40), Constraint::Min(0)]).areas(body);
    let dim = Style::default().fg(Color::DarkGray);
    let head = Style::default()
        .fg(Color::Yellow)
        .add_modifier(Modifier::BOLD);

    let left_block = Block::default()
        .borders(Borders::ALL)
        .border_style(pane_border(true))
        .title(format!(" Needs you ({}) ", state.items.len()));
    let mut lines: Vec<Line> = Vec::new();
    if let Some(e) = &state.error {
        lines.push(Line::styled(
            format!(" {e}"),
            Style::default().fg(Color::Yellow),
        ));
    }
    if state.items.is_empty() {
        lines.push(Line::styled(
            " nothing waits on you: loop fixes to apply, plans to review and runs that did not finish land here",
            dim,
        ));
    }
    let width = usize::from(left.width.saturating_sub(2));
    let mut group: Option<&str> = None;
    let mut selected_line = 0usize;
    for (i, item) in state.items.iter().enumerate() {
        if group != Some(item.group()) {
            group = Some(item.group());
            lines.push(Line::styled(format!(" {}", item.group()), head));
        }
        let what = match item {
            InboxItem::Loop(r) => format!(
                "{} @ {}",
                r.pattern,
                std::path::Path::new(&r.workspace)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default()
            ),
            InboxItem::Plan { name, .. } => name.clone(),
            InboxItem::Run { name, .. } => name.clone(),
        };
        let sel = i == state.selected;
        if sel {
            selected_line = lines.len();
        }
        let text = format!(
            "{}{:<18} {}",
            if sel { "> " } else { "  " },
            item.word(),
            what
        );
        let style = if sel {
            Style::default().add_modifier(Modifier::REVERSED)
        } else {
            Style::default()
        };
        lines.push(Line::styled(truncate_chars(&text, width), style));
    }
    let visible = usize::from(left.height.saturating_sub(2));
    let start = sidebar_window(selected_line, lines.len(), visible);
    f.render_widget(
        Paragraph::new(lines.into_iter().skip(start).collect::<Vec<_>>()).block(left_block),
        left,
    );

    let right_block = Block::default()
        .borders(Borders::ALL)
        .border_style(pane_border(false))
        .title(" Detail ");
    let inner = right_block.inner(right);
    f.render_widget(right_block, right);
    let mut detail: Vec<Line<'static>> = Vec::new();
    let row = |k: &str, v: String| {
        Line::from(vec![Span::styled(format!("  {k:<10} "), dim), Span::raw(v)])
    };
    match state.selected_item() {
        Some(InboxItem::Loop(r)) => {
            detail.push(Line::styled(
                format!(
                    "  {} · {} · {}",
                    r.outcome.word().to_uppercase(),
                    r.pattern,
                    r.id
                ),
                head,
            ));
            detail.push(row("workspace", r.workspace.clone()));
            if let Some(b) = &r.branch {
                detail.push(row(
                    "branch",
                    format!("{b}   (merge it yourself; agent-mux never merges)"),
                ));
            }
            if let Some(w) = &r.worktree {
                detail.push(row("worktree", w.clone()));
            }
            detail.extend(crate::app::loops_view::run_detail_lines(r));
            if let Some(stat) = r.detail_str("diff_stat") {
                detail.push(Line::styled("    diff --stat", dim));
                for s in stat.lines().take(20) {
                    detail.push(Line::raw(format!("      {s}")));
                }
            }
            detail.push(Line::raw(""));
            if r.branch.is_some() {
                detail.push(Line::styled(
                    "  [a] applied: the worktree goes, the branch stays for you to merge",
                    dim,
                ));
                detail.push(Line::styled(
                    "  [x] rejected: worktree and branch are removed",
                    dim,
                ));
            } else {
                detail.push(Line::styled(
                    "  [a] done: you acted on what it found   [x] dismiss: nothing to do",
                    dim,
                ));
            }
        }
        Some(InboxItem::Plan {
            name,
            task,
            problems,
            ..
        }) => {
            detail.push(Line::styled(format!("  PLAN · {name}"), head));
            detail.push(row("task", task.clone()));
            if problems.is_empty() {
                detail.push(row("status", "valid; nothing has run yet".into()));
            }
            for p in problems {
                detail.push(Line::styled(
                    format!("  ! {p}"),
                    Style::default().fg(Color::Red),
                ));
            }
            detail.push(Line::raw(""));
            detail.push(Line::styled(
                "  [Enter] review it in the Workflows view: run, edit, save or discard",
                dim,
            ));
        }
        Some(InboxItem::Run {
            run_id,
            name,
            status,
            error,
            notes,
        }) => {
            detail.push(Line::styled(
                format!("  {} · {name} · {run_id}", status.to_uppercase()),
                head,
            ));
            if let Some(e) = error {
                detail.push(Line::styled(
                    format!("  {e}"),
                    Style::default().fg(Color::Red),
                ));
            }
            for n in notes.iter().take(8) {
                detail.push(Line::styled(format!("  · {n}"), dim));
            }
            detail.push(Line::raw(""));
            detail.push(Line::styled(
                "  [Enter] its report and steps   [x] dismiss from the inbox",
                dim,
            ));
        }
        None => {}
    }
    f.render_widget(
        Paragraph::new(hang(detail, usize::from(inner.width))),
        inner,
    );

    let hints = state
        .selected_item()
        .map(|i| i.hints())
        .unwrap_or(" [Esc] close");
    f.render_widget(
        Paragraph::new(Line::styled(
            fit_hints(hints, usize::from(footer.width)),
            Style::default().fg(Color::Black).bg(Color::Cyan),
        )),
        footer,
    );
}

fn draw_workflows_view(f: &mut Frame, view: &WorkflowsViewState, app: &App) {
    // The whole screen, footer over the status bar: a box inset over the
    // sidebar left fragments of it showing down both edges.
    let area = f.area();
    f.render_widget(Clear, area);
    let [body, footer] = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(area);
    let [left, right] =
        Layout::horizontal([Constraint::Percentage(32), Constraint::Min(0)]).areas(body);
    let facts = app.view_facts();
    let left_block = Block::default()
        .borders(Borders::ALL)
        .border_style(pane_border(view.focus == ViewPane::Runs))
        .title(format!(
            " Workflow runs ({}) ",
            view.rows
                .iter()
                .filter(|r| !matches!(r, RunRow::Header(_)))
                .count()
        ));
    if view.rows.is_empty() {
        let p = Paragraph::new("\n  no runs yet\n\n  Enter on a workflow, or c to compose one")
            .style(Style::default().fg(Color::DarkGray))
            .block(left_block);
        f.render_widget(p, left);
    } else {
        let visible = usize::from(left.height.saturating_sub(2));
        let start = sidebar_window(view.selected, view.rows.len(), visible);
        let end = (start + visible.max(1)).min(view.rows.len());
        let items: Vec<ListItem> = view.rows[start..end]
            .iter()
            .enumerate()
            .map(|(offset, row)| {
                let i = start + offset;
                let is_sel = i == view.selected;
                let marker = if is_sel { "> " } else { "  " };
                let line = match row {
                    RunRow::Header(h) => Line::styled(
                        h.to_string(),
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    ),
                    RunRow::Planned(id) => {
                        let p = facts.planned.iter().find(|p| p.id == *id);
                        Line::from(vec![
                            Span::raw(marker),
                            Span::styled("? ", Style::default().fg(Color::Yellow)),
                            Span::raw(p.map(|p| p.name.clone()).unwrap_or_default()),
                            Span::styled(
                                p.map(|p| {
                                    if p.valid() { " valid" } else { " problems" }.to_string()
                                })
                                .unwrap_or_default(),
                                Style::default().fg(Color::DarkGray),
                            ),
                        ])
                    }
                    RunRow::Live(id) => {
                        let r = facts.live.iter().find(|r| r.run_id == *id);
                        Line::from(vec![
                            Span::raw(marker),
                            Span::styled("▶ ", Style::default().fg(Color::Cyan)),
                            Span::raw(r.map(|r| r.name.clone()).unwrap_or_default()),
                            Span::styled(
                                r.map(|r| format!(" {}", r.progress())).unwrap_or_default(),
                                Style::default().fg(Color::DarkGray),
                            ),
                        ])
                    }
                    RunRow::Stored(id) => {
                        let r = view.stored.iter().find(|r| r.id == *id);
                        let (g, c) = match r.map(|r| r.status.as_str()) {
                            Some("finished") => ("✓", Color::Green),
                            Some("running") => ("▶", Color::Cyan),
                            _ => ("!", Color::Red),
                        };
                        Line::from(vec![
                            Span::raw(marker),
                            Span::styled(format!("{g} "), Style::default().fg(c)),
                            Span::raw(r.map(|r| r.workflow.clone()).unwrap_or_default()),
                            Span::styled(
                                r.map(|r| format!(" {} · {}", &r.id[..8], r.harness))
                                    .unwrap_or_default(),
                                Style::default().fg(Color::DarkGray),
                            ),
                        ])
                    }
                };
                let item = ListItem::new(line);
                if is_sel && view.focus == ViewPane::Runs {
                    item.style(Style::default().add_modifier(Modifier::REVERSED))
                } else if is_sel {
                    item.style(Style::default().fg(Color::Cyan))
                } else {
                    item
                }
            })
            .collect();
        f.render_widget(List::new(items).block(left_block), left);
    }

    // the detail pane's size, known before its block is built, so the title
    // can carry an accurate scroll position on the very first frame
    view.measure(
        right.width.saturating_sub(2),
        right.height.saturating_sub(2),
    );
    let mut title_spans = vec![Span::raw(" ")];
    for (i, tab) in crate::app::workflows_view::ViewTab::ALL
        .into_iter()
        .enumerate()
    {
        if i > 0 {
            title_spans.push(Span::raw(" | "));
        }
        title_spans.push(if tab == view.tab {
            Span::styled(
                tab.label(),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            Span::styled(tab.label(), Style::default().fg(Color::DarkGray))
        });
    }
    // the scroll position comes before the run's name: the name is the part
    // a narrow pane may cut off
    if let Some(pos) = view.scroll_position() {
        title_spans.push(Span::styled(
            format!(" · {pos}"),
            Style::default().fg(Color::DarkGray),
        ));
    }
    let name = view.selected_title(&facts);
    if !name.is_empty() {
        title_spans.push(Span::raw(format!(" · {name} ")));
    } else {
        title_spans.push(Span::raw(" "));
    }
    let right_block = Block::default()
        .borders(Borders::ALL)
        .border_style(pane_border(view.focus == ViewPane::Detail))
        .title(Line::from(title_spans));
    let inner = right_block.inner(right);
    f.render_widget(right_block, right);
    // Long result lines are wrapped, not cut off: `visible_rows` records the
    // pane's size so the scroll counts the rows actually drawn.
    f.render_widget(
        Paragraph::new(view.visible_rows(inner.width, inner.height)),
        inner,
    );
    let footer_style = if matches!(view.pending, ViewPending::None) {
        Style::default().fg(Color::Black).bg(Color::Cyan)
    } else {
        Style::default().fg(Color::Black).bg(Color::Yellow)
    };
    f.render_widget(
        Paragraph::new(Line::styled(
            fit_hints(&view.footer(), usize::from(footer.width)),
            footer_style,
        )),
        footer,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::App;
    use crate::config::Config;
    use crate::status::Status;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use std::time::Instant;

    fn buffer_text(terminal: &Terminal<TestBackend>) -> String {
        let buffer = terminal.backend().buffer();
        let mut out = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                out.push_str(buffer[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn status_labels() {
        assert_eq!(status_label_style(Status::Working).0, "working");
        assert_eq!(status_label_style(Status::Idle).0, "idle");
        assert_eq!(status_label_style(Status::NeedsAttention).0, "attention");
        assert_eq!(status_label_style(Status::Exited(Some(2))).0, "exit 2");
        assert_eq!(status_label_style(Status::Exited(None)).0, "exited");
    }

    #[test]
    fn main_pane_inner_accounts_for_sidebar_borders_and_status_bar() {
        // 100 cols x 40 rows total:
        // cols: 100 - 30 sidebar - 2 border = 68
        // rows: 40 - 1 status bar - 2 border = 37
        let total = ratatui::layout::Rect::new(0, 0, 100, 40);
        assert_eq!(main_pane_inner(total), (37, 68));
    }

    #[test]
    fn main_pane_inner_never_returns_zero() {
        let tiny = ratatui::layout::Rect::new(0, 0, 5, 2);
        let (rows, cols) = main_pane_inner(tiny);
        assert!(rows >= 1 && cols >= 1);
    }

    #[test]
    fn pane_local_matches_layout_math() {
        // pane interior starts right of the sidebar border column
        assert_eq!(PANE_ORIGIN, (SIDEBAR_WIDTH + 1, 1));
        let pane = (37u16, 68u16); // (rows, cols) for a 100x40 terminal
        assert_eq!(pane_local(31, 1, pane), Some((0, 0)));
        assert_eq!(pane_local(31 + 67, 1 + 36, pane), Some((67, 36)));
        // one past either edge is outside
        assert_eq!(pane_local(30, 1, pane), None); // sidebar border
        assert_eq!(pane_local(31 + 68, 1, pane), None);
        assert_eq!(pane_local(31, 0, pane), None); // top border
        assert_eq!(pane_local(31, 1 + 37, pane), None);
    }

    #[test]
    fn pane_clamped_clamps_outside_coordinates() {
        let pane = (37u16, 68u16); // (rows, cols) matching pane_local_matches_layout_math
        // inside: same result as pane_local (passthrough)
        assert_eq!(pane_clamped(31, 1, pane), (0, 0));
        assert_eq!(pane_clamped(31 + 67, 1 + 36, pane), (67, 36));
        // left of the pane -> clamps to the left edge column
        assert_eq!(pane_clamped(0, 5, pane), (0, 4));
        // right of the pane -> clamps to the last column
        assert_eq!(pane_clamped(200, 5, pane), (67, 4));
        // above the pane -> clamps to the top row
        assert_eq!(pane_clamped(35, 0, pane), (4, 0));
        // below the pane -> clamps to the last row
        assert_eq!(pane_clamped(35, 100, pane), (4, 36));
    }

    #[test]
    fn main_pane_inner_dims_accounts_for_hidden_sidebar() {
        let total = ratatui::layout::Rect::new(0, 0, 100, 40);
        // when sidebar is visible (SIDEBAR_WIDTH = 30):
        // cols = 100 - 30 - 2 = 68, rows = 40 - 1 - 2 = 37
        assert_eq!(main_pane_inner_dims(total, false), (37, 68));
        // when sidebar is hidden:
        // cols = 100 - 0 - 2 = 98, rows = 40 - 1 - 2 = 37
        assert_eq!(main_pane_inner_dims(total, true), (37, 98));
    }

    #[test]
    fn pane_local_with_sidebar_hidden() {
        assert_eq!(pane_origin(true), (1, 1));
        let pane = (37u16, 98u16);
        assert_eq!(pane_local_with_sidebar(1, 1, pane, true), Some((0, 0)));
        assert_eq!(
            pane_local_with_sidebar(1 + 97, 1 + 36, pane, true),
            Some((97, 36))
        );
        assert_eq!(pane_local_with_sidebar(0, 1, pane, true), None); // left border
        assert_eq!(pane_local_with_sidebar(1 + 98, 1, pane, true), None);
    }

    #[test]
    fn hidden_sidebar_renders_full_width_harness() {
        let (tx, _rx) = tokio::sync::mpsc::channel(4);
        let mut app = App::new(Config::default_profiles(), None, tx);
        app.sidebar_hidden = true;
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| draw(f, &app, Instant::now())).unwrap();
        let text = buffer_text(&terminal);
        // Sidebar active/history headers shouldn't be rendered
        assert!(
            !text.contains("Active Sessions"),
            "unexpected sidebar in {text}"
        );
        assert!(
            !text.contains("Past Sessions"),
            "unexpected sidebar in {text}"
        );
        // Status bar advertises [b] sidebar
        assert!(text.contains("[b] sidebar"), "missing sidebar hint: {text}");
    }

    #[test]
    fn empty_app_renders_hint() {
        let (tx, _rx) = tokio::sync::mpsc::channel(4);
        let app = App::new(Config::default_profiles(), None, tx);
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| draw(f, &app, Instant::now())).unwrap();
        let text = buffer_text(&terminal);
        assert!(text.contains("no sessions"), "got: {text}");
        assert!(text.contains("[n]"), "keybinding hint missing: {text}");
    }

    #[test]
    fn sidebar_window_keeps_selection_visible() {
        // fits: never scrolls
        assert_eq!(sidebar_window(0, 3, 5), 0);
        assert_eq!(sidebar_window(2, 3, 5), 0);
        // overflow: 0 until the selection passes the window, then follows
        assert_eq!(sidebar_window(0, 10, 4), 0);
        assert_eq!(sidebar_window(3, 10, 4), 0);
        assert_eq!(sidebar_window(4, 10, 4), 1);
        assert_eq!(sidebar_window(9, 10, 4), 6);
        // degenerate viewport
        assert_eq!(sidebar_window(5, 10, 0), 0);
    }

    #[test]
    fn a_hint_line_drops_whole_hints_and_keeps_its_first_and_last() {
        let hints = "[b] sidebar  [Enter] details  [r] run now  [p] pause  [?] help";
        assert_eq!(
            fit_hints(hints, 80),
            hints,
            "a line that fits is left alone"
        );
        let cut = fit_hints(hints, 44);
        assert_eq!(cut, "[b] sidebar  [Enter] details  [?] help");
        assert!(cut.chars().count() <= 44);
        // a leading space is not a hint of its own
        assert_eq!(
            fit_hints(" [Tab] tab  [r] run now  [Esc] close", 24),
            " [Tab] tab  [Esc] close"
        );
        // two pieces are the floor, even when they are too wide
        assert_eq!(fit_hints("[a] one  [b] two", 4), "[a] one  [b] two");
    }

    #[test]
    fn truncate_chars_is_multibyte_safe() {
        assert_eq!(truncate_chars("short", 18), "short");
        // 19 emoji: byte-index slicing panicked here before
        let emoji = "🚀".repeat(19);
        let out = truncate_chars(&emoji, 18);
        assert!(out.ends_with("..."));
        assert_eq!(out.chars().count(), 18);
        assert_eq!(
            truncate_path_chars("/home/silvio/workspace/agent-mux", 20),
            "…workspace/agent-mux"
        );
    }

    #[test]
    fn help_overlay_lists_hidden_chords() {
        let (tx, _rx) = tokio::sync::mpsc::channel(4);
        let mut app = App::new(Config::default_profiles(), None, tx);
        app.mode = crate::app::Mode::Help;
        let mut terminal = Terminal::new(TestBackend::new(100, 64)).unwrap();
        terminal.draw(|f| draw(f, &app, Instant::now())).unwrap();
        let text = buffer_text(&terminal);
        for needle in [
            "Ctrl+Shift+C/V",
            "Ctrl+Shift+F",
            "PgUp/PgDn",
            "Ctrl+Q Ctrl+Q",
            "jump to session N",
            "launch / attach the selected agent",
            "skills view",
            "open the selected execution's traces",
            "list → tree → timeline → loop",
            "fold a loop or workflow run",
            "fold the session's loop or workflow",
            "also sent to Langfuse",
            "resume the selected session",
            "clear all exited sessions",
        ] {
            assert!(
                text.contains(needle),
                "help overlay missing {needle}: {text}"
            );
        }
    }

    #[test]
    fn platform_key_labels_follow_terminal_conventions() {
        let mac = platform_keys_for(true);
        assert_eq!(mac.page_scroll, "Fn+↑/↓ (PgUp/PgDn)");
        assert_eq!(mac.word_navigation, "Option+←/→");

        let other = platform_keys_for(false);
        assert_eq!(other.page_scroll, "PgUp/PgDn");
        assert_eq!(other.word_navigation, "Ctrl+←/→");
    }

    #[test]
    fn control_hint_advertises_help() {
        let (tx, _rx) = tokio::sync::mpsc::channel(4);
        let app = App::new(Config::default_profiles(), None, tx);
        let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
        terminal.draw(|f| draw(f, &app, Instant::now())).unwrap();
        assert!(buffer_text(&terminal).contains("[?] help"));
    }

    #[test]
    fn active_control_hint_advertises_clear_exited() {
        let (tx, _rx) = tokio::sync::mpsc::channel(4);
        let app = App::new(Config::default_profiles(), None, tx);
        let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
        terminal.draw(|f| draw(f, &app, Instant::now())).unwrap();
        assert!(buffer_text(&terminal).contains("[X] clear exited"));
    }

    #[test]
    fn notice_levels_render_distinct_colors() {
        let (tx, _rx) = tokio::sync::mpsc::channel(4);
        let mut app = App::new(Config::default_profiles(), None, tx);
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        for (notice, want) in [
            (crate::app::Notice::info("tracing started"), Color::Cyan),
            (
                crate::app::Notice::warn("tracing: check config"),
                Color::Yellow,
            ),
            (crate::app::Notice::error("write failed"), Color::Red),
        ] {
            app.notice = Some(notice);
            terminal.draw(|f| draw(f, &app, Instant::now())).unwrap();
            let cell = &terminal.backend().buffer()[(0, 23)];
            assert_eq!(cell.style().fg, Some(want), "level color mismatch");
        }
    }

    #[test]
    fn pseudo_terminal_widget_renders_parser_screen() {
        // Wiring check for tui-term without needing a real Session.
        let mut parser = vt100::Parser::new(5, 20, 0);
        parser.process(b"hello-widget");
        let mut terminal = Terminal::new(TestBackend::new(20, 5)).unwrap();
        terminal
            .draw(|f| {
                let widget = tui_term::widget::PseudoTerminal::new(parser.screen());
                f.render_widget(widget, f.area());
            })
            .unwrap();
        assert!(buffer_text(&terminal).contains("hello-widget"));
    }

    #[test]
    fn selection_highlight_marks_expected_cells() {
        use crate::selection::{Pos, Selection};
        use ratatui::buffer::Buffer;
        let area = Rect::new(2, 1, 10, 4); // inner pane at (2,1), 10 cols x 4 rows
        let mut buf = Buffer::empty(Rect::new(0, 0, 20, 10));
        // len=6, offset=2 -> visual row v shows abs row 4 + v
        let sel = Selection {
            anchor: Pos { row: 5, col: 3 },
            head: Pos { row: 6, col: 1 },
        };
        apply_selection_highlight(&mut buf, area, &sel, 6, 2);
        // abs 5 = visual 1, abs 6 = visual 2
        assert!(buf[(2 + 3, 1 + 1)].modifier.contains(Modifier::REVERSED));
        assert!(buf[(2 + 9, 1 + 1)].modifier.contains(Modifier::REVERSED)); // rest of first row
        assert!(buf[(2, 1 + 2)].modifier.contains(Modifier::REVERSED)); // start of last row
        assert!(buf[(2 + 1, 1 + 2)].modifier.contains(Modifier::REVERSED)); // inclusive end
        assert!(!buf[(2 + 2, 1 + 2)].modifier.contains(Modifier::REVERSED));
        assert!(!buf[(2 + 2, 1)].modifier.contains(Modifier::REVERSED)); // row above
        assert!(!buf[(2 + 3, 1 + 3)].modifier.contains(Modifier::REVERSED)); // row below
    }

    #[test]
    fn search_highlight_styles_match_cells() {
        use crate::search::Match;
        use ratatui::buffer::Buffer;
        let area = Rect::new(2, 1, 10, 4);
        let mut buf = Buffer::empty(Rect::new(0, 0, 20, 10));
        let matches = vec![
            Match {
                row: 5,
                col_start: 1,
                col_end: 3,
            },
            Match {
                row: 6,
                col_start: 0,
                col_end: 2,
            },
        ];
        // len=6, offset=2: abs 5 -> visual 1, abs 6 -> visual 2
        apply_search_highlight(&mut buf, area, &matches, 1, 6, 2);
        // non-current match: yellow bg
        let cell = &buf[(2 + 1, 1 + 1)];
        assert_eq!(cell.style().bg, Some(Color::Yellow));
        // current match (index 1): white bg + bold
        let cell = &buf[(2, 1 + 2)];
        assert_eq!(cell.style().bg, Some(Color::White));
        assert!(cell.style().add_modifier.contains(Modifier::BOLD));
        // outside any match: untouched (ratatui's default Cell bg is
        // Color::Reset, not None -- style() always reports Some(_))
        assert_eq!(buf[(2 + 5, 1 + 1)].style().bg, Some(Color::Reset));
    }

    #[test]
    fn search_bar_renders_query_and_count() {
        let (tx, _rx) = tokio::sync::mpsc::channel(4);
        let mut app = App::new(Config::default_profiles(), None, tx);
        let mut st = crate::search::SearchState::new();
        st.query = "hello".into();
        app.search = Some(st);
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| draw(f, &app, Instant::now())).unwrap();
        let text = buffer_text(&terminal);
        assert!(text.contains("Search: hello"), "got: {text}");
        assert!(
            text.contains("no matches"),
            "empty result indicator: {text}"
        );
    }

    #[test]
    fn dialog_mode_renders_profile_and_dir() {
        let (tx, _rx) = tokio::sync::mpsc::channel(4);
        let mut app = App::new(Config::default_profiles(), None, tx);
        app.mode = crate::app::Mode::NewSession(crate::app::DialogState::new(&app.profiles));
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| draw(f, &app, Instant::now())).unwrap();
        let text = buffer_text(&terminal);
        assert!(text.contains("New session"), "got: {text}");
        assert!(text.contains("Claude Code"), "got: {text}");
        assert!(text.contains("Tracing:"), "got: {text}");
        assert!(text.contains("Content Mode:"), "got: {text}");
        // the default profiles run real CLIs, so the options show
        assert!(text.contains("claude options"), "got: {text}");
        assert!(text.contains("Model:"), "got: {text}");
        assert!(text.contains("Approvals:"), "got: {text}");
        assert!(text.contains("Resume:"), "got: {text}");
        assert!(text.contains("One-shot prompt:"), "got: {text}");
    }

    #[test]
    fn launch_options_render_their_values_and_hide_for_a_plain_command() {
        let (tx, _rx) = tokio::sync::mpsc::channel(4);
        let profiles = vec![
            crate::config::Profile {
                name: "Codex".into(),
                command: "codex".into(),
                args: vec![],
                default_dir: None,
                tracing: None,
                model: None,
                bypass_approvals: None,
            },
            crate::config::Profile {
                name: "Shell".into(),
                command: "bash".into(),
                args: vec![],
                default_dir: None,
                tracing: None,
                model: None,
                bypass_approvals: None,
            },
        ];
        let mut app = App::new(profiles.clone(), None, tx);
        let mut dialog = crate::app::DialogState::new(&profiles);
        dialog.model = "gpt-5.6".into();
        dialog.bypass_approvals = true;
        dialog.resume_last = true;
        dialog.one_shot = "fix the failing test".into();
        app.mode = crate::app::Mode::NewSession(dialog);
        let mut terminal = Terminal::new(TestBackend::new(90, 26)).unwrap();
        terminal.draw(|f| draw(f, &app, Instant::now())).unwrap();
        let text = buffer_text(&terminal);
        assert!(text.contains("codex options"), "got: {text}");
        assert!(text.contains("gpt-5.6"), "got: {text}");
        // the hints name the flag this CLI actually uses
        assert!(text.contains("--yolo"), "got: {text}");
        assert!(text.contains("resume --last"), "got: {text}");
        assert!(text.contains("codex exec"), "got: {text}");
        assert!(text.contains("fix the failing test"), "got: {text}");

        // an unset model reads as the CLI's default, not as a blank
        if let crate::app::Mode::NewSession(d) = &mut app.mode {
            d.model.clear();
            d.one_shot.clear();
        }
        terminal.draw(|f| draw(f, &app, Instant::now())).unwrap();
        let text = buffer_text(&terminal);
        assert!(text.contains("(the CLI's default)"), "got: {text}");
        assert!(text.contains("(interactive)"), "got: {text}");

        // a profile that is not a known CLI shows no options at all
        if let crate::app::Mode::NewSession(d) = &mut app.mode {
            *d = crate::app::DialogState::new(&profiles[1..]);
        }
        terminal.draw(|f| draw(f, &app, Instant::now())).unwrap();
        let text = buffer_text(&terminal);
        assert!(!text.contains("Approvals:"), "got: {text}");
        assert!(!text.contains("One-shot"), "got: {text}");
        assert!(
            text.contains("Content Mode:"),
            "the rest still renders: {text}"
        );
    }

    #[test]
    fn session_history_mode_renders_split_view() {
        let (tx, _rx) = tokio::sync::mpsc::channel(4);
        let mut app = App::new(Config::default_profiles(), None, tx);
        let mut history = crate::app::HistoryState::new(None);
        history.sessions = vec![crate::history::SessionSummary {
            session_id: "test-uuid-1234".into(),
            title: "Test History Title".into(),
            modified: std::time::SystemTime::UNIX_EPOCH,
            file_path: std::path::PathBuf::from("/fake/path.jsonl"),
            turn_count: 5,
            project_slug: "-test".into(),
            timestamp_str: "2026-08-30 19:28".into(),
            provider: crate::history::AgentProvider::Claude,
            cwd: None,
        }];
        history.log_lines = vec![
            ratatui::text::Line::raw("👤 USER: hello world"),
            ratatui::text::Line::raw("🤖 CLAUDE: hi there!"),
        ];
        // HistoryState::new may have scrolled a machine-local log; this
        // synthetic 2-line log starts at the top
        history.scroll_offset = 0;
        app.mode = crate::app::Mode::SessionHistory(history);
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal.draw(|f| draw(f, &app, Instant::now())).unwrap();
        let text = buffer_text(&terminal);
        assert!(text.contains("Past Sessions"), "got: {text}");
        assert!(text.contains("Test History Title"), "got: {text}");
        assert!(text.contains("hello world"), "got: {text}");
        assert!(text.contains("hi there!"), "got: {text}");
        assert!(text.contains("switch pane"), "got: {text}");
    }

    #[test]
    fn trace_badge_shapes() {
        use crate::config::Backend;
        assert_eq!(trace_badge(None, true, Backend::Local), "[● TRACE]");
        assert_eq!(trace_badge(None, true, Backend::Langfuse), "[◆ TRACE]");
        let stats = LaunchStats {
            turns: 3,
            total_tokens: Some(12_000),
            cost_usd: Some(0.42),
            running_tool: Some("Bash".into()),
        };
        assert_eq!(
            trace_badge(Some(&stats), false, Backend::Local),
            "[● 3t $0.42]"
        );
        assert_eq!(
            trace_badge(Some(&stats), true, Backend::Both),
            "[◈ 3t $0.42 ▸ Bash]"
        );
        let unpriced = LaunchStats {
            turns: 1,
            total_tokens: Some(12_000),
            cost_usd: None,
            running_tool: None,
        };
        assert_eq!(
            trace_badge(Some(&unpriced), true, Backend::Local),
            "[● 1t 12k tok]"
        );
    }

    #[test]
    fn trace_browser_renders_sessions_turns_and_observations() {
        use crate::tracing::pricing::PriceTable;
        use crate::tracing::store::model::{
            Level, ObservationRow, ObservationType, SessionRow, StoreOp, TraceRow, TraceStatus,
        };
        use crate::tracing::store::{OpenOptions, open_rw};
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let mut store = open_rw(
            &db,
            OpenOptions {
                prices: PriceTable::builtin(),
                run_id: "run-ui".into(),
                retention_days: 0,
                agent_mux_version: "test".into(),
            },
        )
        .unwrap();
        store
            .apply(&[
                StoreOp::Session(SessionRow {
                    key: "claude:s-ui".into(),
                    provider: "claude".into(),
                    session_id: "s-ui".into(),
                    user_id: None,
                    cwd: Some("/home/silvio/workspace/agent-mux".into()),
                    project_slug: Some("-home-silvio-workspace-agent-mux".into()),
                    transcript_path: None,
                    title: Some("fix the widget".into()),
                    seen_ns: 1_700_000_000_000_000_000,
                    extra: None,
                }),
                StoreOp::Trace(TraceRow {
                    id: "t1".into(),
                    session_key: "claude:s-ui".into(),
                    provider: "claude".into(),
                    session_id: "s-ui".into(),
                    launch_id: None,
                    ordinal: 1,
                    name: "Claude Code: fix the widget".into(),
                    status: TraceStatus::Closed,
                    start_ns: 1_700_000_000_000_000_000,
                    end_ns: Some(1_700_000_004_000_000_000),
                    input: Some("fix the widget".into()),
                    output: Some("fixed".into()),
                    thinking: None,
                    skills: None,
                    reported_duration_ms: None,
                    reported_message_count: None,
                    session_cost_usd: None,
                    timing_approx: false,
                    metadata: None,
                }),
                StoreOp::Observation(ObservationRow {
                    id: "o1".into(),
                    trace_id: "t1".into(),
                    parent_id: None,
                    obs_type: ObservationType::Tool,
                    name: "Bash".into(),
                    kind: None,
                    start_ns: 1_700_000_001_000_000_000,
                    end_ns: Some(1_700_000_002_000_000_000),
                    level: Level::Default,
                    status_message: None,
                    model: None,
                    input: Some("{\"command\":\"ls\"}".into()),
                    output: Some("Cargo.toml".into()),
                    thinking: None,
                    usage_raw: None,
                    usage: None,
                    tool_id: Some("tool-1".into()),
                    tool_name: Some("Bash".into()),
                    skill: None,
                    mcp_server: None,
                    path: None,
                    ts_approx: false,
                    metadata: serde_json::Map::new(),
                }),
            ])
            .unwrap();
        drop(store);
        let (tx, _rx) = tokio::sync::mpsc::channel(4);
        let mut app = App::new(Config::default_profiles(), None, tx);
        let mut browser = crate::app::TraceBrowserState::new(Some(&db), None);
        browser.all_projects = true;
        browser.reload_sessions();
        assert_eq!(browser.sessions.len(), 1, "{:?}", browser.error);
        assert_eq!(browser.turns.len(), 1);
        assert_eq!(browser.observations.len(), 1);
        app.mode = crate::app::Mode::TraceBrowser(Box::new(browser));
        let mut terminal = Terminal::new(TestBackend::new(180, 36)).unwrap();
        terminal.draw(|f| draw(f, &app, Instant::now())).unwrap();
        let text = buffer_text(&terminal);
        assert!(text.contains("Sessions (1)"), "got: {text}");
        assert!(text.contains("fix the widget"), "got: {text}");
        assert!(
            text.contains("cwd …/silvio/workspace/agent-mux"),
            "got: {text}"
        );
        assert!(text.contains("Bash"), "got: {text}");
        assert!(text.contains("[Tab] pane"), "got: {text}");
        // expanding shows the observation body
        if let crate::app::Mode::TraceBrowser(b) = &mut app.mode {
            b.focused = BrowserPane::Detail;
            b.toggle_expanded();
        }
        terminal.draw(|f| draw(f, &app, Instant::now())).unwrap();
        let text = buffer_text(&terminal);
        assert!(text.contains("Cargo.toml"), "got: {text}");
        // tracing off: the browser explains instead of panicking
        let off = crate::app::TraceBrowserState::new(None, None);
        assert!(off.error.as_deref().unwrap_or("").contains("off"));
    }

    #[test]
    fn tree_and_timeline_views_render_their_own_shapes() {
        use crate::app::DetailView;
        use crate::tracing::store::query::ObservationView;
        let view = |id: &str, name: &str, depth: usize, start_ns: i64, end_ns: Option<i64>| {
            ObservationView {
                id: id.into(),
                trace_id: "trace-1".into(),
                parent_id: None,
                depth,
                obs_type: if depth == 0 && id == "a" {
                    "agent".into()
                } else {
                    "tool".into()
                },
                name: name.into(),
                kind: None,
                start_ns,
                end_ns,
                level: "DEFAULT".into(),
                status_message: None,
                model: None,
                model_id: None,
                input: None,
                output: None,
                thinking: None,
                usage: None,
                input_tokens: None,
                output_tokens: None,
                cache_read_tokens: None,
                cache_write_tokens: None,
                reasoning_tokens: None,
                total_tokens: Some(1_000),
                total_cost_usd: Some(0.01),
                tool_id: None,
                tool_name: None,
                skill: None,
                mcp_server: None,
                path: None,
                metadata: "{}".into(),
            }
        };
        // real traces carry epoch nanos, and a running row is compared
        // against the clock: anchor the fixture just before now
        let now_ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as i64)
            .unwrap_or(0);
        let base = now_ns - 4_000_000_000;
        let (tx, _rx) = tokio::sync::mpsc::channel(4);
        let mut app = App::new(Config::default_profiles(), None, tx);
        let mut browser = crate::app::TraceBrowserState::new(None, None);
        browser.error = None;
        browser.turns = vec![crate::tracing::store::query::TraceStat {
            id: "trace-1".into(),
            session_key: "claude:s".into(),
            launch_id: None,
            ordinal: 1,
            name: "Claude Code: do it".into(),
            status: "open".into(),
            start_ns: base,
            // still open, so the running row below is measured against now
            end_ns: None,
            latency_ms: 4_000,
            input: None,
            output: None,
            thinking: None,
            skills: "[]".into(),
            reported_duration_ms: None,
            session_cost_usd: None,
            closed_by: None,
            observation_count: 4,
            generation_count: 0,
            tool_count: 3,
            error_count: 0,
            open_count: 0,
            input_tokens: None,
            output_tokens: None,
            cache_read_tokens: None,
            cache_write_tokens: None,
            total_tokens: Some(4_000),
            total_cost_usd: Some(0.04),
            unpriced_generations: 0,
            models: None,
            metadata: "{}".into(),
            retries: 0,
            declined: 0,
        }];
        browser.observations = vec![
            view("a", "agent: Explore", 0, base, Some(base + 2_000_000_000)),
            view("g", "Grep", 1, base + 200_000_000, Some(base + 900_000_000)),
            view(
                "r",
                "Read",
                1,
                base + 1_000_000_000,
                Some(base + 1_800_000_000),
            ),
            view("b", "Bash", 0, base + 2_100_000_000, None),
        ];
        browser.observations[1].parent_id = Some("a".into());
        browser.observations[2].parent_id = Some("a".into());
        browser.focused = BrowserPane::Detail;
        browser.detail_view = DetailView::Tree;
        app.mode = crate::app::Mode::TraceBrowser(Box::new(browser));
        let mut terminal = Terminal::new(TestBackend::new(180, 30)).unwrap();
        terminal.draw(|f| draw(f, &app, Instant::now())).unwrap();
        let text = buffer_text(&terminal);
        assert!(text.contains("tree"), "the title names the view: {text}");
        assert!(text.contains("├─"), "connectors: {text}");
        assert!(text.contains("└─"), "connectors: {text}");
        assert!(
            text.contains("▾"),
            "an expanded parent shows a fold mark: {text}"
        );
        assert!(text.contains("[v] view"), "footer: {text}");

        // folded: the children go away and the parent reports the subtree
        if let crate::app::Mode::TraceBrowser(b) = &mut app.mode {
            b.selected_observation = 0;
            b.toggle_collapsed();
        }
        terminal.draw(|f| draw(f, &app, Instant::now())).unwrap();
        let text = buffer_text(&terminal);
        assert!(text.contains("▸"), "a folded parent flips its mark: {text}");
        assert!(text.contains("+2"), "it says what it hid: {text}");
        assert!(!text.contains("Grep"), "children are hidden: {text}");

        // timeline: an axis, bars, and a cap on the running row
        if let crate::app::Mode::TraceBrowser(b) = &mut app.mode {
            b.collapsed.clear();
            b.detail_view = DetailView::Timeline;
        }
        terminal.draw(|f| draw(f, &app, Instant::now())).unwrap();
        let text = buffer_text(&terminal);
        assert!(
            text.contains("timeline"),
            "the title names the view: {text}"
        );
        assert!(text.contains("0ms"), "axis: {text}");
        assert!(text.contains("█"), "bars: {text}");
        assert!(text.contains("░"), "lead-in before a later bar: {text}");
        assert!(text.contains("▶"), "the running row is capped: {text}");
        assert!(text.contains("Grep"), "every row is listed: {text}");

        // loop: the numbers, with retries named and the time bar drawn
        if let crate::app::Mode::TraceBrowser(b) = &mut app.mode {
            b.detail_view = DetailView::Loop;
            // a second Grep with the same input is a retry
            let mut again = b.observations[1].clone();
            again.id = "g2".into();
            again.input = Some("fn draw".into());
            b.observations[1].input = Some("fn draw".into());
            b.observations.push(again);
        }
        terminal.draw(|f| draw(f, &app, Instant::now())).unwrap();
        let text = buffer_text(&terminal);
        assert!(text.contains("── calls ──"), "{text}");
        assert!(text.contains("tool calls"), "{text}");
        assert!(
            text.contains("Grep ×1"),
            "the retried tool is named: {text}"
        );
        assert!(
            text.contains("── time ──") && text.contains("idle"),
            "{text}"
        );
        assert!(text.contains("cache ratio"), "{text}");

        // summary: the turn's tokens by kind, its cost, and every tool
        // with how many times it ran
        if let crate::app::Mode::TraceBrowser(b) = &mut app.mode {
            b.detail_view = DetailView::Summary;
            b.observations[1].level = "ERROR".into();
            let t = &mut b.turns[0];
            t.input_tokens = Some(1_200);
            t.output_tokens = Some(340);
            t.cache_read_tokens = Some(52_000);
            t.cache_write_tokens = Some(2_500);
            t.total_tokens = Some(56_040);
            t.total_cost_usd = Some(0.42);
        }
        terminal.draw(|f| draw(f, &app, Instant::now())).unwrap();
        let text = buffer_text(&terminal);
        assert!(text.contains("summary"), "the title names the view: {text}");
        assert!(text.contains("── tokens"), "{text}");
        // each value sits right after its own label, two to a row
        for (label, value) in [
            ("input", "1.2k"),
            ("output", "340"),
            ("cache read", "52k"),
            ("cache write", "2.5k"),
            ("total", "56k"),
            ("cost", "$0.42"),
        ] {
            let row = text
                .lines()
                .find(|l| l.contains(&format!("  {label} ")))
                .unwrap_or_else(|| panic!("no {label} row: {text}"));
            let after = &row[row.find(&format!("  {label} ")).unwrap()..];
            let first = after[label.len() + 2..].split_whitespace().next();
            assert_eq!(first, Some(value), "{label} shows {value}: {row}");
        }
        assert!(text.contains("── tools"), "{text}");
        assert!(
            text.contains("5 calls · 4 distinct · 1 error"),
            "the call and error totals: {text}"
        );
        let grep = text
            .lines()
            .find(|l| l.contains("  Grep "))
            .unwrap_or_else(|| panic!("Grep is listed: {text}"));
        assert!(grep.contains("×2"), "Grep ran twice: {grep}");
        assert!(grep.contains("✗1"), "one Grep failed: {grep}");
        let read = text
            .lines()
            .find(|l| l.contains("  Read "))
            .unwrap_or_else(|| panic!("Read is listed: {text}"));
        assert!(!read.contains('✗'), "a clean tool shows no errors: {read}");
        assert!(
            text.contains("agent: Explore"),
            "the launch is a call: {text}"
        );
        let grep_at = text.find("  Grep ").unwrap();
        let bash_at = text.find("  Bash ").unwrap();
        assert!(grep_at < bash_at, "most called first: {text}");

        // one screen: a turn with more tools than rows keeps its totals
        // on screen and folds the tail into a last row, losing nothing
        if let crate::app::Mode::TraceBrowser(b) = &mut app.mode {
            for i in 0..20 {
                let mut o = view(
                    &format!("z{i}"),
                    &format!("Zz{i:02}"),
                    0,
                    base,
                    Some(base + 1),
                );
                if i == 19 {
                    o.level = "ERROR".into();
                }
                b.observations.push(o);
            }
        }
        let mut short = Terminal::new(TestBackend::new(180, 18)).unwrap();
        short.draw(|f| draw(f, &app, Instant::now())).unwrap();
        let text = buffer_text(&short);
        assert!(
            text.contains("25 calls · 24 distinct · 2 errors"),
            "the totals stay on screen: {text}"
        );
        for label in ["input", "cache read", "total", "cost"] {
            assert!(text.contains(&format!("  {label} ")), "{label}: {text}");
        }
        let names: Vec<String> = ["Grep", "Bash", "Read", "agent: Explore"]
            .iter()
            .map(|s| s.to_string())
            .chain((0..20).map(|i| format!("Zz{i:02}")))
            .collect();
        let visible: Vec<&String> = names
            .iter()
            .filter(|n| text.contains(&format!("  {n} ")))
            .collect();
        assert!(
            !text.contains("  agent: Explore "),
            "the least called tool is folded: {text}"
        );
        let fold = text
            .lines()
            .find(|l| l.contains("more"))
            .unwrap_or_else(|| panic!("a fold row: {text}"));
        let hidden = 24 - visible.len();
        let hidden_calls = 25
            - visible
                .iter()
                .map(|n| if *n == "Grep" { 2 } else { 1 })
                .sum::<usize>();
        assert!(
            fold.contains(&format!("+{hidden} more · {hidden_calls} calls · 1 error")),
            "the fold carries what it hides: {fold}"
        );
        let rows: Vec<&str> = text.lines().collect();
        let last = rows.iter().rposition(|l| l.contains("more")).unwrap();
        assert!(
            rows.get(last + 1)
                .is_some_and(|l| l.contains('└') || l.contains('─')),
            "the fold is the pane's last row, right above its border: {text}"
        );

        // a cramped terminal must not panic in any view
        let mut tiny = Terminal::new(TestBackend::new(40, 10)).unwrap();
        tiny.draw(|f| draw(f, &app, Instant::now())).unwrap();
        if let crate::app::Mode::TraceBrowser(b) = &mut app.mode {
            b.detail_view = DetailView::Loop;
        }
        tiny.draw(|f| draw(f, &app, Instant::now())).unwrap();
        if let crate::app::Mode::TraceBrowser(b) = &mut app.mode {
            b.detail_view = DetailView::Tree;
        }
        tiny.draw(|f| draw(f, &app, Instant::now())).unwrap();
    }
}
