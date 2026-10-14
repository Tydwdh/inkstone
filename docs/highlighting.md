# Syntax highlighting

Colouring a line by what its text *is*, without a parser and without a
dependency. This is the design note for the roadmap's "syntax highlighting" and
for the seam a real parser would slot into later.

## What it colouris, and where

The daemon composes the grid, so the daemon decides the colours: a cell already
carries a foreground colour and attribute bits, and both frontends draw those
without knowing anything about languages. Highlighting therefore costs the
protocol nothing and needs no frontend change at all — unlike the two features
before it, this one stays inside the kernel.

```
line + state ──▶ highlights::tokens ──▶ spans ──▶ render::compose ──▶ cells
                     ▲
                     └── the state a line starts in (inside a block comment,
                         inside a raw string), carried down from the line above
```

## A tokenizer, not a parser

The tokenizer knows six kinds of thing — comments, strings, numbers, keywords,
functions and constants — and it knows them by looking at the characters. That
is not a parser and it does not pretend to be: it cannot tell a macro body from
code, and it matters not at all for colouring, because colouring only has to be
*locally* convincing.

What it must get right is the state that crosses a line:

- **A block comment** (`/* … */`) runs on until it is closed, however many lines
  that takes. Rust's nest, C's do not, and vim colours them the same either way.
- **A raw string** in Rust (`r"…"`, `r#"…"#`) runs on too, and a line inside one
  is not code.
- **Everything else** ends at the line's end: an unterminated `"` colours to the
  end of the line and nothing after it, because that is what a person reading
  the screen wants to see.

Languages for now: **Rust**, a shared "curly" tokenizer for the C family
(C, C++, Java, JavaScript, TypeScript, Go), **Python**, **shell**, **config**
(TOML, YAML, INI), **JSON**, **Markdown** and **HTML**, plus **plain** for
everything else — a file this editor cannot colour is coloured as itself.

## Where the state comes from

One entry state per line, cached, computed lazily from the top of the document
to whatever line is on screen. Recomputing the whole file every frame would cost
the file's length per keystroke, which is exactly what a screen-sized document
must not do.

An edit invalidates the cache from the line it touched onwards, and nothing
else: the lines above a change cannot start differently because of it. The cache
is the only state the highlighter keeps, and it lives on the `Editor`, next to
the buffer it describes.

## The palette

Fixed, and in the kernel, because "what a comment looks like" is the same
question as "what colour is a cell" — and the answer for a terminal and a window
must be the same, or the two frontends would disagree about the same screen.

| token | colour | why |
| --- | --- | --- |
| comment | grey, italic | it is there and it is not the point |
| string | green | |
| number | yellow | |
| keyword | magenta, bold | the shape of the code |
| function | blue | |
| constant (`true`, `false`, `nil`) | orange | |

A `:colorscheme` is its own change, and it is a *setting*, so it would be one
line in the settings table plus a mapping — not a new mechanism.

## The seam for a real parser

`highlights::tokens(language, line, state) -> (spans, state)` is the whole
contract. A tree-sitter highlighter would keep a parse tree per buffer instead of
a state per line and would answer the same question with the same shape of
answer; the renderer, the palette and both frontends would not know.

What is deliberately not here: incremental parsing, semantic highlighting (what
a *type* is, as opposed to what a word looks like), and a real parser per file.

## The languages now

Eight tokenizers, told apart by what their grammars need: Rust (nested comments,
raw strings, lifetimes), the C family (one keyword list for C, C++, Java,
JavaScript, TypeScript, Go and their relatives), Python (`#` comments, three
quotes, triple-quoted strings that span lines), a shell (`#` comments, its own
keywords), and a configuration file (TOML, YAML, INI: `#` comments and no
keywords). JSON is a sixth: strings and numbers and *no* comments, which is the
whole of its difference from the C family and the reason it is not simply that.

HTML is different again: tags, what is quoted inside them, `<!-- -->` comments,
and a `<script>` or `<style>` whose body is *another language*. The inner
language is scanned by the same scanner as anywhere else, with the same rules:
a block comment inside a
`<script>` runs over lines exactly as it would in a `.js` file. What the outer
state carries is three fields rather than a `State` of its own — which language is
running, and what it has open — because a state is `Copy` and a state inside a
state is not; and the closing tag wins over whatever the inner language thinks it
is in, which is why a `</script>` inside a JavaScript string has to be written
`"<\/script>"`.
Two things cross a line and both are states of the shape this file already has —
a comment runs until `-->`, and a tag runs until `>`, so `<a href="one` at the end
of a line makes the next line's `two">text` the rest of the value rather than
text. What is deliberately *not* here is a language found anywhere but where the
markup names it: a `<script>` begins one only when its tag name is read as a tag,
so a `<script>` written inside a string is a string, and nothing inside the inner
language opens a further one.

A Markdown fence that says what it holds — ` ```rust `, ` ```py `, ` ```json ` —
has its body coloured as that language, which is the same trick as a `<script>`:
the inner scan goes through the same scanner, and the outer state carries which
language is running and what it has open. A fence with no language, or one this
editor has no grammar for, is code that is not read any further. One rule is
approximated: a closing fence is a line of the same *character*, and how many of
them there are is not counted, so a fence of four backticks is closed by three.

What Markdown has *within* a line is mostly punctuation that means something, and
that is what its scanner walks: a list marker (only with a space after it, so
`-not a list` stays text), a setext underline (a line of `=` or `-` and nothing
else — which is a rule about the line *above*, and the best a line-at-a-time
scanner can do is recognise the underline for what it is), emphasis, code, links
and entities.

A grammar says which of the differences it has — `line_comment`, `python`,
`nested_comments`, `rust` — and the scanner reads those flags. A new language of
one of those shapes is a keyword list and an extension, not a scanner.

Markdown needed a scanner of its own, and the reason is worth
writing down: it has no keywords and no string literals at all, and what it has
instead is *structure* — a heading is a whole line, a quotation is a `>` at the
front and emphasis is a pair of markers around a stretch. Almost all of that is
answerable within one line, which is why it is still a line-oriented scanner; the
part that is not is a code fence, and a fence is a state exactly like a block
comment: everything until the fence that closes it is code, so a `*` inside one
is an asterisk and not the start of something slanted. Notebooks aside, that is
the only thing in Markdown that crosses a line, and it costs one field in the
state that every language shares.
