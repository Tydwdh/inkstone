# Undo: what one `u` takes back

Undo is where an editor's model of *a change* shows up, because every claim it
makes is a claim about what a person meant. The claim here: **one thing the
person did is one `u`**, whether that was a keystroke, a counted motion, a whole
`:s`, or an insert session that wrote a line break and then a sentence.

## A step

```rust
struct Change {
    start: usize,      // where the replacement begins, in characters
    removed: String,   // what was in [start, start + removed.len())
    inserted: String,  // what is there now
    before: Cursor,    // where undo puts the cursor
    after: Cursor,     // where redo puts it
    caret: usize,      // where, inside `inserted`, the next write goes
}
```

One step is one *contiguous* replacement, and `undo` applies it backwards:
remove `inserted`, put `removed` back. That is the whole of the machinery — no
diffing, no text snapshots, no per-line bookkeeping.

`caret` is the field that looks redundant and is not. Typing writes at the
cursor, so the next keystroke's text goes at the end of what this step has
written — usually. `o` does not: it inserts a line break at the end of the line
and leaves the cursor *in front of* it, so the text typed next belongs at the
head of the step's `inserted`, not its tail. `caret` is where the cursor is
inside the replacement, and it is what lets the typing join the step that opened
the line instead of starting a new one.

## Folding keystrokes into steps

Two rules, in order, applied to every edit:

1. **Absorb.** If the new edit continues the newest step, the step grows rather
   than the history. "Continues" is exact, not fuzzy: the new text goes where the
   caret is, the new removal takes back text in front of the caret, the new
   deletion follows the one just made. Typing, backspacing, and deleting a run
   one key at a time are all one step; a jump to another part of the buffer is
   not, because a step is a contiguous range and always will be.
2. **Group.** Everything a command does is one group, and so is a whole insert
   session — which starts when a command enters insert mode and ends when one
   leaves it. When a group closes, the steps inside it are folded into one.

The second rule matters because the first cannot always apply: `cw` deletes the
word and then types the new one, and those two steps do not touch the same place
in a way absorb can join. The fold is exact where it applies and gives up where
it cannot be — steps that overlap (a script editing inside text it has just
written) stay separate rather than becoming a step whose `removed` is a guess.
An extra `u` is a smaller sin than an undo that corrupts the buffer.

```
ihello<Esc>   →  one step   (absorb: each keystroke continues the last)
ohello<Esc>   →  one step   (the group folds the line break and the typing)
3dd           →  one step   (the group folds three deletions)
:%s/a/b/g     →  one step   (one replacement over the changed lines)
2dw then u u  →  one step   (a counted operator is one group)
```

## Which state was written

"Modified" is not a flag on the buffer and it is not a comparison of text: it is
whether the state the buffer is in is the state the last write wrote. Every state
a step produces gets an identity from the history — absorbed keystrokes included,
because growing a step makes a new state out of the one it grew from — and a
write remembers the identity it saved. `u` back to it clears the marker, `u` past
it and `<C-r>` forward put the marker back, and an edit that happens to restore
the old text is still an edit.

The alternative costs more and gets it wrong at the edges: a `modified` flag on
the buffer cannot be cleared by undo at all, and comparing the text against what
is on disk re-reads the file, gets line endings and the trailing newline wrong,
and calls a buffer clean when the user has changed it back by hand — the one case
where being wrong is expensive.

## What this buys, and what it costs

The buy is that nothing else has to know: `undo` and `redo` replay one struct,
the substitute builds one replacement instead of a group of them, and the
highlighter is invalidated from the first line a step touched. A group of steps
is folded *at the moment it closes*, which is the only moment both the before
and after text of the whole group are still knowable — the before text of the
part between two steps is read from the buffer, since neither step touched it.

The cost is two limits, both deliberate. A step is contiguous, so an edit that
is genuinely scattered (a script that edits line 3 and line 900 in one command)
either folds into one wide replacement or stays as several steps. And nothing
here is limited to a buffer: a document that is switched away and switched back
has its own history, and steps are never replayed onto another document, because
their character indices would be a lie there.
