use agent_mux::app::App;
use agent_mux::events::AppEvent;
use agent_mux::{config, ui};
use anyhow::Result;
use crossterm::cursor::SetCursorStyle;
use crossterm::event::{DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use std::io::stdout;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

static KEYBOARD_ENHANCED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn restore_terminal() {
    let _ = disable_raw_mode();
    if KEYBOARD_ENHANCED.swap(false, std::sync::atomic::Ordering::SeqCst) {
        let _ = crossterm::execute!(
            std::io::stdout(),
            crossterm::event::PopKeyboardEnhancementFlags
        );
    }
    let _ = crossterm::execute!(
        std::io::stdout(),
        SetCursorStyle::DefaultUserShape,
        DisableMouseCapture,
        LeaveAlternateScreen
    );
}

/// Restores the terminal on normal exit and on unwind.
struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore_terminal();
    }
}

/// Cap per iteration so a flooding PTY can't starve drawing forever.
const MAX_EVENTS_PER_FRAME: usize = 256;

#[tokio::main]
async fn main() -> Result<()> {
    // one-shot subcommands run before any terminal setup
    {
        let args: Vec<String> = std::env::args().collect();
        match args.get(1).map(String::as_str) {
            Some("trace") => return agent_mux::tracing::cli::run(&args[2..]),
            Some("run") => return agent_mux::tracing::experiments::run_cli(&args[2..]).await,
            Some("langfuse") => {
                eprintln!(
                    "`agent-mux langfuse …` was replaced by `agent-mux trace …` (local SQLite store).\n\
                     Try `agent-mux trace doctor`."
                );
                return Ok(());
            }
            Some("agent") => {
                if let Err(err) = agent_mux::agent::cli::handle_agent_cli(&args[2..]) {
                    eprintln!("{err}");
                    std::process::exit(1);
                }
                return Ok(());
            }
            _ => {}
        }
    }

    let cfg = config::load()?;

    enable_raw_mode()?;
    // Constructed immediately after enable_raw_mode() succeeds, before
    // EnterAlternateScreen: if EnterAlternateScreen fails, raw mode must
    // still have a restorer on unwind/return. The guard's restore path
    // (disable_raw_mode + LeaveAlternateScreen) is harmless to run even if
    // the alternate screen was never entered.
    let _guard = TerminalGuard;
    crossterm::execute!(
        stdout(),
        EnterAlternateScreen,
        EnableMouseCapture,
        SetCursorStyle::DefaultUserShape
    )?;
    if matches!(
        crossterm::terminal::supports_keyboard_enhancement(),
        Ok(true)
    ) {
        if crossterm::execute!(
            stdout(),
            crossterm::event::PushKeyboardEnhancementFlags(
                crossterm::event::KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
            )
        )
        .is_ok()
        {
            KEYBOARD_ENHANCED.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_terminal();
        default_hook(info);
    }));

    let (tx, mut rx) = mpsc::channel::<AppEvent>(1024);

    // keyboard + resize -> channel (blocking crossterm reads on own thread)
    {
        let tx = tx.clone();
        std::thread::spawn(move || {
            loop {
                match crossterm::event::read() {
                    Ok(Event::Key(k)) if k.kind == KeyEventKind::Press => {
                        if tx.blocking_send(AppEvent::Key(k)).is_err() {
                            break;
                        }
                    }
                    Ok(Event::Resize(cols, rows)) => {
                        if tx.blocking_send(AppEvent::Resize(cols, rows)).is_err() {
                            break;
                        }
                    }
                    Ok(Event::Mouse(m)) => {
                        if tx.blocking_send(AppEvent::Mouse(m)).is_err() {
                            break;
                        }
                    }
                    Ok(_) => {}
                    Err(_) => break,
                }
            }
        });
    }

    // tick -> channel (keeps Working/Idle badges fresh with no other events)
    {
        let tx = tx.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_millis(250));
            loop {
                interval.tick().await;
                if tx.send(AppEvent::Tick).await.is_err() {
                    break;
                }
            }
        });
    }

    // Trace runtime: on by default (local SQLite store, no keys). A store
    // that cannot be opened must never stop the TUI — it surfaces as one
    // status-bar line instead.
    let tracing_resolved =
        config::resolve_tracing(cfg.tracing.as_ref(), &|k| std::env::var(k).ok());
    let shutdown_flush = Duration::from_millis(
        tracing_resolved
            .as_ref()
            .map(|r| r.shutdown_flush_ms.max(1500))
            .unwrap_or(0),
    );
    let mut startup_notices: Vec<String> = Vec::new();
    if cfg.legacy_langfuse_section {
        startup_notices.push(
            "tracing: [langfuse] is now [tracing]; its host/keys serve as [tracing.langfuse] credentials"
                .into(),
        );
    }
    if let Some(r) = &tracing_resolved
        && config::is_wsl_drive_mount(&r.db_path)
    {
        startup_notices.push(
            "tracing: db_path is on a Windows drive mount (/mnt/*) — prefer a path under $HOME"
                .into(),
        );
    }
    let trace_rt = match tracing_resolved {
        Some(resolved) => match agent_mux::tracing::TraceRuntime::new(resolved, tx.clone()) {
            Ok(rt) => Some(rt),
            Err(e) => {
                startup_notices.insert(
                    0,
                    format!("tracing: store unavailable — {e} (see `agent-mux trace doctor`)"),
                );
                None
            }
        },
        None => None,
    };

    let raw_args: Vec<String> = std::env::args().collect();
    let hide_sidebar = raw_args
        .iter()
        .any(|a| a == "--hide-sidebar" || a == "--full-screen" || a == "-b")
        || cfg.hide_sidebar;

    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;
    let mut app = App::new(cfg.profiles, trace_rt, tx);
    if hide_sidebar {
        app.sidebar_hidden = true;
    }
    if let Some(first) = startup_notices.into_iter().next() {
        app.notice = Some(agent_mux::app::Notice::warn(first));
    }
    let size = terminal.size()?;
    app.set_terminal_size(size.height, size.width);
    app.restore_saved_sessions();

    let mut draw_err = None;
    let mut current_cursor_style = crossterm::cursor::SetCursorStyle::DefaultUserShape;
    loop {
        let Some(first) = rx.recv().await else {
            break;
        };
        handle_event(&mut app, first);
        for _ in 0..MAX_EVENTS_PER_FRAME {
            match rx.try_recv() {
                Ok(event) => handle_event(&mut app, event),
                Err(_) => break,
            }
        }
        if app.should_quit {
            break;
        }
        if let Err(e) = terminal.draw(|f| ui::draw(f, &app, Instant::now())) {
            draw_err = Some(e);
            break;
        }
        let needed_cursor_style = app.active_cursor_style();
        if needed_cursor_style != current_cursor_style {
            current_cursor_style = needed_cursor_style;
            let _ = crossterm::execute!(stdout(), current_cursor_style);
        }
    }

    // Save active sessions before kill_all, so they can be restored on restart.
    let _ = app.save_active_sessions();
    // kill_all must run on every exit from the loop above -- including the
    // draw-error path -- so live PTY children are never orphaned when we quit.
    app.kill_all();
    app.cleanup_live_snapshot();
    // Bounded store flush AFTER kill_all: the main loop is gone, so the
    // runtime's shutdown watch is the pipelines' only exit signal; the
    // deadline caps quit latency.
    if let Some(rt) = app.take_tracing() {
        rt.shutdown(shutdown_flush).await;
    }
    if let Some(e) = draw_err {
        return Err(e.into());
    }
    Ok(())
}

fn handle_event(app: &mut App, event: AppEvent) {
    match event {
        AppEvent::Key(k) => app.handle_key(&k, Instant::now()),
        AppEvent::Resize(cols, rows) => {
            app.set_terminal_size(rows, cols);
        }
        AppEvent::PtyOutput { id, bytes } => app.handle_pty_output(id, &bytes, Instant::now()),
        AppEvent::PtyExit { id } => app.handle_pty_exit(id),
        AppEvent::Mouse(m) => app.handle_mouse(m, Instant::now()),
        AppEvent::TraceStatus(message) => app.notice = Some(agent_mux::app::Notice::warn(message)),
        AppEvent::TraceStats { launch_id, stats } => app.handle_trace_stats(&launch_id, stats),
        AppEvent::AnalysisUpdated { revision, result } => {
            app.handle_analysis_updated(revision, result);
        }
        AppEvent::Tick => app.on_tick(Instant::now()),
    }
}
