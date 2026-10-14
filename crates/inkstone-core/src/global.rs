//! `:g` and `:v` — running a command on the lines a pattern finds.
//!
//! ```text
//! [%|.][<address>[,<address>]]g<delim>pattern<delim>[command]
//! [%|.][<address>[,<address>]]v<delim>pattern<delim>[command]
//! ```
//!
//! `:g` runs the command on every line the pattern matches, `:v` on every line
//! it does not. The command is any other `:` command, so `:g/TODO/d` is the
//! delete-every-matching-line idiom and `:g/^$/d` squeezes the blank lines out;
//! a line with nothing after it prints what it matched, as vim's does. The range
//! says which lines are *searched*, and defaults to the whole file, which is the
//! difference between this and `:s`, whose default is the cursor's line.
//!
//! The pattern is the same dialect `:s` and `/` use — a Rust regular expression
//! — and the delimiter is any character that is not a letter or a digit, so a
//! pattern full of slashes can be written with something else around it.

use crate::substitute::{address_range, strip_range, Address, SubstituteError};

/// A `:g` or `:v` line, parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Global {
    /// What a line has to match — or, for `:v`, has not to match.
    pub(crate) pattern: String,
    /// The command to run on each matching line, empty for the default `p`.
    pub command: String,
    /// `:v` rather than `:g`: run on the lines that do *not* match.
    pub(crate) invert: bool,
    /// Where the search starts, if the line named one.
    pub(crate) from: Option<Address>,
    /// Where it ends.
    pub(crate) to: Option<Address>,
}

impl Global {
    /// Whether a `:` line is a global rather than a command.
    ///
    /// It is one when it is an address, a `g` or a `v`, and a delimiter — so
    /// `:g/text/d` is a global while `:goto` is a command name that happens to
    /// start with a `g`, and `:vsplit` one that starts with a `v`.
    pub(crate) fn looks_like(line: &str) -> bool {
        // A line with no address at all is the common case, and `strip_range` is
        // written for `:s`, whose range is optional in the same way: it hands
        // back the line itself when there is nothing to strip.
        let rest = strip_range(line).unwrap_or(line);
        let Some(rest) = rest.strip_prefix(['g', 'v']) else {
            return false;
        };
        match rest.chars().next() {
            Some(c) => !c.is_alphanumeric() && !c.is_whitespace(),
            None => false,
        }
    }

    /// Parse a `:` line that [`Global::looks_like`] accepted.
    pub fn parse(line: &str) -> Result<Self, SubstituteError> {
        let rest = strip_range(line).unwrap_or(line);
        let (from, to) = address_range(line);
        let invert = rest.starts_with('v');
        let rest = &rest[1..];
        let delimiter = rest.chars().next().ok_or(SubstituteError::Argument)?;
        if delimiter.is_alphanumeric() || delimiter == '\\' {
            return Err(SubstituteError::Delimiter(delimiter));
        }
        let (pattern, command) = split_at(&rest[delimiter.len_utf8()..], delimiter)?;
        Ok(Global {
            pattern,
            command: command
                .trim()
                .trim_start_matches(delimiter)
                .trim()
                .to_string(),
            invert,
            from,
            to,
        })
    }
}

/// The text up to the next unescaped delimiter, and what comes after it.
///
/// A backslash escapes the delimiter — which is how a pattern containing the
/// delimiter is written — and every other escape is left alone, because the
/// pattern's own dialect has escapes of its own.
fn split_at(rest: &str, delimiter: char) -> Result<(String, String), SubstituteError> {
    let mut pattern = String::new();
    let mut chars = rest.char_indices();
    while let Some((index, c)) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some((_, next)) if next == delimiter => pattern.push(delimiter),
                Some((_, next)) => {
                    pattern.push('\\');
                    pattern.push(next);
                }
                None => pattern.push('\\'),
            }
            continue;
        }
        if c == delimiter {
            return Ok((pattern, rest[index + c.len_utf8()..].to_string()));
        }
        pattern.push(c);
    }
    // No closing delimiter: vim allows `:g/pattern` on its own, and takes the
    // rest of the line as the command — of which there is none.
    Ok((pattern, String::new()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_global_line_is_told_apart_from_a_command_that_starts_the_same_way() {
        assert!(Global::looks_like("g/one/d"));
        assert!(Global::looks_like("%g/one/d"));
        assert!(Global::looks_like("1,5v/one/d"));
        assert!(Global::looks_like("$g/one/d"));
        assert!(!Global::looks_like("goto"), "a command name");
        assert!(!Global::looks_like("vsplit"), "another one");
        assert!(!Global::looks_like("g"), "no delimiter, no pattern");
        assert!(!Global::looks_like("s/one/two/"), "a substitute");
    }

    #[test]
    fn the_pattern_the_command_and_the_range_come_apart() {
        let spec = Global::parse("g/TODO/d").unwrap();
        assert_eq!(spec.pattern, "TODO");
        assert_eq!(spec.command, "d");
        assert!(!spec.invert);
        assert_eq!(spec.from, None);

        // `:v` is the same thing backwards.
        let spec = Global::parse("v/^$/d").unwrap();
        assert!(spec.invert);
        assert_eq!(spec.pattern, "^$");

        // A range, resolved the way `:s` resolves one.
        let spec = Global::parse("%g/one/").unwrap();
        assert_eq!(
            (spec.from, spec.to),
            (Some(Address::Line(1)), Some(Address::Last))
        );
        assert_eq!(spec.command, "", "nothing after the delimiter");

        // No closing delimiter at all: the pattern runs to the end of the line.
        let spec = Global::parse("g/one").unwrap();
        assert_eq!(spec.pattern, "one");
        assert_eq!(spec.command, "");

        // Another delimiter, and an escaped one inside the pattern.
        let spec = Global::parse("g#a/b#d").unwrap();
        assert_eq!(spec.pattern, "a/b");
        assert_eq!(spec.command, "d");
        let spec = Global::parse("g/a\\/b/d").unwrap();
        assert_eq!(spec.pattern, "a/b");
        assert_eq!(spec.command, "d");

        // A command with an argument of its own travels whole.
        let spec = Global::parse("g/one/s/a/b/").unwrap();
        assert_eq!(spec.command, "s/a/b/");
    }
}
