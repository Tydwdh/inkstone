# Windows: a view of a document

A document is a file and what belongs to the text — the undo history, the
search, the syntax colours. A *window* is a look at a document: where the cursor
is, and where the window is scrolled to. Two windows can look at one document,
and then they share every character and none of the position.

That split is what this note is about, because the editor learned documents
[`buffers.md`](buffers.md) first and put the cursor in the wrong one: a cursor
belongs to a look, not to a file. Two windows on one file with one shared cursor
would move together, which is not what anyone means by a split.

## The model

```
Editor
├── documents: Vec<Document>     the files: buffer, history, search, highlighter
├── windows: Vec<Window>         the looks: document, cursor, top_line, left_col
├── current_window: usize
├── buffer, history, search,     the current window's document, live
│   highlighter
├── cursor, viewport.top/left    the current window, live
└── mode, register, keymap,      the session
    cmdline, message, …
```

The same trick as documents, one level up: the current window and the current
window's document keep their state in the editor's own fields, every other one
keeps it in its slot, and switching swaps the two. A command reading
`self.cursor` is reading the cursor of the window that is up, and one reading
`self.buffer` is reading the document it shows.

Two invariants hold it together: `windows[current].document` is the slot the
live document came from, and that slot is a placeholder while the document is
live. Switching windows saves the live window and the live document, moves
`current_window`, and loads the new window and *its* document — which may be the
same document, in which case the document is not swapped at all and only the
position is.

## What a window owns, and what it does not

| Owns | Does not own |
| --- | --- |
| the cursor (row, col, and its wish column) | the text |
| the scroll position (top line, left column) | the undo history |
| which document it shows | the search and its matches |
| | the syntax colours and their per-line state |
| | the size of the screen |

The screen's size lives in the viewport, which mixes it with the scroll — the
same seam as before: `save_window` stores the scroll and leaves the size alone.
The mode is the session's, since only one window can be in insert mode, and a
window that is switched away from comes back in normal mode, having had the
cursor clamped to a real character on the way out.

## The layout

Windows tile one rectangle. The layout is a tree:

```rust
enum Layout {
    Leaf(usize),                                   // a window
    Split { axis: Axis, children: Vec<Layout> },   // side by side, or stacked
}
```

A split divides its rectangle in equal shares along its axis. `:sp` and `:vs`
split the current window's leaf, and a split that already runs the way the new
one does takes the new window as another child rather than nesting inside the
first — so three `:sp`s are one division of the screen into three, not a tree
three deep. Closing a window removes its leaf, and a split left with one child
becomes that child: the tree never keeps a division that divides nothing.

Tree nodes carry no *laid-out* sizes: the rectangles are recomputed from the
screen every frame, so a window that is resized, or a terminal that is, needs
nothing to be kept up to date. What a split does carry is what each child *asked*
for — a `:resize 5` on a window, or nothing at all, which is the common case and
means equal shares of whatever is left. A share is the window's own rows or
columns: a stacked window keeps one row of it for its status line, and the row
between two windows is that status line, so a division costs no row of its own.

The two axes spend their separators differently, and the difference is vim's:

- **Stacked** windows need no extra row at all: the row between two of them is
  the *status line* of the upper one, which was going to be there anyway. The
  division costs a row and the row carries something.
- **Side by side** windows need a column, and it holds `│` — one cell, drawn by
  the renderer.

## Rendering

`compose` walks the tree once, handing each leaf a rectangle, and draws that
window into it: the text from *its* document, with *its* scroll position, *its*
document's syntax colours and search matches, and *its* status line on the last
row of the rectangle. Only the current window draws a cursor and a selection.

One seam shows up here and nowhere else: the document of the window that is up
lives in the editor's own fields, and its slot in `documents` holds a
placeholder. A *second* window on that document — `:sp` on a file you are
already looking at — therefore cannot read it from the slot, and the renderer
reads the text from the view itself and only the position from the window. Get
that wrong and the second window goes blank, which is exactly what the first
version did.

This is the whole reason splits are cheap here: the wire carries a grid of
cells, and a grid does not know whether it holds one window or four. Neither
frontend learns anything about windows, and the daemon keeps sending rows. The
cost of the alternative — letting each frontend lay out its own panes — is the
same cost as letting it hold its own selection: two implementations that
disagree, and a cursor that is in the wrong place after a resize.

## Commands

| Command | What it does |
| --- | --- |
| `:sp [file]` | split the window in two, one above the other; with a file, show that in the new one |
| `:vs [file]` | the same, side by side |
| `:q` | close the current window; on the last window, quit the session |
| `:on` | close every window but this one |
| `<C-w>w` | the next window |
| `<C-w>j` `k` `l` `h` | the window in that direction |
| `<C-w>s` `v` | `:sp` and `:vs` |
| `<C-w>c` | `:q` |

A direction with no window in it says `E16: Invalid range`, and any of the
window keys with only one window says `E85: There is no other window`. A click
in a window that is not the current one goes to that window first, which is what
makes a click a click rather than a jump in the wrong file.

Direction is decided from the rectangles, not from the tree: "down" means the
nearest window below that overlaps this one sideways. With equal shares and a
tree of at most a few windows, the rectangles are the honest answer and the
nearest-neighbour walk is shorter than the tree logic that would answer it
exactly.

## What is not here

Per-window marks, and the `'winminheight'` sort of option that decides how small
a window may get. `:only` closes the other windows without asking questions,
where vim would ask: nothing is lost with the window, since the document it was
showing stays open. Two windows can also show the same document while its undo
history is shared by both — which is correct, and means an undo in one window
redraws the other.
