//! Terminal buffers on screen: draws a terminal [`Snapshot`] as a grid of
//! cells, and works out how many cells fit in a window.
//!
//! SKELETON: owned by the term builder (see docs/design/terminal-substrate.md).
//! It draws each row as plain text in the default colors for now.

use gpui::{AnyElement, div, prelude::*, px, rgba};

use emax::engine::term::Snapshot;

/// Width of a cell for a monospaced font of `font_size` pixels (an
/// estimate; the term builder may measure the real advance).
pub fn cell_width(font_size: f32) -> f32 {
    (font_size * 0.6).max(1.0)
}

/// How many (cols, rows) cells fit in `width`×`height` pixels.
pub fn grid_size(width: f32, height: f32, cell_w: f32, line_h: f32) -> (u16, u16) {
    let cols = (width / cell_w.max(1.0)).floor().clamp(1.0, u16::MAX as f32) as u16;
    let rows = (height / line_h.max(1.0)).floor().clamp(1.0, u16::MAX as f32) as u16;
    (cols, rows)
}

/// The element showing `snap`, one `line_h`-pixel row per terminal row.
pub fn render_terminal(snap: &Snapshot, line_h: f32) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .size_full()
        .overflow_hidden()
        .bg(rgba(snap.background))
        .children(snap.lines.iter().map(|row| {
            let text: String = row.iter().filter(|c| !c.flags.wide_spacer).map(|c| c.ch).collect();
            let fg = row.first().map_or(0xffffffff, |c| c.fg);
            div().h(px(line_h)).whitespace_nowrap().text_color(rgba(fg)).child(text)
        }))
        .into_any_element()
}
