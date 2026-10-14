# Documents, and what belongs to one

The editor used to have exactly one file open, because it had one buffer, one
cursor, one history, one search and one viewport, all of them fields of
`Editor`. More than one file at a time is not a feature bolted onto that shape —
it is the shape, asked a question it did not have to answer before: *which* of
those fields belong to the file and which belong to the session?

## The split

```
Editor
├── documents: Vec<Document>      every open file
├── current: usize                which one is on screen
├── buffer, cursor, history,      the document that is on screen, live
│   viewport, search, highlighter
└── register, mode, keymap,       the session: what a key means, what was
    lua, cmdline, message,        yanked, what is being typed, what it said
    options, selection, …
```

A `Document` is a buffer and everything that follows it through the file: the
cursor, the history, the search and its matches, the highlighter and its per-line
state, and where the window is scrolled to. Two documents share nothing that
could disagree — which is the point. The undo steps of one hold character indices
into *its* text; the search of one holds match offsets for *its* text; the
highlighter of one holds per-line syntax state for *its* lines. Mixing them is
not a bug you find later, it is a bug you cannot fix later.

What stays behind is everything that is true of the session: the register (vim's
registers are global, and `yy` in one file then `p` in another is the point),
the mode, the keymap, the options, the command line and its history, the script
host, and the message row.

## The swap

Only one document is a set of fields at a time. The one on screen is live in the
editor; every other one keeps its state in its slot in `documents`. Switching
saves the live fields into the current slot and loads the new one out:

```rust
pub fn switch_to(&mut self, index: usize) -> bool {
    if index >= self.documents.len() || index == self.current {
        return false;
    }
    self.save_document();  // the live fields into the slot they came from
    self.current = index;
    self.load_document();  // that slot's state into the live fields
    true
}
```

Each swap leaves *a placeholder* in the slot it took from, so the invariant has
two halves: the current slot holds a placeholder, and every other slot holds its
document. Closing a document — or opening one — is the same swap with the slot
added or removed, and `drop_document` is the third half of it: what was on screen
is forgotten rather than stored, because the document is going away.

This is a deliberate cheat, and worth naming as one. The honest model is that
the editor has no buffer of its own and every access goes through
`documents[current]` — which is a borrow-checker problem in every one of the
several hundred places that say `self.buffer`, or a refactor of all of them. The
swap costs one `mem::swap`-per-field at the moment of switching and *nothing at
all* in between, and it leaves every existing line of the editor alone. A file
that keeps its own cursor is then not a feature anyone had to add to the
commands: they were already reading `self.cursor`, and it was already the right
one.

A viewport holds two things that do not belong together: where the window is
scrolled to, which is the document's, and how big the window is, which is the
window's. Only the scroll is saved and loaded. Swapping the whole viewport would
mean a save-and-load pair *reads the size out of a placeholder* — and the
window would quietly become 80 by 24 on every switch.

## The commands

| Command | What it does |
| --- | --- |
| `:e file` | the file becomes a document of its own and is shown. The one that was open keeps its place in the list — and, if there is a file already open with that name, this refills *that* document instead |
| `:e` | refill the current document from disk |
| `:e!` | the same, without complaining about unsaved changes |
| `:ls` | one line naming every document, numbered, `%a` on the one on screen |
| `:b N` / `:b name` | go to a document by its number, or by any part of its name or path |
| `:bn` / `:bp` | the next or previous document, wrapping around |
| `:bd` | close the current document; refuses when it has unsaved changes |
| `:bn!` `:bp!` `:bd!` | the same, abandoning those changes on purpose |

`bd` on the last document leaves an empty one behind rather than a closed
editor, the way a window that has lost its file keeps its place on screen.

Two of these are deliberately not vim. `:e other.txt` on a modified document
*keeps* that document and its changes in the list, where vim would throw them
away; `:bp` back finds them. And a command that cannot be reached from a dirty
document (`:e`, `:bd`) refuses with vim's `E37` rather than acting, because the
alternative is the one thing an editor must never do quietly.

## What is not here

Splits and tabs: a window is a view of a document, and there is one window.
`:b#` (the alternate), `:bufdo`, marks per document, and the `'hidden'` option
that lets `:e` leave a document behind without complaint. A document is also never *unloaded*
— nothing frees a buffer whose text is not needed.
