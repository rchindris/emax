# Terminal substrate: hooks, windows, terminals, PTYs, sessions

Status: contract for the parallel build on `feat/terminal-substrate`.
Everything named here already exists in the tree as a compiling skeleton
(types, traits, signatures, registered commands, config, key bindings); each
builder fills in the bodies it owns. If you need to change a shared signature,
keep the change minimal and list it in your report so the integrator can
reconcile.

The long-term goal is an agent multiplexer written as a Steel (Scheme) plugin
on top of this. So every registry stays **data-driven and addressable by
name**: commands by name (`commands::lookup`), hooks by event-kind name
(`EventKind::from_name("after-command")`) plus handler name, keymaps from
JSON, events serialisable to JSON. Do **not** implement Steel or anything
agent-specific.

## 0. Ground rules (all builders)

- `export PATH="$HOME/.cargo/bin:$PATH"` in every shell. GPUI keeps the
  `runtime_shaders` feature.
- The engine (`src/engine`) has no GUI dependency; anything testable lives
  there. `src/ui.rs` and `src/ui/*.rs` are thin rendering/input layers.
- Style: module doc comment on every file, small focused functions, comments
  that say *why*. Tests per module (`#[cfg(test)] mod tests`) or in
  `src/engine/tests.rs` for key-driven behaviour.
- Tests are deterministic: no sleeps as synchronisation. Use
  `crate::engine::testutil::{wait_until, TIMEOUT, temp_dir}` (poll with a
  timeout, ≤10 s). Sockets and files go in `temp_dir(..)` (short paths:
  macOS limits socket paths to ~104 bytes). Never touch the real
  `~/.config`, `~/.local/state` or the real daemon socket: set paths
  explicitly (`TerminalConfig.socket`, `SessionConfig.state_dir`, function
  arguments) rather than mutating process env vars in tests (tests run in
  parallel threads).
- `cargo build` and `cargo test` pass with zero new warnings.
- Git: commit on your own branch with clear messages; no AI/assistant
  mentions, no `Co-Authored-By`. Never touch `main`.
- Commands: every command a subsystem adds lives in that subsystem's own
  `pub static COMMANDS: &[Command]` (already registered in
  `commands::TABLES`). All names in this doc are already registered as
  stubs returning `Err("<name>: not implemented yet")`, and bound in
  `config/default.json` — replace the stub functions, don't rename.

## 1. File ownership

| Area | Owner | Files |
|---|---|---|
| Hooks (done) | foundation | `src/engine/hooks.rs` |
| Windows & tabs | **windows** | `src/engine/window.rs` (may become `src/engine/window/`), `src/ui.rs` |
| Terminal emulator + terminal buffers | **term** | `src/engine/term/**`, `src/ui/term_view.rs` |
| PTY / local processes | **pty** | `src/engine/pty/local.rs` (+ new files under `src/engine/pty/`) |
| Daemon, protocol, session file | **persistence** | `src/engine/session/**`, `src/main.rs`, `tests/` (integration tests) |

Shared files (foundation-owned; edit only if unavoidable, minimally, and
report it): `Cargo.toml`, `config/default.json`, `src/engine/{editor,buffer,
buffers,commands,config,keymap,modes,theme,mod,testutil}.rs`,
`src/engine/pty/{mod,mock}.rs` (pty builder may extend these two).

Allowed cross-area touch points (already wired — do not move them):

| Where | Call | Implemented by |
|---|---|---|
| `Editor::replace` | `self.layout.adjust_for_edit(..)` | foundation (done) |
| `Editor::switch_to_buffer_ex` | `self.sync_selected_window()` | windows may extend (e.g. emit `WindowConfigurationChanged`) |
| `Editor::kill_buffer` | `layout.replace_buffer`, `terminal_buffer_killed`, emit `BufferKilled` | foundation (done) |
| `Editor::active_keymaps` | puts `terminal_config.keymap` first in terminal buffers | foundation (done) |
| `Editor::tick` | `poll_pty()` → `run_timers` → `run_hooks` | term implements `poll_pty` |
| `EmaxView::handle_key` | `if !keys.is_pending() && ed.terminal_key(&key)` | term implements `terminal_key` |
| `EmaxView::render` | `ed.terminal_snapshot`, `ed.resize_terminal`, `term_view::render_terminal` | windows moves it into the per-window render; term implements |
| `main.rs` | `--pty-daemon` → `session::daemon::main` | persistence |

Merge order for the integrator: **pty → term → windows → persistence**
(each later branch only depends on contracts here, not on earlier bodies).

## 2. Threading model

- The GPUI main thread owns `EmaxView`, which owns the `Editor`. `Editor`
  is not `Send` (hook handlers are `Rc`), and nothing engine-side needs it to
  be.
- A `PtyBackend` does blocking I/O on its own threads (reader per session,
  or one socket reader for the daemon client) and passes events through an
  internal channel. `try_recv` never blocks.
- Waking the UI: `Editor.waker: pty::Waker = Arc<dyn Fn() + Send + Sync>`.
  The view installs one in `EmaxView::new` (an unbounded futures channel;
  a foreground task drains it and calls `EmaxView::tick`). Backends are
  created lazily by `Editor::pty_backend()` with `self.waker.clone()`.
  Backends wrap the waker in a `pty::Notifier`, so it is called once per
  batch: producers `notify()` after queueing; `try_recv` calls `clear()`
  when it finds the queue empty (i.e. before the consumer stops draining).
- `EmaxView::tick` calls `Editor::tick(Instant::now())`, which runs
  `poll_pty()` (drain backend events into terminals), fires due timers and
  drains the hook queue; it redraws if anything happened, then re-arms a
  GPUI timer for `hooks.next_deadline()`. Keys also re-arm it.
- `poll_pty` must bound its work per tick (suggested: stop after 4 MiB of
  output, then call `self.waker` so another tick follows) to keep the UI
  responsive under floods.

## 3. Hooks — `src/engine/hooks.rs` (DONE)

```rust
pub enum Event {                                   // #[serde(tag = "event", rename_all = "kebab-case")]
    BufferCreated { buf: BufferId },
    BufferKilled { buf: BufferId, name: String },
    AfterCommand { name: String },
    WindowConfigurationChanged,
    TerminalOutput { buf: BufferId },
    TerminalTitle { buf: BufferId, title: String },
    TerminalBell { buf: BufferId },
    ProcessExited { buf: BufferId, status: ExitStatus },
    Timer { id: TimerId, name: String },
    KillEmacs,
}
pub enum EventKind { .. }        // name(): "buffer-created", "after-command", ..., from_name()
pub type HookFn = Rc<dyn Fn(&mut Editor, &Event) -> Result<(), String>>;
impl Hooks {
    fn add(&mut self, kind, name: &str, f: HookFn);          // same name replaces in place
    fn add_named(&mut self, kind: &str, name, f) -> Result<(), String>;
    fn remove(&mut self, kind, name) -> bool;
    fn handler_names(&self, kind) -> Vec<&str>;  fn list(&self) -> Vec<HookInfo>;
    fn run_at(&mut self, name, due: Instant, cb: Option<HookFn>) -> TimerId;
    fn run_every(&mut self, name, first: Instant, every: Duration, cb) -> TimerId;
    fn cancel_timer(&mut self, id) -> bool;  fn timers(&self);  fn next_deadline(&self) -> Option<Instant>;
}
impl Editor {
    fn emit(&mut self, e: Event);                 // queue only, never runs handlers
    fn run_hooks(&mut self) -> usize;             // drain; not re-entrant; cap MAX_EVENTS_PER_DRAIN = 1000
    fn run_timers(&mut self, now: Instant) -> usize;
    fn tick(&mut self, now: Instant) -> bool;
}
```

Semantics: handlers run in registration order; an `Err` is reported in the
echo area as `Error in <kind> hook <name>: <msg>` and the others still run.
Events emitted by handlers are handled in the same drain; past 1000 events
the rest are dropped with a warning (loop guard). A due timer queues
`Event::Timer`; its own callback runs first, then `timer` handlers. A
repeating timer that fell behind fires once and is rescheduled from `now`.
`Event::to_json()` gives the plain-data form.

Emission points (who emits what):

| Event | Emitted by |
|---|---|
| `BufferCreated` | `Editor::new` (buffer 1), `Editor::add_buffer` — done |
| `BufferKilled` | `Editor::kill_buffer` — done |
| `AfterCommand` | `Editor::run`, after every command (also failed/refused ones), then the queue is drained — done |
| `KillEmacs` | `save-buffers-kill-terminal` when it decides to quit — done |
| `Timer` | `Hooks::fire_due` — done |
| `WindowConfigurationChanged` | **windows**: split, delete, resize, balance, select, a window showing another buffer, any tab change |
| `TerminalOutput` (once per buffer per `poll_pty`), `TerminalTitle`, `TerminalBell`, `ProcessExited` | **term**, in `poll_pty` |

## 4. Windows & tabs — `src/engine/window.rs` (windows builder)

Data types (in place, keep them; add fields if needed):

```rust
pub struct WindowId(pub u64);
pub enum SplitDir { Horizontal /* side by side, C-x 3 */, Vertical /* stacked, C-x 2 */ }
pub enum Node { Leaf(WindowId), Split { dir: SplitDir, children: Vec<Node>, ratios: Vec<f32> } }
pub struct Window { id, buffer: BufferId, cursors: Vec<Cursor>, primary: u64, next_cursor_id: u64, scroll_line: usize }
pub struct Tab { name: Option<String>, root: Node, selected: WindowId }
pub struct Layout { tabs: Vec<Tab>, current_tab: usize, windows: HashMap<WindowId, Window>, .. }
pub struct Rect { x, y, w, h: f32 }
pub fn compute_rects(root: &Node, area: Rect) -> Vec<(WindowId, Rect)>;   // implement
```

Invariants:
- `Editor.layout` always has ≥1 tab, each tab ≥1 window; `tab.selected` is
  a leaf of `tab.root`; `layout.windows` holds exactly the leaves of all tabs.
- The selected window of the current tab shows `Editor.buf_id`; its live
  point/cursors are `Editor.cursors/primary/next_cursor_id` (its
  `Window.cursors` is stale). All other windows keep their own cursors.
- Splits have ≥2 children; deleting down to one child collapses the split
  into its parent; a child split with the same `dir` as its parent may be
  flattened. Ratios are >0 and sum to 1; `compute_rects` tiles `area`
  exactly (no gaps/overlaps; last child absorbs rounding) and is pure.
- Only the engine changes the layout; the view reads it.

To implement (names fixed; add helpers freely):
- `Editor::select_window(id) -> bool`: store `Editor.cursors` into the old
  window, switch to the new window's buffer (`switch_to_buffer`), install
  its cursors; same-buffer windows just swap cursors. Emits
  `WindowConfigurationChanged`.
- Commands (stubs in `window::COMMANDS`, bound in `default.json`):
  `split-window-below` C-x 2, `split-window-right` C-x 3 (new window shows
  the same buffer with a copy of the cursors; the original stays selected,
  like Emacs), `delete-window` C-x 0 (error on the sole window),
  `delete-other-windows` C-x 1, `other-window` C-x o (prefix arg N, negative
  goes back; order = `Node::windows()` tree order), `balance-windows` C-x +,
  `enlarge-window` C-x ^ / `shrink-window` (N lines),
  `enlarge-window-horizontally` C-x } / `shrink-window-horizontally` C-x {
  (N columns), `windmove-left/right/up/down` C-x w <arrow> (geometric:
  nearest window in that direction overlapping the selected one's span, from
  `compute_rects` on the last frame size), `tab-new` C-x t 2 (new tab, one
  window on the current buffer, selected), `tab-close` C-x t 0 (error on the
  last tab; its windows are dropped, buffers stay), `tab-next` C-x t o /
  C-TAB, `tab-previous` C-x t S-o / C-S-TAB (wrap around; prefix arg N),
  `tab-rename` C-x t r (name = `Args.text` if given, else clear the
  explicit name so the tab shows its selected buffer's name).
- Sizes for enlarge/shrink/windmove: the view reports the frame size in
  cells each render (add e.g. `Layout::set_frame_size(cols: f32, rows: f32)`,
  default 80×24); line/column deltas convert to ratio deltas of the nearest
  ancestor split of the right direction. Keep a minimum window size (e.g.
  4 lines × 10 columns); refuse with a message beyond it.
- `Editor::sync_selected_window` (in window.rs) may emit
  `WindowConfigurationChanged` when the buffer actually changes.

UI (`src/ui.rs`, windows builder): render the current tab's tree with
`compute_rects` (flex layout is fine if it matches), one text view + mode
line per window, the selected window's mode line highlighted, a 1px
separator; per-window `UniformListScrollHandle` keyed by `WindowId` (today
it's keyed by buffer id); `Window.scroll_line` kept up to date; picker
preview and swiper apply to the selected window; a tab bar when there is
more than one tab; clicking a window selects it. For a terminal buffer,
render `term_view::render_terminal` and call
`ed.resize_terminal(buf, cols, rows)` with `term_view::grid_size` of the
window (do this only for the selected window, or the first window showing
that buffer if none is selected). Report `Layout::set_frame_size` each render.

Tests: geometry (exact tiling, nested splits, ratios after delete/balance,
rounding), every command (incl. error cases and prefix args), independent
points for one buffer in two windows (editing in one moves the other's point
correctly), kill-buffer shown in several windows/tabs, tab lifecycle,
windmove in irregular layouts, hooks emitted. Key-driven tests via
`src/engine/tests.rs`-style `T` helper.

## 5. Terminal emulator & terminal buffers — `src/engine/term/` (term builder)

`term/mod.rs` — `Terminal` wraps `alacritty_terminal = "0.26"`
(`Term<L>` + `vte::ansi::Processor`; `L: EventListener` collects `PtyWrite`
→ replies, `Title`/`ResetTitle` → title, `Bell`). Contract:

```rust
pub struct Palette { foreground, background, cursor: Rgba, ansi: [Rgba; 16] }   // Palette::from_theme(&Theme)
pub struct Cell { ch: char, zerowidth: Vec<char>, fg: Rgba, bg: Rgba, flags: CellFlags }
pub struct CellFlags { bold, dim, italic, underline, strikethrough, hidden, wide, wide_spacer }
pub struct CursorState { row, col, shape: CursorShape, visible }
pub struct Snapshot { cols, rows, lines: Vec<Vec<Cell>>, cursor, display_offset, background }
pub struct Modes { app_cursor, app_keypad, bracketed_paste, alt_screen, mouse }
pub struct Feed { replies: Vec<u8>, title: Option<String>, bell: bool }
impl Terminal {
    fn new(cols: u16, rows: u16, scrollback: usize) -> Self;
    fn feed(&mut self, bytes: &[u8]) -> Feed;     // replies MUST be written back to the PTY by the caller
    fn resize(&mut self, cols, rows);  fn size(&self) -> (u16, u16);
    fn title(&self) -> &str;  fn modes(&self) -> Modes;
    fn scroll(&mut self, lines: i32);  fn scroll_to_bottom(&mut self);  fn display_offset(&self) -> usize;
    fn snapshot(&self, &Palette) -> Snapshot;     // inverse applied, bold-as-bright for 0–7, 256-colour cube + greys, truecolour
    fn text(&self) -> (String, (usize, usize));   // history + screen, wrapped lines joined, trailing blanks trimmed; cursor (line, col)
}
```

The theme now has `term_black … term_bright_white` (16 colours, all built-in
themes define them; `Theme::ansi()`).

`term/input.rs`: `encode_key(&Key, Modes) -> Option<Vec<u8>>` and
`encode_paste(&str, Modes) -> Vec<u8>`. Keys arrive in Emacs notation
(`Key { ctrl, meta, shift, sup, name, text }`, names like `a`, `RET`, `TAB`,
`DEL`, `ESC`, `SPC`, `<up>`, `<prior>`, `<f5>`, `<delete>`). xterm rules:
`C-a`..`C-z` → 0x01..0x1a, `C-SPC`/`C-@` → 0x00, `C-[` ESC, `C-\` 0x1c,
`C-]` 0x1d, `C-^` 0x1e, `C-_`/`C-/` 0x1f; `RET` → `\r`, `TAB` → `\t`,
`S-TAB` → `ESC [ Z`, `DEL` → 0x7f, `M-DEL` → `ESC 0x7f`; Meta = ESC prefix;
arrows/home/end `ESC [ A` or `ESC O A` in app-cursor mode, with modifiers
`ESC [ 1 ; m A` (m = 1 + shift + 2·meta + 4·ctrl); `<prior>/<next>/
<insert>/<delete>` `ESC [ 5~/6~/2~/3~`; F1–F4 `ESC O P..S`, F5–F12
`ESC [ 15~ ..`; text keys send their UTF-8 `text`. Super (`s-`) chords → `None`.

`term/buffer.rs` (signatures in place):

```rust
pub enum InputMode { Char, Copy }
pub struct TermState { term: Terminal, session: SessionId, spec: SpawnSpec, mode: InputMode, exited: Option<ExitStatus> }
impl Editor {
    fn is_terminal(&self, buf) -> bool;  fn terminal(&self, buf) -> Option<&TermState>;
    fn terminal_snapshot(&self, buf, &Palette) -> Option<Snapshot>;   // None in copy mode (the view then draws the rope)
    fn open_terminal(&mut self, name: &str, spec: SpawnSpec) -> io::Result<BufferId>;
    fn attach_terminal(&mut self, name: &str, session: SessionId, spec: SpawnSpec) -> io::Result<BufferId>; // for restore
    fn terminal_key(&mut self, key: &Key) -> bool;
    fn terminal_paste(&mut self, text: &str) -> bool;
    fn resize_terminal(&mut self, buf, cols: u16, rows: u16);         // dedupe unchanged sizes
    pub(crate) fn terminal_buffer_killed(&mut self, buf);             // done: kills the session
    fn poll_pty(&mut self) -> bool;
}
```

- A terminal buffer is a `Buffer` with `kind = BufferKind::Terminal`, made
  with `add_buffer` (so names become `*term*`, `*term*<2>`, and
  `BufferCreated` fires), plus a `TermState` in `Editor.terminals`. In char
  mode its rope is empty/unused.
- `term`, `ansi-term`: `SpawnSpec { program: terminal_config.shell_program(),
  args: shell_args, cwd: dir of the current buffer's file (else the editor's
  cwd), env: [("TERM", terminal_config.term), ("COLORTERM", "truecolor")],
  size: last size or 80×24 }` via `self.pty_backend()?.spawn(..)`; the new
  buffer becomes current.
- `terminal_key` (current buffer is a terminal in char mode, else `false`):
  return `false` (let the dispatcher have it) for super chords, keys in
  `terminal_config.escape_keys` (default `C-x`, `M-x`, `C-c`), and keys the
  `terminal` keymap binds alone or as a prefix (`S-<prior>`, `s-v`, `C-c`);
  otherwise encode, write to the session, scroll to the bottom, return `true`.
- `terminal` keymap defaults (config/default.json `keymaps.terminal`):
  `C-c C-j` term-copy-mode, `C-c C-k` term-char-mode, `C-c C-c`
  term-interrupt-subjob, `C-c C-d` term-send-eof, `C-c C-z`
  term-stop-subjob, `C-c C-\` term-quit-subjob, `C-c C-y`/`s-v` term-paste,
  `S-<prior>`/`S-<next>` term-scroll-up/down. All other sequences that start
  with an escape key fall through to the global keymap (C-x b, M-x, ...).
- Copy mode: put `term.text()` in the rope, point at the cursor's position,
  turn on `read-only-mode`, `mode = Copy`; char mode: empty the rope, turn
  read-only off, `mode = Char`. Output keeps updating the `Terminal`
  meanwhile; the text copy is refreshed on the next copy-mode entry.
- `poll_pty`: drain `try_recv` (bounded per tick, see §2); map
  `SessionId` → buffer (ignore unknown sessions); `feed`, write `replies`
  back, emit `TerminalTitle`/`TerminalBell` as fed, one `TerminalOutput`
  per buffer per poll; on `Exited` set `exited`, emit `ProcessExited`, show
  `Process <name> exited` (the buffer stays; keys then do nothing but the
  escape sequences).
- `term-paste` pastes `kill_ring.last()` (the view already pulls the system
  clipboard into the kill ring before `yank`/`term-paste`).
- `src/ui/term_view.rs`: `render_terminal(&Snapshot, line_h) -> AnyElement`
  (runs of same-styled cells; colours, bold/italic/underline, wide chars,
  cursor shape), `cell_width(font_size)` (measure the real advance with the
  text system if you can), `grid_size(..)`.

Tests: `Terminal` against byte streams (text, wrapping, CR/LF, SGR colours
16/256/truecolour, inverse, cursor movement, erase, alt screen, scrollback
limit and scrolling, wide/combining chars, title OSC 0/2, bell, DSR `ESC[6n`
→ `ESC[row;colR` reply, DA reply, invalid UTF-8 split across `feed` calls),
`encode_key` table incl. app-cursor and modifiers, bracketed paste, and
terminal buffers end-to-end with `pty::mock::MockBackend` (spawn spec, key
routing incl. escape keys and `C-c C-c`, output → snapshot, replies written
back, hooks, exit, copy mode round-trip, kill-buffer kills the session,
resize dedupe).

## 6. PTY I/O — `src/engine/pty/` (pty builder)

Contract (`pty/mod.rs`, in place):

```rust
pub struct SessionId(pub u64);
pub struct PtySize { cols: u16, rows: u16, pixel_width: u16, pixel_height: u16 }
pub struct SpawnSpec { program: String, args: Vec<String>, cwd: Option<PathBuf>, env: Vec<(String, String)>, size: PtySize }
pub struct ExitStatus { code: u32, signal: Option<String> }
pub enum PtyEvent { Output(SessionId, Vec<u8>), Exited(SessionId, ExitStatus) }
pub struct SessionInfo { id, program, args, cwd, pid: Option<u32>, size, exited: Option<ExitStatus> }
pub type Waker = Arc<dyn Fn() + Send + Sync>;
pub trait PtyBackend {
    fn spawn(&mut self, SpawnSpec) -> io::Result<SessionId>;   // attached
    fn attach(&mut self, SessionId, PtySize) -> io::Result<()>;
    fn detach(&mut self, SessionId) -> io::Result<()>;
    fn write(&mut self, SessionId, &[u8]) -> io::Result<()>;
    fn resize(&mut self, SessionId, PtySize) -> io::Result<()>;
    fn kill(&mut self, SessionId) -> io::Result<()>;           // no events for it afterwards
    fn list(&mut self) -> io::Result<Vec<SessionInfo>>;
    fn try_recv(&mut self) -> Option<PtyEvent>;               // never blocks
    fn persistent(&self) -> bool { false }
}
pub struct Notifier;   // wake once per batch (done)
```

`pty/mock.rs` (`MockBackend` + `MockHandle`) is done; use it in engine
tests that don't need real processes.

`LocalBackend::new(waker)` (`pty/local.rs`) with `portable-pty = "0.9"`:
- `native_pty_system().openpty(size)`, `CommandBuilder` with program, args,
  cwd, env (on top of the inherited environment).
- One reader thread per session: blocking reads into a 64 KiB buffer; each
  read becomes one `Output` (never per byte); events go through a **bounded**
  channel (e.g. `sync_channel(256)`), so a stalled consumer blocks the reader,
  the kernel PTY buffer fills and the child blocks — memory stays bounded.
  `notifier.notify()` after each send.
- On EOF/error the reader waits for the child and sends `Exited` — always
  after all of that session's `Output`.
- `write` uses the master's writer (`write_all`); `resize` the master;
  `kill` the child killer (then forget the session; drop any later events
  for it in `try_recv`); `detach` drops output while detached; `attach`
  resumes (no replay buffer) and resizes. `list` reports pids. Dropping the
  backend kills its children.
- Non-UTF-8 bytes pass through untouched.

Tests (real processes, `/bin/sh`): output arrives, exit codes and ordering
(all output before `Exited`), echo round-trip through `cat`, large output
(≥5 MB) complete with far fewer events than bytes, backpressure (don't drain
for a while; the child blocks, nothing is lost once draining resumes), resize
visible to `stty size`, `kill` ends the process (check the pid is gone) and
silences events, env and cwd honoured, invalid UTF-8 preserved, waker called
and coalesced, spawn of a missing program fails or exits non-zero, many
sessions in parallel.

## 7. Session persistence — `src/engine/session/` (persistence builder)

### 7.1 Daemon (`session/daemon.rs`, `emax --pty-daemon`)

- `run(DaemonOptions { socket, replay_bytes, idle_exit })` binds a
  `UnixListener` (parent dir created with mode 0700; a stale socket file
  that refuses connections is removed; a live one → `AddrInUse`). Owns a
  `LocalBackend` and, per session, a ring buffer of the last `replay_bytes`
  output bytes (always recording, attached or not), the spec, size and exit
  status. Exited sessions stay (listed with `exited`) until `Kill`.
- One reader thread per client connection feeds a single main loop that owns
  all state (so replay-then-live is atomic). `Spawn` attaches the requesting
  connection; `Attach` sends the ring contents as `Output` frames (≤
  `MAX_OUTPUT_CHUNK` each), then `Exited` if the session has exited, **then**
  the `Reply`, then live output; it also resizes (TUIs redraw). Several
  clients may attach to one session (all get output; last resize wins). A
  disconnect detaches that client's sessions; processes keep running.
  `Shutdown` kills all sessions, replies, removes the socket and exits.
  `idle_exit`: exit after that long with no sessions and no clients.
- `main(args)`: `--pty-daemon [--socket PATH] [--replay-bytes N]
  [--foreground]`; without `--foreground`, detach (`setsid`, stdio to
  `/dev/null`).

### 7.2 Wire protocol (`session/protocol.rs`, in place and tested)

Frame = `u32` big-endian length + `postcard` body, ≤ `MAX_FRAME` (16 MiB).
`ClientMsg::{Hello{version, client_pid}, Request{req, op: Op}}`,
`Op::{Spawn(SpawnSpec), Attach{id,size}, Detach{id}, Write{id,data},
Resize{id,size}, Kill{id}, List, Shutdown}`,
`DaemonMsg::{Hello{version, daemon_pid}, Reply{req, result: Result<Reply,
String>}, Output{id,data}, Exited{id,status}}`,
`Reply::{Ok, Spawned(SessionId), Sessions(Vec<SessionInfo>)}`.
Handshake first (version mismatch → daemon closes). `req == 0` = no reply
(used for `Write`/`Resize`); every other request gets exactly one `Reply`.
`write_frame` / `read_frame` implement the framing.

### 7.3 Client (`session/client.rs`)

`DaemonBackend::connect(socket, waker)`; `connect_or_start(socket,
replay_bytes, waker)` starts `current_exe() --pty-daemon --socket ..
--replay-bytes ..` if connecting fails, then polls `connect` for up to 5 s.
Add `connect_or_start_with(program: &Path, ..)` for tests (integration
tests use `env!("CARGO_BIN_EXE_emax")`; unit tests run `daemon::run` in a
thread on a `temp_dir` socket). A reader thread turns `Output`/`Exited` into
`PtyEvent`s (with a `Notifier`) and routes `Reply`s to the waiting request
(blocking with a timeout, e.g. 5 s → `TimedOut`). `persistent() == true`.
A lost connection reports `Exited` for nothing (sessions live on) and makes
later calls fail with `NotConnected`.

`Editor::pty_backend()` (done) picks the backend from
`terminal_config.backend` and falls back to `LocalBackend` with a message if
the daemon can't be reached.

### 7.4 Session file (`session/mod.rs`, schema in place)

```json
{ "version": 1,
  "buffers": [ { "id": 1, "name": "main.rs", "point": 5, "kind": "file", "path": "/x/main.rs" },
               { "id": 2, "name": "*scratch*", "point": 0, "kind": "text", "text": "..." },
               { "id": 3, "name": "*term*", "point": 0, "kind": "terminal", "session": 12, "spec": { ... } } ],
  "current_buffer": 1,
  "tabs": [ { "name": "work", "root": { "leaf": 1 }, "selected": 1,
              "windows": [ { "id": 1, "buffer": 1, "point": 5, "scroll_line": 0 } ] } ],
  "current_tab": 0 }
```

`save`/`load` (atomic write, version check), `state_dir`,
`session_file_path`, `default_socket_path` are done. To implement:
`Editor::session_snapshot()` and `Editor::restore_session(&SessionFile)`
(fresh ids; files reopened from disk, missing files skipped; terminal
buffers via `attach_terminal` only when the backend is persistent, skipped
with a message if the session is gone; windows/tabs rebuilt through the pub
`Layout` fields plus `alloc_window_id`/`reserve_window_id`; the editor's
current buffer and cursors set from the selected window). Terminal buffers
are saved only when `pty.persistent()`.

`main.rs`: `--pty-daemon`; on start, when `session_config.restore_session`
and no files were given and a session file exists in
`state_dir(session_config.state_dir)`, restore it (problems → message,
start fresh); register a `KillEmacs` handler named `session-save` that writes
`session_snapshot()` (removable by name, like any hook). Quitting never
kills daemon sessions.

Tests: snapshot/restore round trips (files, text buffers, splits, tabs,
points, terminal buffers via `MockBackend` + `add_detached`), missing file,
bad/old session file, daemon end-to-end in-process (spawn, write/echo,
detach + reattach replays the ring then live output, ring bound, resize on
attach, two clients, client crash keeps the session, exited sessions listed,
kill, shutdown, stale socket, version mismatch), and an integration test in
`tests/` that runs the real binary: start daemon, spawn a shell session,
drop the client, reconnect and see the earlier output replayed.

## 8. Configuration (in place)

```json
"terminal": { "shell": null, "shell_args": [], "term": "xterm-256color",
              "scrollback_lines": 10000, "backend": "daemon",
              "replay_bytes": 2097152, "socket": null,
              "escape_keys": ["C-x", "M-x", "C-c"] },
"session":  { "restore_session": true, "state_dir": null },
"keymaps":  { "terminal": { "C-c C-j": "term-copy-mode", ... } }
```

Parsed into `Config.terminal: TerminalConfig` (with `keymap`) and
`Config.session: SessionConfig`, installed with
`Editor::set_terminal_config(Arc<..>)` / `Editor.session_config` in
`main.rs`. Environment overrides: `EMAX_PTYD_SOCKET` (daemon socket),
`EMAX_STATE_DIR` (state dir).

## 9. Key bindings (in place, `keymaps.global`)

C-x 2/3/0/1/o/+/^/}/{ (windows), C-x w <arrows> (windmove), C-x t 2/0/o/S-o/r
and C-TAB / C-S-TAB (tabs). Terminal keys: §5.
