# Directories, and what a listing is

An editor whose only interface is text has to say what a directory is, and the
two editors this one learns from both said it the same way: emacs has dired, vim
has netrw, and in both of them a directory is **a buffer holding a list**. That
is not a workaround for not having a file manager — it is the thing that makes a
file manager out of commands that already exist, because the list is text and
every text command works on it.

`inkstone` opens a directory the same way, as one more kind of buffer.

## A listing is a buffer

`Buffer` says what its text is:

```rust
enum BufferKind {
    File,
    Listing { entries: Vec<Entry> },
}
```

A `File` is what a buffer always was: the text is the file the path names.
A `Listing` is a directory: the lines are its entries, and `entries` is what the
directory looked like when this buffer last read or wrote it — the same fact
`stamp` is for a file, and the baseline a save is measured against.

The reading side is one branch in `Buffer::from_path`, and that placement is the
whole design. Every way a file arrives — `:e src`, the argument both frontends
start with, `gf` on a name under the cursor, `:sp src`, a document switch — ends
up there, so a directory arrives as a listing without a line of code that knows
listings exist. `gf` needed one change to go with it: it refused a name that was
not a file, and a directory is a name a person can put the cursor on.

The path is kept **as it was given**, not resolved. It is what the status line
shows (with the platform's separator, so that a listing reads as the directory
it is: `./`, `src/`), what a save hands back to the filesystem, and what `-`
walks up from. Resolving it would make `:e .` announce a path nobody typed.

Directories come first and each group is sorted by name, the way `:vimgrep`
walks a directory: one order in the codebase, and walking down is what a listing
is for.

## Writing one is the filesystem edit the lines spell

`:w` of a listing does not write bytes: it makes the directory match the text.
That is a diff, and the rules are the interesting part.

**The name is the identity.** A line that keeps its name is nothing to do —
which is what makes reordering a listing an edit with no effect instead of a
storm of renames. What the order *is* is the view.

**One name out and one name in is a rename.** It is the only way a rename can be
meant: anything more is somebody who removed some entries and added others, and
pairing them up would be guessing with their files — a wrong guess being a
removal that was meant to be a rename, and a rename is the one that keeps the
content. A directory and a file are never a rename of one another.

**Removals go first**, so a name is free by the time it is wanted: a listing may
hold names the filesystem must not have two of at once.

**Files are created with `create_new`.** An entry the buffer thinks is new but
the filesystem does not is a disagreement to report, not a file to empty.

**A directory goes only when it is empty.** `fs::remove_dir` is what enforcing
that means, and a listing that removes a subtree is a listing that has to be
asked twice.

**The marker says the type.** `/` marks a directory, the same on every platform
because the listing is a format rather than a path — and the platform's
separator is taken as a marker too, for somebody who reaches for the key their
own file manager uses. A line that contradicts what the entry already is
(`docs` where `docs/` is) says so instead of guessing.

Before any of it runs, every line is checked: a name with a separator in it is a
path, `.` and `..` are the directory itself, two lines cannot name the same
thing, and the one line that holds an empty name is a bare `/`. Nothing is
applied until all of it parses, so a refusal costs nothing.

A write that fails half way says which entry it stopped on, and the baseline
follows the filesystem rather than the plan: the next `:w` measures against what
is really there, so it finishes the job instead of redoing it.

The stamp check is the one files already had: a directory whose entries changed
underneath the buffer is an `E13`, and `:w!` says the person knows. A listing is
never written anywhere else — `:w name` on one is refused, because a listing *is*
the directory it lists.

## The keys

`<CR>` is `open_place`, so a listing's line is a place like any other: a
directory takes the listing's place, because walking down and back up is one
document rather than a pile, and a file opens as a document of its own, so the
listing stays behind where `:b` finds it again. That a listing and a list of
search results are the same gesture is not an accident of binding — they are the
same thing: a line that names somewhere to go.

`-` is new, and is `up_directory`: the parent's listing where there is one, and
vim's "up one line, on the first non-blank character" anywhere else — the same
bargain `<CR>` already strikes with `move_down`. The parent of `.` is the empty
path, so the walk goes through what the directory really is, and through `tidy`
for the prefix Windows puts on a canonical path.

Walking away from a listing that has been edited is refused with `E37`, the way
`:e` on a modified file is. The edits to a listing are the filesystem's *pending*
work — writing is what makes them true — and the one thing an editor must not do
is drop them quietly. This was the bug the smoke run caught: the first version
navigated and lost the edits, and no unit test had asked for the refusal.

## What is not here

- No tree or browser: a listing is one directory, and walking is `<CR>` and `-`.
- No `%` and `d` (netrw's "make a file", "make a directory"). They are not
  needed: a line *is* the entry, so typing a name into the listing and writing it
  makes it — and that is one interface instead of two.
- No attributes: not the size, not the mode, not whether it is a symlink. What
  the line says is the name and whether it is a directory.
- No way to make a directory's *contents*: a listing makes empty directories,
  because that is what one line can say.
- No sorting options, no showing hidden files by another rule, no `cd` and no
  `:pwd`: a relative path is relative to the directory the daemon was started in,
  and that is that.
- `:bd` on a listing forgets it like any document; nothing closes the directory
  a person walked out of.
