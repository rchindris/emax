//! Pure text-motion primitives over a rope: words, lines, sentences,
//! paragraphs and balanced expressions (sexps). All positions are char indices.

use ropey::Rope;

pub fn is_word(c: char) -> bool {
    c.is_alphanumeric()
}

/// Constituents of a symbol for sexp motion (identifiers, lisp-style names).
pub fn is_symbol(c: char) -> bool {
    c.is_alphanumeric()
        || matches!(
            c,
            '_' | '-'
                | '$'
                | '!'
                | '?'
                | '*'
                | '+'
                | '<'
                | '>'
                | '='
                | '/'
                | ':'
                | '&'
                | '%'
                | '~'
                | '^'
                | '@'
                | '#'
                | '.'
        )
}

pub fn is_open(c: char) -> bool {
    matches!(c, '(' | '[' | '{')
}

pub fn is_close(c: char) -> bool {
    matches!(c, ')' | ']' | '}')
}

fn matching(c: char) -> char {
    match c {
        '(' => ')',
        '[' => ']',
        '{' => '}',
        ')' => '(',
        ']' => '[',
        '}' => '{',
        _ => c,
    }
}

pub fn is_hspace(c: char) -> bool {
    c == ' ' || c == '\t'
}

pub fn char_at(r: &Rope, i: usize) -> Option<char> {
    (i < r.len_chars()).then(|| r.char(i))
}

pub fn char_before(r: &Rope, i: usize) -> Option<char> {
    (i > 0 && i <= r.len_chars()).then(|| r.char(i - 1))
}

// ------------------------------------------------------------------ lines

pub fn line_of(r: &Rope, p: usize) -> usize {
    r.char_to_line(p)
}

/// Number of chars in `line`, excluding the line terminator.
pub fn line_len(r: &Rope, line: usize) -> usize {
    let l = r.line(line);
    let mut n = l.len_chars();
    if n > 0 && l.char(n - 1) == '\n' {
        n -= 1;
        if n > 0 && l.char(n - 1) == '\r' {
            n -= 1;
        }
    }
    n
}

pub fn line_start(r: &Rope, p: usize) -> usize {
    r.line_to_char(r.char_to_line(p))
}

pub fn line_end(r: &Rope, p: usize) -> usize {
    let line = r.char_to_line(p);
    r.line_to_char(line) + line_len(r, line)
}

pub fn column(r: &Rope, p: usize) -> usize {
    p - line_start(r, p)
}

/// Position on `line` at `col`, clamped to the line's length.
pub fn pos_at(r: &Rope, line: usize, col: usize) -> usize {
    let line = line.min(r.len_lines().saturating_sub(1));
    r.line_to_char(line) + col.min(line_len(r, line))
}

pub fn is_blank_line(r: &Rope, line: usize) -> bool {
    r.line(line).chars().all(char::is_whitespace)
}

/// First non-blank position on the line containing `p`.
pub fn indentation_end(r: &Rope, p: usize) -> usize {
    let mut i = line_start(r, p);
    let end = line_end(r, p);
    while i < end && is_hspace(r.char(i)) {
        i += 1;
    }
    i
}

pub fn indentation_string(r: &Rope, p: usize) -> String {
    r.slice(line_start(r, p)..indentation_end(r, p)).to_string()
}

// ------------------------------------------------------------------ words

pub fn forward_word(r: &Rope, mut p: usize) -> usize {
    let len = r.len_chars();
    while p < len && !is_word(r.char(p)) {
        p += 1;
    }
    while p < len && is_word(r.char(p)) {
        p += 1;
    }
    p
}

pub fn backward_word(r: &Rope, mut p: usize) -> usize {
    while p > 0 && !is_word(r.char(p - 1)) {
        p -= 1;
    }
    while p > 0 && is_word(r.char(p - 1)) {
        p -= 1;
    }
    p
}

pub fn forward_word_n(r: &Rope, p: usize, n: i64) -> usize {
    let mut p = p;
    if n >= 0 {
        for _ in 0..n {
            p = forward_word(r, p);
        }
    } else {
        for _ in 0..-n {
            p = backward_word(r, p);
        }
    }
    p
}

// ------------------------------------------------------------- paragraphs

/// Moves to the start of the separator line after the current/next paragraph.
pub fn forward_paragraph(r: &Rope, p: usize) -> usize {
    let n = r.len_lines();
    let mut line = r.char_to_line(p);
    // If we're mid-line on a blank line we still treat it as separator.
    while line < n && is_blank_line(r, line) {
        line += 1;
    }
    while line < n && !is_blank_line(r, line) {
        line += 1;
    }
    if line >= n { r.len_chars() } else { r.line_to_char(line) }
}

/// Moves to the start of the separator line before the current/previous paragraph.
pub fn backward_paragraph(r: &Rope, p: usize) -> usize {
    let mut line = r.char_to_line(p) as isize;
    while line >= 0 && is_blank_line(r, line as usize) {
        line -= 1;
    }
    while line >= 0 && !is_blank_line(r, line as usize) {
        line -= 1;
    }
    if line < 0 { 0 } else { r.line_to_char(line as usize) }
}

/// Bounds of the paragraph around (or after) `p`: (text start, text end).
pub fn paragraph_bounds(r: &Rope, p: usize) -> (usize, usize) {
    let end = forward_paragraph(r, p);
    let mut start = backward_paragraph(r, end);
    // skip the separator line(s) at the start
    while start < end && is_blank_line(r, r.char_to_line(start)) && r.char_to_line(start) < r.char_to_line(end) {
        start = r.line_to_char(r.char_to_line(start) + 1);
    }
    let mut e = end;
    while e > start && char_before(r, e).is_some_and(char::is_whitespace) {
        e -= 1;
    }
    (start, e)
}

// --------------------------------------------------------------- sentences

fn sentence_end_after(r: &Rope, i: usize) -> Option<usize> {
    // `i` is a '.', '?' or '!': skip closing punctuation, require whitespace/EOF.
    let len = r.len_chars();
    let mut j = i + 1;
    while j < len && matches!(r.char(j), ')' | ']' | '"' | '\'' | '”' | '’') {
        j += 1;
    }
    (j >= len || r.char(j).is_whitespace()).then_some(j)
}

fn skip_ws(r: &Rope, mut p: usize, limit: usize) -> usize {
    while p < limit && r.char(p).is_whitespace() {
        p += 1;
    }
    p
}

pub fn forward_sentence(r: &Rope, p: usize) -> usize {
    let len = r.len_chars();
    let para_end = forward_paragraph(r, p);
    let mut i = skip_ws(r, p, para_end);
    while i < para_end {
        if matches!(r.char(i), '.' | '?' | '!')
            && let Some(e) = sentence_end_after(r, i)
            && e > p
        {
            return e;
        }
        i += 1;
    }
    // No terminator: the sentence ends where the paragraph's text ends.
    let mut e = para_end;
    while e > p && char_before(r, e).is_some_and(char::is_whitespace) {
        e -= 1;
    }
    if e > p {
        e
    } else if para_end < len && para_end > p {
        forward_sentence(r, para_end)
    } else {
        len
    }
}

pub fn backward_sentence(r: &Rope, p: usize) -> usize {
    let ps = backward_paragraph(r, p);
    let mut s = skip_ws(r, ps, p);
    let mut best = None;
    while s < p {
        best = Some(s);
        let e = forward_sentence(r, s);
        if e <= s {
            break;
        }
        s = skip_ws(r, e, r.len_chars());
    }
    match best {
        Some(b) => b,
        None if ps < p => backward_sentence(r, ps).min(ps),
        None => ps,
    }
}

// -------------------------------------------------------------------- sexps

pub type SexpResult = Result<usize, &'static str>;

fn is_sexp_start(c: char) -> bool {
    is_symbol(c) || is_open(c) || is_close(c) || c == '"'
}

fn skip_string_forward(r: &Rope, open: usize) -> SexpResult {
    let len = r.len_chars();
    let mut i = open + 1;
    while i < len {
        match r.char(i) {
            '\\' => i += 2,
            '"' => return Ok(i + 1),
            _ => i += 1,
        }
    }
    Err("Unbalanced string")
}

fn skip_string_backward(r: &Rope, close: usize) -> SexpResult {
    // `close` is the index of the closing quote.
    let mut i = close;
    while i > 0 {
        i -= 1;
        if r.char(i) == '"' && !(i > 0 && r.char(i - 1) == '\\') {
            return Ok(i);
        }
    }
    Err("Unbalanced string")
}

pub fn forward_sexp(r: &Rope, p: usize) -> SexpResult {
    let len = r.len_chars();
    let mut i = p;
    while i < len && !is_sexp_start(r.char(i)) {
        i += 1;
    }
    if i >= len {
        return Ok(len);
    }
    let c = r.char(i);
    if is_close(c) {
        return Err("Containing expression ends prematurely");
    }
    if c == '"' {
        return skip_string_forward(r, i);
    }
    if is_open(c) {
        let mut stack = vec![c];
        let mut j = i + 1;
        while j < len {
            let d = r.char(j);
            if d == '"' {
                j = skip_string_forward(r, j)?;
                continue;
            }
            if is_open(d) {
                stack.push(d);
            } else if is_close(d) {
                if stack.pop().map(matching) != Some(d) {
                    return Err("Unbalanced parentheses");
                }
                if stack.is_empty() {
                    return Ok(j + 1);
                }
            }
            j += 1;
        }
        return Err("Unbalanced parentheses");
    }
    while i < len && is_symbol(r.char(i)) {
        i += 1;
    }
    Ok(i)
}

pub fn backward_sexp(r: &Rope, p: usize) -> SexpResult {
    let mut i = p;
    while i > 0 && !is_sexp_start(r.char(i - 1)) {
        i -= 1;
    }
    if i == 0 {
        return Ok(0);
    }
    let c = r.char(i - 1);
    if is_open(c) {
        return Err("Containing expression ends prematurely");
    }
    if c == '"' {
        return skip_string_backward(r, i - 1);
    }
    if is_close(c) {
        let mut stack = vec![c];
        let mut j = i - 1;
        while j > 0 {
            j -= 1;
            let d = r.char(j);
            if d == '"' {
                j = skip_string_backward(r, j)?;
                continue;
            }
            if is_close(d) {
                stack.push(d);
            } else if is_open(d) {
                if stack.pop().map(matching) != Some(d) {
                    return Err("Unbalanced parentheses");
                }
                if stack.is_empty() {
                    return Ok(j);
                }
            }
        }
        return Err("Unbalanced parentheses");
    }
    while i > 0 && is_symbol(r.char(i - 1)) {
        i -= 1;
    }
    Ok(i)
}

/// Position of the open bracket enclosing `p`.
pub fn backward_up_list(r: &Rope, p: usize) -> SexpResult {
    let mut depth = 0usize;
    let mut i = p;
    while i > 0 {
        i -= 1;
        let c = r.char(i);
        if c == '"' {
            if let Ok(open) = skip_string_backward(r, i) {
                i = open;
            }
        } else if is_close(c) {
            depth += 1;
        } else if is_open(c) {
            if depth == 0 {
                return Ok(i);
            }
            depth -= 1;
        }
    }
    Err("At top level")
}

/// Position just after the close bracket enclosing `p`.
pub fn up_list(r: &Rope, p: usize) -> SexpResult {
    let len = r.len_chars();
    let mut depth = 0usize;
    let mut i = p;
    while i < len {
        let c = r.char(i);
        if c == '"' {
            i = skip_string_forward(r, i)?;
            continue;
        }
        if is_open(c) {
            depth += 1;
        } else if is_close(c) {
            if depth == 0 {
                return Ok(i + 1);
            }
            depth -= 1;
        }
        i += 1;
    }
    Err("At top level")
}

/// Position just inside the next open bracket.
pub fn down_list(r: &Rope, p: usize) -> SexpResult {
    let len = r.len_chars();
    let mut i = p;
    while i < len {
        let c = r.char(i);
        if is_open(c) {
            return Ok(i + 1);
        }
        if is_close(c) {
            return Err("Containing expression ends prematurely");
        }
        i += 1;
    }
    Err("No more lists")
}

// ------------------------------------------------------------------ search

/// Next occurrence of `needle` starting at or after `from`.
pub fn find_forward(r: &Rope, needle: &str, from: usize) -> Option<usize> {
    if needle.is_empty() {
        return None;
    }
    if needle.contains('\n') {
        let hay = r.slice(from..).to_string();
        return hay.find(needle).map(|b| from + hay[..b].chars().count());
    }
    let first = r.char_to_line(from);
    for line in first..r.len_lines() {
        let start = r.line_to_char(line);
        let text = r.line(line).to_string();
        let skip_chars = from.saturating_sub(start);
        let skip_bytes = text.char_indices().nth(skip_chars).map(|(b, _)| b).unwrap_or(text.len());
        if let Some(b) = text[skip_bytes..].find(needle) {
            return Some(start + text[..skip_bytes + b].chars().count());
        }
    }
    None
}

/// Last occurrence of `needle` that ends at or before `before`.
pub fn find_backward(r: &Rope, needle: &str, before: usize) -> Option<usize> {
    if needle.is_empty() {
        return None;
    }
    if needle.contains('\n') {
        let hay = r.slice(..before).to_string();
        return hay.rfind(needle).map(|b| hay[..b].chars().count());
    }
    let last = r.char_to_line(before);
    for line in (0..=last).rev() {
        let start = r.line_to_char(line);
        let mut text = r.line(line).to_string();
        if line == last {
            let keep = before - start;
            let b = text.char_indices().nth(keep).map(|(b, _)| b).unwrap_or(text.len());
            text.truncate(b);
        }
        if let Some(b) = text.rfind(needle) {
            return Some(start + text[..b].chars().count());
        }
    }
    None
}

// ------------------------------------------------------------------- filling

/// Reflow `text` (one paragraph) to `width` columns, prefixing each line with `indent`.
pub fn fill(text: &str, indent: &str, width: usize) -> String {
    let mut out = String::new();
    let mut col = 0;
    for word in text.split_whitespace() {
        let wlen = word.chars().count();
        if col == 0 {
            out.push_str(indent);
            col = indent.chars().count();
        } else if col + 1 + wlen > width {
            out.push('\n');
            out.push_str(indent);
            col = indent.chars().count();
        } else {
            out.push(' ');
            col += 1;
        }
        out.push_str(word);
        col += wlen;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(s: &str) -> Rope {
        Rope::from_str(s)
    }

    #[test]
    fn words() {
        let t = r("foo  bar-baz, qux");
        assert_eq!(forward_word(&t, 0), 3);
        assert_eq!(forward_word(&t, 3), 8);
        assert_eq!(forward_word(&t, 8), 12);
        assert_eq!(backward_word(&t, 12), 9);
        assert_eq!(backward_word(&t, 9), 5);
        assert_eq!(forward_word_n(&t, 0, 3), 12);
        assert_eq!(forward_word_n(&t, 12, -2), 5);
    }

    #[test]
    fn paragraphs() {
        let t = r("a\nb\n\nc\nd\n\n\ne\n");
        assert_eq!(forward_paragraph(&t, 0), 4); // blank line after "a b"
        assert_eq!(forward_paragraph(&t, 4), 9);
        assert_eq!(backward_paragraph(&t, 9), 4);
        assert_eq!(backward_paragraph(&t, 6), 4);
        assert_eq!(backward_paragraph(&t, 4), 0);
        assert_eq!(paragraph_bounds(&t, 6), (5, 8));
    }

    #[test]
    fn sentences() {
        let t = r("One two. Three four? Five\nsix.\n\nNew para.");
        assert_eq!(forward_sentence(&t, 0), 8);
        assert_eq!(forward_sentence(&t, 8), 20);
        assert_eq!(forward_sentence(&t, 20), 30);
        assert_eq!(backward_sentence(&t, 30), 21);
        assert_eq!(backward_sentence(&t, 21), 9);
        assert_eq!(backward_sentence(&t, 15), 9);
        assert_eq!(forward_sentence(&t, 30), 41);
    }

    #[test]
    fn sexps() {
        let t = r("(foo (bar \"x)\" baz) qux) end");
        assert_eq!(forward_sexp(&t, 0), Ok(24));
        assert_eq!(forward_sexp(&t, 1), Ok(4));
        assert_eq!(forward_sexp(&t, 4), Ok(19));
        assert_eq!(backward_sexp(&t, 19), Ok(5));
        assert_eq!(backward_sexp(&t, 24), Ok(0));
        assert_eq!(forward_sexp(&t, 10), Ok(14)); // string
        assert_eq!(backward_up_list(&t, 7), Ok(5));
        assert_eq!(up_list(&t, 7), Ok(19));
        assert_eq!(down_list(&t, 0), Ok(1));
        assert!(forward_sexp(&t, 19).is_ok());
        assert_eq!(forward_sexp(&t, 23), Err("Containing expression ends prematurely"));
    }

    #[test]
    fn search() {
        let t = r("ab foo\ncd foo foo\n");
        assert_eq!(find_forward(&t, "foo", 0), Some(3));
        assert_eq!(find_forward(&t, "foo", 4), Some(10));
        assert_eq!(find_forward(&t, "foo", 11), Some(14));
        assert_eq!(find_forward(&t, "foo", 15), None);
        assert_eq!(find_backward(&t, "foo", 14), Some(10));
        assert_eq!(find_backward(&t, "foo", 10), Some(3));
        assert_eq!(find_forward(&t, "foo\ncd", 0), Some(3));
    }

    #[test]
    fn filling() {
        assert_eq!(fill("aaa bbb ccc ddd", "  ", 10), "  aaa bbb\n  ccc ddd");
    }
}
