//! Swiper-style search (`swiper`, `swiper-backward`): every line matching the query is listed in
//! a panel; moving through the list previews the match in the buffer, RET jumps
//! there (leaving the mark where the search started), C-g goes back.
//!
//! The query is split on whitespace and a line matches when it contains every
//! term, in any order. Smart case: a term with an uppercase letter is
//! case-sensitive, otherwise it ignores case.

use std::ops::Range;

use ropey::Rope;

use super::cursor::Cursor;
use super::editor::{Editor, Scroll};
use super::keymap::{Key, Keymap};

/// What the search keymap can bind keys to.
pub const SEARCH_ACTIONS: &[&str] = &[
    "search-quit",
    "search-accept",
    "search-next",
    "search-previous",
    "search-next-or-history",
    "search-previous-or-history",
    "search-page-down",
    "search-page-up",
    "search-first",
    "search-last",
    "search-delete-char",
    "search-delete-word",
    "search-clear",
    "search-insert-symbol",
];

/// How far search-page-down / search-page-up move the selection.
pub const PAGE: i64 = 10;

/// Lower-case a char without changing the char count (so folded and original
/// lines share char positions).
fn fold(c: char) -> char {
    let mut l = c.to_lowercase();
    match (l.next(), l.next()) {
        (Some(x), None) => x,
        _ => c,
    }
}

fn fold_str(s: &str) -> String {
    s.chars().map(fold).collect()
}

struct Term {
    /// Folded unless case-sensitive.
    text: String,
    case_sensitive: bool,
}

fn parse(query: &str) -> Vec<Term> {
    query
        .split_whitespace()
        .map(|t| {
            let case_sensitive = t.chars().any(char::is_uppercase);
            Term { text: if case_sensitive { t.to_string() } else { fold_str(t) }, case_sensitive }
        })
        .collect()
}

fn char_index(s: &str, byte: usize) -> usize {
    s[..byte].chars().count()
}

/// What happened to the search after a key.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SwiperExit {
    Open,
    Accepted,
    Cancelled,
}

pub struct Swiper {
    pub query: String,
    pub backward: bool,
    /// The buffer's lines (without newlines), snapshotted when the search opened:
    /// the buffer can't change while the search has the keyboard.
    lines: Vec<String>,
    /// `lines` with each char lower-cased, for case-insensitive terms.
    folded: Vec<String>,
    terms: Vec<Term>,
    /// The query `matches` was computed for (to narrow instead of rescanning).
    matched_query: String,
    /// Matching line numbers, in buffer order.
    pub matches: Vec<usize>,
    pub selected: usize,
    pub origin_line: usize,
    origin_point: usize,
    origin_col: usize,
    origin_cursors: Vec<Cursor>,
    origin_primary: u64,
}

impl Swiper {
    fn new(rope: &Rope, cursors: Vec<Cursor>, primary: u64, point: usize, backward: bool) -> Self {
        let lines: Vec<String> = rope
            .lines()
            .map(|l| {
                let mut s = l.to_string();
                if s.ends_with('\n') {
                    s.pop();
                    if s.ends_with('\r') {
                        s.pop();
                    }
                }
                s
            })
            .collect();
        let folded = lines.iter().map(|l| fold_str(l)).collect();
        let point = point.min(rope.len_chars());
        let origin_line = rope.char_to_line(point);
        let mut s = Self {
            query: String::new(),
            backward,
            lines,
            folded,
            terms: Vec::new(),
            matched_query: String::new(),
            matches: Vec::new(),
            selected: 0,
            origin_line,
            origin_point: point,
            origin_col: point - rope.line_to_char(origin_line),
            origin_cursors: cursors,
            origin_primary: primary,
        };
        s.refilter();
        s
    }

    pub fn total_lines(&self) -> usize {
        self.lines.len()
    }

    pub fn line_text(&self, line: usize) -> &str {
        self.lines.get(line).map_or("", |s| s.as_str())
    }

    pub fn selected_line(&self) -> Option<usize> {
        self.matches.get(self.selected).copied()
    }

    pub fn has_terms(&self) -> bool {
        !self.terms.is_empty()
    }

    fn line_matches(&self, i: usize) -> bool {
        self.terms.iter().all(|t| {
            let hay = if t.case_sensitive { &self.lines[i] } else { &self.folded[i] };
            hay.contains(&t.text)
        })
    }

    fn refilter(&mut self) {
        // Typing more only narrows the previous matches, so skip the full rescan.
        let narrowing = !self.matched_query.trim().is_empty() && self.query.starts_with(&self.matched_query);
        self.terms = parse(&self.query);
        let candidates = if narrowing { std::mem::take(&mut self.matches) } else { (0..self.lines.len()).collect() };
        self.matches = candidates.into_iter().filter(|&i| self.line_matches(i)).collect();
        self.matched_query = self.query.clone();
        self.preselect();
    }

    /// Select the first match at or after the line the search started on
    /// (the last one at or before it, searching backward).
    fn preselect(&mut self) {
        let o = self.origin_line;
        let n = self.matches.len();
        self.selected = if self.backward {
            self.matches.iter().rposition(|&l| l <= o).unwrap_or(n.saturating_sub(1))
        } else {
            self.matches.iter().position(|&l| l >= o).unwrap_or(0)
        };
    }

    /// Char ranges (within `line`) of every term occurrence, sorted and merged.
    pub fn highlights(&self, line: usize) -> Vec<Range<usize>> {
        if self.terms.is_empty() || line >= self.lines.len() {
            return Vec::new();
        }
        let mut v: Vec<Range<usize>> = Vec::new();
        for t in &self.terms {
            let hay = if t.case_sensitive { &self.lines[line] } else { &self.folded[line] };
            let len = t.text.chars().count();
            for (b, _) in hay.match_indices(&t.text) {
                let s = char_index(hay, b);
                v.push(s..s + len);
            }
        }
        v.sort_by_key(|r| (r.start, r.end));
        let mut merged: Vec<Range<usize>> = Vec::with_capacity(v.len());
        for r in v {
            match merged.last_mut() {
                Some(last) if r.start <= last.end => last.end = last.end.max(r.end),
                _ => merged.push(r),
            }
        }
        merged
    }

    /// Column point lands on for a match on `line`: the end of the first term's
    /// first occurrence (like swiper / isearch).
    fn match_col(&self, line: usize) -> usize {
        let Some(t) = self.terms.first() else { return 0 };
        let hay = if t.case_sensitive { &self.lines[line] } else { &self.folded[line] };
        hay.find(&t.text).map_or(0, |b| char_index(hay, b) + t.text.chars().count())
    }

    pub fn move_by(&mut self, d: i64) {
        let n = self.matches.len() as i64;
        if n > 0 {
            self.selected = (self.selected as i64 + d).rem_euclid(n) as usize;
        }
    }

    pub fn move_clamped(&mut self, d: i64) {
        let n = self.matches.len() as i64;
        if n > 0 {
            self.selected = (self.selected as i64 + d).clamp(0, n - 1) as usize;
        }
    }

    pub fn push_str(&mut self, s: &str) {
        self.query.push_str(s);
        self.refilter();
    }

    fn delete_char(&mut self) {
        self.query.pop();
        self.refilter();
    }

    fn delete_word(&mut self) {
        let trimmed = self.query.trim_end_matches(|c: char| !c.is_alphanumeric());
        let cut = trimmed.rfind(|c: char| !c.is_alphanumeric()).map_or(0, |i| i + 1);
        self.query.truncate(cut);
        self.refilter();
    }

    /// The symbol around where the search started (for M-n).
    fn symbol_at_origin(&self) -> Option<String> {
        let chars: Vec<char> = self.line_text(self.origin_line).chars().collect();
        let col = self.origin_col;
        let sym = |c: &char| c.is_alphanumeric() || *c == '_' || *c == '-';
        let mut s = col.min(chars.len());
        while s > 0 && sym(&chars[s - 1]) {
            s -= 1;
        }
        let mut e = col.min(chars.len());
        while e < chars.len() && sym(&chars[e]) {
            e += 1;
        }
        (s < e).then(|| chars[s..e].iter().collect())
    }
}

impl Editor {
    pub fn open_swiper(&mut self, backward: bool) {
        let point = self.primary_cursor().point;
        self.swiper = Some(Swiper::new(&self.buf.rope, self.cursors.clone(), self.primary, point, backward));
        self.scroll = Scroll::EnsureVisible;
    }

    fn restore_origin(&mut self) {
        let Some(s) = self.swiper.as_ref() else { return };
        self.cursors = s.origin_cursors.clone();
        self.primary = s.origin_primary;
    }

    /// Move point to the selected match (or back to the start if there is none).
    fn swiper_preview(&mut self) {
        let Some(s) = self.swiper.as_ref() else { return };
        let Some(line) = s.selected_line() else {
            self.restore_origin();
            return;
        };
        let pos = if !s.has_terms() && line == s.origin_line {
            s.origin_point
        } else {
            self.buf.rope.line_to_char(line) + s.match_col(line)
        };
        let mut c = s
            .origin_cursors
            .iter()
            .find(|c| c.id == s.origin_primary)
            .cloned()
            .unwrap_or_else(|| Cursor::new(s.origin_primary, 0));
        c.point = pos;
        c.goal_col = None;
        c.deactivate();
        self.primary = c.id;
        self.cursors = vec![c];
        self.scroll = Scroll::EnsureVisible;
        self.reveal = None;
    }

    fn swiper_accept(&mut self) -> SwiperExit {
        let Some(s) = self.swiper.as_ref() else { return SwiperExit::Cancelled };
        let (found, origin, query) = (s.selected_line().is_some(), s.origin_point, s.query.trim().to_string());
        if !found {
            self.restore_origin();
            self.message = Some(format!("No match for “{query}”"));
        } else {
            self.swiper_preview();
            let i = self.primary_index();
            if self.cursors[i].point != origin {
                self.cursors[i].push_mark(origin, false);
                self.message = Some("Mark saved where search started".into());
            }
        }
        let s = self.swiper.take().unwrap();
        if s.has_terms() {
            self.last_search = s.query.trim().to_string();
        }
        self.last_command = "swiper";
        self.scroll = Scroll::EnsureVisible;
        SwiperExit::Accepted
    }

    /// Accept match `i` (mouse click on a row).
    pub fn swiper_accept_index(&mut self, i: usize) -> SwiperExit {
        if let Some(s) = self.swiper.as_mut() {
            s.selected = i.min(s.matches.len().saturating_sub(1));
        }
        self.swiper_accept()
    }

    /// Handle a key while the search is open: run the action `keymap` binds
    /// it to, or type it into the query.
    pub fn swiper_key(&mut self, keymap: &Keymap, key: &Key) -> SwiperExit {
        match keymap.lookup(&[key.canonical()]) {
            Some(action) => self.swiper_action(action),
            None => match key.insertable_text() {
                Some(t) => {
                    let t = t.to_string();
                    self.swiper_insert(&t)
                }
                None if self.swiper.is_some() => SwiperExit::Open,
                None => SwiperExit::Cancelled,
            },
        }
    }

    pub fn swiper_insert(&mut self, text: &str) -> SwiperExit {
        let Some(s) = self.swiper.as_mut() else { return SwiperExit::Cancelled };
        s.push_str(text);
        self.swiper_preview();
        SwiperExit::Open
    }

    /// Run a search action (one of `SEARCH_ACTIONS`).
    pub fn swiper_action(&mut self, action: &str) -> SwiperExit {
        let last_search = self.last_search.clone();
        let Some(s) = self.swiper.as_mut() else { return SwiperExit::Cancelled };
        let recall = s.query.is_empty() && !last_search.is_empty();
        match action {
            "search-quit" => {
                self.restore_origin();
                self.swiper = None;
                self.message = Some("Quit".into());
                self.scroll = Scroll::Keep;
                return SwiperExit::Cancelled;
            }
            "search-accept" => return self.swiper_accept(),
            // With an empty query, recall the previous search (like ivy's C-s C-s).
            "search-next-or-history" if recall => s.push_str(&last_search),
            "search-previous-or-history" if recall => {
                s.backward = true;
                s.push_str(&last_search);
            }
            "search-next" | "search-next-or-history" => s.move_by(1),
            "search-previous" | "search-previous-or-history" => s.move_by(-1),
            "search-page-down" => s.move_clamped(PAGE),
            "search-page-up" => s.move_clamped(-PAGE),
            "search-first" => s.selected = 0,
            "search-last" => s.selected = s.matches.len().saturating_sub(1),
            "search-delete-char" => s.delete_char(),
            "search-delete-word" => s.delete_word(),
            "search-clear" => {
                s.query.clear();
                s.refilter();
            }
            "search-insert-symbol" => {
                if let Some(sym) = s.symbol_at_origin() {
                    if !s.query.is_empty() && !s.query.ends_with(' ') {
                        s.query.push(' ');
                    }
                    s.push_str(&sym);
                }
            }
            _ => {}
        }
        self.swiper_preview();
        SwiperExit::Open
    }
}
