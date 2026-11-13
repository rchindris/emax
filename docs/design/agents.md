# Agents: a Herdr-style agent multiplexer on the terminal substrate

Status: proposed. Builds on [terminal-substrate.md](terminal-substrate.md).

Goal: run many coding agents (Claude Code, Codex, ...) in ymacs terminals,
know at a glance which are **working**, **blocked** (need you) or **done**,
get told when one needs you, and let agents start, prompt, read and wait on
each other from the command line — with or without an ymacs window open.
Reference: [Herdr](https://herdr.dev/core/) ("tmux for agents").

Decisions (2026-11-12):

1. **Steel first.** Agent knowledge and policy (which program is which
   agent, how its screen reads, what to do on a state change) is Scheme,
   run by an embedded [Steel](https://github.com/mattwparas/steel) engine.
   Rust provides mechanisms: screens, processes, events, a control socket.
2. **The daemon is the agent host.** `ymacs --pty-daemon` already owns the
   processes; it now also keeps each terminal's screen, runs the agent
   plugin and serves the control API. Orchestration keeps working with no
   window open; the editor is one client among others.
3. **First cut = M0–M4 below**: state detection, editor UI, `ymacs ctl`.
   Agent-CLI integrations (hooks installers, resume on restore) and git
   worktrees come after.

## Vocabulary

| Herdr | ymacs |
|---|---|
| server | the PTY daemon (`ymacs --pty-daemon`) |
| terminal | a daemon session (`SessionId`), shown in a terminal buffer |
| pane | a window showing a terminal buffer |
| tab / workspace | tab |
| agent | a terminal whose program the agent plugin recognised, with a state and an optional name |

Agent states: `idle`, `working`, `blocked` (an approval, question or
permission prompt is on screen), `done` (idle after working, not yet seen),
`unknown` (recognised, not classifiable). Names: `[a-z][a-z0-9_-]{0,31}`,
unique among live agents; commands take a name or a terminal id.

## Architecture

```
 ymacs ctl ... ──┐                         ┌── editor (GUI) ── terminal buffers, badges,
 agent hooks ───┤  unix socket (framed)   │                   rollups, agents picker,
 OSC 7501 ──────┤                         │                   notifications
                ▼                         │
        ┌─────────────── daemon main loop (one thread, owns everything) ─────────┐
        │ LocalBackend (PTYs) → per session: replay ring + headless Terminal     │
        │ status scanner (OSC 7501) · foreground process (tcgetpgrp)             │
        │ Steel engine: agents.scm (+ user scripts) ← host API (Rust fns)        │
        │ agent registry · pending waits · subscriptions                         │
        └─────────────────────────────────────────────────────────────────────────┘
```

The Steel engine is not `Send` (it uses `Rc`); it is created on, and never
leaves, the daemon's main-loop thread. Scheme never blocks: waits are
registered requests the main loop resolves when an event satisfies them.

## M0 — Steel host (`src/engine/script/`) — done

- `steel-core = "0.8.3"` (~1 min clean build); `regex` for `regex-match?`.
- `ScriptHost` wraps the engine: `with_builtins`, `load(name, src)`,
  `load_file`, `load_dir` (user scripts, `steel/*.scm` next to the user
  config, name order), `eval`, `call`, `emit(event, args)`, `handles(event)`,
  `next_deadline()` / `fire_due(now)` for the host's loop. Script errors
  never crash the host: loads return them, failing handlers and timers are
  logged through the host and skipped.
- The host API is the `Host` trait (screens, process, env, input,
  `agent-set!`/`agent-clear!`, log). Every entry point takes `&mut dyn Host`
  for the duration of the call; registered functions reach it through a
  scoped thread-local pointer, taken out while in use, so no two `&mut`
  coexist and nothing outlives the call.
- Steel requires registered Rust functions to be `Send + Sync`, and its
  values are not, so Scheme values never live in Rust: handlers and timer
  callbacks are kept by a small Scheme prelude (`on`, `run-after`,
  `cancel-timer`); Rust keeps only event names, timer ids and deadlines.
- Built-in scripts are `include_str!`-ed from `steel/` (`core.scm`:
  `find-line`, `any-line?`, `debounce`).
- Startup: ~80 ms per engine (release and debug alike, deps are optimised).
  Fine for the daemon, which starts once; the editor should create its
  engine lazily.

## M1 — Screens and process facts in the daemon — done

- Per session the daemon keeps a headless `term::Terminal` (200 lines of
  history), fed with every output chunk and resized with the session.
  `Terminal::bottom_lines(n)` reads the live bottom as text: it ends at the
  lower of the cursor and the last row with text, so a fresh screen's blank
  rows don't count. Cost: ~144 MB/s of text in release, so the PTY stays the
  bottleneck. Debug builds are ~8× slower (alacritty's generic grid code is
  compiled into our unoptimised crate); the daemon's megabyte-sized tests
  wait with `THROUGHPUT_TIMEOUT`.
- While **no client is attached**, the daemon's terminal answers the
  program's queries (DSR, DA, colors), so a program started headless
  doesn't hang waiting for a cursor position. An attached client's own
  terminal answers them otherwise; there is never more than one answer.
- **OSC 7501** (Herdr's open "Program Status Protocol"): `ESC ] 7501 ;
  state=<s>[:app=<name>][:...] (BEL | ESC \)`. `term::status::StatusScanner`
  scans the raw bytes, keeping state across chunks, and `Terminal::feed`
  reports what it finds in `Feed.status`. The daemon keeps the last report
  per session; an empty `state` clears it.
- **Foreground process**: `PtyBackend::foreground(id) -> ProcessInfo { pid,
  name, exe, argv }`. It uses `tcgetpgrp` on the master, then the executable
  (`proc_pidpath` on macOS, `/proc/<pid>/exe` on Linux) and the command line
  (`KERN_PROCARGS2` or `/proc/<pid>/cmdline`). `name` is what the process was
  invoked as (argv[0]'s file name), because Claude Code's executable is a
  versioned file behind a `claude` symlink; for `node …/codex`, scripts
  look at `argv`. The local backend implements it; others answer `None`.
- Env set in every daemon program: `YMACS_SOCKET` and `YMACS_TERMINAL=<session
  id>`. The id comes from `PtyBackend::next_id()`, predicted before the spawn
  (the main loop is single-threaded) and checked after it by a debug
  assertion. `YMACS_AGENT=<kind>` in the client's spawn env is the manual
  override M2 will honour.
- `Daemon::{screen, foreground, spawn_env, program_status}` are what M2's
  `Host` implementation is built on.

## M2 — Agent layer (daemon) and `agents.scm`

Rust side (`src/engine/agents/`): the registry (`Agent { session, kind,
name, state, since, seen, completion_seq }`), state transitions (`done` =
`working`→`idle` while unseen; `seen` set by a client), event fan-out.

Host API given to Scheme (first set):

| function | does |
|---|---|
| `(terminal-screen id rows)` | the bottom `rows` rows as a list of strings (live bottom, not scrolled view) |
| `(terminal-process id)` | foreground process name, or `#f` |
| `(terminal-env id key)` | a value from the spawn env |
| `(terminal-send id text)` / `(terminal-keys id keys)` | input (`"esc"`, `"enter"`, `"ctrl+c"` ...) |
| `(agent-set! id kind state)` / `(agent-clear! id)` | publish what the plugin decided |
| `(on 'output|'status|'exit|'spawn f)` | event handlers |
| `(run-after ms f)` | timers (debounce) |

`steel/agents.scm` (built in, overridable by user scripts):

- `agent-kinds`: process names → kind (`claude`, `codex`, `gemini`, `opencode`, ...).
- `agent-rules`: per kind, ordered `(state regex)` pairs over the bottom N
  rows; `blocked` only on explicit approval/question/permission UI; no
  match → `idle` (labelled as fallback, like Herdr).
- On `output`: debounce ~150 ms, then classify; on `status` (OSC 7501 or a
  `report` over the socket): take it as is, rules are skipped for that
  terminal while reports keep coming.

## M3 — Control API and `ymacs ctl`

- Same socket and framing as today; protocol version bump. New
  `Op::Ctl { json }` → `Reply::Ctl { json }`, and `Op::Subscribe` for
  clients that want `AgentChanged` / `SessionSpawned` events pushed.
- `ymacs ctl <noun> <verb> [args]` is a thin client: prints the JSON result,
  exits 0; errors are JSON on stderr, exit 1 (bad usage: 2). Error codes
  follow Herdr's: `agent_not_ready`, `agent_blocked`, `agent_not_idle`,
  `agent_not_running`, `agent_prompt_stalled`, `timeout`, `no_editor`.
- Commands:
  - `terminal list | spawn [--cwd D] [--name N] -- CMD... | read ID [--lines N] | send-text ID TEXT | send-keys ID KEY... | wait-output ID --regex R [--timeout MS] | kill ID`
  - `agent list | get T | start NAME --kind K [--cwd D] [-- ARGS] | prompt T TEXT [--wait] [--until STATE]... [--timeout MS] | read T [--lines N] | wait T [--until STATE]... [--timeout MS] | send-keys T KEY... | report T --state S [--agent K] | rename T NAME | seen T`
  - `window split [--right|--below] [--terminal ID]` — forwarded to a subscribed editor; `no_editor` if none.
- `agent start` spawns a terminal and returns only once the plugin has
  recognised the agent and it is ready for input (default timeout 30 s).
  `agent prompt` sends text + Enter (bracketed paste if the program asked);
  with `--wait` it refuses when `blocked`, waits up to 5 s to see `working`
  (else `agent_prompt_stalled`), then for `--until` (default
  `idle|done|blocked`). Waits are pending requests, not polling.

## M4 — Editor UI

- The editor subscribes when it connects. Terminals spawned by `ctl` get a
  terminal buffer (not shown until asked); `Editor.agents` mirrors the
  daemon's registry; hook event `AgentStateChanged { buf, state, kind, name }`.
- Mode line: agent badge (`claude ● working`). Tab bar: rollup per tab
  (`blocked` > `working` > `done`). Showing an agent's buffer in the
  selected window marks it seen.
- Commands: `agents` (`C-x a a`, picker over all agents with state and live
  preview), `agent-next-attention` (`C-x a n`, next blocked, then done),
  `agent-start` (`C-x a s`, pick a kind, start it in a new window).
- Notifications on `blocked`/`done` for agents not visible in the selected
  window: a system notification (macOS `osascript`, Linux `notify-send`)
  and the bell. The policy (which states, quiet hours) is Scheme.

## Later

Integrations (`ymacs integration install claude`: Claude Code hooks calling
`ymacs ctl agent report`; session ids for `claude --resume` on restore),
agent per git worktree, the Steel engine in the editor for editor plugins,
remote machines.

## Risks

- **Version skew**: an old daemon from before M3 keeps running until idle
  exit. The editor detects the old protocol version and offers to restart it
  (sessions in it would be lost; say so).
- **Daemon weight**: links Steel and an emulator per session. Measure RSS
  with 20 agents; scrollback in the daemon can be smaller than the editor's.
- **Detection accuracy**: screen rules drift as agent UIs change; that is
  why they are Scheme and user-overridable, and OSC 7501 / reports win.
