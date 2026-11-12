//! Keys to terminal input bytes (xterm conventions): Ctrl chords, Meta as an
//! ESC prefix, cursor/function keys honouring application cursor mode, and
//! bracketed paste.

use super::Modes;
use crate::engine::keymap::Key;

const ESC: u8 = 0x1b;

/// The bytes `key` sends, or `None` if it sends nothing (e.g. Super chords,
/// which stay with the editor, and keys a terminal has no encoding for).
pub fn encode_key(key: &Key, modes: Modes) -> Option<Vec<u8>> {
    if key.sup {
        return None;
    }
    if let Some(seq) = special_key(key, modes) {
        return Some(seq);
    }
    let body = match key.name.as_str() {
        "RET" => vec![b'\r'],
        "TAB" if key.shift => return Some(b"\x1b[Z".to_vec()),
        "TAB" => vec![b'\t'],
        // C-DEL is ^H, as xterm sends for Ctrl-Backspace.
        "DEL" => vec![if key.ctrl { 0x08 } else { 0x7f }],
        "ESC" => vec![ESC],
        "SPC" if key.ctrl => vec![0],
        "SPC" => vec![b' '],
        name if name.chars().count() == 1 => char_key(key, name.chars().next()?)?,
        // An unnamed key that types text (input methods, dead keys).
        _ if !key.ctrl && !key.meta => key.text.as_ref().filter(|t| !t.is_empty())?.as_bytes().to_vec(),
        _ => return None,
    };
    Some(with_meta(key.meta, body))
}

/// `body` with an ESC in front if Meta is held.
fn with_meta(meta: bool, mut body: Vec<u8>) -> Vec<u8> {
    if meta {
        body.insert(0, ESC);
    }
    body
}

/// A key named by one character: Ctrl turns it into a control code;
/// otherwise it sends the text it types.
fn char_key(key: &Key, c: char) -> Option<Vec<u8>> {
    if key.ctrl {
        if let Some(code) = control_code(c) {
            return Some(vec![code]);
        }
        // xterm sends the plain character for Ctrl chords without a control code.
    }
    // With Ctrl or Meta the platform's text is unreliable (macOS Option
    // types `å` for M-a), so the key name decides; Shift capitalises it.
    let text = if key.ctrl || key.meta {
        None
    } else {
        key.text.as_deref().filter(|t| !t.is_empty() && !t.chars().any(char::is_control))
    };
    let s = match text {
        Some(t) => t.to_string(),
        None if key.shift => c.to_uppercase().collect(),
        None => c.to_string(),
    };
    Some(s.into_bytes())
}

/// The control code Ctrl+`c` sends, if it has one.
fn control_code(c: char) -> Option<u8> {
    Some(match c.to_ascii_lowercase() {
        c @ 'a'..='z' => c as u8 - b'a' + 1,
        '@' | '2' | ' ' => 0x00,
        '[' | '3' => 0x1b,
        '\\' | '4' => 0x1c,
        ']' | '5' => 0x1d,
        '^' | '6' | '~' => 0x1e,
        '`' => 0x00,
        '_' | '/' | '7' | '-' => 0x1f,
        '?' | '8' => 0x7f,
        _ => return None,
    })
}

/// xterm's modifier parameter: 1 + Shift + 2·Meta + 4·Ctrl (1 = none).
fn modifier_param(key: &Key) -> u8 {
    1 + key.shift as u8 + 2 * key.meta as u8 + 4 * key.ctrl as u8
}

/// Cursor, editing and function keys (`<up>`, `<prior>`, `<f5>`...).
fn special_key(key: &Key, modes: Modes) -> Option<Vec<u8>> {
    let name = key.name.strip_prefix('<')?.strip_suffix('>')?;
    let m = modifier_param(key);
    // Keys ending in a letter: CSI <letter>, SS3 <letter> in application
    // cursor mode (cursor keys) or always (F1–F4), CSI 1;m <letter> with modifiers.
    let letter = |final_byte: u8, ss3: bool| {
        if m > 1 {
            format!("\x1b[1;{m}{}", final_byte as char).into_bytes()
        } else if ss3 {
            vec![ESC, b'O', final_byte]
        } else {
            vec![ESC, b'[', final_byte]
        }
    };
    // Keys ending in `~`: CSI n ~, CSI n;m ~ with modifiers.
    let tilde = |n: u8| if m > 1 { format!("\x1b[{n};{m}~") } else { format!("\x1b[{n}~") }.into_bytes();
    let app = modes.app_cursor;
    Some(match name {
        "up" => letter(b'A', app),
        "down" => letter(b'B', app),
        "right" => letter(b'C', app),
        "left" => letter(b'D', app),
        "home" => letter(b'H', app),
        "end" => letter(b'F', app),
        "insert" => tilde(2),
        "delete" => tilde(3),
        "prior" => tilde(5),
        "next" => tilde(6),
        "f1" => letter(b'P', true),
        "f2" => letter(b'Q', true),
        "f3" => letter(b'R', true),
        "f4" => letter(b'S', true),
        "f5" => tilde(15),
        "f6" => tilde(17),
        "f7" => tilde(18),
        "f8" => tilde(19),
        "f9" => tilde(20),
        "f10" => tilde(21),
        "f11" => tilde(23),
        "f12" => tilde(24),
        _ => return None,
    })
}

/// The bytes pasting `text` sends (bracketed when the program asked for it).
/// Line breaks become `\r` (what Enter sends). Inside brackets, control
/// characters other than tab and line breaks are dropped (ESC among them),
/// so pasted text can't end the bracket early, however it nests the end
/// marker, and then run the rest as typed input.
pub fn encode_paste(text: &str, modes: Modes) -> Vec<u8> {
    let text = text.replace("\r\n", "\r").replace('\n', "\r");
    if modes.bracketed_paste {
        let text: String = text.chars().filter(|&c| matches!(c, '\t' | '\r') || !c.is_control()).collect();
        let mut v = b"\x1b[200~".to_vec();
        v.extend_from_slice(text.as_bytes());
        v.extend_from_slice(b"\x1b[201~");
        v
    } else {
        text.into_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enc(k: &str) -> Option<Vec<u8>> {
        encode_key(&Key::parse(k), Modes::default())
    }

    fn app(k: &str) -> Option<Vec<u8>> {
        encode_key(&Key::parse(k), Modes { app_cursor: true, ..Modes::default() })
    }

    fn bytes(s: &str) -> Option<Vec<u8>> {
        Some(s.as_bytes().to_vec())
    }

    #[test]
    fn plain_text_keys() {
        assert_eq!(enc("a"), bytes("a"));
        assert_eq!(enc("S-a"), bytes("A"));
        assert_eq!(enc("1"), bytes("1"));
        assert_eq!(enc("!"), bytes("!"));
        // Text the platform reports wins for plain keys (other layouts).
        let k = Key { name: "e".into(), text: Some("é".into()), ..Key::default() };
        assert_eq!(encode_key(&k, Modes::default()), bytes("é"));
        let k = Key { name: "é".into(), text: Some("é".into()), ..Key::default() };
        assert_eq!(encode_key(&k, Modes::default()), bytes("é"));
        // An unnamed key typing text (input method).
        let k = Key { name: "<compose>".into(), text: Some("日本".into()), ..Key::default() };
        assert_eq!(encode_key(&k, Modes::default()), bytes("日本"));
    }

    #[test]
    fn ctrl_letters() {
        for (i, c) in ('a'..='z').enumerate() {
            assert_eq!(enc(&format!("C-{c}")), Some(vec![i as u8 + 1]), "C-{c}");
            // Shift doesn't change a control code.
            assert_eq!(enc(&format!("C-S-{c}")), Some(vec![i as u8 + 1]), "C-S-{c}");
            assert_eq!(enc(&format!("C-{}", c.to_ascii_uppercase())), Some(vec![i as u8 + 1]));
        }
    }

    #[test]
    fn ctrl_symbols() {
        assert_eq!(enc("C-SPC"), Some(vec![0]));
        assert_eq!(enc("C-@"), Some(vec![0]));
        assert_eq!(enc("C-2"), Some(vec![0]));
        assert_eq!(enc("C-["), Some(vec![0x1b]));
        assert_eq!(enc("C-\\"), Some(vec![0x1c]));
        assert_eq!(enc("C-]"), Some(vec![0x1d]));
        assert_eq!(enc("C-^"), Some(vec![0x1e]));
        assert_eq!(enc("C-_"), Some(vec![0x1f]));
        assert_eq!(enc("C-/"), Some(vec![0x1f]));
        assert_eq!(enc("C-?"), Some(vec![0x7f]));
        // No control code: the plain character, like xterm.
        assert_eq!(enc("C-1"), bytes("1"));
        assert_eq!(enc("C-."), bytes("."));
    }

    #[test]
    fn meta_is_an_esc_prefix() {
        assert_eq!(enc("M-a"), bytes("\x1ba"));
        assert_eq!(enc("M-S-a"), bytes("\x1bA"));
        assert_eq!(enc("C-M-a"), Some(vec![0x1b, 0x01]));
        assert_eq!(enc("M-DEL"), Some(vec![0x1b, 0x7f]));
        assert_eq!(enc("M-RET"), bytes("\x1b\r"));
        assert_eq!(enc("M-SPC"), bytes("\x1b "));
        assert_eq!(enc("C-M-SPC"), Some(vec![0x1b, 0]));
        assert_eq!(enc("M-ESC"), bytes("\x1b\x1b"));
        assert_eq!(enc("M-."), bytes("\x1b."));
        // macOS Option text is ignored: the key name decides.
        let k = Key { meta: true, name: "a".into(), text: Some("å".into()), ..Key::default() };
        assert_eq!(encode_key(&k, Modes::default()), bytes("\x1ba"));
    }

    #[test]
    fn editing_keys() {
        assert_eq!(enc("RET"), bytes("\r"));
        assert_eq!(enc("C-RET"), bytes("\r"));
        assert_eq!(enc("TAB"), bytes("\t"));
        assert_eq!(enc("S-TAB"), bytes("\x1b[Z"));
        assert_eq!(enc("DEL"), Some(vec![0x7f]));
        assert_eq!(enc("C-DEL"), Some(vec![0x08]));
        assert_eq!(enc("ESC"), Some(vec![0x1b]));
        assert_eq!(enc("SPC"), bytes(" "));
        assert_eq!(enc("S-SPC"), bytes(" "));
        assert_eq!(enc("<insert>"), bytes("\x1b[2~"));
        assert_eq!(enc("<delete>"), bytes("\x1b[3~"));
        assert_eq!(enc("<prior>"), bytes("\x1b[5~"));
        assert_eq!(enc("<next>"), bytes("\x1b[6~"));
        assert_eq!(enc("C-<delete>"), bytes("\x1b[3;5~"));
        assert_eq!(enc("S-<prior>"), bytes("\x1b[5;2~"));
    }

    #[test]
    fn cursor_keys_and_app_cursor_mode() {
        for (name, c) in [("up", 'A'), ("down", 'B'), ("right", 'C'), ("left", 'D'), ("home", 'H'), ("end", 'F')] {
            let k = format!("<{name}>");
            assert_eq!(enc(&k), bytes(&format!("\x1b[{c}")), "{k}");
            assert_eq!(app(&k), bytes(&format!("\x1bO{c}")), "{k} app");
            // With modifiers the CSI form is used in both modes.
            for (mods, m) in [("S-", 2), ("M-", 3), ("M-S-", 4), ("C-", 5), ("C-S-", 6), ("C-M-", 7), ("C-M-S-", 8)] {
                let want = bytes(&format!("\x1b[1;{m}{c}"));
                assert_eq!(enc(&format!("{mods}{k}")), want, "{mods}{k}");
                assert_eq!(app(&format!("{mods}{k}")), want, "{mods}{k} app");
            }
        }
    }

    #[test]
    fn function_keys() {
        let want = [
            "\x1bOP", "\x1bOQ", "\x1bOR", "\x1bOS", "\x1b[15~", "\x1b[17~", "\x1b[18~", "\x1b[19~", "\x1b[20~", "\x1b[21~",
            "\x1b[23~", "\x1b[24~",
        ];
        for (i, w) in want.iter().enumerate() {
            let k = format!("<f{}>", i + 1);
            assert_eq!(enc(&k), bytes(w), "{k}");
            // Application cursor mode doesn't change function keys.
            assert_eq!(app(&k), bytes(w), "{k} app");
        }
        assert_eq!(enc("S-<f1>"), bytes("\x1b[1;2P"));
        assert_eq!(enc("C-<f4>"), bytes("\x1b[1;5S"));
        assert_eq!(enc("C-<f5>"), bytes("\x1b[15;5~"));
        assert_eq!(enc("M-<f12>"), bytes("\x1b[24;3~"));
        assert_eq!(enc("<f13>"), None);
    }

    #[test]
    fn super_and_unknown_keys_send_nothing() {
        assert_eq!(enc("s-v"), None);
        assert_eq!(enc("s-<up>"), None);
        assert_eq!(enc("C-s-a"), None);
        assert_eq!(enc("<menu>"), None);
        assert_eq!(enc("C-<menu>"), None);
        assert_eq!(enc("<up"), None);
        let k = Key { name: "<compose>".into(), text: None, ..Key::default() };
        assert_eq!(encode_key(&k, Modes::default()), None);
    }

    #[test]
    fn every_parsed_key_encodes_without_panicking() {
        let names = ["a", "Z", "0", "-", "SPC", "RET", "TAB", "DEL", "ESC", "<up>", "<f1>", "<f12>", "<home>", "<prior>", "é", ""];
        for mods in ["", "C-", "M-", "S-", "s-", "C-M-", "C-M-S-", "C-M-S-s-"] {
            for n in names {
                for modes in [Modes::default(), Modes { app_cursor: true, bracketed_paste: true, ..Modes::default() }] {
                    let _ = encode_key(&Key::parse(&format!("{mods}{n}")), modes);
                }
            }
        }
    }

    #[test]
    fn paste() {
        let plain = Modes::default();
        let bracketed = Modes { bracketed_paste: true, ..Modes::default() };
        assert_eq!(encode_paste("ls -l", plain), b"ls -l");
        assert_eq!(encode_paste("a\nb\r\nc", plain), b"a\rb\rc");
        assert_eq!(encode_paste("ls", bracketed), b"\x1b[200~ls\x1b[201~");
        assert_eq!(encode_paste("", bracketed), b"\x1b[200~\x1b[201~");
        assert_eq!(encode_paste("x\x1b[201~rm -rf\n", bracketed), b"\x1b[200~x[201~rm -rf\r\x1b[201~");
        // A nested end marker can't be rebuilt by removing an inner one.
        let nested = encode_paste("a\x1b[20\x1b[201~1~rm -rf ~\n", bracketed);
        assert_eq!(nested, b"\x1b[200~a[20[201~1~rm -rf ~\r\x1b[201~");
        let inner = &nested[6..nested.len() - 6];
        assert!(!inner.contains(&0x1b), "no ESC inside the brackets");
        // Other controls go too (C1 included); tabs and line breaks stay.
        assert_eq!(encode_paste("a\tb\x03\x7f\u{9b}201~c\r\n", bracketed), b"\x1b[200~a\tb201~c\r\x1b[201~");
        assert_eq!(encode_paste("日本\n", plain), "日本\r".as_bytes());
    }
}
