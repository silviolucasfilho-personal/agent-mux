use agent_mux::app::App;
use agent_mux::events::AppEvent;
use agent_mux::{config, ui};
use anyhow::Result;
use crossterm::event::{Event, KeyEventKind};
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
    let _ = crossterm::execute!(std::io::stdout(), LeaveAlternateScreen);
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
    let cfg = config::load()?;

    enable_raw_mode()?;
    crossterm::execute!(stdout(), EnterAlternateScreen)?;
    let _guard = TerminalGuard;
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

    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;
    let mut app = App::new(cfg.profiles, tx);
    let size = terminal.size()?;
    let (rows, cols) = ui::main_pane_inner(Rect::new(0, 0, size.width, size.height));
    app.set_pane_size(rows, cols);

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
        terminal.draw(|f| ui::draw(f, &app, Instant::now()))?;
    }

    app.kill_all();
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
        AppEvent::Tick => {} // redraw after every batch covers badge refresh
    }
}
