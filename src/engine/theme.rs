//! Themes and fonts: how the editor looks.
//!
//! A theme is a set of named colors, written in the config as `#rrggbb` or
//! `#rrggbbaa`. Themes live under `themes` and the active one is picked by
//! `theme`, either one name or a `{ "light": …, "dark": … }` pair that
//! follows the system appearance. A theme may `inherit` another one and set
//! only the colors it changes.
//!
//! Syntax highlighting colors are keys `syntax.<capture>`, named after
//! tree-sitter captures (`syntax.keyword`, `syntax.function.method`…). They
//! are optional: a capture with no color of its own takes the color of a
//! shorter name (`function`), and text with none keeps `foreground`.
//!
//! This module has no GUI dependencies: colors are plain `0xRRGGBBAA` values.

use std::collections::BTreeMap;

/// A color as `0xRRGGBBAA`.
pub type Rgba = u32;

/// The theme used when the configured one can't be found, and the one an
/// incomplete theme without `inherits` takes its missing colors from.
pub const DEFAULT_THEME: &str = "emax-dark";

/// The key in a theme that names the theme it builds on.
const INHERITS: &str = "inherits";

/// Theme keys starting with this set syntax highlighting colors.
const SYNTAX_PREFIX: &str = "syntax.";

macro_rules! theme_colors {
    ($( $(#[doc = $doc:literal])* $field:ident, )*) => {
        /// Every color the UI draws with.
        #[derive(Clone, Debug, PartialEq)]
        pub struct Theme {
            /// The theme's name in the config (`emax-dark`, …).
            pub name: String,
            $( $(#[doc = $doc])* pub $field: Rgba, )*
            /// Syntax highlighting colors by capture name (`keyword`,
            /// `function.method`…), set as `syntax.<name>` (see `syntax_color`).
            pub syntax: BTreeMap<String, Rgba>,
        }

        impl Theme {
            /// The color names a theme can set, as written in the config.
            pub const COLOR_NAMES: &[&str] = &[$(stringify!($field)),*];

            /// A theme defining every color itself (no inheritance).
            pub(crate) fn complete(name: &str, entries: &BTreeMap<String, String>) -> Result<Self, String> {
                let color = |name: &str| {
                    let v = entries.get(name).ok_or_else(|| format!("no color for {name}"))?;
                    parse_color(v).ok_or_else(|| format!("{name}: \"{v}\" is not a color"))
                };
                let mut syntax = BTreeMap::new();
                for (key, v) in entries {
                    if let Some(name) = key.strip_prefix(SYNTAX_PREFIX) {
                        let c = parse_color(v).ok_or_else(|| format!("{key}: \"{v}\" is not a color"))?;
                        syntax.insert(name.to_string(), c);
                    }
                }
                Ok(Self { name: name.to_string(), $( $field: color(stringify!($field))?, )* syntax })
            }

            fn slot(&mut self, name: &str) -> Option<&mut Rgba> {
                match name {
                    $( stringify!($field) => Some(&mut self.$field), )*
                    _ => None,
                }
            }
        }
    };
}

theme_colors! {
    /// Buffer background; also the text color on cursors and the current match.
    background,
    /// Normal text.
    foreground,
    /// Text that stands out from `foreground`: selected rows, the typed query.
    emphasis,
    /// Secondary text: line numbers, hints, annotations.
    dim,
    /// Accents: prompts, the modified marker.
    accent,
    /// Background of the line holding the cursor.
    current_line,
    /// The primary cursor.
    cursor,
    /// The other cursors (multiple cursors), and modified-buffer markers.
    secondary_cursor,
    /// The active region.
    region,
    /// Every search match in the buffer.
    search_match,
    /// The selected search match.
    search_current,
    /// Matched characters in picker and search candidates.
    match_text,
    /// The mode line.
    modeline,
    /// The mode line while another buffer is previewed.
    modeline_preview,
    /// The picker and search panels.
    panel,
    /// Borders of the panels.
    panel_border,
    /// The selected row in a panel.
    row_selected,
    /// A row under the mouse.
    row_hover,
    /// Small labels, like "current" in the buffer picker.
    badge,
    /// The veil drawn over the editor behind the buffer picker (use an alpha).
    overlay,
    /// Terminal ANSI color 0 (black).
    term_black,
    /// Terminal ANSI color 1 (red).
    term_red,
    /// Terminal ANSI color 2 (green).
    term_green,
    /// Terminal ANSI color 3 (yellow).
    term_yellow,
    /// Terminal ANSI color 4 (blue).
    term_blue,
    /// Terminal ANSI color 5 (magenta).
    term_magenta,
    /// Terminal ANSI color 6 (cyan).
    term_cyan,
    /// Terminal ANSI color 7 (white).
    term_white,
    /// Terminal ANSI color 8 (bright black).
    term_bright_black,
    /// Terminal ANSI color 9 (bright red).
    term_bright_red,
    /// Terminal ANSI color 10 (bright green).
    term_bright_green,
    /// Terminal ANSI color 11 (bright yellow).
    term_bright_yellow,
    /// Terminal ANSI color 12 (bright blue).
    term_bright_blue,
    /// Terminal ANSI color 13 (bright magenta).
    term_bright_magenta,
    /// Terminal ANSI color 14 (bright cyan).
    term_bright_cyan,
    /// Terminal ANSI color 15 (bright white).
    term_bright_white,
}

impl Theme {
    /// The color for a syntax capture: the most specific one the theme
    /// sets, dropping trailing parts (`function.method` -> `function`).
    pub fn syntax_color(&self, capture: &str) -> Option<Rgba> {
        let mut name = capture;
        loop {
            if let Some(c) = self.syntax.get(name) {
                return Some(*c);
            }
            name = &name[..name.rfind('.')?];
        }
    }
}

/// Parse `#rgb`, `#rrggbb` or `#rrggbbaa` (the `#` is optional).
pub fn parse_color(s: &str) -> Option<Rgba> {
    let hex = s.strip_prefix('#').unwrap_or(s);
    if !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let v = u32::from_str_radix(hex, 16).ok();
    match hex.len() {
        3 => {
            let v = v?;
            let (r, g, b) = ((v >> 8) & 0xf, (v >> 4) & 0xf, v & 0xf);
            Some((r * 0x11) << 24 | (g * 0x11) << 16 | (b * 0x11) << 8 | 0xff)
        }
        6 => Some(v? << 8 | 0xff),
        8 => v,
        _ => None,
    }
}

/// Raw themes as merged from the config layers: name -> key -> value.
pub(crate) type RawThemes = BTreeMap<String, BTreeMap<String, String>>;

/// Build the theme `name` from the merged raw themes. Problems are reported
/// in `warnings`; bad entries are skipped. `None` if there is no such theme
/// (or it can't be built at all, e.g. an inheritance cycle).
pub(crate) fn resolve(name: &str, raw: &RawThemes, fallback: &Theme, warnings: &mut Vec<String>) -> Option<Theme> {
    resolve_inner(name, raw, fallback, warnings, &mut Vec::new())
}

fn resolve_inner(
    name: &str,
    raw: &RawThemes,
    fallback: &Theme,
    warnings: &mut Vec<String>,
    chain: &mut Vec<String>,
) -> Option<Theme> {
    let entries = raw.get(name)?;
    if chain.iter().any(|n| n == name) {
        chain.push(name.to_string());
        warnings.push(format!("themes: inheritance cycle {}", chain.join(" -> ")));
        return None;
    }
    chain.push(name.to_string());

    let parent = entries.get(INHERITS);
    let mut theme = match parent {
        Some(p) => match resolve_inner(p, raw, fallback, warnings, chain) {
            Some(t) => t,
            None => {
                if !raw.contains_key(p) {
                    warnings.push(format!("themes.{name}: inherits unknown theme \"{p}\""));
                }
                fallback.clone()
            }
        },
        None => fallback.clone(),
    };
    chain.pop();
    theme.name = name.to_string();

    let mut missing: Vec<&str> = Theme::COLOR_NAMES.to_vec();
    for (key, value) in entries {
        if key == INHERITS {
            continue;
        }
        if let Some(capture) = key.strip_prefix(SYNTAX_PREFIX).filter(|c| !c.is_empty()) {
            match parse_color(value) {
                Some(c) => {
                    theme.syntax.insert(capture.to_string(), c);
                }
                None => {
                    warnings.push(format!("themes.{name}.{key}: \"{value}\" is not a color (use #rrggbb or #rrggbbaa)"))
                }
            }
            continue;
        }
        let Some(slot) = theme.slot(key) else {
            warnings.push(format!("themes.{name}: unknown color \"{key}\""));
            continue;
        };
        match parse_color(value) {
            Some(c) => {
                *slot = c;
                missing.retain(|m| m != key);
            }
            None => {
                warnings.push(format!("themes.{name}.{key}: \"{value}\" is not a color (use #rrggbb or #rrggbbaa)"))
            }
        }
    }
    if parent.is_none() && !missing.is_empty() {
        warnings.push(format!(
            "themes.{name}: no color for {} (taken from {DEFAULT_THEME}; set \"inherits\" to build on another theme)",
            missing.join(", ")
        ));
    }
    Some(theme)
}

/// Build every theme in `raw`, sorted by name. Each one's problems are
/// reported once; a theme that can't be built at all is left out.
pub(crate) fn resolve_all(raw: &RawThemes, fallback: &Theme, warnings: &mut Vec<String>) -> Vec<Theme> {
    raw.keys().filter_map(|name| resolve(name, raw, fallback, warnings)).collect()
}

/// The fonts the editor uses.
#[derive(Clone, Debug, PartialEq)]
pub struct Fonts {
    /// Font for buffer text, line numbers and the mode line.
    pub family: String,
    /// Font for the picker and the hints in panels.
    pub ui_family: String,
    /// Size of buffer text, in pixels.
    pub size: f32,
    /// The size the panels (picker, search, tab bar) are drawn for, in pixels.
    pub ui_size: f32,
    /// Height of a text line, in pixels (whole pixels keep scrolling exact).
    pub line_height: f32,
}

impl Fonts {
    /// `ui_size` defaults to `size`, so the panels follow the buffer text.
    pub fn new(family: &str, ui_family: &str, size: f32, ui_size: Option<f32>, line_height: f32) -> Self {
        Self {
            family: resolve_family(family),
            ui_family: resolve_family(ui_family),
            size,
            ui_size: ui_size.unwrap_or(size),
            line_height: (size * line_height).round().max(1.0),
        }
    }
}

/// The monospaced font `"monospace"` stands for on this platform.
pub const PLATFORM_MONOSPACE: &str = if cfg!(target_os = "macos") {
    "Menlo"
} else if cfg!(target_os = "windows") {
    "Consolas"
} else {
    "DejaVu Sans Mono"
};

/// Map the generic names `monospace` and `system-ui` to real font names.
fn resolve_family(name: &str) -> String {
    match name {
        "monospace" => PLATFORM_MONOSPACE.to_string(),
        // GPUI's name for the platform UI font.
        "system-ui" => ".SystemUIFont".to_string(),
        other => other.to_string(),
    }
}

/// The themes in use: one for a light system appearance, one for dark (the
/// same theme twice when the config names only one).
#[derive(Clone, Debug, PartialEq)]
pub struct Themes {
    pub light: Theme,
    pub dark: Theme,
}

impl Theme {
    /// Whether this is a dark theme (its background is darker than mid-grey).
    pub fn is_dark(&self) -> bool {
        let channel = |shift: u32| ((self.background >> shift) & 0xff) as f32 / 255.0;
        let luma = 0.2126 * channel(24) + 0.7152 * channel(16) + 0.0722 * channel(8);
        luma < 0.5
    }

    /// The 16 ANSI colors terminals draw with (0–7 normal, 8–15 bright).
    pub fn ansi(&self) -> [Rgba; 16] {
        [
            self.term_black,
            self.term_red,
            self.term_green,
            self.term_yellow,
            self.term_blue,
            self.term_magenta,
            self.term_cyan,
            self.term_white,
            self.term_bright_black,
            self.term_bright_red,
            self.term_bright_green,
            self.term_bright_yellow,
            self.term_bright_blue,
            self.term_bright_magenta,
            self.term_bright_cyan,
            self.term_bright_white,
        ]
    }
}

impl Themes {
    pub fn get(&self, dark: bool) -> &Theme {
        if dark { &self.dark } else { &self.light }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors() {
        assert_eq!(parse_color("#1e1f22"), Some(0x1e1f22ff));
        assert_eq!(parse_color("0b0c0f80"), Some(0x0b0c0f80));
        assert_eq!(parse_color("#fff"), Some(0xffffffff));
        assert_eq!(parse_color("#12345"), None);
        assert_eq!(parse_color("#gggggg"), None);
        assert_eq!(parse_color("+12345"), None);
    }

    #[test]
    fn generic_font_names() {
        let f = Fonts::new("monospace", "system-ui", 14.0, None, 1.5);
        assert_eq!(f.family, PLATFORM_MONOSPACE);
        assert_eq!(f.ui_family, ".SystemUIFont");
        assert_eq!((f.line_height, f.ui_size), (21.0, 14.0));
        let g = Fonts::new("Iosevka", "Inter", 13.0, Some(15.0), 1.4);
        assert_eq!((g.line_height, g.ui_size), (18.0, 15.0));
    }
}
