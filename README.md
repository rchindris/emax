# emax

A fast, modern Emacs-style text editor written in Rust.

emax keeps Emacs' editing model and key bindings, rendered on the GPU with
[GPUI](https://www.gpui.rs/) and backed by a [ropey](https://github.com/cessen/ropey)
rope. It is **not** an Emacs Lisp clone: the goal is the Emacs editing experience,
without the Emacs runtime.

> **Status:** early prototype. Expect missing features and breaking changes.

## Features

- **GPU-rendered UI.** Lines are virtualised, so large files stay responsive.
  The window has a mode line and an echo area.
- **Rope-backed buffers** with grouped undo/redo.
- **Emacs key bindings.** Prefix keys (`C-x …`), prefix arguments (`C-u`,
  `C-u 12`, `M-5`, `M--`), shift-selection and transient mark.
- **Structural motion** by word, sentence, paragraph and balanced expression (sexp).
- **Kill ring** with `yank-pop`, connected to the system clipboard.
- **Multiple cursors**, using the bindings from `multiple-cursors.el`. Every
  command runs at every cursor.
- **Swiper-style search** (`C-s`): every matching line is listed in a panel and
  previewed in the buffer as you move through it. Space-separated terms match
  in any order, with smart case.
- **Multiple buffers** with a fuzzy-filtering switcher (`C-x b`).
- **Major and minor modes.** Every buffer has one major mode (`rust-mode`,
  `markdown-mode`, …), chosen from a `-*- mode -*-` marker, the `#!` line or
  the file name, plus any number of minor modes (`auto-fill-mode`,
  `read-only-mode`, `overwrite-mode`, `electric-pair-mode`). Major modes are
  plain data in the configuration, so you can add your own.
- **Commands use their GNU Emacs names** (`forward-sexp`, `kill-ring-save`, …).
  `M-x` runs any of them by name, with fuzzy matching.

## Building and running

emax needs a recent stable Rust toolchain (edition 2024, so Rust 1.85 or newer).

```sh
cargo run --release               # opens a *scratch* buffer with a key cheat sheet
cargo run --release -- path/file  # opens a file
```

Dependencies are optimised even in debug builds, so plain `cargo run` is also
fast enough for daily use while you work on the editor.

Run the tests with:

```sh
cargo test
```

## Keys

Option is **Meta** (`M-`) and Cmd is **Super** (`s-`). The common macOS
shortcuts (`s-c`, `s-v`, `s-x`, `s-z`, `s-s`, `s-a`, `s-q`) also work.

| Area | Keys |
| --- | --- |
| Motion | `C-f` `C-b` `M-f` `M-b` `C-n` `C-p` `C-a` `C-e` `M-m` `M-a` `M-e` `M-{` `M-}` |
| Sexps | `C-M-f` `C-M-b` `C-M-u` `C-M-d` |
| Buffer and screen | `M-<` `M->` `C-v` `M-v` `C-l` `M-g g` |
| Mark | `C-SPC` `C-x C-x` `C-x h` `M-@` `M-h` `C-M-SPC`; `C-u C-SPC` jumps to the previous mark |
| Kill and yank | `C-k` `C-w` `M-w` `M-d` `M-DEL` `M-k` `C-M-k` `M-z` `C-y` `M-y` |
| Delete | `C-d` `DEL` `M-\` `M-SPC` `M-^` `C-x C-o` |
| Edit | `C-t` `M-t` `C-x C-t` `C-M-t` `M-u` `M-l` `M-c` `C-x C-u` `C-x C-l` `M-q` `C-o` `C-q` |
| Undo and redo | `C-/` (undo), `C-?` (redo) |
| Multiple cursors | `C->` `C-<` (next/previous like this), `C-c C-<` (all like this), `C-S-c C-S-c` (one per line), `C-g` (back to one cursor) |
| Search | `C-s` (`s-f`) search, `C-r` search backward; inside: `C-s`/`C-r` next/previous, `RET` jump, `C-g` cancel, `M-n` symbol at point, `C-s C-s` repeat the last search |
| Buffers | `C-x b` `C-x C-b` `C-x <left>` `C-x <right>` `C-x k` |
| Files | `C-x C-s` (save), `C-x C-c` (quit) |
| Commands | `M-x` (run a command by name; `C-u M-x` passes the prefix argument on) |
| Modes | `C-x C-q` (read-only), `<insert>` (overwrite); `M-x rust-mode` etc. switch the major mode |

These are the default bindings. The full set is in
[`config/default.json`](config/default.json), and every key can be changed (see
[Configuration](#configuration)).

## Configuration

Settings and key bindings come from JSON. The defaults are in
[`config/default.json`](config/default.json), which is built into the binary
(`emax --print-default-config` prints it). Your own file is read from the first
of these that is set:

1. `$EMAX_CONFIG`
2. `$XDG_CONFIG_HOME/emax/config.json`
3. `~/.config/emax/config.json`

Your file only needs what you want to change. Settings replace the defaults.
Keymap entries are added to the default keymaps and replace any binding for the
same keys. Setting an entry to `null` removes a default binding:

```json
{
  "editor": { "fill_column": 80, "tab_width": 2 },
  "keymaps": {
    "global": { "C-z": "undo", "C-x C-c": null },
    "search": { "C-j": "search-accept" }
  }
}
```

| Section | What it holds |
| --- | --- |
| `editor.fill_column` | Column for `fill-paragraph` (default 70) |
| `editor.tab_width` | Display width of a tab and the indent step (default 4) |
| `font.family` | Font for buffer text (default `"monospace"`: Menlo on macOS, Consolas on Windows, DejaVu Sans Mono on Linux) |
| `font.ui_family` | Font for the buffer picker and panel hints (default `"system-ui"`, the platform's UI font) |
| `font.size` | Text size in pixels (default 14). The mode line, echo area, buffer picker and search panel scale with it |
| `font.line_height` | Line height as a multiple of the size (default 1.5) |
| `theme` | The theme to use (default `"emax-dark"`), or `{ "light": …, "dark": … }` to follow the system appearance |
| `themes` | Theme definitions; see [Themes](#themes) |
| `keymaps.global` | Editing keys; multi-key sequences like `"C-x C-s"` work. Values are command names (`forward-char`, `swiper`, …) |
| `keymaps.picker` | Keys while the buffer picker is open; values are `picker-*` actions |
| `keymaps.search` | Keys while the search panel is open; values are `search-*` actions |
| `modes` | Major and minor modes; see [Modes](#modes) |

Keys use Emacs notation: `C-` Control, `M-` Meta (Option), `S-` Shift, `s-`
Super (Cmd), and names like `RET`, `TAB`, `DEL`, `ESC`, `SPC`, `<left>`,
`<next>`. Mistakes such as bad JSON, unknown commands or malformed keys don't
stop the editor. That entry (or file) is skipped, and the problem shows in the
echo area and on stderr.

### Modes

Each buffer has one **major mode** and any number of **minor modes**, shown in
the mode line as `(Rust Fill)`.

When a file is opened, its major mode is the first of these that applies:

1. a marker on the first line (or the second, after `#!`), as in Emacs:
   `-*- rust -*-` or `-*- mode: rust -*-`
2. the program on a `#!` line (`#!/usr/bin/env steel` gives `lisp-mode`)
3. the file name, matched against each mode's `files` patterns (the most
   specific pattern wins)
4. `default_major_mode` (`fundamental-mode`)

`*scratch*` starts in `initial_major_mode` (`text-mode`). The built-in major
modes are `fundamental-mode`, `text-mode`, `markdown-mode`, `prog-mode`,
`rust-mode`, `json-mode` and `lisp-mode`. Each mode is also a command:
`M-x markdown-mode` switches the buffer to it. Like in Emacs, switching drops
the buffer's minor modes and buffer-local settings (except `read-only-mode`),
then turns on the ones the new mode wants.

A major mode is defined like this; every field is optional:

```json
{
  "modes": {
    "major": {
      "go-mode": {
        "lighter": "Go",
        "parent": "prog-mode",
        "files": ["*.go"],
        "interpreters": ["gorun"],
        "editor": { "tab_width": 8 },
        "minor_modes": ["auto-fill-mode"],
        "keymap": { "C-c C-f": "fill-paragraph" }
      }
    }
  }
}
```

| Field | Meaning |
| --- | --- |
| `lighter` | Mode-line name (default: from the mode's name, `go-mode` gives `Go`) |
| `parent` | Mode to inherit from: its settings, keymap and minor modes apply unless this mode overrides them (`""` removes an inherited parent) |
| `files` | File name patterns with `*` and `?`; lowercase patterns ignore case |
| `interpreters` | Programs on a `#!` line that select the mode (trailing version numbers are ignored, so `python` matches `python3`) |
| `editor` | `fill_column` and `tab_width` for buffers in this mode |
| `minor_modes` | Minor modes to turn on, after the parent's (Emacs' mode hooks) |
| `keymap` | Bindings that apply in this mode, over the parent's and the global ones |

Defining a mode that already exists merges your fields into it. For example,
`{"modes": {"major": {"rust-mode": {"editor": {"fill_column": 80}}}}}` changes
only rust-mode's fill column.

The **minor modes** are built in:

| Mode | What it does | Default lighter |
| --- | --- | --- |
| `auto-fill-mode` | Breaks the line at `fill_column` when you type a space or newline past it. On in `text-mode` | `Fill` |
| `read-only-mode` | Refuses every change (what a kill copies still goes to the kill ring). On for files you can't write | `RO` |
| `overwrite-mode` | Typed characters replace the ones after point, up to the end of the line | `Ovwrt` |
| `electric-pair-mode` | Typing `(`, `[`, `{` or `"` also inserts the closing half. Typing the closer steps over it, `DEL` in an empty pair removes both halves, and with a region the pair wraps it. On in `prog-mode` | none |

`modes.minor.<name>` sets a minor mode's `lighter` and `keymap`. Running a
minor mode's command toggles it. With a prefix argument it turns the mode on
if the argument is positive (`C-u` counts) and off otherwise.

Keys are looked up in this order: the minor modes' keymaps (the most recently
enabled first), the major mode's keymap and then its parents', and finally
`keymaps.global`. Settings resolve the same way: a buffer-local value (e.g.
from `C-x f`), the major mode and its parents, then `editor`.

### Themes

emax comes with `emax-dark`, `emax-light`, `solarized-dark` and
`solarized-light`. A theme is a set of colors
written as `#rrggbb` or `#rrggbbaa`. `emax --print-default-config` lists every
color name with its value in both built-in themes.

To change a few colors of a built-in theme, set just those colors under its
name. To make a new theme, give it a name and let it `inherit` an existing one
so you only list what differs:

```json
{
  "theme": { "light": "paper", "dark": "emax-dark" },
  "themes": {
    "emax-dark": { "cursor": "#ff6c6b" },
    "paper": { "inherits": "emax-light", "background": "#fdf6e3", "current_line": "#f5eedc" }
  }
}
```

A theme that doesn't `inherit` should define every color. Any color it leaves
out is taken from `emax-dark`, and the echo area warns about it.

## Project layout

```
config/
  default.json       built-in settings, key bindings and modes
src/
  main.rs            app entry point: opens the file or *scratch* buffer and the window
  ui.rs              GPUI view: rendering and platform-key to Emacs-key translation
  engine/            the editing engine, with no GUI dependencies
    theme.rs         themes, colors and fonts
    buffer.rs        rope, file association, undo/redo history
    buffers.rs       multiple buffers in most-recently-used order; picker actions, M-x
    cursor.rs        point, mark and mark ring for each cursor
    editor.rs        editor state: buffer, cursors, kill ring, command bookkeeping
    config.rs        JSON configuration: settings, keymaps, modes, user overrides
    modes.rs         major and minor modes: selection, settings and keymap layering
    commands.rs      interactive commands, named after their GNU Emacs counterparts
    keymap.rs        key parsing, keymaps, prefix keys and arguments, dispatcher
    search.rs        swiper-style line search (C-s)
    picker.rs        fuzzy "completing read" picker (buffer switching, M-x)
    text.rs          pure motion primitives: words, sentences, paragraphs, sexps
    tests.rs         behavioural tests driven by real key sequences
```

The engine does not depend on the GUI, so you can test it without opening a
window. In `tests.rs`, buffer strings mark cursor positions with `|`, and
several `|` mean several cursors.

## License

emax is released under the [MIT License](LICENSE).
