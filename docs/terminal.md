# A terminal in a buffer

`:term` is the last thing `docs/jobs.md` promised and did not deliver: a program
whose *output is not a document but a screen* — one that moves its cursor, paints
colours, scrolls, and expects to be typed at. `:!` already runs commands; what a
terminal adds is a program that talks back.

This note exists because the roadmap's rule is that anything wanting to change
`inkstone-rpc` gets one first. The interesting conclusion is that this does
**not** want to change the wire — and the reason is worth writing down, because
it is the payoff of two decisions already made.

## What it takes

Three pieces, in the order they have to be built:

1. **A pseudo-terminal.** A program that draws expects a *terminal* on its
   standard output, not a pipe: it asks the width in columns, reads keys as keys
   rather than as bytes, and turns its own echo and line editing off. That means
   a PTY — ConPTY on Windows, `openpty` on Unix — which is a dependency
   (`portable-pty` is the usual one) or a platform module of our own.
2. **A parser for what a terminal writes.** The bytes are not text: they are text
   with escape sequences in it, and a terminal is the thing that knows what
   `ESC[2J` means. The `vte` crate is the parser alacritty uses, it is small, and
   it is a state machine that calls you back for each character and each
   sequence.
3. **A screen to put it on.** Rows of cells, each with a character and a set of
   attributes, a cursor, and scrolling.

The third piece is the one the editor already has — and that is the whole point.

## Why the wire does not have to change

A frontend is dumb on purpose: it draws a grid of cells, each of which already
carries attribute bits, and the daemon composes that grid. What a *terminal*
buffer needs is exactly that grid — a cell is a cell — and the only question is
where its attributes come from. For a file they come from the highlighter; for a
terminal they come from the escape sequences. Both are decided in `compose`, in
the kernel, and the frontends are told nothing either way.

So the seam is a third `BufferKind`:

```rust
enum BufferKind {
    File,
    Listing { entries: Vec<Entry> },
    Terminal { /* the screen: cells, attributes, the cursor */ },
}
```

A listing already proved the shape: a buffer that is not a file, whose text is
still text, and whose save is something else. A terminal goes further — its text
is *rewritten* by the program rather than by a person — and it needs one thing a
listing did not: **attributes that are not the highlighter's**. That is a side
table beside the buffer's lines, read by `compose` in place of the syntax
colours, and it never leaves the daemon.

What does travel is what already travels: keys go out (a frontend's keys reach
the kernel as keys today), the screen comes back as rows, and a resize is a
resize. There is no new message.

## The two decisions this note did not have

**The keys.** A terminal's keys are the program's — that is what a terminal is —
and the way back to the editor's is `<C-\>`, which is vim's escape too. `<Esc>`
cannot be it: a program that draws needs `<Esc>`. So a terminal buffer's *insert*
mode is the program's and its *normal* mode is the editor's, which is what the
status line says: `-- TERMINAL --` while the keys are the program's, and the
ordinary label once they are not.

**The text.** The screen is the truth and the buffer's lines are a view of it,
rewritten whenever the program draws. That is the one place a change does not go
through `Editor::replace_range`, and the reason is in `ARCHITECTURE.md`: a
program redrawing its screen is not an edit somebody made, so it must not be a
`u` step — a `u` that scrolled a terminal back would undo the program's work
rather than a person's.

## What reuses what

- **Running it** is `docs/jobs.md` with a different sink. The job's `Call` action
  hands the output to Lua; a terminal's hands it to the parser and the screen.
  The pipes, the threads, the `:kill` flag and the tick are the same machinery —
  which is why the job work came first.
- **Typing at it** is the kernel's key handling with a different destination:
  keys become bytes and go to the child, which is the `stdin` half of the job
  that a filter already writes to.
- **The scrollback** is the buffer. A terminal in a buffer is a terminal with
  scrollback, search, and `:%!` for free — the thing a real terminal emulator
  has to build separately. Lines that scroll off the top are still lines.
- **The cursor** is the buffer's cursor while the program is in charge of it,
  and the window's when a person looks away — which is a question the kernel
  already answers for every other kind of document.

## What it cost

`vte` is the parser — the hard part, and the reason not to write one — and it
lives in the *kernel*, where the screen is: the session feeds it bytes and the
kernel keeps the screen, because a screen is what a buffer is. `portable-pty`
opens the pty, in the session, beside the job runner it shares its shape with.

The screen model on top is a few hundred lines and a lot of small decisions, and
the tests are where they were made: a character printed in the last column parks
the cursor there with the wrap *pending* (or a program filling the final cell
overwrites it); `CSI C` moves right from where the cursor is rather than to a
column; the alternate screen saves the cursor as well as the cells; and a wide
character takes two cells, the second of which carries no text of its own.

What is *not* implemented is a scroll region (`CSI r`): a program that sets one
scrolls the whole screen instead. That is wrong in a way a person sees — a status
bar pinned to the bottom of a full-screen program scrolls away — rather than a
way that is silent, and it is the first thing to add when somebody needs it.

**The frontends were told nothing.** Not one line changed in either of them: a
terminal is a grid of cells, and so is everything else. If that ever stops being
true, something has gone into the wrong layer.

## What is not in it

- No multiplexing, no sessions, no detaching.
- No mouse reporting to the program (`docs/selection.md` already says this is
  left for later), and no bracketed paste.
- No scroll region, and no answer to the other questions a program can ask
  (`CSI n` beyond "where is the cursor").
- No image protocols (sixel, kitty), no hyperlinks.
- No scrollback limit: the buffer is the scrollback, and a command that prints
  for an hour fills the buffer for an hour. That is a real hazard and the first
  thing to reconsider when this is built.
