//! Keys to terminal input bytes (xterm conventions): Ctrl chords, Meta as an
//! ESC prefix, cursor/function keys honouring application cursor mode, and
//! bracketed paste.
//!
//! SKELETON: owned by the term builder. The bodies are minimal (plain text only).

use super::Modes;
use crate::engine::keymap::Key;

/// The bytes `key` sends, or `None` if it sends nothing (e.g. Super chords,
/// which stay with the editor).
pub fn encode_key(key: &Key, _modes: Modes) -> Option<Vec<u8>> {
    if key.sup || key.ctrl || key.meta {
        return None;
    }
    key.insertable_text().map(|t| t.as_bytes().to_vec())
}

/// The bytes pasting `text` sends (bracketed when the program asked for it).
pub fn encode_paste(text: &str, modes: Modes) -> Vec<u8> {
    if modes.bracketed_paste {
        let mut v = b"\x1b[200~".to_vec();
        v.extend_from_slice(text.as_bytes());
        v.extend_from_slice(b"\x1b[201~");
        v
    } else {
        text.as_bytes().to_vec()
    }
}
