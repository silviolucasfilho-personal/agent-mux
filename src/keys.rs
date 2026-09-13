use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Encode a key press as the bytes a terminal would send. `None` = no
/// encoding (key is dropped). Caller must filter to KeyEventKind::Press.
pub fn encode_key(key: &KeyEvent) -> Option<Vec<u8>> {
    encode_key_with_mode(key, false)
}

/// Like `encode_key`, but respects application cursor mode (`DECCKM`)
/// for unmodified arrow keys and Home/End.
pub fn encode_key_with_mode(key: &KeyEvent, app_cursor: bool) -> Option<Vec<u8>> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    let mut buf: Vec<u8> = Vec::new();
    match key.code {
        KeyCode::Char(c) => {
            // Meta/Option characters use the traditional ESC prefix. Arrow
            // keys use xterm's CSI modifier parameter below instead.
            if alt {
                buf.push(0x1b);
            }
            if ctrl {
                let lower = c.to_ascii_lowercase();
                if lower.is_ascii_lowercase() {
                    buf.push(lower as u8 - b'a' + 1);
                } else {
                    match c {
                        ' ' | '@' => buf.push(0x00),
                        '[' => buf.push(0x1b),
                        '\\' => buf.push(0x1c),
                        ']' => buf.push(0x1d),
                        '^' => buf.push(0x1e),
                        '_' => buf.push(0x1f),
                        '?' => buf.push(0x7f),
                        _ => return None,
                    }
                }
            } else {
                let mut tmp = [0u8; 4];
                buf.extend_from_slice(c.encode_utf8(&mut tmp).as_bytes());
            }
        }
        KeyCode::Enter => {
            if ctrl || shift {
                // In interactive CLIs (Claude Code, Antigravity, Codex) and terminals,
                // Ctrl+Enter and Shift+Enter insert a newline (\n) rather than
                // submitting the prompt (\r).
                if alt {
                    buf.extend_from_slice(b"\x1b\n");
                } else {
                    buf.push(b'\n');
                }
            } else if alt {
                // Option+Enter / Alt+Enter sends ESC Enter for multiline editing.
                buf.extend_from_slice(b"\x1b\r");
            } else {
                buf.push(b'\r');
            }
        }
        KeyCode::Backspace => {
            if alt {
                // Option+Backspace: backward-kill-word
                buf.extend_from_slice(b"\x1b\x7f");
            } else if ctrl {
                // Ctrl+Backspace: Ctrl+W backward-kill-word
                buf.push(0x17);
            } else {
                buf.push(0x7f);
            }
        }
        KeyCode::Tab => buf.push(b'\t'),
        KeyCode::BackTab => buf.extend_from_slice(b"\x1b[Z"),
        KeyCode::Esc => buf.push(0x1b),
        KeyCode::Left => {
            // Word navigation: macOS users (Ghostty, iTerm2) expect Option+Left
            // or Ctrl+Left to move backward by word. On macOS zsh, bash, Claude Code,
            // Codex, and readline, \x1bb is backward-word.
            if (alt || ctrl) && !shift {
                buf.extend_from_slice(b"\x1bb");
            } else {
                let modifier = 1 + u8::from(shift) + 2 * u8::from(alt) + 4 * u8::from(ctrl);
                if modifier == 1 {
                    if app_cursor {
                        buf.extend_from_slice(b"\x1bOD");
                    } else {
                        buf.extend_from_slice(b"\x1b[D");
                    }
                } else {
                    buf.extend_from_slice(format!("\x1b[1;{modifier}D").as_bytes());
                }
            }
        }
        KeyCode::Right => {
            // Word navigation: Option+Right or Ctrl+Right moves forward by word (\x1bf).
            if (alt || ctrl) && !shift {
                buf.extend_from_slice(b"\x1bf");
            } else {
                let modifier = 1 + u8::from(shift) + 2 * u8::from(alt) + 4 * u8::from(ctrl);
                if modifier == 1 {
                    if app_cursor {
                        buf.extend_from_slice(b"\x1bOC");
                    } else {
                        buf.extend_from_slice(b"\x1b[C");
                    }
                } else {
                    buf.extend_from_slice(format!("\x1b[1;{modifier}C").as_bytes());
                }
            }
        }
        KeyCode::Up => {
            let modifier = 1 + u8::from(shift) + 2 * u8::from(alt) + 4 * u8::from(ctrl);
            if modifier == 1 {
                if app_cursor {
                    buf.extend_from_slice(b"\x1bOA");
                } else {
                    buf.extend_from_slice(b"\x1b[A");
                }
            } else {
                buf.extend_from_slice(format!("\x1b[1;{modifier}A").as_bytes());
            }
        }
        KeyCode::Down => {
            let modifier = 1 + u8::from(shift) + 2 * u8::from(alt) + 4 * u8::from(ctrl);
            if modifier == 1 {
                if app_cursor {
                    buf.extend_from_slice(b"\x1bOB");
                } else {
                    buf.extend_from_slice(b"\x1b[B");
                }
            } else {
                buf.extend_from_slice(format!("\x1b[1;{modifier}B").as_bytes());
            }
        }
        KeyCode::Home => {
            if app_cursor && !ctrl && !alt && !shift {
                buf.extend_from_slice(b"\x1bOH");
            } else {
                buf.extend_from_slice(b"\x1b[H");
            }
        }
        KeyCode::End => {
            if app_cursor && !ctrl && !alt && !shift {
                buf.extend_from_slice(b"\x1bOF");
            } else {
                buf.extend_from_slice(b"\x1b[F");
            }
        }
        KeyCode::PageUp => buf.extend_from_slice(b"\x1b[5~"),
        KeyCode::PageDown => buf.extend_from_slice(b"\x1b[6~"),
        KeyCode::Delete => {
            if alt {
                // Option+Delete: kill-word forward
                buf.extend_from_slice(b"\x1bd");
            } else {
                buf.extend_from_slice(b"\x1b[3~");
            }
        }
        KeyCode::Insert => buf.extend_from_slice(b"\x1b[2~"),
        KeyCode::F(n) => match n {
            1 => buf.extend_from_slice(b"\x1bOP"),
            2 => buf.extend_from_slice(b"\x1bOQ"),
            3 => buf.extend_from_slice(b"\x1bOR"),
            4 => buf.extend_from_slice(b"\x1bOS"),
            5 => buf.extend_from_slice(b"\x1b[15~"),
            6 => buf.extend_from_slice(b"\x1b[17~"),
            7 => buf.extend_from_slice(b"\x1b[18~"),
            8 => buf.extend_from_slice(b"\x1b[19~"),
            9 => buf.extend_from_slice(b"\x1b[20~"),
            10 => buf.extend_from_slice(b"\x1b[21~"),
            11 => buf.extend_from_slice(b"\x1b[23~"),
            12 => buf.extend_from_slice(b"\x1b[24~"),
            _ => return None,
        },
        _ => return None,
    }
    Some(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    #[test]
    fn plain_chars_pass_through_utf8() {
        assert_eq!(encode_key(&key(KeyCode::Char('a'))), Some(b"a".to_vec()));
        assert_eq!(
            encode_key(&key(KeyCode::Char('é'))),
            Some("é".as_bytes().to_vec())
        );
    }

    #[test]
    fn ctrl_letters_map_to_control_bytes() {
        assert_eq!(encode_key(&ctrl('c')), Some(vec![0x03]));
        assert_eq!(encode_key(&ctrl('q')), Some(vec![0x11]));
        assert_eq!(encode_key(&ctrl('A')), Some(vec![0x01])); // case-insensitive
    }

    #[test]
    fn ctrl_punctuation() {
        assert_eq!(
            encode_key(&KeyEvent::new(KeyCode::Char('['), KeyModifiers::CONTROL)),
            Some(vec![0x1b])
        );
        assert_eq!(
            encode_key(&KeyEvent::new(KeyCode::Char(' '), KeyModifiers::CONTROL)),
            Some(vec![0x00])
        );
    }

    #[test]
    fn special_keys() {
        assert_eq!(encode_key(&key(KeyCode::Enter)), Some(b"\r".to_vec()));
        assert_eq!(encode_key(&key(KeyCode::Backspace)), Some(vec![0x7f]));
        assert_eq!(encode_key(&key(KeyCode::Tab)), Some(b"\t".to_vec()));
        assert_eq!(encode_key(&key(KeyCode::BackTab)), Some(b"\x1b[Z".to_vec()));
        assert_eq!(encode_key(&key(KeyCode::Esc)), Some(vec![0x1b]));
        assert_eq!(encode_key(&key(KeyCode::Up)), Some(b"\x1b[A".to_vec()));
        assert_eq!(encode_key(&key(KeyCode::Down)), Some(b"\x1b[B".to_vec()));
        assert_eq!(encode_key(&key(KeyCode::Right)), Some(b"\x1b[C".to_vec()));
        assert_eq!(encode_key(&key(KeyCode::Left)), Some(b"\x1b[D".to_vec()));
        assert_eq!(encode_key(&key(KeyCode::Delete)), Some(b"\x1b[3~".to_vec()));
        assert_eq!(encode_key(&key(KeyCode::PageUp)), Some(b"\x1b[5~".to_vec()));
        assert_eq!(
            encode_key(&key(KeyCode::PageDown)),
            Some(b"\x1b[6~".to_vec())
        );
    }

    #[test]
    fn enter_with_modifiers() {
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        let ctrl_enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL);
        let shift_enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT);
        let alt_enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT);
        let ctrl_alt_enter =
            KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL | KeyModifiers::ALT);

        assert_eq!(encode_key(&enter), Some(b"\r".to_vec()));
        assert_eq!(encode_key(&ctrl_enter), Some(b"\n".to_vec()));
        assert_eq!(encode_key(&shift_enter), Some(b"\n".to_vec()));
        assert_eq!(encode_key(&alt_enter), Some(b"\x1b\r".to_vec()));
        assert_eq!(encode_key(&ctrl_alt_enter), Some(b"\x1b\n".to_vec()));
    }

    #[test]
    fn word_navigation_and_deletion() {
        let opt_left = KeyEvent::new(KeyCode::Left, KeyModifiers::ALT);
        let opt_right = KeyEvent::new(KeyCode::Right, KeyModifiers::ALT);
        let ctrl_left = KeyEvent::new(KeyCode::Left, KeyModifiers::CONTROL);
        let ctrl_right = KeyEvent::new(KeyCode::Right, KeyModifiers::CONTROL);
        let opt_backspace = KeyEvent::new(KeyCode::Backspace, KeyModifiers::ALT);
        let ctrl_backspace = KeyEvent::new(KeyCode::Backspace, KeyModifiers::CONTROL);
        let opt_delete = KeyEvent::new(KeyCode::Delete, KeyModifiers::ALT);

        assert_eq!(encode_key(&opt_left), Some(b"\x1bb".to_vec()));
        assert_eq!(encode_key(&opt_right), Some(b"\x1bf".to_vec()));
        assert_eq!(encode_key(&ctrl_left), Some(b"\x1bb".to_vec()));
        assert_eq!(encode_key(&ctrl_right), Some(b"\x1bf".to_vec()));
        assert_eq!(encode_key(&opt_backspace), Some(b"\x1b\x7f".to_vec()));
        assert_eq!(encode_key(&ctrl_backspace), Some(vec![0x17]));
        assert_eq!(encode_key(&opt_delete), Some(b"\x1bd".to_vec()));

        // Shift modifier preserves xterm sequence for selection
        let shift_opt_left =
            KeyEvent::new(KeyCode::Left, KeyModifiers::ALT | KeyModifiers::SHIFT);
        assert_eq!(encode_key(&shift_opt_left), Some(b"\x1b[1;4D".to_vec()));
    }

    #[test]
    fn application_cursor_mode() {
        let up = KeyEvent::new(KeyCode::Up, KeyModifiers::NONE);
        let down = KeyEvent::new(KeyCode::Down, KeyModifiers::NONE);
        let right = KeyEvent::new(KeyCode::Right, KeyModifiers::NONE);
        let left = KeyEvent::new(KeyCode::Left, KeyModifiers::NONE);
        let home = KeyEvent::new(KeyCode::Home, KeyModifiers::NONE);
        let end = KeyEvent::new(KeyCode::End, KeyModifiers::NONE);

        assert_eq!(encode_key_with_mode(&up, true), Some(b"\x1bOA".to_vec()));
        assert_eq!(encode_key_with_mode(&down, true), Some(b"\x1bOB".to_vec()));
        assert_eq!(encode_key_with_mode(&right, true), Some(b"\x1bOC".to_vec()));
        assert_eq!(encode_key_with_mode(&left, true), Some(b"\x1bOD".to_vec()));
        assert_eq!(encode_key_with_mode(&home, true), Some(b"\x1bOH".to_vec()));
        assert_eq!(encode_key_with_mode(&end, true), Some(b"\x1bOF".to_vec()));

        assert_eq!(encode_key_with_mode(&up, false), Some(b"\x1b[A".to_vec()));
    }

    #[test]
    fn alt_char_gets_esc_prefix() {
        let k = KeyEvent::new(KeyCode::Char('b'), KeyModifiers::ALT);
        assert_eq!(encode_key(&k), Some(b"\x1bb".to_vec()));
    }

    #[test]
    fn function_keys() {
        assert_eq!(encode_key(&key(KeyCode::F(1))), Some(b"\x1bOP".to_vec()));
        assert_eq!(encode_key(&key(KeyCode::F(5))), Some(b"\x1b[15~".to_vec()));
        assert_eq!(encode_key(&key(KeyCode::F(12))), Some(b"\x1b[24~".to_vec()));
    }

    #[test]
    fn unencodable_keys_return_none() {
        assert_eq!(encode_key(&key(KeyCode::CapsLock)), None);
    }
}
