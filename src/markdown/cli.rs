//! `agent-mux md <file>`: the Markdown viewer and editor on its own, full
//! screen, without the multiplexer (no sessions restored, no store
//! touched). `--edit` opens the source; a file that does not exist yet
//! opens empty in the editor and `Ctrl+S` creates it. `--print` writes the
//! rendering to stdout as plain text instead (`--width`, else `$COLUMNS`,
//! else the terminal's, else 80).

use std::io::{Write, stdout};
use std::path::PathBuf;

use anyhow::{Result, bail};
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture, Event,
    KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use crate::app::markdown_view::{MarkdownView, Outcome, Pane};

const USAGE: &str = "agent-mux md <file> [--edit]       view or edit a Markdown file
agent-mux md <file> --print [--width N]   print it rendered, as plain text";

pub fn run(args: &[String]) -> Result<()> {
    let mut path: Option<PathBuf> = None;
    let mut edit = false;
    let mut print = false;
    let mut width: Option<usize> = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-h" | "--help" => {
                println!("{USAGE}");
                return Ok(());
            }
            "--edit" | "-e" => edit = true,
            "--print" | "-p" => print = true,
            "--width" | "-w" => {
                let n = it
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--width needs a number"))?;
                width = Some(
                    n.parse()
                        .map_err(|_| anyhow::anyhow!("--width {n}: not a number"))?,
                );
            }
            other if other.starts_with('-') => bail!("unknown option {other}\n{USAGE}"),
            other if path.is_none() => path = Some(PathBuf::from(other)),
            other => bail!("one file at a time ({other})\n{USAGE}"),
        }
    }
    let Some(path) = path else {
        bail!("{USAGE}");
    };
    if path.is_dir() {
        bail!("{} is a directory", path.display());
    }
    if print {
        let text = std::fs::read_to_string(&path)
            .map_err(|e| anyhow::anyhow!("could not read {}: {e}", path.display()))?;
        let width = width
            .or_else(|| std::env::var("COLUMNS").ok().and_then(|c| c.parse().ok()))
            .or_else(|| crossterm::terminal::size().ok().map(|(w, _)| w as usize))
            .unwrap_or(80);
        let mut out = stdout().lock();
        for line in super::render(&text, width).plain() {
            writeln!(out, "{line}")?;
        }
        return Ok(());
    }
    let mut view = MarkdownView::open(&path).map_err(|e| anyhow::anyhow!(e))?;
    if edit {
        view.pane = Pane::Edit;
    }
    let editor = crate::config::load().ok().and_then(|c| c.editor);
    interactive(&mut view, editor.as_deref())
}

/// Raw mode and the alternate screen, undone on drop (and on panic).
struct Screen {
    enhanced: bool,
}

impl Screen {
    fn enter() -> Result<Screen> {
        enable_raw_mode()?;
        let mut screen = Screen { enhanced: false };
        crossterm::execute!(
            stdout(),
            EnterAlternateScreen,
            EnableMouseCapture,
            EnableBracketedPaste
        )?;
        if matches!(
            crossterm::terminal::supports_keyboard_enhancement(),
            Ok(true)
        ) && crossterm::execute!(
            stdout(),
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        )
        .is_ok()
        {
            screen.enhanced = true;
        }
        Ok(screen)
    }

    fn leave(&mut self) {
        if self.enhanced {
            let _ = crossterm::execute!(stdout(), PopKeyboardEnhancementFlags);
            self.enhanced = false;
        }
        let _ = crossterm::execute!(
            stdout(),
            DisableBracketedPaste,
            DisableMouseCapture,
            LeaveAlternateScreen
        );
        let _ = disable_raw_mode();
    }
}

impl Drop for Screen {
    fn drop(&mut self) {
        self.leave();
    }
}

fn interactive(view: &mut MarkdownView, editor: Option<&str>) -> Result<()> {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = crossterm::execute!(
            stdout(),
            DisableBracketedPaste,
            DisableMouseCapture,
            LeaveAlternateScreen
        );
        let _ = disable_raw_mode();
        default_hook(info);
    }));
    let mut screen = Screen::enter()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;
    loop {
        terminal.draw(|f| crate::ui::markdown::draw(f, f.area(), view))?;
        let outcome = match crossterm::event::read()? {
            Event::Key(k) => match crate::keys::normalize_event(k) {
                Some(k) => view.handle_key(&k),
                None => Outcome::None,
            },
            Event::Mouse(m) => view.handle_mouse(&m),
            Event::Paste(s) => {
                view.paste(&s);
                Outcome::None
            }
            _ => Outcome::None,
        };
        match outcome {
            Outcome::None => {}
            Outcome::Close => break,
            Outcome::OpenUrl(url) => {
                if let Err(e) = crate::links::open(&url) {
                    view.notice = Some(format!("could not open {url}: {e}"));
                }
            }
            Outcome::OpenEditor(path) => {
                let command = crate::assets::editor_command(editor);
                screen.leave();
                let status = command.split_first().map(|(program, args)| {
                    std::process::Command::new(program)
                        .args(args)
                        .arg(&path)
                        .status()
                });
                screen = Screen::enter()?;
                terminal.clear()?;
                match status {
                    Some(Ok(s)) if s.success() => view.reload(),
                    Some(Ok(s)) => {
                        view.reload();
                        view.notice = Some(format!("editor exited with {s}"));
                    }
                    Some(Err(e)) => view.notice = Some(format!("editor: {e}")),
                    None => view.notice = Some("no editor configured".into()),
                }
            }
        }
    }
    drop(screen);
    Ok(())
}
