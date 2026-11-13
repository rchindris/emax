//! Syntax highlighting with tree-sitter.
//!
//! A major mode names a language (`"syntax": "rust"` in its configuration,
//! inherited from its parent). A buffer in such a mode gets a [`Syntax`]: a
//! parser and the buffer's syntax tree. Every change to the text is
//! recorded as an edit of the tree as it happens ([`Syntax::record_edit`],
//! called from `Buffer`), and the tree is brought up to date once per
//! frame, before drawing ([`Syntax::update`]), so a command making many
//! changes costs one incremental parse.
//!
//! Highlighting runs the language's highlights query over the lines on
//! screen only and maps each capture name (`keyword`, `function.method`…)
//! to a color of the theme (`syntax.keyword`…, see `Theme::syntax_color`).
//!
//! Languages are compiled in: [`LANGUAGES`] lists them, each a grammar and
//! its highlights query. Adding one is a line there plus the grammar crate.

use std::collections::HashMap;
use std::ops::{ControlFlow, Range};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use ropey::Rope;
use streaming_iterator::StreamingIterator;
use tree_sitter::{InputEdit, Language, Node, ParseOptions, ParseState, Parser, Point, Query, QueryCursor, Tree};

use super::buffer::BufferKind;
use super::editor::Editor;
use super::theme::{Rgba, Theme};
use tree_sitter_language::LanguageFn;

/// A language emax can parse.
pub struct LanguageDef {
    /// The name modes use in `"syntax"`.
    pub name: &'static str,
    /// Other names it goes by, as in Markdown code fences (`sh`, `py`).
    pub aliases: &'static [&'static str],
    grammar: LanguageFn,
    highlights: &'static str,
    /// Where other languages are embedded (`""` when nowhere), see [`Syntax::highlight`].
    injections: &'static str,
}

const fn lang(name: &'static str, grammar: LanguageFn, highlights: &'static str) -> LanguageDef {
    LanguageDef { name, aliases: &[], grammar, highlights, injections: "" }
}

/// Every language, by name.
pub static LANGUAGES: &[LanguageDef] = &[
    LanguageDef {
        aliases: &["sh", "shell", "zsh", "shellscript", "console"],
        ..lang("bash", tree_sitter_bash::LANGUAGE, tree_sitter_bash::HIGHLIGHT_QUERY)
    },
    LanguageDef {
        aliases: &["emacs-lisp", "lisp"],
        ..lang("elisp", tree_sitter_elisp::LANGUAGE, tree_sitter_elisp::HIGHLIGHTS_QUERY)
    },
    LanguageDef { aliases: &["jsonc"], ..lang("json", tree_sitter_json::LANGUAGE, tree_sitter_json::HIGHLIGHTS_QUERY) },
    LanguageDef {
        aliases: &["md"],
        injections: tree_sitter_md::INJECTION_QUERY_BLOCK,
        ..lang("markdown", tree_sitter_md::LANGUAGE, tree_sitter_md::HIGHLIGHT_QUERY_BLOCK)
    },
    // Paragraph text, embedded by `markdown`.
    lang("markdown_inline", tree_sitter_md::INLINE_LANGUAGE, tree_sitter_md::HIGHLIGHT_QUERY_INLINE),
    LanguageDef {
        aliases: &["py", "python3"],
        ..lang("python", tree_sitter_python::LANGUAGE, tree_sitter_python::HIGHLIGHTS_QUERY)
    },
    LanguageDef { aliases: &["rs"], ..lang("rust", tree_sitter_rust::LANGUAGE, tree_sitter_rust::HIGHLIGHTS_QUERY) },
    LanguageDef {
        aliases: &["scm", "racket", "steel", "guile"],
        ..lang("scheme", tree_sitter_scheme::LANGUAGE, tree_sitter_scheme::HIGHLIGHTS_QUERY)
    },
    lang("toml", tree_sitter_toml_ng::LANGUAGE, tree_sitter_toml_ng::HIGHLIGHTS_QUERY),
    LanguageDef { aliases: &["yml"], ..lang("yaml", tree_sitter_yaml::LANGUAGE, tree_sitter_yaml::HIGHLIGHTS_QUERY) },
];

/// Embedded languages nest at most this deep (Markdown text in a code
/// fence in Markdown…).
const MAX_INJECTION_DEPTH: usize = 3;

/// A parse that takes longer than this is given up and the buffer is shown
/// without highlighting (it never stalls typing).
const PARSE_BUDGET: Duration = Duration::from_millis(500);

pub fn language(name: &str) -> Option<&'static LanguageDef> {
    LANGUAGES.iter().find(|l| l.name == name)
}

/// A language by its name or an alias, ignoring case: how embedded code
/// names its language (a Markdown fence's info string).
pub fn language_named(name: &str) -> Option<&'static LanguageDef> {
    let name = name.to_ascii_lowercase();
    LANGUAGES.iter().find(|l| l.name == name || l.aliases.contains(&name.as_str()))
}

/// The names of the languages, for messages.
pub fn language_names() -> Vec<&'static str> {
    LANGUAGES.iter().map(|l| l.name).collect()
}

/// A language's grammar and compiled highlights query (built once, on first use).
struct Compiled {
    language: Language,
    query: Query,
    injections: Option<Injections>,
}

/// A compiled injections query and the indices of its captures.
struct Injections {
    query: Query,
    content: u32,
    language: Option<u32>,
}

fn compiled(def: &'static LanguageDef) -> &'static Compiled {
    static ALL: OnceLock<Vec<Compiled>> = OnceLock::new();
    let all = ALL.get_or_init(|| {
        LANGUAGES
            .iter()
            .map(|l| {
                let language = Language::new(l.grammar);
                let query = Query::new(&language, l.highlights)
                    .unwrap_or_else(|e| panic!("built-in highlights query for {}: {e}", l.name));
                let injections = (!l.injections.is_empty()).then(|| {
                    let query = Query::new(&language, l.injections)
                        .unwrap_or_else(|e| panic!("built-in injections query for {}: {e}", l.name));
                    let content = query.capture_index_for_name("injection.content").expect("@injection.content");
                    let language = query.capture_index_for_name("injection.language");
                    Injections { query, content, language }
                });
                Compiled { language, query, injections }
            })
            .collect()
    });
    let i = LANGUAGES.iter().position(|l| std::ptr::eq(l, def)).expect("a language from LANGUAGES");
    &all[i]
}

/// Text colors for some lines: per line, column ranges (in chars) and their color.
pub type LineColors = Vec<(Range<usize>, Rgba)>;

/// A buffer's parser and syntax tree.
pub struct Syntax {
    pub language: &'static LanguageDef,
    parser: Parser,
    tree: Option<Tree>,
    /// Edits were recorded since the last parse.
    stale: bool,
    /// The buffer's length (chars) after the recorded edits: if the text
    /// has another length, it changed without telling us, and is parsed afresh.
    expected_chars: usize,
    /// The last parse ran out of time: don't retry until the text changes.
    gave_up: bool,
    /// The trees of the embedded code last highlighted.
    injected: Mutex<InjectionCache>,
}

impl Syntax {
    pub fn new(language: &'static LanguageDef) -> Self {
        let mut parser = Parser::new();
        parser.set_language(&compiled(language).language).expect("grammar matches the tree-sitter version");
        Self {
            language,
            parser,
            tree: None,
            stale: true,
            expected_chars: usize::MAX,
            gave_up: false,
            injected: Mutex::default(),
        }
    }

    /// The tree as of the last `update` (`None` before the first parse, or
    /// if parsing gave up).
    pub fn tree(&self) -> Option<&Tree> {
        self.tree.as_ref()
    }

    /// Record that chars `start..start + old_chars` of `rope` (the text
    /// *before* the change) are being replaced by `new`.
    pub fn record_edit(&mut self, rope: &Rope, start: usize, old_chars: usize, new: &str) {
        self.stale = true;
        self.gave_up = false;
        self.expected_chars = rope.len_chars() - old_chars + new.chars().count();
        let Some(tree) = self.tree.as_mut() else { return };
        let at = |c: usize| {
            let line = rope.char_to_line(c);
            let byte = rope.char_to_byte(c);
            (byte, Point::new(line, byte - rope.line_to_byte(line)))
        };
        let (start_byte, start_position) = at(start);
        let (old_end_byte, old_end_position) = at(start + old_chars);
        let new_end_position = match new.rfind('\n') {
            Some(i) => Point::new(start_position.row + new.matches('\n').count(), new.len() - i - 1),
            None => Point::new(start_position.row, start_position.column + new.len()),
        };
        tree.edit(&InputEdit {
            start_byte,
            old_end_byte,
            new_end_byte: start_byte + new.len(),
            start_position,
            old_end_position,
            new_end_position,
        });
    }

    /// Bring the tree up to date with `rope`: incrementally after recorded
    /// edits, from scratch the first time or if the text changed unrecorded.
    pub fn update(&mut self, rope: &Rope) {
        if rope.len_chars() != self.expected_chars {
            self.tree = None;
            self.stale = true;
            self.gave_up = false;
            self.expected_chars = rope.len_chars();
        }
        if !self.stale || self.gave_up {
            return;
        }
        let len = rope.len_bytes();
        let mut read = |byte: usize, _: Point| -> &[u8] {
            if byte >= len {
                return &[];
            }
            let (chunk, chunk_start, _, _) = rope.chunk_at_byte(byte);
            &chunk.as_bytes()[byte - chunk_start..]
        };
        let started = Instant::now();
        let mut budget = |_: &ParseState| {
            if started.elapsed() > PARSE_BUDGET { ControlFlow::Break(()) } else { ControlFlow::Continue(()) }
        };
        let options = ParseOptions::new().progress_callback(&mut budget);
        let tree = self.parser.parse_with_options(&mut read, self.tree.as_ref(), Some(options));
        if tree.is_none() {
            self.parser.reset();
            self.gave_up = true;
        }
        self.tree = tree;
        self.stale = false;
    }

    /// The text colors of `lines` (clamped to the buffer), one entry per line.
    ///
    /// Where captures overlap, the innermost node wins; for the same node,
    /// the first pattern in the query wins (tree-sitter's convention).
    /// Captures the theme has no color for leave the text as it is.
    ///
    /// Code in another language (a language's injections query finds it:
    /// a Markdown code fence, Markdown paragraph text) is parsed on its own
    /// and painted over its host's colors.
    pub fn highlight(&self, rope: &Rope, lines: Range<usize>, theme: &Theme) -> Vec<LineColors> {
        let last = rope.len_lines();
        let lines = lines.start.min(last)..lines.end.min(last);
        let mut out = vec![Vec::new(); lines.len()];
        let Some(tree) = self.tree.as_ref() else { return out };
        if lines.is_empty() {
            return out;
        }
        let (first_char, end_char) = (rope.line_to_char(lines.start), rope.line_to_char(lines.end));
        let mut painter = Painter {
            host: rope,
            first_char,
            first_byte: rope.char_to_byte(first_char),
            cells: vec![None; end_char - first_char],
            theme,
            cache: self.injected.lock().unwrap_or_else(|e| e.into_inner()),
            used: Vec::new(),
        };
        let range = painter.first_byte..rope.char_to_byte(end_char);
        painter.layer(self.language, tree, rope, 0, range, 0);
        let Painter { cells, mut cache, used, .. } = painter;
        cache.retain(|k, _| used.contains(k));
        drop(cache);

        for (i, line) in lines.enumerate() {
            let start = rope.line_to_char(line) - first_char;
            let end = rope.line_to_char(line + 1).min(end_char) - first_char;
            let row = &cells[start..end];
            let mut col = 0;
            while col < row.len() {
                let c = row[col];
                let mut j = col + 1;
                while j < row.len() && row[j] == c {
                    j += 1;
                }
                if let Some(c) = c {
                    out[i].push((col..j, c));
                }
                col = j;
            }
        }
        out
    }
}

/// Embedded code's trees, by language and text.
type InjectionCache = HashMap<(&'static str, String), Tree>;

/// Paints the colors of a range of the buffer, one language layer at a time.
struct Painter<'a> {
    host: &'a Rope,
    /// Where the painted range starts in `host`.
    first_char: usize,
    first_byte: usize,
    /// The color of each char of the range.
    cells: Vec<Option<Rgba>>,
    theme: &'a Theme,
    cache: MutexGuard<'a, InjectionCache>,
    /// The cache entries this highlight used (the others are dropped).
    used: Vec<(&'static str, String)>,
}

impl Painter<'_> {
    /// Paint `range` (bytes of `text`) from `tree`, the tree of `text` in
    /// `language`. `text` starts at byte `base` of the buffer.
    fn layer(
        &mut self,
        language: &'static LanguageDef,
        tree: &Tree,
        text: &Rope,
        base: usize,
        range: Range<usize>,
        depth: usize,
    ) {
        let this = compiled(language);
        let source = |node: Node| text.byte_slice(node.byte_range()).chunks().map(str::as_bytes);
        let colors: Vec<Option<Rgba>> = this.query.capture_names().iter().map(|n| self.theme.syntax_color(n)).collect();

        let mut captures: Vec<(usize, usize, usize, u32)> = Vec::new();
        let mut cursor = QueryCursor::new();
        cursor.set_byte_range(range.clone());
        let mut matches = cursor.matches(&this.query, tree.root_node(), source);
        while let Some(m) = matches.next() {
            for c in m.captures {
                captures.push((c.node.start_byte(), c.node.end_byte(), m.pattern_index, c.index));
            }
        }
        // Outer nodes first (so inner ones paint over them), then by pattern.
        captures.sort_by_key(|&(s, e, p, _)| (s, std::cmp::Reverse(e), p));
        let mut last_range = None;
        for (s, e, _, capture) in captures {
            if last_range == Some((s, e)) {
                continue;
            }
            last_range = Some((s, e));
            if let Some(color) = colors[capture as usize] {
                self.paint(base + s.clamp(range.start, range.end), base + e.clamp(range.start, range.end), color);
            }
        }

        let Some(inj) = this.injections.as_ref().filter(|_| depth < MAX_INJECTION_DEPTH) else { return };
        let mut embedded: Vec<(&'static LanguageDef, Range<usize>)> = Vec::new();
        let mut cursor = QueryCursor::new();
        cursor.set_byte_range(range.clone());
        let mut matches = cursor.matches(&inj.query, tree.root_node(), source);
        while let Some(m) = matches.next() {
            let named = m.captures.iter().find(|c| Some(c.index) == inj.language).map(|c| {
                let name: String = text.byte_slice(c.node.byte_range()).into();
                // An info string can say more: `rust,ignore`, `{.python}`.
                let name = name.trim_start_matches(['{', '.']).to_string();
                name.split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '-' || c == '+'))
                    .next()
                    .unwrap_or("")
                    .to_string()
            });
            let set = || {
                inj.query
                    .property_settings(m.pattern_index)
                    .iter()
                    .find(|p| &*p.key == "injection.language")?
                    .value
                    .as_deref()
                    .map(str::to_string)
            };
            let Some(lang) = named.or_else(set).as_deref().and_then(language_named) else { continue };
            for c in m.captures.iter().filter(|c| c.index == inj.content) {
                embedded.push((lang, c.node.byte_range()));
            }
        }
        for (lang, content) in embedded {
            let code: String = text.byte_slice(content.clone()).into();
            let key = (lang.name, code);
            if !self.cache.contains_key(&key) {
                let mut parser = Parser::new();
                if parser.set_language(&compiled(lang).language).is_err() {
                    continue;
                }
                let Some(tree) = parser.parse(&key.1, None) else { continue };
                self.cache.insert(key.clone(), tree);
            }
            let sub = Rope::from_str(&key.1);
            let tree = self.cache[&key].clone();
            self.used.push(key);
            let inner = range.start.max(content.start) - content.start
                ..range.end.min(content.end).max(content.start) - content.start;
            if !inner.is_empty() {
                self.layer(lang, &tree, &sub, base + content.start, inner, depth + 1);
            }
        }
    }

    /// Paint buffer bytes `start..end` (within the painted range).
    fn paint(&mut self, start: usize, end: usize, color: Rgba) {
        if start >= end {
            return;
        }
        let (s, e) = (self.host.byte_to_char(start) - self.first_char, self.host.byte_to_char(end) - self.first_char);
        self.cells[s..e].fill(Some(color));
    }
}

impl Editor {
    /// Bring the syntax trees of buffers `ids` (those on screen) up to date:
    /// start or drop one when the major mode's language changed, then parse.
    pub fn update_syntax(&mut self, ids: &[u64]) {
        let modes = self.modes.clone();
        let current = std::iter::once((self.buf_id, &mut self.buf));
        for (id, buf) in current.chain(self.others.iter_mut().map(|s| (s.id, &mut s.buf))) {
            if !ids.contains(&id) {
                continue;
            }
            let want =
                if buf.kind == BufferKind::Text { modes.syntax(buf.modes.major).and_then(language) } else { None };
            let have = buf.syntax.as_ref().map(|s| s.language);
            if want.map(|l| l.name) != have.map(|l| l.name) {
                buf.syntax = want.map(|l| Box::new(Syntax::new(l)));
            }
            if let Some(s) = buf.syntax.as_mut() {
                s.update(&buf.rope);
            }
        }
    }
}

#[cfg(test)]
#[path = "syntax_tests.rs"]
mod tests;
