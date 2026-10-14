# Roadmap

What is done, what is next, and what each next thing has to respect. The
constraints are the useful half: they are the invariants from
[`ARCHITECTURE.md`](../ARCHITECTURE.md) applied to work that does not exist yet.

Each entry is a snapshot of the work at the moment it was written: its *Left for
later* says what was still open **then**, and a later entry may have shipped it —
the newest state of the project is the last entry, not the first.

## Done — milestone 1, the walking skeleton

Headless daemon plus dumb crossterm frontend over MessagePack on stdio; a rope
buffer whose line model treats a terminator as structure, not content; normal,
insert and command-line modes; a command registry with 29 built-in commands; a
keymap a script can extend; a Lua host; line numbers and tabstop; save, quit,
reload, with the refusals (`E32`, `E37`) that stop data loss.

## Done — undo and redo

An edit history with `u` and `<C-r>`, restoring the text and the cursor.

- **What shipped:** one step is one *replacement* — the character range that
  changed, the text that was in it and the text that is in it now — so a step
  stays meaningful while later edits are recorded on top of it, and undoing
  newest-first always puts back the text that was there. Steps are grouped the
  way a user thinks: an insert session is one step, and so is one command, which
  is what makes a counted `3x` or a multi-line `dd` a single `u`.
- **Constraints it had to respect, and how:** a terminator is not content, so a
  step that removes the buffer's last one owns the newline the invariant hands
  back — otherwise undoing `dd` on the last line of a file would leave a stray
  empty line. The normal-mode cursor clamp runs after every undo, so a restored
  cursor cannot sit on a terminator. A save clears "modified" and not the
  history. `:e` clears the history, because its indices belong to the document
  that was open.
- **Files:** `history.rs` (new), plus a field on `Editor`, two commands, two
  default bindings, and rows in the README key table — the shape this item
  predicted, except that every mutation had to funnel through one
  `Editor::replace_range` for the recording to be complete.
- **Left for later:** a save point. Undoing back to what is on disk still
  reports the buffer as modified.

## Done — a windowed frontend

`inkstone-gui`: the same daemon, the same framing, the same screen, drawn in a
window with `wgpu` instead of a terminal.

- **What shipped:** a frontend that spawns `inkstone-server`, folds the frames it
  sends into a `Screen`, and draws the rows a delta named — one instanced quad
  pipeline, one draw call, an atlas that grows on demand. Keys, input-method
  commits and resizes go back the other way.
- **Constraints it had to respect, and how:** deltas only make sense against the
  screen a frontend already has, so the window *is* the `Screen` and nothing
  else; the palette and the layout rules stay in the daemon, which is why the
  window can be as dumb as the terminal frontend. Two things the wire had to add
  first: attributes a renderer can use, and `ClientMsg::Text` for anything that
  was never a key — an input method has nothing else to send.
- **What it cost, in hindsight:** the drawing was the easy part; the platform
  was not. An input method left on in normal mode swallows the keys that run
  commands, so the frames now carry whether text is being inserted and the window
  hands the keyboard over only then.
- **Files:** `crates/inkstone-gui` (new), `crates/inkstone-client` (new, shared
  with the terminal frontend), and `docs/rendering.md` for the decisions.
- **Left for later:** selection and clipboard, mouse, key repeat, smooth
  scrolling.

## Done — operators and a register

`d`, `y` and `c` take a motion, `dd`/`yy`/`cc` take lines, `p`/`P` put what was
taken, and normal mode counts.

- **What shipped:** a pending operator that resolves the next key as a motion
  through the keymap — so a remapped `w` is still the motion `d` works on — plus
  a register that remembers text *and* whether it was whole lines, because that
  is the difference between pasting into a line and pasting between lines.
- **Constraints it had to respect, and how:** a terminator is not content, so
  `dd`/`yy` take lines with theirs and paste them back whole (a CRLF line stays a
  CRLF line); the normal-mode cursor clamp runs after every range, so `d$` cannot
  leave the cursor on a terminator; and every range goes through
  `Editor::cut`/`copy`, so a put is one undo step and the register always holds
  what the last delete took.
- **What the counts cost:** the count prefix is the keymap's `Param::Int`
  channel, which the commands already read, so `3x` and `3G` needed no new
  plumbing — only a cap, because a key held down could otherwise ask for a
  billion trips round a loop.
- **Files:** `register.rs` (new), `editor.rs` (the count, the operator and the
  dispatch), `command.rs` (the motion table, the spans, the operators and the
  put), `keymap.rs`, and the README's key table.
- **Left for later:** text objects, named registers, and visual mode — which is
  the same range machinery with the selection coming from the screen instead of
  from a motion.

## Done — visual mode

`v` and `V`, the motions that extend a selection, and the operators on it.

- **What shipped:** a selection that is just an anchor and a kind, with the
  cursor as the other end — so extending it needs no new commands at all, it
  needs the motions that already exist. A key visual mode does not bind falls
  through to the normal mode's keymap, which means a user's remapping of `w` is
  what `v w d` uses too.
- **Constraints it had to respect, and how:** a selection is a buffer range, so
  it is recomputed from the anchor and the cursor on every frame rather than
  stored as indices that later edits would move; the highlight is the
  reverse-video attribute the protocol already had, so the wire did not change;
  an operator over a selection is one `cut`, so it is one `u`; `clamp` keeps the
  cursor off the terminator in visual mode as it does in normal mode.
- **Vim's rule that surprised the tests, twice:** a visual motion selects
  *through* the character the cursor lands on, so `vwd` is not `dw`, and a
  backwards selection includes the character the anchor sat on. Both are now
  pinned by tests that say so.
- **Files:** `mode.rs` (the fourth mode and the selection kind), `editor.rs` (the
  selection, the dispatch and the fall-through), `command.rs` (the visual
  commands), `keymap.rs`, `render.rs` (the highlight and the status label),
  `lua.rs` (scripts can name the mode).
- **Left for later:** blockwise (`<C-v>`) selections, text objects inside a
  selection, and `:` on a selection (there are no ex ranges yet).

## Done — search

`/`, `?`, `n`, `N`, and every match on screen highlighted.

- **What shipped:** a pattern and the places it matches, held as character ranges
  and refreshed after every edit — because that is what a match is, and every
  edit moves one. `/` and `?` open the same line a `:` command does, with a
  different first character, so there was no second prompt to build.
- **Constraints it had to respect, and how:** a match must never straddle a
  terminator invisibly, so the pattern is searched for *within a line*: a
  highlight is a row and a span of columns, and a match running from one line
  into the next could not be drawn as the one thing the user typed. The
  highlight is the attribute bits the protocol already carries — underline for a
  match, bold for the one the cursor is in — so no frontend needed a new message
  and the terminal frontend got the highlight for free.
- **What the tests caught:** the first search after `/` must stay on a match the
  cursor is already in, while `n` must move off it; the two rules look like one
  and are not. And "the pattern is per line" needs a test that says so out loud
  (`/e
t/` finds nothing).
- **Files:** `search.rs` (new), `editor.rs` (the prompt, running a search, the
  refresh after an edit), `command.rs`, `keymap.rs`, `settings.rs`
  (`ignorecase`), `render.rs` (the prompt line and the highlight).
- **Left for later:** regular expressions, `*`/`#`, `:s`, and incremental search
  while typing the pattern.

## Done — editing the command line

Arrow keys and history inside `:` and `/`.

- **What shipped:** the line at the bottom is a `String` and a *character* index
  — not a `Buffer`, exactly as this item said it should be — so typing, deleting
  and moving inside it are four small functions rather than the whole line model
  dragged in. `Up`/`Down` walk what has been run, with commands and searches in
  separate histories, and the half-typed line is remembered while the walk looks
  at the history.
- **Constraints it had to respect, and how:** the cursor of the *terminal* had to
  learn about the prompt: while a line is being typed the terminal cursor belongs
  in that line, which is also where an input method's window should appear. A
  line longer than the window scrolls, counting characters rather than display
  columns — a command line is being typed into, and a wide character in it is
  worth the cells it costs rather than a second layout pass.
- **Files:** `editor.rs` (the cursor, the editing, the histories) plus one arm
  each in `render.rs` and its tests. No new commands and no keymap entries: a
  prompt reads its own keys, the way insert mode does.
- **Left for later:** completion of any kind, `q:` and `q/` windows, and
  searching *incrementally* while the pattern is being typed.

## Done — multi-key insert mappings

`ed.map("i", "jk", "<Esc>")`, and the typing that has to stay immediate around
it.

- **What shipped:** insert mode holds back the keys that *could* start a mapping
  and answers with the mapping the moment the sequence is complete; when a key
  proves the sequence is not a mapping, the held keys are typed after all, in
  the order they were pressed — a `<BS>` among them still deletes.
- **Constraints it had to respect, and how:** there is no clock in the daemon, so
  there is nothing to wait on: the keys are held rather than typed-and-undone,
  and a key that cannot start a mapping is typed immediately, which keeps
  ordinary typing as immediate as it was. A key bound on its own is answered at
  once, so binding both `j` and `jk` means the longer one can never fire — stated
  in the README, because it is the kind of thing a user will otherwise
  rediscover.
- **What the tests caught:** the sequence notation is *concatenated*
  (`parse_key("ij k")` is four keys, one of them a space), so a test that meant
  "type `i` then `jk`" has to say `ijk`; and the keymap's own rule — exact before
  prefix — is what decides the ambiguity, not the pending machinery.
- **Files:** `editor.rs` (a typed queue of held keys, the resolution and the
  flush), `keymap.rs` (the single-key rule and its error are gone), `lua.rs` (the
  error goes away), and the README paragraph that promised the limitation.

## Done — the pointer and the clipboard

Clicking, dragging, the wheel, and getting text in and out of the system
clipboard, with [`selection.md`](selection.md) as the design note.

- **What shipped:** the pointer as four messages in screen cells (`Press`,
  `Drag`, `Release`, `ScrollUp`/`ScrollDown`) and a copy as two (`ClientMsg::Copy`,
  `ServerMsg::Clipboard`). A drag makes the selection visual mode makes, so
  nothing downstream had to learn a second kind of selection.
- **Constraints it had to respect, and how:** a frontend sends cells and the
  daemon turns them into buffer positions, because only the daemon knows the
  gutter, the tabstops, the wide graphemes and the horizontal scroll — that walk
  is `draw_line` run backwards. The clipboard belongs to the system, so the
  kernel hands the text over (`Editor::clipboard`) and a frontend that can reach a
  clipboard writes it; OSC 52 is how the terminal frontend reaches one at all.
  Nothing waits for a reply: the protocol is still notification-only.
- **What the tests caught:** the daemon-side tests needed the *screen* to prove a
  drag (reverse-video cells across two rows) rather than the selection struct,
  which is what a user sees; and the 20-column test window truncated the status
  line, so the mode assertion had to be a prefix check.
- **Files:** `docs/selection.md` (new), `inkstone-rpc` (the messages),
  `render.rs` (`position_at`, the inverse walk), `editor.rs` (press/drag/release,
  the view scroll, the copy), `command.rs` + `keymap.rs` (`copy` on `<C-c>`),
  both frontends, `server` (the clipboard frame).
- **Left for later:** double-click word selection, triple-click lines, block
  selection, dragging past the edge to scroll, mouse reporting *to* a program
  running in the editor (there is no terminal inside yet), and named registers.

## Done — syntax highlighting

Comments, strings, numbers, keywords, names and constants, coloured by a
tokenizer, with [`highlighting.md`](highlighting.md) as the design note.

- **What shipped:** a tokenizer per language family, a per-line state cache, and
  a palette in the kernel — because "what a comment looks like" is the same
  question as "what colour is a cell", and the two frontends have to agree about
  it. The colours ride on the cell's foreground colour and attribute bits, so
  nothing on the wire and nothing in either frontend changed.
- **Constraints it had to respect, and how:** a block comment does not end where
  the screen does, so the state that crosses a line is carried *down the whole
  document* — cached per line, filled lazily to whatever line is on screen, and
  thrown away from the first line an edit could have changed. And the four things
  that make a cell — the token's colour, the selection, the search match, the
  cursor's match — have to *compose* rather than replace each other, which is why
  the row's styles resolve in one place instead of four.
- **What the tests caught:** the comment span must include the `/*` that opened
  it (a span starting after it looks like a bug on screen), Rust's nested
  comments need a walk rather than a search for the first `*/`, and `fn f<…>(…)`
  needs the name *definition* rule — a name followed by `(` is a call, and a name
  after `fn` is not.
- **Files:** `highlight.rs` (new, with the palette and the cache), `render.rs`
  (the row's styles), `editor.rs` (invalidation, the language, the setting),
  `settings.rs`, `docs/highlighting.md`.
- **Left for later:** `tree-sitter` and everything it brings, more languages
  (Markdown, JSON, TOML, shell), a colourscheme setting, and injection (a
  `<script>` inside HTML).

## Done — real patterns, and replacing with them

`/` searches with regular expressions, and `:s` replaces with them.

- **What shipped:** the same compiled pattern serves both, since a substitution
  replaces what a search finds. Patterns are Rust's dialect and are matched
  *within a line*, which is what keeps `^` and `$` meaning the line's ends and a
  match something the screen can draw; the replacement is written the way vim
  writes one (`&`, `\1`) and translated into the engine's spelling, with every
  group reference braced because `$2_` reads as a group *named* `2_`.
- **Constraints it had to respect, and how:** a substitute is one change. The
  first version replaced line by line inside one history group, and a group only
  folds steps that *continue* each other — two lines replaced in different places
  do not, so `u` undid one line and left the other. The fix is the shape it
  should have had: one replacement spanning the first and last line that changed,
  with the untouched lines between them riding along unchanged.
- **What the tests caught:** that undo bug, the `$2_` trap, a pattern that does
  not compile being swallowed and reported as "nothing found" (the two answers
  are different, and `:s` now says `E54`), and the arithmetic of a comment's span
  in the highlighter, which the same session fixed.
- **Files:** `search.rs` (regex, a compiled pattern kept, byte→character
  offsets), `substitute.rs` (new: the line's shape, the delimiter, the
  translation), `command.rs` (the `substitute` command), `editor.rs` (the
  application, one span, the messages).
- **Left for later:** vim's own regex dialect, `:s` with line ranges, the `c`
  flag, `:s` with no arguments (repeat the last one), and `*`/`#`.

## Done — more than one file at a time

- **What shipped:** `:e` opens a file as a document of its own, `:ls`, `:b`,
  `:bn`, `:bp`, `:bd` and their `!` forms manage them. Each document keeps its
  own cursor, history, search, syntax colours and scroll position; the register,
  the keymap and the options stay with the session. Design note:
  [`buffers.md`](buffers.md).
- **Constraints it had to respect, and how:** switching had to leave every other
  line of the editor alone. The live document's fields *are* the editor's fields
  and a switch swaps them with the slot, so a command reading `self.cursor` was
  already reading the right cursor. Two things could not be swapped: the window's
  size, which lives in the viewport but belongs to the window, and the selection,
  which holds character indices into the document that is leaving.
- **What the tests caught:** that a switch from a modified document must refuse
  (vim's `E37`) rather than lose it — and, much more usefully, that a *single*
  insert session's undo was broken: `ohello<Esc>` then `u` left the empty line
  behind, because `o` wrote its line break in a group that closed before the
  typing joined it. Fixing it meant a step knowing where its caret is, and a
  group folding into one step when it closes. Design note: [`undo.md`](undo.md).
- **Files:** `editor.rs` (`Document`, the swap, `switch_to`, `open_file`,
  `close_current`) and `command.rs` (the buffer commands); `history.rs` for the
  group fold and the caret.
- **Left for later:** `:b#`, marks per document, `'hidden'`, and
  unloading a document that is no longer needed.

## Done — windows, and the cursor in the right one

- **What shipped:** `:sp`, `:vs`, `:on`, `<C-w>w` `h` `j` `k` `l` `s` `v` `c`,
  and `:q` that closes a window. A window owns its cursor and its scroll; the
  document owns the text, the undo history and the colours. Design note:
  [`windows.md`](windows.md).
- **Constraints it had to respect, and how:** the cursor had been put in the
  document when documents arrived, and that is the wrong place for it — two
  windows on one file must move apart. Moving it meant a second level of the
  same swap trick, and it meant the layout had to be a tree rather than a list
  of rectangles, because `:vs` then `:sp` divides one half of the screen and not
  the other. No node stores a size: the screen is divided again every frame, so
  a resize is free.
- **What the smoke test caught:** `:vs file` showed the *file* in both windows,
  and a second window on the document that was already on screen came out
  blank — the live document's slot holds a placeholder, and both the split
  command and the renderer were reading from the slot. The fix is a rule worth
  naming: a window whose document is the live one reads the text from the view
  and only the position from the window.
- **Files:** `editor.rs` (`Window`, `Layout`, `Rect`, `window_rects`, the
  swap/split/close methods), `command.rs` (the window commands, the direction
  search), `render.rs` (panes, separators, hit-testing a cell back to a window),
  `keymap.rs` (`<C-w>` keys, and the README table the test checks).
- **Left for later:** resizing a split (and dragging its separator),
  per-window marks and folds, and `'winminheight'`.

## Done — which state was written

- **What shipped:** the `[+]` marker and `:q` now follow vim through undo. `u`
  back to the state a file was written in clears the marker; `u` past it, or
  `<C-r>` forward, puts it back; an edit that restores the same text is still an
  edit. Design note: [`undo.md`](undo.md).
- **The shape:** the buffer keeps no flag. The undo history hands out an
  identity to every state its steps make, and the last write remembers which one
  it wrote — so the question "has this changed?" is one comparison of two
  numbers, and no text is ever re-read or compared.
- **What the smoke test caught:** nothing this time, but the previous round's
  smoke did: `:q` refused after undoing back to a clean file and the session
  needed `:q!`. The test now covers the sequence at the editor level and the
  terminal run is what a person would do.
- **Files:** `history.rs` (state identities, `reset`, `is_dirty`, `mark_saved`),
  `buffer.rs` (the `dirty` flag and its generation both gone), `editor.rs` and
  `command.rs` (the questions and the write).

## Done — text objects, and the word under the cursor

- **What shipped:** `iw`/`aw`, quotes, brackets and their `a` forms, usable with
  `d`/`y`/`c` and in visual mode; `*` and `#` to search for the word under the
  cursor. Design notes are in the code and in `ARCHITECTURE.md`.
- **Constraints it had to respect, and how:** an object is not a motion — it
  cannot be run and watched, because the cursor must not move while it is being
  computed — so the operator gets a range rather than a name, and everything
  downstream (`cut`, `copy`, insert mode) is the code the motions already used.
  What is *around* the cursor is decided by scanning characters, which means a
  bracket inside a string counts: the highlighter knows better and a later
  increment can ask it.
- **What the tests caught:** that `ciw` on the *last* word of a line typed the
  new text one character early — the cursor the cut leaves is clamped for normal
  mode, and insert mode has to be entered before the cut, not after. The same
  latent bug was in `cw`, which no test had hit because `cw` at the end of a line
  is the case that shows it.
- **Files:** `command.rs` (the object ranges, `*`/`#`), `editor.rs` (the pending
  object, applying a range, visual-mode objects), `keymap.rs` and the README
  table.
- **Left for later:** `ip`/`ap` (paragraphs), tag objects (`it`/`at`), and
  syntax-aware objects that skip strings and comments.

## Done — named registers

- **What shipped:** `"a`-`"z`, `"0` (the last yank), `"1`-`"9` (the delete
  history), `"_` (the black hole) and `"+`/`"*` (the clipboard as far as the
  kernel can know it), plus `:reg`. The `"` comes before the operator, as in vim:
  `"ayy`, `"add`, `"ap`.
- **Constraints it had to respect, and how:** the kernel cannot read a clipboard,
  so `"+` is filled by what a paste *brought* — a bracketed paste is the system
  clipboard arriving, and remembering it at that moment is the only honest way to
  know. `"+y` goes out over the same path a copy does. The register prefix is a
  third pending state alongside the operator and the text object, and like them it
  is cleared whenever the mode changes.
- **What the tests caught:** that the black hole was still shifting the numbered
  registers — vim's `"_` leaves *every* register alone, not just the unnamed one —
  and that a stray `"` in a text-object test was being read as a register name,
  which is the prefix working as designed.
- **Files:** `register.rs` (the `Registers` store, the prefix state, the
  numbered-register shift), `editor.rs` (the `"` interception, `store`,
  `chosen_register`), `command.rs` (`:reg`, `p`/`P` reading the chosen register),
  `inkstone-server` (a paste fills `"+`).
- **Left for later:** uppercase registers (appending), `"-` (the small-delete
  register), `".` (the last inserted text), `"%`/`"#` (the file name), and
  reading the clipboard on demand.

## Done — the `.` repeat

- **What shipped:** `.` does the last change again, with a count; a change is a
  command that changed the buffer *or* opened an insert session, and an insert
  session's keys are replayed with it.
- **Constraints it had to respect, and how:** "did this command change
  anything?" had to be answerable without diffing text — the undo history counts
  its edits, so one comparison around the command answers it. And the typing that
  follows a command is not commands at all: it is recorded as it happens, as the
  keys that were typed, so a backspace inside the session repeats as a backspace.
  Replaying a *net* text instead would put back text the user had already rubbed
  out.
- **What the tests caught:** that my first expectations had the cursor's wish
  column wrong twice — `.` repeats the change *where the cursor is now*, and
  after `i…<Esc>` the cursor is on the last character typed, not past it. The
  behavior was vim's; the tests learned it.
- **Files:** `history.rs` (an edits counter), `editor.rs` (the recording, the
  session log, `replay_insert`), `command.rs` (`repeat`), `keymap.rs` and the
  README table.
- **Left for later:** `.` after a `:` command (vim replays the last `:s` with
  `&`, not `.`), and the keys that only moved the cursor inside a session.

## Done — character finds

- **What shipped:** `f`/`F`/`t`/`T` with counts, `;` and `,`, and the four of
  them as motions for `d`, `y` and `c` and inside a selection.
- **Constraints it had to respect, and how:** a find is two keys and the second
  is *any* key, so it cannot be a keymap binding — the keymap maps sequences to
  command names. It is caught in the three dispatch paths instead, and the
  character travels as a parameter. All four are *inclusive* motions, which is
  the flag that makes `dt)` delete the character before the bracket: `t` lands on
  the last character the operator should take, not on the one it should leave.
- **What the tests caught:** that the operator path hands over the motion's
  *name*, so `df,` had to remember the character before the operator ran — and
  that the visual path needed the same two-key handling as the normal one.
- **Files:** `command.rs` (the four finds, `;`/`,`, the motion table), `editor.rs`
  (the interception, `last_find`), `keymap.rs` and the README table.
- **Left for later:** `;` and `,` with a count of their own (`3;`), and finds
  that cross lines (`f` never does in vim either).

## Done — paragraph objects

- **What shipped:** `ip` and `ap`, in the operator grammar and in visual mode.
- **Constraints it had to respect, and how:** a paragraph is *lines*, so a text
  object's answer had to be able to say "whole lines, terminators and all" — the
  span that the motions already use, with the flag they already carry. That flag
  also had to reach the visual selection: a charwise selection cannot express a
  range that ends at a line terminator, because the cursor may not sit on one.
- **What the tests caught:** exactly that — `vip` then `d` left one line break
  behind, because the selection was charwise and its end was clamped off the
  terminator before the delete ran.
- **Files:** `command.rs` (`paragraph_object`, the object spans), `editor.rs`
  (`apply_operator` taking the kind, `select_object`).
- **Left for later:** `}` and `{` paragraph motions, and `'paragraphs'`.

## Done — six families of language

- **What shipped:** Python, shells, TOML/YAML/INI and JSON alongside Rust and the
  C family: `#` comments, Python's triple-quoted strings across lines, and JSON
  with no comments at all.
- **Constraints it had to respect, and how:** the scanner was written for two
  languages and had their differences hard-coded — `//`, block comments, raw
  strings. A grammar now *says* which differences it has (`line_comment`,
  `python`, `nested_comments`, `rust`) and the scanner reads those flags, so a
  language of an existing shape is a keyword list and an extension. The state a
  line ends in grew one field for a triple-quoted string, which is the only new
  thing a line can carry into the next.
- **What the tests caught:** that a `#` inside a string is not a comment, and
  that the line after an unclosed triple quote is still inside the string — the
  two ways a naive scanner gets Python wrong.
- **Files:** `highlight.rs` (the grammar tables, the flags, the scanner's Python
  branch), `docs/highlighting.md`.
- **Left for later:** injection (a `<script>` in HTML, a fence in Markdown),
  more languages of new *shapes*, and a real parser.

## Done — scrolling the window around the cursor

- **What shipped:** `zt`, `zz` and `zb`, and `5G` meaning the fifth line.
- **Constraints it had to respect, and how:** every move ends in `after_edit`,
  which keeps the view inside the document — right when the view is *following*
  the cursor, wrong when a command has just said where to put it. The scroll
  commands therefore set the scroll *after* that bookkeeping, which is what lets
  `zz` on the last line leave empty rows below the way vim does.
- **What the debug found:** two bugs, one of them mine and older. `f`/`t` were
  swallowing the key after *any* pending sequence (`z` then `t` became "start a
  till-find"), and `5G` was a repeat rather than a line number. The first was
  why `zt` appeared to do nothing — the command ran and then a half-typed find
  ate everything after it.
- **Files:** `command.rs` (the three scrolls, `goto_last_line`), `editor.rs`
  (`starts_find` requiring a clean slate), `keymap.rs` and the README table.
- **Left for later:** `'scrolloff'`, `Ctrl-E`/`Ctrl-Y` line scrolling, and
  `H`/`M`/`L`.

## Done — ranges for `:s`, and `:&`

- **What shipped:** `:5s`, `:2,3s`, `:.,$s`, `:$s` beside `%s` and a bare `:s`;
  `:&` to do the last substitute again on this line.
- **Constraints it had to respect, and how:** a range is *addresses*, not line
  numbers: `.` and `$` have to be read when the line runs, against the file as it
  is then, or `:5s/a/b/` would mean a line that an edit has moved. The parser
  therefore keeps the addresses and the editor resolves them — which is also what
  lets `:&` throw the range away and mean "this line" instead.
- **What the tests caught:** that `s/…/…/` with no address at all is the case a
  range parser forgets — `s` is not an address and neither is what follows it —
  and that my own expectations had the arithmetic wrong three times (a pasted
  editor swapping `one` for `ONE`, a count of lines that had matches).
- **Files:** `substitute.rs` (the addresses, `strip_range`, `address_range`),
  `editor.rs` (`substitute_rows`), `command.rs` (`apply_substitute`,
  `substitute_again`).
- **Left for later:** `:&&`, marks as addresses (`'a,'b`), and `/pat/`
  addresses.

## Done — the `c` flag: asking about every match

- **What shipped:** `:s///c` (and `:%s///gc`), with `y`, `n`, `a`, `q`, `l` and
  Escape, one undo step for the whole conversation.
- **Constraints it had to respect, and how:** asking is a *conversation*, and
  every other command in the editor is one key press from start to finish — so
  the substitution had to become a small state machine that lives across keys:
  a row, a position in that row to carry on from, and the match in hand. Carrying
  on from the end of what was written (rather than searching from the top of the
  row each time) is what keeps a replacement that contains the pattern from being
  found again for ever. The one undo step is the same trick insert sessions use:
  `run_command` leaves the group open while questions are up, and the last answer
  closes it.
- **What the tests caught:** that the group was opened twice and so never folded
  into one step; that `q` was being treated as "not an answer"; and that the
  message is cleared on every key press, which is right for a message and wrong
  for a question.
- **Files:** `substitute.rs` (the flag), `editor.rs` (`Confirming`, the
  start/advance/answer/finish pair, the `run_command` and `handle_key` seams),
  `command.rs` (`apply_substitute` starting one).
- **Left for later:** moving the cursor to the match being asked about, `^E`/`^Y`
  to scroll while answering, and `:&&`.

## Done — tab pages

- **What shipped:** `:tabnew [file]`, `:tab split`, `gt`/`gT`, `:tabclose`,
  `:tabs`, and `[N/M]` in the status line when there is more than one. Each tab
  keeps its own windows and splits; the documents are shared.
- **Constraints it had to respect, and how:** the third level of the same swap —
  a tab holds a window list and a layout, the live tab keeps them in the editor's
  own fields, and switching saves one and loads the other. Because the documents
  are *not* in the tab, a file opened in one tab is the same file in every other,
  which is what `:b` and `:ls` have to mean across tabs. `:q` became a three-step
  retreat: window, then tab, then session.
- **What the tests caught:** that a keymap binding is written *concatenated* —
  `"gt"` — while the README table spells it `gt` with a space, and that my patch
  had gone in with the README's spelling and so silently matched nothing. The
  README-vs-keymap test is what noticed, which is exactly what it is for.
- **Files:** `editor.rs` (`Tab`, the swaps, `open_tab`/`close_tab`/`step_tab`,
  the status line), `command.rs` (the commands, the two-word `:tab split`), the
  cmdline name table, `keymap.rs` and the README table.
- **Left for later:** `:tabmove`, `:tabonly`, `<C-w>T`, and a real tabline row.

## Done — resizing a split

- **What shipped:** `:resize`, `:resize N`, `:resize +N`/`-N`, `:vertical
  resize`, and `<C-w>+`, `<C-w>-`, `<C-w>=`.
- **Constraints it had to respect, and how:** the layout deliberately stored no
  sizes — every frame divided the screen again — and that is what made a resize
  free. So a split now stores what each child *asked* for and nothing else: zero
  still means "equal shares of what is left", which is every split that was ever
  made by `:sp`, and a window that is resized for the first time starts from the
  size it is drawn at rather than from nothing. The extra row or column that does
  not divide is given to the earliest windows, which is what the division did
  before anyone could resize.
- **What the tests caught:** that `:resize 5` and `:resize +5` are different
  answers to the same word — one asks for a size and the other for five more, and
  `" +5".parse::<i32>()` loses the difference — and that the *current* window is
  the one that moves, which is how the tests had to read the rectangles.
- **Files:** `editor.rs` (the sizes in `Layout`, `split_sizes`, `resize_window`,
  `equalize_windows`), `command.rs` (the commands, the two-word `:vertical
  resize`), `keymap.rs` and the README table, `docs/windows.md`.
- **Left for later:** `:resize` with no division asking for a bigger screen
  (`'winminheight'`), and `:vertical` for the other window commands.

## Done — dragging a separator

- **What shipped:** a press on the line between two windows picks it up and the
  size follows the pointer, for both axes, in the terminal and in the window.
- **Constraints it had to respect, and how:** the separator is found from the
  *rectangles* rather than the tree, because the rectangles are what was drawn: a
  vertical separator is the column left of a window that is not the first, and a
  horizontal one is the last row of a window that has another below it — which is
  its status line too. The drag carries the size it *started* at and the pointer
  position it started at, so the answer is a distance rather than the sum of
  however many drag events arrived, and a press there must not begin a selection
  because the separator is not text.
- **What the tests caught:** that the window on the near side of a separator ends
  *at* the separator column and not at the next window's left edge — an
  off-by-one that made every press miss — and that the right window shrinks
  visibly when the drag goes right, which the GUI screenshot shows as a truncated
  status line.
- **Files:** `editor.rs` (`Resizing`, `separator_at`, the pointer press/drag/
  release path, `resize_window_at`).
- **Left for later:** a cursor shape for a picked-up separator, and dragging one
  in a nested layout where more than two windows meet.

## Done — the diagnostics half of LSP

- **What shipped:** diagnostics per document, drawn as an underline on the range
  and a severity colour on the gutter's line number; `]d`/`[d` to walk them;
  `:diagnostics` to list them; the `set_diagnostics` command a client calls.
  Design note: [`lsp.md`](lsp.md).
- **What the design note settled before any code:** that the wire does not
  change. Every frontend feature in this repository has ended up the same way —
  the kernel owns the state and composes the grid — and diagnostics are no
  exception: an underlined range is an attribute bit the frontends already draw,
  and the severity is the colour of the line number, which is the one cell of a
  row that is ours. The note also fixes where the server process will live (the
  daemon, with the loop selecting between the socket and the server's channel)
  and where the protocol's UTF-16 offsets will be converted (in the client, in
  one place, with a test).
- **What the tests caught:** that a reloaded document kept the diagnostics of the
  text it no longer held, and that the document swap had to learn about them like
  every other per-document field; and that the gutter colour only shows when the
  line numbers do, which is a fact about the option and not about diagnostics.
- **Files:** `diagnostic.rs` (new: severity, ranges, what a row shows), the
  document and its swaps, `render.rs` (the underline, the gutter colour),
  `command.rs` (`]d`, `[d`, `:diagnostics`, `set_diagnostics`), `docs/lsp.md`.
- **Left for later:** the client itself — framing, the lifecycle, a thread and a
  channel, and the fake-server test — and the requests that answer something
  (completion first, which needs a popup and is a change of its own).

## Done — the language server client

- **What shipped:** `inkstone-lsp`: `/Content-Length` framing, the lifecycle
  (`initialize` with capabilities, `didOpen`, `didChange` with the whole
  document, `didSave`, `didClose`, `shutdown`), a reader thread and a channel, and
  `utf16_to_char`/`char_to_utf16`. Tested end to end against a real process over
  real pipes.
- **Constraints it had to respect, and how:** a server pushes, so a client that
  only reads when the editor asks would lose everything said in between — one
  thread per server, and a channel. Positions are UTF-16 code units in the
  protocol and characters everywhere else, so the conversion is a pair of
  functions with a test that uses a character outside the basic plane; a
  mid-pair offset rounds to the character that owns the unit.
- **What the test caught:** that the fixture cannot be the test binary — a test
  harness writes to standard output, and standard output *is* the protocol — so
  the fake server is a binary of its own, found through `CARGO_BIN_EXE_…`; and
  that a helper which drains messages up to the one it is waiting for throws away
  the notification that arrived first, which is exactly the notification this
  whole feature is about.
- **Files:** `crates/inkstone-lsp` (`framing.rs`, `position.rs`, `lib.rs`,
  `src/bin/fake-lsp-server.rs`, `tests/against_a_server.rs`), the workspace
  manifest.
- **Left for later:** the daemon wiring — a setting for the command, a `select!`
  in its loop, the payload conversion, `didChange` after a write — and then the
  requests that answer something.

## Done — the language server, wired in

- **What shipped:** `lsp` as a setting (off by default), a server found on `PATH`
  for the file that is open (or named by `INKSTONE_LSP`), `initialize` with the
  file's folder as the workspace, `didOpen`/`didClose` as the session's file
  changes, `didChange` with the whole document whenever the edit count moves, and
  the conversion of a `publishDiagnostics` payload into kernel diagnostics.
- **Constraints it had to respect, and how:** the daemon's loop blocked on the
  socket, so a server that pushes while nothing is happening would have been
  heard only on the next keystroke — the loop now waits with a timeout and looks
  at the server between messages, which needs no extra thread and is below what a
  person notices. Diagnostics belong to the *document* the payload names, and a
  session holds several, so both the live one and the slots are looked up. And
  the version the protocol wants for `didChange` was already being counted: the
  history's edit count, which only ever grows.
- **What the tests caught:** that a payload's position has to be converted
  against the line it points into and clamped into the file, since a server is
  often a keystroke behind; and, in the smoke, that the two ways of editing a
  terminal session differ — `x` is a change the daemon hears, and an `i` that
  nobody can escape from is a change it hears too but a session you cannot ask
  about afterwards.
- **Files:** `crates/inkstone-server` (the session's `LspServer`,
  `diagnostics_from`, `sync_lsp`, `pump_lsp`, `tick`, the loop's timeout),
  `crates/inkstone-core` (`edit_count`, the `lsp` setting), `crates/inkstone-lsp`
  (the fixture now answers a change with a second diagnostic, so "heard" and "not
  heard" can be told apart).
- **Left for later:** the requests that answer something (completion first,
  which needs a popup), incremental sync, and one server per *language* rather
  than per session.

## Done — completion from the words in the file

- **What shipped:** `<C-n>`/`<C-p>`/`<C-y>`/`<C-e>` in insert mode, a popup drawn
  in the grid under the word being completed, and the words of the file as the
  list. The design is vim's: the list *types* what it picks, so it is something
  you type through rather than confirm afterwards.
- **Constraints it had to respect, and how:** the popup is drawn where every
  other feature is drawn — cells in the grid — so no frontend learned anything
  and the wire did not change. The list gathers its words once, when it opens,
  and filters that gathered list afterwards: rescanning the file per keystroke
  would be a popup nobody could type through. It hangs from the *word's* start
  rather than the cursor's column, because the cursor moves along as items are
  typed and a list that moved with it would jump under the hands. And everything
  the list types is one undo step, using the group trick insert sessions use.
- **What the tests caught, and what the smoke could not:** three of my own
  arithmetic slips about columns and insertion points (a column is an insertion
  point, so "count" ends at column 9 and not 8); that `<C-n>` on a space offered
  nothing, because the item equal to an *empty* prefix was being filtered out of
  everything; and that the GUI's key injection is not reliable enough to drive a
  popup screenshot — the popup's drawing is asserted cell by cell in a unit test
  instead.
- **Files:** `completion.rs` (new: items, the word at the cursor, the words of the
  file, filtering), `editor.rs` (the state machine and the keys), `render.rs` (the
  popup), `keymap.rs` and the README table.
- **Left for later:** completion from a language server (`textDocument/completion`
  needs the client to match a request's `id` to its response; the kernel's
  `set_completions` is the seam and is already there), `<C-x>` modes, and fuzzy
  matching rather than a prefix.

## Done — completion from the language server

- **What shipped:** `<C-n>` also asks the server, and its answer replaces the
  popup's items; the client can make requests and take the answers by id.
- **Constraints it had to respect, and how:** an answer may arrive before anyone
  looks for it, so answers are kept by id rather than handed straight on. The
  kernel must not wait for a server — it does not know one exists — so the local
  words go up first and the answer replaces them when it comes. And the prefix
  that was typed goes back into the buffer when the answer arrives: a server
  answers about the word as it was asked about, and the local list may have typed
  a guess into it since.
- **What the test caught:** exactly that last one — the answer arrived, its items
  did not start with the guess the local list had typed, and the whole answer was
  filtered away. The test that noticed was asserting the server's items were
  being offered, and it was right to.
- **Files:** `inkstone-lsp` (`request`, `poll`, `answer`, the fixture's
  completion arm), `inkstone-core` (`completion_request`, `set_completions`
  restoring the typed prefix), `inkstone-server` (`ask_lsp`,
  `completion_items_from`, the answer in `pump_lsp`).
- **Left for later:** `completionItem/resolve` for the details of the picked
  item, `isIncomplete` re-asking on every keystroke, and fuzzy matching.

## Done — the jump list

- **What shipped:** a per-document list of the places jumps started from,
  `<C-o>` and `<C-i>` to walk it with a count, and `:jumps`.
- **Constraints it had to respect, and how:** the walk has to work *in both
  directions*, and that is one requirement with two halves. A jump records where
  it started — so the walk is built out of the places left behind — and walking
  records where *it* started too, which is what puts the current place in the
  list so that `<C-i>` has something to come back to. The other half is not
  adding a place twice: leaving a place that is already the entry under the index
  would fill the list with the same line, so it is skipped, and the walk moves by
  the index and not by the list's length.
  That left `''` alone, deliberately: it is one place and not a walk — the last
  jump's start — so pressing it twice toggles, which is what those keys are for
  and what they did before this list existed. The two share the *recording*, not
  the structure.
- **What the smoke caught:** that a terminal hands 0x09 over as a Tab *or* as the
  character `\t` depending on the terminal, so `<Tab>` had to be bound to the same
  command as `<C-i>` — which vim does too, for the same reason — and the TUI
  maps both spellings to one key.
- **What the tests caught:** that walking has to record first — without it the
  first `<C-o>` skipped over the place the walk was standing on and landed one
  too far back — and that a jump whose start and end are the same place is not a
  jump at all, which the first version of the test was made of.
- **Files:** `inkstone-core` (a `jumps` module with its tests, the `Document`
  field and its swaps, `record_jump`, three commands, two keys).
- **Left for later:** jumps across files (the list is per document) and `g;`/`g,`
  for the *change* list.

## Done — the last of the manual's "Behavior of folds" rules

- **What shipped:** the cursor is *drawn* in the first column while it is on a fold
  line. The word the manual uses is "displayed", and the first attempt at this read it
  as the cursor's own column and set it to zero — which undid the motion an operator was
  about to take (`dl` on a fold's first line became a motion of no width), and two tests
  said so at once. The rule belongs where the cursor's screen cell is worked out, which
  is one `Some(...)` in the renderer.
- **All four of the manual's rules are now in:** a closed fold is one line to a motion;
  an operator that reaches into one takes all of it; a search started on one begins past
  it; insert mode opens the fold the cursor is on; a substitute's range is widened to
  it; and the cursor is drawn in the first column on it.
- **What the render test needed:** the `Case` harness had no folds, so it could not
  draw one; it now carries a fold list and a level, which is what made this testable at
  all.

## Done — two more of the manual's fold rules

- **What shipped:** a search started on a closed fold begins *past* it — "a forward
  search always starts from the end of the closed fold, while a backwards search starts
  from the start of the closed fold" — and entering insert mode opens the fold the
  cursor is on, since "when in Insert mode, the cursor line is never folded. That allows
  you to see what you type!"
- **The third rule, which turned out to be about drawing:** "while the cursor is on
  folded lines, the cursor is always *displayed* in the first column". I read it as the
  cursor's own column and set it to zero — and two tests went red at once, because
  zeroing the column undoes the motion an operator is about to take (`dl` on a fold's
  first line became a motion of no width). The word the manual uses is *displayed*: it
  belongs in the renderer, where the cursor's screen cell is worked out, and that is
  where the roadmap now says it goes.
- **The fourth rule, which took two attempts and a wrong place:** "for Ex commands that
  operate on buffer lines, the range is adjusted to always start at the first line of a
  closed fold and end at the last line of a closed fold. Thus `:s/foo/bar/g` when used
  with the cursor on a closed fold will replace 'foo' with 'bar' in all lines of the
  fold." The first attempt widened the range where it is *resolved*, and did nothing:
  a substitute carries its range **in the line** (`Substitute::parse`), and the
  dispatcher hands that line over without the resolved range. The fix is where the line
  is built — the range is spelled out in front of the stripped line, and only when it
  actually changed, so `:%s/…/` stays `%`. The lesson is the one this project keeps
  meeting from the other side: **find out which value the code being changed actually
  reads**, before deciding what to change.

## Done — an undo takes a fold back with the text it was on

- **What shipped:** `u` restores a fold along with the text it was on. A fold is not
  text — it is a range of *line numbers* — so it is not in the history, and deleting
  the lines of a fold used to leave the fold gone for good. The editor now keeps a
  second stack, `fold_states`, whose entry `i` is the folds after `i` history steps,
  and an undo or a redo reads it at the step it lands on.
- **The design, and the two wrong versions of it:** the first wrote the "state being
  left" on every undo and redo; the second wrote it only in the undo. Both were
  trying to *fill* the stack, and the trace showed the stack only ever had one entry —
  the state *before* the first edit. The rule that works is the one that needs no
  writing at all in undo or redo: **each edit writes two entries** — the state it
  found and the state it leaves — and the two calls then only read. The truncate in
  them drops the entries an undone step can no longer reach.
- **The redo half, and the one word it was:** the trace at the end of `replace_range`
  showed both entries written — `[[the fold], []]`, the state found and the state left
  — and the trace in `restore_folds` showed the undo's `truncate(steps + 1)` **dropping
  the second one**. A redo lands one step *ahead* of where an undo leaves the history,
  so that entry has to survive: the truncate is `steps + 2`. An edit made after an undo
  drops it again with its own `steps + 1`, which is what keeps a redone-away step from
  landing on a stale fold.
- **The lesson, in the form this project keeps meeting it:** the first version of this
  tried to *write* the missing entries in undo and redo, and the answer was to *keep*
  one — the difference between filling a gap and not making it. The thread the roadmap
  wrote down was the one that found it, and it took one print in each place.
- **What the tests caught:** the two wrong versions above, each in one run, and that
  `truncate` does not grow — an index one past the end needs a push, not an
  assignment.

## Done — a closed fold is a whole

- **What shipped:** an operator that reaches into a *closed* fold takes all of it. The
  manual's own example is `dl`, "deletes the whole closed fold under the cursor", and
  it does: the span is widened to the fold's lines before anything is done with it.
  `widen_over_folds` is the one place that knows the rule, and both span sources (a
  motion, and a text object) go through it.
- **What the tests caught:** that the rule had to be in *two* places — the first
  version put it in `apply_operator`, which is the *object* path, and `dl` (a motion)
  went its own way. `motion_span` is where a motion's span is built, and both of its
  branches now widen.
- **The case that looked wrong and was not, settled by two prints:** a *linewise*
  motion from the line above a closed fold — `d2j` with the fold on lines 1-3 — deletes
  rows 0 to 4, which is *further* than the count says. Two prints settled it in one
  run: `take_operator` gives a count of two, and `motion_span` reports the span as rows
  0..4 — because the **stepping rule** is what moves it. `j` onto a closed fold's first
  line is one row, and the next `j` from there steps over the whole fold to the line
  after it. The widening had nothing to do: the span already covered the fold. There
  was no bug, and the case is now in the test with that explanation.
- **What the same trace turned up, which *is* a gap:** `u` puts the *text* of a deleted
  fold back and not the fold itself — a fold is not part of the history. Vim's undo
  restores folds. That is on the list below now, and it is why the `d2j` case needed a
  fresh editor: the `u` before it had quietly left a buffer with no fold in it.
- **Left for later:** the manual's other "Behavior of folds" rules, none of which are
  here yet — the cursor is displayed in the first column while it is on folded lines,
  a search does not find a match inside the current closed fold, insert mode never
  folds the cursor's line, and an Ex range is adjusted to a closed fold's bounds. And
  the one the trace turned up: **undo does not bring a fold back** with the text it
  was on.

## Done — `'foldlevel'`, `zm` and `zr`

- **What shipped:** `'foldlevel'` (how deep the folds are *shown* closed), `zm` (one
  level fewer) and `zr` (one more). A fold's *level* is its nesting — the manual's own
  sentence — so the depth is worked out from the fold list rather than stored.
- **The model, which took a test to get right:** there are *two* controls, and they are
  independent — the fold's own `closed` flag (what a person did with `zf`, `zo`, `zc`)
  and `'foldlevel'` (what `zm` and `zr` move). A fold is drawn as one line if *either*
  says so. That is why `indent` folds are now made with `closed: false`: with the
  automatic method the *level* is what decides, and a fold that was also marked closed
  by hand would ignore `zr` — which is exactly what the first version of the test
  showed, and what made the model obvious.
- **The side effect that is vim's too:** `zo` on a fold that the *level* is holding
  shut moves the level out of its way (to that fold's depth), since otherwise opening
  it would be impossible. `zR` sets the level past the deepest fold and `zM` sets it to
  zero, which is what makes the two mean "all of them" whatever the levels are.
- **Left for later:** `'foldminlines'`, `'foldtext'` (a fold line's text is vim's
  default shape and nothing else), and the other four methods.

## Done — `'foldignore'` and `'foldnestmax'`

- **What shipped:** `'foldignore'` (characters that make a line *ignored* by indent
  folding: it takes the lower of the levels around it, which is what `#` is for in C)
  and `'foldnestmax'` (how deep the nesting may go; twenty, as in vim).
- **The rule from the manual, again:** "Some lines are ignored and get the fold level
  of the line above or below it, whichever is lower. These are empty or white lines
  and lines starting with a character in `'foldignore'`. White space is skipped before
  checking for characters in `'foldignore'`. For C use `#` to ignore preprocessor
  lines." That is the whole of `'foldignore'`, and it went in as three lines of code
  where the blank-line rule already was.
- **What the tests caught, and the real find:** `:set foldignore=#` had no effect on
  the folds. Setting the option was right; what was missing was *working the folds out
  again* — the hook added for `'foldmethod'` fired only for that one setting. With
  `indent`, the folds are a function of the settings (the method, the ignored
  characters, the depth, and what a tab counts as), so any `:set` while the method is
  `indent` now works them out again. A `:set` is not a keystroke; a promise of folds is
  not folds.
- **And two of my own expectations:** that the `#` lines cut the block into *two* folds
  (the second piece was one line, and a one-line fold is not a fold), and that the
  fold without `'foldignore'` would start at `body` — it does, and with `'foldignore'`
  it starts at the `#include` line itself, since an ignored line's level is what the
  fold is measured by.
- **Left for later:** `'foldminlines'` (folds shorter than that are not made — this
  editor's rule is the blunt "more than one line"), and the other four methods.

## Done — `:mkview` and `:loadview`

- **What shipped:** `:mkview` writes the window's folds and position to a file of their
  own; `:loadview` reads them back. The file lives under the config directory and is
  named after the file it is for plus a hash of its full path, so two `main.rs` in
  different projects do not share one. A view that is not there says so, and a view
  made before the file changed drops the folds that name lines which are gone.
- **The rule the manual gave, which is why this was worth doing:** "the manual folds
  are lost when you abandon the file. To save the folds use the `:mkview` command."
  That sentence is two features: saving them, and *losing* them — and the second was
  missing here. Folds belong to the window, so showing another file in it left the
  folds of the *previous* file in place, pointing at line numbers of text that was not
  there any more. The test caught it: a fresh editor with a fresh file came up with
  two folds.
- **Where the views go, and why the hash:** vim mangles the path into a name in
  `~/.vim/view`. This keeps them in the config directory beside `init.lua`, under the
  file's own name and an FNV-1a hash of its full path — a hash small enough to write
  in ten lines, and enough to keep same-named files apart.
- **What the tests caught:** the stale-folds case above, and that a `:` command's name
  is what the dispatch table says it is — `make_view` is the function, `mkview` is the
  command, and the two are joined in one table (the test said `E492: Not a command:
  mkview` until the name was added).
- **Left for later:** `:mkview {n}` and `:loadview {n}` (vim keeps nine), what a view
  does about *options* and mappings (vim's holds them too), and the auto-view/auto-save
  pair.

## Done — `'foldmethod'` with the indent method

- **What shipped:** `'foldmethod'` (`manual`, the default, or `indent`) and `zx`, which
  works the folds out again. With `indent` the folds come from the indentation.
- **The rule, from the manual I had already read:** "the foldlevel is computed from
  the indent of the line, divided by `'shiftwidth'` (rounded down). A sequence of
  lines with the same or higher fold level form a fold, with the lines with a higher
  level forming a nested fold. Some lines are ignored and get the fold level of the
  line above or below it, whichever is lower. These are empty or white lines…" That
  is the whole algorithm, and it went in as three sentences of code: a level per line,
  blank lines taking the lower neighbour, and a fold starting where the level *rises*
  and ending at the last line at least as far in.
- **The one substitution:** the manual divides by `'shiftwidth'`, which this editor
  does not have — indentation here is `'tabstop'` — so that is what divides, and both
  the README and the setting's own doc say so rather than leaving it to be discovered.
- **When they are worked out, and why not more often:** when the method is set, and by
  `zx`. Vim's own manual calls the `expr` method expensive because it recomputes
  per line; doing this after every keystroke would be the same mistake. The *line
  numbers* of the folds are still kept up to date by every edit (that is what
  `shift_folds` is for), so a fold made this way does not drift — what goes stale is
  the *shape* after a big re-indent, which is what `zx` is for.
- **What the tests caught:** one of my own expectations about what a *closed outer*
  fold hides (everything inside it, inner fold and all — the code was right).
- **Left for later:** `'foldnestmax'` (a cap on nesting), `'foldignore'` (lines whose
  indentation does not count), `'foldminlines'`, and the other four methods.

## Done — `'virtualedit'` for a block, at the second attempt

- **What shipped:** a `<C-v>` rectangle may reach past the end of a short line. `l`
  and `h` in a block move the column and nothing else, and the two column clamps
  (`Cursor::clamp` in `after_edit`, and `clamp_normal_cursor`) let a block's column
  be free. Normal mode is untouched: `l` still stops at the last character.
- **How the thread was pulled, which is the point of this entry:** the first attempt's
  branch never ran and a print inside it never fired. The roadmap said to pull that
  thread, so this time the *probe came first*: a print at the top of `handle_visual`
  (it **is** called for every visual key), then a print of the branch's own condition —
  `block=true mode=Visual selection=Some(Blockwise)` — and *then* the branch. It ran.
  Two prints, one run each, and the thing that had cost a whole turn was settled.
- **What was different, as far as can be told:** nothing in the condition; the branch
  is the same `if` it was. What is certain is the order of work: last time the branch
  was written and *then* looked for; this time it was looked for first, and written
  where the probe said it would run. When a patch does not take effect, find out
  *where the code is* before deciding what it should say.
- **What the test caught:** that coming *down* onto a short line stops at its end
  (`move_down` clamps the column, which is right — the rectangle is the thing that may
  go past it), and that three `l`s from there reach three columns past the line.

## Tried and put back — `'virtualedit'`, and a test I lost

- **What I set out to build:** a block whose corner is *past the end of a short line*,
  which is what vim's `'virtualedit'` is for: `<C-v>` then `l` should keep going when
  the line under the cursor has run out, so a rectangle can cover columns that only
  some of its lines have.
- **Why it is not in the tree:** the column clamps in two places (`Cursor::clamp` and
  `clamp_normal_cursor`), and both were exempted for a block — but the *motion* still
  stops, because `Cursor::move_right` moves down a line when the column is at the end
  rather than going past it. The branch that was meant to handle `l`/`h` in a block was
  written, placed at the top of `handle_visual` — and never ran: a print inside it
  never fired, and I did not find out why before the turn ran long. So the whole thing
  came back out rather than shipping a half-rule.
- **What the next attempt should start from:** the print that never fired is the
  thread to pull — `handle_visual` is called for every key in visual mode (the match
  in `handle_key` says so), so either the branch was not where I thought it was or
  something before it returns. The two clamp exemptions are the enabling half and are
  written down here rather than in the tree.
- **What that cost, which is the real lesson:** rewriting the *nesting* test region
  with a slice — `s[:start] + new + s[end:]` — silently deleted the test that was in
  between, and the suite stayed green at 411 because a test that is gone cannot fail.
  It was found only when a later `grep` for the test name came back empty. **A slice
  that replaces a region takes everything in it**, and the count of tests is worth
  watching when one is.
- **What did land from the turn:** the nesting test is back, and it now says what it
  knows — the folds nest, a closed outer one takes an inner one with it, the cursor
  settles on the *innermost* fold it is inside, and a fold over the same lines twice
  is one fold. Three of its cases had to be rewritten because a *motion* over a closed
  fold steps over it: that is the rule working, not the test being awkward.

## Done — nesting, and folds a motion steps over

- **What shipped:** folds *nest* — `zf` inside another fold is a fold of its own
  rather than a replacement for it — and a closed fold is one line as far as a motion
  is concerned, so `j` from a fold's first line comes out at the line after it and `k`
  into one lands on its first line.
- **The two rules, and why they are the same rule:** vim's fold *level* is its
  nesting, and everything else follows from that. `fold_lines` used to remove any fold
  that overlapped the new one — which made the second `zf` replace the first — and now
  removes only one over *exactly* the same lines. The drawing needed no change at all:
  `closed_fold_at` finds the outermost closed fold that starts at a line, and the walk
  skips to its end, so a closed outer fold takes an inner one with it by construction.
- **What the tests caught, and what it cost to see:** three tests went red when the
  stepping rule went in, and the reason was the same in each — the *direction* the
  cursor was going is what decides where it lands, and nothing in the editor knew it.
  `settle_cursor` now takes the row the cursor came from (`last_cursor_row`, kept by
  `after_edit`): down into a fold comes out after it, up into one lands on its first
  line, and an edit that did not move the cursor at all — closing a fold — puts it on
  the first line, which is the line that is drawn.
- **The test that took four tries:** `a_fold_follows_the_lines_it_was_put_on`. Every
  failure was arithmetic about *where a line went* (`o` puts the new line below the
  cursor, not on it) or about a cursor set by poking the field rather than by moving —
  which leaves `last_cursor_row` stale, and is the live/slot lesson in its newest
  clothes. It is now one insert above, one insert inside with the fold open, and one
  delete.
- **Left for later:** fold *levels* as numbers (what `zm`/`zr` would need — "one level
  more or less"), `'foldmethod'` (indent, marker, expression), `'foldtext'`,
  `:mkview`/`:loadview`, and `zj`/`zk` skipping *nested* folds one level at a time.

## Done — `zj`/`zk`, and folds that follow the text

- **What shipped:** `zj`/`zk` (with a count) step to the start of the next and
  previous fold, and the folds are kept up to date as lines come and go: a line added
  above a fold moves it down, one added inside makes it bigger, and one taken away
  makes it smaller — so a fold stays on the text it was put on.
- **Why the second one mattered more:** a fold holds *line numbers*, and without this
  it drifts off its text the first time anyone types above it — a fold that lies about
  where it is, is worse than no fold. The fix is in `replace_range`, which is the one
  place every edit goes through: the line the edit starts on is read before the change
  (after it the index means something else), and the *delta* is read from the buffer
  afterwards.
- **The mistake in that fix, which the trace caught in one run:** I worked the delta
  out from the text that was inserted — "one line per newline, plus one if it does not
  end with one" — and that is wrong for every insert that is not at the end of a line.
  Typing `n`, `e`, `w` into a new line moved the fold down three times. The buffer's
  own line count before and after is the only thing that knows how many lines an edit
  made, and it now says so.
- **And a rule I got wrong by reasoning:** `zj`/`zk` first filtered on `closed`, on the
  argument that an open fold is not in the way. The trace showed the cost — with the
  only fold before the cursor open, `zk` said there was no fold at all. Every fold
  counts, open or closed: a fold that is open is still a fold.
- **Left for later:** fold levels and nesting, `'foldmethod'`, `'foldtext'`,
  `:mkview`/`:loadview`, and a fold whose *text* is edited (the range follows, the
  identity does not).

## Done — folds

- **What shipped:** `zf{motion}` (an operator, like `d` and `y`), `zo`/`zc` for the
  fold the cursor is in, `zR`/`zM` for all of them, and `zd`. A closed fold is drawn
  as its first line with `+--  N lines: ` in front of it; the lines it hides are not
  drawn at all.
- **The one structural thing folds change:** the rows and the lines stop being the
  same thing. Everything in this editor assumed `line_no = top_line + row` — one
  addition, in the renderer — and with a closed fold the Nth drawn row is not the Nth
  line. That is the whole of the drawing change: one walk of the visible lines
  (`visible_lines`), used by the renderer, and the viewport snapping off a line that
  is not drawn. The view's scroll is still worked out in *lines*, so `'scrolloff'`
  can be a line or two out with folds closed; the cursor is always on screen, which is
  what the setting is for, and the roadmap says so.
- **What is deliberately not here:** fold *levels*, which is what nesting is. Vim's
  `zR`/`zM` walk the levels; this has one flag per fold. A fold is a range and open
  or closed, and the type says so — adding levels later means comparing a number
  instead of a flag, not re-cutting the feature.
- **Where the state lives, and the line this project keeps meeting:** folds belong to
  the *window* (vim's rule, and the reason the same file can be folded two ways in
  two windows), so they join the window's save/load swap — the same rule the notebook
  has carried since the list windows, applied before it could bite.
- **What the tests caught:** three of my own slips — a `settle_cursor` that landed in
  `handle_resize` instead of `after_edit` (the same two-line pattern appears twice in
  the file), a one-line `zf` expectation that used a *linewise* motion (`zfj` folds
  two lines, `zfl` folds none), and an error message I had borrowed from vim without
  meaning what vim means by it (`E350` is about `'foldmethod'`; a fold of one line now
  says "cannot fold one line").
- **Left for later:** fold levels and nesting, `'foldmethod'` (indent, marker,
  expression), `'foldtext'`, `:mkview`/`:loadview` (folds do not survive the file
  being closed), `zj`/`zk` (the next and previous fold), and folds that follow edits
  (a fold holds line numbers, so inserting a line above it moves the fold with the
  text only by luck).

## Done — `'path'`, and a setting that is a list

- **What shipped:** `'path'`, a comma-separated list of directories, and `gf`
  searching it when the name is not beside the file that is open. `:set path=src,tests`
  is how it is written, and the empty parts of a list are not directories.
- **What it cost, and why that is worth knowing:** every setting was `Copy`, so
  `Options` was too, and two places copied the whole thing (`let options =
  *self.options.borrow()`). A list cannot be `Copy`, so the bound became `Clone` and
  those two lines clone. That is the whole change — and it is the kind of thing that
  is *cheap only because* the settings table is generated from one macro: a hand-written
  settings struct would have meant touching every field.
- **What the tests caught:** that the round-trip test only knew how to probe a bool or
  an integer and panicked on anything else — "unprobeable type" — which is exactly the
  test doing its job: it exists to make a new setting *prove* it round-trips, and a
  list has to say how.
- **Left for later:** the count (`3gf` goes to the third name on the line — vim's rule,
  which I have not read and will not guess at), `'path'` with `**` (vim's wildcards),
  and `<C-w>f` (open the name in a split).

## Done — `gJ`

- **What shipped:** `gJ` (normal and visual): join without inserting or removing any
  spaces at all, which is vim's rule — the terminators go and the next line follows
  exactly as it was, indent and all. It shares `J`'s body through one flag, so the
  range arithmetic, the single edit and the one `u` are the same code.
- **What the tests caught:** that my patch to the shared body had not applied — the
  test said `gJ` behaved exactly like `J`, which is the *useful* shape of a failure
  (the keymap was right, the flag never reached the loop, and the output said so).
  The lesson is the one from two turns ago, in its cheapest form: when a patch and a
  test disagree, read the function, not the patch.
- **Left for later:** `gJ` and `'joinspaces'` in `gq` (vim uses the option when
  formatting joins lines too), and the bracket/quote clause of `'joinspaces'`.

## Done — `'joinspaces'`, and a panic it found

- **What shipped:** `'joinspaces'` (on by default, as in vim): a line that ends with
  `.`, `?` or `!` is joined with two spaces, and `:set nojoinspaces` makes it one.
  Vim's clause about a bracket or a quote between the stop and the space is not done,
  and the README says so.
- **The panic, which is the real find:** the new test made the editor *crash* —
  `Attempt to index past end of Rope: line index 2, Rope line length 2` — and the
  backtrace put it in `clamp_normal_cursor`, which asked for the length of the
  cursor's line *before* clamping the cursor's row. `J` is what exposes it (joining
  four lines into one leaves the cursor below the last line), but any command that
  takes lines away could have: `:g/…/d`, `:sort` on a range, a block delete. The row
  is now clamped first, which is what `clamp_cursor` already did.
- **How long the panic took to find, and what finally did it:** four wrong guesses
  (the live buffer, the history's indices, the merge, `text_range`) before I asked for
  a **backtrace** instead of a print. The lesson is the one from the block-`c` turn,
  one level up: when the answer is a call stack, print the stack, not the state.
- **What the tests caught:** the panic, and two of my own ordering slips (a test that
  reused an editor whose buffer a previous case had already joined).
- **Left for later:** the bracket/quote clause, `gJ` (join with no space at all), and
  `'joinspaces'` in `gq` (vim uses it when formatting joins lines too).

## Done — `J`, and where its rules came from

- **What shipped:** `J` (join with the next line, `count` lines in all, `E85` on the
  last line) and `J` over a selection (every line of it).
- **Where the rule came from:** the visual-mode operator table I read last turn —
  `J` is listed there as "join (1)", and the note for (1) is "**Always whole lines**".
  That sentence *is* `vJ`'s behaviour, and it is the reason this was worth doing
  without another read: the rule for the visual form was already quoted in the
  roadmap. The normal form is the one everybody knows — the indent goes, a space
  takes its place.
- **The honest simplification:** vim puts *two* spaces after a `.` `!` or `?` when
  `'joinspaces'` is on (it is on by default). This does one space every time, which
  is vim with the option off, and says so rather than guessing at the rule.
- **What the tests caught:** two of my own mistakes in the *range* — I first joined
  from the start of the first line (which deleted it), then replaced the whole range
  with one space per join (which deleted every line but the first). The range is from
  the end of the first line to the end of the last one's text, with the lines put back
  in order; the last line's terminator is outside it and stays where it was.
- **Left for later:** `'joinspaces'` and the two-space rule, `gJ` (join with no space
  at all), and `J` on a block selection (which vim does not do either).

## Done — keyboard macros

- **What shipped:** `q{register}` to record, `q` to stop, `@{register}` to play,
  `3@a` for a count, and `@@` for the last one. The second attempt, and it landed.
- **What the first attempt got wrong, and what the design turned out to be:** it put
  the letter-taking state *beside* the keymap's own pending state, and the two
  fought over `q`. The answer was to make it part of the pending machinery: `q` is
  already a keymap *prefix* (`q:` and `q/`), so a `q` followed by a letter the table
  does not name is a macro — answered in the `Lookup::None` arm of the *normal*
  lookup, with `:` and `/` handed straight back to the keymap. That is why `q:`
  still works, and why nothing had to be special-cased twice.
- **The two things that took the longest, both found by tracing:** the branch first
  landed in the `Lookup::None` arm of the *operator-pending* lookup, where it never
  ran; and a bare `q` while recording does not arrive as a pair at all, so waiting
  for `qq` never ended anything. The stop is answered before the keymap, which is
  the one place in the editor where a key means two things — and the roadmap says
  which reading won (vim would record the `q:` and open the window; ending the
  recording is what a person expects).
- **The one honest simplification:** a recording stops at a bare `q`, so `q:` and
  `q/` cannot be *used* while recording. Vim records the pair. It is written down
  rather than guessed at.
- **Left for later:** `.` replaying the last macro, `@@` after a *counted* `@`
  (it repeats the register, which is what vim does), and macros in registers that
  `"ap` can paste (vim keeps the keys in a register; this keeps them in a store of
  its own).

## Tried and put back — keyboard macros (the first attempt)

- **What I set out to build:** `q{reg}` to record, `q` to stop, `@{reg}` to play,
  `@@` for the last one, with a count. It is the biggest thing this editor still
  lacks after the basics, and the architecture looked ready for it: `handle_key` sees
  every key, so recording is "spell the key and append it", and playing is "parse the
  register and feed the keys back through the same funnel".
- **Why it is not in the tree:** the keys fight. `q` is *already* a keymap prefix —
  `q:` and `q/` are the history windows — so a `q` that waits for a register letter
  has to be answered before the keymap's lookup, and then `:` and `/` have to be
  handed *back* to it, and the hand-back has to restore the prefix the keymap would
  have made. I did that, and it broke in a way I did not chase to the bottom: a
  recording was already running when the `q:` test pressed `q`, so the test saw `q`
  *ending* a macro that the test never started. Two tests were failing and one of
  them (`q:`) had been green; the honest move was to put the whole thing back rather
  than ship a `q` that breaks the window it shares a key with.
- **What the next attempt should start from:** the state and the shape are known —
  `recording: Option<(char, String)>`, `macros: [Option<String>; 26]`, `last_macro`,
  and a `macro_ask` that waits for the letter. The thing to solve first is the *key
  conflict*: either give the recording a key the keymap does not use as a prefix, or
  make the "waiting for a letter" state part of the keymap's own pending machinery
  rather than a second mechanism beside it. The second is probably right, and it is
  the piece of design I did not do before writing code.
- **What did land from the attempt:** the counted operator, which turned out to need
  no code at all — `2d3w` and `2d<C-v>j` already multiplied their counts, because
  `take_operator` multiplies them before the motion runs. Two tests now say so.

## Done — forcing a motion's shape

- **What shipped:** `o_v`, `o_V` and `o_CTRL-V` — `v`, `V` and `<C-v>` after an
  operator say what shape the motion's range will be. The three are one mechanism
  (`ForcedShape`), which is why the block form landed last turn and the other two
  were a small change to the same seam.
- **Where the shape belongs:** not in the operator's command signature — that would
  have touched ten commands — but on the *editor*, read by `motion_span` where the
  span is built, and **taken** there, since one operator takes one motion. That is
  the whole of the plumbing: a field, a branch in the operator-pending handler, and
  three lines in the span builder.
- **What the manual said, and what it means for `inclusive`:** `o_v` "forces the
  operator to work characterwise, also when the motion is linewise. If the motion was
  linewise, it will become exclusive. If the motion already was characterwise, toggle
  inclusive/exclusive." That sentence *is* the implementation: `inclusive = kind !=
  Linewise && !motion.inclusive`.
- **What the tests caught:** four of my own expectations about what exclusive means —
  I kept expecting `dv$` to take the last character (it does not: `$` is inclusive,
  so `v` makes it exclusive) and `dvj` to take the whole next line (it does not: it
  stops at the character below). The rule was right in the code the first time and
  wrong in my head four times, which is the useful way round.
- **Left for later:** a counted operator (`2d<C-v>j`), `o_v` with a find (`dvf,`),
  and `virtualedit` (a block corner past the end of a line).

## Done — `d<C-v>`, an operator forced to be blockwise

- **What shipped:** `d<C-v>{motion}`, `y<C-v>{motion}` and `c<C-v>{motion}` —
  vim's `o_CTRL-V`, which the manual states in one line: "When used after an
  operator, before the motion command: force the operator to work blockwise. This
  works like Visual block mode selection, with the corners defined by the cursor
  position before and after the motion." The example is `d<C-V>j`: "deletes the
  character under the cursor and the character below the cursor."
- **How long the answer took to find, and why that is the lesson:** four reads of
  vim's manual across two turns, because I kept looking in `visual.txt` (where the
  *visual* forms live) for a rule that is in `motion.txt` under *forced motion*. The
  lesson is not "read more" — it is "read where the rule is about", and the two turns
  I spent not building this were spent looking in the right manual but the wrong
  chapter.
- **The design, once the rule was known:** `<C-v>` after an operator is *not* a
  motion, so it must be answered **before** the motion lookup — the first attempt put
  it in the `visual_block` command and it never ran, because the operator handler
  asked "is this a motion?" first and said "`<C-v>` is not a motion". The block's
  anchor is the cursor at that moment; when the motion arrives it runs *as a motion*,
  and the rectangle is made into a blockwise selection; then the operator's *visual*
  command applies it. That last step is the whole trick: `Operator::visual_command`
  exists so that a forced block goes through exactly the same code as a selected one,
  which is why `d`, `y` and `c` all worked with no further changes.
- **What the tests caught:** that a motion is *one* movement command — `d<C-v>j2l`
  applies after the `j` and the `2l` is a separate command, which is what the manual
  says and what my test did not.
- **Files:** `inkstone-core` (`Editor::block_anchor`, the `<C-v>` branch in
  `handle_operator_pending`, `Operator::visual_command`).
- **Left for later:** `d<C-v>` with a *counted* operator (`2d<C-v>j`), `virtualedit`
  (a block whose corner is past the end of a line), and `o_v`/`o_V` (forcing a motion
  to be characterwise or linewise, which is the same idea and two more keys).

## Done — `q:` and `q/`

- **What shipped:** the command and search history in a window of their own, oldest
  first, with `<CR>` running the line under the cursor and closing the window.
- **Why it is a list and not a message:** a history is a list of things to *do
  again*, which is exactly what the list windows are for — and the editor already
  had the shape (a read-only document, a window of its own, a `<CR>` that means
  something). The one new thing is that `<CR>` *runs* the line rather than going to a
  place, so the list kind decides what `<CR>` does: places for a quickfix list, and
  a line to run for a history.
- **The one thing that surprised me:** a command run from a *script* is not in the
  history. `remember_prompt` is in the prompt's Enter handler, not in
  `execute_cmdline`, so `ed.execute_cmdline("set number")` — what a test does — leaves
  no trace. That is right (a history is what a person typed) and it is worth knowing.
- **What the tests caught:** three of my own mistakes — that `parse_key` reads a
  two-key sequence's space as part of it (`"q :"` is not two keys, so the test feeds
  them one at a time), that `q` was being used by *other* tests as an example of an
  unmapped key (it is a prefix now), and that my test's little `prompt` helper
  hard-coded the `:` it was called with — which made `/one` a *command*.
- **Files:** `inkstone-core` (`List::Commands`/`List::Searches`,
  `open_history_window`, the `<CR>` dispatch, two keymap rows).
- **Left for later:** `q?` (backward searches), editing a line before running it (vim
  lets you: the window is not read-only for the *current* line), and `:` on a
  history line putting it on the command line instead of running it.

## Done — a line too short to reach the block

- **What shipped:** the rule vim documents and this did not have. `v_b_I`: "a line
  that does not extend into the block remains unmodified". `v_b_A`: appending pads
  to the end-of-block column. So `I` and `c` skip such a line, `A` pads it, and a
  block made with `$` has no column to reach — `A` appends at the end of each line.
- **How the answer was found:** by reading vim's manual, which is the thing I said
  I would do before building the operator form of a block and had not done. It said
  two things this editor had wrong: that `I` does not pad, and that `$` in a block
  means "to the longest highlighted line" — which, for `d`, gives exactly what the
  ragged version gives, but for `A` does *not* (vim appends at the block's edge,
  not at each line's end). The ragged reading of `$` turned out to be right after
  all, for `A` on a `$`-block; the padding was not.
- **The design:** which rows a change touches is a question about the block's
  *shape*, so it is asked before anything is deleted — asking afterwards would say
  every line had shrunk out of the block, since the deletion is what shortened them.
  `start_block_insert` takes the rows rather than a range, which is what lets the
  caller answer that question.
- **What the tests caught:** that the `c` case was asking the question in the wrong
  order (its rows had already been shortened), and two of my own arithmetic slips
  about padding.
- **Left for later:** `d<C-v>` (the operator form — the manual says the block is
  made with motions and the operator applies after them, which is a design of its
  own), and `virtualedit`, which vim needs for blocks that start past the end of a
  line.

## Done — `:set all` and `:set inv{name}`

- **What shipped:** `:set all` — every setting and its value, in a window of its own
  — and `:set inv{name}`, which turns a switch the other way.
- **The decision:** `:set all` could have been one long message, and vim shows a
  pager for it. This editor already has a shape for "a lot of lines a person wants
  to scroll": the list window. So it is one — a read-only document with no targets,
  since a setting's name is not a place to go to — which means it scrolls, it can be
  closed, and it inherits the read-only rule (`E21`) that every other list has.
- **What the tests caught:** nothing about the feature; what it caught was that I had
  put the `:set` handler in the wrong file. `:set` is in the *editor* (where the
  three spellings live) and not in `command.rs`, and my patch matched nothing there
  — a `cargo build` that passes after a patch that did not apply is a thing to
  watch for.
- **Files:** `inkstone-core` (`List::Settings`, `open_settings_window`, the `all`
  and `inv` arms of `:set`).
- **Left for later:** `:setlocal`/`:setglobal` (they need a scope for options that
  does not exist yet), and changing a setting *from* the window (`<CR>` on a line
  could run it).

## Done — `$` over a block, and what a block's right edge is

- **What shipped:** `$` over a `<C-v>` block makes it ragged — the right edge becomes
  the end of each line — so `d` takes the tail of every line and `A` types at the end
  of each. `I` and `c` are unaffected (their edge is the left one), and `$` with any
  other selection is the line-end motion it always was.
- **The design, and why it was worth doing properly:** a block's right edge stopped
  being a `usize` and became an `Option<usize>`, where `None` means "the end of this
  line". That one change says the whole thing: the spans, the highlight, and where
  `A` types all ask the same question and all get the same answer, and no path had
  to learn about raggedness separately. It is also why the change went through the
  renderer without a special case — `selection_columns` already worked a row at a
  time.
- **The lesson:** the ragged block is the first thing in the block feature that is
  *not* a rectangle, and the right way to say it was to make the *shape* say it
  rather than to add a flag that every caller has to remember. The flag is in the
  selection (`to_end`), which is where it belongs: it is what a person asked for.
- **What the tests caught:** nothing this time — the case ran green on the first
  try, which is the first time that has happened for a block increment, and is
  probably the sign that the shape was right.
- **Files:** `inkstone-core` (`Selection::to_end`, `visual_block` and `block_span_on`
  taking an optional right edge, `block_to_end`, one keymap row).
- **Left for later:** `d<C-v>` (a block after an operator, which needs the operator
  machinery to hold a block pending), and `$` as a block edge in *insert* mode's
  block insert (`A` covers it).

## Done — `gf`

- **What shipped:** `gf` opens the file whose name is under the cursor, read as a
  file name rather than as a word, resolved relative to the file that is open.
- **The one decision in it:** the name is a run of *file-name* characters, not the
  word the motions use. `src/main.rs` is three words and one file name, so a `gf`
  built on `w` would open `main`. Vim calls the rule `isfname`, and this is that rule
  in spirit: letters, digits, `_ - . / \ ~ :`, and anything else ends the name — so
  `see src/main.rs.` gives `src/main.rs` and not the full stop.
- **A name that is not there is refused:** `E484`, as vim says. `:e` makes a new
  file out of a name a person typed; `gf` is a jump, and a jump to a file that is not
  there is a mistake worth naming. (The test caught this: `open_file` happily made
  the file, because that is what `:e` wants.)
- **What the tests caught:** that `gf` is a two-key sequence spelled `g f` in the
  README's table — the table test said so, in one line, which is what it is for.
- **Files:** `inkstone-core` (`goto_file`, `file_name_under_cursor`, one keymap row).
- **Left for later:** `'path'` (vim searches it), a count (`3gf` is the third name on
  the line), `<C-w>f` (open it in a split), and `gF` (a name followed by a line
  number).

## Done — typing on a rectangle

- **What shipped:** `I` and `A` on a `<C-v>` rectangle: the same typing at the
  block's left edge, and one past its right one, on every row of it. Each row keeps
  its own position — the rows are different lengths and what is typed on one is not
  where the others are — a row too short for the column is padded, and the whole
  typing is one `u`.
- **How it works:** a blockwise insert is a list of the rows' cursors and which of
  them is being typed on, and every keystroke that insert mode handles is *replayed*
  on the others through the same funnel. A key that leaves insert mode leaves it for
  all of them. The state is deliberately *not* per document: it lasts as long as the
  insert session, and a session cannot outlive a switch to another document.
- **The history had to change, and this is the interesting part:** the fold that
  turns a group into one step required the steps to be made left to right, because
  it walked them in the order they were made. A block insert types along the rows
  and comes back to the first one for the next key, so its steps are not in that
  order — and what the fold actually needs is that they do not *overlap*. The steps
  are now sorted by where they are, with overlapping ones still left alone. Two
  tests pin both halves of that.
- **The merge, and the answer that was there all along:** folding a group of
  overlapping steps needs the *original* text of the span, and the steps do not
  carry it — which is what I wrote down last time as the blocker. It is not a
  blocker: the steps know enough to *replay* it. Undo already does exactly this —
  each step is undone with the coordinates it was made with, which is why the run
  works — so the group is taken back to what the buffer was before it, the span is
  read there, and the steps are applied again. The span is deliberately a *superset*
  of what the group touched: over-covering costs nothing (text outside the group's
  reach is the same before and after, so a replacement that puts it back unchanged is
  still the same change), and under-covering would lose text. The replay is checked
  against the buffer's length afterwards and refused if it did not put it back.
- **The group that cannot fold:** a group's steps were folded into one change when
  they could be said as one replacement, and left as separate steps when they could
  not — which meant a `u` took back only the last of them, and "one group is one
  `u`" was not true. Now the steps carry their group's id, and undo takes the whole
  run back in the reverse of the order they were made (redo, forward). That is
  correct by construction: each step is undone with the coordinates it was made
  with. The tests that pinned the old behaviour were updated, because the old
  behaviour was the *wrong* half of the rule — an insert session with a snippet
  accepted inside it is one visit to insert mode, and a visit is one step.
- **`c` on a rectangle, at the fourth attempt:** it now takes the block out and
  types on every row of it, as one `u`. The three failures before it were all the
  same mistake in different clothes — reasoning about where the steps would land
  instead of looking. What the print showed, in one line, was the thing no amount of
  reasoning had produced: **the insert session's group opens *after* the command
  returns**, when `run_command` notices the mode changed. So the deletion, made
  *inside* the command, landed in the command's group and the typing in the
  session's, and no merge could join them. The fix is three lines — the `c` command
  opens a group itself, so the session's nests inside it — and the print paid for
  itself several times over.
- **What I did *not* ship, and why:** `c` on a rectangle. It deletes the block and
  then types where it was — and the typing lands *on* the deletion's own start, so
  the two overlap and cannot fold: the deletion and the typing come apart under `u`.
  Vim does this as one step, and it took four attempts — the story is above, and the
  short version is that the third attempt printed the group's steps and the fourth
  read them. `c` now does what vim does.
- **What the tests caught:** five of my own expectations — including one where a
  put's block was one column wide because I had left out the `ll`, and one where the
  offsets of a hand-built group were computed against the buffer as it was *before*
  the earlier steps.
- **Left for later:** `c` on a rectangle (the merge), `$` as a block edge, and
  `<C-v>` in the middle of an operator (`d<C-v>`).

## Done — `<C-v>`, a rectangle

- **What shipped:** `<C-v>` selects a block — the rows between the two ends and
  the columns between them — with `d`, `y` and `c` acting on it, drawn as a
  rectangle, and the status line saying `VISUAL BLOCK`.
- **The design problem, and the shape of the answer:** everything else in this
  editor says a selection is *one character range*. A rectangle is not: it is one
  range per row. Rather than teach every span-based path about blocks, the kernel
  gained `visual_block` (the rectangle) and `block_spans` (the ranges, when they are
  wanted), and `View.selection` stays `None` while a block is up — the renderer asks
  about the block first, because it already worked a row at a time.
- **The history, which is where the real lesson was:** taking the rows out from the
  bottom up is the obvious way (later offsets stay valid), and it produced a block
  that took *three* `u`s to undo. `History` folds a group's steps when each starts
  after the last — that is how a whole insert session becomes one step — and a block
  taken backwards starts *before*. So the rows are taken from the first down, with
  each row's offsets worked out as the deletion reaches it. One command, one step,
  which is what a person expects of a block delete and what the test now pins.
- **A trap worth writing down:** `:normal` puts the cursor at the start of its
  range's line *before* running its keys. A sequence split across two `:normal`
  calls is therefore not the sequence a person types — the block's other end moves
  between them — which cost four wrong readings before the trace showed the
  rectangle was right and the *cursor* had been reset.
- **What the tests caught:** four of my own fixture errors — rows that did not line
  up (so "the same columns" was not what I thought), a rectangle whose width I had
  miscounted, a `<C-v>` pressed twice (which *ends* the selection), and a `c` case
  where I forgot that the second row is in the block too.
- **Files:** `inkstone-core` (`VisualKind::Blockwise`, `visual_block`,
  `block_spans`, `cut_block`, `copy_block`, `selection_columns`).
- **What came next:** the blockwise *put*, which is what the shape in the register
  was for. `Kind::Blockwise` is a third kind beside charwise and linewise, and `put`
  gained a third branch: the register's rows go on consecutive buffer rows, at the
  cursor's column (after it for `p`, at it for `P`), padded with spaces when a row
  is too short — the one place in this editor that invents text, since a column is a
  place and a place past the end of a line is a space. A count repeats the rectangle
  *sideways*, which is what vim does with one.
- **The lesson the tests taught:** four wrong expectations in a row, all of the same
  kind — I kept computing what the *block* would look like while forgetting which
  row of it was going where, or that a row cannot be reached by putting on it when
  the block's rows are consecutive. The fix that ended it was structural: each case
  now starts from its own editor, since a put that has already happened is a
  different document.
- **Left for later:** `I`/`A` (typing at the block's edges on every row), `$` as a
  block edge, `<C-v>` in the middle of an operator (`d<C-v>`), and a block put over
  a *block* selection (which would replace the rectangle rather than insert into it).

## Done — the scrolling keys

- **What shipped:** `<C-e>`/`<C-y>` (a line at a time), `<C-d>`/`<C-u>` (half a
  screen), `<C-f>`/`<C-b>` (a whole one).
- **The rule I got wrong first, and the test that caught it:** I wrote
  `<C-e>`/`<C-y>` as "the view moves and the cursor stays until the view reaches it",
  which is how a *scrollbar* behaves and not how vim does. Vim moves the text and
  the cursor together by the same amount — that is what keeps the cursor on the same
  screen row — and the end of the file is where the two come apart: the view cannot
  move, so neither does the cursor. The second design is also the simpler one: one
  rule instead of two branches, and the "how far did it actually go" answer is read
  back from the viewport rather than guessed.
- **Why they are not motions:** vim does not let `d<C-e>` mean anything, and this
  does not either — the scroll clears the half-typed operator, which is the
  `clear_pending` rule this project has met three times now.
- **What the tests caught:** three of my own arithmetic slips — `<C-u>` is half a
  page and not a whole one, a count on `<C-y>` scrolls that many lines, and `25`
  written as `\u{25}` is `%` rather than `<C-y>`.
- **Files:** `inkstone-core` (`Editor::scroll_view_by`, `scroll_line_down`/`_up`,
  `half_page_down`/`_up`, six keymap rows).
- **Left for later:** `'sidescrolloff'` and horizontal scrolling of the same kind,
  and `zz`/`zt`/`zb` keeping the *cursor's* row when `scrolloff` is set.

## Done — the screen in the motion vocabulary

- **What shipped:** `H`/`M`/`L` (with counts, linewise) and `scrolloff`.
- **Why these two together:** they are the same idea from both sides — what the
  *screen* means to the cursor. `H`/`M`/`L` move the cursor to a screen row;
  `scrolloff` moves the screen around the cursor. Both need the viewport, which the
  kernel already had (it is what `zt`/`zz`/`zb` and `page_up`/`page_down` use), so
  neither needed a new concept — only the one place that decides which lines are on
  screen (`ensure_cursor_visible`).
- **The design decision in `scrolloff`:** it is applied *before* the clamps that
  keep the view inside the file, which is what makes the ends of the file win. The
  margin is also capped at half the window, as vim does, or a tall setting would
  leave no room for the cursor to move in at all.
- **The setting type had to change:** `textwidth`'s type was `Columns`, whose doc
  already said "a number of columns where zero means something rather than nothing" —
  and `scrolloff` is the same shape in *lines*. It is now `Count`, with the unit in
  the setting's name rather than the type's, since what the type knows is that zero
  is a value.
- **What the tests caught:** two of my own expectations — the bottom row of the
  screen is `top_line + rows - 1` and not `top_line + rows`, and a `dH` case where I
  wrote out the whole file by hand and got it wrong.
- **Files:** `inkstone-core` (`goto_screen_line`, `Motion` entries, `Count`,
  `ScrollOff`, `ensure_cursor_visible`).
- **Left for later:** `'sidescrolloff'` (the horizontal half), `H`/`M`/`L` in
  visual mode as text objects, and `Ctrl-E`/`Ctrl-Y`, which scroll the view without
  moving the cursor — the one place a screen motion and a cursor motion come apart.

## Done — `:set` the way vim spells it

- **What shipped:** the three spellings anybody types — `:set number` turns a
  switch **on**, `:set nonumber` turns it off, `:set number?` asks what it is. A
  bare name that is not a switch (`:set textwidth`) still asks, there being no "on"
  for a number.
- **How it was found:** by a smoke, not by reading the code. The smoke of
  `formatonsave` did nothing, and the reason was two layers down: `:set lsp` (what I
  typed) only *reports*, so the server was never started, so there was nothing to
  format. An editor whose options cannot be set the way a vim user sets them is an
  editor whose features look broken from the outside — and this is the second time
  this project has learned that lesson from the *outside* (the first was the
  `E776`-shaped one about list windows).
- **Why the old rule existed:** `:set {name}` was written as a *query* from the
  start, with `no{name}` for off — a rule that was documented and tested, and which
  quietly made the commonest spelling in the world a no-op. The test that pinned it
  was updated rather than deleted: what it pinned was a choice, and the choice was
  wrong.
- **Evidence:** the kernel test walks all four spellings and the message each one
  leaves (`number=true`, `number=false`, `textwidth=20`, `E518` for `nope?`), and
  the smoke that found it now ends with the formatted file on disk.
- **Left for later:** `:set all` (every option in one message), `:set inv{name}`
  (the toggle), and `:setlocal`/`:setglobal`, which need a scope for options that
  does not exist yet.

## Done — `formatOnSave`

- **What shipped:** `formatonsave` (off by default): `:w` writes the file, asks for
  `textDocument/formatting`, applies the answer and writes the result again.
- **The decision, and why it went that way:** "format on save" can mean two orders —
  format *then* write, or write *then* format *then* write again. The first is one
  write and the file never holds unformatted text, but it needs the kernel to *defer*
  a save on an answer that may never come, which in this architecture means a
  deferred write path that has to survive a server that dies mid-save. The second is
  two writes and, for the moment between them, a file with unformatted text in it —
  and no way for a person to lose a save. It went the second way: a save that can
  fail because a language server is slow is not a save.
- **What it had to touch:** the ask carries `save: bool` (the daemon is the one that
  can write, since it owns the server); `Ask::Format` gained the field, and the two
  `:format` commands pass `false`. The write itself moved into one method,
  `Editor::write_buffer`, because the daemon writing the formatted file and the
  kernel writing `:w` must leave the saved marker in the same place — `:w!` still
  goes through `Buffer::save_with` for the "the file changed on disk" override.
- **Evidence:** the kernel test checks the *order* (the file is written before the ask
  exists), that the ask carries `save: true` and no range, and that `:w {file}` asks
  nothing; the daemon test runs the real fake server over a real pipe and checks the
  file on disk, the dirty flag and the message.
- **Left for later:** `insertSpaces` and `tabSize` from settings (the request says
  `insertSpaces: false`, which is what the editor types), `formatonsave` for a
  *range*, and asking on `:wq` as well — which it already does, since `:wq` runs
  `:w`.

## Done — `gq` on a list

- **What shipped:** a wrapped list keeps its marker on one line and lines the rest
  up under the text.
- **Constraints it had to respect, and how:** the marker is sometimes the shared
  leader (two bullets) and sometimes only the first line's own start (a numbered
  list, whose `1. ` and `2. ` share nothing, or a range of one line) — so the width
  is looked for in both places, which is the whole of the change. A quotation is
  deliberately not a list: every line of one is quoted, so its `>` is repeated.
- **What the tests caught:** three of my own expectations about width — that a
  quoted pair of lines would come back as one, that a numbered list's lines share a
  leader, and that indentation does not count towards `textwidth` when it plainly
  does not — each fixed by reading the number rather than the shape.
- **Files:** `inkstone-core` (`list_marker_width`, the continuation in
  `format_lines`).
- **Left for later:** nested lists (the indent of a sub-item), `autoformat` while
  typing, and a list's marker changing width as its numbers grow.

## Done — `:colder` and `:cnewer`

- **What shipped:** a history of the last ten lists, and two commands to walk it.
- **Constraints it had to respect, and how:** the history holds the *lists* rather
  than the windows — what a person wants back is the places a question named, and
  re-opening one is how they see it again — which also keeps the window model out of
  it entirely. Two details follow from that: showing a list closes the one that was
  up, so walking back and forth does not pile windows up, and the walk restores the
  *places* as well as the lines, so `:cnext` walks what is on screen rather than
  what was there before.
- **What the tests caught:** nothing, because the shape was a walk — an index, a
  step that refuses at the ends, a message that says where it is — which this project
  has now built four times (jumps, changes, places, lists).
- **Files:** `inkstone-core` (`ListSnapshot`, `list_history`, `remember_list`,
  `walk_list_history`, two commands and their names).
- **Left for later:** keeping several lists *open* at once.
- **A leak, and what it cost to see:** a closed list window used to leave its
  document in `documents` for the rest of the session — a scratch document per list
  opened, visible in `:ls`. The fix *was* four lines: ask whether the window's
  document is a list, and close the document too when the window goes. Two things
  made it look like more than four lines, and both are worth writing down because
  the next person will meet them:
  1. `close_document` on the *current* document drops the live fields, and the tail
     of that function already loads the remaining document again — so adding a
     `load_document()` of my own "to be safe" was the *harm*: it turned one test
     green and two red, and the red ones were not about lists at all.
  2. The test that failed then was mine: it set `ed.buffer` directly, which writes
     the *live* buffer and leaves the document slot empty, so the name came back
     `[No Name]` and the loop looked like it was deleting the file. Opening a real
     file (`ed.open_file`) made the same loop pass ten times over.
  The lesson is the line this project keeps meeting, seen from the other side: the
  live/slot split is not only a trap for the code being written, it is a trap for
  the *test* being written — a test that pokes the live field is testing a document
  nobody can open.

## Done — a file that changed underneath

- **What shipped:** `:w` refuses when the file on disk is not the one this buffer
  read, and `:w!` overrides.
- **Constraints it had to respect, and how:** this was on the list of *known gaps
  that are not features* — the honest kind of list — and it turned out to be a
  feature after all, with one real decision in it: what "changed" means. Length and
  modification time is what a filesystem gives cheaply and what every editor uses;
  the alternative (a hash of the file) costs a read of the whole file on every save
  to catch a case this does not. A path with nothing on disk has *nothing* to
  compare with, which is why a new file saves as it always did — the check is
  "there is something there and it is not what I read", not "there is something
  there".
- **What the tests caught:** that two existing tests were themselves "another
  process" — they wrote a file with `std::fs::write` and then saved a buffer that
  had never read it, which is exactly what the new check refuses. One of them now
  asserts the refusal *and* then overrides, which is a better test than it was.
- **Files:** `inkstone-core` (`Buffer::stamp`, `save_with`, `BufferError::Changed`,
  `save_force`, the `w!` name).
- **Left for later:** watching the file while it is open (a warning before the save
  rather than at it), and `autoread`-style reloading.

## Done — `:{range}format`

- **What shipped:** the ranged half of formatting.
- **Constraints it had to respect, and how:** the kernel's command table passes
  parameters rather than closures, so the range arrives as two integers and the
  command turns them back into lines — the same shape every other `:` command with
  a range has. And the *ask* grew two fields rather than a variant: a question that
  carries a place can carry a range as well, and the daemon decides from those
  whether the method is `formatting` or `rangeFormatting`.
- **What the tests caught:** nothing, which is what the third use of one shape
  should cost — the range, the conversion and the application were all there.
- **Files:** `inkstone-core` (`Ask::Format`'s two fields, `format_range_command`,
  the `:` arm), the daemon (the method choice), the fixture.
- **Left for later:** `formatOnSave`, and the options a person would want to pass
  (`insertSpaces`, `trimTrailingWhitespace`).

## Done — `:format`

- **What shipped:** `textDocument/formatting`, applied to the document.
- **Constraints it had to respect, and how:** an answer is a list of edits in the
  *original* text's positions, so something has to reconcile them with a text that
  is moving under them — and the obvious answer (apply them backwards) is wrong for
  a reason that has nothing to do with the arithmetic: the history records steps in
  the order they were applied, and a group that runs backwards cannot be folded
  into one entry, so `u` would take back the last edit instead of the formatting.
  The fix was to keep the order and carry a running difference. The other half is
  the conversion every position from a server needs, which this reuses.
- **What the tests caught:** the ordering above — as a `u` that undid one edit of
  two — and two of my own expectations about offsets (where a line break sits, and
  that an empty document is one empty line rather than no text at all).
- **Files:** `inkstone-core` (`Ask::Format`, `apply_edits`, the `:format` name), the
  daemon (`edits_from`, the answer arm), the fixture.
- **Left for later:** `textDocument/rangeFormatting` (the `=` operator's answer),
  `formatOnSave`, and passing real options (`insertSpaces`, `trimTrailingWhitespace`)
  from the settings.

## Done — `>`, `<` and `=` over a selection

- **What shipped:** the three operators over a visual selection.
- **Constraints it had to respect, and how:** this was the "same operator with the
  selection as its range" the last increment predicted, and it was — with one thing
  that is easy to forget: leaving visual mode has to go through `clear_pending`
  *before* the mode changes, or `gv` has nothing to come back to. That is the same
  ordering this project has now got wrong three times, which is why it is written
  into `clear_pending`'s own documentation.
- **What the tests caught:** nothing, and one assertion was written *because* of the
  history above: `vj>` then `gv<` has to put the two lines back, which tests the two
  features against each other rather than one at a time.
- **Files:** `inkstone-core` (three visual commands, three keys, the README table).
- **Left for later:** the `'<` and `'>` marks that vim sets at the ends of a
  selection, and `=` over a selection with an `equalprg`.

## Done — `>` and `<`

- **What shipped:** the shift operators, with the notation a keymap needs for a
  literal angle bracket.
- **Constraints it had to respect, and how:** a keymap cannot spell a bare `>` —
  the notation uses angle brackets for names, so the key is `<gt>` and the
  *doubled* key has to be compared against the same spelling `encode_key`
  produces, which is how the operator's `key()` came to be `<gt>` rather than `>`.
  The other half was a decision to *stop generalizing*: folding the `}` rule into
  the new range helper changed what a `}` does, and a shipped behaviour is not worth
  breaking to save one function — so the rule kept its own, tested implementation
  and the helper serves the operators.
- **What the tests caught:** both of those, and one wrong expectation of mine about
  a count meaning levels rather than lines (it means lines, as in vim).
- **Files:** `inkstone-core` (`Operator::Indent`/`Outdent`, `shift_indent`, four
  commands, two keys, the README table).
- **Left for later:** `=` with an `equalprg`, indenting a *selection* in visual
  mode (which is the same operator with the selection as its range), and
  `shiftwidth` as a setting of its own rather than a tab.

## Done — `=`

- **What shipped:** the reindent operator: `gg=G`, `==`, `=ap`, with a count.
- **Constraints it had to respect, and how:** the depth is a property of the
  *document*, not of the range — a block being reindented in the middle of a file
  is inside everything above it, so the depth is counted from the top and the range
  only says which lines are rewritten. The closing line is the other half: it is
  written one level *out* from the line before it, which is what makes `}` line up
  with its `{` rather than with the body. And the honest limitation is written down
  rather than hidden: brackets are counted as they are written, so a brace inside a
  string counts — asking the highlighter would be a second opinion about what is
  code, and a reindent that is wrong once is one keystroke to fix.
- **What the tests caught:** nothing, which is the reward of a shape that had
  already been built three times — an operator over lines, rewritten as one edit.
- **Files:** `inkstone-core` (`Operator::Reindent`, `reindent_lines`,
  `depth_before`, `line_delta`, two commands, the `=` key, the README table).
- **Left for later:** reindenting by a language's own style (the `=` of an LSP
  `formatting` request), `cindent`, and indenting a *selection* with `<`/`>`.

## Done — `smartindent`

- **What shipped:** a level after an opener, and a level back when a `}` is typed
  where a line has nothing but indentation.
- **Constraints it had to respect, and how:** `smartindent` is `autoindent` *plus*
  a rule, not a third mode — so the indent a new line gets became one function that
  both settings call, and the setting that is on decides what it returns. The level
  is a tab rather than `tabstop` spaces because a tab is what the Tab key types:
  indentation that does not match what the keyboard does is two conventions in one
  file. The outdent is the same idea backwards, and it has to count in *tabs* when
  the indent is tabs and in a tab stop's worth when it is spaces.
- **What the tests caught:** an expectation of my own again — that the cursor was
  on the line whose `{` mattered, when it was on the line below — which is the
  third time this increment and the one before it have been settled by printing the
  state rather than by re-reading the code.
- **Files:** `inkstone-core` (`SmartIndent`, `indent_for_new_line`,
  `line_opens_something`, `outdent_current_line`, the `}` rule in `type_text`).
- **Left for later:** `cindent`'s idea of a continuation line, indentation that
  follows a language's own style, and `=` reindenting a range.

## Done — `autoindent`

- **What shipped:** a setting that makes `o`, `O` and a typed line break start the
  new line where the last one did.
- **Constraints it had to respect, and how:** the indentation is a *prefix* of the
  new line and the cursor belongs *after* it, which is two decisions in one
  insertion — and both of them are the kind a test will get wrong before the code
  does. The other half is the line break: the break and its indentation have to be
  one edit, or a person who types a line break and then changes their mind needs
  two undos for one keystroke.
- **What the tests caught:** three of my own wrong expectations in a row — that
  `O` opens a line *below* (it opens above), that it copies the indentation of the
  line above rather than the one the cursor is on, and that a three-character
  selection deletes three characters' worth of what I thought it did. Each was
  found by printing the state rather than by reading the code, which is the
  cheapest way to settle an argument with yourself.
- **Files:** `inkstone-core` (`AutoIndent`, `indent_of`, `open_line_indent`, the
  insert-mode break, two commands).
- **Left for later:** `smartindent` (an extra level after `{` or `:`), `cindent`,
  and indentation that respects tabs over spaces.

## Done — `gv` and `gi`

- **What shipped:** `gv` (the last visual selection) and `gi` (where insert mode
  was left).
- **Constraints it had to respect, and how:** both are "remember where a mode
  stopped", and the honest place for that is the one function that runs when a mode
  *does* stop — `clear_pending` — which meant fixing the order in `escape` and
  `escape_visual`: they set the mode first, so `clear_pending` could not see what it
  was leaving. That is a two-line change with a real lesson in it: a function that
  reads the mode has to be called before the mode changes, and the tests that
  existed already pinned the *behavior* (that Escape leaves no selection), which is
  how the mistake was caught at once.
- **What the tests caught:** the ordering above, and one wrong expectation of my
  own about what a three-character selection deletes — which is what a test is for.
- **Files:** `inkstone-core` (`last_visual`, `insert_again_at`, `reselect`,
  `insert_again`, two commands, two keys, the README table).
- **Left for later:** `gv` across documents (the memory is per document, which is
  right, but a person may expect otherwise), `gi` keeping the *column* rather than
  the place, and `g;`-style lists of selections.

## Done — narrowing a list

- **What shipped:** `:copen {pattern}` and `:lopen {pattern}`.
- **Constraints it had to respect, and how:** the rows and their targets had to be
  filtered *together* — a list whose lines and whose places disagree is worse than
  no filter at all — so the two are built in one pass rather than filtered and then
  re-aligned. And a filter that matches nothing must not open an empty window:
  an empty list looks like a bug, so it says `Pattern not found` and leaves the
  screen alone, which is the same answer a search gives.
- **What the tests caught:** nothing, because the shape was already there — the
  rows were built in one loop and the targets alongside them, which is what made
  the filter a condition rather than a pass of its own.
- **Files:** `inkstone-core` (`open_places_list_where`, `open_diagnostics_list_where`,
  `filter_rows`, two commands).
- **Left for later:** keeping several lists (so a narrowed one is not the only
  one), sorting the window, and a filter that can be *changed* without reopening.

## Done — the count in the status line

- **What shipped:** `E1 W2 H1` on the status line of the file that is up.
- **Constraints it had to respect, and how:** the status line is built in *four*
  places — the live pane, the panes that are not the live one, the editor's own
  `status()`, and the tests — so the diagnostics went into the function rather than
  into one caller, and every window's status line counts *its* document's
  diagnostics rather than the screen's. That is the difference between a window on
  a file and the file: two windows on one file show the same count, and a window on
  another file shows its own.
- **What the tests caught:** nothing, which is what a one-argument extension of an
  existing formatter ought to be.
- **Files:** `inkstone-core` (`render::status_line`, `diagnostics_count`, its four
  call sites).
- **Left for later:** the count of the *whole* list (which `:lopen`'s message
  already has), and a sign in the gutter for the window's top line.

## Done — diagnostics across open files

- **What shipped:** `:lopen` gathers what every *open* document's server has said,
  names the file on each line, and goes there with `<CR>`.
- **Constraints it had to respect, and how:** the kernel keeps diagnostics per
  *document* — which is right, since a server says things about a file — so a list
  that spans them has to walk the documents and take the live one's out of the live
  field, which is the same live-field-versus-slot line that bit this feature twice
  before. What is deliberately *not* here is `workspace/diagnostic`: that is a
  protocol of its own, with progress reports and a server that may not support it,
  and what a person wants first is the files they already have open.
- **What the tests caught:** that a test asserting *which* diagnostic the walk
  lands on becomes order-dependent the moment the list spans documents — the
  assertion was rewritten to say what it means (that closing the window moves
  nothing) rather than to pin an order nobody promised.
- **Files:** `inkstone-core` (`open_diagnostics_list`, the message).
- **Left for later:** `workspace/diagnostic`, a count in the status line, and
  filtering the window by severity.

## Done — a query you can name, and a script that says what it holds

- **What shipped:** `:wsymbols {query}`, and `language=`/`lang=`/`type=` on a
  `<script>`.
- **Constraints it had to respect, and how:** both were the "nearly free" items
  the two increments before them promised, and both were — with one structural
  consequence worth naming: an ask that carries a *query* is no longer `Copy`, so
  the three places that moved an `Ask` out of a shared reference now clone it, and
  `position` takes `&self`. That is the honest cost of a richer question, and it
  was three lines.
- **What the tests caught:** nothing, which is the point of doing the nearly-free
  things: they were free because the shapes were already right.
- **Files:** `inkstone-core` (`Ask::WorkspaceSymbols::query`, `script_inner`, the
  shared language table), the daemon (the named-query branch).
- **Left for later:** a prompt for the query rather than a command argument,
  fuzzy matching, and `type` on a `<style>` (which is CSS either way).

## Done — a fence that says what it holds

- **What shipped:** ` ```rust `, ` ```py `, ` ```sh `, ` ```json `, ` ```toml `
  and the rest of the names this editor has grammars for, with their bodies
  coloured as that language.
- **Constraints it had to respect, and how:** this was predicted as "almost free"
  once `<script>` worked, and it was — with one correction. The first version had
  `Inner` name the *two* languages HTML can hold, so what it wanted to be was a
  *grammar selector*, and that is what it became: one enum, one `grammar()` method,
  and the two callers share the scan. What was *not* free is a rule: Markdown's
  closing fence has to be at least as long as the opening one, and this counts the
  character and not the count — the cheaper of the two rules, documented rather
  than guessed at.
- **What the tests caught:** that a fence has to be *closed* before another opens
  (inside one, a line that looks like a fence is body text — which is what a
  person writing a document about fences needs), and that a body's own emphasis is
  not emphasis when the fence said Rust.
- **Files:** `inkstone-core` (`Inner`'s five new arms, `grammar`, `inner_for`, the
  fence branches of `markdown`).
- **Left for later:** the fence's length, `{rust}`-style info strings, and titles
  after the language (` ```rust title="x" ` is read as Rust, which is right, but
  the rest of the info string is ignored).

## Done — a language inside a language

- **What shipped:** `<script>` and `<style>` bodies coloured as JavaScript and CSS.
- **Constraints it had to respect, and how:** this was the one design question
  left on the highlighting line, and the answer was *not* to give `State` an inner
  `State` — because a state is `Copy`, which every line's cache depends on, and a
  state inside a state is not `Copy`. So the outer state carries the three things
  the inner language actually needs (which language, and what it has open), and the
  inner scan goes through the *same* scanner as everywhere else, with the state
  mapped in and out at the boundary. The recursion is the other half: the opening
  tag's `>` hands the rest of the line back to the same walk, so the "what is
  inside a script" logic exists once rather than twice.
- **What the tests caught:** the closing tag *must* win — a `//` comment in a
  `<script>` would otherwise swallow the `</script>` that ends it, and the whole
  rest of the file would be a comment. That is exactly the rule real tokenizers
  have, and the reason `"<\/script>"` is a thing people write.
- **Files:** `inkstone-core` (`Inner`, three state fields, `inner_spans`, `CSS`,
  the `<script>`/`<style>` hooks in `html`).
- **Left for later:** `language="…"` on a `<script>` (CoffeeScript, JSON, and the
  rest), `<pre>` and CDATA, and the same trick for Markdown's fenced blocks.

## Done — `:wsymbols`

- **What shipped:** `workspace/symbol`, with the word under the cursor as the
  query, its answer filling the place list and opening the window.
- **Constraints it had to respect, and how:** it is the first question that is not
  about a place *and* is not about one file — and the answer is a list of places in
  files that are not open, which is exactly what the list window was built for, so
  the last three increments paid for this one: the request path, the list, the
  window and `<CR>` were all in place, and what was left was a query (the word at
  the cursor) and a decision (a project-wide answer is *read*, so the window
  opens, where a single reference is *gone to*).
- **What the tests caught:** two things that would have been silent in use. A
  `SymbolInformation` keeps its position inside a `location`, which the place
  reader did not look in — so the list came back empty and the window said nothing.
  And asking a question while a list window is up has no file to ask about: the
  ask was dropped and the message row kept saying "asking the language server..."
  forever. It now says `no file to ask about`, which is a thing a person can act
  on.
- **Files:** `inkstone-core` (`Ask::WorkspaceSymbols`, the `:wsymbols` name), the
  daemon (`word_at`, the query, the answer arm, `locations_from`'s `location`
  case), the fixture.
- **Left for later:** a prompt for the query rather than the word under the cursor,
  fuzzy matching, and `workspace/symbol` results *kept* rather than replacing the
  list.

## Done — a diagnostics window too

- **What shipped:** `:lopen` and `:lclose`, the same window the place list has.
- **Constraints it had to respect, and how:** the *point* was that this should cost
  almost nothing, and it nearly did — the window, the read-only refusal and the
  `<CR>` key were already there, and a diagnostic is a line of text with a place
  attached, which is what a list line is. Two things had to be fixed to make it
  true. The count of diagnostics has to be taken *before* the window is made,
  because diagnostics belong to the document on screen and the list is a document
  of its own — the swap that shows it takes them away. And a list has to carry its
  own *targets*: what each line goes to belongs to the list, not to the session,
  since two lists can be open at once and the second would otherwise walk the
  first one's places.
- **What the tests caught:** both of those, as the same symptom twice — a window
  that opened saying `0 diagnostics`, and a `<CR>` that went nowhere. They are the
  same trap in two places: the kernel keeps the document on screen in live fields
  and everything else in a slot, and a feature that adds a *document* has to
  respect that line.
- **Files:** `inkstone-core` (`List`, `list_targets`, `open_diagnostics_list`, two
  commands and their names).
- **Left for later:** several lists at once *kept* rather than open, sorting and
  filtering in the window, and `:lopen` on a list that is not the current file's.

## Done — the list window

- **What shipped:** `:copen` and `:cclose`, and `<CR>` to go to the place the
  cursor's line names.
- **Constraints it had to respect, and how:** the point of this one was to need
  *nothing* new. The list is a document — plain text in a buffer, shown in a
  window the split machinery already knows how to make — so no window kind, no
  renderer change and no protocol change were needed to draw it. What was new is
  one flag on a document (this is a list, not a file), one refusal in `replace_range`
  (`E21`, the read-only answer), and one key: `<CR>` means "go to the place on
  this line" in a list and "move down" anywhere else, which is vim's `<CR>` both
  ways.
- **What the tests caught:** one off-by-one in the test rather than the code (a
  list of two places has two lines), which is the sort of thing worth writing down
  because it is what the *next* person will also guess wrong.
- **Files:** `inkstone-core` (`Document::places_list`, `open_places_list`,
  `close_places_list`, `open_place_at_cursor`, three commands, the `<CR>` key).
- **Left for later:** several lists at once, `:colder`/`:cnewer`, sorting and
  filtering the window, and showing a list of *diagnostics* the same way.

## Done — `gq` and `textwidth`

- **What shipped:** `gq` as an operator over a motion, a text object or a count,
  `gqq` for whole lines, and the `textwidth` setting it fills to.
- **Constraints it had to respect, and how:** the operator machinery took it the
  way it took `gU` — a variant, three names, and the doubled key — but two things
  were new. A rewrap is about *lines*, and an operator's range is characters, so
  the range is turned back into line numbers before anything happens; and
  `textwidth` is the first setting whose zero means something, which is what the
  settings table's own doc comment had already said would happen — so it grew a
  `Columns` type rather than borrowing the counts, and `:set textwidth=0` is
  "do not wrap" rather than an error.
- **What the tests caught:** two things worth keeping. That a rewrap of a range
  whose break lands in the same place rewrites nothing — the test that asserted
  "nothing moved" was *right* and said nothing, so it became a test of the range
  that does move. And that the expected text of a wrap is worth computing rather
  than guessing, since a twenty-column wrap is not a forty-column one.
- **Files:** `inkstone-core` (`Operator::Format`, `format_lines`, `Columns`, the
  `gq` key, the motion table).
- **What came next, and why it had to:** the smoke for this found that nothing
  could set `textwidth` from the `:` line at all — the options layer was reachable
  from Lua and from nothing else — so `:set {name}={value}` came with it. It is
  the shortest possible seam: the settings table already knows every name and every
  kind of value, so `:set` is a name lookup, a value read the way a script reads
  one, and a message; `:set {name}` without a value asks instead of setting, which
  is what makes the two commands one.
- **What came next:** the three things the first version left out, all of which
  turned out to be the same idea from different sides. A *leader* is what the
  lines of a range have in common, so it is computed once and put on every line
  the rewrap produces — and it does not count towards the width, since a person
  reads past it. Breaking *while typing* is the same rule applied one keystroke at
  a time, and the subtle part is that the break must not move the cursor: it takes
  the place of a space already passed, so what comes next still belongs where it
  did, and a paste lands whole rather than half-wrapped. And `:set no{name}` is the
  smallest of the three: vim's spelling for a switch that is being turned off.
- **What the tests caught:** that breaking "at the last space before the cursor"
  depends on *when* the line is noticed — a keystroke at a time and a paste arrive
  with different cursors — so the rule became "before the width", which is the same
  answer either way; and that leaving the cursor at the start of the new line put
  the next typed space in front of the wrong word.
- **Left for later:** `gq` on a line that is part of a list or a quotation
  (the marker is kept, but nesting is not understood), and `autoindent`.

## Done — the rest of the file commands

- **What shipped:** `:w >> {file}`, and `:{range}read {file}` placing the lines
  after the range's last line.
- **Constraints it had to respect, and how:** appending and writing are the same
  walk over the same lines and differ in one call — `OpenOptions().append(true)`
  against `fs::write` — so the text of a range was pulled out into one function and
  the two commands are three lines each. The other half is the range's *meaning*
  for `:read`: `:2read other.txt` is not "read into line 2", it is "read after
  line 2", which is why the command moves the cursor before reading rather than
  teaching `read_file_below` about ranges.
- **What the tests caught:** nothing this time — both were shapes already in the
  file, which is what "the rest of" ought to mean.
- **Files:** `inkstone-core` (`append_range`, `range_text`, the two `:` arms).
- **Left for later:** `:r !command` and `:w !command` (reading and writing a
  process), and `:w!` as a distinct "write even so" — there is no read-only state
  to override here yet.

## Done — a walkable list of places

- **What shipped:** references and symbols now fill a *list* as well as saying
  themselves once, and `:clist`/`:cnext`/`:cprev` walk it — across files, with a
  count.
- **Constraints it had to respect, and how:** the list is of *places in files*,
  which is why it lives on the session and not on a document: walking off the end
  of one file into another is the whole point, and a per-document list could not
  do it. The rest is the shape the other walks already had — an index, a step that
  refuses at the ends, and a message that says where it is — with one difference:
  this walk *goes* to what it finds rather than moving a cursor inside what is
  already open, so it reuses `goto_location`, which is what `gd` uses. That is the
  first time a `:` command has moved between files, and it cost nothing because
  the kernel already knew how.
- **What the tests caught:** nothing — the walk is the third of its kind here, and
  by now the shape is known. The test that was worth writing is the one for what
  the *daemon* leaves behind: two places in the list and the walk on the first,
  which is the seam between an answer and a command.
- **Files:** `inkstone-core` (`set_places`, `walk_places`, `places_text`, three
  commands and their names), the daemon (`places_from`).
- **Left for later:** showing the list in a window rather than one line of message
  row, `:lnext`-style per-window lists, and keeping several lists.

## Done — signature help

- **What shipped:** `textDocument/signatureHelp`, asked with `<C-k>` in insert
  mode or `:signature`, answered in the message row with the current parameter
  named.
- **Constraints it had to respect, and how:** a parameter's label is written one of
  *two* ways in the protocol — as text, or as a pair of character offsets into the
  signature's own label — and the second is not a fallback but a first-class
  spelling, so both are read; offsets that do not fit the label it came with are
  ignored rather than sliced, since a cut-out of nonsense is worse than the whole
  signature. The rest is the shape every question here has: a position, a method
  name, and an answer put where it can be read.
- **What the tests caught:** nothing this time, because the answer's shape was
  already understood from the three questions before it — which is the point of
  having one path for all of them.
- **Files:** `inkstone-core` (`Ask::Signature`, the key and the name), the daemon
  (`signature_text`), the fixture.
- **Left for later:** showing the whole signature on several lines, `parameterInformation`
  documentation, and asking again as the cursor moves between arguments.

## Done — `:symbols`

- **What shipped:** `textDocument/documentSymbol`, listed in the message row and
  gone to.
- **Constraints it had to respect, and how:** it is the first question here that
  is not about a place — and the *kernel* still leaves it as one, because the
  question an `Ask` carries is a position and a document's symbols do not have
  one. That is deliberate: one shape for every leave-a-question, rather than a
  second shape for the odd one out. The answer has two shapes in the protocol and
  they read as one here (nested symbols carry their own ranges, flat ones hide
  theirs in a `location`), which is the same normalise-then-use pattern the
  reference answers needed.
- **What the tests caught:** a half-thought in the *reporting* — the second number
  after each symbol was the length of its name rather than anything to do with the
  file — so the line is written out in words now, which cannot be misread.
- **Files:** `inkstone-core` (`Ask::Symbols`, the `:symbols` name), the daemon
  (`symbols_text`, `symbols_from`), the fixture.
- **Left for later:** a symbol *tree* to walk, `signatureHelp`, and
  `workspace/symbol`.

## Done — the rest of Markdown's line shapes

- **What shipped:** list markers, setext underlines and entities, in Markdown and
  (for entities) in HTML.
- **Constraints it had to respect, and how:** everything here scans one line, and
  a setext heading is by definition *two* — a title and an underline. The honest
  reading is therefore of the underline alone: a line of `=` or `-` and nothing
  else means the same thing wherever it appears, so it can be coloured without
  knowing what came before it. The list marker had the opposite trap: `*` is both
  a bullet and emphasis, so the marker is only a marker with a space after it, and
  the emphasis scan starts after it rather than over it.
- **What the tests caught:** nothing this time — the shapes were small and the
  tests were written beside them — which is worth saying once in a while.
- **Files:** `inkstone-core` (`markdown`, `list_marker_end`, `entity_end`, the
  HTML entity case, the doc and the CHANGELOG).
- **Left for later:** a body coloured as its language (`<script>`, `<style>`),
  setext headings coloured *with* their title, CDATA, and tables.

## Done — HTML, and a lesson about untracked files

- **What shipped:** a tokenizer for HTML and XML — tag names, attribute values,
  declarations and comments — with the two states that cross a line.
- **What went wrong, and what it cost:** while adding it, a `python -c` edit over
  `highlight.rs` deleted everything between two anchors, and the file is
  *untracked* in git, so there was no copy to go back to. What made the recovery
  possible is that the tests were on the other side of the edit: twenty-two of
  them, pinning the token kinds, the keyword lists' behaviour, the raw-string and
  nested-comment rules, the state cache and the span invariants. They were the
  specification, so the scanner could be written again *to* them — and it was,
  with two failures left that the tests named exactly (`panic!` is a call; a line
  beginning inside a comment has no opener to step over).
- **The lesson, written down because it will repeat:** the tree is the deliverable
  and it is uncommitted, so a splice that is wrong is not recoverable by git. The
  tests-are-the-spec property is what saved this; the *rule* is to splice with the
  read-and-replace tools rather than a script that computes anchor indices.
- **Files:** `inkstone-core` (`Language::Html`, `html`, `scalar_end`, two state
  fields, `Grammar`/`scan` and the Markdown scanner rebuilt, the doc, the README).
- **Left for later:** a script or style body coloured as its language, entities,
  and CDATA.

## Done — Markdown

- **What shipped:** a tokenizer for Markdown: headings, quotations, links,
  emphasis, inline code and fenced blocks.
- **Constraints it had to respect, and how:** every other language here is a
  keyword list plus a handful of flags, and Markdown has neither keywords nor
  strings — so the honest move was a scanner of its own rather than bending the
  grammar struct into a shape it was not meant for. The other constraint was the
  *state*: all of Markdown is answerable within a line except a fenced block, and
  a fence is the same shape as a block comment — one field in the state every
  language already shares, which is what makes the line inside a fence code and
  the `*` in it an asterisk.
- **What the tests caught:** that a quotation returns early and so swallows its
  own content — a `> **thick**` should be a quotation *and* thick, which is what
  made the `>` the quotation mark and the rest of the line ordinary scanning; and
  that the fence character matters, since a `~~~` block is not closed by a line of
  backticks.
- **Files:** `inkstone-core` (`Language::Markdown`, the `fence` state field,
  `markdown`, `fence_at`, `emphasis_end`, the doc and the README).
- **Left for later:** setext headings (underlined with `=`), lists and their
  nesting, HTML inside a document, and tables.

## Done — `:read` and `:{range}write`

- **What shipped:** a file's lines brought in below the cursor's, and any range of
  the buffer written out to a file.
- **Constraints it had to respect, and how:** *when a file is not there*, the two
  half-commands want opposite things. `:e` treats a path that does not exist as a
  new file, because that is how a file gets created; `:read` must not, because
  reading nothing and saying nothing is worse than an error — so the existence
  check is in the read and the test that caught it is the one for a path that is
  not there. And a *partial* write must not mark the buffer saved: what is on disk
  is part of the buffer, and the rest of it is not, so `:1,5w` leaves the `[+]`
  where `:w` clears it.
- **What the tests caught:** the `:e` behaviour leaking into `:read`, which
  inserted an empty line where it should have said the file was missing.
- **Files:** `inkstone-core` (`read_file_below`, `write_range`, the two arms of
  the `:` line, the README).
- **Left for later:** `:r !command` and `:w !command` (reading and writing a
  process), `:r` with a range to put the lines after, and `:w >>` to append.

## Done — `:normal`

- **What shipped:** `:normal {keys}` over a range, with the keymap's notation for
  keys a `:` line cannot hold.
- **Constraints it had to respect, and how:** the keys are the rest of the line
  *verbatim* — a space in them is a key, not a separator — so they are read
  before the line is split into words rather than after. Two decisions came out of
  that: notation is honoured, because there is no other way to say Escape on a
  `:` line, and a name that is *not* one of the known keys is the characters it
  looks like, because a person writing `<` on purpose meant `<`. That second half
  is the whole of `parse_normal_keys`, and it is deliberately not `parse_key`,
  which fails on the first angle bracket that is not part of a name.
- **What the tests caught:** that a run which ends while still inserting has to be
  put back into normal mode — that is what makes `:%normal A;` append to *every*
  line rather than typing the rest of the keys into the first one — and that
  `:normal A <` is two characters rather than an error.
- **Files:** `inkstone-core` (`parse_normal_keys`, the `normal` arm of the `:`
  line, the README).
- **Left for later:** `:normal` with a count (`:3normal`), `:execute`, and keys
  given as a register (`:normal @a`).

## Done — `:sort`

- **What shipped:** `:sort` over the file or a range, with vim's flags — `!`,
  `n`, `i`, `u`, alone or together, with a space or without one.
- **Constraints it had to respect, and how:** the lines' terminators are not
  theirs but the *position's*: a file whose last line has no line break must not
  grow one when a different line lands there, so each line keeps the terminator
  of the row it is written into rather than the one it came from. The other half
  is the ordinary one for this editor: the whole range is rewritten as a single
  edit, so it is one thing to undo, and the range itself is the range the `:` line
  named *before* the command name was cut off it.
- **What the tests caught:** both of the above as separate bugs — `:sort!` has no
  space between the name and the flag, so `strip_prefix("sort")` on the command
  word was the only reading that handled both spellings; and reading the range
  *after* stripping it from the line found nothing, so `2,3sort` quietly sorted
  the whole file.
- **Files:** `inkstone-core` (`SortSpec`, `Editor::sort_lines`, the `sort` arm of
  the `:` line).
- **Left for later:** `:sort` by a pattern in the line (`:sort /re/`), sorting a
  selection, and `:sort` with a field range.

## Done — references you can go to, and choices in a snippet

- **What shipped:** `gr` now moves the cursor to the first place an answer names
  as well as listing them, and `${1|one,two|}` is read as a choice.
- **Constraints it had to respect, and how:** both are the same shape of
  constraint — a list with no list to put it in. References have one line of
  message row, so the places are *said* and the first is *gone to*, which is the
  half a person actually needs; the rest are where `<C-o>` comes back from. A
  choice has no key to pick with, so the first option is what a snippet types,
  which is what a server means by it anyway — the syntax is read rather than
  refused, and what is missing is a way to cycle.
- **What the tests caught:** that a choice's options are separated by commas
  *and* closed by a `|`, so reading only the first option means skipping the rest
  of them rather than typing them: `one,two,three` came out as text before the
  reader knew where the first option ended.
- **Files:** `inkstone-server` (`goto_first_reference`), `inkstone-core`
  (`completion::snippet`), the README.
- **What came next:** the choice key — `<C-n>` and `<C-p>` pick another option
  when a snippet is being filled in and no completion list is up. That split is
  what the whole thing is: one key, two meanings, and the state decides which —
  the list takes it when there is a list, and a snippet takes it when there is
  not. Picking *another* option means the text grows or shrinks, and every stop
  after it is a range, so they move by the difference; that is the one piece of
  arithmetic a choice needs and a plain stop does not.
- **What came next:** mirrors — a place written twice — and the shape of that
  turned out to be a *deduplication*: the arithmetic that keeps ranges true when
  text grows had already been written for a choice, so the second writing follows
  the first by going through the same path rather than by having rules of its own.
  What was new was only that the path is now the one *every* edit takes
  (`replace_range`), which is what makes it work for typing as well as for
  picking — and a flag, because the mirrors' own edits come back through it.
- **What came next:** the selection — a snippet's stop is *selected* when the
  walk arrives at it, so what is typed replaces the placeholder instead of going
  in beside it, and the splice is one edit rather than a delete and an insert, so
  one `u` undoes it. That needed the insert path to know about selections at all
  (`type_text` replaces one when there is one), which is the first thing in this
  editor that insert mode and visual mode share — and the bug that took longest
  to find was in it: the replacement's *range* was shadowed by the replacement's
  *new end*, so it ate one character past the placeholder and every range after
  it was wrong by that much.
- **Left for later:** a real list to walk references in.

## Done — snippets

- **What shipped:** an item with `insertTextFormat` 2 is typed as its text with
  the defaults filled in, and `<Tab>`/`<S-Tab>` walk the places it leaves to fill.
- **Constraints it had to respect, and how:** a snippet is a *little language*,
  and it belongs where it is read: the server passes the flag and the kernel reads
  the text, because what is typed is the buffer's business and the popup is the
  kernel's. Three details were worth getting right rather than approximating:
  stops are stepped through in number order (a person writes `a${2}b${1}c` meaning
  to fill in the `1` first), a default may hold stops of its own (so reading one is
  recursive, and a nested stop is recorded at its own place in the whole text), and
  past the last stop the session is *over* rather than looping, so `<Tab>` is a tab
  again exactly where a person would expect it.
- **What the tests caught:** that the backward step has to skip the stop it is
  standing on — `snippet_step` counts one past the current place, so going back is
  two steps, not one; and that the end-to-end popup can land on the snippet item
  rather than the plain one, which is a fact about a *list* rather than about a
  snippet.
- **Files:** `inkstone-core` (`completion::snippet` and its tests, the snippet
  session on the editor, the two insert-mode keys, the README table),
  `inkstone-server` (the flag), the fixture's snippet item.
- **Left for later:** mirrors (`${1:name}` used twice), `${1|one,two|}` choices,
  selecting a placeholder's text so typing replaces it, and variables (`$TM_FILE`).

## Done — `completionItem/resolve`

- **What shipped:** the second half of completion: the details of the item the
  list has moved to, asked about lazily, one item at a time.
- **Constraints it had to respect, and how:** the kernel's items are not what a
  server sent — they are a label, what to insert and a detail — so the raw answer
  is kept beside them and the *item* goes back as it arrived, `data` and all,
  because that field is the server's own and means nothing here. The item is
  identified by its label, which is what the two sides have in common; and what
  comes back goes on *both* copies of an item — the list of everything and the
  list of what is still matching — since a list that showed one and not the other
  would show the detail only after the next keystroke.
- **What the tests caught:** that a smoke test is not always the right instrument.
  The terminal could not show this one — its completion request was not answered
  in that session, a separate question — so the proof is an end-to-end test that
  drives the real binary over its pipes, opens the file the way a frontend opens
  one, types a word, asks for completions, and reads the *popup rows* off the
  screen for both the item and its resolved detail. It is slower than a smoke and
  it does not depend on a terminal happening to behave.
- **Files:** `inkstone-core` (`completion_resolve`, `set_completion_detail`),
  `inkstone-server` (`ask_resolve`, `raw_completions`, `resolving`, the answer
  arm), the fixture's `completionItem/resolve`.
- **Left for later:** resolving on the *first* item before the list moves,
  documentation longer than one line, and `insertTextFormat` (snippets).

## Done — a language server's answers, actually drawn

- **What shipped:** two fixes with one root: a server that runs when the file
  arrives, and a frame asked for when an answer changes the screen.
- **How it was found:** by doubting a smoke that should have passed. Hover had
  been verified in a terminal two increments earlier, so when `gr` and then even
  `K` showed nothing, the honest move was to stop adding features and find out
  why — and the trail ran through the daemon loop, a *second* smoke with an init
  file that changes something visible in the gutter (line numbers, to prove the
  init was loaded), and finally a test that drives the real binary over its pipes
  with a file opened the way a frontend opens one.
- **What the test caught that reading did not:** that the daemon only ticked when
  a server was already running, so the tick that would have *started* one never
  ran — a bootstrap deadlock that no in-process test could see, because those
  tests build the session and call `tick` by hand. The in-process test passed
  while the binary failed, and that difference was the whole clue.
- **The second bug, and the one a person would notice:** an answer arrives between
  keystrokes, and nothing was asking for a frame. The kernel did the work and the
  screen never showed it. `K` and `gd` were not broken — they were *lucky*
  whenever another key drew the next frame, which is why the earlier smoke passed
  and the later ones did not.
- **Files:** `inkstone-server` (`tick` and `tick_lsp`, `lsp_tried_for`, the loop's
  unconditional tick), and two tests: the init-file-to-server chain, and the
  end-to-end run against the real binary.
- **Left for later:** nothing here; this is the state the earlier increments
  claimed.

## Done — references

- **What shipped:** `gr`, which asks where a name is used and says so in the
  message row.
- **Constraints it had to respect, and how:** the answer is a list and the UI has
  one line, which is the same constraint `:diagnostics`, `:jumps` and `:changes`
  already answered — so references are reported the same way: a count, then the
  first few places named by file, and `... N more` past eight. The other half was
  *not* duplicating the parsing: a place in an answer can be a location, a list of
  them, or a `LocationLink` under other names, and a definition answer is just the
  first of that list — so the extraction became one function and `definition_from`
  became three lines of it.
- **What the tests caught:** nothing, and the reason is worth keeping: the shapes
  were already understood, because the definition path had met them first.
- **Files:** `inkstone-core` (`Ask::References`, the `gr` key), `inkstone-server`
  (`locations_from`, `references_text`, the method and answer arms), the fixture.
- **Left for later:** a list you can walk — a quickfix-style window, or references
  as marks — and `textDocument/references` with `includeDeclaration` set either
  way.

## Done — the change list

- **What shipped:** the places edits were made, `g;`/`g,` to walk them with a
  count, and `:changes`.
- **Constraints it had to respect, and how:** *one entry per line changed, not
  per keystroke*. An insert session types into one place, and a list that grew a
  member per character would be useless to walk — so recording a place that is
  already in the list moves it to the end instead of adding it again, which is
  also what makes a `cw` on a line that was already typed into a single entry.
  The other constraint was *where* to record: every edit in the kernel goes
  through `Editor::replace_range`, undo and redo included, and the undo path is
  the one that goes through the buffer directly — which is what makes that one
  call the whole hook, and what keeps undo out of the list without a check for it.
- **What the tests caught:** that truncating at the walk's index has to keep the
  entry it is standing on — dropping `index` entries instead of `index + 1` left
  a list of one, which every walk then walked in place.
- **Files:** `inkstone-core` (a `changes` module with its tests, the `Document`
  field and its swaps, one line in `replace_range`, three commands, two keys).
- **Left for later:** undo/redo entries if vim turns out to keep them, the text
  of a change as it was rather than as it is, and per-file change lists across a
  session.

## Done — `:delmarks` and `:clearjumps`

- **What shipped:** `:delmarks {letters}`, `:delmarks!` and `:clearjumps` — the
  housekeeping halves of the two features before them, so that a mark or a place
  in the list can be got rid of without closing the file.
- **Constraints it had to respect, and how:** the argument is a *little language*
  — letters together or apart, a hyphen for a run — and vim's reading of it is
  what makes `:delmarks a-c` and `:delmarks abc` the same thing. A letter that is
  not one of the twenty-six is named back rather than having a mark invented for
  it, which is the difference between a command that says `E474` and one that
  quietly does nothing.
- **What the tests caught:** nothing this time, which is worth saying: the work
  was small enough that its shape was already right — the two modules had `clear`
  and `clear_all` to call, and the `:` name map had the place to put them.
- **Files:** `inkstone-core` (`Marks::clear_all`, `Jumps::clear`, three commands,
  four names on the `:` line).
- **Left for later:** uppercase (global) marks, `:delmarks!`'s vim rule that it
  keeps marks of other buffers when there are any, and `:'a` in a range's *file*
  part.

## Done — vim's pattern dialect

- **What shipped:** a translation layer between the dialect patterns are typed in
  and the one they are compiled in, used by `:s` and `/` alike: groups,
  alternation, the four ways of writing a count, word boundaries, `\c`/`\C`, and
  `\v`/`\V`.
- **Constraints it had to respect, and how:** the two dialects overlap, and where
  they do, *nothing may change* — `.`, `*`, `[a-z]`, `^`, `$`, `\d` and an
  escaped literal all mean the same thing in both, so the translation is a list
  of the places they differ and not a rewrite. That is also why it is one
  function used by both callers rather than a flag on each. Where they overlap
  *misleadingly* — `\z`, which vim uses for the ends of a match and the engine
  for an anchor at the end of the text — the translation refuses instead of
  guessing, which is the one thing that made this worth doing carefully: a
  pattern that quietly matches something else is worse than one that will not
  compile.
- **What the tests caught:** that vim closes a count with a *plain* brace —
  `\{2,3}` — where I had written a translator looking for an escaped one, which
  swallowed the rest of the pattern; and that `\V` has to leave the switches
  reachable, since they are how a pattern gets out of it.
- **Files:** `inkstone-core` (a `dialect` module with its tests, `DialectError`,
  the two callers in `substitute` and `search`).
- **Left for later:** `\zs` and `\ze`, vim's identifier and file-name classes
  (`\i`, `\k`, `\f`), `\m`/`\M`, and `~` in a replacement.

## Done — marks in ranges, and the last jump

- **What shipped:** `:'a,'bd` and every other `:` command with a mark in its
  range, plus `''` and `` `` ``, which toggle between the last jump and here.
- **Constraints it had to respect, and how:** parsing a `:` line happens without
  an editor — `substitute.rs` sees text and nothing else — so an address could
  grow a `Mark(letter)` variant but not a *line*: the letter is carried through
  the parse and looked up where the range is resolved, which is the editor.
  Everything that resolves an address then went through one method, which is
  what made the second half of this cheap and turned up the bug below.
  The last jump is one place rather than a list, so `''` toggles instead of
  walking back through a history; the keys that set it are the ones that
  travel — mark jumps, searches, `%` — and not the motions within a line.
- **What the tests caught:** that a single address meant "from that line to the
  end" rather than that line — `:5s/a/b/` was substituting over the rest of the
  file, and `:.s` over everything below the cursor. Fixing it is what `:'as/t/T/`
  in the mark test checks, by leaving the `t` of the next line alone.
- **Files:** `inkstone-core` (`Address::Mark`, `resolve_address`,
  `resolve_range`, `unset_range_mark`, the range check in `execute_cmdline`,
  `record_jump`, the two jump-back commands, the mark motions).
- **Left for later:** a real jumplist (`<C-o>`/`<C-i>`), `:delmarks`, and marks
  that carry through edits.

## Done — marks

- **What shipped:** `m{a-z}`, `` `{a-z} ``, `'{a-z}`, `:marks`, and both jumps as
  motions (`d'a`, `` d`a ``).
- **Constraints it had to respect, and how:** two, and the second is the one that
  made this small. A mark is a place in a *file*, so it went into `Document` next
  to the text, the history and the diagnostics, and it moves through the same
  three swaps those do — which is what makes it leave the screen with its file
  and come back with it, with no extra code for either.
  And a key that waits for a letter is not a command: `m`, `` ` `` and `'` went
  in beside `f{char}`, whose shape was already there — a key that is intercepted
  before the keymap, a letter that is taken from the next key, and a *name* passed
  on to the motion so that an operator can take one. `d'a` came almost free from
  that, and only needed the letter kept where the motion could read it, which a
  find's character already had a field for.
- **What the tests caught:** that `'a` is linewise and `` `a `` is not — `d'a`
  with the cursor above the mark takes *both* lines and leaves nothing, which is
  what vim does and not what was expected.
- **Left for later:** marks in a `:` range (`:'a,'bd`), `''` and `` `` `` (the
  last jump), `:delmarks`, and carrying marks through edits, which is the
  bookkeeping this version does without.
- **Files:** `inkstone-core` (a `marks` module with its tests, the `Document`
  field and its swaps, `starts_mark`/`take_mark_target`, four commands, the
  motion table).

## Done — `:g` and `:v`

- **What shipped:** `:g/pattern/command` and `:v/pattern/command`, over a range,
  with `:d` and `:p` as the commands they are usually given, and `:&&` next to
  the `:&` that was already there.
- **Constraints it had to respect, and how:** three of them, and each one is the
  same constraint seen from a different side. *Editing while iterating*: the
  command is usually a delete, so the lines are collected first and worked
  through from the bottom up, which is the only order in which line numbers stay
  true. *The one-line message row*: a command that prints cannot print, so the
  matching lines are collected into the row itself — the shape `:diagnostics`
  already had — and a real command is left to say whatever it says; the count is
  written over it only when there was nothing else to say, which is what lets
  `:g/one/wobble` report the unknown command rather than "2 lines".
  *Reuse*: the `:` line parsing is `:s`'s, down to the range helper and the
  delimiter rules, so `:g#a/b#d` and `:g/one` (no closing delimiter) work the way
  a `:s` line does.
- **What the tests caught:** that `strip_range` is written for `:s` and hands
  back `None` for a line with no address at all, which made `:g/one/d` not
  recognized as a global in the first place.
- **Files:** `inkstone-core` (a new `global` module with the parser and its
  tests, `Editor::run_global`, `print_line`, `substitute_again_flags`, the `:`
  name map).
- **Left for later:** nested ranges on the inner command, `:g!`, marks, and the
  `:print` of a whole file, which wants a pager rather than a message row.

## Done — the case operators

- **What shipped:** `g~`, `gu` and `gU` over a motion, a text object, a count, a
  whole line (`g~~`, `guu`, `gUU`) and a visual selection (`~`, `u`, `U`).
- **Constraints it had to respect, and how:** two of them were already solved by
  the shape the editor had. An operator is a command that either takes a motion
  or leaves itself pending, and the pending state already carried a count, a text
  object and a doubling key — so the case operators are three more variants of
  `Operator`, and every one of those features came along for free.
  The other constraint was that a case change is not a cut: nothing goes into the
  register, the text stays where it is, and the cursor ends where the range
  began, so the whole thing goes through one `Editor::change_case` rather than
  through `cut` and a re-insert.
- **What the tests caught:** that vim's doubling key for a `g`-prefixed operator
  is the key it *ends* on — `gUU`, not `gUgU` — which is not the same rule as
  `dd`; and that `gu` and `gU` are also bound in visual mode, because vim binds
  both spellings there.
- **Files:** `inkstone-core` (`Operator`'s three new variants, `change_case`,
  `case_motion`, `case_lines`, the three visual commands, the keymap, the README
  table).
- **Left for later:** `:g/re/cmd`, and case changes that respect a *spell-check*
  or a language's case rules, which vim does not do either.

## Done — `~`, `%`, and counting a number

- **What shipped:** `~` (toggle case, which moves over what it changed), `%` (the
  matching bracket), and `<C-a>`/`<C-x>` (a number, moved by the count).
- **Constraints it had to respect, and how:** all three are motions that are not
  motions, and each one had a rule to get right rather than a feature to write. A
  number is *at or after* the cursor, not only under it, and the number the cursor
  is in the middle of is all of it — the scan walks back over the digits first,
  which is the difference between `100` becoming `101` and becoming `11`. A minus
  in front of the digits belongs to the number, so `-3` counts down rather than
  up. `~` toggles rather than sets, and stops at the end of the line, because a
  count that crossed a line break would make it a motion. `%` reuses the pair
  search the text objects already had — `enclosing_open` and `matching_close` —
  which is why a brace four lines away is found and one with no pair is reported
  rather than guessed at.
- **What the tests caught:** the scan's first shape stopped at the cursor when
  nothing was under it, so `<C-a>` in front of a number did nothing; and the
  `-` handling made the digit run end at the minus, which reported a negative
  number as no number at all.
- **Files:** `inkstone-core` (`number_at`, `bump_number`, `increment`,
  `decrement`, `toggle_case`, `match_bracket`, the keymap, the README table).
- **Left for later:** `g~`/`gu`/`gU` as operators, `~` in visual mode, `<C-a>` in
  visual mode (which counts every line of the selection), and vim's octal and
  hexadecimal numbers.

## Done — hover and definition

- **What shipped:** `K` and `gd`, answered by the language server: a hover in the
  message row as one line, a definition by opening the file it names and going to
  the position.
- **Constraints it had to respect, and how:** both are the shape completion
  already had — a question left by the kernel, asked by the session, answered
  under an id — so what was left was the *answers*: four shapes for a hover, three
  for a location, and a place to put each. A hover has one line to live in, so
  every shape is flattened to one; a definition needs the file, which may not be
  open, and a column in UTF-16 against a line that may not be in memory yet.
- **What the tests caught:** that a definition pointing at a *nonexistent* file
  lands in an empty buffer where the position is clamped away — which is correct,
  and is why the fixture now answers with an absolute path under its own working
  directory and the test writes that file first.
- **Files:** `inkstone-core` (`Ask`, `ask_server`, `goto_location`, the `K`/`gd`
  keys), `inkstone-server` (`hover_text_from`, `definition_from`,
  `path_from_uri`, `goto_definition_from`), the fixture's two new arms.
- **Left for later:** a popup for a long hover, `definition` with a preview
  window, references, and `textDocument/documentSymbol`.

## Next, roughly in order

Each item is meant to be *one* coherent change. If one grows into touching six
modules with new logic in each, that is a sign it should be split.

### 1. Bigger seams, not yet designed

the quickfix-shaped list window, `\zs`, and the rest of the dialect.
Each one should get its own design note first if it wants to change
`inkstone-rpc`; the further features are from the protocol, the cheaper they
are, which is the point of the split. [`rendering.md`](rendering.md) covers
everything a frontend needs, [`selection.md`](selection.md) the pointer and the
clipboard.

## Known gaps that are not features

- Horizontal scroll counts characters, not display columns, so a tab in the
  scrolled-off region can leave the view a column off.
- A brand-new empty file is written as one byte (`"\n"`) rather than vim's zero.
- The daemon sends a whole screen only once, and rows after that; a row is a run
  per identical-cell stretch, so a line of text costs a run per character. See
  [`rendering.md`](rendering.md) if that ever matters.
- Highlighting is per line and the state is per line, so a change high in a large
  file still costs the states from there down — lazily, but linearly.
