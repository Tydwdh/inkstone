# Working on inkstone

Orientation for a new contributor — human or agent — picking this up cold.
[`README.md`](README.md) is for users, [`ARCHITECTURE.md`](ARCHITECTURE.md) is
the contract, [`docs/roadmap.md`](docs/roadmap.md) is what is done and what
comes next, and the design notes under [`docs/`](docs) — `undo`, `windows`,
`buffers`, `selection`, `highlighting`, `rendering`, `lsp` — are how each of
those areas was decided. This file is how to work on it without breaking it.

## The one-paragraph summary

A modal terminal editor in Rust, split into a headless kernel, a daemon that
hosts it and speaks MessagePack over stdio, and a dumb crossterm frontend that
only draws the grid the daemon composes. Everything the editor *is* lives in
`inkstone-core`; the two binaries are thin. Nothing is tokio; the editor never
crosses a thread boundary.

## Commands

```sh
cargo build --workspace --locked
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo fmt --all --check
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --locked
```

All five run in CI — the build and the tests on Windows and Linux, fmt, clippy
and doc on Linux. **Run all five before committing.** A
change is not done because it compiles; the gate is the definition of done.

Check the **exit status** of each. Grepping for the absence of a warning also
comes back clean when the build failed outright, which is an easy way to
believe a broken tree is fine.

`cargo run -p inkstone -- file.txt` works only if `inkstone-server` has been
built, because the frontend looks for the daemon next to its own executable —
`cargo build --workspace` first.

## What is not tested, and how to check it

The workspace is covered by 565 tests, including one that drives the real
daemon binary over pipes and inspects the bytes it writes. The kernel, the
protocol and the daemon are 494 of them; the rest belong to the frontends, the
client and the LSP crate.
**A frontend's drawing is not covered by CI and cannot be**: raw mode needs a
real terminal, and a window needs a real window. Both the window's metrics and
the terminal frontend's key translation are unit tested; what the pixels or the
terminal do with them is checked by hand.

So after touching anything visible — rendering, input, the daemon's startup —
run it in a terminal:

```sh
printf 'alpha\nbeta\n' > /tmp/t.txt && ./target/debug/inkstone /tmp/t.txt
# then: j to move, i to type, Esc, :wq
```

Under a pseudo-terminal you can automate it. With the `bash` tool in pty mode:

- start it as a named service: `./target/debug/inkstone t.txt`
- read the screen: `read proc://<name>`
- type at it: `write proc://<name>` with the keys as content

Two quirks of that harness: it appends Enter to what you write, and it cannot
send Escape. So drive normal-mode-only sequences (`Gdd`, `:wq`) and check the
file afterwards with `od -c`. Always use a `.exe` suffix on Windows and
relative paths — the shell in pty mode is not the one that runs your `cd`.

## Where things live

| crate | what | tests |
| --- | --- | --- |
| `inkstone-lsp` | the language server client: framing, the lifecycle, positions |
| `inkstone-core` | document, cursor, edit history, register, search, modes (normal, visual, insert, command), commands, keymap, screen model, Lua host | unit tests in each module |
| `inkstone-rpc` | the wire: messages, framing, the screen grid types, the screen a frontend holds | framing round trips, deltas |
| `inkstone-server` | the daemon: an `Editor` plus a session loop | `tests/session.rs` (in-process and end-to-end) |
| `inkstone-client` | spawning the daemon, framing, folding frames — shared by the frontends | path resolution |
| `inkstone` | the crossterm frontend and launcher | key translation, drawing, terminal checks |
| `inkstone-gui` | the windowed frontend: the same grid on the GPU | key translation, cell metrics, palette |

Read in this order to understand it: `inkstone-core/src/lib.rs` (module map) →
`editor/mod.rs` (the `Editor` and its state) → `editor/keys.rs::handle_key`
(the mode state machine, the one entry point) → `render.rs::compose` (how a
screen is made) → `inkstone-rpc/src/lib.rs` (what crosses the boundary) →
`inkstone-server/src/lib.rs::Session::handle` (and
`crates/inkstone-client/src/lib.rs` for how a frontend talks to it).

The kernel is one type in a directory: `editor/` has a file per concern
(`keys`, `insert`, `edits`, `ex`, `prompt`, `folds`, `pointer`, `lists`, `lsp`,
`windows`, `visual`, `options`, `documents`) and `editor/tests/` has one file per
topic for the tests. `ARCHITECTURE.md` has the table. A method that only its
siblings call is `pub(super)`; one a sibling *module of the crate* calls is
`pub(crate)`; only what the daemon or a frontend uses is `pub`.

## Rules that are enforced, not just written down
- **Every public item is documented.** `#![warn(missing_docs)]` is on in all
  three libraries and CI runs clippy with `-D warnings`.
- **Doc links must resolve.** `cargo doc` runs with `-D warnings`.
- **The README key table must match the keymap.** A test parses it out of the
  README and compares it against `Keymap::with_defaults`.
- **Command names must exist.** `commands!` expands to `register(stringify!(name),
  name)`, so a typo is a build error, not a dead binding.
- **A new setting, key or attribute cell cannot be half-added**: the
  declaration macros generate the enum, the list and the lookup, and tests in
  both crates iterate those lists.
- **ARCHITECTURE.md's cost table** says which files each kind of change reaches;
  keep it true when you add a new kind.

## Conventions

- **A bug fix starts with the failing test.** Reproduce it, watch it fail, then
  fix. Every fix in the changelog was done that way, and it is why the suite
  catches regressions.
- **Invariants are the contract.** The line model (a terminator is not content),
  the cursor (`col` ranges to `line_len`, and normal mode clamps off the
  terminator), grid cells (empty text means a continuation cell), message
  lifetime, and the Lua callback queue. `ARCHITECTURE.md` states each with the
  reason; read it before changing anything in those areas.
- **Every edit goes through `Editor::replace_range`.** It is the one place a
  change is recorded, so a new command that mutates `buffer` directly would
  compile and then quietly be the one change `u` cannot undo.
- **Prefer deleting to adding.** Dead API with no caller outside its own test
  goes.
- **Commits** are conventional (`fix:`, `refactor:`, `docs:`, `chore:`) and
  explain *why*, in full sentences. One coherent change per commit.
- **Look at `git status` before staging.** Do not `git add -A` blindly: the
  working tree can contain files that are not yours.

## Environment notes

- `mlua` builds Lua from source (`vendored`), so a C compiler is required —
  MSVC on Windows, gcc/cc elsewhere.
- `.cargo/config.toml` disables crates.io's TLS revocation check. That is a
  workaround for this machine's proxy, not a project decision.
- Windows terminals must speak ANSI (Windows Terminal, or conhost with
  `ENABLE_VIRTUAL_TERMINAL_PROCESSING`). Raw mode fails under a redirected pipe.
- The frontend finds its daemon as a sibling executable, or via
  `inkstone --server <PATH>`.
- **`cargo clippy` caches, and its dead-code analysis is per target.** A "never
  used" warning can be stale (it once named a method a test calls) and it counts
  the library build, so a method only tests use is always reported. Before
  deleting anything on that warning: grep the tests. If only tests use it, that
  is what `#[cfg(test)]` is for.
