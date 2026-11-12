//! Emacs line editing in single-line text fields (picker queries, the
//! settings search and value fields). Like Emacs' minibuffer, which inherits
//! the global keymap, keys a field's own keymap leaves unbound run these
//! global editing commands on the field.
//!
//! A field's cursor is kept as `tail`, the number of bytes after it. Code
//! that rewrites the start of a field (a file picker entering a directory,
//! `//` restarting the path) then leaves the cursor where it was relative to
//! the text after it; code that replaces the whole text sets `tail` to 0.

/// What a field command did.
#[derive(Clone, Debug, PartialEq)]
pub enum FieldEdit {
    /// Only the cursor moved.
    Moved,
    /// The text changed.
    Changed,
    /// The text changed and this was cut (it belongs on the kill ring).
    Killed(String),
}

/// The global commands a field understands.
pub const FIELD_COMMANDS: &[&str] = &[
    "forward-char",
    "backward-char",
    "forward-word",
    "backward-word",
    "move-beginning-of-line",
    "move-end-of-line",
    "delete-char",
    "delete-backward-char",
    "kill-word",
    "backward-kill-word",
    "kill-line",
    "yank",
    "transpose-chars",
];

/// The cursor's byte offset in `text`.
pub fn point(text: &str, tail: usize) -> usize {
    let mut p = text.len().saturating_sub(tail);
    while !text.is_char_boundary(p) {
        p -= 1;
    }
    p
}

fn set_point(text: &str, tail: &mut usize, p: usize) {
    *tail = text.len() - p;
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric()
}

fn next_char(text: &str, p: usize) -> usize {
    text[p..].chars().next().map_or(p, |c| p + c.len_utf8())
}

fn prev_char(text: &str, p: usize) -> usize {
    text[..p].chars().next_back().map_or(p, |c| p - c.len_utf8())
}

/// The end of the word at or after `p` (`M-f`).
fn word_end(text: &str, p: usize) -> usize {
    let rest = &text[p..];
    let start = rest.find(is_word).unwrap_or(rest.len());
    let len = rest[start..].find(|c| !is_word(c)).unwrap_or(rest.len() - start);
    p + start + len
}

/// The start of the word at or before `p` (`M-b`).
fn word_start(text: &str, p: usize) -> usize {
    let before = text[..p].trim_end_matches(|c| !is_word(c));
    before.trim_end_matches(is_word).len()
}

/// Type `s` at the cursor.
pub fn insert(text: &mut String, tail: usize, s: &str) {
    let p = point(text, tail);
    text.insert_str(p, s);
}

/// Delete the character before the cursor; false at the start.
pub fn delete_backward(text: &mut String, tail: usize) -> bool {
    let p = point(text, tail);
    let q = prev_char(text, p);
    text.replace_range(q..p, "");
    q != p
}

/// Delete the word before the cursor, returning it.
pub fn kill_word_backward(text: &mut String, tail: usize) -> String {
    let p = point(text, tail);
    let q = word_start(text, p);
    text.drain(q..p).collect()
}

/// Run global command `cmd` on the field, `yank` being the text `C-y`
/// inserts. `None` if the command isn't a field command (or has nothing to yank).
pub fn apply(text: &mut String, tail: &mut usize, cmd: &str, yank: Option<&str>) -> Option<FieldEdit> {
    let p = point(text, *tail);
    let killed = |text: &mut String, r: std::ops::Range<usize>| {
        let s: String = text.drain(r).collect();
        if s.is_empty() { FieldEdit::Moved } else { FieldEdit::Killed(s) }
    };
    let edit = match cmd {
        "forward-char" => {
            set_point(text, tail, next_char(text, p));
            FieldEdit::Moved
        }
        "backward-char" => {
            set_point(text, tail, prev_char(text, p));
            FieldEdit::Moved
        }
        "forward-word" => {
            set_point(text, tail, word_end(text, p));
            FieldEdit::Moved
        }
        "backward-word" => {
            set_point(text, tail, word_start(text, p));
            FieldEdit::Moved
        }
        "move-beginning-of-line" => {
            *tail = text.len();
            FieldEdit::Moved
        }
        "move-end-of-line" => {
            *tail = 0;
            FieldEdit::Moved
        }
        "delete-char" => {
            let q = next_char(text, p);
            text.replace_range(p..q, "");
            *tail = text.len() - p;
            if q == p { FieldEdit::Moved } else { FieldEdit::Changed }
        }
        "delete-backward-char" => {
            if delete_backward(text, *tail) {
                FieldEdit::Changed
            } else {
                FieldEdit::Moved
            }
        }
        "kill-word" => {
            let e = killed(text, p..word_end(text, p));
            *tail = text.len() - p;
            e
        }
        "backward-kill-word" => {
            let s = kill_word_backward(text, *tail);
            if s.is_empty() { FieldEdit::Moved } else { FieldEdit::Killed(s) }
        }
        "kill-line" => {
            let e = killed(text, p..text.len());
            *tail = 0;
            e
        }
        "yank" => {
            // A field is one line: yank only the first line of what was killed.
            let s = yank?.lines().next().unwrap_or("");
            text.insert_str(p, s);
            FieldEdit::Changed
        }
        // Swap the characters around the cursor (the last two at the end) and move past them.
        "transpose-chars" => {
            let (a, b) = if p == text.len() {
                (prev_char(text, prev_char(text, p)), p)
            } else {
                (prev_char(text, p), next_char(text, p))
            };
            let mid = if p == text.len() { prev_char(text, p) } else { p };
            if a == mid || mid == b {
                return Some(FieldEdit::Moved);
            }
            let swapped = format!("{}{}", &text[mid..b], &text[a..mid]);
            text.replace_range(a..b, &swapped);
            *tail = text.len() - b;
            FieldEdit::Changed
        }
        _ => return None,
    };
    Some(edit)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Run `cmds` on `s` (`|` marks the cursor), returning the field with its cursor.
    fn run(s: &str, cmds: &[&str]) -> String {
        let p = s.find('|').unwrap();
        let mut text = s.replace('|', "");
        let mut tail = text.len() - p;
        for c in cmds {
            apply(&mut text, &mut tail, c, Some("yanked\nsecond line")).unwrap();
        }
        let p = point(&text, tail);
        format!("{}|{}", &text[..p], &text[p..])
    }

    #[test]
    fn motion() {
        assert_eq!(run("hello wor|ld", &["move-beginning-of-line"]), "|hello world");
        assert_eq!(run("he|llo world", &["move-end-of-line"]), "hello world|");
        assert_eq!(run("he|llo", &["forward-char", "forward-char"]), "hell|o");
        assert_eq!(run("h|é", &["forward-char"]), "hé|");
        assert_eq!(run("|é", &["backward-char"]), "|é");
        assert_eq!(run("|foo-bar baz", &["forward-word", "forward-word"]), "foo-bar| baz");
        assert_eq!(run("foo-bar baz|", &["backward-word", "backward-word"]), "foo-|bar baz");
    }

    #[test]
    fn deletion_and_killing() {
        assert_eq!(run("ab|cd", &["delete-char"]), "ab|d");
        assert_eq!(run("abcd|", &["delete-char"]), "abcd|");
        assert_eq!(run("ab|cd", &["delete-backward-char"]), "a|cd");
        assert_eq!(run("~/src/em|ax", &["backward-kill-word"]), "~/src/|ax");
        assert_eq!(run("a |b-c d", &["kill-word"]), "a |-c d");
        assert_eq!(run("ab|cd", &["kill-line"]), "ab|");
        let (mut text, mut tail) = ("abcd".to_string(), 2);
        assert_eq!(apply(&mut text, &mut tail, "kill-line", None), Some(FieldEdit::Killed("cd".into())));
        assert_eq!(apply(&mut text, &mut tail, "kill-line", None), Some(FieldEdit::Moved));
    }

    #[test]
    fn yank_and_transpose() {
        assert_eq!(run("a|b", &["yank"]), "ayanked|b");
        assert_eq!(run("ab|cd", &["transpose-chars"]), "acb|d");
        assert_eq!(run("abcd|", &["transpose-chars"]), "abdc|");
        assert_eq!(run("|ab", &["transpose-chars"]), "|ab");
        let (mut text, mut tail) = (String::new(), 0);
        assert_eq!(apply(&mut text, &mut tail, "yank", None), None);
        assert_eq!(apply(&mut text, &mut tail, "next-line", None), None);
    }

    #[test]
    fn typing_at_the_cursor() {
        let mut text = "acd".to_string();
        insert(&mut text, 2, "b");
        assert_eq!(text, "abcd");
        assert!(delete_backward(&mut text, 3));
        assert_eq!(text, "bcd");
        assert!(!delete_backward(&mut text, 3));
    }
}
