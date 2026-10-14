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
//! The *pattern* is a Rust regular expression, the same dialect `/` searches
//! with. The *replacement* is written the way vim writes one — `&` for the whole
//! match, `\1` for a group — and is translated here into the engine's `$` forms,
//! so a person who has typed `:%s/foo/&bar/` before can type it again. A `$` a
//! user writes is a literal `$`, unless it is followed by a digit or a brace,
//! which is the one case where the engine's own spelling is more useful than
//! vim's.

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
    /// The pattern to find, in the regex dialect.
    pub(crate) pattern: String,
    /// What to put in its place, translated for the engine.
    pub(crate) replacement: String,
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
    pub fn parse(line: &str) -> Result<Self, SubstituteError> {
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
        let mut ignore_case = false;
        let mut confirm = false;
        for flag in flags.chars() {
            match flag {
                'g' => all = true,
                'i' => ignore_case = true,
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
        if let Some(forced) = translated.ignore_case {
            ignore_case = forced;
        }
        Ok(Self {
            pattern: translated.pattern,
            replacement: translate(&replacement),
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
fn translate(replacement: &str) -> String {
    let mut out = String::with_capacity(replacement.len());
    let mut chars = replacement.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            // `\1` is a group, `\&` is an ampersand, and anything else after a
            // backslash is itself.
            '\\' => match chars.next() {
                Some(digit) if digit.is_ascii_digit() => {
                    out.push_str("${");
                    out.push(digit);
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

#[cfg(test)]
mod tests {
    use super::*;

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
        let spec = Substitute::parse("%s/one/two/g").expect("parses");
        assert_eq!(
            spec,
            Substitute {
                pattern: "one".into(),
                replacement: "two".into(),
                all: true,
                from: Some(Address::Line(1)),
                to: Some(Address::Last),
                ignore_case: false,
                confirm: false,
            }
        );

        let spec = Substitute::parse("s/one/two/").expect("parses");
        assert!(!spec.all);
        assert_eq!(
            (spec.from, spec.to),
            (None, None),
            "no range is the cursor's line"
        );

        let spec = Substitute::parse(".s/one/two/gi").expect("parses");
        assert!(spec.all && spec.ignore_case);
        assert_eq!(
            (spec.from, spec.to),
            (Some(Address::Current), Some(Address::Current))
        );

        let spec = Substitute::parse("s/one/two/gc").expect("parses");
        assert!(spec.all && spec.confirm, "`c` asks about every match");

        // Line numbers, both ends, and the two addresses with meanings.
        let spec = Substitute::parse("5,10s/a/b/").expect("parses");
        assert_eq!(
            (spec.from, spec.to),
            (Some(Address::Line(5)), Some(Address::Line(10)))
        );
        let spec = Substitute::parse("%s/a/b/").expect("parses");
        assert_eq!(
            (spec.from, spec.to),
            (Some(Address::Line(1)), Some(Address::Last))
        );
        let spec = Substitute::parse(".,$s/a/b/").expect("parses");
        assert_eq!(
            (spec.from, spec.to),
            (Some(Address::Current), Some(Address::Last))
        );
        let spec = Substitute::parse("$s/a/b/").expect("parses");
        assert_eq!(
            (spec.from, spec.to),
            (Some(Address::Last), Some(Address::Last))
        );
        let spec = Substitute::parse("3,7s/a/b/").expect("parses");
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
            Substitute::parse("s|a/b|c|").expect("parses").pattern,
            "a/b",
            "a delimiter is what lets a pattern contain a slash"
        );
        assert_eq!(
            Substitute::parse("s#one#two#").expect("parses").replacement,
            "two"
        );
        // An escaped delimiter is the delimiter itself.
        assert_eq!(
            Substitute::parse("s/a\\/b/c/").expect("parses").pattern,
            "a/b"
        );
    }

    #[test]
    fn the_trailing_delimiter_and_the_replacement_may_be_left_off() {
        assert_eq!(Substitute::parse("s/one").expect("parses").pattern, "one");
        assert_eq!(Substitute::parse("s/one").expect("parses").replacement, "");
        assert_eq!(Substitute::parse("s/one/").expect("parses").replacement, "");
        assert_eq!(
            Substitute::parse("s/one/two").expect("parses").replacement,
            "two"
        );
        assert!(Substitute::parse("s/one/two/g").expect("parses").all);
    }

    #[test]
    fn the_replacement_is_translated_out_of_vims_dialect() {
        assert_eq!(translate("two"), "two");
        assert_eq!(translate("a&b"), "a${0}b", "`&` is the whole match");
        assert_eq!(
            translate("\\1-\\2"),
            "${1}-${2}",
            "`\\1` is a group, written with braces so `\\1_` cannot be read as a name"
        );
        assert_eq!(translate("\\&"), "&", "and `\\&` is an ampersand");
        assert_eq!(
            translate("$1"),
            "${1}",
            "the engine's own spelling still works, and is normalised"
        );
        assert_eq!(translate("${2}_x"), "${2}_x", "a braced one is left alone");
        assert_eq!(
            translate("\\2_\\1"),
            "${2}_${1}",
            "which is the difference between the second group and a group named `2_`"
        );
        assert_eq!(translate("$"), "$$", "a lone `$` is a dollar sign");
        assert_eq!(translate("a$b"), "a$$b");
        assert_eq!(translate("\\\\"), "\\", "and `\\\\` is a backslash");
    }

    #[test]
    fn a_line_that_makes_no_sense_says_so() {
        assert_eq!(
            Substitute::parse("s").unwrap_err(),
            SubstituteError::Argument
        );
        assert_eq!(
            Substitute::parse("s/one/two/x").unwrap_err(),
            SubstituteError::Flag('x')
        );
        assert_eq!(
            Substitute::parse("s/one/two/g/i").unwrap_err(),
            SubstituteError::Flag('/'),
            "a fourth part is trailing characters"
        );
        assert_eq!(
            Substitute::parse("s\\one\\two\\").unwrap_err(),
            SubstituteError::Delimiter('\\')
        );
        assert_eq!(
            Substitute::parse("s/one/two/x").unwrap_err().to_string(),
            "E488: Trailing characters: x"
        );
        assert_eq!(
            Substitute::parse("s").unwrap_err().to_string(),
            "E471: Argument required"
        );
    }
}
