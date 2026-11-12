//! The editing engine: everything that does not depend on the GUI.

pub mod buffer;
pub mod buffers;
pub mod commands;
pub mod config;
pub mod cursor;
pub mod editor;
pub mod keymap;
pub mod picker;
pub mod search;
pub mod text;
pub mod theme;

#[cfg(test)]
mod tests;

pub use buffer::Buffer;
pub use buffers::BufView;
pub use config::{Appearance, Config, Keymaps, Settings};
pub use theme::{Fonts, Theme, Themes};
pub use editor::{Args, Editor, RawArg, Scroll};
pub use keymap::{Dispatcher, Key, Keymap, Outcome};
pub use search::{Swiper, SwiperExit};
