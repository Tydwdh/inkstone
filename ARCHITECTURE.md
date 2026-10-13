# Architecture

What crosses each boundary in `inkstone`, and the invariants that hold on the other
side of it. Everything here is enforced by tests; the point of writing it down
is that a new feature has to respect these, not rediscover them.

> Working on the code? Start with [`AGENTS.md`](AGENTS.md) (commands, rules,
> what CI cannot check) and [`docs/roadmap.md`](docs/roadmap.md) (what is next,
> and the constraints it inherits from here).

## Processes and threads

`inkstone` (frontend) spawns `inkstone-server` (daemon) as a child and talks to it over
the child's stdin/stdout. There is exactly one writer to each direction.

| process | thread | owns |
| --- | --- | --- |
| `inkstone` | main | the terminal, the draw loop, the child handles |
| `inkstone` | `inkstone-input-reader` | blocking `event::read()` |
| `inkstone` | `inkstone-rpc-reader` | reading frames from the daemon |
| `inkstone-server` | main | the `Editor`, the output stream, the Lua VM |
| `inkstone-server` | `inkstone-server-rpc-reader` | blocking read of client frames |

The editor never crosses a thread boundary, so it needs no locks. The only
shared state is messages in a `crossbeam-channel`.

## The wire

Frames are `u32` big-endian length + MessagePack payload, capped at 64 MiB
(`inkstone_rpc::MAX_FRAME_LEN`). Milestone 1 is notification-only: no request ids,
no replies. A clean EOF (zero bytes of a header) means the peer is gone.

```mermaid
sequenceDiagram
    participant F as inkstone (frontend)
    participant D as inkstone-server
    F->>D: Resize{cols,rows}
    F->>D: Command{name:"edit_file"}
    D-->>F: Redraw{grid,cursor,cursor_visible}
    F->>D: Input{KeyMsg}
    F->>D: Input{KeyMsg}
    D-->>F: Redraw{...}
    F->>D: Input{':','q','!',Enter}
    D-->>F: Redraw{...}
    D-->>F: Quit
    F->>D: Shutdown
```

The daemon emits one frame per client message that could have changed the
screen (`Editor::redraw`), plus one before it starts reading, so a frontend
that only reads still sees a screen. `ClientMsg::Input` is a *key*, never a
character: mode logic lives in the daemon.

`Grid`/`Cell`/`Color` are defined once, in `inkstone-rpc`. `inkstone-core` re-exports
them; it does not define its own.

## The screen model

A `Grid` is `width * height` cells, row-major, and it is always fully
populated — empty screen space is a cell containing `" "`. That makes a grid
self-describing: a frontend writes every cell and never has to clear first.

A cell whose `text` is **empty** is a *continuation cell*: the second half of a
double-width grapheme stored in the cell to its left. Frontends must skip those
when drawing, or they will blank the right half of every CJK character.

Row layout is fixed: `height - 2` text rows, then the status line (reverse
video, edge to edge), then the command/message line.

The cursor's screen column comes out of the same walk that draws the line
(`render::draw_line` returns it), so tabs and double-width graphemes cannot
leave the cursor on the wrong cell.

## The line model

`Buffer` text always ends with `'\n'`. An empty buffer is one empty line.

A line is **content plus its terminator**, and the terminator is not part of the
content: `Buffer::line` strips it, `Buffer::line_terminator` returns it. That is
what makes `$`, `A`, `o` and `dd` behave on a CRLF file, and it means
`Buffer::text` still returns the original bytes — opening and saving never
rewrites line endings.

The set of terminators is the one the rope recognises (LF, CRLF, CR, VT, FF,
NEL, LS, PS), so the two models can never disagree about how many lines a
buffer has.

## The cursor

`col` is an *insertion point*: `0..=line_len(row)`, where `line_len` excludes
the terminator. A column of `line_len(row)` means "just past the last
character", which is where insert mode belongs.

Normal mode does not belong there. `Editor::clamp_normal_cursor` keeps the
cursor on a real character whenever the mode is normal, which is what makes
`x`, `$` and `l` behave at the end of a line. It runs before dispatching a
normal-mode key and after every edit.

## Modes

| mode | dispatch |
| --- | --- |
| normal | keymap → command registry, with a pending buffer for multi-key sequences |
| insert | insert-mode keymap first, then built-in text entry; mappings are single-key |
| command | edits a command line, executed on `<Enter>` by `Editor::execute_cmdline` |

A command is `fn(&mut Editor, &[Param]) -> Result<()>` in a flat registry
(`CommandRegistry`), addressable by name, with a numeric first parameter treated
as a repeat count. Commands cannot capture state, which is what lets a script
layer enumerate and call them.

Dispatch order for a name: built-in registry → Lua command → `E492`.

## Lua

Scripts reach the editor through one global, `ed`. They cannot borrow it: the
editor owns the Lua VM, so a callback borrowing the editor would alias. Instead
`ed.run(...)` **queues** the call and `Editor::run_command` drains the queue
after the Lua call returns, bounded by `MAX_COMMAND_DEPTH` (100).

`init.lua` is sourced once at startup. A failure is reported on the status line
and ignored — a config typo must not stop the editor from opening.

## Messages a user can see

| text | when |
| --- | --- |
| `E32: No file name` | writing a buffer that has no path |
| `E37: No write since last change (add ! to override)` | `:q` or `:e` with unsaved changes |
| `E492: Not a command: X` | unknown name on the `:` line, or from `ed.run` |
| `E5108: command nesting too deep` | runaway Lua command recursion |
| `unmapped key: X` | normal-mode key with no binding |
| `init.lua: ...` | the config failed to load |
| `"path" written` / `"name"` | a successful write / a freshly opened file |

A message survives until the next key pressed *outside* command mode, so the
key that runs a `:` command does not erase what it just reported.

## What a change costs

The recurring shapes — keys, cell attributes, settings, commands — are declared
once, in a macro, and everything else is generated from that declaration:
`key_codes!` (`inkstone-rpc`) emits the enum, `ALL` and the spelling table;
`cell_attributes!` emits the constants and `ATTRS`; `settings!` and `commands!`
(`inkstone-core`) emit the setting enum, the options struct, its defaults and the
`set`/`get` pair, and the command registry. There is no second list to keep in
step, which is what makes a forgotten case a build error instead of a runtime
surprise.

| change | files touched | what catches a miss |
| --- | --- | --- |
| new command | write the handler in `command.rs`, add its name to the `commands!` list, optionally a default key + a README row | a name with no function is a build error; the README test |
| new motion | `cursor.rs` (pure) + one thin command | compiler |
| new setting | one line in the `settings!` table (plus a `SettingValue` impl for a new type) | nothing else to touch; the generated test round-trips it |
| new key | one line in the `key_codes!` table | the frontend's mapping test fails to compile until it is mapped |
| new cell attribute | one line in `cell_attributes!`, then `render.rs` and `inkstone/src/main.rs` | the frontend attribute test |
| new message | `inkstone-rpc`, `inkstone-server` (`Session::handle`), `inkstone` (frontend match) | exhaustive matches |
| new mode (e.g. visual) | `mode.rs`, `editor.rs` (dispatch), `command.rs`, `keymap.rs`, `lua.rs` (name), `render.rs` (display) | exhaustive matches on `Mode` |
| new frontend | nothing existing | — (that is the point of the split) |

A new mode touching six files is not a design failure: a mode *is* a key
handler, a set of commands, a thing scripts can name and a thing to draw. What
matters is that each touch is one match arm, and that three of the six are
compiler-enforced.

## Adding things

- **A new command**: write the function in `command.rs`, register it in
  `CommandRegistry::with_builtins`, bind it in `Keymap::with_defaults` if it
  deserves a default key, and add a row to the README's keymap table (a test
  compares that table to the built-in keymap).
- **A new motion**: implement the movement on `Cursor` (pure, buffer in,
  position out), then a command that calls it and ends with `after_edit()`,
  which re-clamps the cursor, scrolls it into view and marks the screen dirty.
- **A new setting**: add one line to the `settings!` table in `settings.rs`.
  The enum, the struct, the default, the name lookup and `set`/`get` all follow
  from it. A new *type* of value needs a `SettingValue` impl saying how it maps
  to and from `Param`.
- **A new key**: add one line to the `key_codes!` table in `inkstone-rpc`. The
  notation layer needs nothing; the frontend is forced to map it, because the
  test that iterates `KeyCode::ALL` matches over the enum exhaustively.
- **A new cell attribute**: add one line to `cell_attributes!`, then teach
  `render.rs` to emit it and the frontend to translate it — the attribute test
  fails until both do.
- **A new command**: write `fn name(editor: &mut Editor, params: &[Param])` in
  `command.rs` and add `name` to the `commands!` list. The name is not repeated
  anywhere, so it cannot be registered under the wrong one.
- **A new message**: add it to `ClientMsg`/`ServerMsg`, handle it in
  `Session::handle`. Frontends are free to ignore anything they do not know.
- **A new frontend**: implement the framing, blit `Grid`, forward terminal keys
  as `KeyMsg`. Nothing else is required — that is the whole contract.

## Deliberate limitations

- Sending a redraw per keystroke is fine at this size; there is no diffing.
- Horizontal scroll is measured in characters, not display columns, so a tab in
  the scrolled-off region can leave the view a column off.
- A brand-new empty file is written as one byte (`"\n"`), which is stable on
  reload but differs from vim's zero bytes.
