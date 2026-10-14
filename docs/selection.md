# Selection, the pointer, and the clipboard

What a drag selects, who owns it, and how text gets in and out of the OS
clipboard. This is the design note [`roadmap.md`](roadmap.md) asks for before
anything here changes `inkstone-rpc` — and it changes the wire twice, for the
pointer and for the clipboard.

## The selection is the editor's

There is already one: visual mode's, an anchor and a kind with the cursor as the
other end. A drag makes the *same* thing, so a mouse selection and a keyboard
selection are the same object, and every operator that works on one works on the
other — `d`, `y`, `c`, `p`, and the highlight.

What that rules out is the tempting alternative: a frontend that keeps its own
selection and copies text out of the cells it holds. It would work, once, and
then disagree with the editor about what a line ends with, what a tab is, and
what a yank means.

```
            press/drag/release            buffer position
   window ─────────────────────▶ daemon ────────────────▶ selection
      ▲                             │
      │  ServerMsg::Redraw/Rows     │  ServerMsg::Clipboard
      └─────────────────────────────┘
```

The frontend sends *screen cells*; the daemon turns them into buffer positions,
because only the daemon knows the layout — the gutter, the tabstops, the wide
graphemes and the horizontal scroll are all its business.

## The pointer

```text
ClientMsg::Pointer { kind, x, y }      x, y are screen cells, zero-based

kind: Press    put the cursor at the cell, and start a characterwise selection
      Drag     move the selection's moving end to the cell
      Release  end the drag: a press that never dragged collapses to the cursor
      ScrollUp / ScrollDown   the view, three lines, without moving the cursor
```

Three decisions in that table:

- **A press starts a selection, and a release that never dragged collapses it.**
  That is what makes a click and a drag the same gesture with different ends,
  and it is why the daemon remembers whether a drag happened between them.
- **A click puts the cursor on the cell under it** — through the same walk that
  draws the line, run backwards, so a click on a tab lands on the tab and a
  click on the right half of a wide grapheme lands on the grapheme.
- **Scrolling is the view, not the cursor.** A wheel moves what is on screen;
  the cursor stays where the user left it, which is what every other editor
  does and what makes a wheel-over-a-long-file harmless.

Out of scope for now, each its own change: double-click word selection,
triple-click lines, block selection, dragging past the edge to scroll, modifier
keys on a click, and mouse reporting *by an application* to a program running in
the editor (there is no terminal-in-a-terminal here yet).

## The clipboard

The clipboard belongs to the system, and neither the daemon nor the terminal is
the system. So copy is two steps: the daemon says what the text is, and the
frontend writes it where text goes.

```text
ClientMsg::Copy          the visual selection if there is one, else the register
ServerMsg::Clipboard(s)  what to write: bytes, exactly as the buffer holds them
```

- **The window** writes it to the OS clipboard, and `Ctrl+V` reads that clipboard
  back and sends it as `ClientMsg::Text` — the message that already exists for
  text that was never a key. A paste therefore arrives as one undoable change,
  whatever it spells.
- **The terminal** has no clipboard API, but it has OSC 52: the frontend can ask
  the terminal to put text on the system clipboard, which Windows Terminal,
  iTerm2, kitty and wezterm all honour. Pasting in a terminal is the terminal's
  own paste (`Ctrl+Shift+V`, or a middle-click), which arrives as `Text` where
  the terminal speaks bracketed paste and as keys where it does not.
- **The register is not the clipboard.** Copying *out* reads the register;
  pasting arrives as text and does not touch it. Vim's `"+` and `"*` registers
  are their own feature, and this note deliberately does not grow them.

## What has to stay true

- A frontend forwards cells and draws rows; it never decides what a position
  means, and it never copies out of its own grid.
- Nothing here needs a request id: every message is a notification in one
  direction, the way the rest of the protocol is. The daemon does not wait to be
  asked.
- The pointer is optional. A frontend without one — the terminal, over `ssh` —
  loses nothing but clicking.
