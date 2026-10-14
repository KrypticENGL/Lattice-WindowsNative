//! Key events to VT input bytes.

use crate::event::{Key, Modifiers};

/// Encodes a key press for the shell, or `None` if the key sends nothing.
/// Printable text arrives separately as `Event::Char`.
pub fn encode_key(key: Key, mods: Modifiers, app_cursor: bool) -> Option<Vec<u8>> {
    let modifier = 1 + u8::from(mods.shift) + 2 * u8::from(mods.alt) + 4 * u8::from(mods.ctrl);
    let cursor = |c: char| -> Vec<u8> {
        if modifier > 1 {
            format!("\x1b[1;{modifier}{c}").into_bytes()
        } else if app_cursor {
            format!("\x1bO{c}").into_bytes()
        } else {
            format!("\x1b[{c}").into_bytes()
        }
    };
    let tilde = |n: u8| -> Vec<u8> {
        if modifier > 1 {
            format!("\x1b[{n};{modifier}~").into_bytes()
        } else {
            format!("\x1b[{n}~").into_bytes()
        }
    };
    let bytes = match key {
        Key::Enter => b"\r".to_vec(),
        Key::Backspace if mods.ctrl => b"\x08".to_vec(),
        Key::Backspace => b"\x7f".to_vec(),
        Key::Tab if mods.shift => b"\x1b[Z".to_vec(),
        Key::Tab => b"\t".to_vec(),
        Key::Escape => b"\x1b".to_vec(),
        Key::Up => cursor('A'),
        Key::Down => cursor('B'),
        Key::Right => cursor('C'),
        Key::Left => cursor('D'),
        Key::Home => cursor('H'),
        Key::End => cursor('F'),
        Key::Insert => tilde(2),
        Key::Delete => tilde(3),
        Key::PageUp => tilde(5),
        Key::PageDown => tilde(6),
        Key::F(n @ 1..=4) => format!("\x1bO{}", (b'P' + n - 1) as char).into_bytes(),
        Key::F(n @ 5..=12) => tilde([15, 17, 18, 19, 20, 21, 23, 24][usize::from(n - 5)]),
        Key::Letter(c) if mods.ctrl => vec![(c as u8) & 0x1f],
        Key::Space if mods.ctrl => vec![0],
        _ => return None,
    };
    Some(bytes)
}

/// Prepares pasted text: newlines become CR, optionally bracketed.
pub fn encode_paste(text: &str, bracketed: bool) -> Vec<u8> {
    let body = text.replace("\r\n", "\r").replace('\n', "\r");
    if bracketed {
        format!("\x1b[200~{body}\x1b[201~").into_bytes()
    } else {
        body.into_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enc(key: Key, mods: Modifiers) -> Vec<u8> {
        encode_key(key, mods, false).unwrap()
    }

    #[test]
    fn basic_keys() {
        assert_eq!(enc(Key::Enter, Modifiers::NONE), b"\r");
        assert_eq!(enc(Key::Backspace, Modifiers::NONE), b"\x7f");
        assert_eq!(enc(Key::Up, Modifiers::NONE), b"\x1b[A");
        assert_eq!(enc(Key::Delete, Modifiers::NONE), b"\x1b[3~");
        assert_eq!(enc(Key::F(1), Modifiers::NONE), b"\x1bOP");
        assert_eq!(enc(Key::F(5), Modifiers::NONE), b"\x1b[15~");
    }

    #[test]
    fn modifiers_and_ctrl_letters() {
        assert_eq!(enc(Key::Letter('C'), Modifiers::CTRL), [3]);
        assert_eq!(enc(Key::Right, Modifiers::CTRL), b"\x1b[1;5C");
        assert_eq!(enc(Key::Tab, Modifiers::SHIFT), b"\x1b[Z");
        assert!(encode_key(Key::Letter('A'), Modifiers::NONE, false).is_none());
    }

    #[test]
    fn application_cursor_mode() {
        assert_eq!(
            encode_key(Key::Left, Modifiers::NONE, true).unwrap(),
            b"\x1bOD"
        );
    }

    #[test]
    fn paste_normalises_newlines() {
        assert_eq!(encode_paste("a\r\nb\nc", false), b"a\rb\rc");
        assert_eq!(encode_paste("x", true), b"\x1b[200~x\x1b[201~");
    }
}
