# Running a command

`:{range}!cmd`, `:r !cmd` and `:!cmd` are the edge of the editor that is not the
editor: a program, its output, and what that output is for. vim has all three
and waits for each of them, because vim's whole design is one thread that stops
when it is told to. This one cannot: the kernel *is* the thread everything else
happens on, and a build that takes a minute would be a minute of a frozen screen
with nobody listening for a keystroke.

So the kernel asks, the way it asks a language server, and the answer arrives at
a tick.

## Who does what

```
Editor (inkstone-core)          Session (inkstone-server)
   │                                  │
   ├── run_shell / read_command       │
   │     makes a Job, and the message │
   │     "!cmd"                       │
   │                                  │
   ├── job_request() ────────────────►├── spawn: the shell, three pumps, a waiter
   │                                  │
   │                                  ├── :kill sets a flag the waiter reads
   │                                  │
   ◄────────────── job_finished ──────┤── the outcome at a tick
   │     the buffer edit happens here │
```

The kernel never touches a process, and the session never touches a buffer. What
crosses is a command line and its input, and then an outcome: standard output,
standard error, and an exit code. `crates/inkstone-server/src/job.rs` is the
half with the process; `crates/inkstone-core/src/editor/jobs.rs` is the half
with the buffer.

Nothing about this is on the wire. A frontend sends `:!echo hi` as a key or as a
command line like any other, and the document that comes back is a grid of cells
like any other — which is why both frontends got this without a line of their
own.

## The three things a `!` can be

| typed | the range is | the output is |
| --- | --- | --- |
| `:%!sort` | the program's input | replaces the range — a **filter** |
| `:r !date` | — | inserted below the line the command was given from |
| `:!cargo build` | — | a **document** of its own, in a window below |

With a range, what makes a filter a filter is that it is one text becoming
another, and the range is what says which text. Without one, a command is a
command: it runs on its own, and what it said becomes text because *output is
text*, and text in this editor lives in a buffer.

`cmd /C` runs it on Windows and `sh -c` everywhere else. A command *line* is a
command line: quoting, pipes and redirection are the shell's business, and
splitting it here would be a second, worse shell.

## The parts that are easy to get wrong

**Every pipe has a thread of its own.** Reading standard output to the end and
*then* reading standard error is a deadlock waiting for a compiler: the program
fills the second pipe while the first is still open and neither side moves again.
Standard input is written on a third thread for the same reason — a program that
reads nothing until it has written something would otherwise hold a write up
while a read waits for it.

**`:kill` sets a flag, and the thread that is waiting does the killing.** A child
behind a mutex its waiter holds for the whole wait is a child nothing can kill,
so the wait is a `try_wait` poll and the kill is an `AtomicBool` the waiter reads.

**A pipe can outlive the process.** On Windows a killed shell leaves its own
child holding the pipe, so the answer is what has arrived two seconds after the
process went: a short answer is better than a command that never finishes.

**A program does not get to change the file's line endings.** `sort` on Windows
ends every line with CRLF, and a file that was LF must not come back from
`:%!sort` as one that was not. What the filter replaces is the *text*; whether a
line ends with LF or CRLF is the line model's business, and the program's
endings are rewritten to match the range's.

**An answer that arrives after the text moved on is refused.** Vim never has to
ask, because it waits. A command that runs while a person goes on editing is the
price of not freezing, and the price is paid at the end: a filter's range and a
`:r !` line are held as the *text* they were, and text that is no longer that
text gets `E: the text changed while "cmd" ran` instead of a replacement.

**One command at a time.** A second `:!` while one is running is refused, with
`:kill` named, because that is the thing the person wants to know.

**Both streams go into the document.** A terminal shows them interleaved because
both are writing to it, and a build's errors are on the second — so a document
holding only standard output would hold the half nobody asked for. They are
appended rather than interleaved, which is the honest approximation: the order
between them is the order they arrived in, and the order *within* each is exact.

## What a script gets

`ed.shell(command, callback)` runs a command and hands the answer to Lua: the
callback is called the way every other Lua callback in this host is — one table,
the output first and the exit code second, `-1` when a command was killed — and
what it asks for runs when it returns.

The text of the buffer is *not* reachable from Lua, and that is the design rather
than a gap: the kernel owns the text, and a script reaches it the way a person
does — by running commands. `ed.run("normal", { "ihello<Esc>" })` is the door for
typing, and it is the door with the undo history behind it. What a script *can* be
handed is what comes from outside the editor: `ed.read_file(path)` and
`ed.write_file(path, text)`, and whatever a command it ran has to say.

## What is not here

- No terminal: a command that wants to *draw* — an editor, `top`, a password
  prompt — has nowhere to draw. `:!` is for commands that answer and exit.
- No interactive input. Standard input is the range, or nothing: a program that
  asks a question reads end-of-file and does whatever it does then.
- No `:w !cmd`, which vim spells as "write this buffer into that command".
- No exit-code reporting beyond the message row, no `'shell'` option, no job
  list, and no reading of a command's output into a *list* — an error list from
  a build is a good idea that this is not.
- Killing the *shell* does not always kill what it started: on Windows the
  grandchild keeps running until its next write to a pipe that is closed.
