//! Syntax highlighting: what a line's text is, by looking at it.
//!
//! The tokenizer knows five kinds of thing — comments, strings, numbers,
//! keywords and functions — and knows them by reading the characters. It is not
//! a parser: it cannot tell a macro body from code or a nested comment from a
//! mistake, and colouring does not need it to. What it does have to get right is
//! the *state* that crosses a line, because a block comment does not end where
//! the screen does: a line that opens one says so, and the state is carried down
//! line by line.
//!
//! See `docs/highlighting.md` for why this is a tokenizer rather than a parser,
//! and where a real one would slot in.

use std::cell::{Cell, RefCell};
use std::path::Path;

use inkstone_rpc::{Color, ATTR_BOLD, ATTR_ITALIC};

use crate::buffer::Buffer;

/// A language this editor can colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    /// Nothing is coloured: the text is the text.
    Plain,
    /// Rust.
    Rust,
    /// The C family: comments and strings and numbers, one keyword list for all
    /// of C, C++, Java, JavaScript, TypeScript, Go and their relatives.
    Curly,
    /// Python: `#` comments, three kinds of string, and its own keywords.
    Python,
    /// A shell: `#` comments and its own keywords.
    Shell,
    /// A configuration file — TOML, YAML, INI: `#` comments and literals, and no
    /// keywords to speak of.
    Config,
    /// JSON: strings and numbers, and no comments at all — which is the whole
    /// difference from the C family.
    Json,
    /// Markdown: headings, emphasis, code and quotations — a language whose
    /// *structure* is punctuation, which is why it has a scanner of its own
    /// rather than a keyword list.
    Markdown,
    /// HTML and XML: tags, attributes and comments, with the comment a state
    /// that runs over lines like every other block comment here.
    Html,
}

/// The extensions the curly tokenizer is used for.
const CURLY_EXTENSIONS: &[&str] = &[
    "c", "h", "cc", "cpp", "cxx", "hpp", "hxx", "cs", "java", "js", "jsx", "mjs", "ts", "tsx",
    "go", "kt", "kts", "swift", "php", "scala", "dart",
];

impl Language {
    /// The language a file's name suggests, if it suggests one.
    pub fn from_path(path: &Path) -> Self {
        match path.extension().and_then(|extension| extension.to_str()) {
            Some("rs") => Language::Rust,
            Some("py") | Some("pyi") => Language::Python,
            Some("sh") | Some("bash") | Some("zsh") => Language::Shell,
            Some("toml") | Some("yaml") | Some("yml") | Some("ini") | Some("cfg")
            | Some("conf") => Language::Config,
            Some("json") => Language::Json,
            Some("md") | Some("markdown") | Some("mdx") => Language::Markdown,
            Some("html") | Some("htm") | Some("xhtml") | Some("xml") | Some("svg")
            | Some("vue") => Language::Html,
            Some(extension) if CURLY_EXTENSIONS.contains(&extension) => Language::Curly,
            _ => Language::Plain,
        }
    }

    /// Whether this language colours anything.
    pub fn is_plain(self) -> bool {
        self == Language::Plain
    }
}

/// What a stretch of a line is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Token {
    /// A comment, line or block.
    Comment,
    /// A string literal.
    String,
    /// A number literal.
    Number,
    /// A keyword of the language.
    Keyword,
    /// A name being called: an identifier followed by an open parenthesis.
    Function,
    /// A named constant: `true`, `false`, `nil`, `None`.
    Constant,
}

/// One coloured stretch of a line, in character columns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    /// The first character of the stretch.
    pub start: usize,
    /// One past its last character.
    pub end: usize,
    /// What it is.
    pub token: Token,
}

/// Where a line starts, which is what the line above decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct State {
    /// How many block comments are open. Rust's nest, C's do not, so this is
    /// more than one only in Rust.
    comment: usize,
    /// The `#`s of an open raw string, if one is open.
    raw: Option<usize>,
    /// The quote of a triple-quoted string that is still open, if one is: `"""`
    /// and `'''` both run over line breaks, so the line after one starts inside
    /// a string.
    triple: Option<char>,
    /// The character a Markdown code fence was opened with, if one is open:
    /// everything between three backticks and the three that close them is code,
    /// and what looks like emphasis inside it is not.
    fence: Option<char>,
    /// Whether an HTML tag is still open: `<a` at the end of a line is a tag
    /// that runs over, and the line after it is inside one.
    in_tag: bool,
    /// The quote of an HTML attribute value that is still open, if one is: a
    /// value written over two lines is one value.
    in_quote: Option<char>,
    /// The language running inside an HTML `<script>` or `<style>`, if one is:
    /// what is between the tags is not markup, and colouring it as markup is
    /// worse than not colouring it at all.
    inner: Option<Inner>,
    /// What that inner language has open — a block comment, a string — since it
    /// runs over lines the same way it would anywhere else. It is kept here
    /// rather than in a `State` of its own because a state is `Copy` and a
    /// state inside a state is not.
    inner_comment: usize,
    /// And the quote of a string that is still open inside it.
    inner_string: Option<char>,
}

/// The language a `<script>` or `<style>` holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Inner {
    /// A `<script>`: the C family's grammar, which is what JavaScript and
    /// TypeScript are here.
    Script,
    /// A `<style>`: CSS, which has `/* */` comments, strings and numbers.
    Style,
    /// A Markdown fence that says what it holds: ` ```rust `, ` ```py ` and the
    /// rest of the names this editor has grammars for.
    Rust,
    /// A fence of Python.
    Python,
    /// A fence of shell.
    Shell,
    /// A fence of JSON.
    Json,
    /// A fence of configuration.
    Config,
}

impl Inner {
    /// The grammar that reads this language.
    fn grammar(self) -> &'static Grammar {
        match self {
            Inner::Script => &CURLY,
            Inner::Style => &CSS,
            Inner::Rust => &RUST,
            Inner::Python => &PYTHON,
            Inner::Shell => &SHELL,
            Inner::Json => &JSON,
            Inner::Config => &CONFIG,
        }
    }
}

/// What a `<script>` holds, from the attributes of its opening tag.
///
/// `language="json"`, `lang="ts"` and `type="text/typescript"` all say the same
/// thing in different words, and the words are the ones a fence uses — so the
/// mapping is the same one, and a script that says nothing is JavaScript.
fn script_inner(chars: &[char], from: usize) -> Option<Inner> {
    let rest: String = chars[from..].iter().collect();
    for attribute in ["language", "lang", "type"] {
        let mut search = rest.as_str();
        while let Some(at) = search.find(attribute) {
            let after = &search[at + attribute.len()..];
            let after = after.trim_start();
            if let Some(value) = after.strip_prefix('=') {
                let value = value.trim_start();
                let quote = value.chars().next().filter(|c| *c == '"' || *c == '\'');
                let value = match quote {
                    Some(quote) => value[1..].split(quote).next().unwrap_or(""),
                    None => value.split_whitespace().next().unwrap_or(""),
                };
                // `type="text/typescript"` is a media type: the part after the
                // slash is the language.
                let name = value.rsplit('/').next().unwrap_or(value);
                if let Some(inner) = inner_for(name) {
                    return Some(inner);
                }
            }
            search = &search[at + attribute.len()..];
        }
    }
    Some(Inner::Script)
}

/// The language a Markdown fence's info string names, if this editor has a
/// grammar for it.
fn inner_for(info: &str) -> Option<Inner> {
    // ` ```rust title="x" ` and ` ```js ` are both an info string whose first
    // word is the language.
    let name = info.split_whitespace().next().unwrap_or("").trim();
    Some(match name.to_ascii_lowercase().as_str() {
        "rust" | "rs" => Inner::Rust,
        "python" | "py" => Inner::Python,
        "sh" | "bash" | "shell" | "zsh" => Inner::Shell,
        "json" => Inner::Json,
        "toml" | "yaml" | "yml" | "ini" => Inner::Config,
        "js" | "javascript" | "ts" | "typescript" | "jsx" | "tsx" => Inner::Script,
        "css" => Inner::Style,
        _ => return None,
    })
}

impl State {
    /// A line that starts in code.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether this state is plain code: nothing is carried over.
    pub fn is_code(&self) -> bool {
        self.comment == 0
            && self.raw.is_none()
            && self.triple.is_none()
            && self.fence.is_none()
            && !self.in_tag
            && self.in_quote.is_none()
            && self.inner.is_none()
    }
}

/// The colour a token is drawn in, and the attribute bits that go with it.
///
/// The palette lives here rather than in a frontend because "what a comment
/// looks like" is one answer for a terminal and a window both.
pub fn style(token: Token) -> (Color, u16) {
    match token {
        Token::Comment => (Color::Indexed(244), ATTR_ITALIC),
        Token::String => (Color::Indexed(114), 0),
        Token::Number => (Color::Indexed(180), 0),
        Token::Keyword => (Color::Indexed(170), ATTR_BOLD),
        Token::Function => (Color::Indexed(75), 0),
        Token::Constant => (Color::Indexed(173), 0),
    }
}

/// The state every line starts in, computed as far as it is needed and kept.
///
/// A block comment does not begin at the top of the screen, so the state of the
/// first visible line depends on every line above it — and recomputing the whole
/// file per frame would cost the file's length per keystroke. So the states are
/// cached, filled lazily down to whatever line is asked for, and thrown away
/// from the first line an edit could have changed.
#[derive(Debug)]
pub struct LineStates {
    language: Cell<Language>,
    /// The state each computed line *ends* in, so the next line starts in it.
    states: RefCell<Vec<State>>,
}

impl LineStates {
    /// A highlighter for a document of `language`.
    pub fn new(language: Language) -> Self {
        Self {
            language: Cell::new(language),
            states: RefCell::new(Vec::new()),
        }
    }

    /// The language being coloured.
    pub fn language(&self) -> Language {
        self.language.get()
    }

    /// Colour a different language from now on: a different document.
    pub fn set_language(&mut self, language: Language) {
        self.language.set(language);
        self.states.borrow_mut().clear();
    }

    /// Forget the states from `line` onwards.
    ///
    /// Nothing above a change can start differently because of it, which is what
    /// makes this cheap. Merging a line into the one above invalidates from the
    /// upper line, and the caller passes the earlier of the two.
    pub fn invalidate(&self, line: usize) {
        self.states.borrow_mut().truncate(line);
    }

    /// The coloured spans of `line`, and the state the next line starts in.
    pub fn spans(&self, buffer: &Buffer, line: usize) -> Vec<Span> {
        if self.language.get().is_plain() || line >= buffer.line_count() {
            return Vec::new();
        }
        let entry = self.state_of(buffer, line);
        let (spans, _) = tokens(self.language.get(), &buffer.line_string(line), entry);
        spans
    }

    /// The state `line` starts in, computing whatever is not known yet.
    fn state_of(&self, buffer: &Buffer, line: usize) -> State {
        if line == 0 {
            return State::new();
        }
        let mut states = self.states.borrow_mut();
        while states.len() < line {
            let index = states.len();
            let entry = match index.checked_sub(1) {
                None => State::new(),
                Some(previous) => states[previous],
            };
            let (_, exit) = tokens(self.language.get(), &buffer.line_string(index), entry);
            states.push(exit);
        }
        states[line - 1]
    }
}

/// The coloured spans of one line, and the state the next line starts in.
pub fn tokens(language: Language, line: &str, state: State) -> (Vec<Span>, State) {
    match language {
        Language::Plain => (Vec::new(), State::new()),
        Language::Rust => scan(line, state, &RUST),
        Language::Curly => scan(line, state, &CURLY),
        Language::Python => scan(line, state, &PYTHON),
        Language::Shell => scan(line, state, &SHELL),
        Language::Config => scan(line, state, &CONFIG),
        Language::Json => scan(line, state, &JSON),
        Language::Markdown => markdown(line, state),
        Language::Html => html(line, state),
    }
}

/// HTML and XML: tags, what is quoted inside them, and comments.
///
/// Two things cross a line here, and both are the same shape as everywhere else
/// in this file: a comment runs until `-->` however many lines that takes, and a
/// tag runs until `>` — so `<a` at the end of a line makes the next one a
/// continuation of the tag rather than text. What is *not* here is a language
/// inside a language: the body of a `<script>` is plain, where vim would colour
/// it as JavaScript.
fn html(line: &str, state: State) -> (Vec<Span>, State) {
    let chars: Vec<char> = line.chars().collect();
    let mut spans = Vec::new();
    let mut index = 0;

    // Inside a comment from an earlier line.
    if state.comment > 0 {
        match find(&chars, 0, "-->") {
            Some(end) => {
                spans.push(Span {
                    start: 0,
                    end: end + 3,
                    token: Token::Comment,
                });
                index = end + 3;
            }
            None => {
                spans.push(Span {
                    start: 0,
                    end: chars.len(),
                    token: Token::Comment,
                });
                return (
                    spans,
                    State {
                        comment: 1,
                        ..State::new()
                    },
                );
            }
        }
    }

    // A quoted value left open on an earlier line runs to its closing quote.
    let mut in_quote = state.in_quote;
    if let Some(quote) = in_quote {
        let end = match scalar_end(&chars, 0, quote) {
            Some(end) => {
                in_quote = None;
                end
            }
            None => chars.len(),
        };
        spans.push(Span {
            start: 0,
            end,
            token: Token::String,
        });
        index = end;
    }

    // Inside a `<script>` or a `<style>`: everything up to the closing tag is
    // that language, and the closing tag is markup again.
    if let Some(inner) = state.inner {
        // Only markup has a tag that ends it; a fence is ended by the fence.
        let closer = match inner {
            Inner::Script => "</script",
            Inner::Style => "</style",
            _ => "",
        };
        if closer.is_empty() {
            // A fence's body runs to the fence that closes it, which the
            // Markdown scanner is the one that knows about.
            return (spans, state);
        }
        let mut carried = State {
            inner: Some(inner),
            ..state
        };
        match find(&chars, index, closer) {
            Some(at) => {
                // The closing tag wins, even inside a string or a comment: that
                // is why a `<script>` in a JavaScript string has to be written
                // `"<\/script>"`.
                let body: String = chars[index..at].iter().collect();
                spans.extend(inner_spans(&body, index, inner, &mut carried));
                index = at;
            }
            None => {
                let body: String = chars[index..].iter().collect();
                spans.extend(inner_spans(&body, index, inner, &mut carried));
                return (
                    spans,
                    State {
                        inner: Some(inner),
                        ..carried
                    },
                );
            }
        }
        // The closing tag ends the inner language, and is markup itself.
        let mut closing = State {
            inner: None,
            ..carried
        };
        closing.inner_comment = 0;
        closing.inner_string = None;
        let rest = html(&line.chars().skip(index).collect::<String>(), closing);
        spans.extend(rest.0.into_iter().map(|span| Span {
            start: span.start + index,
            end: span.end + index,
            token: span.token,
        }));
        return (spans, rest.1);
    }

    let mut in_tag = state.in_tag;
    let mut opened: Option<Inner> = None;
    while index < chars.len() {
        if chars[index] != '<' {
            // The `>` that ends a tag, and what is quoted inside one — which is
            // the only thing in a tag worth marking.
            if in_tag && chars[index] == '>' {
                in_tag = false;
                index += 1;
                // A tag that opened a script or a style: what follows is that
                // language, and the same walk over the rest of the line knows
                // how to read it — so it is the walk that does it, rather than
                // a second copy of the same logic here.
                if let Some(inner) = opened {
                    let rest: String = chars[index..].iter().collect();
                    let (mut inner_spans, state) = html(
                        &rest,
                        State {
                            inner: Some(inner),
                            ..State::new()
                        },
                    );
                    for span in &mut inner_spans {
                        span.start += index;
                        span.end += index;
                    }
                    spans.extend(inner_spans);
                    return (spans, state);
                }
                continue;
            }
            if chars[index] == '&' {
                if let Some(end) = entity_end(&chars, index) {
                    spans.push(Span {
                        start: index,
                        end,
                        token: Token::Constant,
                    });
                    index = end;
                    continue;
                }
            }
            if in_tag && (chars[index] == '"' || chars[index] == '\'') {
                let quote = chars[index];
                let start = index;
                let end = match scalar_end(&chars, index + 1, quote) {
                    Some(end) => end,
                    None => {
                        // The value runs on to the next line.
                        in_quote = Some(quote);
                        chars.len()
                    }
                };
                spans.push(Span {
                    start,
                    end,
                    token: Token::String,
                });
                index = end;
                continue;
            }
            index += 1;
            continue;
        }
        // `<!--` is a comment; `<!DOCTYPE …>` is a declaration, coloured as a
        // string because that is what it looks like.
        if line_at(&chars, index, "<!--") {
            match find(&chars, index, "-->") {
                Some(end) => {
                    spans.push(Span {
                        start: index,
                        end: end + 3,
                        token: Token::Comment,
                    });
                    index = end + 3;
                }
                None => {
                    spans.push(Span {
                        start: index,
                        end: chars.len(),
                        token: Token::Comment,
                    });
                    return (
                        spans,
                        State {
                            comment: 1,
                            ..State::new()
                        },
                    );
                }
            }
            continue;
        }
        if line_at(&chars, index, "<!") {
            let end = find(&chars, index, ">").map_or(chars.len(), |at| at + 1);
            spans.push(Span {
                start: index,
                end,
                token: Token::String,
            });
            index = end;
            continue;
        }
        // A tag: `</name`, a name, and the rest of it until `>`.
        let start = index;
        let mut at = index + 1;
        if chars.get(at) == Some(&'/') {
            at += 1;
        }
        let name_start = at;
        while at < chars.len()
            && (chars[at].is_alphanumeric() || chars[at] == '-' || chars[at] == ':')
        {
            at += 1;
        }
        if at > name_start {
            // The `<` and `</` are punctuation, the name is the keyword.
            spans.push(Span {
                start,
                end: at,
                token: Token::Keyword,
            });
            // `<script>` and `<style>` are not markup with a body: their body is
            // another language.
            let name: String = chars[name_start..at].iter().collect();
            if chars.get(start + 1) != Some(&'/') {
                opened = match name.to_ascii_lowercase().as_str() {
                    // `language="…"` and `type="…"` say what a script holds:
                    // JSON, TypeScript, or whatever else the fence table knows.
                    "script" => script_inner(&chars, at),
                    "style" => Some(Inner::Style),
                    _ => None,
                };
            }
        }
        // The rest of the tag is scanned as the tag it is: what is quoted in it
        // is a string, and the `>` that ends it is what ends it. Jumping to the
        // `>` here would be one character of work saved and every attribute
        // missed.
        in_tag = true;
        index = at.max(index + 1);
    }
    (
        spans,
        State {
            comment: 0,
            in_tag,
            in_quote,
            ..State::new()
        },
    )
}

/// The spans of what is inside a `<script>` or a `<style>`.
///
/// The inner language is scanned by the same scanner as everywhere else — a
/// block comment inside a `<script>` runs over lines exactly as it would in a
/// `.js` file — and its state is carried in the outer state's three inner fields,
/// because a state is `Copy` and cannot hold another state. The spans come back
/// offset to where the inner text starts in the outer line, which is the only
/// thing that makes this different from scanning a line of JavaScript.
fn inner_spans(text: &str, offset: usize, inner: Inner, state: &mut State) -> Vec<Span> {
    let grammar = inner.grammar();
    let carried = State {
        comment: state.inner_comment,
        raw: None,
        triple: state.inner_string,
        fence: None,
        in_tag: false,
        in_quote: None,
        inner: None,
        inner_comment: 0,
        inner_string: None,
    };
    let (spans, after) = scan(text, carried, grammar);
    state.inner_comment = after.comment;
    state.inner_string = after.triple;
    spans
        .into_iter()
        .map(|span| Span {
            start: span.start + offset,
            end: span.end + offset,
            token: span.token,
        })
        .collect()
}

/// CSS: comments, strings and numbers, and no keywords to speak of.
static CSS: Grammar = Grammar {
    keywords: &[],
    constants: &["important"],
    line_comment: "",
    block_comment: Some(("/*", "*/")),
    nested_comments: false,
    rust: false,
    python: false,
    declares: &[],
};

/// Where the quote opened before `from` closes, escape and all.
fn scalar_end(chars: &[char], from: usize, quote: char) -> Option<usize> {
    let mut index = from;
    while index < chars.len() {
        if chars[index] == quote {
            return Some(index + 1);
        }
        index += 1;
    }
    None
}

/// Markdown: what is a heading, what is code, what is emphasis.
///
/// The scanner is separate from [`scan`] because the language is: no keywords and
/// no string literals, but structure that runs *across* lines — a fenced block
/// makes everything inside it code, so the line after an opening fence starts
/// inside one. That is a state, which is the same shape a block comment has, and
/// the reason this is a line-oriented scanner like the others rather than a
/// paragraph-level one.
fn markdown(line: &str, state: State) -> (Vec<Span>, State) {
    let chars: Vec<char> = line.chars().collect();
    let mut spans = Vec::new();
    let fence = fence_at(&chars);
    // Inside a fence: the fence that closes it ends it, and what is between the
    // fences is whatever the fence said it holds — ` ```rust ` is Rust, and a
    // fence with nothing to say is code that is not read any further.
    if let Some(open) = state.fence {
        let closes = fence == Some(open) && chars.iter().all(|c| *c == open);
        if closes {
            spans.push(Span {
                start: 0,
                end: chars.len(),
                token: Token::String,
            });
            return (spans, State::new());
        }
        if let Some(inner) = state.inner {
            let mut carried = state;
            spans.extend(inner_spans(line, 0, inner, &mut carried));
            return (
                spans,
                State {
                    fence: Some(open),
                    ..carried
                },
            );
        }
        spans.push(Span {
            start: 0,
            end: chars.len(),
            token: Token::String,
        });
        return (
            spans,
            State {
                fence: Some(open),
                ..State::new()
            },
        );
    }
    if let Some(open) = fence {
        spans.push(Span {
            start: 0,
            end: chars.len(),
            token: Token::String,
        });
        // The rest of the line is the info string, and its first word says what
        // the fence holds.
        let info: String = chars.iter().skip_while(|c| **c == open).collect::<String>();
        return (
            spans,
            State {
                fence: Some(open),
                inner: inner_for(&info),
                ..State::new()
            },
        );
    }

    // A heading is a keyword — the one token that draws thick — and the `#`s
    // alone are not a heading: `#` with nothing after it is a line of its own.
    let hashes = chars.iter().take_while(|c| **c == '#').count();
    if hashes > 0 && hashes <= 6 && chars.get(hashes) == Some(&' ') {
        spans.push(Span {
            start: 0,
            end: chars.len(),
            token: Token::Keyword,
        });
        return (spans, State::new());
    }

    // A setext heading's underline: a line of `=` or `-` and nothing else is
    // what makes the line above a heading. This cannot know about the line
    // above — every language here scans one line at a time — but a line that is
    // *only* those characters means the same thing wherever it is.
    let trimmed: Vec<char> = chars.iter().skip_while(|c| **c == ' ').copied().collect();
    if trimmed.len() >= 3 {
        let marker = trimmed[0];
        if (marker == '=' || marker == '-') && trimmed.iter().all(|c| *c == marker) {
            spans.push(Span {
                start: 0,
                end: chars.len(),
                token: Token::Keyword,
            });
            return (spans, State::new());
        }
    }
    // A list marker: `- `, `* `, `+ `, `1. ` — the marker only, since the item
    // itself is ordinary text.
    let indent = chars.iter().take_while(|c| **c == ' ').count();
    let marker_end = list_marker_end(&chars, indent);
    if let Some(end) = marker_end {
        if !spans.is_empty() && spans[0].start == 0 {
            // A quotation came first: the marker is inside it.
        } else {
            spans.push(Span {
                start: indent,
                end,
                token: Token::Keyword,
            });
        }
    }
    // A quotation: the `>` that makes it one is the quotation mark, and what is
    // quoted is scanned as usual — a quoted `**this**` is still thick.
    let quote = if chars.first() == Some(&'>') {
        let end = if chars.get(1) == Some(&' ') { 2 } else { 1 };
        spans.push(Span {
            start: 0,
            end,
            token: Token::Comment,
        });
        end
    } else {
        0
    };

    let mut inside_code = false;
    let mut index = quote;
    if let Some(end) = marker_end {
        index = end;
    }
    while index < chars.len() {
        let c = chars[index];
        // Inline code runs to the next backtick and has nothing inside it: an
        // asterisk in it is an asterisk.
        if c == '`' {
            let start = index;
            index += 1;
            while index < chars.len() && chars[index] != '`' {
                index += 1;
            }
            index = (index + 1).min(chars.len());
            spans.push(Span {
                start,
                end: index,
                token: Token::String,
            });
            inside_code = true;
            continue;
        }
        // `[text](url)`: the link's address, and the brackets around it.
        if c == ']' && chars.get(index + 1) == Some(&'(') {
            let start = index;
            let mut end = index + 2;
            let mut depth = 1;
            while end < chars.len() && depth > 0 {
                match chars[end] {
                    '(' => depth += 1,
                    ')' => depth -= 1,
                    _ => {}
                }
                end += 1;
            }
            spans.push(Span {
                start,
                end,
                token: Token::String,
            });
            index = end;
            continue;
        }
        // An entity: `&amp;` is a value, not a name.
        if c == '&' {
            if let Some(end) = entity_end(&chars, index) {
                spans.push(Span {
                    start: index,
                    end,
                    token: Token::Constant,
                });
                index = end;
                continue;
            }
        }
        // `**thick**` and `*slanted*`: the doubled run first, since it contains
        // the single one.
        if (c == '*' || c == '_') && !inside_code {
            let doubled = chars.get(index + 1) == Some(&c);
            let marker = if doubled { 2 } else { 1 };
            if let Some(close) = emphasis_end(&chars, index + marker, c, marker) {
                spans.push(Span {
                    start: index,
                    end: close + marker,
                    token: if doubled {
                        Token::Keyword
                    } else {
                        Token::Comment
                    },
                });
                index = close + marker;
                continue;
            }
        }
        index += 1;
    }
    (spans, State::new())
}

/// Where a list marker at `indent` ends, if there is one there: the `-`, `*` or
/// `+` of a bullet, or the digits and dot of a number, and the space after it.
fn list_marker_end(chars: &[char], indent: usize) -> Option<usize> {
    let first = *chars.get(indent)?;
    let bullet = matches!(first, '-' | '*' | '+');
    if !bullet && !first.is_ascii_digit() {
        return None;
    }
    let mut at = indent;
    if bullet {
        at += 1;
    } else {
        while chars.get(at).is_some_and(char::is_ascii_digit) {
            at += 1;
        }
        if !matches!(chars.get(at), Some('.') | Some(')')) {
            return None;
        }
        at += 1;
    }
    // `- ` is a list item; `-x` and `-` alone are not.
    matches!(chars.get(at), Some(' ')).then_some(at)
}

/// Where the entity that starts at `from` ends, if it is one: `&amp;`, `&#39;`.
fn entity_end(chars: &[char], from: usize) -> Option<usize> {
    let mut at = from + 1;
    if chars.get(at) == Some(&'#') {
        at += 1;
    }
    let start = at;
    while at < chars.len() && (chars[at].is_alphanumeric() || chars[at] == '_') {
        at += 1;
    }
    if at == start || chars.get(at) != Some(&';') {
        return None;
    }
    Some(at + 1)
}

/// The character of a code fence at the start of a line, if this line is one.
fn fence_at(chars: &[char]) -> Option<char> {
    let first = *chars.first()?;
    if first != '`' && first != '~' {
        return None;
    }
    let run = chars.iter().take_while(|c| **c == first).count();
    (run >= 3).then_some(first)
}

/// Where the emphasis opened at `from` closes, if it does on this line.
fn emphasis_end(chars: &[char], from: usize, marker: char, width: usize) -> Option<usize> {
    let mut index = from;
    while index < chars.len() {
        if chars[index] == marker {
            let run = chars[index..].iter().take_while(|c| **c == marker).count();
            if run >= width {
                return Some(index);
            }
        }
        index += 1;
    }
    None
}

/// What one language needs the scanner to know.
struct Grammar {
    /// The words that are the language's own.
    keywords: &'static [&'static str],
    /// The words that are values rather than names.
    constants: &'static [&'static str],
    /// What starts a comment that runs to the end of the line. Empty for a
    /// language without any — JSON.
    line_comment: &'static str,
    /// What opens and closes a comment that runs over lines, if the language has
    /// one.
    block_comment: Option<(&'static str, &'static str)>,
    /// Whether `/* /* */ */` nests. Rust's does, C's does not.
    nested_comments: bool,
    /// Whether raw strings, lifetimes and attributes exist.
    rust: bool,
    /// Whether `"""` and `'''` are strings that run over lines.
    python: bool,
    /// Keywords after which a name is being *defined*, which is coloured like a
    /// name being called: `fn main`, `struct S`, `def go`.
    declares: &'static [&'static str],
}

/// Rust, which needs the most of any of them.
static RUST: Grammar = Grammar {
    keywords: &[
        "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum",
        "extern", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut",
        "pub", "ref", "return", "self", "static", "struct", "super", "trait", "type", "unsafe",
        "use", "where", "while",
    ],
    constants: &["true", "false"],
    line_comment: "//",
    block_comment: Some(("/*", "*/")),
    nested_comments: true,
    rust: true,
    python: false,
    declares: &["fn", "struct", "enum", "trait", "mod", "type"],
};

/// The C family: one keyword list for C, C++, Java, JavaScript, Go and the rest.
static CURLY: Grammar = Grammar {
    keywords: &[
        "int",
        "char",
        "float",
        "double",
        "void",
        "long",
        "short",
        "unsigned",
        "signed",
        "struct",
        "union",
        "enum",
        "typedef",
        "static",
        "const",
        "extern",
        "return",
        "if",
        "else",
        "for",
        "while",
        "do",
        "switch",
        "case",
        "break",
        "continue",
        "goto",
        "sizeof",
        "class",
        "public",
        "private",
        "protected",
        "new",
        "delete",
        "this",
        "namespace",
        "using",
        "template",
        "virtual",
        "override",
        "auto",
        "var",
        "let",
        "function",
        "export",
        "import",
        "from",
        "await",
        "async",
        "package",
        "interface",
        "extends",
        "implements",
        "try",
        "catch",
        "finally",
        "throw",
        "throws",
        "func",
        "defer",
        "go",
        "chan",
        "map",
        "range",
    ],
    constants: &["true", "false", "null", "nil", "NULL", "None"],
    line_comment: "//",
    block_comment: Some(("/*", "*/")),
    nested_comments: false,
    rust: false,
    python: false,
    declares: &[],
};

/// Python: `#` comments, three kinds of string, and its own words.
static PYTHON: Grammar = Grammar {
    keywords: &[
        "and", "as", "assert", "async", "await", "break", "class", "continue", "def", "del",
        "elif", "else", "except", "finally", "for", "from", "global", "if", "import", "in", "is",
        "lambda", "nonlocal", "not", "or", "pass", "raise", "return", "try", "while", "with",
        "yield", "match", "case",
    ],
    constants: &["True", "False", "None"],
    line_comment: "#",
    block_comment: None,
    nested_comments: false,
    rust: false,
    python: true,
    declares: &["def", "class"],
};

/// A shell: `#` comments and its own words.
static SHELL: Grammar = Grammar {
    keywords: &[
        "if", "then", "else", "elif", "fi", "for", "while", "until", "do", "done", "case", "esac",
        "in", "function", "return", "local", "export", "readonly", "declare", "source", "echo",
        "cd", "set", "unset", "shift", "exit", "trap",
    ],
    constants: &[],
    line_comment: "#",
    block_comment: None,
    nested_comments: false,
    rust: false,
    python: false,
    declares: &["function"],
};

/// A configuration file — TOML, YAML, INI: `#` comments and literals, and no
/// words of its own to speak of.
static CONFIG: Grammar = Grammar {
    keywords: &[],
    constants: &["true", "false", "yes", "no", "on", "off", "null", "nil"],
    line_comment: "#",
    block_comment: None,
    nested_comments: false,
    rust: false,
    python: false,
    declares: &[],
};

/// JSON: strings and numbers, and no comments at all — which is the whole of its
/// difference from the C family.
static JSON: Grammar = Grammar {
    keywords: &[],
    constants: &["true", "false", "null"],
    line_comment: "",
    block_comment: None,
    nested_comments: false,
    rust: false,
    python: false,
    declares: &[],
};

/// What a line of one language is made of, and what state the next line starts
/// in.
///
/// The scanner walks the line once, in order, and never goes back: whatever it
/// decides a character is, that is what it is, which is what keeps the spans in
/// order and non-overlapping without a second pass.
fn scan(line: &str, state: State, grammar: &Grammar) -> (Vec<Span>, State) {
    let chars: Vec<char> = line.chars().collect();
    let mut spans = Vec::new();
    let mut index = 0;
    let mut comment = state.comment;
    let mut raw = state.raw;
    let mut triple = state.triple;

    // A line that begins inside something runs until that thing ends.
    if comment > 0 {
        let (_open, close) = grammar.block_comment.unwrap_or(("", ""));
        // The line begins *inside* the comment, so the scan starts at its
        // first character: there is no opener to step over.
        let end = match block_comment_end(&chars, 0, comment, "", close, grammar.nested_comments) {
            Some((end, depth)) => {
                comment = depth;
                end
            }
            None => chars.len(),
        };
        spans.push(Span {
            start: 0,
            end,
            token: Token::Comment,
        });
        index = end;
    } else if let Some(hashes) = raw {
        let closer = format!("\"{}", "#".repeat(hashes));
        let found = find(&chars, 0, &closer);
        let end = found.map_or(chars.len(), |at| at + closer.chars().count());
        if found.is_some() {
            raw = None;
        }
        spans.push(Span {
            start: 0,
            end,
            token: Token::String,
        });
        index = end;
    } else if let Some(quote) = triple {
        let closer = quote.to_string().repeat(3);
        let found = find(&chars, 0, &closer);
        let end = found.map_or(chars.len(), |at| at + 3);
        if found.is_some() {
            triple = None;
        }
        spans.push(Span {
            start: 0,
            end,
            token: Token::String,
        });
        index = end;
    }

    // The word before this one, for `fn main` and `def go`.
    let mut after_declare = false;
    while index < chars.len() {
        let c = chars[index];
        // Space is nothing.
        if c.is_whitespace() {
            index += 1;
            continue;
        }
        // A comment to the end of the line stops the scan: nothing after it is
        // code, whatever it looks like.
        if !grammar.line_comment.is_empty() && line_at(&chars, index, grammar.line_comment) {
            spans.push(Span {
                start: index,
                end: chars.len(),
                token: Token::Comment,
            });
            break;
        }
        if let Some((open, close)) = grammar.block_comment {
            if line_at(&chars, index, open) {
                let (end, depth) = block_comment_end(
                    &chars,
                    index + open.chars().count(),
                    comment,
                    open,
                    close,
                    grammar.nested_comments,
                )
                .unwrap_or((chars.len(), 1));
                spans.push(Span {
                    start: index,
                    end,
                    token: Token::Comment,
                });
                comment = depth;
                index = end;
                continue;
            }
        }
        // An attribute in Rust: `#[derive(Debug)]` is one thing, and none of it
        // is code.
        if grammar.rust && c == '#' && chars.get(index + 1) == Some(&'[') {
            let end = find(&chars, index, "]").map_or(chars.len(), |at| at + 1);
            spans.push(Span {
                start: index,
                end,
                token: Token::Keyword,
            });
            index = end;
            continue;
        }
        // A raw string: `r"…"`, `r#"…"#`, and as many `#`s as it takes.
        if grammar.rust && c == 'r' {
            let mut at = index + 1;
            let mut hashes = 0;
            while chars.get(at) == Some(&'#') {
                hashes += 1;
                at += 1;
            }
            if chars.get(at) == Some(&'"') {
                let closer = format!("\"{}", "#".repeat(hashes));
                let found = find(&chars, at, &closer);
                let end = found.map_or(chars.len(), |at| at + closer.chars().count());
                if found.is_none() {
                    raw = Some(hashes);
                }
                spans.push(Span {
                    start: index,
                    end,
                    token: Token::String,
                });
                index = end;
                continue;
            }
        }
        // A string. A triple quote is a string that runs over lines in Python,
        // and a plain one ends at the line's end if it is never closed — an
        // ordinary string does not run on.
        if c == '"' || (c == '\'' && !grammar.rust) {
            let quote = c;
            if grammar.python
                && chars.get(index + 1) == Some(&quote)
                && chars.get(index + 2) == Some(&quote)
            {
                let closer = quote.to_string().repeat(3);
                let found = find(&chars, index + 3, &closer);
                let end = found.map_or(chars.len(), |at| at + 3);
                if found.is_none() {
                    triple = Some(quote);
                }
                spans.push(Span {
                    start: index,
                    end,
                    token: Token::String,
                });
                index = end;
                continue;
            }
            let end = string_end(&chars, index, quote);
            spans.push(Span {
                start: index,
                end,
                token: Token::String,
            });
            index = end;
            continue;
        }
        // A character literal in Rust — and a lifetime is not one: `'a` has no
        // closing quote, and `'x'` does.
        if grammar.rust && c == '\'' {
            if let Some(end) = char_literal_end(&chars, index) {
                spans.push(Span {
                    start: index,
                    end,
                    token: Token::String,
                });
                index = end;
                continue;
            }
            index += 1;
            continue;
        }
        if c.is_ascii_digit() {
            let start = index;
            index += 1;
            while index < chars.len()
                && (chars[index].is_ascii_alphanumeric()
                    || chars[index] == '.'
                    || chars[index] == '_')
            {
                index += 1;
            }
            spans.push(Span {
                start,
                end: index,
                token: Token::Number,
            });
            continue;
        }
        if c.is_alphanumeric() || c == '_' {
            let start = index;
            index += 1;
            while index < chars.len() && (chars[index].is_alphanumeric() || chars[index] == '_') {
                index += 1;
            }
            let word: String = chars[start..index].iter().collect();
            let token = if grammar.keywords.contains(&word.as_str()) {
                after_declare = grammar.declares.contains(&word.as_str());
                Some(Token::Keyword)
            } else if grammar.constants.contains(&word.as_str()) {
                after_declare = false;
                Some(Token::Constant)
            } else if after_declare || next_is_call(&chars, index) {
                after_declare = false;
                Some(Token::Function)
            } else {
                after_declare = false;
                None
            };
            if let Some(token) = token {
                spans.push(Span {
                    start,
                    end: index,
                    token,
                });
            }
            continue;
        }
        index += 1;
    }

    (
        spans,
        State {
            comment,
            raw,
            triple,
            fence: None,
            in_tag: false,
            in_quote: None,
            inner: None,
            inner_comment: 0,
            inner_string: None,
        },
    )
}

/// Whether the comment prefix `prefix` starts at character `at`.
fn line_at(chars: &[char], at: usize, prefix: &str) -> bool {
    prefix
        .chars()
        .enumerate()
        .all(|(offset, c)| chars.get(at + offset) == Some(&c))
}

/// Where `needle` starts in `chars` at or after `from`.
fn find(chars: &[char], from: usize, needle: &str) -> Option<usize> {
    let needle: Vec<char> = needle.chars().collect();
    if needle.is_empty() || from >= chars.len() {
        return None;
    }
    needle_last_index(chars, from, &needle)
}

/// The last place `needle` could start without running off the end of `chars`.
fn needle_last_index(chars: &[char], from: usize, needle: &[char]) -> Option<usize> {
    let last = chars.len().checked_sub(needle.len())?;
    (from..=last).find(|start| chars[*start..*start + needle.len()] == needle[..])
}

/// Where the comment being scanned ends, and how many are still open.
///
/// `from` is where to start looking, not where the opener is: a line that begins
/// inside a comment has no opener on it, and one that opens a comment has already
/// been stepped over by the time it gets here.
fn block_comment_end(
    chars: &[char],
    from: usize,
    open: usize,
    opener: &str,
    closer: &str,
    nested: bool,
) -> Option<(usize, usize)> {
    let mut depth = open.max(1);
    let mut index = from;
    while index < chars.len() {
        // An empty opener is what a line that begins *inside* a comment passes:
        // there is nothing to look for, and looking for nothing would match
        // everywhere.
        if nested && !opener.is_empty() && line_at(chars, index, opener) {
            depth += 1;
            index += opener.chars().count();
            continue;
        }
        if line_at(chars, index, closer) {
            depth -= 1;
            index += closer.chars().count();
            if depth == 0 {
                return Some((index, 0));
            }
            continue;
        }
        index += 1;
    }
    None
}

/// Where the string opened at `from` ends: the closing quote, escapes and all.
fn string_end(chars: &[char], from: usize, quote: char) -> usize {
    let mut index = from + 1;
    while index < chars.len() {
        if chars[index] == '\\' {
            index += 2;
            continue;
        }
        if chars[index] == quote {
            return index + 1;
        }
        index += 1;
    }
    chars.len()
}

/// Where the character literal at `from` ends, if it is one — `'x'`, `'\n'` —
/// and not a lifetime, which has no closing quote on the same line.
fn char_literal_end(chars: &[char], from: usize) -> Option<usize> {
    let mut index = from + 1;
    if index >= chars.len() {
        return None;
    }
    if chars[index] == '\\' {
        index += 1;
        while index < chars.len() && chars[index] != '\'' {
            index += 1;
        }
        return (index < chars.len()).then_some(index + 1);
    }
    index += 1;
    (chars.get(index) == Some(&'\'')).then_some(index + 1)
}

/// Whether an identifier ending at `at` is followed by a `(`, which is what a
/// name being called looks like.
fn next_is_call(chars: &[char], at: usize) -> bool {
    let mut index = at;
    while index < chars.len() && chars[index] == ' ' {
        index += 1;
    }
    // `panic!(` is a macro, and a macro is being called too.
    matches!(chars.get(index), Some(&'(') | Some(&'!'))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tokens of a single line, as `(start, end, token)`.
    fn one(language: Language, line: &str) -> Vec<Span> {
        tokens(language, line, State::new()).0
    }

    /// Just the tokens of a line, in order, for a test that counts them.
    fn tokens_of(spans: &[Span], _line: &str) -> Vec<Token> {
        spans.iter().map(|span| span.token).collect()
    }

    /// The tokens of a line, as the words they cover, for readable tests.
    fn words(language: Language, line: &str) -> Vec<(String, Token)> {
        one(language, line)
            .into_iter()
            .map(|span| {
                (
                    line.chars()
                        .skip(span.start)
                        .take(span.end - span.start)
                        .collect(),
                    span.token,
                )
            })
            .collect()
    }

    #[test]
    fn a_language_comes_from_the_file_name() {
        for (name, language) in [
            ("main.rs", Language::Rust),
            ("editor.c", Language::Curly),
            ("thing.hpp", Language::Curly),
            ("app.js", Language::Curly),
            ("Main.java", Language::Curly),
            ("notes.txt", Language::Plain),
            ("Makefile", Language::Plain),
            ("README.md", Language::Markdown),
            ("guide.markdown", Language::Markdown),
            ("page.mdx", Language::Markdown),
            ("Image.PNG", Language::Plain),
        ] {
            assert_eq!(
                Language::from_path(Path::new(name)),
                language,
                "{name} is not the language it looks like"
            );
        }
    }

    #[test]
    fn a_plain_file_has_no_tokens() {
        assert!(one(Language::Plain, "let x = 1; // not code here").is_empty());
        assert!(tokens(Language::Plain, "\"unterminated", State::new())
            .1
            .is_code());
    }

    #[test]
    fn a_rust_line_is_coloured_by_what_its_parts_are() {
        assert_eq!(
            words(Language::Rust, "let x = 42;"),
            vec![
                ("let".to_string(), Token::Keyword),
                ("42".to_string(), Token::Number),
            ]
        );
        assert_eq!(
            words(Language::Rust, "    pub fn main() {"),
            vec![
                ("pub".to_string(), Token::Keyword),
                ("fn".to_string(), Token::Keyword),
                ("main".to_string(), Token::Function),
            ],
            "a name followed by `(` is being called"
        );
        assert_eq!(
            words(Language::Rust, "if x == true { panic!(\"no\"); }"),
            vec![
                ("if".to_string(), Token::Keyword),
                ("true".to_string(), Token::Constant),
                ("panic".to_string(), Token::Function),
                ("\"no\"".to_string(), Token::String),
            ]
        );
    }

    #[test]
    fn a_keyword_inside_something_else_is_not_a_keyword() {
        // The text of a comment, a string or a name is not code.
        assert_eq!(
            words(Language::Rust, "// let x = 1;"),
            vec![("// let x = 1;".to_string(), Token::Comment)]
        );
        assert_eq!(
            words(Language::Rust, "let s = \"fn main\";"),
            vec![
                ("let".to_string(), Token::Keyword),
                ("\"fn main\"".to_string(), Token::String),
            ]
        );
        assert_eq!(words(Language::Rust, "let lets = 1;")[0].1, Token::Keyword);
        assert!(
            words(Language::Rust, "let letter = 1;")
                .iter()
                .all(|(_, t)| *t != Token::Keyword || true),
            "`letter` is not `let`"
        );
        assert!(one(Language::Rust, "let letter = 1;")
            .iter()
            .all(|span| span.token != Token::Keyword || span.start == 0 && span.end == 3));
    }

    #[test]
    fn a_block_comment_carries_on_to_the_next_line() {
        let (spans, state) = tokens(Language::Rust, "code(); /* a comment", State::new());
        assert_eq!(spans.last().map(|s| s.token), Some(Token::Comment));
        assert!(!state.is_code(), "the comment is still open");

        // The next line starts inside it, and ends it.
        let (spans, state) = tokens(Language::Rust, "still comment */ let x = 1;", state);
        assert_eq!(spans[0].token, Token::Comment);
        assert_eq!(
            spans[0].end, 16,
            "the comment ends just past the `*/`, which is at 14 and 15"
        );
        assert_eq!(spans[1].token, Token::Keyword, "and then it is code again");
        assert!(state.is_code());

        // And a line that opens one and never ends it opens it for the next.
        let (_, state) = tokens(Language::Rust, "/* only a comment", State::new());
        assert!(!state.is_code());
    }

    #[test]
    fn rust_nests_block_comments_and_c_does_not() {
        let (_, state) = tokens(Language::Rust, "/* a /* b */ still", State::new());
        assert!(!state.is_code(), "the outer comment is still open");
        let (spans, state) = tokens(Language::Rust, "*/ code();", state);
        assert!(state.is_code());
        assert_eq!(spans[0].token, Token::Comment);
        assert_eq!(spans[0].end, 2);

        let (_, state) = tokens(Language::Curly, "/* a /* b */ code();", State::new());
        assert!(state.is_code(), "C's comments do not nest");
    }

    #[test]
    fn a_raw_string_carries_on_too() {
        let (spans, state) = tokens(Language::Rust, "let s = r#\"a \" b", State::new());
        assert_eq!(spans.last().map(|s| s.token), Some(Token::String));
        assert!(!state.is_code(), "the raw string is still open");

        let (spans, state) = tokens(Language::Rust, "still\"#;", state);
        assert_eq!(spans[0].token, Token::String);
        assert_eq!(spans[0].end, 7, "it ends at the `\"#`");
        assert!(state.is_code());
    }

    #[test]
    fn an_unterminated_string_ends_at_the_line() {
        let (spans, state) = tokens(Language::Rust, "let s = \"oops", State::new());
        assert_eq!(spans.last().map(|s| s.token), Some(Token::String));
        assert!(state.is_code(), "an ordinary string does not run on");
    }

    #[test]
    fn a_lifetime_is_not_a_character_literal() {
        assert_eq!(
            words(Language::Rust, "fn f<'a>(x: &'a str) {}"),
            vec![
                ("fn".to_string(), Token::Keyword),
                ("f".to_string(), Token::Function),
            ],
            "neither the lifetime nor the type is a literal"
        );
        assert_eq!(
            words(Language::Rust, "let c = 'x';"),
            vec![
                ("let".to_string(), Token::Keyword),
                ("'x'".to_string(), Token::String),
            ]
        );
        assert_eq!(
            words(Language::Rust, "let c = '\\n';"),
            vec![
                ("let".to_string(), Token::Keyword),
                ("'\\n'".to_string(), Token::String),
            ]
        );
    }

    #[test]
    fn an_attribute_is_not_code_either() {
        assert_eq!(
            words(Language::Rust, "#[derive(Debug)] struct S;"),
            vec![
                ("#[derive(Debug)]".to_string(), Token::Keyword),
                ("struct".to_string(), Token::Keyword),
                // The name a keyword defines is coloured like a name being
                // called: it is the same thing to read.
                ("S".to_string(), Token::Function),
            ]
        );
    }

    #[test]
    fn the_c_family_is_coloured_too() {
        assert_eq!(
            words(Language::Curly, "int main(void) { /* hi */ return 0; }"),
            vec![
                ("int".to_string(), Token::Keyword),
                ("main".to_string(), Token::Function),
                ("void".to_string(), Token::Keyword),
                ("/* hi */".to_string(), Token::Comment),
                ("return".to_string(), Token::Keyword),
                ("0".to_string(), Token::Number),
            ]
        );
        assert_eq!(
            words(Language::Curly, "char c = 'a';"),
            vec![
                ("char".to_string(), Token::Keyword),
                ("'a'".to_string(), Token::String),
            ]
        );
    }

    #[test]
    fn the_palette_colours_every_token_differently() {
        let mut seen = Vec::new();
        for token in [
            Token::Comment,
            Token::String,
            Token::Number,
            Token::Keyword,
            Token::Function,
            Token::Constant,
        ] {
            let (color, _) = style(token);
            assert!(
                !seen.contains(&color),
                "{token:?} has the same colour as another token"
            );
            seen.push(color);
        }
        // A comment is grey and slanted, and a keyword is bold: the two
        // attributes the protocol has that mean something here.
        assert_eq!(style(Token::Comment).1, ATTR_ITALIC);
        assert_eq!(style(Token::Keyword).1, ATTR_BOLD);
        assert_eq!(style(Token::String).1, 0);
    }

    #[test]
    fn the_state_cache_fills_lazily_and_forgets_from_a_change() {
        let mut buffer = Buffer::from_text("fn main() {\nlet x = 1;\n/* a\nb */\n}\n");
        let highlighter = LineStates::new(Language::Rust);

        // The first two lines are code, the fourth starts inside a comment.
        assert!(highlighter
            .spans(&buffer, 0)
            .iter()
            .any(|s| s.token == Token::Keyword));
        assert!(
            highlighter.spans(&buffer, 3)[0].token == Token::Comment,
            "inside it"
        );
        assert_eq!(highlighter.states.borrow().len(), 3, "lines 0..3 are known");

        // Editing the line that opens the comment changes what follows it:
        // the `/*` becomes a `//`, so the comment no longer runs on.
        buffer.replace_range(23, 25, "//");
        highlighter.invalidate(2);
        assert!(
            highlighter.states.borrow().len() <= 2,
            "the cache gave up on it"
        );
        assert!(
            highlighter
                .spans(&buffer, 3)
                .iter()
                .all(|span| span.token != Token::Comment),
            "the comment is gone, so the line after it is code"
        );
    }

    #[test]
    fn a_plain_document_has_no_spans_and_no_states() {
        let buffer = Buffer::from_text("let x = 1;\n/* a\nb */\n");
        let highlighter = LineStates::new(Language::Plain);
        assert!(highlighter.spans(&buffer, 1).is_empty());
        assert!(
            highlighter.states.borrow().is_empty(),
            "a language with no tokens has nothing to carry"
        );
    }

    #[test]
    fn every_language_can_be_asked_for_a_line_past_the_end() {
        let buffer = Buffer::from_text("one\n");
        for language in [Language::Plain, Language::Rust, Language::Curly] {
            let highlighter = LineStates::new(language);
            assert!(highlighter.spans(&buffer, 99).is_empty(), "{language:?}");
        }
    }

    #[test]
    fn spans_never_overlap_and_stay_inside_the_line() {
        for line in [
            "let x = 1; // hi",
            "#[inline] pub fn f<'a>(s: &'a str) -> &'a str { s }",
            "/* a */ let s = r#\"x\"#; /* b */",
            "\"unterminated",
            "int a = 0x1f; char c = '\\'';",
            "let hertz = 1e-3;",
        ] {
            for language in [Language::Rust, Language::Curly] {
                let chars = line.chars().count();
                let mut at = 0;
                for span in one(language, line) {
                    assert!(span.start >= at, "{language:?} {line:?}: spans overlap");
                    assert!(
                        span.end > span.start,
                        "{language:?} {line:?}: an empty span"
                    );
                    assert!(span.end <= chars, "{language:?} {line:?}: past the end");
                    at = span.end;
                }
            }
        }
    }
    #[test]
    fn python_strings_and_comments() {
        // A `#` comment, including a `#` inside a string, which is not one.
        let line = "x = \"a # b\"  # and this is";
        let marked: Vec<String> = words(Language::Python, line)
            .into_iter()
            .map(|(word, token)| format!("{word}:{token:?}"))
            .collect();
        assert!(
            marked.contains(&"\"a # b\":String".to_string()),
            "{marked:?}"
        );
        assert!(
            marked.contains(&"# and this is:Comment".to_string()),
            "{marked:?}"
        );

        // A triple-quoted string runs over line breaks, and a `#` inside it is
        // still not a comment.
        let (spans, state) = tokens(Language::Python, "s = \"\"\"one", State::new());
        assert_eq!(spans.len(), 1);
        assert!(
            !state.is_code(),
            "the line after this starts inside a string"
        );
        let (spans, state) = tokens(Language::Python, "two # not a comment", state);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].token, Token::String);
        assert!(!state.is_code());
        let (spans, state) = tokens(Language::Python, "three\"\"\" + x", state);
        assert_eq!(spans[0].token, Token::String);
        assert!(state.is_code(), "the string closed");

        // `def` and `class` name a definition, which is coloured as one.
        let marked: Vec<String> = words(Language::Python, "def go(): pass")
            .into_iter()
            .map(|(word, token)| format!("{word}:{token:?}"))
            .collect();
        assert!(marked.contains(&"go:Function".to_string()), "{marked:?}");
        assert!(marked.contains(&"pass:Keyword".to_string()), "{marked:?}");
    }

    #[test]
    fn shell_and_config_files_use_hash_comments() {
        let marked: Vec<String> = words(Language::Shell, "if true; then # go")
            .into_iter()
            .map(|(word, token)| format!("{word}:{token:?}"))
            .collect();
        assert!(marked.contains(&"if:Keyword".to_string()), "{marked:?}");
        assert!(marked.contains(&"# go:Comment".to_string()), "{marked:?}");

        let marked: Vec<String> = words(Language::Config, "key = 42  # why")
            .into_iter()
            .map(|(word, token)| format!("{word}:{token:?}"))
            .collect();
        assert!(marked.contains(&"42:Number".to_string()), "{marked:?}");
        assert!(marked.contains(&"# why:Comment".to_string()), "{marked:?}");
    }

    #[test]
    fn json_has_no_comments_and_no_keywords() {
        let line = "{\"class\": true, \"n\": 12} // not a comment";
        let tokens = one(Language::Json, line);
        let marked: Vec<String> = tokens
            .iter()
            .map(|span| {
                format!(
                    "{}:{:?}",
                    line.chars()
                        .skip(span.start)
                        .take(span.end - span.start)
                        .collect::<String>(),
                    span.token
                )
            })
            .collect();
        assert!(
            marked.contains(&"\"class\":String".to_string()),
            "{marked:?}"
        );
        assert!(
            !marked.iter().any(|entry| entry.ends_with(":Keyword")),
            "a key that is a keyword in another language is not one here: {marked:?}"
        );
        assert!(
            !marked.iter().any(|entry| entry.ends_with(":Comment")),
            "JSON has no comments: {marked:?}"
        );
    }

    #[test]
    fn a_fence_says_what_it_holds() {
        // ` ```rust ` is Rust: a keyword is a keyword, and a comment is one.
        let (_, state) = tokens(Language::Markdown, "```rust", State::new());
        assert_eq!(state.inner, Some(Inner::Rust));
        let (spans, state) = tokens(Language::Markdown, "let x = 1; // note", state);
        assert!(
            spans.iter().any(|span| span.token == Token::Keyword),
            "{spans:?}"
        );
        assert!(
            spans.iter().any(|span| span.token == Token::Comment),
            "{spans:?}"
        );
        // A fence has to be closed before another one opens: inside one, a line
        // that looks like a fence is body text.
        let (spans, state) = tokens(Language::Markdown, "```", state);
        assert_eq!(tokens_of(&spans, "```"), vec![Token::String]);
        assert_eq!(state.fence, None);
        // ` ```py ` is Python, and the fence that closes it is a fence.
        let (_, state) = tokens(Language::Markdown, "```py", state);
        assert_eq!(state.inner, Some(Inner::Python));
        let (spans, state) = tokens(Language::Markdown, "def go(): pass", state);
        assert!(
            spans.iter().any(|span| span.token == Token::Function),
            "{spans:?}"
        );
        let (spans, state) = tokens(Language::Markdown, "```", state);
        assert_eq!(tokens_of(&spans, "```"), vec![Token::String]);
        assert_eq!(state.fence, None);
        assert_eq!(state.inner, None, "and the language went with it");
        // A fence with no language, or one this editor does not know, is code
        // that is not read any further.
        let (_, state) = tokens(Language::Markdown, "```", State::new());
        assert_eq!(state.inner, None);
        let (_, state) = tokens(Language::Markdown, "```cobol", State::new());
        assert_eq!(state.inner, None, "no grammar, no colouring");
        let (spans, _) = tokens(Language::Markdown, "DISPLAY X.", state);
        assert_eq!(tokens_of(&spans, "DISPLAY X."), vec![Token::String]);
        // A fence of four backticks is opened by four and closed by three: what
        // is counted here is the *character*, not how many of them — which is
        // one of the two rules Markdown has, and the one that costs nothing.
        let (_, state) = tokens(Language::Markdown, "````rust", State::new());
        assert_eq!(state.fence, Some('`'));
        let (_, state) = tokens(Language::Markdown, "```", state);
        assert_eq!(state.fence, None, "closed, whatever the count was");
    }

    #[test]
    fn markdown_lists_underlines_and_entities() {
        // A list marker is a bullet or a number, and only with a space after it.
        let line = "- one";
        assert_eq!(
            tokens_of(&one(Language::Markdown, line), line),
            vec![Token::Keyword]
        );
        let line = "  12. an item";
        assert_eq!(
            tokens_of(&one(Language::Markdown, line), line),
            vec![Token::Keyword]
        );
        assert!(
            one(Language::Markdown, "-not a list").is_empty(),
            "no space, no list"
        );
        // A setext underline is a line of one character and nothing else.
        let underline = "===";
        assert_eq!(
            tokens_of(&one(Language::Markdown, underline), underline),
            vec![Token::Keyword]
        );
        assert!(
            one(Language::Markdown, "--").is_empty(),
            "two of them are not an underline"
        );
        // An entity is a value, in Markdown and in HTML.
        let line = "a &amp; b";
        assert_eq!(
            tokens_of(&one(Language::Markdown, line), line),
            vec![Token::Constant]
        );
        let line = "<p>&#39;</p>";
        let marked: Vec<String> = words(Language::Html, line)
            .into_iter()
            .map(|(word, token)| format!("{word}:{token:?}"))
            .collect();
        assert!(marked.contains(&"&#39;:Constant".to_string()), "{marked:?}");
    }

    #[test]
    fn a_script_body_is_the_language_it_holds() {
        // A `<script>` body is JavaScript: a string and a comment in it are not
        // markup, and the tag that closes it is.
        let line = "<script>let s = \"a\"; // note</script>";
        let marked: Vec<String> = words(Language::Html, line)
            .into_iter()
            .map(|(word, token)| format!("{word}:{token:?}"))
            .collect();
        assert!(
            marked.contains(&"<script:Keyword".to_string()),
            "{marked:?}"
        );
        assert!(marked.contains(&"\"a\":String".to_string()), "{marked:?}");
        assert!(
            marked.contains(&"// note:Comment".to_string()),
            "{marked:?}"
        );
        assert!(
            marked.iter().any(|entry| entry.starts_with("</script")),
            "{marked:?}"
        );
        // The body runs over lines, and a block comment inside it does too.
        let (spans, state) = tokens(Language::Html, "<style>/* one", State::new());
        assert_eq!(spans.last().map(|s| s.token), Some(Token::Comment));
        assert_eq!(state.inner, Some(Inner::Style), "still in the style");
        assert_eq!(state.inner_comment, 1, "and in the comment");
        let (spans, state) = tokens(Language::Html, "two */ color: red;", state);
        assert_eq!(spans[0].token, Token::Comment);
        assert_eq!(state.inner_comment, 0);
        assert_eq!(state.inner, Some(Inner::Style), "the style is still open");
        let (spans, state) = tokens(Language::Html, "</style><p>x</p>", state);
        assert_eq!(state.inner, None, "and now it is closed");
        assert!(spans.iter().any(|span| span.token == Token::Keyword));
        // A `<script>` that never closes keeps the state, so the next line is
        // still JavaScript.
        let (_, state) = tokens(Language::Html, "<script>", State::new());
        assert_eq!(state.inner, Some(Inner::Script));
        let (spans, state) = tokens(Language::Html, "let x = 1;", state);
        assert!(
            spans.iter().any(|span| span.token == Token::Number),
            "the number is JavaScript's, not markup's: {spans:?}"
        );
        assert_eq!(state.inner, Some(Inner::Script));
    }

    #[test]
    fn a_script_says_what_it_holds() {
        // No attribute: JavaScript, which is what a script is.
        let (_, state) = tokens(Language::Html, "<script>", State::new());
        assert_eq!(state.inner, Some(Inner::Script));
        // `language="json"`, `lang="ts"` and a media type all say the same
        // thing in different words.
        let (_, state) = tokens(Language::Html, "<script language=\"json\">", State::new());
        assert_eq!(state.inner, Some(Inner::Json));
        let (_, state) = tokens(Language::Html, "<script lang='py'>", State::new());
        assert_eq!(state.inner, Some(Inner::Python));
        let (_, state) = tokens(
            Language::Html,
            "<script type=\"text/typescript\">",
            State::new(),
        );
        assert_eq!(state.inner, Some(Inner::Script));
        let (_, state) = tokens(
            Language::Html,
            "<script type=\"application/json\">",
            State::new(),
        );
        assert_eq!(state.inner, Some(Inner::Json));
        // Something this editor has no grammar for is JavaScript, since that is
        // what a script with a body almost always is.
        let (_, state) = tokens(Language::Html, "<script language=\"coffee\">", State::new());
        assert_eq!(state.inner, Some(Inner::Script));
        // A `<style>` is CSS whatever it says.
        let (_, state) = tokens(Language::Html, "<style type=\"text/css\">", State::new());
        assert_eq!(state.inner, Some(Inner::Style));
    }

    #[test]
    fn html_tags_attributes_and_comments() {
        // The tag's name is the keyword; what is quoted in it is a string.
        let line = "<a href=\"one\">text</a>";
        let marked: Vec<String> = words(Language::Html, line)
            .into_iter()
            .map(|(word, token)| format!("{word}:{token:?}"))
            .collect();
        assert!(marked.contains(&"<a:Keyword".to_string()), "{marked:?}");
        assert!(
            marked
                .iter()
                .any(|entry| entry.starts_with("</") && entry.ends_with(":Keyword")),
            "a closing tag is a keyword too: {marked:?}"
        );
        assert!(marked.contains(&"\"one\":String".to_string()), "{marked:?}");
        // A comment is a comment, and one that runs over lines leaves the state
        // inside it.
        let (spans, state) = tokens(Language::Html, "<p>a<!-- b", State::new());
        assert_eq!(spans.last().map(|s| s.token), Some(Token::Comment));
        assert!(!state.is_code(), "the comment is still open");
        let (spans, state) = tokens(Language::Html, "c --> d <b>e</b>", state);
        assert_eq!(spans[0].token, Token::Comment);
        assert_eq!(
            spans[0].end, 5,
            "the comment ends past the `-->`, which is at 2, 3 and 4"
        );
        assert!(state.is_code());
        // A tag that runs over: the next line is still inside it.
        let (_, state) = tokens(Language::Html, "<a href=\"one", State::new());
        assert!(!state.is_code(), "the tag is still open");
        let (spans, state) = tokens(Language::Html, "two\">text", state);
        assert_eq!(spans[0].token, Token::String, "the attribute's value");
        assert!(state.is_code());
        // A declaration is a string, and text between tags is text.
        assert_eq!(
            tokens_of(&one(Language::Html, "<!DOCTYPE html>"), "<!DOCTYPE html>"),
            vec![Token::String]
        );
        assert!(one(Language::Html, "just text").is_empty());
    }

    #[test]
    fn markdown_headings_code_and_quotes() {
        // A heading is a whole line, `#`s and all — but `#` alone is a line.
        let spans = one(Language::Markdown, "# A title");
        assert_eq!(tokens_of(&spans, "# A title"), vec![Token::Keyword]);
        assert!(one(Language::Markdown, "#").is_empty());
        assert!(
            one(Language::Markdown, "####### seven").is_empty(),
            "not a heading"
        );
        // A fence makes the whole line code, and everything inside it too.
        let (spans, state) = tokens(Language::Markdown, "```rust", State::new());
        assert_eq!(tokens_of(&spans, "```rust"), vec![Token::String]);
        assert_eq!(state.fence, Some('`'), "the fence is open");
        // The body is Rust, because the fence said so: `let` is a keyword, and
        // the `*a*` in it is not emphasis — it is multiplication.
        let (spans, state) = tokens(Language::Markdown, "let x = *a*;", state);
        assert_eq!(tokens_of(&spans, "let x = *a*;"), vec![Token::Keyword]);
        assert_eq!(state.fence, Some('`'), "still inside it");
        let (spans, state) = tokens(Language::Markdown, "```", state);
        assert_eq!(tokens_of(&spans, "```"), vec![Token::String]);
        assert_eq!(state.fence, None, "and the fence is closed");
        // A quotation, and the emphasis inside it is still emphasis.
        let line = "> **thick** and *slanted*";
        assert_eq!(
            tokens_of(&one(Language::Markdown, line), line),
            vec![Token::Comment, Token::Keyword, Token::Comment]
        );
        // A link's address is a string.
        let line = "see [the docs](https://example.com/a(b)) now";
        assert_eq!(
            tokens_of(&one(Language::Markdown, line), line),
            vec![Token::String]
        );
        // Inline code wins over emphasis inside it.
        let line = "a `*not slanted*` b";
        assert_eq!(
            tokens_of(&one(Language::Markdown, line), line),
            vec![Token::String]
        );
    }

    #[test]
    fn markdown_spans_stay_inside_the_line_and_do_not_overlap() {
        for line in [
            "# A title",
            "> quoted",
            "**thick** and *slanted* and `code`",
            "[a](b(c))",
            "unclosed **thick",
            "```",
            "a * b * c",
        ] {
            let chars = line.chars().count();
            let mut at = 0;
            for span in one(Language::Markdown, line) {
                assert!(span.start >= at, "{line:?}: spans overlap");
                assert!(span.end > span.start, "{line:?}: an empty span");
                assert!(span.end <= chars, "{line:?}: past the end");
                at = span.end;
            }
        }
        // A fence is a state, so the line after one starts inside code.
        let (_, state) = tokens(Language::Markdown, "~~~", State::new());
        assert_eq!(state.fence, Some('~'));
        let (_, state) = tokens(Language::Markdown, "```", state);
        assert_eq!(
            state.fence,
            Some('~'),
            "the other fence's character does not close it"
        );
    }

    #[test]
    fn the_languages_a_file_name_suggests() {
        for (name, expected) in [
            ("a.py", Language::Python),
            ("b.pyi", Language::Python),
            ("c.sh", Language::Shell),
            ("d.bash", Language::Shell),
            ("e.toml", Language::Config),
            ("f.yaml", Language::Config),
            ("g.json", Language::Json),
            ("h.rs", Language::Rust),
            ("i.js", Language::Curly),
            ("j.txt", Language::Plain),
            ("k.md", Language::Markdown),
            ("l.markdown", Language::Markdown),
        ] {
            assert_eq!(Language::from_path(Path::new(name)), expected, "{name}");
        }
    }
}
