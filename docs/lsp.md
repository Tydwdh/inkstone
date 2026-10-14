# A language server, and where it can live

A language server is a program that speaks JSON-RPC over its standard input and
output: the editor starts it, tells it which files are open and what is in them,
and the server answers with diagnostics, completions, definitions, hovers and
more. This is the design note the roadmap asks for before anything here changes
`inkstone-rpc` — and the first thing the design says is that it does not.

## The wire does not change

Every other editor feature in this repository ended up in the same shape: the
kernel owns the state, `compose` draws the grid, and the frontends draw cells.
Diagnostics are the same. They are drawn where everything else is drawn — the
underline of the character range is an attribute bit, and the severity is the
colour of the line number in the gutter, which is the one place on a line that is
ours and not the file's. A frontend that can draw an underlined search match can
draw a squiggly error, because it is the same bit.

So nothing new goes on the wire. What the daemon needs is a way to *put*
diagnostics into the editor, which is a kernel API call, and what the kernel
needs is somewhere to keep them, which is next to the search matches: per
document, because they are about a file.

## Where the server process lives

The daemon. It is the process that owns file paths, and a language server wants
paths and URIs; it is also the process that already has a thread of its own to
block on. The kernel stays synchronous and never waits for a subprocess: it is
called with a finished list of diagnostics, and it has no idea where they came
from.

```
frontend ──stdin/stdout── daemon ──stdio── language server
                             │
                        inkstone-lsp: one thread per server
```

The daemon's loop reads one client message at a time, but not from the socket
directly: a spawned `inkstone-server-rpc-reader` thread blocks on the socket and
pushes what it reads into one crossbeam channel, and the loop waits on that
channel with a 10 ms timeout (`TICK`). A server that pushes
`publishDiagnostics` has to be able to wake that loop, and this is what the
timeout is for: a message from the client wakes it at once, and otherwise it
looks at the server's own channel on the tick — the daemon already depends on
`crossbeam-channel` for exactly this kind of seam.

## The protocol, as far as this needs it

Framing is the whole of the transport: `Content-Length: N\r\n\r\n` followed by
`N` bytes of JSON. Two headers are allowed and one is used. Everything else is
JSON-RPC 2.0 — requests with an `id`, notifications without one, and responses
that carry either a result or an error. The client here is a *sender* of
requests and a *receiver* of notifications, and it also files the answers: a
request goes out with an id and what comes back is kept under that id, in
`answers`, until the session asks for it — a server may answer before the client
has looked. The only notification it acts on is
`textDocument/publishDiagnostics`.

The lifecycle, in order:

| step | who | what |
| --- | --- | --- |
| `initialize` | client | what this editor is and what it can do |
| `initialized` | client | the server may start working |
| `textDocument/didOpen` | client | the file's text, and its language |
| `textDocument/publishDiagnostics` | server | the file's problems, whenever it likes |
| `textDocument/didChange` | client | the whole text again, after an edit |
| `textDocument/didClose` | client | the file went away |
| `shutdown` / `exit` | client | the server is done |

`didChange` sends the **whole text**, not a diff. That copy is made on every
*edit*, not every save: the document's edit count is what the server is told
about, and `sync_lsp` compares it with the version the server holds, sending a
full-text `didChange` whenever it moves. `didSave` is never sent. Incremental
sync is the server's stronger mode and a later optimisation still: a document of
a few megabytes costs one copy per edit, and being wrong about an offset would be
worse than being slow for a moment.

## Positions, which are not ours

LSP counts a position as a line and a **UTF-16 code unit** in that line. The
kernel counts *characters*. The two agree for everything in the basic
multilingual plane and disagree for an emoji, which is two code units and one
character — so the conversion happens in one place, in the client, with a test
that uses a character outside the plane. The kernel never sees a UTF-16 offset,
and the client never sees a character index.

## What is here, and what is next

`inkstone-lsp` is the client: the framing with its tests, the lifecycle, a reader
thread and a channel, and the position conversion. It is tested against a real
process over real pipes — `fake-lsp-server`, a binary the crate builds beside its
tests — which is where the framing bugs actually live; an in-memory stream is
easier to be sure about and easier to be wrong about.

What is wired, as of the increment after this note: the `lsp` setting (off by
default), a command found on `PATH` for the file that is open — or named by
`INKSTONE_LSP` — the session's `didOpen` when the file changes, its `didChange`
whenever the document's edit *count* moves (the whole text and a version, which
is what `History::edits` was already counting), the conversion of a
`publishDiagnostics` payload into the kernel's `Diagnostic`s against the line it
points into, and a daemon loop that wakes a hundredth of a second after the last
message so a server that pushes is heard rather than noticed later.

Two details worth knowing. Diagnostics go to the document they are *about*: the
payload names a URI, and a session may hold several files, so the live one and
the ones in their slots are both looked up. And a document with no server to
start says so in the message row instead of failing quietly.

## The requests that answer something

`textDocument/completion` was the first of them, and it needed the half of the
protocol the client did not have: an answer carries the id of the question, so
the client keeps what it sent and files what comes back by that id. Two things
about how it is used here are worth knowing.

A server is *asked* every time the list is opened or moved, and the kernel does
not wait for it: the words already in the file are put up first, because there is
no reason to sit in silence for a round trip to be told what is on the screen
already. When the answer comes it replaces the list — the same popup, better
items — and what it would type is the server's `insertText`, or its `textEdit`'s
new text, or its label, in that order.

And the typed prefix goes *back into the buffer* when the answer arrives. A server
answers about the word as it was asked about; by the time the answer comes, the
local list may have typed a guess into the buffer, and filtering the server's
answer by that guess would throw away the better list for the sake of the worse
one.

`textDocument/hover` and `textDocument/definition` followed, and they cost
almost nothing: the same question-left-by-the-kernel, asked-by-the-session,
answer-under-an-id shape. A hover is shown in the message row, which is one line:
the protocol's four shapes for it — a string, a marked string, a list of either,
and a `MarkupContent` with plaintext or markdown — all come out as one line of
text, because that is all a status line is. A definition names a file and a
position: the file is opened — from the session's documents if it is open
already, from disk if it is not — and the position is converted against the line
it points into.

`textDocument/references` was the third, and it is the one that needed a decision
rather than a path: the answer is a *list* of places, and this editor has one line
of message row and no quickfix list. So the list is what that line says — the
count, then the first few places named by file, capped like `:diagnostics` — and
the parsing of a place (a location, a list of them, a `LocationLink`, or nothing
at all) was pulled out into one function that the definition answer now uses too.

Two things about *when* a server runs, both found by asking a real terminal
rather than a test. A frontend opens its file after the session is up, so a
server decided only at startup is a server decided about no file: the session
starts one when the file arrives, remembering which file it last tried so that a
file with no server for it is not looked for again and again. And an answer
arrives between keystrokes — nothing else would ask for a frame — so the tick
compares what the frontend is shown (the message row, the cursor, which document)
before and after, and asks for a frame when an answer moved it. Without that the
answer was applied in the kernel and never drawn, which is exactly what made `K`
and `gd` look unreliable: they worked when some other key happened to repaint.

Completion has a second half. An answer names the items and says little about
them; `completionItem/resolve` is what fills in what each one *is*, and it is
asked about the item the list has moved to and no other — one question per item a
person looks at, rather than a hundred for a list nobody read. The item is sent
back as the server sent it, `data` and all, because a server is entitled to put
something in there that only it understands; what comes back is the item's
`detail`, or the first line of its documentation when that is all there is.

An item may be a snippet — `insertTextFormat` 2 — and then the text it carries
is not what is typed but a description of it: `${1:name}`, `$2`, `$0`, and a
default that may hold stops of its own. The reading of that belongs to the kernel,
next to the popup it came from, so the server passes the flag and nothing else.
The text goes into the buffer with the defaults in it, and the stops are kept as
ranges so that `<Tab>` can walk them — in *number* order, since that is the order
a person meant when they wrote the snippet, not the order the stops appear.

A number written twice is one place, and the second writing is a *mirror* of the
first: what is typed into one is what the other says. That falls out of the same
mechanism the choice needed — an edit keeps the snippet's ranges true, in one
path — which is why typing and picking a choice are the same code rather than two
with rules of their own.

Stepping to a place in a snippet selects what is in it, and typing replaces it —
which makes insert mode aware of a selection, the one thing it and visual mode now
share. The splice is one edit, so it is one thing to undo.

`textDocument/documentSymbol` is the one question here that is not about a *place*:
the answer is what the file holds, and the list is what the message row can carry —
a name, what kind of thing it is, and the line it is on. A server may send nested
symbols, each carrying its own range, or flat ones whose position lives inside a
`location`; both come out as the same list, and the first is where the cursor goes,
which is the same shape references have.

## What is deliberately not here

Multiple servers for one file, `workspace/` edits, incremental sync, semantic
tokens, folding, rename and code actions. Of the requests that *answer*
something — completion, hover, definition — completion was the one worth doing
first, and it was a change of its own: it needed a popup, which is a new thing to
draw, and the popup has to be in the same place as the diagnostics: the kernel's
grid. That change has since been made, and the account of it is the section
above.
