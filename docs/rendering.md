# Rendering: the wire, and a second frontend

What the daemon sends, what it costs, and what it has to send before a frontend
can draw the screen on the GPU. The daemon/frontend split exists so that a
frontend can be replaced; this note is what that costs, measured rather than
guessed. It is also the design note [`roadmap.md`](roadmap.md) asks for before
anything here changes `inkstone-rpc`.

## What a screen costs today

Measured on a real session. The daemon sends a whole screen once, and after that
only the rows that changed, as runs of identical cells.

| viewport | full frame, before any of this | full frame now | a keystroke that moves the cursor | a keystroke that edits a line |
| --- | --- | --- | --- | --- |
| 80x24 | 63,429 B | **11,543 B** | **232 B** | **570 B** |
| 200x60 | 396,070 B | **72,024 B** | **232 B** | **572 B** |

Three decisions got it there, all taken:

- **Compact MessagePack.** `write_msg` uses `to_vec`, not `to_vec_named`: a
  struct travels as an array of its fields instead of a map keyed by field
  names. Worth 2.1x on its own, because a screen is 2,000 identical little
  structs.
- **Packed colours.** `Color` travels as one integer (kind in the low two bits,
  payload above it) instead of an enum variant, whose *name* cost six bytes —
  twelve for the two colours of every blank cell. That is the difference between
  a blank cell costing 16 bytes and costing 6.
- **Row deltas, as runs.** `ServerMsg::Rows` carries the rows that changed, each
  as runs of identical cells, and a keystroke that changes nothing but the
  cursor carries no rows at all. Runs are what makes the cost independent of the
  window: the status row's reverse-video padding is one run of a couple of bytes
  instead of two hundred cells.

What is left of the 232 B is the *text* of the status line, one run per
character, because a run means identical cells. Collapsing those into "one
style, this string" would need the frontend to know how wide a grapheme is —
which is exactly the kind of layout knowledge that lives in the daemon today —
and 232 B per keystroke over a pipe is not a reason to move it. If it ever
becomes one, this is where to look.

## Next: nothing on the wire

A second frontend can be built against what is here. What it needs before it can
be *good* is in the two sections below.

## Before a GPU frontend

The two protocol gaps this note listed are closed: the attribute vocabulary now
covers italic, faint and strikethrough (`cell_attributes!` is still one line per
bit, so a renderer that can draw more can say so), and `ClientMsg::Text` carries
text that was never a key, which is what an input method commits and what a paste
is. A windowed frontend needs nothing else from the wire.

What remained was the frontend itself: shaping and font fallback (cosmic-text
does it), IME plumbing, selection and clipboard, mouse reporting, key repeat, and
knowing what a "line" is on screen. That was the real cost of a windowed
frontend, not the GPU calls — and the next section is what came of it.

## `inkstone-gui`

The crate exists, and it is a frontend in the shape of the crossterm one: it
spawns the same daemon through `inkstone-client`, speaks the same framing, folds
the same frames into the same `Screen`, and draws what is left to draw. Nothing
in `inkstone-core` and nothing in the protocol's shape had to change for it.

- **Stack:** `winit` 0.30 for the window and events, `wgpu` 30 for the surface,
  `cosmic-text` 0.19 (shaping, bidi, font fallback, CJK and emoji) over
  `swash`/`fontdb`.
- **Drawing:** one instanced quad pipeline, one draw call per frame, for the row
  backgrounds, the glyphs, the underlines and strikethroughs, and the cursor. The
  instance buffer is laid out *per row*, so a delta frame rebuilds and uploads
  only the rows the daemon named. Glyphs are shaped and rasterized into an atlas
  that grows on demand; `Color::Reset` means the window theme, and the 256-colour
  palette is computed rather than pasted in.
- **What a row must not forget:** the frame draws every row's whole reservation,
  and the slots a row is not using are quads that draw nothing — but only if
  they are *zeroed*. A row that needs fewer quads than last frame (a deleted
  line, a shorter status line) has to clear the rest, or the old glyphs stay on
  screen. That is the one bug this design produced, and it is pinned by a test.
- **Modifiers:** the window and the terminal disagree about where Shift lives,
  so the rule is the terminal frontend's: Shift is baked into a character, Ctrl
  and Alt become modifiers, `<S-Tab>` is Tab with Shift.
- **Input methods:** an IME belongs where text goes and nowhere else. Left on in
  normal mode it eats exactly the keys that run commands — `dd`, `:wq` — so the
  window allows it only while a frame says the editor is inserting text, and the
  cursor is drawn as a bar there instead of a block. That is why the frames carry
  `insert` at all.
- **Verification:** the framing half is headless-tested (key translation, the
  window-to-grid arithmetic, the palette, the quad encoding, the shader through
  `naga`). The window itself is checked by hand against the terminal frontend on
  the same file — both compose the same grid, so the screens are comparable cell
  by cell.

Still missing, and each its own change: key repeat for held keys, and smooth
scrolling. Neither needs the wire to change.

## What stays true

- The daemon owns every decision; a frontend blits what it is sent. A shadow grid
  is a cache of the *screen*, never of the document, the cursor or the modes.
- A new frontend still needs exactly three things: framing, a grid blit, and
  translating terminal or window events into `KeyMsg`.
- Nothing here is allowed to make the crossterm frontend worse: it is the
  frontend that runs everywhere, including over ssh and inside tmux.
