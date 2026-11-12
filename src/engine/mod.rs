//! The editing engine: everything that does not depend on the GUI.

pub mod buffer;
pub mod buffers;
pub mod commands;
pub mod config;
pub mod cursor;
pub mod dired;
pub mod editor;
pub mod files;
pub mod hooks;
pub mod keymap;
pub mod modes;
pub mod picker;
pub mod pty;
pub mod search;
pub mod session;
pub mod settings;
pub mod syntax;
pub mod term;
pub mod text;
pub mod theme;
pub mod window;

#[cfg(test)]
mod e2e_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
pub(crate) mod testutil;

pub use buffer::{Buffer, BufferId, BufferKind};
pub use buffers::BufView;
pub use config::{Appearance, Config, Keymaps, PtyBackendKind, SessionConfig, Settings, TerminalConfig};
pub use editor::{Args, Editor, RawArg, Scroll};
pub use hooks::{Event, EventKind, HookFn, Hooks, TimerId};
pub use keymap::{Dispatcher, Key, Keymap, Outcome, where_is};
pub use modes::{BufferModes, MinorMode, Modes};
pub use search::{Swiper, SwiperExit};
pub use settings::SettingsEditor;
pub use theme::{Fonts, Theme, Themes};
pub use window::{Layout, Node, SplitDir, WindowId};
