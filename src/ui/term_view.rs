//! Terminal buffers on screen: draws a terminal [`Snapshot`] as a grid of
//! cells, and works out how many cells fit in a window.
//!
//! Each row is split into runs of same-styled cells (`Snapshot::row_runs`)
//! and every run is placed at its column, so the grid stays aligned whatever
//! the font does with wide or missing glyphs.

use std::cell::Cell as StdCell;

use gpui::{AnyElement, FontWeight, SharedString, Window, div, font, prelude::*, px, rgba};

use emax::engine::term::{CursorShape, Run, Snapshot};

thread_local! {
    /// The cell width last computed by `cell_width`/`measure_cell_width`:
    /// `render_terminal` lays out with the same width `grid_size` used.
    static CELL_WIDTH: StdCell<Option<f32>> = const { StdCell::new(None) };
}

fn remember(w: f32) -> f32 {
    CELL_WIDTH.with(|c| c.set(Some(w)));
    w
}

/// Width of a cell for a monospaced font of `font_size` pixels (an
/// estimate; see [`measure_cell_width`] for the real advance).
pub fn cell_width(font_size: f32) -> f32 {
    remember((font_size * 0.6).max(1.0))
}

/// Width of a cell in `family` at `font_size` pixels: the advance of `m`
/// as the text system lays it out (falls back to the estimate).
#[allow(dead_code)] // for the per-window terminal view
pub fn measure_cell_width(window: &Window, family: impl Into<SharedString>, font_size: f32) -> f32 {
    let ts = window.text_system();
    let id = ts.resolve_font(&font(family));
    match ts.advance(id, px(font_size), 'm') {
        Ok(size) if f32::from(size.width) >= 1.0 => remember(f32::from(size.width)),
        _ => cell_width(font_size),
    }
}

/// How many (cols, rows) cells fit in `width`×`height` pixels.
pub fn grid_size(width: f32, height: f32, cell_w: f32, line_h: f32) -> (u16, u16) {
    let cols = (width / cell_w.max(1.0)).floor().clamp(1.0, u16::MAX as f32) as u16;
    let rows = (height / line_h.max(1.0)).floor().clamp(1.0, u16::MAX as f32) as u16;
    (cols, rows)
}

/// The element showing `snap`, one `line_h`-pixel row per terminal row,
/// with cells as wide as the last [`cell_width`]/[`measure_cell_width`].
pub fn render_terminal(snap: &Snapshot, line_h: f32) -> AnyElement {
    let cell_w = CELL_WIDTH.with(StdCell::get).unwrap_or(line_h * 0.5);
    render_terminal_with(snap, cell_w, line_h)
}

/// The element showing `snap` with `cell_w`×`line_h`-pixel cells.
pub fn render_terminal_with(snap: &Snapshot, cell_w: f32, line_h: f32) -> AnyElement {
    let rows = (0..snap.rows).map(|r| {
        let runs = snap.row_runs(r).into_iter().filter(|run| !run.is_blank(snap.background));
        div()
            .relative()
            .flex_none()
            .w_full()
            .h(px(line_h))
            .children(runs.map(|run| render_run(run, cell_w, line_h)))
    });
    div()
        .relative()
        .size_full()
        .overflow_hidden()
        .bg(rgba(snap.background))
        .whitespace_nowrap()
        .flex()
        .flex_col()
        .children(rows)
        .children(render_cursor(snap, cell_w, line_h))
        .into_any_element()
}

fn render_run(run: Run, cell_w: f32, line_h: f32) -> impl IntoElement {
    let f = run.flags;
    div()
        .absolute()
        .left(px(run.col as f32 * cell_w))
        .top_0()
        .w(px(run.width as f32 * cell_w))
        .h(px(line_h))
        .overflow_hidden()
        .bg(rgba(run.bg))
        .text_color(rgba(run.fg))
        .when(f.bold, |d| d.font_weight(FontWeight::BOLD))
        .when(f.italic, |d| d.italic())
        .when(f.underline, |d| d.underline())
        .when(f.strikethrough, |d| d.line_through())
        .child(run.text)
}

/// The cursor drawn over its cell: a block shows the character in reverse
/// (the cell's colors swapped), the other shapes are outlines and bars in
/// the cell's foreground.
fn render_cursor(snap: &Snapshot, cell_w: f32, line_h: f32) -> Option<AnyElement> {
    let c = snap.cursor;
    let cell = snap.lines.get(c.row)?.get(c.col)?;
    if !c.visible {
        return None;
    }
    let cells = if cell.flags.wide { 2.0 } else { 1.0 };
    let base = div().absolute().left(px(c.col as f32 * cell_w)).top(px(c.row as f32 * line_h));
    let (w, h) = (cells * cell_w, line_h);
    let el = match c.shape {
        CursorShape::Block => {
            let mut text: String = cell.ch.to_string();
            text.extend(&cell.zerowidth);
            base.w(px(w)).h(px(h)).bg(rgba(cell.fg)).text_color(rgba(cell.bg)).child(text)
        }
        CursorShape::HollowBlock => base.w(px(w)).h(px(h)).border_1().border_color(rgba(cell.fg)),
        CursorShape::Beam => base.w(px(2.0)).h(px(h)).bg(rgba(cell.fg)),
        CursorShape::Underline => base.top(px(c.row as f32 * line_h + h - 2.0)).w(px(w)).h(px(2.0)).bg(rgba(cell.fg)),
    };
    Some(el.into_any_element())
}
