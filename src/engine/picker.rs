//! A generic "completing read" picker: a query, a list of candidates, fuzzy
//! filtering with match positions (for highlighting) and a selection.
//! Used by switch-to-buffer and M-x.

use super::editor::Args;

/// What the picker is choosing.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum PickerKind {
    /// A buffer (`Candidate::id` is the buffer id).
    #[default]
    Buffers,
    /// A command to run with `args` (`Candidate::id` indexes `names`).
    Commands { names: Vec<&'static str>, args: Args },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Candidate {
    /// Identifies what the candidate stands for (a buffer id, ...).
    pub id: u64,
    pub label: String,
    /// Secondary text (e.g. the file path).
    pub detail: String,
    /// Right-aligned note (e.g. "1,204 lines").
    pub annotation: String,
    pub modified: bool,
    pub current: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Match {
    /// Index into `candidates`.
    pub index: usize,
    pub score: i64,
    /// Char positions in the label that matched the query.
    pub positions: Vec<usize>,
}

pub struct Picker {
    pub kind: PickerKind,
    pub prompt: String,
    pub query: String,
    pub candidates: Vec<Candidate>,
    pub matches: Vec<Match>,
    pub selected: usize,
    /// Buffer id awaiting a second C-k because it has unsaved changes.
    pub confirm_kill: Option<u64>,
    pub message: Option<String>,
}

impl Picker {
    pub fn new(prompt: &str, candidates: Vec<Candidate>) -> Self {
        let mut p = Self {
            kind: PickerKind::Buffers,
            prompt: prompt.into(),
            query: String::new(),
            candidates,
            matches: Vec::new(),
            selected: 0,
            confirm_kill: None,
            message: None,
        };
        p.refilter();
        p
    }

    pub fn with_kind(mut self, kind: PickerKind) -> Self {
        self.kind = kind;
        self
    }

    pub fn set_candidates(&mut self, candidates: Vec<Candidate>) {
        let keep = self.selected;
        self.candidates = candidates;
        self.refilter();
        self.selected = keep.min(self.matches.len().saturating_sub(1));
    }

    pub fn refilter(&mut self) {
        let mut matches: Vec<Match> = self
            .candidates
            .iter()
            .enumerate()
            .filter_map(|(i, c)| fuzzy(&self.query, &c.label).map(|(score, positions)| Match { index: i, score, positions }))
            .collect();
        if !self.query.trim().is_empty() {
            // Best score first; ties keep the original (most-recently-used) order.
            matches.sort_by(|a, b| b.score.cmp(&a.score).then(a.index.cmp(&b.index)));
        }
        self.matches = matches;
        self.selected = 0;
    }

    pub fn selected_candidate(&self) -> Option<&Candidate> {
        self.matches.get(self.selected).map(|m| &self.candidates[m.index])
    }

    pub fn move_by(&mut self, d: i64) {
        let n = self.matches.len() as i64;
        if n == 0 {
            return;
        }
        self.selected = (self.selected as i64 + d).rem_euclid(n) as usize;
    }

    /// Like `move_by` but stops at the ends (for paging).
    pub fn move_clamped(&mut self, d: i64) {
        let n = self.matches.len() as i64;
        if n == 0 {
            return;
        }
        self.selected = (self.selected as i64 + d).clamp(0, n - 1) as usize;
    }

    pub fn push_str(&mut self, s: &str) {
        self.query.push_str(s);
        self.refilter();
    }

    pub fn delete_char(&mut self) {
        self.query.pop();
        self.refilter();
    }

    pub fn delete_word(&mut self) {
        let trimmed = self.query.trim_end_matches(|c: char| !c.is_alphanumeric());
        let cut = trimmed.rfind(|c: char| !c.is_alphanumeric()).map(|i| i + 1).unwrap_or(0);
        self.query.truncate(cut);
        self.refilter();
    }
}

// ------------------------------------------------------------------ fuzzy

/// Orderless fuzzy match: every space-separated term must match the text as a
/// subsequence (in any order relative to the other terms). Smart case: a term
/// with an uppercase letter matches case-sensitively. Returns (score, positions).
pub fn fuzzy(query: &str, text: &str) -> Option<(i64, Vec<usize>)> {
    let terms: Vec<&str> = query.split_whitespace().collect();
    if terms.is_empty() {
        return Some((0, Vec::new()));
    }
    let chars: Vec<char> = text.chars().collect();
    let mut total = 0;
    let mut positions = Vec::new();
    for t in terms {
        let (s, p) = fuzzy_term(t, &chars)?;
        total += s;
        positions.extend(p);
    }
    positions.sort_unstable();
    positions.dedup();
    // Prefer shorter candidates a little.
    total -= chars.len() as i64 / 10;
    Some((total, positions))
}

fn is_boundary(t: &[char], p: usize) -> bool {
    if p == 0 {
        return true;
    }
    let (a, b) = (t[p - 1], t[p]);
    (!a.is_alphanumeric() && b.is_alphanumeric()) || (a.is_lowercase() && b.is_uppercase())
}

fn fuzzy_term(term: &str, text: &[char]) -> Option<(i64, Vec<usize>)> {
    let q: Vec<char> = term.chars().collect();
    let smart_case = q.iter().any(|c| c.is_uppercase());
    let eq = |a: char, b: char| if smart_case { a == b } else { a.to_lowercase().eq(b.to_lowercase()) };

    // Forward pass: earliest position where the whole term has matched.
    let mut qi = 0;
    let mut end = None;
    for (i, &c) in text.iter().enumerate() {
        if eq(c, q[qi]) {
            qi += 1;
            if qi == q.len() {
                end = Some(i);
                break;
            }
        }
    }
    let end = end?;
    // Backward pass: latest start, giving the tightest window ending at `end`.
    let mut qi = q.len();
    let mut start = 0;
    for i in (0..=end).rev() {
        if eq(text[i], q[qi - 1]) {
            qi -= 1;
            if qi == 0 {
                start = i;
                break;
            }
        }
    }
    // Prefer an occurrence that begins on a word boundary, if one exists later.
    let mut best: Option<(i64, Vec<usize>)> = None;
    let mut s = start;
    loop {
        if let Some(found) = match_from(&q, text, s, &eq) {
            let sc = score(text, &found);
            if best.as_ref().is_none_or(|(b, _)| sc > *b) {
                best = Some((sc, found));
            }
        }
        // Next candidate start: the next boundary position whose char matches q[0].
        match (s + 1..text.len()).find(|&i| eq(text[i], q[0]) && is_boundary(text, i)) {
            Some(n) => s = n,
            None => break,
        }
    }
    let (mut sc, pos) = best?;
    if text.len() == q.len() && text.iter().zip(&q).all(|(a, b)| eq(*a, *b)) {
        sc += 50; // exact match
    }
    Some((sc, pos))
}

fn match_from(q: &[char], text: &[char], start: usize, eq: &dyn Fn(char, char) -> bool) -> Option<Vec<usize>> {
    let mut pos = Vec::with_capacity(q.len());
    let mut qi = 0;
    for (i, &c) in text.iter().enumerate().skip(start) {
        if qi < q.len() && eq(c, q[qi]) {
            pos.push(i);
            qi += 1;
            if qi == q.len() {
                return Some(pos);
            }
        }
    }
    None
}

fn score(text: &[char], positions: &[usize]) -> i64 {
    let mut score = 0i64;
    let mut prev: Option<usize> = None;
    for &p in positions {
        score += 16;
        if is_boundary(text, p) {
            score += 10;
        }
        if let Some(pp) = prev {
            if p == pp + 1 {
                score += 12;
            } else {
                score -= ((p - pp - 1) as i64).min(8);
            }
        }
        prev = Some(p);
    }
    if positions.first() == Some(&0) {
        score += 8;
    }
    score - (positions.first().copied().unwrap_or(0) as i64).min(10) / 2
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subsequence_and_positions() {
        assert_eq!(fuzzy("mn", "main.rs").unwrap().1, vec![0, 3]);
        assert!(fuzzy("xyz", "main.rs").is_none());
        assert_eq!(fuzzy("", "anything").unwrap().0, 0);
    }

    #[test]
    fn prefers_boundaries_and_contiguity() {
        let (a, _) = fuzzy("ed", "editor.rs").unwrap();
        let (b, _) = fuzzy("ed", "keymap_dispatched.rs").unwrap();
        assert!(a > b);
        let (c, pc) = fuzzy("ui", "build_ui.rs").unwrap();
        assert_eq!(pc, vec![6, 7]); // the boundary "ui", not "u" of build + "i"
        let (d, _) = fuzzy("ui", "buildingit.rs").unwrap();
        assert!(c > d);
    }

    #[test]
    fn orderless_terms_and_smart_case() {
        assert!(fuzzy("rs main", "main.rs").is_some());
        assert!(fuzzy("Main", "main.rs").is_none());
        assert!(fuzzy("main", "Main.rs").is_some());
    }

    #[test]
    fn exact_match_wins() {
        let (a, _) = fuzzy("text", "text").unwrap();
        let (b, _) = fuzzy("text", "text.rs").unwrap();
        assert!(a > b);
    }

    #[test]
    fn picker_filtering_and_movement() {
        let c = |id, l: &str| Candidate { id, label: l.into(), detail: String::new(), annotation: String::new(), modified: false, current: false };
        let mut p = Picker::new("Buffer", vec![c(1, "main.rs"), c(2, "editor.rs"), c(3, "*scratch*")]);
        assert_eq!(p.matches.len(), 3);
        p.push_str("rs");
        assert_eq!(p.matches.len(), 2);
        p.move_by(1);
        p.move_by(1);
        assert_eq!(p.selected, 0); // wraps
        p.push_str("x");
        assert!(p.selected_candidate().is_none());
        p.delete_char();
        p.delete_word();
        assert_eq!(p.query, "");
        assert_eq!(p.matches.len(), 3);
    }
}
