# Changelog

## Unreleased

### Added
- Commands can be run: `:!cmd` runs one and puts what it said in a window of its
  own, `:r !cmd` reads its output into the buffer below the line the command was
  given from, and `:{range}!cmd` — `:%!sort`, `:2,5!jq .` — filters those lines
  through it, as one edit, so one `u` puts the text back. `:kill` stops a command
  that will not come back, and a second command while one is running says so
  rather than queueing behind it. The command runs on threads of its own, so the
  editor keeps working while a build does; the shell is `cmd /C` on Windows and
  `sh -c` elsewhere, and neither frontend needed a line of its own for any of it.
  Both of a command's streams go into its output document, because a build's
  errors are on the second one.
- A directory opens as a **listing** — `inkstone .`, `:e src`, or `gf` on a name
  that turns out to be one. It shows what is in the directory, one entry to a
  line, directories first and marked with a `/`, and the path it was opened with
  in the status line. `<CR>` goes to the entry the cursor is on: a directory
  takes the listing's place, because walking down and back up is one document
  rather than a pile of them, and a file opens as a document of its own, so the
  listing stays behind where `:b` finds it again. `-` goes up a level, and is
  vim's "up one line" anywhere that is not a listing. Reading a directory as a
  file used to fail with whatever the platform calls that — `i/o error: 拒绝访问。
  (os error 5)` on Windows — which is a poor answer to a question as clear as
  "show me this folder".
- Writing a listing is making the directory match it. A name typed in is an entry
  made (empty, because a line says a name and nothing else — a `/` on the end
  says a directory), a name changed is a rename, and a name taken out is a
  removal: a directory goes only when it is empty, so no line can delete a
  subtree. One name out and one name in is a rename rather than a removal and a
  creation, which is what keeps the file's content. Every line is checked before
  anything is applied, a file is only ever created rather than truncated, and a
  listing that has been edited refuses to be walked away from with `E37` until it
  is written — the edits to a listing are work nobody has done yet.

### Fixed
- `:copen {pattern}` and `:lopen {pattern}` take a *search's* pattern: a group is
  a group rather than the characters `(a)`, `\zs` means what it means to a
  search, and a `\c` or `\C` in the pattern settles case over `'ignorecase'` and
  `'smartcase'`. They were compiling exactly what they were given — which the
  function that opens them has always said they do not.
- A horizontal scroll counts cells and not characters. A tab is a run of cells
  and a wide grapheme takes two, so a cursor a few characters into a tabbed line
  is well past the right edge of a narrow window: the view scrolled too little
  and left the cursor off the screen, which the roadmap called "a column off".
  The scroll now works in cells, the way the vertical one works in lines, and a
  view that starts in the middle of a tab draws the cells that are left of it —
  which is what vim does with them.
- Closing a list window (`:cclose`, `:lclose`) now closes the document the list was
  made of. It used to stay in `documents` for the rest of the session, one scratch
  document per list opened, all of them visible in `:ls`.
- A language server's answers now reach the screen. They arrive *between*
  keystrokes, and nothing was asking for a frame when one changed what the
  frontend is shown — the message row of a hover, or where a definition goes —
  so the work was done in the kernel and never drawn. This is why `K` and `gd`
  looked unreliable in a terminal: they worked when some other key happened to
  draw the next frame.
- The session starts a language server when the file arrives, not only when it
  starts: a frontend opens its file *after* the session is up, so a daemon that
  decided once, at startup, decided about no file at all. The daemon also ticks
  whether or not a server is running yet, since the tick that starts one is the
  one before there is one.
- `:g` and `:v` take their pattern through the dialect, as `/` and `:s` do. They
  were the one place it was compiled exactly as it is typed, so `:g/\(a\)/d`
  looked for the characters `(a)`, `:g/a\zsb/d` found nothing at all, and
  `:g/\<word\>/d` worked only because the two dialects spell that one the same
  way. A `\c` or `\C` in the pattern now settles case for them as it does for a
  search, over `'ignorecase'` and `'smartcase'`.
- A pattern this editor refuses to translate now says the same thing wherever it
  is typed. A `:s` said `E: … is not supported: …`, the code cut down to a bare
  `E:`; the same refusal through `/` said `E54: E: …`, the code twice. The code
  belongs to the error that knows it is an `E54` and not to the sentence both
  share, so both now read `E54: \zs cannot be followed by a multi`.

### Added
- `:vimgrep`'s `f` flag, which makes the pattern *fuzzy*: its characters have to
  be in the line and in order, but not next to each other, and the pattern is a
  literal string rather than a regular expression — so `:vimgrep /rdme/f **`
  finds `README`, `read_me` and `r a d m e`, and `a.c` is a pattern with a dot
  in it rather than "any character". Several words are each found somewhere in
  the line and in any order, a capital in the pattern wants a capital, and the
  place the list records is the *best-scoring* one rather than the first, since
  the point of fuzzy matching is that the match you meant is the one that looks
  best. The scoring is fzy's, which is what vim's own fuzzy matching is built
  on, and every rule above was measured against vim rather than read off the
  manual: the four sentences about it are one paragraph of `pattern.txt`, and the
  columns a fuzzy `:vimgrep` reports are what `getqflist()` says.
- `'wildignore'` and `'suffixes'`, the two settings `:vimgrep` wanted: a file
  that matches a `'wildignore'` pattern is left out of a search whether a
  wildcard found it or the argument named it (a pattern with a separator in it is
  matched against the whole path, which is what makes `target/*` keep a build
  tree out of a search), and a file whose name ends with one of `'suffixes'` is
  searched last, so a search finds the source rather than its backup first. Both
  defaults are vim's, which makes `'suffixes'` non-empty — `.bak,~,.o,.h,.info,
  .swp,.obj` — exactly as it is there.
- `:vimgrep /{pattern}/[g][j] {file}...`, which fills the places list — the one
  `:copen` shows, `<CR>` goes to a line of and `:cnext` walks — from a search of
  its own. The pattern is `/`'s, dialect and all, and so are the case rules
  except that `'smartcase'` is not used, which the manual says of this command
  alone; `\c` and `\C` settle it over `'ignorecase'`. The files may hold the
  wildcards `'path'` takes, with the name a pattern too (`src/*.rs`, `**/*.rs`),
  and a pattern that is a plain word needs no delimiters (`:vimgrep TODO *.rs`
  is the idiom). `g` makes every match on a line a place of its own, `j` fills
  the list without going to the first one, a count in front of the command is
  the most places to find, and `//` is the last pattern a search used. A
  `:vimgrep` that finds nothing says `E480: No match` and leaves an empty list,
  as vim's does — it does not leave the list that was there before.
- Vim's own character classes, written out for the engine: `\i`/`\k` (a name's
  characters), `\I`/`\K` (the same without digits), `\f`/`\F` (a file name's
  characters), `\h` (where a name starts), `\a`, `\l`, `\u` (letters), `\x` and
  `\o` (hex and octal digits), and `\H`, `\A`, `\L`, `\U`, `\X`, `\O` for the
  characters those do not match. Vim builds the first six out of `'isident'`,
  `'iskeyword'` and `'isfname'`, which are not settings here, so they are those
  options' defaults; and where vim's engine counts bytes these are Unicode, which
  is what `\d` and `\w` already meant here — `\k` finds a name in `héllo`,
  because that is what the highlighter already calls a name. `\A` no longer means
  the engine's start-of-text anchor by accident.
- `\zs` and `\ze` in a pattern, which "set the start and end of the match": each
  becomes an empty group where it matched, and the part of a match the pattern
  *reports* — the cells a search highlights and the text `:s` replaces — is read
  back from there. Vim's own rules, including the awkward ones: either may be
  said several times and the last one encountered in a matching branch is the one
  that counts, a `\ze` before the `\zs` is no end at all, and `&` in a
  replacement is the reported match while `\1` is still the pattern's own first
  group — the markers are groups too, and the numbers are what a replacement
  means by them. A `\zs` inside a collection, or one followed by a multi, is
  refused rather than guessed at (vim reads the first as characters and refuses
  the second itself).
- `{Visual}gf`: the highlighted text is the name of the file to open — "same as
  "gf", but the highlighted text is used as the name of the file to edit. 'isfname'
  is ignored" — which is the only way to open a name with a space in it, since the
  cursor's rules stop at the space.
- A file name a person types is expanded the way vim expands one: `~` at the front
  is the home directory and `$VAR`/`${VAR}` is the variable's value, wherever the
  name is typed — `:e`, `:w`, `:r`, `gf`. `~user` is left as written, since looking
  a user up needs the password database; so is a variable that is not set.
- `'cedit'` (CTRL-F, vim's default): the key that opens the command-line window
  from the command line, with "the command as typed so far" as its last line. Both
  of vim's spellings work — `<C-f>` and the caret form `^F` — and a modified letter
  is the same key whatever its case, since a terminal has one CTRL-F.
- The `q:` and `q/` windows can be *edited* — vim's command-line window, "you can
  now use commands to move around and edit the text in the window" — and `<CR>`
  runs the line under the cursor in insert mode as well as in normal mode, which
  is what the manual says and what a person typing in that window expects. What
  was edited but not run goes with the window. The other lists (a server's
  answers, the settings) stay read-only: a list that can be edited is a list that
  can lie about what it says.
- `'cmdwinheight'` says how many screen lines that window uses, seven by default;
  there is no nesting (`E1292: Command-line window is already open`); `<C-c>`
  continues in Command-line mode with the line under the cursor, leaving the window
  where it is; and every command that would leave the window — another file,
  another window, another tab, a list window of its own — says `E11: Invalid in
  command-line window; :q<CR> closes the window`. `:q` there throws the window
  away without asking about changes, since what was edited in it is discarded by
  design.
- `'suffixesadd'`: suffixes `gf` tries when the name as written is not there, so
  `:set suffixesadd=.rs` finds `main.rs` from `main`. The name as written is
  looked for over every `'path'` entry first, so what a person typed wins over
  what a setting guessed.
- A `'path'` entry can search *upwards*, which is vim's second kind of file
  searching: `include;/usr` looks in `include` and in every directory above it up
  to the stop directory, and `dir;` goes to the root. A directory that is not
  there ends the walk rather than searching above it. The combined form
  (`**;/stop`) is not implemented.
- `gf` takes a count — the count'th file the search finds — and reads a name the
  way vim does in one more respect: trailing punctuation (`.,:;!`) is not part of
  it, so `gf` on `see src/main.rs.` opens `src/main.rs`.
- `gF` is `gf` with a line: a number after the name is a line in the file, and the
  cursor is put there (`src/main.rs 42`).
- `'path'` entries can hold vim's file-searching wildcards: `*` for a run of
  characters within one name, and a component that is exactly `**` for that
  directory and everything under it (thirty levels, or as many as `**5` says).
  `:set path=**` is what makes `gf` find a name anywhere under the file that is
  open. The upward `dir;/stop` form is not implemented.
- The rest of `:setlocal`/`:setglobal`: with no argument they name the values
  that scope is holding (vim's "all local options which are different from the
  default"), and `:setlocal {name}?` answers `--{name}={value}` when the file has
  no value of its own and the global one is what is in use — vim's marker for
  exactly that.
- `'smartcase'`: with `'ignorecase'` on, a pattern that has an upper case letter
  in it is taken literally — `/Foo` finds `Foo`, `/foo` finds either. It applies
  to `/`, `?`, `n`, `N`, `:g` and `:s`, which is vim's own list for it.
- `'ignorecase'` now reaches `:s` and `:g`, which had been matching case-
  sensitively whatever the setting said. A substitute's `i` flag means "ignore
  case here" and the new `I` flag means "case matters here", so a pattern can
  still say what it wants either way round.
- `'foldminlines'`, the last setting of the fold family: a fold is *displayed*
  closed only when it is longer than that many lines, so a two-line fold shows
  both of its lines with `:set foldminlines=2`. The fold itself is still closed —
  `zo` opens it, and a later `zc` closes it again — which is why the manual says
  the setting "only has an effect on what is displayed".
- A fold's marker says `+--  1 line: one` for a fold that hides one line: it
  said `1 lines`, because the count is of what is hidden and the plural was only
  ever right for the smaller half of the cases.
- `:setlocal` and `:setglobal`, the manual's own table: `:set {name}={value}`
  writes the value in force *and* the one every other file gets, `:setlocal` only
  the file that is open, and `:setglobal` only the value outside every file. A
  file's own values travel with it — switching away puts what is in force back to
  the global value, and coming back brings the file's values with it — and asking
  follows the same three spellings.

### Added
- The undo tree: undoing and then editing again no longer throws the undone
  branch away. `u`/`<C-r>` walk the branch the text is on, `g-`/`g+` (with
  counts) walk the states in the order they were *made* whichever branch they are
  on, `:earlier {count}`/`:later {count}` are the same from the `:` line, and
  `:undolist` names the leaves of the tree, with the number `:undo {number}`
  takes, how deep each is and how old.
- `:undo {number}` jumps to the state a number names, and `:earlier`/`:later`
  take vim's time forms as well as a count: `:earlier 10s`, `:later 1h`, in
  seconds, minutes, hours or days. `{N}f` (file writes) is not implemented.
- `:undolist` counts leaves, not states: a state something else was made from is
  not one.

### Changed
- The kernel's internals were reorganized for legibility, with no change in
  behaviour. `editor.rs` (14,705 lines: one 6,157-line `impl` block and half its
  length in tests) is now `editor/`, a file per concern, with `editor/tests/` one
  file per topic; `inkstone-core`'s public interface went from 575 `pub`
  declarations to 268, the rest `pub(crate)`; `execute_cmdline` went from 380
  lines of if-blocks to 104 and eight named handlers plus a command table; and
  the daemon's `pump_lsp`/`ask_lsp` went from 180/104 lines to 5/65 with the
  answers and requests named. The narrowing also deleted five pieces of dead
  code that `pub` had been hiding, and marked five test-only methods
  `#[cfg(test)]`.

### Added
- `'hlsearch'` (on, where vim defaults to off) and `:nohlsearch`, which is not
  the same thing: the command stops drawing the matches without forgetting the
  pattern, and any search brings the underlining back, while the setting stays
  where it is put. A search being typed still shows the match it found, whatever
  either says — that is what `'incsearch'` is for.
- `'wrapscan'` (on, the way vim has it): `:set nowrapscan` makes a search that
  runs off the end of the file say `E486: Pattern not found` and leave the
  cursor where it was, instead of coming round again.
- `:normal` now leaves what its keys had to say standing, the way `:g` does for
  the command it runs on each line: `:normal /zzz<Enter>` says `E486` rather
  than covering it with a count of lines normalised.
- Double, triple and quadruple clicks select what the manual says they do: a word
  under the pointer (or, on a bracket, everything up to its pair), a line, and a
  rectangular block. A drag after a double click takes whole words until the
  button comes up, and a double click's selection survives the release — a click
  is still a click, but a double click is not one. The frontend counts the clicks,
  because it is the one with a clock: the protocol carries the count, not a time.
- `'incsearch'`, on by default: the cursor goes to the match while a search is
  still being typed, and is drawn on that match rather than on the line being
  typed, so a search is aimed rather than committed blind. `<Enter>` accepts it
  where the preview put the cursor, `<Esc>` puts the cursor back *and* puts the
  search that was in force back, and `<C-g>`/`<C-t>` walk the matches of the
  pattern so far. The preview is a real search — the pattern is compiled and run
  on every keystroke — so what is highlighted on screen is the pattern being
  typed and not the last one's; a pattern that matches nothing, or does not
  compile yet, shows and says nothing until the line is accepted.
- `gr` goes to the first place a reference answer names — the list is what the
  message row can hold, and the place is where a person meant to go, with `<C-o>`
  bringing them back.
- A placeholder is *selected* when a snippet steps to it, so typing replaces
  what is there instead of going in beside it: `<Tab>` to `${1:name}` and typing
  `value` leaves `value`, not `valuename`. The replacement is one edit, so one
  `u` puts the placeholder back. Leaving the snippet lets the selection go.
- A place written twice in a snippet — `${1:name}` and `${1:name}` again — is one
  place: the second follows the first, so what is typed into one is what the other
  says, and the walk steps to it once rather than twice. The following comes from
  the same place every other range correction does — an edit goes through one
  path, and that path keeps the snippet's ranges true — so typing and picking a
  choice are the same mechanism rather than two.
- A snippet choice, `${1|one,two|}`, is read *and* pickable from: the first option
  is typed, and `<C-n>`/`<C-p>` in a snippet session replace it with the next or
  the previous one — the keys the completion list has for the rest of the time.
  The stops after a choice move by however much the text grew or shrank, since
  they are ranges and a range is only true until the text in front of it changes.
- Snippets: an item whose text is a snippet — `insertTextFormat` 2 — is typed as
  its *text*, with the placeholders' defaults in it and the markers gone, and
  `<Tab>` in insert mode steps to each place it wants something filled in, with
  `<S-Tab>` back again. The stops are stepped through in *number* order rather
  than the order they are written, which is what makes a snippet's fields come
  out in the order they were meant; `${1:name}` types `name` and `$1` types
  nothing; a default may hold stops of its own; `\$` is a dollar. Past the last
  stop the session is over and `<Tab>` is a tab again, and leaving insert mode
  ends it wherever it was.
- The details of a completion item come from `completionItem/resolve`: a server's
  answer names the items and leaves what each one *is* for a second question,
  asked about the item the list has moved to and no other. The item is asked about
  as the server sent it — `data` and all — rather than as the kernel read it, and
  what comes back goes where the popup shows it: the item's `detail`, or the first
  line of its documentation when that is all there is.
- `gr` asks a language server where a name is used, and the answer goes in the
  message row: how many places there are and where the first few are, named by
  the file they are in, capped the way the other lists are. The four shapes a
  place can arrive in — a location, a list of them, a `LocationLink`, and nothing
  — all read as the same list, which is also what a definition answer now goes
  through, so the two share the parsing rather than each having a dialect.
- A change list, `g;` and `g,` to walk it, and `:changes` to see it: every edit
  leaves the place it happened, one entry per line changed rather than one per
  keystroke — typing a word into a line is one entry, a `cw` on it is another —
  and the walk goes back and forward through them with a count. Editing after
  walking back starts a new list from there, the way an undo does. Undo and redo
  add nothing: they put text back where it was, and an entry for that would say
  an edit happened where none did.
- `:delmarks {letters}` — written together or apart, with a hyphen for a run of
  them (`:delmarks a-c`) — `:delmarks!` for every mark of the buffer, and
  `:clearjumps` for the jump list. A letter that is not one of the twenty-six is
  named back rather than taken for a mark that was never there.
- A jump list, and `<C-o>`/`<C-i>` to walk it: the keys that travel — a mark, a
  search, `%` — leave the place they started in it, a count walks that many
  places at once, `:jumps` lists them newest first with the walk's place marked,
  and the list holds a hundred places like vim's. Leaving a place that is already
  the one under the walk's index does not add it twice, which is what makes
  `<C-o>` and `<C-i>` come back to where they were rather than pile up duplicates.
  A terminal cannot tell `<C-i>` from `<Tab>`, so both are bound, as in vim.
  `''` and `` `` `` stay what they were: one place, the last jump's start, so
  pressing one twice toggles.
- Patterns are written in vim's dialect and translated for the engine, in `:s`
  and in `/` alike: `\(` `\)` and `\|` for groups and alternation, `\+`,
  `\?` and `\{n,m}` (and the lazy `\{-n,m}`) for counts, `\<` and `\>` for
  word boundaries, `\c` and `\C` for case — over the `i` flag when both say
  something — and `\v`/`\V` to turn magic on and off. Everything the two
  dialects already share is left alone. `\zs` and `\ze` are refused rather than
  translated, since the engine's `\z` is a different thing entirely and would
  quietly match something else.
- Marks in a `:` range: `:'a,'bd`, `:'a,'bs/x/y/g`, `:'a,$s/o/O/g` and the rest.
  A range is lines, so a backtick names the same line as an apostrophe here, and
  an address range in front of a command now means the lines it names: `:5d`
  deletes line five, `:'a,'bd` the lines between two marks. A range that names a
  mark nobody set stops the command rather than acting on a line nobody asked
  for. This also fixed what one address meant: `:5s/a/b/` is line five — and
  `:.s` the cursor's line — where it used to mean "from there to the end".
- `''` and `` `` `` go back to where the last jump started — a mark jump, a
  search, or `%` — and are jumps themselves, so pressing one twice toggles
  between the two places. Both are motions too, so `d`` ` works.
- Marks: `m{a-z}` puts one down, `` `{a-z} `` goes to that exact character and
  `'{a-z}` to the first non-blank one of its line, and `:marks` lists them. Both
  jumps are motions, so `d'a` takes whole lines and `` d`a `` the text up to the
  mark. A mark belongs to the file rather than to the window looking at it, so
  switching documents takes it off the screen and brings it back with the text.
  A mark is *not* carried through later edits the way vim carries one — that
  costs bookkeeping in every edit — so the position is clamped into the text when
  it is jumped to, and a mark can never point past the end.
- `:g/pattern/command` and `:v/pattern/command` run any other `:` command on the
  lines a pattern matches — or on the lines it does not. The lines are worked
  through from the bottom up, because the command is usually one that takes lines
  out (`:g/TODO/d`, `:v/^$/d`) and removing a line does not move the ones above
  it; the pattern is the same dialect `:s` and `/` use, the delimiter can be
  anything that is not a letter, and a range says which lines are *searched*. A
  line with nothing after the pattern prints what it matched, in the one line of
  message row there is, capped the way `:diagnostics` is.
- `:d` (delete the current line) and `:p` (say what it is), which is what `:g`
  and `:v` reach for when the command is one of them.
- `:&&` repeats the last substitute with the flags it had, so a `:s/x/y/g` can be
  used on the next line; `:&` goes on repeating it without them.
- `g~`, `gu` and `gU` are operators: they take a motion, a text object or a
  count, and `g~~`, `guu`, `gUU` do whole lines. Over a visual selection they
  are `~`, `u` and `U` — or `g~`, `gu`, `gU`, which vim allows too. A case
  change is not a cut: the register keeps what it had, and `.` repeats it.
- `~` toggles the case of the characters it moves over, and a count stops at the
  end of the line — it is not a motion, and a `~` that walked into the next line
  would be one.
- `%` goes to the bracket that matches the one under the cursor, or the first
  bracket on the line if the cursor is not on one, using the same search the text
  objects use — so a brace whose pair is four lines down is found, and one with no
  pair is said so rather than guessed at.
- `<C-a>` and `<C-x>` add to and take away from the number under the cursor, or
  the next number on the line, by the count: `5<C-a>` is five more. A minus in
  front of the digits is the number's own, so `-3` goes to `-4`, and a number the
  cursor is inside of means the whole of it — `100` rather than the `0` under the
  cursor. Only decimal digits count, unlike vim, which reads a leading `0` as
  octal and `0x` as hexadecimal.
- `K` asks what the thing under the cursor is, and `gd` where it is defined —
  both answered by the language server when one is running, and both nothing at
  all when one is not. A hover's answer is shown in the message row and squeezed
  onto one line, whatever shape it came in (a string, a marked string, a list of
  them, or a `MarkupContent`); a definition opens the file it names — in this
  session's list of documents if it is already open, from disk if it is not — and
  puts the cursor on the position, with the protocol's UTF-16 column converted
  against the line it points into.
- Completion can come from the language server. `<C-n>` opens the list on the
  words in the file at once — there is no reason to wait for a round trip to be
  told what is already here — and the session asks the server in the same breath:
  the answer replaces the list, and what it would type is the server's
  `insertText` (or its `textEdit`'s new text, or its label). The client grew the
  half of the protocol it did not have: requests with ids, answers kept by the id
  they answer — a server may answer before anyone looks — and an API for taking
  an answer. Asking puts the typed prefix back in the buffer first, because a
  server is answering about the word as it was asked about.
- Completion. `<C-n>` and `<C-p>` open a list of the words already in the file —
  nearest the cursor first, one of each, and only words of two characters or
  more — and *type* the one they pick, the way vim does: the list is something
  you look at while typing rather than a menu you confirm afterwards. Typing (or
  backspacing) narrows it by what is in front of the cursor, and a word nothing
  fits closes it, keeping what has been typed. `<C-y>` keeps what is typed and
  puts the list away, `<C-e>` puts back the word that was there before the list
  opened, and leaving insert mode keeps what is typed. Everything the list types
  is one step of undo. It is drawn in the grid, under the word being completed —
  which is where everything else is drawn, so no frontend learned anything —
  flipped above the cursor when there is no room below, with the pick in reverse
  video.
- A language server can be turned on, and the editor will talk to it. `lsp` is a
  setting (`ed.set("lsp", true)` in `init.lua`), off by default: the daemon looks
  for a server for the file that is open — `rust-analyzer` for Rust, `pyright` or
  `pylsp` for Python, `typescript-language-server` for JavaScript and TypeScript,
  `gopls` for Go — and starts the first one it finds on `PATH`. It says who it
  is (`initialize`, with the file's folder as the workspace), tells the server
  what is open and what is in it, sends the whole document again whenever the
  document's edit count moves, and puts what the server pushes back where
  diagnostics live — converting the protocol's UTF-16 columns into this editor's
  characters against the line they point into. The daemon's loop waits a
  hundredth of a second between messages so a server that speaks while nothing
  else is happening is heard rather than noticed a keystroke later.
  `INKSTONE_LSP` names a command to use instead, for a server that is not
  installed under its usual name.
- `inkstone-lsp`, a language server client: the protocol's framing
  (`Content-Length`, a blank line, that many bytes of JSON) with an in-memory
  test for every way a stream can be malformed, its lifecycle (`initialize`,
  `initialized`, `didOpen`, `didChange` with whole documents, `didSave`,
  `didClose`, `shutdown`), a reader thread so a server that pushes can be heard
  while the editor is doing something else, and the position conversion between
  the protocol's UTF-16 code units and this editor's characters. The end-to-end
  test runs it against a real process over real pipes — a `fake-lsp-server`
  binary the crate builds beside its tests — because that is where framing bugs
  live.
- Diagnostics: the editor now has somewhere to put what a language server says
  about a file, and draws them — the range is underlined (an error in bold as
  well as underlined, so it stands out from a warning without either of them
  taking a colour away from the syntax) and the line number in the gutter takes
  the severity's colour, which is the one part of a row that is the editor's and
  not the file's. `]d` and `[d` walk them, wrapping and saying what each one is;
  `:diagnostics` lists them. They belong to the document, like the search
  matches, so a file keeps its own and a reload drops the ones that were about
  the text that is no longer there. `set_diagnostics` is the seam a client or a
  script calls. Design note: [`docs/lsp.md`](docs/lsp.md) — of which the
  load-bearing finding is that the wire does not change: the kernel composes the
  grid, so a diagnostic is an attribute bit and a gutter colour, and the
  frontends learn nothing.
- Dragging the line between two windows resizes them. A press on a separator
  picks it up instead of starting a selection — the separator is not text — and
  the size follows the pointer's distance from where it was picked up, so the
  drag does not depend on how many events arrived. A window cannot be dragged
  away altogether: it keeps a row or a column of its own.
- Splits can be resized. `:resize 5` makes the window that is up five rows tall,
  `:resize` grows it by one, `:resize -2` and `:resize +2` move it, and
  `:vertical resize N` does the same across; `<C-w>+`, `<C-w>-` and `<C-w>=`
  are the keys for growing and for equal shares again. A size is rows or columns
  of the window itself — the row a stacked window keeps for its status line is
  its own — and a window with no division beside it has nothing to resize, so it
  says so and stays put.
- Tab pages. `:tabnew [file]` opens a tab of its own — on an empty file, or on
  the one named — `:tab split` puts the document that is on screen in a new tab,
  `gt` and `gT` walk the tabs, `:tabclose` closes one,
  `:tabs` lists them, and the status line says `[2/3]` when there is more than
  one. Each tab has its own windows and its own split layout; the *documents* are
  shared, so a file opened in one tab is the same file in another. `:q` closes a
  window, then the tab, then the session, one step at a time.
- `:s` takes the `c` flag: `:%s/a/b/gc` asks about every match —
  `replace with b (y/n/a/q/l)?` — where `y` replaces, `n` skips, `a` replaces
  this one and all the rest, `q` stops, `l` replaces this one and stops, and
  Escape gives up. Every answer belongs to one undo step, so the whole
  conversation is one `u`, and a pattern that never matched still says `E486`
  rather than reporting nought substitutions.
- `:s` takes line ranges: `:5s/…`, `:2,3s/…`, `:.,$s/…`, `:$s/…`, with `%` still
  the whole file and no range still the line the cursor is on. Addresses are
  resolved against the file as it is when the line runs, so `:5s/a/b/` means the
  fifth line now, whatever moved it there.
- `:&` repeats the last `:s` on the cursor's line: the same pattern and
  replacement, without the `g` that was used last time — which is what makes
  `:%s/a/b/g` then `:&` a way to walk down a file. `:&&` (with the flags) is not
  here; `E33` is what `:&` says when nothing has been substituted yet.
- `zt`, `zz` and `zb` scroll the window so the cursor's line is at the top, the
  middle or the bottom of it, without moving the cursor — including empty rows
  below the last line, which is what `zz` on the last line of a file shows.
- Syntax highlighting for Python, shells, TOML/YAML/INI and JSON, beside Rust and
  the C family. Python's `#` comments, `def`/`class` names, and its triple-quoted
  strings — which span lines, so the lines after one are still inside a string —
  are all coloured; JSON gets no comment colouring at all, because it has no
  comments, and its keys are not mistaken for keywords of other languages.
- `ip` and `ap`, the paragraph text objects: a paragraph is a run of lines that
  are not blank, `ip` is those lines and `ap` also takes the blank lines after
  them (or the ones before, when the paragraph ends the file). They are whole
  *lines*, so `dip` fills the register the way `dd` does and pastes as lines.
  On a blank line `ip` is the run of blank lines the cursor is in, which is what
  clearing the gap between two paragraphs means.
- `f{char}`, `F{char}`, `t{char}` and `T{char}` — to the next or previous
  character on the line, on it (`f`, `F`) or beside it (`t`, `T`) — with `;` and
  `,` to do the last one again either way, counts (`3f,`), and a place in the
  operator grammar: `df,` deletes *through* the comma, `dt)` up to the closing
  bracket, and `yf"` yanks through the quote. A find never leaves its line, and
  `Escape` calls one off.
- `.` repeats the last change. A change is a command that changed the buffer or
  one that opened an insert session, so `x`, `dd`, `ciw` and `cwfoo<Esc>` all
  repeat — and the text a session typed is typed again by `.`, backspaces and
  deletes included, which is why `iab<BS>c<Esc>` then `.` types `ac` and not
  `ab<BS>c`. A count repeats it that many times (`3.`), a motion or a yank in
  between does not disturb it, and `.` keeps working after `u`.
- Named registers. `"a` to `"z` before an operator, a yank or a put chooses where
  the text goes (`"ayy`, `"ap`, `"_dd`); `"0` always holds the last yank, `"1` to
  `"9` the last deletes newest-first, `"_` is the black hole that leaves every
  register untouched, and `"+y` hands text to the system clipboard while a paste
  from the terminal fills `"+` with what it pasted. `:reg` lists them.
- Text objects: `iw`/`aw`, the quoted text (`i"`, `a"`, and `'` and a backtick),
  and bracketed pairs (`i(`/`a(`, `i[`, `i{`, `i<` with their closing forms, `b`
  and `B` for the two most common). They work with `d`, `y` and `c` (`diw`,
  `ci"`, `ya{`) and in visual mode (`viw`, `va"`). `aw` takes the space around a
  word — the one after it, or the one before when it ends the line — and a block
  across lines keeps the line break its closing bracket sits on, so `di{` on a
  function leaves the braces on lines of their own.
- `*` and `#` search for the word under the cursor, forwards and backwards,
  matching that word whole rather than its letters inside a longer one.
- Windows. `:sp` and `:vs` divide the screen, `<C-w>w` `h` `j` `k` `l` move
  between the windows, `<C-w>s` `v` `c` split and close, `:q` closes the window
  it is run in and quits on the last one, and `:on` closes the others. A window
  has its own cursor and its own scroll position, so two windows can look at one
  file from two places, and an edit made in either is in both. Every window gets
  its own status line; the ones that are not current say `NORMAL`, since only
  the window that is up can be in another mode. Design note:
  [`docs/windows.md`](docs/windows.md).
- More than one file at a time. `:e other.txt` opens it as a document of its own
  and the one that was open keeps its place: `:ls` lists them, `:bn`/`:bp` walk
  them, `:b N`/`:b name` jump, `:bd` closes (refusing when there is unsaved work,
  `:bd!` when you mean it). Each document keeps its own cursor, undo history,
  search and syntax colours, and where the window was scrolled to; the register,
  the keymap and the options belong to the session.
- Search patterns are regular expressions — Rust's dialect, matched within a
  line, so `^` and `$` are the line's own ends and a pattern cannot match a line
  break. `\d+`, groups, `(?i)` and the rest of that dialect work in `/`, `?`,
  `n`, `N` and in `:s`; a pattern that does not compile says why (`E54`) instead
  of pretending to have found nothing.
- `:s/pattern/replacement/[g]` — and `:%s/…` for the whole file. The delimiter
  can be anything that is not a letter, so `:s|/usr/bin|/opt/bin|` works; the
  replacement is written the way vim writes one, with `&` for the whole match and
  `\1` for a group; `g` takes every match on a line and `i` ignores case. It
  reports how many places it changed, leaves the cursor on the last line it
  changed, and the whole thing is *one* `u` however many lines it touched.
- Syntax highlighting: comments, strings, numbers, keywords, function names and
  named constants are coloured, for Rust and for the C family (C, C++, Java,
  JavaScript, TypeScript, Go and their relatives). A block comment or a Rust raw
  string carries on to the next line and the line after that, because the
  tokenizer carries its state down the document — cached per line and thrown away
  from the first line an edit could have changed, so a keystroke costs the
  screen, not the file. `highlight` (on by default) turns it off. Colours compose
  with everything else: a selected keyword keeps its colour, and a search match
  inside a string is still underlined. Nothing was needed on the wire or in
  either frontend.
- The pointer: clicking puts the cursor where it pointed, dragging selects — the
  same selection visual mode makes, so the operators and the highlight already
  work on it — and a click with no drag between collapses it again. The wheel
  scrolls the view and leaves the cursor alone. A cell a frontend sends is turned
  into a buffer position by the daemon, through the same walk that draws the line
  run backwards, so a click on a tab, on the second half of a CJK character or
  into the line-number gutter all land where they should.
- Copying to the system clipboard: `<C-c>` (or a window's `Ctrl+C`) hands the
  selection — or the register, with nothing selected — to a frontend that can
  reach a clipboard. The window writes it to the OS clipboard and `Ctrl+V` reads
  it back as `ClientMsg::Text`; the terminal asks its own terminal with OSC 52.
  Vim's `"+` and `"*` registers are not part of this: the register is not the
  clipboard.
- Insert-mode mappings can be key sequences: `ed.map("i", "jk", "<Esc>")` works.
  The keys that could start a mapping are held back until the next key says
  whether they are one; if they are not, they are typed after all, in the order
  they were pressed — so nothing is lost, and a key that cannot start a mapping
  is typed immediately.
- Editing and history on the line at the bottom: arrow keys, `<Home>`, `<End>`,
  `<BS>` and `<Del>` work inside a `:` command or a `/` search, wherever the
  cursor is, and the line scrolls sideways when it is longer than the window.
  `Up` and `Down` walk what has been run — commands and searches kept apart, the
  way vim keeps them — and the line that was being typed comes back when the
  walk runs off the end.
- Search: `/pattern` forwards, `?pattern` backwards, `n` and `N` between
  matches, and wrapping round the document the way vim does. Every match on
  screen is underlined and the one the cursor is inside is bold, which is what
  the attribute bits added for it were for. `ignorecase` (off by default) folds
  case; a search left by `Esc` stays usable, and an empty pattern repeats the
  last one.
- Visual mode: `v` selects characters and `V` whole lines, the same motions
  extend the selection, and `d`, `y` and `c` act on it — as does `p`, which puts
  the register over the selection. `o` puts the cursor on the other end, `v`/`V`
  toggle between the two kinds (and leave, pressed again), and the status line
  says which kind is up. A selection is highlighted with the reverse-video bit
  the protocol already had, so no frontend needed a new message.
- Operators and a register: `d`, `y` and `c` take a motion (`dw`, `d$`, `d0`,
  `db`, `dj`, `dG`, `dgg`, `dd`), `yy`/`dd` work on whole lines with their
  terminators, and `p`/`P` put what the last delete or yank took. Vim's rules
  where they matter and are visible: `cw` changes the word rather than the space
  after it, `cc` leaves the line behind, empty, and a put is one `u` to take
  back.
- Counts in normal mode: `3x`, `2dd`, `5j`, `2d3w` (the operator's count times
  the motion's), and `3G`/`3gg` to go to a line. A count larger than the buffer
  stops at the end of it rather than spinning: `100j` on three lines is the last
  line, not a million steps.
- `ClientMsg::Text`: text that was never a key — a paste, or what an input
  method committed — lands at the cursor and is one `u` to take back, whatever
  it spells. The terminal frontend uses it for a bracketed paste (where
  crossterm can deliver one, which is Unix only), and a windowed frontend needs
  it for an input method.
- A continuation cell — the right half of a wide grapheme — now carries its
  grapheme's colours and attributes instead of being a plain blank. A highlight
  that covers a CJK character covers both of its cells, in both frontends.
- Cell attributes for italic, faint and strikethrough text. Nothing emits them
  yet; they are the vocabulary a syntax highlighter and a GPU renderer need, and
  every frontend has to translate them or the tests say so.
- `inkstone-client`: finding and starting the daemon, framing and folding its
  frames, shared by every frontend instead of written twice.
- Frames carry whether the editor is inserting text, because a windowed frontend
  has to know: an input method belongs where text goes and nowhere else. Left on
  in normal mode it swallows exactly the keys that run commands, which is what
  one looked like from the outside — `dd` and `:wq` did nothing until the window
  learned to keep it out. The cursor is a bar while inserting, a block otherwise.
- Undo and redo, on `u` and `<C-r>`. One step is one change as a user thinks of
  it — a whole insert session (typing, `<Enter>`, backspace), or one command, so
  a counted `3x` or a multi-line `dd` is a single `u`. A step records the
  character range it replaced rather than a copy of the document, and undo
  restores the cursor along with the text.
- A step that removes the buffer's last terminator owns the newline the
  trailing-newline invariant hands back, so undoing `dd` on the last line of a
  file does not leave a stray empty line behind.
- `inkstone-gui`: the windowed frontend, and the first thing that draws a screen
  without a terminal. It spawns the same daemon, speaks the same framing, folds
  the same frames into the same `Screen` and draws what is left to draw with
  `wgpu`: one instance buffer laid out row by row, so a delta frame rebuilds and
  reuploads only the rows the daemon named, and one draw call per frame for the
  row backgrounds, the glyphs, the underlines and strikethroughs and the cursor
  block. Glyphs are shaped by `cosmic-text` and rasterized into an atlas that
  grows on demand; `Color::Reset` means the window theme and the 256-colour
  palette is computed rather than pasted. Keys and input-method commits go the
  other way, a resize becomes `ClientMsg::Resize` so the daemon re-lays out, and
  a close or a `:q!` becomes `Shutdown` and a waited-for daemon. What CI cannot
  check (a window needs a window) is checked by hand; the key translation, the
  window-to-grid arithmetic and the palette are tested.

### Changed
- The wire is MessagePack's compact form, a colour travels as one integer
  instead of an enum variant whose *name* was written out for every cell, and a
  session sends the whole screen only once: after that it sends the rows that
  changed, each as runs of identical cells. A full 80x24 screen went from 63 KiB
  to 11 KiB, and a keystroke that moves the cursor from 63 KiB to 232 bytes
  (200x60: 387 KiB to 72 KiB, and 232 bytes) — which matters because the daemon
  sent a whole screen on every keystroke.
- `docs/rendering.md`: what a screen costs on the wire, and what a second (GPU)
  frontend needs from the protocol before it can exist.
- The project is called **inkstone**. Crates are `inkstone`, `inkstone-core`,
  `inkstone-rpc` and `inkstone-server`; the configuration directory moved to
  `%APPDATA%\inkstone\config` (`$XDG_CONFIG_HOME/inkstone/` elsewhere).

### Fixed
- `5G` went to the last line instead of the fifth: a count before `G` is a *line
  number*, the one place in the editor where a count is not a repeat.
- A character find swallowed the next key after any pending sequence: `zt` set
  the window's scroll *and then* waited for a character to find, so the scroll
  happened with the wrong answer twice over. `f`, `F`, `t` and `T` now only
  start a find when nothing else is pending.
- Changing the word at the end of a line put the cursor a character too far
  left, so `ciw` on a line's last word typed the new text before the space it
  meant to replace. The cut clamps the cursor for normal mode, and insert mode
  has to be entered before it, not after.
- The `[+]` marker and `:q` now agree with vim after an undo: `u` back to the
  state a file was written in clears the marker, so `:q` quits instead of
  refusing with `E37` and needing `:q!`. Nothing is stored to make this work —
  the undo history gives every state an identity, and the last write remembers
  which one it wrote, so an edit that happens to restore the same *text* is
  still an edit.
- One `u` now takes back a whole insert session that a command started: `ohello`
  then `<Esc>` used to leave the empty line behind when undone, and `cw` followed
  by typing used to leave the word deleted. The session's group was being closed
  before the typing joined it, and two steps inside one group were folded into a
  single step only when they happened to chain into each other.
- A counted `x` — `ed.run("delete_char", 3)`, or the same over RPC — deleted
  past the end of the line and joined the next one onto it. It now stops on the
  last character of the line, the way repeated `x` does in vim.
- The windowed frontend left the old glyphs of a row on screen when a row needed
  fewer quads than the frame before — deleting the last line of a file showed it
  best. The frame draws every row's whole reservation, so the slots a row gives
  up are now written as quads that draw nothing.
- Arrow keys, `<Home>`, `<End>`, `<PageUp>` and `<PageDown>` did nothing in
  insert mode — the keys you reach for most while typing. Navigation is now an
  ordinary insert-mode binding set, so `ed.map("i", ...)` can override it.
- `<PageUp>`/`<PageDown>` moved a single line instead of scrolling a screen.
- `ed.map("i", "jk", ...)` was accepted and then never fired. It is now
  rejected at load with an explanation.

### Changed
- The recurring lists — protocol keys, cell attributes, settings and built-in
  commands — are declared once and generate the code around them, so there is
  no second list to keep in step. Concretely: a new key is one line in
  `inkstone-rpc` (the notation layer needs nothing, and the frontend fails to
  compile until it maps the key); a new setting is one line in
  `settings.rs`; a new command is a handler plus its name, with no name string
  to get wrong. Command handlers are now named exactly like the commands they
  implement.
- Option access goes through a `Setting` enum instead of option name strings,
  so the editor, the script layer and the renderer cannot disagree about which
  options exist, and a forgotten case is a build error rather than a runtime
  "unknown option".
- `KeyCode::ALL` and `ATTRS` enumerate the protocol's keys and cell attributes.
  Neither list is enforceable by the compiler, so each is paired with a test
  that iterates it and covers the notation, the terminal mapping and the
  attribute translation.
- `Shift`+`Tab` used to be dropped by the frontend; it now arrives as `<S-Tab>`,
  which the protocol could always express.

### Removed
- Dead API with no caller outside its own test: `inkstone_rpc::write_msg_compact`
  (a second framing encoder), `Grid::resize`, `Viewport::bottom_line`,
  `Keymap::is_empty`, `CommandRegistry::{is_empty, len, names}`, and the
  `Editor::cursor_settled` synonym for `after_edit`.
- `Buffer::insert` and `Buffer::remove`. Every change to the text now goes
  through `Buffer::replace_range`, so that a change is *one* operation: undo
  rebuilds a deletion and an insertion together, and a buffer that restored its
  trailing newline in between the two would come back with one newline too many.

### Known gaps
- The register is the unnamed one only: `"a` and friends are not there yet, and
  nothing outside the editor can fill it.
- Text objects (`diw`, `ci(`) are not implemented, so `d` and `c` take motions
  only; `d` followed by anything else says so and changes nothing.
- A middle or right click, a double click and a drag past the edge of the window
  do nothing: the pointer carries the left button and the wheel, and a frontend
  sends what the protocol has.
- Syntax highlighting is a tokenizer, not a parser: it colours by what the
  characters look like, so a macro body, a type name and a nested comment are
  guesses. It knows Rust and the C family and colours anything else as itself;
  `tree-sitter` grammars are the upgrade, and `highlights::tokens` is the seam
  they would slot into.
- Opening a document and switching away from a modified one is deliberately not
  vim: `:e other.txt` keeps the changed document in the list rather than throwing
  its changes away, and `:bp` finds it again. Only `!` abandons anything.
- Patterns are Rust's regex dialect, not vim's: `\(`, `\<`, `\zs` and the rest
  of vim's own spelling do not work, and a pattern can never match across a line
  break. There is no `*`/`#` (the word under the cursor), no `:s` line ranges
  (`:5,10s/…`), no `c` flag (confirm each change) and no repeat (`:s` alone).
- A windowed session has no selection, no clipboard and no mouse; keys held down
  do not repeat, and there is no smooth scrolling.
- The `:` command line has no cursor movement (no arrows, no history).
- Undo has no save point: undoing back to what is on disk still reports the
  buffer as modified.

## 0.1.0 — walking skeleton

### Added
- A headless daemon (`inkstone-server`) that owns all editor state and composes the
  screen, and a crossterm frontend (`inkstone`) that only draws it, talking over
  length-prefixed MessagePack frames on stdio.
- A modal editing model: normal, insert and command-line.
- A command registry with 29 built-in commands, and a keymap that a script can
  extend with `ed.map`.
- An embedded Lua host: `init.lua` can remap keys, register commands and set
  `tabstop` and `number`.
- Basic editing: open/save, `h j k l`, `w b`, `0 $`, `gg G`, `i I a A o O`,
  `x`, `dd`, and `:w :q :wq :q! :e`.
- A rope-backed buffer, a composable screen grid, and a status line.

### Fixed
Defects found by a bug pass over the skeleton; each was reproduced by a failing
test before it was fixed:

- CRLF files were edited through their raw bytes: `A` appended *after* the
  carriage return, `$` parked the cursor on an invisible character, and `dd`
  left a stray `\r` behind.
- Normal mode let the cursor sit one column past the last character.
- The cursor's screen column came from a character count, so tabs and
  double-width graphemes put it on the wrong cell.
- `w` skipped empty lines instead of stopping on them, and swallowed a
  following punctuation run.
- `$` landed on the line terminator rather than the last character.
- The line-number gutter was invisible to the scroll maths.
- A terminal reporting a zero-sized window produced a meaningless scroll offset.
- `:e` discarded unsaved changes without asking.
- The Delete key did nothing in insert mode and was unbound in normal mode.
