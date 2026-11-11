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
- **Commands use their GNU Emacs names** (`forward-sexp`, `kill-ring-save`, …).
  `M-x` is bound but not implemented yet.

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
| `keymaps.global` | Editing keys; multi-key sequences like `"C-x C-s"` work. Values are command names (`forward-char`, `swiper`, …) |
| `keymaps.picker` | Keys while the buffer picker is open; values are `picker-*` actions |
| `keymaps.search` | Keys while the search panel is open; values are `search-*` actions |

Keys use Emacs notation: `C-` Control, `M-` Meta (Option), `S-` Shift, `s-`
Super (Cmd), and names like `RET`, `TAB`, `DEL`, `ESC`, `SPC`, `<left>`,
`<next>`. Mistakes such as bad JSON, unknown commands or malformed keys don't
stop the editor. That entry (or file) is skipped, and the problem shows in the
echo area and on stderr.

## Project layout

```
config/
  default.json       built-in settings and key bindings
src/
  main.rs            app entry point: opens the file or *scratch* buffer and the window
  ui.rs              GPUI view: rendering and platform-key to Emacs-key translation
  engine/            the editing engine, with no GUI dependencies
    buffer.rs        rope, file association, undo/redo history
    buffers.rs       multiple buffers in most-recently-used order
    cursor.rs        point, mark and mark ring for each cursor
    editor.rs        editor state: buffer, cursors, kill ring, command bookkeeping
    config.rs        JSON configuration: settings and keymaps, user overrides
    commands.rs      interactive commands, named after their GNU Emacs counterparts
    keymap.rs        key parsing, keymaps, prefix keys and arguments, dispatcher
    search.rs        swiper-style line search (C-s)
    picker.rs        fuzzy "completing read" picker (buffer switching; later M-x)
    text.rs          pure motion primitives: words, sentences, paragraphs, sexps
    tests.rs         behavioural tests driven by real key sequences
```

The engine does not depend on the GUI, so you can test it without opening a
window. In `tests.rs`, buffer strings mark cursor positions with `|`, and
several `|` mean several cursors.
