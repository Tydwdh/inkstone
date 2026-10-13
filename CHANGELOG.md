# Changelog

## Unreleased

### Fixed
- Arrow keys, `<Home>`, `<End>`, `<PageUp>` and `<PageDown>` did nothing in
  insert mode — the keys you reach for most while typing. Navigation is now an
  ordinary insert-mode binding set, so `ed.map("i", ...)` can override it.
- `<PageUp>`/`<PageDown>` moved a single line instead of scrolling a screen.
- `ed.map("i", "jk", ...)` was accepted and then never fired. It is now
  rejected at load with an explanation.

### Known gaps
- Multi-key insert mappings are not implemented.
- The `:` command line has no cursor movement (no arrows, no history).

## 0.1.0 — walking skeleton

### Added
- A headless daemon (`edit-server`) that owns all editor state and composes the
  screen, and a crossterm frontend (`edit`) that only draws it, talking over
  length-prefixed MessagePack frames on stdio.
- A modal editing model: normal, insert and command-line.
- A command registry with 29 built-in commands, and a keymap that a script can
  extend with `ed.map`.
- An embedded Lua host: `init.lua` can remap keys, register commands and set
  `tabstop` and `number`.
- Basic editing: open/save, `h j k l`, `w b`, `0 $`, `gg G`, `i I a A o O`,
  `x`, `dd`, and `:w :q :wq :q! :e`.
- A rope-backed buffer, a composable screen grid, and a status line.

### Fixed
Defects found by a bug pass over the skeleton; each was reproduced by a failing
test before it was fixed:

- CRLF files were edited through their raw bytes: `A` appended *after* the
  carriage return, `$` parked the cursor on an invisible character, and `dd`
  left a stray `\r` behind.
- Normal mode let the cursor sit one column past the last character.
- The cursor's screen column came from a character count, so tabs and
  double-width graphemes put it on the wrong cell.
- `w` skipped empty lines instead of stopping on them, and swallowed a
  following punctuation run.
- `$` landed on the line terminator rather than the last character.
- The line-number gutter was invisible to the scroll maths.
- A terminal reporting a zero-sized window produced a meaningless scroll offset.
- `:e` discarded unsaved changes without asking.
- The Delete key did nothing in insert mode and was unbound in normal mode.
