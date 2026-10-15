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

## What it costs, honestly

A VT parser that is *correct* is the hard part, and `vte` is the answer to it;
the screen model on top — cells, attributes, scroll regions, the alternate
screen, wide characters — is a few hundred lines and a lot of small decisions
(what a `\t` does at the right margin; whether a `\r\n` pair is two operations).
It is a project of its own, not a step of this one, and it should be split
again the moment it grows a second frontend concern: **the frontends should not
have to know**, and if one of them does, something has gone into the wrong layer.

## What is not in it

- No multiplexing, no sessions, no detaching.
- No mouse reporting to the program (`docs/selection.md` already says this is
  left for later), and no bracketed paste.
- No image protocols (sixel, kitty), no hyperlinks.
- No scrollback limit: the buffer is the scrollback, and a command that prints
  for an hour fills the buffer for an hour. That is a real hazard and the first
  thing to reconsider when this is built.
