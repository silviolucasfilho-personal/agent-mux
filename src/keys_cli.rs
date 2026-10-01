//! `agent-mux keys`: what this terminal delivers for each key, so the chord
//! layer (`keymap::CHORDS`) can be probed per terminal instead of assumed.
//! Raw mode, the keyboard-enhancement flags agent-mux itself pushes, every
//! key event printed as it arrives; `Ctrl+C` ends it with a summary of the
//! chords that were seen. The support answer to "my keys do not work".

use anyhow::Result;
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use std::io::Write;

use crate::keymap::{CHORDS, Chord, chord};

pub fn run() -> Result<()> {
    let enhanced = matches!(
        crossterm::terminal::supports_keyboard_enhancement(),
        Ok(true)
    );
    println!("agent-mux keys — press keys; Ctrl+C ends it.");
    println!(
        "keyboard enhancement (kitty protocol): {}",
        if enhanced {
            "supported"
        } else {
            "not supported — Ctrl+Shift arrives as Ctrl, ⌘ never"
        }
    );
    println!();
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
        let mut out = std::io::stdout();
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
            write!(
                out,
                "{:<24} {:<32} {}\r\n",
                format!("{:?}", k.code),
                format!("{:?}", k.modifiers),
                c.map(|c| format!("chord {c:?}")).unwrap_or_default()
            )?;
            out.flush()?;
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
    println!();
    println!("chords delivered:");
    for (c, mac, other, bare, meaning) in CHORDS {
        let mark = if seen.contains(c) { "yes" } else { " - " };
        println!("  {mark}  {mac:<8} {other:<14} {bare:<10} {meaning}");
    }
    Ok(())
}
