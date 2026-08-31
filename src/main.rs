use agent_mux::app::App;
use agent_mux::events::AppEvent;
use agent_mux::{config, ui};
use anyhow::Result;
use crossterm::event::{DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use std::io::stdout;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

fn restore_terminal() {
    let _ = disable_raw_mode();
    let _ = crossterm::execute!(std::io::stdout(), DisableMouseCapture, LeaveAlternateScreen);
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
        if args.get(1).map(String::as_str) == Some("langfuse") {
            return match args.get(2).map(String::as_str) {
                Some("doctor") => agent_mux::langfuse::doctor::run(),
                _ => {
                    eprintln!("usage: agent-mux langfuse doctor");
                    Ok(())
                }
            };
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
    crossterm::execute!(stdout(), EnterAlternateScreen, EnableMouseCapture)?;
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

    // Langfuse runtime: only when [langfuse] is enabled and keys resolve.
    // A misconfigured section must never stop the TUI — it surfaces as one
    // status-bar line instead.
    let langfuse_resolved =
        config::resolve_langfuse(cfg.langfuse.as_ref(), &|k| std::env::var(k).ok());
    let langfuse_misconfigured =
        cfg.langfuse.as_ref().is_some_and(|lf| lf.enabled) && langfuse_resolved.is_none();
    let secret_in_cwd_file = langfuse_resolved.as_ref().is_some_and(|r| r.secret_from_file)
        && cfg.loaded_from.as_deref() == Some(std::path::Path::new("profiles.toml"));
    let shutdown_flush = Duration::from_millis(
        langfuse_resolved
            .as_ref()
            .map(|r| r.shutdown_flush_ms)
            .unwrap_or(0),
    );
    let langfuse_rt = langfuse_resolved
        .map(|resolved| agent_mux::langfuse::LangfuseRuntime::new(resolved, tx.clone()));

    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;
    let mut app = App::new(cfg.profiles, langfuse_rt, tx);
    if langfuse_misconfigured {
        app.error =
            Some("langfuse: enabled but keys don't resolve — tracing is off (see `agent-mux langfuse doctor`)".into());
    } else if secret_in_cwd_file {
        app.error = Some(
            "langfuse: secret_key found in ./profiles.toml (commit hazard) — prefer $LANGFUSE_SECRET_KEY".into(),
        );
    }
    let size = terminal.size()?;
    let (rows, cols) = ui::main_pane_inner(Rect::new(0, 0, size.width, size.height));
    app.set_pane_size(rows, cols);

    let mut draw_err = None;
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
    }

    // kill_all must run on every exit from the loop above -- including the
    // draw-error path -- so live PTY children are never orphaned when we quit.
    app.kill_all();
    // Bounded Langfuse flush AFTER kill_all: the main loop is gone, so the
    // runtime's shutdown watch is the pipelines' only exit signal; the
    // deadline (breaker-aware inside) caps quit latency.
    if let Some(rt) = app.take_langfuse() {
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
            let (r, c) = ui::main_pane_inner(Rect::new(0, 0, cols, rows));
            app.set_pane_size(r, c);
        }
        AppEvent::PtyOutput { id, bytes } => app.handle_pty_output(id, &bytes, Instant::now()),
        AppEvent::PtyExit { id } => app.handle_pty_exit(id),
        AppEvent::Mouse(m) => app.handle_mouse(m, Instant::now()),
        AppEvent::LangfuseStatus(message) => app.error = Some(message),
        AppEvent::Tick => {} // redraw after every batch covers badge refresh
    }
}
