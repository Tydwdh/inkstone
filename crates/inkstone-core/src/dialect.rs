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
//! | `\zs` `\ze` | `()` | where the match starts and ends |
//! | `\i` `\k` | `[\p{Alphabetic}\p{N}_]` | a name's characters |
//! | `\I` `\K` | `[\p{Alphabetic}_]` | the same, without digits |
//! | `\f` | `[\p{Alphabetic}\p{N}_/.+,\-#$%~=]` | a file name's characters |
//! | `\F` | `[\p{Alphabetic}_/.+,\-#$%~=]` | the same, without digits |
//! | `\h` | `[\p{Alphabetic}_]` | the head of a name |
//! | `\a` `\l` `\u` | `\p{Alphabetic}` `\p{Ll}` `\p{Lu}` | letters |
//! | `\x` `\o` | `[0-9A-Fa-f]` `[0-7]` | hex and octal digits |
//! | `\H` `\A` `\L` `\U` `\X` `\O` | the characters those do not match | |
//! | `\c` `\C` | — | ignore case, or do not |
//! | `\v` | — | everything is magic, which the engine already is |
//! | `\V` | — | nothing is magic |
//!
//! `\zs` and `\ze` are zero-width, and the engine has no way to say "the match
//! starts here and ends there": a match there is a stretch of text and nothing
//! in a pattern can cut it in half. So each becomes an empty group, and the
//! part of a match the pattern *reports* is read back from where those groups
//! landed — [`Extent`] is that reading, and it is what makes `:s/a\zsb/X/`
//! replace the `b`.
//!
//! Vim's own character classes are built out of options (`'isident'`,
//! `'iskeyword'`, `'isfname'`) that this editor does not have, so what is here
//! is those options' defaults, written out as classes. Vim's engine is
//! byte-oriented there and this one is not: `\i` is every Unicode letter or
//! number, which is what the highlighter already calls a name.
//!
//! Everything else is left exactly as it was: `.` and `*` are as magic in one
//! as in the other, `\d` and `\w` are the engine's own Unicode sets, an escaped
//! literal — `\.`, `\*` — is an escaped literal in both, and `\b` is the
//! engine's word boundary rather than vim's backspace, which is the useful
//! reading of it and not vim's.
//!
//! What is *not* here: `\p` and `\P`, vim's "printable" class, which is
//! byte-oriented in vim's own engine — it counts a CJK character as printable
//! and a Greek one as not — and so has no meaning worth copying here.

/// What a pattern asked for that cannot be done here.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DialectError {
    /// `\zs` or `\ze` inside a collection, where vim does not read it as a
    /// marker: "Matches at any position, but not inside []".
    #[error("{0} is not supported inside a collection: vim reads it there as characters")]
    InCollection(&'static str),
    /// `\zs` or `\ze` with a multi after it, which vim refuses too: "This
    /// cannot be followed by a multi. *E888*".
    #[error("{0} cannot be followed by a multi")]
    Multi(&'static str),
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
    /// `\zs` and `\ze`: which part of a match the pattern reports as the match.
    pub(crate) extent: Extent,
}

/// Where a match begins and ends, when the pattern says so from inside itself.
///
/// `\zs` and `\ze` are zero-width, so the engine cannot report them as part of
/// a match: each becomes an empty group, and the *reported* match — what a
/// search highlights, where `n` goes, what `:s` replaces — is read back from
/// where those groups landed.
///
/// A pattern may say either of them several times, in a branch, or in a group
/// that repeats. Vim's rule is that "the last one encountered in a matching
/// branch is used", and for a match found left to right the one encountered
/// last is the one furthest along — which is also the one the engine keeps, a
/// group that matched more than once reporting its last match. A marker that
/// took no part at all — a branch that was not taken, a group that matched
/// nothing — does not count, and the end the match already had stands.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Extent {
    /// The `\zs` groups, in the order they were written, as the engine numbers
    /// them.
    starts: Vec<usize>,
    /// The `\ze` groups, likewise.
    ends: Vec<usize>,
    /// The engine's number for each group the pattern itself wrote, in order.
    ///
    /// `\zs` and `\ze` are groups too, so the numbering a replacement refers to
    /// with `\1` is the numbering *before* they were counted: a group written
    /// after a marker is one number further along in the engine's eyes.
    groups: Vec<usize>,
}

impl Extent {
    /// Whether the pattern says nothing about where the match starts or ends.
    pub(crate) fn is_empty(&self) -> bool {
        self.starts.is_empty() && self.ends.is_empty()
    }

    /// The engine's group for the `n`th group the pattern wrote.
    ///
    /// An index the pattern does not have is left alone, so that a replacement
    /// naming a group that is not there is an empty expansion rather than a
    /// number pointing at something else.
    pub(crate) fn group(&self, n: usize) -> usize {
        match n.checked_sub(1) {
            Some(index) => self.groups.get(index).copied().unwrap_or(n),
            None => n,
        }
    }

    /// The part of a match the pattern reports as the match itself, in bytes.
    ///
    /// The whole match, when the pattern says nothing.
    pub(crate) fn span(&self, caps: &regex::Captures<'_>) -> (usize, usize) {
        let whole = caps.get(0).expect("a match has a whole match");
        if self.is_empty() {
            return (whole.start(), whole.end());
        }
        let at = |group: usize| caps.get(group).map(|hit| hit.start());
        let start = self
            .starts
            .iter()
            .filter_map(|&group| at(group))
            .max()
            .unwrap_or_else(|| whole.start());
        // An end before the start is no end at all — `a\zeb\zsc` ends where it
        // matched, at the `c` — which is vim's own reading of the two.
        let end = self
            .ends
            .iter()
            .filter_map(|&group| at(group))
            .filter(|&at| at >= start)
            .max()
            .unwrap_or_else(|| whole.end());
        (start, end)
    }
}

/// Translate a pattern from vim's dialect into the engine's.
pub(crate) fn translate(input: &str) -> Result<Pattern, DialectError> {
    let mut out = String::with_capacity(input.len());
    let mut ignore_case = None;
    // `\V` and everything after it: never magic again.
    let mut nomagic = false;
    // Inside `[...]`, where vim reads `\zs` and `\ze` as characters. `first` is
    // the vim rule that a `]` right after the `[` (or after its `^`) is a
    // member of the collection rather than the end of it.
    let mut collection = false;
    let mut first = false;
    // Where `\zs` and `\ze` land, and how many groups the engine will have
    // opened by then.
    let mut extent = Extent::default();
    let mut groups = 0;
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            if nomagic && is_magic(c) {
                out.push('\\');
            } else {
                match c {
                    '[' if !collection => {
                        collection = true;
                        first = true;
                    }
                    ']' if collection => {
                        // A `]` that has a member before it ends the collection.
                        if first {
                            first = false;
                        } else {
                            collection = false;
                        }
                    }
                    '^' if collection && first => {}
                    _ if collection => first = false,
                    _ => {}
                }
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
        // A backslash inside a collection escapes the character after it — one
        // of `]`, `^`, `-`, `\` — and the character it escapes is a member.
        if collection {
            first = false;
        }
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
            // A group the pattern wrote is a group the engine counts, and where
            // it lands in that count is what a `\1` in a replacement means.
            '(' => {
                out.push('(');
                groups += 1;
                extent.groups.push(groups);
            }
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
            // them as they are would quietly match something else: each becomes
            // an empty group where it matched, and the match the pattern
            // reports is read back from there.
            'z' => {
                let marker = match chars.peek() {
                    Some('s') => Some(true),
                    Some('e') => Some(false),
                    _ => None,
                };
                let Some(starts) = marker else {
                    out.push_str("\\z");
                    continue;
                };
                chars.next();
                let which = if starts { "\\zs" } else { "\\ze" };
                // Vim does not read either of them inside a collection: the
                // characters are members there, which this translator does not
                // otherwise model, so it refuses rather than guess at them.
                if collection {
                    return Err(DialectError::InCollection(which));
                }
                // "This cannot be followed by a multi": a marker matches
                // nothing, so repeating it would say nothing.
                if followed_by_multi(&mut chars) {
                    return Err(DialectError::Multi(which));
                }
                out.push_str("()");
                groups += 1;
                if starts {
                    extent.starts.push(groups);
                } else {
                    extent.ends.push(groups);
                }
            }
            // Vim's own character classes — `\i`, `\k`, `\f`, `\h`, `\a`, `\l`,
            // `\u`, `\x`, `\o` and their relatives — have no spelling of their
            // own in the engine, which would read each of them as an escape it
            // does not know and fail. They are written out instead: the engine
            // knows the same characters under the same properties.
            'i' | 'I' | 'k' | 'K' | 'f' | 'F' | 'h' | 'H' | 'a' | 'A' | 'l' | 'L' | 'u' | 'U'
            | 'x' | 'X' | 'o' | 'O' => {
                out.push_str(character_class(next));
            }
            // Anything else is left as it is: an escape the engine does not
            // know fails to compile, which is a better answer than matching
            // something else quietly.
            other => {
                out.push('\\');
                out.push(other);
            }
        }
    }
    Ok(Pattern {
        pattern: out,
        ignore_case,
        extent,
    })
}

/// Vim's own character classes, in the engine's spelling.
///
/// Vim builds these out of options — `'isident'`, `'iskeyword'`, `'isfname'` —
/// which this editor does not have, so what is here is those options' *defaults*
/// as vim documents them: `\i` and `\k` are the same set (`@,48-57,_,192-255`
/// with `@` meaning the letters), `\f` adds the punctuation a path is made of,
/// and `\h` is a name's first character.
///
/// Where vim's own engine is byte-oriented — a "letter" is `[A-Za-z]` plus the
/// bytes above 127 — these are the engine's Unicode sets instead, which is what
/// `\d` and `\w` already meant here. A pattern that wants a narrower set of
/// characters writes the class out.
///
/// The capital letters are not always the opposite: `\I`, `\K` and `\F` are
/// "like the same letter, but excluding digits", while `\A`, `\L`, `\U`, `\H`,
/// `\X` and `\O` are what vim's manual says they are — the characters the
/// lowercase form does *not* match.
fn character_class(escape: char) -> &'static str {
    match escape {
        'i' | 'k' => "[\\p{Alphabetic}\\p{N}_]",
        'I' | 'K' => "[\\p{Alphabetic}_]",
        'f' => "[\\p{Alphabetic}\\p{N}_/.+,\\-#$%~=]",
        'F' => "[\\p{Alphabetic}_/.+,\\-#$%~=]",
        'h' => "[\\p{Alphabetic}_]",
        'H' => "[^\\p{Alphabetic}_]",
        'a' => "\\p{Alphabetic}",
        'A' => "\\P{Alphabetic}",
        'l' => "\\p{Ll}",
        'L' => "\\P{Ll}",
        'u' => "\\p{Lu}",
        'U' => "\\P{Lu}",
        'x' => "[0-9A-Fa-f]",
        'X' => "[^0-9A-Fa-f]",
        'o' => "[0-7]",
        'O' => "[^0-7]",
        // Nothing else reaches here: the caller matches the escapes above.
        _ => unreachable!("not a character class"),
    }
}

/// Whether a multi — `*`, `\+`, `\?` or `\{n,m}` — follows.
///
/// Vim refuses `\zs` and `\ze` followed by one of them, and so does this: the
/// pattern is looked at rather than taken, since the question is about the next
/// character rather than a step of the translation.
fn followed_by_multi(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> bool {
    match chars.peek() {
        Some('*') => true,
        Some('\\') => {
            let mut ahead = chars.clone();
            ahead.next();
            matches!(ahead.next(), Some('+') | Some('?') | Some('{'))
        }
        _ => false,
    }
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
    fn a_match_can_say_where_it_starts_and_ends() {
        // Each marker is an empty group where it matched, which is how its
        // position is read back after the match.
        assert_eq!(pattern("a\\zsb"), "a()b");
        assert_eq!(pattern("a\\zeb"), "a()b");
        assert_eq!(pattern("\\zs"), "()");
        assert_eq!(pattern("a\\zsb\\zec"), "a()b()c");
        // `\z` on its own is the engine's own anchor and is left alone.
        assert_eq!(pattern("a\\z"), "a\\z");
        assert_eq!(pattern("a\\zs\\z"), "a()\\z");
    }

    #[test]
    fn the_markers_are_groups_the_pattern_did_not_write() {
        // `\1` in a replacement is the pattern's own first group, and a marker
        // in between moves the engine's numbering along.
        let translated = translate("\\(a\\)\\zs\\(b\\)").expect("translated");
        assert_eq!(translated.pattern, "(a)()(b)");
        assert_eq!(translated.extent.group(1), 1, "before the marker");
        assert_eq!(translated.extent.group(2), 3, "the marker is group two");
        assert_eq!(translated.extent.group(9), 9, "a group that is not there");

        let plain = translate("\\(a\\)\\(b\\)").expect("translated");
        assert_eq!(plain.extent.group(2), 2, "and nothing moved");
        assert!(plain.extent.is_empty());
    }

    #[test]
    fn vims_character_classes_are_written_out_for_the_engine() {
        // Vim builds these from `'isident'`, `'iskeyword'` and `'isfname'`,
        // which are not settings here: they are the options' defaults, in the
        // engine's spelling. The capital letters are not all opposites — `\I`
        // is "like `\i`, but excluding digits" — and the test says which is
        // which, because that is the kind of thing a reading of the manual
        // gets wrong.
        assert_eq!(pattern("\\i"), "[\\p{Alphabetic}\\p{N}_]");
        assert_eq!(pattern("\\k"), "[\\p{Alphabetic}\\p{N}_]");
        assert_eq!(pattern("\\I"), "[\\p{Alphabetic}_]");
        assert_eq!(pattern("\\K"), "[\\p{Alphabetic}_]");
        assert_eq!(pattern("\\f"), "[\\p{Alphabetic}\\p{N}_/.+,\\-#$%~=]");
        assert_eq!(pattern("\\F"), "[\\p{Alphabetic}_/.+,\\-#$%~=]");
        assert_eq!(pattern("\\h"), "[\\p{Alphabetic}_]");
        assert_eq!(pattern("\\a"), "\\p{Alphabetic}");
        assert_eq!(pattern("\\l"), "\\p{Ll}");
        assert_eq!(pattern("\\u"), "\\p{Lu}");
        assert_eq!(pattern("\\x"), "[0-9A-Fa-f]");
        assert_eq!(pattern("\\o"), "[0-7]");
        // The opposites are the opposites of *those*, not "without digits".
        assert_eq!(pattern("\\H"), "[^\\p{Alphabetic}_]");
        assert_eq!(pattern("\\A"), "\\P{Alphabetic}");
        assert_eq!(pattern("\\L"), "\\P{Ll}");
        assert_eq!(pattern("\\U"), "\\P{Lu}");
        assert_eq!(pattern("\\X"), "[^0-9A-Fa-f]");
        assert_eq!(pattern("\\O"), "[^0-7]");
        // And under `\V` a class is the character it looks like, as with
        // everything else there.
        assert_eq!(pattern("\\V\\i"), "i");
        assert_eq!(pattern("\\V\\x2"), "x2");
    }

    #[test]
    fn what_is_not_here_is_not_guessed_at() {
        // A marker inside a collection, where vim reads it as characters
        // rather than as a marker — and where this translator would otherwise
        // be reading the characters after it as a collection's members.
        assert_eq!(translate("[\\zs]"), Err(DialectError::InCollection("\\zs")));
        assert_eq!(translate("[\\ze]"), Err(DialectError::InCollection("\\ze")));
        // An escape is a member, so a collection is still a collection after
        // one: the marker in `[\]]` is inside it too.
        assert_eq!(
            translate("[\\]\\zs]"),
            Err(DialectError::InCollection("\\zs"))
        );
        // And a marker followed by a multi, which vim refuses as well.
        assert_eq!(translate("a\\zs*"), Err(DialectError::Multi("\\zs")));
        assert_eq!(translate("a\\ze\\?"), Err(DialectError::Multi("\\ze")));
        assert_eq!(translate("a\\zs\\{2}"), Err(DialectError::Multi("\\zs")));
        // An escaped multi is not a multi, and neither is one after `\V`,
        // where nothing is magic at all.
        assert_eq!(pattern("a\\zs\\*"), "a()\\*");
        assert_eq!(pattern("\\V\\zs*"), "zs\\*");
    }
}
