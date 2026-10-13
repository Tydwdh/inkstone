# inkstone

A modal terminal editor, written from scratch in Rust. The name is 砚台: the
stone a calligrapher grinds ink on, the thing the brush is dipped into before
anything is written.

The design borrows from both sides of the family: **nvim**'s split between a
headless kernel that owns all state and a thin frontend that only draws, and
**emacs**'s idea that everything is a command a script can call.

```
inkstone  (crossterm frontend)  ──msgpack over stdio──▶  inkstone-server  (headless daemon)
                                                            │
                                                            ▼
                                                       inkstone-core  (editor kernel)
```

The daemon composes the screen as a grid of styled cells; the frontend blits
it. No editor state lives in the frontend, so a future GUI or a remote client
can reuse the daemon unchanged.

## Status

Milestone 1: a walking skeleton. It is usable for basic editing, and
deliberately thin.

Working: open/save, modal editing, movement, insert, `x`, `dd`, `:w :q :wq :q!
:e`, line numbers, tabstop, status line, Lua configuration (`init.lua`).

Not implemented: undo, Visual mode, operators (`dw`, `yy`, `p`), search and
replace, syntax highlighting, LSP, splits, mouse, multiple buffers, cmdline
editing (arrow keys inside `:`), multi-key insert mappings.

## Build and run

Both binaries must end up in the same directory: `inkstone` finds `inkstone-server`
next to itself.

```sh
cargo build --workspace
cargo run -p inkstone -- sample.txt     # or: ./target/debug/inkstone.exe sample.txt
```

`cargo run -p inkstone` alone is not enough on a fresh clone — it builds only the
`inkstone` package, and the daemon has to exist first.

It needs a real terminal (Windows Terminal or conhost): raw mode fails under a
redirected pipe. `inkstone --server <PATH>` points at a daemon somewhere else.

## Keys

`MODE` is `n` (normal) or `i` (insert). `KEYS` uses vim-ish angle notation;
`<C-x>` is Ctrl-X, `<A-x>` is Alt-X, `<S-Tab>` is Shift-Tab. A sequence like
`dd` is two keys pressed in order.

<!-- keymap:begin -->
| mode | keys | command(s) |
| --- | --- | --- |
| n | `h` | `move_left` |
| n | `j` | `move_down` |
| n | `k` | `move_up` |
| n | `l` | `move_right` |
| n | `<Left>` | `move_left` |
| n | `<Right>` | `move_right` |
| n | `<Up>` | `move_up` |
| n | `<Down>` | `move_down` |
| n | `<Home>` | `move_line_start` |
| n | `<End>` | `move_line_end` |
| n | `<PageUp>` | `page_up` |
| n | `<PageDown>` | `page_down` |
| n | `0` | `move_line_start` |
| n | `$` | `move_line_end` |
| n | `w` | `move_word_forward` |
| n | `b` | `move_word_backward` |
| n | `g g` | `goto_first_line` |
| n | `G` | `goto_last_line` |
| n | `i` | `enter_insert` |
| n | `I` | `enter_insert_before` |
| n | `a` | `append` |
| n | `A` | `append_eol` |
| n | `o` | `open_line_below` |
| n | `O` | `open_line_above` |
| n | `x` | `delete_char` |
| n | `<Del>` | `delete_char` |
| n | `d d` | `delete_line` |
| n | `:` | `enter_command` |
| n | `<Esc>` | `escape` |
| i | `<Left>` | `move_left` |
| i | `<Right>` | `move_right` |
| i | `<Up>` | `move_up` |
| i | `<Down>` | `move_down` |
| i | `<Home>` | `move_line_start` |
| i | `<End>` | `move_line_end` |
| i | `<PageUp>` | `page_up` |
| i | `<PageDown>` | `page_down` |
| i | `<Del>` | `delete_char` |
<!-- keymap:end -->

The table above is checked against the built-in keymap by a test, so it cannot
drift.

Keys that are not bindings, because they are what the mode is for:

| mode | key | does |
| --- | --- | --- |
| i | any character | insert it |
| i | `<Enter>` | insert a line break |
| i | `<Tab>` | insert a tab |
| i | `<BS>` | delete the grapheme before the cursor, joining lines at column 0 |
| i | `<Esc>` | back to normal mode, stepping left one character |
| : | any character | append to the command line |
| : | `<BS>` | delete from the command line |
| : | `<Enter>` | run the command |
| : | `<Esc>` | abandon the command |

### Commands on the `:` line

`w` write · `w <path>` write elsewhere · `q` quit (refuses if unsaved) ·
`wq` write and quit · `q!` quit regardless · `e <path>` reload from disk
(refuses if unsaved) · `e! <path>` discard changes and reload.

## Configuration

`init.lua` lives in the platform config directory (`%APPDATA%\inkstone\config\init.lua`
on Windows, `$XDG_CONFIG_HOME/inkstone/init.lua` on Linux). A file that fails to
parse is reported on the status line and ignored; it never stops the editor.

```lua
ed.set("number", true)     -- line-number gutter
ed.set("tabstop", 8)

ed.map("n", "<C-s>", "save")                        -- to a built-in command
ed.map("n", "Q", function() ed.run("quit") end)     -- or to a function

ed.command("strip_trailing_space", function()       -- new command
  ed.run("move_line_end")
end)
```

| call | meaning |
| --- | --- |
| `ed.map(mode, lhs, rhs)` | bind `lhs` in mode `"n"` or `"i"` to a command name, a function, or `nil` to unbind |
| `ed.command(name, fn)` | register (or replace) a command callable by name |
| `ed.run(name, params)` | run a command; the call is queued and executed after the script returns |
| `ed.set(name, value)` / `ed.get(name)` | options: `tabstop` (integer), `number` (boolean) |

Insert-mode mappings must be a single key: typing is resolved one keystroke at
a time, so `ed.map("i", "jk", ...)` is rejected at load rather than silently
never firing.

## Layout

| crate | what |
| --- | --- |
| `inkstone-core` | the kernel: buffer, cursor, modes, command registry, keymap, screen model, Lua host |
| `inkstone-rpc` | the wire protocol: message types, framing, and the single definition of the screen grid |
| `inkstone-server` | the daemon: owns an `Editor`, turns client messages into server frames |
| `inkstone` | the crossterm frontend: terminal events in, grid blits out |

[`ARCHITECTURE.md`](ARCHITECTURE.md) states the invariants and contracts that
cross those boundaries, and where to add the next thing.

## Tests

```sh
cargo test --workspace
cargo clippy --workspace --all-targets
cargo fmt --all --check
```

The suite includes a deterministic 4000-keystroke stress test that re-checks
the buffer and viewport invariants after every key, and an end-to-end test that
drives the real daemon binary over its pipes and inspects the bytes it writes.
The keymap table above is compared against the built-in bindings by
`keymap::tests::the_readme_keymap_table_matches_the_defaults`, so it cannot
quietly go stale.
