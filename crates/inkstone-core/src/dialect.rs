//! The pattern dialect: vim's regular expressions, in the engine's spelling.
//!
//! Patterns are typed the vim way — `/\<word\>`, `:s/\(a\|b\)/x/` — and the
//! engine is Rust's, so the two dialects meet here. Vim's *magic* mode is the
//! default and is what this translates:
//!
//! | vim | engine | means |
//! |---|---|---|
//! | `\(` `\)` | `(` `)` | a group |
//! | `\|` | `|` | either side |
//! | `\+` | `+` | one or more |
//! | `\?` | `?` | none or one |
//! | `\{n,m}` | `{n,m}` | a count |
//! | `\{-n,m}` | `{n,m}?` | a count, as few as possible |
//! | `\<` `\>` | `\b` | the start or end of a word |
//! | `\c` `\C` | — | ignore case, or do not |
//! | `\v` | — | everything is magic, which the engine already is |
//! | `\V` | — | nothing is magic |
//!
//! Everything else is left exactly as it was: `.` and `*` are as magic in one
//! as in the other, `\d` and `\w` mean the same thing in both, and an escaped
//! literal — `\.`, `\*` — is an escaped literal in both.
//!
//! What is *not* here: `\zs` and `\ze`, which move where a match starts and
//! ends. The engine has no way to say that — a match is a stretch of text and
//! nothing in a pattern can cut it in half — so a pattern using them fails to
//! compile and says so, rather than matching something else quietly. Nor are
//! vim's character classes for identifiers and file names (`\i`, `\k`, `\f`),
//! which the engine spells as Unicode classes and a person can write out.

/// What a pattern asked for that cannot be done here.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DialectError {
    /// A pattern that says where a match starts or ends in the middle of itself.
    #[error("{0} is not supported: a match here is a stretch of text and cannot be cut in half")]
    NotSupported(&'static str),
}

/// A pattern translated for the engine, and what it said about case.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Pattern {
    /// The pattern in the engine's dialect.
    pub(crate) pattern: String,
    /// `Some(true)` after a `\c`, `Some(false)` after a `\C`, and `None` when
    /// the pattern said nothing about case — which leaves the decision to the
    /// `i` flag or to `ignorecase`.
    pub(crate) ignore_case: Option<bool>,
}

/// Translate a pattern from vim's dialect into the engine's.
pub(crate) fn translate(input: &str) -> Result<Pattern, DialectError> {
    let mut out = String::with_capacity(input.len());
    let mut ignore_case = None;
    // `\V` and everything after it: never magic again.
    let mut nomagic = false;
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            if nomagic && is_magic(c) {
                out.push('\\');
            }
            out.push(c);
            continue;
        }
        let Some(next) = chars.next() else {
            // A trailing backslash: the engine reads it the same way, as an
            // error, and says so in its own words.
            out.push('\\');
            continue;
        };
        // Under `\V` nothing is magic, so an escape is the character itself —
        // except for the switches, which are how a pattern gets out of `\V`.
        if nomagic && !matches!(next, 'v' | 'V' | 'm' | 'M' | 'c' | 'C') {
            if is_magic(next) {
                out.push('\\');
            }
            out.push(next);
            continue;
        }
        match next {
            '(' => out.push('('),
            ')' => out.push(')'),
            '|' => out.push('|'),
            '+' => out.push('+'),
            '?' => out.push('?'),
            '<' | '>' => out.push_str("\\b"),
            'c' => ignore_case = Some(true),
            'C' => ignore_case = Some(false),
            'v' => nomagic = false,
            'V' => nomagic = true,
            // Vim's escape for a tab, which the engine also knows.
            'e' => out.push_str("\\x1b"),
            '{' => push_count(&mut out, &mut chars),
            // `\zs` and `\ze` move where a match begins and ends. The engine
            // reads `\z` as an anchor at the very end of the text, so leaving
            // them would quietly match something else — a refusal is the honest
            // answer.
            'z' => {
                let which = match chars.peek() {
                    Some('s') => {
                        chars.next();
                        "\\zs"
                    }
                    Some('e') => {
                        chars.next();
                        "\\ze"
                    }
                    _ => {
                        out.push_str("\\z");
                        continue;
                    }
                };
                return Err(DialectError::NotSupported(which));
            }
            // `\i` and its relatives are not here either: the engine would read
            // them as an escape it does not know and fail, which is a better
            // answer than matching something else.
            other => {
                out.push('\\');
                out.push(other);
            }
        }
    }
    Ok(Pattern {
        pattern: out,
        ignore_case,
    })
}

/// `\{n,m}` and its lazy spelling `\{-n,m}`, which the engine wants as
/// `{n,m}` and `{n,m}?`.
///
/// The count's own characters are copied as they are — the engine knows the
/// same syntax, and vim closes the count with a plain brace — so all that has
/// to change is the brace vim escapes and the `-` that makes it lazy.
fn push_count(out: &mut String, chars: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    // `\{-}` is vim's lazy `*`: as few as possible, any number.
    let lazy = chars.peek() == Some(&'-');
    if lazy {
        chars.next();
        if chars.peek() == Some(&'}') {
            chars.next();
            out.push_str("*?");
            return;
        }
    }
    out.push('{');
    let mut closed = false;
    while let Some(c) = chars.peek().copied() {
        chars.next();
        // A plain brace closes the count; the ones the count is made of are
        // digits, a comma and the `-` already taken.
        if c == '}' {
            closed = true;
            break;
        }
        out.push(c);
    }
    // A count that was never closed is closed here, so that what the engine
    // complains about is the pattern rather than an unbalanced brace of ours.
    let _ = closed;
    out.push('}');
    if lazy {
        out.push('?');
    }
}

/// Whether a character has a meaning of its own in the engine's dialect.
fn is_magic(c: char) -> bool {
    matches!(
        c,
        '.' | '*' | '+' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '|' | '^' | '$' | '\\'
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pattern(input: &str) -> String {
        translate(input)
            .expect("a pattern that is supported")
            .pattern
    }

    #[test]
    fn vims_magic_escapes_become_the_engines_own() {
        assert_eq!(pattern("\\(a\\|b\\)"), "(a|b)");
        assert_eq!(pattern("a\\+"), "a+");
        assert_eq!(pattern("a\\?"), "a?");
        assert_eq!(pattern("a\\{2,3}"), "a{2,3}");
        assert_eq!(pattern("a\\{-2,3}"), "a{2,3}?");
        assert_eq!(pattern("a\\{-}"), "a*?");
        assert_eq!(pattern("\\<word\\>"), "\\bword\\b");
        // What the two dialects agree about, they agree about here too.
        assert_eq!(pattern("a.c"), "a.c");
        assert_eq!(pattern("a*"), "a*");
        assert_eq!(pattern("\\d+\\w*"), "\\d+\\w*");
        assert_eq!(pattern("a\\.b"), "a\\.b", "an escaped literal stays one");
        assert_eq!(pattern("[a-z]{2}"), "[a-z]{2}");
        assert_eq!(pattern("^one$"), "^one$");
        assert_eq!(pattern("\\t"), "\\t");
    }

    #[test]
    fn case_can_be_asked_for_by_the_pattern_itself() {
        let asked = translate("\\cone").unwrap();
        assert_eq!(asked.pattern, "one");
        assert_eq!(asked.ignore_case, Some(true));
        let forced = translate("\\Cone").unwrap();
        assert_eq!(forced.ignore_case, Some(false));
        assert_eq!(translate("one").unwrap().ignore_case, None, "nothing said");
        // The switch is taken out wherever it is, and the rest is translated.
        assert_eq!(pattern("o\\cne"), "one");
    }

    #[test]
    fn very_magic_is_already_the_engine_and_very_nomagic_is_escaped() {
        assert_eq!(
            pattern("\\va.c"),
            "a.c",
            "as magic as the engine already is"
        );
        assert_eq!(pattern("\\Va.c"), "a\\.c", "nothing is magic after this");
        assert_eq!(pattern("\\Va*"), "a\\*");
        assert_eq!(
            pattern("\\V\\<x"),
            "<x",
            "an escape is the character itself"
        );
    }

    #[test]
    fn what_is_not_here_is_not_guessed_at() {
        // `\zs` and `\ze` are refused rather than quietly translated: the
        // engine reads `\z` as an anchor, which would match something else.
        assert_eq!(translate("a\\zsb"), Err(DialectError::NotSupported("\\zs")));
        assert_eq!(translate("a\\zeb"), Err(DialectError::NotSupported("\\ze")));
        // `\z` on its own is the engine's anchor and is left alone.
        assert_eq!(pattern("a\\z"), "a\\z");
    }
}
