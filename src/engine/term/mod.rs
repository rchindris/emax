//! Terminal emulation: a wrapper around `alacritty_terminal` that turns the
//! bytes a program prints into a grid of cells, and keys into the bytes the
//! program expects.
//!
//! [`Terminal`] is plain data (no threads, no I/O): the caller feeds it
//! output with [`Terminal::feed`] and writes [`Feed::replies`] (answers to
//! queries such as DSR/DA) back to the process. Rendering reads a
//! [`Snapshot`] with colors already resolved through a [`Palette`] built from
//! the theme.

pub mod buffer;
pub mod input;
#[cfg(test)]
mod tests;

use std::cell::RefCell;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use alacritty_terminal::event::{Event as TermEvent, EventListener, WindowSize};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::{Cell as TermCell, Flags};
use alacritty_terminal::term::{Config as TermConfig, Term, TermMode};
use alacritty_terminal::vte::ansi::{Color, CursorShape as VteCursorShape, Processor, Rgb, Timeout};
use serde::Serialize;

use super::theme::{Rgba, Theme};

/// Smallest and largest grid we build: a 1-column grid can't hold a wide
/// character, and a huge one would only waste memory.
const MIN_COLS: u16 = 2;
const MIN_ROWS: u16 = 1;
const MAX_COLS: u16 = 4096;
const MAX_ROWS: u16 = 2048;

/// Colors a terminal draws with.
#[derive(Clone, Debug, PartialEq)]
pub struct Palette {
    pub foreground: Rgba,
    pub background: Rgba,
    pub cursor: Rgba,
    /// 0–7 normal, 8–15 bright (256-color indices 16–255 are computed).
    pub ansi: [Rgba; 16],
}

impl Palette {
    pub fn from_theme(t: &Theme) -> Self {
        Self { foreground: t.foreground, background: t.background, cursor: t.cursor, ansi: t.ansi() }
    }

    /// xterm's default colors (used to answer color queries before anything
    /// has been drawn, and in tests).
    pub fn xterm() -> Self {
        Self {
            foreground: 0xe5e5e5ff,
            background: 0x000000ff,
            cursor: 0xe5e5e5ff,
            ansi: [
                0x000000ff, 0xcd0000ff, 0x00cd00ff, 0xcdcd00ff, 0x0000eeff, 0xcd00cdff, 0x00cdcdff, 0xe5e5e5ff,
                0x7f7f7fff, 0xff0000ff, 0x00ff00ff, 0xffff00ff, 0x5c5cffff, 0xff00ffff, 0x00ffffff, 0xffffffff,
            ],
        }
    }

    /// The color of 256-color index `i` (0–15 from the palette, then the
    /// 6×6×6 cube and the 24-step grey ramp).
    pub fn indexed(&self, i: u8) -> Rgba {
        match i {
            0..=15 => self.ansi[i as usize],
            16..=231 => {
                const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
                let n = i - 16;
                rgb_to_rgba(Rgb { r: LEVELS[(n / 36) as usize], g: LEVELS[(n / 6 % 6) as usize], b: LEVELS[(n % 6) as usize] })
            }
            232..=255 => {
                let v = 8 + 10 * (i - 232);
                rgb_to_rgba(Rgb { r: v, g: v, b: v })
            }
        }
    }
}

fn rgb_to_rgba(c: Rgb) -> Rgba {
    (c.r as u32) << 24 | (c.g as u32) << 16 | (c.b as u32) << 8 | 0xff
}

fn rgba_to_rgb(c: Rgba) -> Rgb {
    Rgb { r: (c >> 24) as u8, g: (c >> 16) as u8, b: (c >> 8) as u8 }
}

/// `c` at two thirds of its brightness (alpha kept), like alacritty's dim colors.
fn dimmed(c: Rgba) -> Rgba {
    let ch = |shift: u32| (((c >> shift) & 0xff) * 2 / 3) << shift;
    ch(24) | ch(16) | ch(8) | (c & 0xff)
}

/// What a cell's character looks like: alacritty keeps the tab that moved
/// the cursor in the cell it started from, which shows as a blank.
fn shown_char(c: char) -> char {
    if c == '\t' { ' ' } else { c }
}

/// Cell attributes (inverse video is already applied to `fg`/`bg`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct CellFlags {
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
    pub strikethrough: bool,
    pub hidden: bool,
    /// The first half of a double-width character (the next cell is its spacer).
    pub wide: bool,
    /// The second half of a double-width character: draw nothing.
    pub wide_spacer: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Cell {
    pub ch: char,
    /// Combining characters drawn on top of `ch`.
    pub zerowidth: Vec<char>,
    pub fg: Rgba,
    pub bg: Rgba,
    pub flags: CellFlags,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub enum CursorShape {
    #[default]
    Block,
    Underline,
    Beam,
    HollowBlock,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CursorState {
    /// Row within the visible rows (0 = top), column in cells.
    pub row: usize,
    pub col: usize,
    pub shape: CursorShape,
    /// Hidden by the program (DECTCEM) or scrolled out of view.
    pub visible: bool,
}

/// What to draw: the visible rows (taking the scrollback position into account).
#[derive(Clone, Debug, PartialEq)]
pub struct Snapshot {
    pub cols: usize,
    pub rows: usize,
    /// `rows` rows of `cols` cells each.
    pub lines: Vec<Vec<Cell>>,
    pub cursor: CursorState,
    /// How many lines the view is scrolled back (0 = following output).
    pub display_offset: usize,
    pub background: Rgba,
}

/// Consecutive cells of a row drawn with one style: what a renderer places
/// at column `col`, `width` cells wide.
#[derive(Clone, Debug, PartialEq)]
pub struct Run {
    pub col: usize,
    /// Width in cells (a wide character is a run of its own, 2 cells wide,
    /// so a renderer can place it exactly whatever the font's glyph width).
    pub width: usize,
    /// The characters, combining characters included.
    pub text: String,
    pub fg: Rgba,
    pub bg: Rgba,
    /// Style flags (`wide`/`wide_spacer` cleared: `width` says it).
    pub flags: CellFlags,
}

impl Run {
    /// Nothing to draw: spaces on the terminal's background.
    pub fn is_blank(&self, background: Rgba) -> bool {
        self.bg == background && self.text.chars().all(|c| c == ' ') && !self.flags.underline && !self.flags.strikethrough
    }
}

impl Snapshot {
    /// Row `row` split into runs of same-styled cells, left to right,
    /// covering every column exactly once.
    pub fn row_runs(&self, row: usize) -> Vec<Run> {
        let Some(cells) = self.lines.get(row) else { return Vec::new() };
        let mut runs: Vec<Run> = Vec::new();
        // Whether the last run is a wide character (nothing joins it).
        let mut last_wide = false;
        for (col, c) in cells.iter().enumerate() {
            let after_wide = col > 0 && cells[col - 1].flags.wide;
            if c.flags.wide_spacer && after_wide {
                continue; // covered by the wide character's run
            }
            let flags = CellFlags { wide: false, wide_spacer: false, ..c.flags };
            // A spacer that isn't behind a wide character pads a line whose
            // wide character didn't fit: it draws as a blank.
            let ch = if c.flags.wide_spacer { ' ' } else { c.ch };
            let wide = c.flags.wide && col + 1 < cells.len();
            match runs.last_mut() {
                Some(r) if !wide && !last_wide && r.fg == c.fg && r.bg == c.bg && r.flags == flags => {
                    r.width += 1;
                    r.text.push(ch);
                    r.text.extend(&c.zerowidth);
                }
                _ => {
                    let mut text = String::from(ch);
                    text.extend(&c.zerowidth);
                    runs.push(Run { col, width: if wide { 2 } else { 1 }, text, fg: c.fg, bg: c.bg, flags });
                }
            }
            last_wide = wide;
        }
        runs
    }

    /// Row `row` as text (wide-character spacers skipped, combining
    /// characters kept, trailing blanks trimmed). For tests and debugging.
    pub fn row_text(&self, row: usize) -> String {
        let mut s = String::new();
        for c in self.lines.get(row).into_iter().flatten().filter(|c| !c.flags.wide_spacer) {
            s.push(c.ch);
            s.extend(&c.zerowidth);
        }
        s.trim_end().to_string()
    }
}

/// Terminal modes that change how input is encoded.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Modes {
    /// DECCKM: arrows send `ESC O A` instead of `ESC [ A`.
    pub app_cursor: bool,
    /// DECKPAM: keypad application mode.
    pub app_keypad: bool,
    /// Paste is wrapped in `ESC [200~` ... `ESC [201~`.
    pub bracketed_paste: bool,
    /// The alternate screen is active (full-screen programs).
    pub alt_screen: bool,
    /// Some mouse reporting mode is on.
    pub mouse: bool,
}

/// What feeding output produced.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Feed {
    /// Bytes to write back to the process (replies to DSR, DA, ...).
    pub replies: Vec<u8>,
    /// The title changed: `Some(title)` (`""` when reset).
    pub title: Option<String>,
    /// The bell rang.
    pub bell: bool,
}

/// Collects what `Term` reports while parsing; drained after each feed.
#[derive(Clone, Default)]
struct Listener(Arc<Mutex<Vec<TermEvent>>>);

impl EventListener for Listener {
    fn send_event(&self, event: TermEvent) {
        // Only the events a headless terminal answers or reports.
        if matches!(
            event,
            TermEvent::PtyWrite(_)
                | TermEvent::Title(_)
                | TermEvent::ResetTitle
                | TermEvent::Bell
                | TermEvent::ColorRequest(..)
                | TermEvent::TextAreaSizeRequest(_)
        ) {
            self.0.lock().unwrap_or_else(|e| e.into_inner()).push(event);
        }
    }
}

impl Listener {
    fn take(&self) -> Vec<TermEvent> {
        std::mem::take(&mut *self.0.lock().unwrap_or_else(|e| e.into_inner()))
    }
}

/// Synchronized updates (mode 2026) turned off: the parser would hold output
/// back until the program ends the update or a timeout passes, and nothing
/// would redraw a headless terminal when that timeout passes. The editor
/// drains all waiting output before drawing anyway.
#[derive(Default)]
struct NoSync;

impl Timeout for NoSync {
    fn set_timeout(&mut self, _: Duration) {}
    fn clear_timeout(&mut self) {}
    fn pending_timeout(&self) -> bool {
        false
    }
}

/// Grid dimensions for `Term::new`/`Term::resize`.
struct Size {
    cols: usize,
    rows: usize,
}

impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        self.rows
    }
    fn screen_lines(&self) -> usize {
        self.rows
    }
    fn columns(&self) -> usize {
        self.cols
    }
}

fn clamp_size(cols: u16, rows: u16) -> Size {
    Size { cols: cols.clamp(MIN_COLS, MAX_COLS) as usize, rows: rows.clamp(MIN_ROWS, MAX_ROWS) as usize }
}

/// A terminal screen with scrollback.
pub struct Terminal {
    term: Term<Listener>,
    parser: Processor<NoSync>,
    listener: Listener,
    scrollback: usize,
    title: String,
    /// The palette last drawn with: color queries (OSC 4/10/11) are answered
    /// with what the user actually sees.
    palette: RefCell<Palette>,
}

impl Terminal {
    /// A blank `cols`×`rows` terminal keeping up to `scrollback` lines of history.
    /// Sizes are clamped to 2–4096 columns and 1–2048 rows.
    pub fn new(cols: u16, rows: u16, scrollback: usize) -> Self {
        let listener = Listener::default();
        let config = TermConfig { scrolling_history: scrollback, ..TermConfig::default() };
        let term = Term::new(config, &clamp_size(cols, rows), listener.clone());
        Self {
            term,
            parser: Processor::new(),
            listener,
            scrollback,
            title: String::new(),
            palette: RefCell::new(Palette::xterm()),
        }
    }

    /// Process output from the program. Escape sequences and UTF-8
    /// characters may be split across calls.
    pub fn feed(&mut self, bytes: &[u8]) -> Feed {
        self.parser.advance(&mut self.term, bytes);
        let mut feed = Feed::default();
        for event in self.listener.take() {
            match event {
                TermEvent::PtyWrite(s) => feed.replies.extend_from_slice(s.as_bytes()),
                TermEvent::Title(t) => feed.title = Some(t),
                TermEvent::ResetTitle => feed.title = Some(String::new()),
                TermEvent::Bell => feed.bell = true,
                TermEvent::ColorRequest(index, format) => {
                    let color = self.color_for_query(index);
                    feed.replies.extend_from_slice(format(color).as_bytes());
                }
                TermEvent::TextAreaSizeRequest(format) => {
                    let (cols, rows) = self.size();
                    let size = WindowSize { num_lines: rows, num_cols: cols, cell_width: 0, cell_height: 0 };
                    feed.replies.extend_from_slice(format(size).as_bytes());
                }
                _ => {}
            }
        }
        if let Some(t) = &feed.title {
            self.title = t.clone();
        }
        feed
    }

    /// The color a program asking for color `index` (OSC 4/10/11/12) gets.
    fn color_for_query(&self, index: usize) -> Rgb {
        if let Some(c) = self.term.colors()[index.min(alacritty_terminal::term::color::COUNT - 1)] {
            return c;
        }
        rgba_to_rgb(self.base_color(index, &self.palette.borrow()))
    }

    /// Resize to `cols`×`rows` (clamped like [`Terminal::new`]). The primary
    /// screen reflows wrapped lines; the alternate screen is cut or padded.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.term.resize(clamp_size(cols, rows));
    }

    /// (cols, rows).
    pub fn size(&self) -> (u16, u16) {
        let g = self.term.grid();
        (g.columns() as u16, g.screen_lines() as u16)
    }

    pub fn scrollback_limit(&self) -> usize {
        self.scrollback
    }

    /// Lines of history currently kept above the screen.
    pub fn history_len(&self) -> usize {
        self.term.grid().history_size()
    }

    /// The last title the program set (`""` if none).
    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn modes(&self) -> Modes {
        let m = *self.term.mode();
        Modes {
            app_cursor: m.contains(TermMode::APP_CURSOR),
            app_keypad: m.contains(TermMode::APP_KEYPAD),
            bracketed_paste: m.contains(TermMode::BRACKETED_PASTE),
            alt_screen: m.contains(TermMode::ALT_SCREEN),
            mouse: m.intersects(TermMode::MOUSE_MODE),
        }
    }

    /// Scroll the view `lines` into the history (negative: back toward the
    /// bottom). Clamped to the history there is.
    pub fn scroll(&mut self, lines: i32) {
        self.term.scroll_display(Scroll::Delta(lines));
    }

    pub fn scroll_to_bottom(&mut self) {
        self.term.scroll_display(Scroll::Bottom);
    }

    pub fn display_offset(&self) -> usize {
        self.term.grid().display_offset()
    }

    /// What to draw, with colors resolved through `palette`: inverse video
    /// applied, bold draws colors 0–7 bright, 256-color cube and greys,
    /// truecolor as is, colors the program redefined (OSC 4/10/11) win.
    pub fn snapshot(&self, palette: &Palette) -> Snapshot {
        if *self.palette.borrow() != *palette {
            *self.palette.borrow_mut() = palette.clone();
        }
        let grid = self.term.grid();
        let (cols, rows) = (grid.columns(), grid.screen_lines());
        let offset = grid.display_offset();
        let lines = (0..rows)
            .map(|r| {
                let row = &grid[Line(r as i32 - offset as i32)];
                (0..cols).map(|c| self.cell(&row[Column(c)], palette)).collect()
            })
            .collect();
        Snapshot {
            cols,
            rows,
            lines,
            cursor: self.cursor_state(),
            display_offset: offset,
            background: self.base_color(Self::BACKGROUND, palette),
        }
    }

    const FOREGROUND: usize = 256;
    const BACKGROUND: usize = 257;
    const CURSOR: usize = 258;
    const DIM_BLACK: usize = 259;
    const BRIGHT_FOREGROUND: usize = 267;
    const DIM_FOREGROUND: usize = 268;

    /// The color at alacritty color index `i` (0–255 indexed, then the
    /// named foreground/background/cursor/dim colors): the program's
    /// override if it set one, else the palette's.
    fn base_color(&self, i: usize, p: &Palette) -> Rgba {
        if let Some(c) = self.term.colors()[i.min(alacritty_terminal::term::color::COUNT - 1)] {
            return rgb_to_rgba(c);
        }
        match i {
            0..=255 => p.indexed(i as u8),
            Self::FOREGROUND | Self::BRIGHT_FOREGROUND => p.foreground,
            Self::BACKGROUND => p.background,
            Self::CURSOR => p.cursor,
            Self::DIM_FOREGROUND => dimmed(p.foreground),
            _ if (Self::DIM_BLACK..Self::DIM_BLACK + 8).contains(&i) => dimmed(p.ansi[i - Self::DIM_BLACK]),
            _ => p.foreground,
        }
    }

    /// The color index a cell color refers to (bold-as-bright for 0–7), or
    /// the RGB value of a truecolor cell.
    fn color(&self, c: Color, bold: bool, p: &Palette) -> Rgba {
        let index = match c {
            Color::Spec(rgb) => return rgb_to_rgba(rgb),
            Color::Named(n) => n as usize,
            Color::Indexed(i) => i as usize,
        };
        let index = if bold && index < 8 { index + 8 } else { index };
        self.base_color(index, p)
    }

    fn cell(&self, c: &TermCell, p: &Palette) -> Cell {
        let f = c.flags;
        let bold = f.contains(Flags::BOLD);
        let dim = f.contains(Flags::DIM);
        let mut fg = self.color(c.fg, bold && !dim, p);
        if dim {
            fg = dimmed(fg);
        }
        let mut bg = self.color(c.bg, false, p);
        if f.contains(Flags::INVERSE) {
            std::mem::swap(&mut fg, &mut bg);
        }
        let hidden = f.contains(Flags::HIDDEN);
        if hidden {
            fg = bg;
        }
        Cell {
            ch: shown_char(c.c),
            zerowidth: c.zerowidth().map(<[char]>::to_vec).unwrap_or_default(),
            fg,
            bg,
            flags: CellFlags {
                bold,
                dim,
                italic: f.contains(Flags::ITALIC),
                underline: f.intersects(Flags::ALL_UNDERLINES),
                strikethrough: f.contains(Flags::STRIKEOUT),
                hidden,
                wide: f.contains(Flags::WIDE_CHAR),
                wide_spacer: f.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER),
            },
        }
    }

    fn cursor_state(&self) -> CursorState {
        let grid = self.term.grid();
        let point = grid.cursor.point;
        let mut col = point.column.0.min(grid.columns() - 1);
        if col > 0 && grid[point.line][Column(col)].flags.contains(Flags::WIDE_CHAR_SPACER) {
            col -= 1;
        }
        let row = point.line.0 + grid.display_offset() as i32;
        let in_view = (0..grid.screen_lines() as i32).contains(&row);
        let shape = match self.term.cursor_style().shape {
            VteCursorShape::Block => CursorShape::Block,
            VteCursorShape::Underline => CursorShape::Underline,
            VteCursorShape::Beam => CursorShape::Beam,
            VteCursorShape::HollowBlock => CursorShape::HollowBlock,
            VteCursorShape::Hidden => CursorShape::Block,
        };
        let shown = self.term.mode().contains(TermMode::SHOW_CURSOR) && self.term.cursor_style().shape != VteCursorShape::Hidden;
        CursorState { row: row.max(0) as usize, col, shape, visible: shown && in_view }
    }

    /// Scrollback and screen as text (trailing blanks trimmed, wrapped lines
    /// joined), and the cursor's (line, column) in it — for copy mode.
    /// Columns count characters (combining characters included); the cursor
    /// column is clamped to its line's trimmed length.
    pub fn text(&self) -> (String, (usize, usize)) {
        let grid = self.term.grid();
        let cursor = grid.cursor.point;
        let mut lines: Vec<String> = Vec::new();
        let mut current = String::new();
        let mut cursor_pos = (0, 0);
        let top = -(grid.history_size() as i32);
        for l in top..grid.screen_lines() as i32 {
            let row = &grid[Line(l)];
            let mut wrapped = false;
            for c in 0..grid.columns() {
                let cell = &row[Column(c)];
                if l == cursor.line.0 && c == cursor.column.0 {
                    cursor_pos = (lines.len(), current.chars().count());
                }
                if c == grid.columns() - 1 && cell.flags.contains(Flags::WRAPLINE) {
                    wrapped = true;
                }
                if cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
                    continue;
                }
                current.push(shown_char(cell.c));
                current.extend(cell.zerowidth().into_iter().flatten());
            }
            if !wrapped {
                lines.push(std::mem::take(&mut current).trim_end().to_string());
            }
        }
        if !current.is_empty() {
            lines.push(current.trim_end().to_string());
        }
        // Trailing empty lines go, except up to the cursor's.
        while lines.len() > cursor_pos.0 + 1 && lines.last().is_some_and(String::is_empty) {
            lines.pop();
        }
        let len = lines.get(cursor_pos.0).map_or(0, |s| s.chars().count());
        cursor_pos.1 = cursor_pos.1.min(len);
        (lines.join("\n"), cursor_pos)
    }
}
