# inkstone

[![ci](https://github.com/Tydwdh/inkstone/actions/workflows/ci.yml/badge.svg)](https://github.com/Tydwdh/inkstone/actions/workflows/ci.yml)

A modal terminal editor, written from scratch in Rust. The name is 砚台: the
stone a calligrapher grinds ink on, the thing the brush is dipped into before
anything is written.

The design borrows from both sides of the family: **nvim**'s split between a
headless kernel that owns all state and a thin frontend that only draws, and
**emacs**'s idea that everything is a command a script can call.

```
inkstone  (crossterm frontend)  ──msgpack over stdio──▶  inkstone-server  (headless daemon)
                                                            │
                                                            ▼
                                                       inkstone-core  (editor kernel)
```

The daemon composes the screen as a grid of styled cells; the frontend blits
it. No editor state lives in the frontend, so a second frontend — `inkstone-gui`
today, a remote client tomorrow — reuses the daemon unchanged.

## Status

A working editor rather than a skeleton: it is usable for real editing and thin
on purpose — every feature is a command in the kernel and a key on the way to
it, not a frontend of its own.

Working: open/save, modal editing, movement (with counts: `3x`, `2dd`, `5j`),
insert, `x`, the operators `d`, `y` and `c` with a motion (`dw`, `d$`, `yy`,
`3dd`, `cw`) or a text object (`diw`, `ci"`, `ya{`, `dip`), visual mode (`v`,
`V`, and a `<C-v>` rectangle, with the same operators), registers (`p`, `P`,
`"ayy`, `"ap`, `"_dd`, `"0p`, `"+y`, `:reg`), search (`/`, `?`, `n`, `N`, `*`,
`#`, with every match highlighted) and regular-expression replace (`:s`,
`:%s`, with ranges, `:&` and the `c` flag) in vim's own pattern dialect,
`:vimgrep` to search a project and fill the list of places, syntax
highlighting for Rust, the C family, Python, shells, TOML/YAML/INI, JSON,
Markdown and HTML (fenced blocks and `<script>` bodies in their own language),
undo (`u`) and redo (`<C-r>`), with the state a file was written in remembered
so undoing back to it clears the modified marker, several files at once (`:e`,
`:ls`, `:b`, `:bn`, `:bp`, `:bd`) in several windows (`:sp`, `:vs`, `<C-w>`,
resizable and draggable) and tabs (`:tabnew`, `gt`, `:tabclose`), folds (`zf`,
`zo`, `zc`, `'foldmethod'`), keyboard macros (`q`, `@`), marks, a jump list
(`<C-o>`, `<C-i>`) and a change list (`g;`, `g,`), mouse selection and the
clipboard, command-line editing with history and multi-key insert mappings,
language-server answers when one is running (diagnostics, `K`, `gd`, `gr`,
completion with snippets, `<C-k>`, `:format`), Lua configuration (`init.lua`),
line numbers, tabstop, status line, `.` to do the last change again, and
`f`/`t`/`F`/`T` character finds.

Two frontends draw it: `inkstone` in a terminal, and `inkstone-gui` in a window,
where the screen is drawn on the GPU and an input method works in insert mode.
Both are the same program below the drawing, and both take a click: a drag
selects, `<C-c>` copies to the system clipboard, and the wheel scrolls. Extra
clicks extend the selection the way the manual says — double takes the word under
the pointer (or, on a bracket, everything up to its pair), triple takes the line,
quadruple takes a rectangular block — and a drag after a double click takes whole
words, until the button comes up.

Not implemented: `\p` and `\P`, vim's "printable" class, which is byte-oriented
in vim's own engine — it counts a CJK character as printable and a Greek one as
not — and so has no meaning worth copying here; and `\@<=` and the lookarounds,
which the regex engine has no way to say.
[`docs/roadmap.md`](docs/roadmap.md) lists the rest, with what each thing would
have to respect.

## Build and run

`inkstone` looks for `inkstone-server` beside itself first — which is what
`cargo build --workspace` leaves — and then on `PATH`.

```sh
cargo build --workspace
cargo run -p inkstone -- sample.txt     # or: ./target/debug/inkstone.exe sample.txt
cargo run -p inkstone-gui -- sample.txt # the same screen, in a window
```

`cargo run -p inkstone` alone is not enough on a fresh clone — it builds only the
`inkstone` package, and the daemon has to exist first.

It needs a real terminal (Windows Terminal or conhost): raw mode fails under a
redirected pipe. `inkstone --server <PATH>` points at a daemon somewhere else.

## Keys

`MODE` is `n` (normal), `v` (visual) or `i` (insert). `KEYS` uses vim-ish angle
notation;
`<C-x>` is Ctrl-X, `<A-x>` is Alt-X, `<S-Tab>` is Shift-Tab. A sequence like
`dd` is two keys pressed in order.

<!-- keymap:begin -->
| mode | keys | command(s) |
| --- | --- | --- |
| n | `h` | `move_left` |
| n | `j` | `move_down` |
| n | `k` | `move_up` |
| n | `l` | `move_right` |
| n | `<Left>` | `move_left` |
| n | `<Right>` | `move_right` |
| n | `<Up>` | `move_up` |
| n | `<Down>` | `move_down` |
| n | `<Home>` | `move_line_start` |
| n | `<End>` | `move_line_end` |
| n | `<PageUp>` | `page_up` |
| n | `<PageDown>` | `page_down` |
| n | `0` | `move_line_start` |
| n | `$` | `move_line_end` |
| n | `w` | `move_word_forward` |
| n | `b` | `move_word_backward` |
| n | `g g` | `goto_first_line` |
| n | `<C-w> w` | `window_next` |
| n | `<C-w> h` | `window_left` |
| n | `<C-w> j` | `window_down` |
| n | `<C-w> k` | `window_up` |
| n | `<C-w> l` | `window_right` |
| n | `<C-w> s` | `split_rows` |
| n | `<C-w> v` | `split_columns` |
| n | `<C-w> c` | `window_close` |
| n | `<C-w> =` | `equalize_windows` |
| n | `<C-w> +` | `resize` |
| n | `<C-w> -` | `resize_vertical` |
| n | `<C-w> _` | `resize_vertical` |
| n | `G` | `goto_last_line` |
| n | `g f` | `goto_file` |
| n | `g F` | `goto_file_line` |
| v | `g f` | `goto_file_visual` |
| n | `J` | `join` |
| n | `z f` | `fold_motion` |
| n | `z o` | `fold_open` |
| n | `z c` | `fold_close` |
| n | `z R` | `fold_open_all` |
| n | `z M` | `fold_close_all` |
| n | `z d` | `fold_delete` |
| n | `z m` | `fold_level_less` |
| n | `z r` | `fold_level_more` |
| n | `z x` | `fold_update` |
| n | `z j` | `fold_next` |
| n | `z k` | `fold_previous` |
| n | `g J` | `join_tight` |
| n | `q :` | `command_history` |
| n | `q /` | `search_history` |
| n | `<C-v>` | `visual_block` |
| n | `H` | `goto_screen_top` |
| n | `M` | `goto_screen_middle` |
| n | `L` | `goto_screen_bottom` |
| n | `<C-e>` | `scroll_line_down` |
| n | `<C-y>` | `scroll_line_up` |
| n | `<C-d>` | `half_page_down` |
| n | `<C-u>` | `half_page_up` |
| n | `<C-f>` | `page_down` |
| n | `<C-b>` | `page_up` |
| n | `i` | `enter_insert` |
| n | `I` | `enter_insert_before` |
| n | `a` | `append` |
| n | `A` | `append_eol` |
| n | `o` | `open_line_below` |
| n | `O` | `open_line_above` |
| n | `x` | `delete_char` |
| n | `v` | `enter_visual` |
| n | `V` | `enter_visual_line` |
| n | `d` | `delete_motion` |
| n | `y` | `yank_motion` |
| n | `c` | `change_motion` |
| n | `p` | `put_after` |
| n | `<C-c>` | `copy` |
| n | `P` | `put_before` |
| n | `<Del>` | `delete_char` |
| n | `g ;` | `change_older` |
| n | `g ,` | `change_newer` |
| n | `<C-o>` | `jump_oldest` |
| n | `<C-i>` | `jump_newest` |
| n | `<Tab>` | `jump_newest` |
| n | `u` | `undo` |
| n | `~` | `toggle_case` |
| n | `<CR>` | `open_place` |
| n | `=` | `reindent_motion` |
| n | `<gt>` | `indent_motion` |
| n | `<lt>` | `outdent_motion` |
| n | `g i` | `insert_again` |
| n | `g v` | `reselect` |
| n | `g q` | `format_motion` |
| n | `g ~` | `toggle_case_motion` |
| n | `g u` | `lower_motion` |
| n | `g U` | `upper_motion` |
| n | `g -` | `undo_earlier` |
| n | `g +` | `undo_later` |
| n | `%` | `match_bracket` |
| n | `<C-a>` | `increment` |
| n | `<C-x>` | `decrement` |
| n | `.` | `repeat` |
| n | `<C-r>` | `redo` |
| n | `:` | `enter_command` |
| n | `/` | `search_forward` |
| n | `?` | `search_backward` |
| n | `K` | `hover` |
| n | `g d` | `goto_definition` |
| n | `g r` | `goto_references` |
| n | `] d` | `next_diagnostic` |
| n | `[ d` | `previous_diagnostic` |
| n | `g t` | `tab_next` |
| n | `g T` | `tab_previous` |
| n | `z t` | `scroll_to_top` |
| n | `z z` | `scroll_to_center` |
| n | `z b` | `scroll_to_bottom` |
| n | `;` | `find_repeat` |
| n | `,` | `find_reverse` |
| n | `*` | `search_word_forward` |
| n | `#` | `search_word_backward` |
| n | `n` | `search_next` |
| n | `N` | `search_previous` |
| n | `<Esc>` | `escape` |
| i | `<Left>` | `move_left` |
| i | `<Right>` | `move_right` |
| i | `<Up>` | `move_up` |
| i | `<Down>` | `move_down` |
| i | `<Home>` | `move_line_start` |
| i | `<End>` | `move_line_end` |
| i | `<PageUp>` | `page_up` |
| i | `<PageDown>` | `page_down` |
| i | `<Del>` | `delete_forwards` |
| i | `<C-k>` | `signature_help` |
| i | `<Tab>` | `snippet_next` |
| i | `<S-Tab>` | `snippet_previous` |
| i | `<C-n>` | `snippet_choice_next` |
| i | `<C-p>` | `snippet_choice_previous` |
| i | `<C-y>` | `complete_accept` |
| i | `<C-e>` | `complete_cancel` |
| v | `v` | `enter_visual` |
| v | `J` | `join_visual` |
| v | `g J` | `join_tight_visual` |
| v | `$` | `block_to_end` |
| v | `I` | `insert_block_left` |
| v | `A` | `insert_block_right` |
| v | `<Esc>` | `escape_visual` |
| v | `d` | `delete_visual` |
| v | `x` | `delete_visual` |
| v | `y` | `yank_visual` |
| v | `c` | `change_visual` |
| v | `o` | `swap_ends` |
| v | `<gt>` | `indent_visual` |
| v | `<lt>` | `outdent_visual` |
| v | `=` | `reindent_visual` |
| v | `u` | `lower_visual` |
| v | `U` | `upper_visual` |
| v | `~` | `toggle_case_visual` |
| v | `g u` | `lower_visual` |
| v | `g U` | `upper_visual` |
| v | `g ~` | `toggle_case_visual` |
| v | `<C-c>` | `copy` |
| v | `p` | `put_visual` |
| v | `P` | `put_visual` |
<!-- keymap:end -->

The table above is checked against the built-in keymap by a test, so it cannot
drift. Visual mode binds only what acts on the selection: every other key it is
given falls through to the normal mode's motions, so `v` `w` `d` deletes a word
and `v` `j` `d` takes two lines — through the same commands, remappings included.

`<C-v>` selects a *rectangle*: the rows between the two ends and the columns
between them, drawn as one. A rectangle may reach past the end of a short line —
`l` and `h` in a block move the column and nothing else, so a corner can sit where a
line has run out, which is what vim's `'virtualedit'` allows there. Normal mode never
lets the cursor past the last character of a line. `$` makes it *ragged* instead — the right edge becomes
the end of each line, which is what `<C-v>$d` means: "from here to the end of every
line", not "this many columns". A line too short to reach the left edge contributes
only what it has. `I` and `A` type at the rectangle's left edge and one past its right one, with each
row keeping its own position, and the whole typing is one `u` however many rows it
went to. The two differ for a line that is too short to reach the block, as vim's do:
`I` (and `c`) leave such a line **unmodified** — "a line that does not extend into
the block remains unmodified" — while `A` appends and **pads** the line out to the
block's edge, so the text lines up in a column. A block made with `$` has no column
to reach: `A` appends at the end of each line. A motion can be forced to work the other way, which is vim's `o_v`, `o_V` and
`o_CTRL-V`: `v`, `V` or `<C-v>` typed *after* an operator say what shape the motion's
range will be, rather than starting a selection.

- `d<C-v>j` deletes the character under the cursor and the character below it — the
  block's corners are where the cursor was and where the motion took it.
- `dvj` works characterwise, so a linewise motion becomes *exclusive*: it takes from
  the cursor up to, but not including, the character below.
- `dv$` and `dvw` go the other way, since `$` and `w` are characterwise: `v` turns
  inclusive into exclusive and the other way round.
- `dVw` works linewise whatever the motion was.

The motion is *one* movement command, counted if it needs to be (`d<C-v>2l`), and
`y` and `c` are the same operator with another verb.

`d`, `y` and `c` take the same columns out of every
row — in one undo step, since it is one command — and `c` goes on to type on
**every** row of the rectangle, in the place the block was, the deletion and the
typing being one `u` between them and a row shorter than the
rectangle contributes only what it has, there being no characters past the end of
a line to take. The register remembers the *shape*: `p` and `P` put a block back **in columns** —
one row of the register per row of the buffer, starting after the cursor's column
for `p` and at it for `P`, with a count repeating the rectangle sideways. A row
that is too short for the column is padded with spaces, which is the one place
this editor invents text: a column is a place, and a place past the end of a line
is a space.

### Undo and redo

`u` undoes, `<C-r>` redoes. One step is one change as a user thinks of it: a
whole insert session (everything between entering and leaving insert mode — the
typing, the `<Enter>`s, the backspaces, and a snippet accepted in the middle of it),
or one command — one `dd`, one `x`. A command whose edits cannot be said as one
replacement (a block insert types on every row of a rectangle) is still one step:
its steps are undone together, in the reverse of the order they were made. Undoing restores the
cursor as well as the text.

The history is vim's *undo tree*, not a stack: undoing and then editing again
does not throw the undone branch away. `u` and `<C-r>` walk the branch the text
is on, while `g-` and `g+` (with a count: `3g-`) walk the states in the order
they were *made*, whichever branch they are on — which is how you get back to
what you undid. `:earlier {count}` and `:later {count}` are the same two from the
`:` line, with vim's time forms as well — `:earlier 10s`, `:later 1h`, in
seconds, minutes, hours or days — and `:undolist` names the leaves of the tree,
each with the number `:undo {number}` takes, how deep it is and how long ago it
was made. `{N}f` (file writes) is the one form that is not there.

Writing the file clears the modified flag, not the history: `:q` after a `u`
still refuses until you write again. Reopening a file (`:e`) starts a fresh
history, because the steps of the old document no longer mean anything.

Keys that are not bindings, because they are what the mode is for:

| mode | key | does |
| --- | --- | --- |
| i | any character | insert it |
| i | `<Enter>` | insert a line break |
| i | `<Tab>` | insert a tab |
| i | `<BS>` | delete the grapheme before the cursor, joining lines at column 0 |
| i | `<Esc>` | back to normal mode, stepping left one character |
| : | any character | append to the command line |
| : | `<BS>` | delete from the command line |
| : | `<Enter>` | run the command |
| : | `<Esc>` | abandon the command |
| : | `<Left>` `<Right>` | move within the line |
| : | `<Home>` `<End>` | jump to either end |
| : | `<Del>` | delete the character under the cursor |
| : | `<Up>` `<Down>` | walk what has been run (commands and searches apart) |

### Commands on the `:` line

`/pattern` searches forwards, `?pattern` backwards, `n` and `N` move between
matches and every match on screen is underlined (the one the cursor is on is
bold as well). An empty pattern repeats the last search. `:nohlsearch` stops
drawing the matches without forgetting the pattern — the next search brings the
underlining back — while `:set nohlsearch` is the setting itself and stays off.
Vim defaults that setting to off; this editor has always underlined matches, so
it starts on.

`'wrapscan'` is on, the way vim has it: a search that runs off the end of the
file comes round again. `:set nowrapscan` is what makes `n` past the last match
say `E486: Pattern not found` instead of silently starting over.

`'ignorecase'` makes a search ignore case, and `'smartcase'` takes that back
when the pattern has an upper case letter in it — `/Foo` finds `Foo`, `/foo`
finds either. All three of `/`, `?`, `:s` and `:g` obey both, and a substitute's
`i` flag means "ignore case here" while `I` means "case matters here", whichever
way the settings point.

`'incsearch'` is on: the cursor goes to the match as the pattern is typed, and
the cursor is drawn there rather than on the line being typed, so a search is
aimed rather than committed blind. `<Enter>` accepts it where the preview put the
cursor; `<Esc>` puts the cursor back and puts the search that was in force back
too, so `n` still walks what it walked. `<C-g>` and `<C-t>` walk the matches of
the pattern so far — next and previous — while the line is still open.

`J` joins the cursor's line with the one below it — the next line's indent goes and
a space takes its place — and a count joins that many lines (`3J`). `gJ` joins
without inserting or removing any spaces at all: the indent stays exactly where it
was. Over a selection
`J` joins every line of it, which is what vim's "always whole lines" note means. A
line that ends a sentence — `.`, `?` or `!` — is joined with *two* spaces while
`'joinspaces'` is on (it is, as in vim); `:set nojoinspaces` makes it one. Vim's
clause about a bracket or a quote between the stop and the space (`.)`) is not done:
the line's last character is what is looked at.

Folds hide a range of lines behind its first one: `zf{motion}` folds the lines a
motion covers, `zo` and `zc` open and close the fold the cursor is in, `zR` and `zM`
do all of them, `zd` takes one away, and `zj`/`zk` step to the next and previous one. A closed fold is drawn as its first line
with `+--  N lines: ` in front of it, the lines behind it are not drawn at all, and
the cursor cannot rest inside one — it settles on the fold's first line, so `j` never
walks into a fold you cannot see. Folds belong to the *window*, as in vim: the same
file can be folded one way in one window and another way in the next. Folds *nest*: a fold inside another is a fold of its own, and a closed fold takes
everything inside it — inner folds and all — with it, which is what vim means by a
fold's level being its nesting. A closed fold is one line as far as a motion is
concerned, so `j` steps over one rather than into it, and an operator that reaches into
one takes all of it — `dl` on a closed fold deletes the whole fold, as vim's manual
says. A search started *on* a closed fold begins past it rather than inside it (forward
from the line after it, backward from its first line), entering insert mode opens the
fold the cursor is on, so what is typed can be seen, the cursor is *drawn* in the first
column while it is on a fold line (its own column is left alone), and a substitute's
range is widened to any closed fold it touches — `:s/foo/bar/g` with the cursor on a closed fold
replaces in every line of the fold, which is vim's own example. Folds hold *line numbers*, and
the editor keeps them up to date as lines are added and taken away, so a fold stays on
the text it was put on. `'foldmethod'` is `manual` — folds you make with `zf`
are what there is — or `indent`, which works them out from the indentation and
again on `zx`. `'foldminlines'` says how many lines a fold has to be to be
*displayed* closed (one, vim's default, means two lines), and `'foldlevel'`,
`'foldnestmax'` and `'foldignore'` are the rest of the family. `'foldtext'` is
not implemented: vim evaluates an expression for each drawn fold line, and this
editor will not run a script per line per frame.

`q{register}` records what you type into a register, `q` stops the recording, and
`@{register}` plays it back — through the same state machine every key goes through,
so a macro is exactly what it recorded. A count plays it that many times (`3@a`) and
`@@` plays the last one again. While a recording is running, `q` is the key that ends
it, which is the one place a key means two things: `q:` and `q/` are the history
windows, and a recording ends before either can be reached.

`q:` opens what has been typed on the `:` line in a window of its own, oldest first,
and `q/` the same for searches. It is a window like any other: the lines in it can be
edited, and `<CR>` runs the one under the cursor — a command for `q:`, a search for
`q/`, and it works in insert mode too. The window goes away when a line is run, so
what the command did is not half a screen down, and an edit that is not run goes with
it. `'cmdwinheight'` says how tall the window is, there is no nesting — `q:` inside one
says `E1292: Command-line window is already open` — and `<C-c>` puts the line under
the cursor back on the command line to keep typing, with the window still there.
`'cedit'` (CTRL-F, vim's default) opens the same window from the command line, with
the line being typed as its last line, so a long command can be written in a real
window. Nothing that would leave it works while it is up — `:e`, `:sp`, `:tabnew`, `:bn`
and the rest say `E11: Invalid in command-line window; :q<CR> closes the window` —
and `:q` throws the window away without asking, because what was edited there is
discarded by design. A command run from a script is not
in the history: it is what a *person* typed.

A name typed anywhere — `:e`, `:w`, `:r`, `gf` — has `~` expanded to the home
directory and `$VAR`/`${VAR}` to the variable's value, which is vim's rule for a
file name. `~user` is left as written: looking a user up needs the password
database, which the standard library does not offer.

`{Visual}gf` opens the file whose name is the *selection*, which is how a name with
a space in it is opened at all: `'isfname'` is ignored there, and only leading
blanks are skipped, so what is highlighted is the name.

`gf` opens the file whose name is under the cursor — read as a *file name* rather
than as a word, so `src/main.rs` is one name and not three. It is looked for beside
the file that is open first, since that is what a name in a file means, and then in
`'path'` — a comma-separated list of directories searched in order, which is how a
name in an `import` line finds a file in another directory (`:set path=src,tests`). An
entry may hold a wildcard, which is vim's `file-searching`: `*` is a run of
characters within one name, and a component that is exactly `**` means that
directory and everything under it — thirty levels, or as many as `**5` says — so
`:set path=**` finds a file anywhere under the one that is open. An entry may also
search *upwards*: `include;/usr` looks in `include`, then in the directory it is in,
and so on up to `/usr` — and `dir;` with nothing after the semicolon goes to the
root. A count takes the
count'th file the search finds (`2gf`), trailing punctuation is not part of the name
(`see src/main.rs.` is `src/main.rs`), `'suffixesadd'` is tried when the name as
written is not there (`:set suffixesadd=.rs` finds `main.rs` from `main`), and `gF`
reads a number after the name as a line to put the cursor on (`src/main.rs 42`). A name that is not there says `E484`
rather than being made into a new file: `:e` is how a person makes one, and `gf` is
a jump.

`*` and `#` search for the word under the cursor, forwards and backwards.

`f{char}`, `F{char}`, `t{char}` and `T{char}` move to the next, previous, or
next-but-one character on the line, and `;` and `,` do the last of them again
either way. They are motions like any other, so `df,` deletes through the next
comma and `dt)` deletes up to the closing bracket.

`.` does the last change again, counting the typing an insert session did as part
of it: `cwfoo<Esc>` then `.` changes the next word to `foo`, and a backspace you
made inside the session is made again.

Named registers: `"a` to `"z` are yours, `"0` holds the last yank, `"1` to `"9`
the last deletes, `"_` throws text away without a trace, and `"+` is the system
clipboard as far as the editor can know it — a paste from the terminal fills it,
and `"+y` sends text out to it.

An operator takes a text object as readily as a motion: `diw` deletes the word,
`ci"` changes what is inside the quotes, `ya{` copies the block with its braces,
and `dip` deletes the paragraph — a run of lines that are not blank, whole lines
and all. `aw` takes the space around a word as well, `ap` the blank lines around
a paragraph, and `i`/`a` work in visual mode too (`viw`, `va"`, `vip`).

Patterns are written the vim way and compiled by the engine: `\(a\|b\)` is a
group either side of an alternation, `\+` and `\?` are one-or-more and
none-or-one, `\{2,3}` is a count and `\{-2,3}` the same count taken as few
times as possible, `\<` and `\>` are the ends of a word, and `\c` and `\C`
ask for case-insensitivity or refuse it — wherever they are in the pattern, and
over the `i` flag if both say something. `\v` and `\V` switch everything magic
on and off, the first being what the engine already does. Vim's own character
classes are there too, written out for the engine: `\k` and `\i` are a name's
characters (letters, numbers and `_`), `\I` and `\K` the same without digits,
`\f` and `\F` a file name's characters, `\h` where a name starts, `\a`, `\l` and
`\u` for letters, `\x` and `\o` for hex and octal digits, and `\H`, `\A`, `\L`,
`\U`, `\X` and `\O` for the characters those do not match. Vim builds them out
of `'isident'`, `'iskeyword'` and `'isfname'`, which are not settings here, so
they are those options' defaults, and they are Unicode-aware where vim's engine
counts bytes — `\k` finds a name in `héllo` because that is what the highlighter
already calls a name. The rest is shared
ground: `.` and `*`, `\d` and `\w` and `\s`, `[a-z]`, `^` and `$` for the ends
of a *line* — since a pattern is matched within a line and can never match a line
break — all mean what they look like. `:s` replaces what one finds, and every group reference in its
replacement is written with braces, so `\1` and `$1` both mean the first group
rather than the beginning of a group *name*.
The line at the bottom is edited like any other line — arrows, `<Home>`,
`<End>`, `<BS>`, `<Del>` — and `Up`/`Down` walk what has been run.

A separator can be dragged with the mouse as well — a press on the line between
two windows picks it up rather than starting a selection — and a drag past the
edge stops at a row or a column, so a window cannot be dragged away altogether.

`resize N` / `vertical resize N` set the size of the window that is up
(`:resize` alone grows it by one, `+`/`-` move it), `<C-w>+` `-` are the keys for
that and `<C-w>=` gives every window its equal share again.

`sp` / `vs` split the screen (with a file, on that file) · `<C-w>` `h` `j` `k`
`l` / `w` move between windows · `<C-w>c` or `q` close one · `on` closes the
others. Each window has its own cursor and scroll position, so `:sp` on one file
shows it in two places at once.

`m{a-z}` puts a mark down where the cursor is, `` `{a-z} `` goes back to that
exact character and `'{a-z}` to the first non-blank character of its line. They
are motions like any other, which is the difference between them: `d'a` takes
whole lines and `` d`a `` the text up to the mark. A mark belongs to its file,
so it goes off the screen with it and comes back with it; `:marks` lists what
there is. What this does not do is *carry* a mark through later edits — insert a
paragraph above one and it is a paragraph short of where it was put, which vim
pays for with bookkeeping in every edit — so a mark is clamped into the text
when it is jumped to instead.

A `:` range can name a mark — `:'a,'bd` deletes from the line `a` is on to the
line `b` is on, and `:'a,'bs/x/y/g` substitutes over the same lines. One line on
its own means that line, and a range that names a mark nobody set stops the
command instead of acting on a line nobody asked for.

`g;` and `g,` walk back and forward through the change list — the places edits
were made, one entry per line changed rather than per keystroke, listed by
`:changes` — and `<C-o>` and `<C-i>` walk back and forward through the jump list (`<Tab>` is the
same key as `<C-i>`, in a terminal as in vim), and `` `${1|one,two|}` `` is a choice: the first
option is typed, and `<C-n>`/`<C-p>` in the snippet pick another — the completion
list's keys for the rest of the time. A place written twice follows its first
writing, and the walk steps to it once. Stepping to a place selects what is in it,
so typing replaces the placeholder rather than going in beside it — marks, searches
and `%` leave a place behind, `:jumps` lists them, and a count walks that many at
once.

`''` and `` `` `` go back to where the last jump started — a mark jump, a search,
or `%` — and as they are jumps themselves, pressing one twice puts the cursor
back where it was.

`g/pattern/command` and `v/pattern/command` run a command on every line a pattern
matches — or on every line it does not — working through them from the bottom up,
so `:g/TODO/d` is "delete the lines with TODO on them" and `:v/^$/d` squeezes the
blank lines out. Any other `:` command can be the command, `:g/TODO/s/TODO/done/`
included, and a line with nothing after the pattern prints what matched. `:d` and
`:p` are there for that, being "delete the current line" and "say what it is".
The pattern is `/`'s, dialect and all — `\(a\)` is a group, `\<word\>` a whole
word and `\c` the pattern's own word about case.
`:&` repeats the last substitute without its flags and `:&&` with them, which is
how a `:s/x/y/g` gets used on the next line.

A language server can be spoken to, and the editor will do it: set `lsp` to true
(`ed.set("lsp", true)` in `init.lua`) and the daemon starts a server for the file
that is open — `rust-analyzer` for Rust, `pyright-langserver --stdio` or `pylsp`
for Python, `typescript-language-server --stdio` for JavaScript and TypeScript,
`gopls` for Go —
finding it on `PATH`, saying who it is, telling it what is open and what changed,
and putting what it pushes back where the diagnostics above are drawn from. It is
off by default: starting a process is a bigger thing than a colour. The client
itself lives in `inkstone-lsp`, and the design is in
[`docs/lsp.md`](docs/lsp.md).

The screen is part of the motion vocabulary, as in vim: `H`, `M` and `L` go to the
top, middle and bottom line *of the screen* — with a count, that many lines in from
the edge (`3H`, `3L`) — and they are linewise motions, so `dH` deletes whole lines.
The scrolling keys are the other side of the same idea: `<C-e>` and `<C-y>` scroll
the view a line at a time with the cursor going along by the same amount — vim's
rule, which is what keeps the cursor on the same screen row — and `<C-d>`/`<C-u>`
move it half a screen while `<C-f>`/`<C-b>` move it a whole one. None of the six is
a motion: `d<C-e>` deletes nothing, and the half-typed `d` is put away.
`scrolloff` (off by default) keeps that many lines of context above and below the
cursor, so the view scrolls before the cursor reaches the edge; vim's rule that it
never exceeds half the window, and the end of the file winning over it, are both
kept.

`:set all` shows every setting and its value in a window of its own — a list, so it
is not for editing — and `:set inv{name}` turns a switch the other way.

Options are set the way vim spells them: `:set number` turns a switch on,
`:set nonumber` off, and `:set number?` asks what it is — a bare name that is not a
switch (`:set textwidth`) asks too, since a number has no "on". `:set {name}={value}`
sets any of them from the `:` line, and the same names are what `ed.set` writes.

A value can belong to one file, which is what `:setlocal` is for — vim's table,
exactly:

| command | value in force here | value every other file gets |
|---|---|---|
| `:set {name}={value}` | set | set |
| `:setlocal {name}={value}` | set | — |
| `:setglobal {name}={value}` | — | set |

So `:setlocal tabstop=2` in one file leaves every other file at the global
value, and coming back to that file brings its `2` with it. Asking follows the
same three spellings: `:setlocal tabstop?` is this file's value, `:setglobal
tabstop?` the one outside every file, and `:set tabstop?` the one in force. A
`:setlocal` whose answer is the global value is written with vim's `--` in front
(`--tabstop=4`), and with no argument at all `:setlocal` and `:setglobal` name
the values that scope is holding.

Saving can ask the server to lay the file out first: `formatonsave` (off by default)
makes `:w` write the file and then ask for `textDocument/formatting`, applying what
comes back and writing *that* — so the file on disk is what the server laid out and
the buffer is clean. The order matters: the write happens first, so a server that
never answers costs a person nothing, and `:w {file}` — writing to somewhere else —
does not ask at all, since what would be laid out is not what was written.

In insert mode `<C-n>` and `<C-p>` complete: they list the words already in the
file — the ones nearest the cursor first — and type the one they pick as you move
along the list. With a language server running, the same keystroke also asks it,
and its answer replaces the list. Typing narrows the list, `<C-y>` keeps what is typed, `<C-e>`
puts back what was there before, and the whole session is one `u`.

A snippet — an item whose text is written with placeholders — is typed as its
text, and `<Tab>` steps to each place it wants something filled in, `<S-Tab>` back
again; with nothing to step to, `<Tab>` is a tab as it always was.

A completion list from a language server fills in each item's details as the
selection lands on it — the second question, `completionItem/resolve`, asks about
one item at a time rather than all hundred of them.

`K` asks the language server what the word under the cursor is, `gd` asks where it
is defined and goes there — opening the file if it is not already open — and `gr`
asks where else it is used: the message row says how many places there are and
where the first few are — named by file — and the cursor goes to the first of
them, with `<C-o>` bringing you back.

A `<script>` or `<style>` body is coloured as the language it holds — JavaScript
and CSS, or whatever `language="json"` / `lang="ts"` / `type="text/typescript"`
says — rather than as markup, with the closing tag winning over whatever the
inner language thinks it is in, which is why a `</script>` inside a string has to
be written `"<\/script>"`.

A Markdown fence that says what it holds — ` ```rust `, ` ```py ` — has its body
coloured as that language, the same way a `<script>` does.

Markdown is coloured too — headings, quotations, links, `**thick**`,
`*slanted*` and `` `code` ``, with a fenced block making everything inside it
code — which is a tokenizer of its own rather than a keyword list, since what
Markdown has is not words but punctuation.

A file that changed on disk since it was read is not overwritten by `:w`: the
write is refused with `E13` and the other process's text stays where it is. `:w!`
writes anyway, which is what the `!` is for.

`read {file}` puts a file's lines below the line the cursor is on — or below the
range's last line, with `:2read other.txt` — and `{range}write {file}` writes
those lines out (`:1,5w notes.txt`), leaving the buffer as unsaved as it was,
since only part of it has been written. `:w >> notes.txt` adds to what is already
in the file instead of replacing it.

`normal {keys}` runs the keys as if typed, once for each line of the range —
`:%normal A;` ends every line with a semicolon — with `<Esc>` and the other names
in the keymap's notation for keys that cannot be typed on a `:` line. What is not
one of those names is the character it looks like, so `%normal A <` types a space
and an angle bracket.

`sort` sorts the lines of the file, or of the range in front of it: `:sort!` the
other way round, `:sort n` by the number in each line, `:sort i` ignoring case and
`:sort u` keeping one of each — written together when you want more than one.

`<C-k>` in insert mode — or `:signature` — asks a language server what the call
being typed takes, and says so in the message row with the parameter being typed
picked out.

The last ten lists are kept: `:colder` and `:cnewer` walk back and forward through
them — showing one re-opens its window rather than piling windows up — and the ends
say `E553` rather than wrapping.

What the last question named is kept as a list: `:clist` says it in one line,
`:copen {pattern}` shows it in a window of its own — narrowed to the lines the
pattern matches, which is what makes a list of two hundred places usable — `<CR>` on a line goes to the place that
line names and closes the window, `:cclose` just closes it — taking the list's own
document with it, so a session that opens a hundred lists is not carrying a hundred
scratch documents in `:ls` — and the list is not for editing — and `:cnext` and `:cprev` walk it — opening another file when a place is in one — and a count walks
that many at once. It stops at either end rather than wrapping.

`vimgrep /{pattern}/[g][j] {file}...` fills that list from a search of its own:
the files may hold the wildcards `'path'` takes (`src/*.rs`, `**/*.rs`), a
pattern that is a plain word needs no delimiters around it (`:vimgrep TODO
*.rs`), `g` makes every match on a line a place of its own, and `j` fills the
list without going to the first one. A count in front of the command is the most
places to find, `//` is the last pattern a search used, and nothing found is
`E480` with an empty list rather than the one that was there before. The
pattern, the dialect and the case rules are a search's — except that
`'smartcase'` is not used, which is what the manual says of this command alone.
Two settings say what is looked at: `'wildignore'` is a list of file patterns to
leave alone — a pattern with a separator in it is matched against the whole path,
which is what makes `:set wildignore=target/*` keep a build tree out of a search
— and `'suffixes'` names the files searched *last*, so a search finds the source
rather than its backup first.

`format` asks a language server to lay the file out — `textDocument/formatting`, or
`:{range}format` for part of it —
and applies what it answers with: the edits are given in the document's original
positions and applied in order with a running difference, as one step, so one `u`
takes the whole answer back.

`wsymbols` asks a language server what the *project* holds — `:wsymbols counter`
looks for what it names, and with no name the word under the cursor is the query: the answer is a list of places in other files, which is
filled and shown in the same window `:copen` uses — a search across a project is
something to read rather than to be dropped into.

`symbols` asks a language server what the file holds — the names, what kind of
thing each is and the line it is on — and goes to the first of them.

`marks` lists where the marks are, and `:delmarks a b` or `:delmarks a-c` takes
some of them away — `:delmarks!` takes them all. `:clearjumps` empties the jump
list the same way.

`\zs` and `\ze` say where inside itself a match begins and ends: `s/foo\zsbar/X/`
replaces the `bar` and leaves the `foo`, and a `\ze` on its own is a match that
ends where it says while the text after it is still needed to match —
`end\ze\(if\|for\)` is a match that is "end" in "endif". Vim's rules for the
awkward cases are the ones here: a pattern may say either of them several times
and it is the last one encountered in a matching branch that counts, a `\ze`
before the `\zs` is no end at all, and `&` in a replacement is the match the
pattern reports. A `\zs` inside a collection, or one followed by a multi, is
refused rather than guessed at: vim reads the first as characters and refuses the
second itself.

`diagnostics` lists what a language server has said about this file, and `]d` and
`[d` walk its diagnostics, wrapping around and saying what each one is. The range
is underlined where it is, and the line number in the gutter carries the
severity's colour. The seam a client uses to hand them over is the
`set_diagnostics` command — see [`docs/lsp.md`](docs/lsp.md) for the design and
for where a real language server would live.

`tabnew [file]` opens a tab, `gt` and `gT` walk them, `tabclose` closes one and
`tabs` lists them; the status line shows `[2/3]` when there is more than one.
Each tab keeps its own windows and splits, and every tab sees the same files.

`reg` lists what every register holds.

`e file` opens a file · `ls` lists what is open · `bn` / `bp` next · `b N` or
`b name` jump to one · `bd` close it (refuses if unsaved; `bd!` means it). Every
open file keeps its own cursor, undo history, search and colours, and comes back
where it was.

`s/pat/rep/[g]` replaces on this line, `%s/pat/rep/[g]` in the whole file, and a
range can say which lines instead: `:5s/…`, `:2,3s/…`, `:.,$s/…`, `:$s/…`.
`:&` repeats the last substitute on the line the cursor is on, without its `g` —
the way to walk down a file a match at a time. With the `c` flag `:s` asks about
each match instead of deciding for you: `y` replaces, `n` skips, `a` replaces the
rest, `q` stops, and the whole conversation is one `u`. The
delimiter can be anything that is not a letter, a digit or a backslash
(`:s|/usr/bin|/opt/bin|`), `g`
takes every match on a line, `i` ignores case, `&` in the replacement is the
whole match and `\1` the first group. It reports how many places it changed and
is one `u` to undo.

`w` write · `w <path>` write elsewhere · `q` quit (refuses if unsaved) ·
`wq` write and quit · `q!` quit regardless · `e <path>` reload from disk
(refuses if unsaved) · `e! <path>` discard changes and reload.

## Configuration

`init.lua` lives in the platform config directory (`%APPDATA%\inkstone\config\init.lua`
on Windows, `$XDG_CONFIG_HOME/inkstone/init.lua` on Linux). A file that fails to
parse is reported on the status line and ignored; it never stops the editor.

```lua
ed.set("number", true)     -- line-number gutter
ed.set("tabstop", 8)

ed.map("n", "<C-s>", "save")                        -- to a built-in command
ed.map("n", "Q", function() ed.run("quit") end)     -- or to a function

ed.command("strip_trailing_space", function()       -- new command
  ed.run("move_line_end")
end)
```

| call | meaning |
| --- | --- |
| `ed.map(mode, lhs, rhs)` | bind `lhs` in mode `"n"`, `"v"` or `"i"` to a command name, a function, or `nil` to unbind |
| `ed.command(name, fn)` | register (or replace) a command callable by name |
| `ed.run(name, params)` | run a command; the call is queued and executed after the script returns |
| `ed.set(name, value)` / `ed.get(name)` | options: `tabstop`, `textwidth` and `scrolloff` (integers), `number`, `ignorecase`, `smartcase`, `incsearch`, `hlsearch`, `wrapscan`, `highlight`, `autoindent`, `smartindent`, `lsp`, `formatonsave`, `joinspaces` (booleans), `path`, `wildignore`, `suffixes` and `suffixesadd` (comma-separated lists), `foldmethod` (`manual` or `indent`), `foldlevel`, `foldminlines` and `cmdwinheight` (integers), `foldnestmax` (an integer) and `foldignore` (characters) |

Insert-mode mappings can be sequences too: `ed.map("i", "jk", "<Esc>")` is the
one everybody writes. The keys that could start a mapping are held back until
the next key says whether they are one — so `jk` runs the mapping, and `jx`
types both characters after all. There is no clock, so a key that is also bound
on its own (`j`) runs on its own at once; mapping `jk` and `j` at the same time
means the longer one cannot fire.

## Layout

| crate | what |
| --- | --- |
| `inkstone-lsp` | the language server client: framing, the lifecycle, and the positions |
| `inkstone-core` | the kernel: buffer, cursor, edit history, modes, command registry, keymap, screen model, Lua host |
| `inkstone-rpc` | the wire protocol: message types, framing, the screen model, and the screen a frontend holds |
| `inkstone-server` | the daemon: owns an `Editor`, turns client messages into server frames |
| `inkstone-client` | the daemon plumbing every frontend needs: spawn, framing, folding frames |
| `inkstone` | the crossterm frontend: terminal events in, grid blits out |
| `inkstone-gui` | the windowed frontend: the same grid, drawn on the GPU |

[`ARCHITECTURE.md`](ARCHITECTURE.md) states the invariants and contracts that
cross those boundaries, and where to add the next thing.
[`AGENTS.md`](AGENTS.md) is the orientation for whoever works on it next
(including agents), and [`docs/roadmap.md`](docs/roadmap.md) is what is planned
and what each piece of it has to respect. [`docs/rendering.md`](docs/rendering.md)
is what the wire carries, what it costs, and what a second frontend needs.

## Tests

```sh
cargo test --workspace
cargo clippy --workspace --all-targets
cargo fmt --all --check
```

The suite includes a deterministic 4000-keystroke stress test that re-checks
the buffer and viewport invariants after every key, and an end-to-end test that
drives the real daemon binary over its pipes and inspects the bytes it writes.
The window model is written up in `docs/windows.md`, alongside the notes on
buffers, undo, selection, highlighting, rendering and the language server.

The keymap table above is compared against the built-in bindings by
`keymap::tests::the_readme_keymap_table_matches_the_defaults`, so it cannot
quietly go stale.
