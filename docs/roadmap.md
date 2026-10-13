# Roadmap

What is done, what is next, and what each next thing has to respect. The
constraints are the useful half: they are the invariants from
[`ARCHITECTURE.md`](../ARCHITECTURE.md) applied to work that does not exist yet.

## Done — milestone 1, the walking skeleton

Headless daemon plus dumb crossterm frontend over MessagePack on stdio; a rope
buffer whose line model treats a terminator as structure, not content; normal,
insert and command-line modes; a command registry with 29 built-in commands; a
keymap a script can extend; a Lua host; line numbers and tabstop; save, quit,
reload, with the refusals (`E32`, `E37`) that stop data loss.

## Next, roughly in order

Each item is meant to be *one* coherent change. If one grows into touching six
modules with new logic in each, that is a sign it should be split.

### 1. Undo and redo

The first feature that needs state the editor has never had.

- **What:** an edit history with `u` and `<C-r>`, undo/redo restoring both the
  text and the cursor.
- **Respects:** a terminator is not content, so an undo of `dd` has to put the
  terminator back with the line; the normal-mode cursor clamp must be re-applied
  after undo, or the cursor can end up on a terminator; a save clears
  "modified", not the history.
- **Design note:** group changes that a user thinks of as one (a whole insert
  session, one `dd`) rather than per character, and store character ranges, not
  rope slices, so a history entry stays valid after later edits.
- **Files:** a new `history.rs` in `inkstone-core`, plus a field on `Editor`, two
  commands, two default bindings, and rows in the README key table.

### 2. Operators and registers

`dw`, `cw`, `d$`, `yy`, `p`, `P` — the motions to select by, and a register to
put through.

- **What:** an operator takes a motion (or a doubled key for a line) and a
  register; text yanked is remembered.
- **Respects:** operators must not push the cursor past the end of a line into
  normal mode; `dd` already encodes the "line plus its terminator" rule, and
  `d`+motion should reuse it rather than re-implement it.
- **Files:** the pending-key machinery in `editor.rs` already handles a two-key
  sequence; this needs it to carry an operator, plus commands and a register on
  `Editor`.

### 3. Visual mode

A fourth mode, and the first test of the mode machinery.

- **What:** `v` selects characters, `V` selects lines; operators act on the
  selection.
- **Respects:** the selection is a buffer range, so it must survive edits that
  move it; the renderer highlights with the attribute bits the protocol already
  has, so no protocol change is needed.
- **Files:** `mode.rs`, the dispatch in `editor.rs`, commands, default bindings,
  `Setting`-free rendering in `render.rs`, and `parse_mode` in `lua.rs` so
  scripts can name the mode. Six files, each one arm.

### 4. Search

`/pattern`, `n`, `N`, and highlighting matches.

- **What:** a search prompt reuses the command line; matches are highlights.
- **Respects:** a match must never straddle a terminator invisibly; the highlight
  path is the same one syntax highlighting will use, so build it once.
- **Files:** a search module, a `:`-line-like prompt, and the renderer.

### 5. Command-line editing

Arrow keys and history inside `:`.

- **What:** move within the command line, recall previous commands.
- **Respects:** the command line is not a buffer; keep it a `String` plus a
  cursor index rather than reaching for `Buffer`.
- **Files:** `editor.rs` only, plus tests.

### 6. Multi-key insert mappings

`ed.map("i", "jk", "<Esc>")`.

- **What:** insert mode currently resolves one key at a time and rejects longer
  mappings at load.
- **Respects:** typing must stay immediate, so a pending insert sequence has to
  be *flushed as literal text* when it turns out not to be a mapping. That
  means `Editor::pending` becomes a typed key queue rather than a string.
- **Files:** `editor.rs` (pending), `keymap.rs` (drop the single-key rule),
  `lua.rs` (the error goes away).

### 7. Bigger seams, not yet designed

Syntax highlighting, LSP, splits, multiple buffers, mouse, a GUI frontend.
Each one should get its own design note first if it wants to change
`inkstone-rpc`; the further features are from the protocol, the cheaper they
are, which is the point of the split.

## Known gaps that are not features

- Horizontal scroll counts characters, not display columns, so a tab in the
  scrolled-off region can leave the view a column off.
- A brand-new empty file is written as one byte (`"\n"`) rather than vim's zero.
- The daemon sends a whole grid per keystroke; there is no diffing.
- No file-change detection: if another process writes the file, `:w` overwrites
  it without asking.
