use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Encode a key press as the bytes a terminal would send. `None` = no
/// encoding (key is dropped). Caller must filter to KeyEventKind::Press.
pub fn encode_key(key: &KeyEvent) -> Option<Vec<u8>> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let mut buf: Vec<u8> = Vec::new();
    if alt {
        buf.push(0x1b);
    }
    match key.code {
        KeyCode::Char(c) => {
            if ctrl {
                let lower = c.to_ascii_lowercase();
                if lower.is_ascii_lowercase() {
                    buf.push(lower as u8 - b'a' + 1);
                } else {
                    return None;
                }
            } else {
                let mut tmp = [0u8; 4];
                buf.extend_from_slice(c.encode_utf8(&mut tmp).as_bytes());
            }
        }
        KeyCode::Enter => buf.push(b'\r'),
        KeyCode::Backspace => buf.push(0x7f),
        KeyCode::Tab => buf.push(b'\t'),
        KeyCode::BackTab => buf.extend_from_slice(b"\x1b[Z"),
        KeyCode::Esc => buf.push(0x1b),
        KeyCode::Up => buf.extend_from_slice(b"\x1b[A"),
        KeyCode::Down => buf.extend_from_slice(b"\x1b[B"),
        KeyCode::Right => buf.extend_from_slice(b"\x1b[C"),
        KeyCode::Left => buf.extend_from_slice(b"\x1b[D"),
        KeyCode::Home => buf.extend_from_slice(b"\x1b[H"),
        KeyCode::End => buf.extend_from_slice(b"\x1b[F"),
        KeyCode::PageUp => buf.extend_from_slice(b"\x1b[5~"),
        KeyCode::PageDown => buf.extend_from_slice(b"\x1b[6~"),
        KeyCode::Delete => buf.extend_from_slice(b"\x1b[3~"),
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
        assert_eq!(encode_key(&key(KeyCode::Char('é'))), Some("é".as_bytes().to_vec()));
    }

    #[test]
    fn ctrl_letters_map_to_control_bytes() {
        assert_eq!(encode_key(&ctrl('c')), Some(vec![0x03]));
        assert_eq!(encode_key(&ctrl('q')), Some(vec![0x11]));
        assert_eq!(encode_key(&ctrl('A')), Some(vec![0x01])); // case-insensitive
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
        assert_eq!(encode_key(&key(KeyCode::PageDown)), Some(b"\x1b[6~".to_vec()));
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
