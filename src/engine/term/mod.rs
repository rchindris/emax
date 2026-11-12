//! Terminal emulation: a wrapper around `alacritty_terminal` that turns the
//! bytes a program prints into a grid of cells, and keys into the bytes the
//! program expects.
//!
//! [`Terminal`] is plain data (no threads, no I/O): the caller feeds it
//! output with [`Terminal::feed`] and writes [`Feed::replies`] (answers to
//! queries such as DSR/DA) back to the process. Rendering reads a
//! [`Snapshot`] with colors already resolved through a [`Palette`] built from
//! the theme.
//!
//! SKELETON: owned by the term builder (see docs/design/terminal-substrate.md).
//! The types and signatures here are the contract; the bodies are minimal
//! (a blank screen that ignores output).

pub mod buffer;
pub mod input;

use serde::Serialize;

use super::theme::{Rgba, Theme};

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

/// A terminal screen with scrollback.
pub struct Terminal {
    cols: usize,
    rows: usize,
    scrollback: usize,
    title: String,
}

impl Terminal {
    /// A blank `cols`×`rows` terminal keeping up to `scrollback` lines of history.
    pub fn new(cols: u16, rows: u16, scrollback: usize) -> Self {
        Self { cols: cols.max(1) as usize, rows: rows.max(1) as usize, scrollback, title: String::new() }
    }

    /// Process output from the program.
    pub fn feed(&mut self, _bytes: &[u8]) -> Feed {
        Feed::default()
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.cols = cols.max(1) as usize;
        self.rows = rows.max(1) as usize;
    }

    /// (cols, rows).
    pub fn size(&self) -> (u16, u16) {
        (self.cols as u16, self.rows as u16)
    }

    pub fn scrollback_limit(&self) -> usize {
        self.scrollback
    }

    /// The last title the program set (`""` if none).
    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn modes(&self) -> Modes {
        Modes::default()
    }

    /// Scroll the view `lines` into the history (negative: back toward the bottom).
    pub fn scroll(&mut self, _lines: i32) {}

    pub fn scroll_to_bottom(&mut self) {}

    pub fn display_offset(&self) -> usize {
        0
    }

    pub fn snapshot(&self, palette: &Palette) -> Snapshot {
        let blank = Cell { ch: ' ', zerowidth: Vec::new(), fg: palette.foreground, bg: palette.background, flags: CellFlags::default() };
        Snapshot {
            cols: self.cols,
            rows: self.rows,
            lines: vec![vec![blank; self.cols]; self.rows],
            cursor: CursorState { row: 0, col: 0, shape: CursorShape::Block, visible: true },
            display_offset: 0,
            background: palette.background,
        }
    }

    /// Scrollback and screen as text (trailing blanks trimmed, wrapped lines
    /// joined), and the cursor's (line, column) in it — for copy mode.
    pub fn text(&self) -> (String, (usize, usize)) {
        (String::new(), (0, 0))
    }
}
