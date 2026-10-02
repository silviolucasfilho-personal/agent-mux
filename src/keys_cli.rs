//! `agent-mux keys`: what this terminal delivers for each key, so the chord
//! layer (`keymap::CHORDS`) can be probed per terminal instead of assumed.
//! Raw mode, the keyboard-enhancement flags agent-mux itself pushes, every
//! key event printed as it arrives; `Ctrl+C` ends it with a summary of the
//! chords that were seen. The support answer to "my keys do not work".
//! `--log <file>` keeps a copy of every line; stdout must stay the
//! terminal, since the protocol query is answered on it.

use anyhow::Result;
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use std::io::Write;

use crate::keymap::{CHORDS, Chord, chord};

pub fn run(args: &[String]) -> Result<()> {
    let mut log: Option<std::fs::File> = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--log" => {
                let path = it
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--log needs a file"))?;
                log = Some(std::fs::File::create(path)?);
            }
            "-h" | "--help" => {
                println!(
                    "agent-mux keys [--log <file>]: print what this terminal delivers for each key"
                );
                return Ok(());
            }
            other => anyhow::bail!("unknown argument {other}"),
        }
    }
    let mut say = |line: &str| {
        print!("{line}\r\n");
        let _ = std::io::stdout().flush();
        if let Some(f) = log.as_mut() {
            let _ = writeln!(f, "{line}");
        }
    };
    let enhanced = matches!(
        crossterm::terminal::supports_keyboard_enhancement(),
        Ok(true)
    );
    say("agent-mux keys — press keys; Ctrl+C ends it.");
    say(&format!(
        "keyboard enhancement (kitty protocol): {}",
        if enhanced {
            "supported"
        } else {
            "not supported — Ctrl+Shift arrives as Ctrl, ⌘ never"
        }
    ));
    say("");
    crossterm::terminal::enable_raw_mode()?;
    let mut pushed = false;
    if enhanced
        && crossterm::execute!(
            std::io::stdout(),
            crossterm::event::PushKeyboardEnhancementFlags(
                crossterm::event::KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
            )
        )
        .is_ok()
    {
        pushed = true;
    }
    let mut seen: Vec<Chord> = Vec::new();
    let result = (|| -> Result<()> {
        loop {
            let Event::Key(k) = crossterm::event::read()? else {
                continue;
            };
            if k.kind != KeyEventKind::Press {
                continue;
            }
            if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('c') {
                break;
            }
            let c = chord(&k);
            say(&format!(
                "{:<24} {:<32} {}",
                format!("{:?}", k.code),
                format!("{:?}", k.modifiers),
                c.map(|c| format!("chord {c:?}")).unwrap_or_default()
            ));
            if let Some(c) = c
                && !seen.contains(&c)
            {
                seen.push(c);
            }
        }
        Ok(())
    })();
    if pushed {
        let _ = crossterm::execute!(
            std::io::stdout(),
            crossterm::event::PopKeyboardEnhancementFlags
        );
    }
    crossterm::terminal::disable_raw_mode()?;
    result?;
    say("");
    say("chords delivered:");
    for (c, mac, other, bare, meaning) in CHORDS {
        let mark = if seen.contains(c) { "yes" } else { " - " };
        say(&format!(
            "  {mark}  {mac:<8} {other:<14} {bare:<10} {meaning}"
        ));
    }
    Ok(())
}
