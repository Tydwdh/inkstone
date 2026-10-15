//! `:s` — replacing what a pattern finds.
//!
//! # The shape of the line
//!
//! ```text
//! [%|.]s<delim>pattern<delim>replacement<delim>[gi]
//! ```
//!
//! `%` means the whole file, `.` (or nothing) the current line, and `<delim>` is
//! any character that is not a letter or a digit — `/` unless the pattern has a
//! slash in it, which is what the delimiter is for. The last delimiter may be
//! left off, and so may the replacement: `:s/pat` deletes what it finds, which is
//! vim's shorthand for an empty replacement.
//!
//! # Two dialects, one conversation
//!
//! The *pattern* is typed in vim's dialect, the same one `/` searches with, and
//! is translated into the engine's by `dialect` — which is also what
//! says where a match begins and ends when `\zs` and `\ze` are in it, since what
//! a substitute replaces is the part of a match the pattern *reports* rather
//! than the text the engine matched. The *replacement* is written the way vim
//! writes one — `&` for the whole match, `\1` for a group — and is translated
//! here into the engine's `$` forms, so a person who has typed
//! `:%s/foo/&bar/` before can type it again. A `$` a user writes is a literal
//! `$`, unless it is followed by a digit or a brace, which is the one case where
//! the engine's own spelling is more useful than vim's.

/// One end of a line range: vim's addresses, as far as `:s` uses them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Address {
    /// A line number, counted from one.
    Line(usize),
    /// The line the cursor is on.
    Current,
    /// The last line of the file.
    Last,
    /// The line a mark was put on: `:'a`. The mark is looked up when the range
    /// is resolved, since parsing a `:` line has no editor to look it up in.
    Mark(char),
}

/// A `:s` line, parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Substitute {
    /// The pattern as it was typed, which is what a message shows — a search
    /// keeps its own the same way.
    pub(crate) pattern: String,
    /// The same pattern in the engine's dialect, which is what is compiled.
    pub(crate) translated: String,
    /// What to put in its place, translated for the engine.
    pub(crate) replacement: String,
    /// Where `\zs` and `\ze` put the ends of a match, if the pattern has any:
    /// what a substitute replaces is the part the pattern reports, not the
    /// text the engine matched.
    pub(crate) extent: crate::dialect::Extent,
    /// Whether every match on a line is replaced, or only the first.
    pub all: bool,
    /// The first line to look at, if one was named.
    pub(crate) from: Option<Address>,
    /// The last line to look at, if one was named.
    pub(crate) to: Option<Address>,
    /// Whether this substitute ignores case.
    pub(crate) ignore_case: bool,
    /// Whether every match is asked about before it is replaced.
    pub(crate) confirm: bool,
}

/// Why a `:s` line did not parse.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SubstituteError {
    /// A bare `s` with nothing to replace.
    #[error("E471: Argument required")]
    Argument,
    /// The delimiter is not a delimiter.
    #[error("E488: Trailing characters: {0}")]
    Delimiter(char),
    /// A flag that is not one.
    #[error("E488: Trailing characters: {0}")]
    Flag(char),
    /// A pattern that asked for something this cannot do. The `E54` belongs in
    /// the text here: a `:` line prints the error as it is, while a failed
    /// search adds the same code in front of the engine's own explanation.
    #[error("E54: {0}")]
    Dialect(crate::dialect::DialectError),
}

impl Substitute {
    /// Whether a `:` line is a substitute rather than a command.
    ///
    /// It is one when it is an address, an `s`, and a delimiter — so `s/foo/bar/`
    /// is a substitute while `set` is a command name that happens to start with
    /// an `s`.
    pub(crate) fn looks_like(line: &str) -> bool {
        let Some(rest) = strip_range(line) else {
            return false;
        };
        let Some(rest) = rest.strip_prefix('s') else {
            return false;
        };
        match rest.chars().next() {
            Some(c) => !c.is_alphanumeric() && !c.is_whitespace(),
            None => false,
        }
    }

    /// Parse a `:` line that `Substitute::looks_like` accepted.
    ///
    /// `ignorecase` and `smartcase` are the settings: vim's `'ignorecase'` is
    /// used for `:s` as much as for `/`, and `'smartcase'` takes it back for a
    /// pattern with an upper case letter in it. The `i` and `I` flags settle it
    /// either way, and a `\c` or `\C` in the pattern settles it last, being the
    /// most specific thing said.
    pub fn parse(line: &str, ignorecase: bool, smartcase: bool) -> Result<Self, SubstituteError> {
        let rest = strip_range(line).ok_or(SubstituteError::Argument)?;
        let (from, to) = address_range(line);
        let Some(rest) = rest.strip_prefix('s') else {
            return Err(SubstituteError::Argument);
        };
        let Some(delimiter) = rest.chars().next() else {
            return Err(SubstituteError::Argument);
        };
        if delimiter.is_alphanumeric() || delimiter == '\\' {
            return Err(SubstituteError::Delimiter(delimiter));
        }

        let parts = split(&rest[delimiter.len_utf8()..], delimiter);
        let pattern = parts.first().cloned().unwrap_or_default();
        let replacement = parts.get(1).cloned().unwrap_or_default();
        let flags = parts.get(2).cloned().unwrap_or_default();
        if parts.len() > 3 {
            return Err(SubstituteError::Flag(delimiter));
        }

        let mut all = false;
        let mut forced: Option<bool> = None;
        let mut confirm = false;
        for flag in flags.chars() {
            match flag {
                'g' => all = true,
                'i' => forced = Some(true),
                // `:s/one/two/I` is vim's way of saying "case matters here" when
                // `'ignorecase'` is on.
                'I' => forced = Some(false),
                // `c` asks before each match. It is the one flag that cannot be
                // carried out inside one command: the answers are keys.
                'c' => confirm = true,
                other => return Err(SubstituteError::Flag(other)),
            }
        }

        // The pattern is typed the vim way and compiled the engine's way, and
        // `\c` in it decides case the way the `i` flag does — with the pattern
        // winning, since it is the more specific of the two.
        let translated = crate::dialect::translate(&pattern).map_err(SubstituteError::Dialect)?;
        let mut ignore_case = forced
            .unwrap_or_else(|| crate::search::ignore_case_for(&pattern, ignorecase, smartcase));
        if let Some(forced) = translated.ignore_case {
            ignore_case = forced;
        }
        Ok(Self {
            pattern: pattern.clone(),
            translated: translated.pattern,
            replacement: translate(&replacement, &translated.extent),
            extent: translated.extent,
            all,
            from,
            to,
            ignore_case,
            confirm,
        })
    }
}

/// What a `:` line says before its `s`: `%` for the whole file, an address, or a
/// pair of them. `None` when what comes first is not an address at all, which is
/// how `:set` and `:substitute` are told apart from `:s/x/y/`.
pub(crate) fn strip_range(line: &str) -> Option<&str> {
    if let Some(rest) = line.strip_prefix('%') {
        return Some(rest);
    }
    // `s/…/…/` with no address at all is the common case and the one the loop
    // below cannot see: `s` is not an address, and neither is the text after it.
    if starts_substitute(line) {
        return Some(line);
    }
    let mut rest = line;
    for _ in 0..2 {
        let (address, tail) = strip_address(rest)?;
        let _ = address;
        rest = tail;
        match rest.strip_prefix(',') {
            Some(tail) => rest = tail,
            None => break,
        }
    }
    Some(rest)
}

/// Whether the line is a substitute command: an `s` and then a delimiter that
/// is not a letter, which is what tells it from a command whose name starts with
/// one.
fn starts_substitute(line: &str) -> bool {
    match line.strip_prefix('s').and_then(|rest| rest.chars().next()) {
        Some(c) => !c.is_alphanumeric() && !c.is_whitespace(),
        None => false,
    }
}

/// One address, and what follows it. `None` when there is none.
fn strip_address(line: &str) -> Option<(Address, &str)> {
    let first = line.chars().next()?;
    match first {
        '.' => Some((Address::Current, &line[1..])),
        '$' => Some((Address::Last, &line[1..])),
        c if c.is_ascii_digit() => {
            let digits: String = line.chars().take_while(char::is_ascii_digit).collect();
            let number = digits.parse::<usize>().ok()?;
            Some((Address::Line(number), &line[digits.len()..]))
        }
        // `'a` is the line the mark `a` is on. Vim also writes a mark in a range
        // with a backtick, where the character and the line are the same thing;
        // here either spelling is the line, because a range is lines.
        '\'' | '`' => {
            let mut chars = line.chars();
            chars.next();
            let letter = chars.next()?;
            // `''` and `` `` `` are the last jump, which is not a mark: it is left
            // for the caller, which is the only thing that knows where it was.
            if letter == '\'' || letter == '`' {
                return None;
            }
            letter
                .is_ascii_lowercase()
                .then(|| (Address::Mark(letter), &line[1 + letter.len_utf8()..]))
        }
        _ => None,
    }
}

/// The pair of addresses a `:` line names, if it names any.
pub(crate) fn address_range(line: &str) -> (Option<Address>, Option<Address>) {
    if line.starts_with('%') {
        return (Some(Address::Line(1)), Some(Address::Last));
    }
    let mut rest = line;
    let Some((first, tail)) = strip_address(rest) else {
        return (None, None);
    };
    rest = tail;
    match rest.strip_prefix(',') {
        Some(tail) => match strip_address(tail) {
            Some((second, _)) => (Some(first), Some(second)),
            None => (Some(first), None),
        },
        None => (Some(first), Some(first)),
    }
}

/// Split on `delimiter`, honouring `\` escapes, into at most three parts.
///
/// The delimiter itself is unescaped — `\/` in a pattern is a slash — and every
/// other escape is left exactly as it was, because the pattern's dialect has its
/// own meaning for a backslash and this is not the place to decide it.
fn split(text: &str, delimiter: char) -> Vec<String> {
    let mut parts: Vec<String> = vec![String::new()];
    let mut escaped = false;
    for c in text.chars() {
        if escaped {
            if c != delimiter {
                parts.last_mut().expect("there is always a part").push('\\');
            }
            parts.last_mut().expect("there is always a part").push(c);
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else if c == delimiter {
            if parts.len() == 3 {
                // A fourth part is trailing characters; keep them together so
                // the caller can say so.
                parts.last_mut().expect("there is always a part").push(c);
            } else {
                parts.push(String::new());
            }
        } else {
            parts.last_mut().expect("there is always a part").push(c);
        }
    }
    if escaped {
        parts.last_mut().expect("there is always a part").push('\\');
    }
    parts
}

/// A replacement written the vim way, in the engine's spelling.
///
/// `\1` is the pattern's first group, in the pattern's own numbering — which
/// `\zs` and `\ze` move along in the engine's, since each of them is a group
/// too, so the number is looked up in `extent`. `&` and `\0` are the whole
/// match, which for a pattern with markers is the part it reports.
fn translate(replacement: &str, extent: &crate::dialect::Extent) -> String {
    let mut out = String::with_capacity(replacement.len());
    let mut chars = replacement.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            // `\1` is a group, `\&` is an ampersand, and anything else after a
            // backslash is itself.
            '\\' => match chars.next() {
                Some(digit) if digit.is_ascii_digit() => {
                    let group = digit as usize - '0' as usize;
                    out.push_str("${");
                    out.push_str(&extent.group(group).to_string());
                    out.push('}');
                }
                Some(other) => out.push(other),
                None => out.push('\\'),
            },
            // `&` is the whole match, in vim.
            '&' => out.push_str("${0}"),
            // A `$` is a literal one unless it starts a group reference, and a
            // group reference is written with braces: the engine reads `$2_` as
            // a group *named* `2_`, which is nobody's intention.
            '$' => {
                let mut digits = String::new();
                while let Some(next) = chars.peek() {
                    if next.is_ascii_digit() {
                        digits.push(*next);
                        chars.next();
                    } else {
                        break;
                    }
                }
                if digits.is_empty() {
                    match chars.peek() {
                        Some('{') => out.push('$'),
                        _ => out.push_str("$$"),
                    }
                } else {
                    out.push_str("${");
                    out.push_str(&digits);
                    out.push('}');
                }
            }
            other => out.push(other),
        }
    }
    out
}

/// What one match becomes: the replacement, with the match and its groups in it.
pub(crate) fn expand(
    caps: &regex::Captures<'_>,
    extent: &crate::dialect::Extent,
    replacement: &str,
    text: &str,
) -> String {
    let mut out = String::with_capacity(replacement.len());
    expand_into(caps, extent, replacement, text, &mut out);
    out
}

/// The same, into a string that is already being built.
///
/// The engine's own `expand` is what reads `${1}` and `${name}`, and it is used
/// here so that a replacement is read the same way it always was — with one
/// thing the engine cannot say: `${0}` is the text the engine matched, while
/// what `&` and `\0` mean, and what `\zs` and `\ze` can move, is the part the
/// pattern *reports*. That part goes in first, as text, with its own `$`
/// escaped so that nothing in it can be read as a group reference.
fn expand_into(
    caps: &regex::Captures<'_>,
    extent: &crate::dialect::Extent,
    replacement: &str,
    text: &str,
    out: &mut String,
) {
    if extent.is_empty() {
        caps.expand(replacement, out);
        return;
    }
    let (start, end) = extent.span(caps);
    let matched = text[start..end].replace('$', "$$");
    caps.expand(&replacement.replace("${0}", &matched), out);
}

/// Replace what a pattern finds in one line, and how many places that was.
///
/// The engine's own `replace_all` cannot be used — it replaces the text the
/// pattern matched, and a pattern with `\zs` or `\ze` reports less than that —
/// so the line is walked match by match. `all` is the `g` flag: without it, only
/// the first match on the line is replaced, which is what `replacen` did.
pub(crate) fn replace_line(
    regex: &regex::Regex,
    extent: &crate::dialect::Extent,
    replacement: &str,
    line: &str,
    all: bool,
) -> (String, usize) {
    let mut out = String::with_capacity(line.len());
    let mut last = 0;
    let mut count = 0;
    for caps in regex.captures_iter(line) {
        if count == 1 && !all {
            break;
        }
        let (start, end) = extent.span(&caps);
        // What is between the last match and this one is the line's own text,
        // and matches do not overlap, so these two ends only move forward.
        out.push_str(&line[last..start]);
        expand_into(&caps, extent, replacement, line, &mut out);
        last = end;
        count += 1;
    }
    out.push_str(&line[last..]);
    (out, count)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The replacement side of a `:` line, translated with no markers in it.
    fn out(input: &str) -> String {
        translate(input, &crate::dialect::Extent::default())
    }

    /// `:s` over one line, through the same walk the editor does.
    fn spliced(line: &str, spec: &Substitute) -> String {
        let regex = regex::RegexBuilder::new(&spec.translated)
            .case_insensitive(spec.ignore_case)
            .build()
            .expect("the test's pattern compiles");
        replace_line(&regex, &spec.extent, &spec.replacement, line, spec.all).0
    }

    #[test]
    fn a_range_can_name_a_mark() {
        // One address is that line: the parser fills the second end with it,
        // which is what makes `:5d` one line and not five to the end.
        assert_eq!(
            address_range("'a'd"),
            (Some(Address::Mark('a')), Some(Address::Mark('a')))
        );
        assert_eq!(
            address_range("'a,'bs/x/y/"),
            (Some(Address::Mark('a')), Some(Address::Mark('b')))
        );
        // A backtick names the same line in a range: a range is lines.
        assert_eq!(
            address_range("`a,`bd"),
            (Some(Address::Mark('a')), Some(Address::Mark('b')))
        );
        // A mark and a number, either way round.
        assert_eq!(
            address_range("'a,5d"),
            (Some(Address::Mark('a')), Some(Address::Line(5)))
        );
        assert_eq!(
            address_range("1,'ad"),
            (Some(Address::Line(1)), Some(Address::Mark('a')))
        );
        // `''` is the last jump, which is not a mark: the caller has it.
        assert_eq!(address_range("''d"), (None, None));
        assert_eq!(address_range("'Ad"), (None, None), "only lower case");
    }

    #[test]
    fn a_substitute_is_told_from_a_command_name() {
        assert!(Substitute::looks_like("s/foo/bar/"));
        assert!(Substitute::looks_like("%s/foo/bar/g"));
        assert!(Substitute::looks_like(".s|foo|bar|"));
        assert!(Substitute::looks_like("s#foo#bar#"));
        // A command name that starts with `s`, and a bare `s`.
        assert!(!Substitute::looks_like("set"));
        assert!(!Substitute::looks_like("s"));
        assert!(!Substitute::looks_like("s foo"));
        assert!(!Substitute::looks_like("w"));
    }

    #[test]
    fn the_parts_of_a_line_are_the_pattern_the_replacement_and_the_flags() {
        let spec = Substitute::parse("%s/one/two/g", false, false).expect("parses");
        assert_eq!(
            spec,
            Substitute {
                pattern: "one".into(),
                translated: "one".into(),
                replacement: "two".into(),
                extent: crate::dialect::Extent::default(),
                all: true,
                from: Some(Address::Line(1)),
                to: Some(Address::Last),
                ignore_case: false,
                confirm: false,
            }
        );

        let spec = Substitute::parse("s/one/two/", false, false).expect("parses");
        assert!(!spec.all);
        assert_eq!(
            (spec.from, spec.to),
            (None, None),
            "no range is the cursor's line"
        );

        let spec = Substitute::parse(".s/one/two/gi", false, false).expect("parses");
        assert!(spec.all && spec.ignore_case);
        assert_eq!(
            (spec.from, spec.to),
            (Some(Address::Current), Some(Address::Current))
        );

        let spec = Substitute::parse("s/one/two/gc", false, false).expect("parses");
        assert!(spec.all && spec.confirm, "`c` asks about every match");

        // Line numbers, both ends, and the two addresses with meanings.
        let spec = Substitute::parse("5,10s/a/b/", false, false).expect("parses");
        assert_eq!(
            (spec.from, spec.to),
            (Some(Address::Line(5)), Some(Address::Line(10)))
        );
        let spec = Substitute::parse("%s/a/b/", false, false).expect("parses");
        assert_eq!(
            (spec.from, spec.to),
            (Some(Address::Line(1)), Some(Address::Last))
        );
        let spec = Substitute::parse(".,$s/a/b/", false, false).expect("parses");
        assert_eq!(
            (spec.from, spec.to),
            (Some(Address::Current), Some(Address::Last))
        );
        let spec = Substitute::parse("$s/a/b/", false, false).expect("parses");
        assert_eq!(
            (spec.from, spec.to),
            (Some(Address::Last), Some(Address::Last))
        );
        let spec = Substitute::parse("3,7s/a/b/", false, false).expect("parses");
        assert_eq!(
            (spec.from, spec.to),
            (Some(Address::Line(3)), Some(Address::Line(7)))
        );

        // And a range is what tells `:s/x/y/` from a command whose name starts
        // with a digit or a dot.
        assert!(Substitute::looks_like("%s/a/b/"));
        assert!(Substitute::looks_like("5,10s/a/b/"));
        assert!(Substitute::looks_like(".,$s/a/b/"));
        assert!(!Substitute::looks_like("set number"));
        assert!(!Substitute::looks_like("substitute"));
    }

    #[test]
    fn a_delimiter_can_be_anything_that_is_not_a_letter() {
        assert_eq!(
            Substitute::parse("s|a/b|c|", false, false)
                .expect("parses")
                .pattern,
            "a/b",
            "a delimiter is what lets a pattern contain a slash"
        );
        assert_eq!(
            Substitute::parse("s#one#two#", false, false)
                .expect("parses")
                .replacement,
            "two"
        );
        // An escaped delimiter is the delimiter itself.
        assert_eq!(
            Substitute::parse("s/a\\/b/c/", false, false)
                .expect("parses")
                .pattern,
            "a/b"
        );
    }

    #[test]
    fn the_trailing_delimiter_and_the_replacement_may_be_left_off() {
        assert_eq!(
            Substitute::parse("s/one", false, false)
                .expect("parses")
                .pattern,
            "one"
        );
        assert_eq!(
            Substitute::parse("s/one", false, false)
                .expect("parses")
                .replacement,
            ""
        );
        assert_eq!(
            Substitute::parse("s/one/", false, false)
                .expect("parses")
                .replacement,
            ""
        );
        assert_eq!(
            Substitute::parse("s/one/two", false, false)
                .expect("parses")
                .replacement,
            "two"
        );
        assert!(
            Substitute::parse("s/one/two/g", false, false)
                .expect("parses")
                .all
        );
    }

    #[test]
    fn the_replacement_is_translated_out_of_vims_dialect() {
        assert_eq!(out("two"), "two");
        assert_eq!(out("a&b"), "a${0}b", "`&` is the whole match");
        assert_eq!(
            out("\\1-\\2"),
            "${1}-${2}",
            "`\\1` is a group, written with braces so `\\1_` cannot be read as a name"
        );
        assert_eq!(out("\\&"), "&", "and `\\&` is an ampersand");
        assert_eq!(
            out("$1"),
            "${1}",
            "the engine's own spelling still works, and is normalised"
        );
        assert_eq!(out("${2}_x"), "${2}_x", "a braced one is left alone");
        assert_eq!(
            out("\\2_\\1"),
            "${2}_${1}",
            "which is the difference between the second group and a group named `2_`"
        );
        assert_eq!(out("$"), "$$", "a lone `$` is a dollar sign");
        assert_eq!(out("a$b"), "a$$b");
        assert_eq!(out("\\\\"), "\\", "and `\\\\` is a backslash");
    }

    #[test]
    fn a_group_is_the_one_the_pattern_wrote_and_not_the_engine_that_matched_it() {
        // `\zs` is a group of its own in the engine — an empty one, where the
        // match starts — so `\1` has to be read as the pattern's own first
        // group rather than the engine's.
        let spec = Substitute::parse("s/\\(a\\)\\zsb/X\\1/", false, false).expect("parses");
        assert_eq!(spec.translated, "(a)()b");
        assert_eq!(spec.replacement, "X${1}");
        assert_eq!(spliced("ab", &spec), "aXa");

        // A group written after a marker is the engine's third, and it still
        // answers to `\2` when the replacement asks for it.
        let spec =
            Substitute::parse("s/\\zs\\(a\\)\\(b\\)/\\1-\\2/", false, false).expect("parses");
        assert_eq!(spec.translated, "()(a)(b)");
        assert_eq!(spec.replacement, "${2}-${3}");
        assert_eq!(spliced("ab", &spec), "a-b");
    }

    #[test]
    fn what_a_match_is_is_the_part_the_pattern_reports() {
        // `&` is the match, and a match that `\zs` and `\ze` moved is the part
        // they left — vim's own reading of the two.
        let spec = Substitute::parse("s/a\\zsb/[&]/", false, false).expect("parses");
        assert_eq!(spliced("ab", &spec), "a[b]");
        let spec = Substitute::parse("s/end\\ze\\(if\\|for\\)/X/", false, false).expect("parses");
        assert_eq!(spliced("endif", &spec), "Xif");
        // A match of nothing is a place to put the replacement: an empty one
        // at the end of "ab" is where the match stopped.
        let spec = Substitute::parse("s/a\\zs\\zeb/X/", false, false).expect("parses");
        assert_eq!(spliced("ab", &spec), "aXb");
    }

    #[test]
    fn a_line_without_markers_is_replaced_the_way_the_engine_replaces_it() {
        // The walk is new code, and the engine's own `replace_all` is what it
        // has to agree with whenever a pattern says nothing about where the
        // match is.
        for line in [
            "s/a/X/", "s/a/X/g", "s/a*/X/g", "s/^/> /g", "s/e$/E/g", "s/&/&&/g",
        ] {
            let spec = Substitute::parse(line, false, false).expect("parses");
            let regex = regex::RegexBuilder::new(&spec.translated)
                .build()
                .expect("compiles");
            for text in ["", "a", "baab", "one two", "aaa"] {
                let engine = if spec.all {
                    regex
                        .replace_all(text, spec.replacement.as_str())
                        .into_owned()
                } else {
                    regex
                        .replacen(text, 1, spec.replacement.as_str())
                        .into_owned()
                };
                assert_eq!(spliced(text, &spec), engine, "{line} on {text:?}");
            }
        }
    }

    #[test]
    fn a_line_that_makes_no_sense_says_so() {
        assert_eq!(
            Substitute::parse("s", false, false).unwrap_err(),
            SubstituteError::Argument
        );
        assert_eq!(
            Substitute::parse("s/one/two/x", false, false).unwrap_err(),
            SubstituteError::Flag('x')
        );
        assert_eq!(
            Substitute::parse("s/one/two/g/i", false, false).unwrap_err(),
            SubstituteError::Flag('/'),
            "a fourth part is trailing characters"
        );
        assert_eq!(
            Substitute::parse("s\\one\\two\\", false, false).unwrap_err(),
            SubstituteError::Delimiter('\\')
        );
        assert_eq!(
            Substitute::parse("s/one/two/x", false, false)
                .unwrap_err()
                .to_string(),
            "E488: Trailing characters: x"
        );
        assert_eq!(
            Substitute::parse("s", false, false)
                .unwrap_err()
                .to_string(),
            "E471: Argument required"
        );
    }
}
