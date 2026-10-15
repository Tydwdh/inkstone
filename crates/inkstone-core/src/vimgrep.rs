//! `:vimgrep` — every place a pattern matches, in the list.
//!
//! ```text
//! [count]vim[grep][!] /{pattern}/[g][j] {file}...
//! [count]vim[grep][!] {pattern} {file}...
//! ```
//!
//! The pattern is `/`'s — vim's dialect, translated the way a search translates
//! one — and what it fills is the places list: the one `:copen` shows in a
//! window of its own, `<CR>` goes to a line of, and `:cnext` and `:cprev` walk.
//! A search across a project is something to *walk* rather than to be dropped
//! into, which is what the list is for.
//!
//! The manual's own rules, each of them measured against vim 9.2 before it was
//! written down here:
//!
//! - `\c` and `\C` in the pattern settle case, `'ignorecase'` is what a pattern
//!   that says nothing gets, and `'smartcase'` is *not* used — the one command
//!   where the manual says so in as many words.
//! - The pattern is wrapped in any character that is not an identifier one
//!   (`/x/`, `#x#`, `|x|`), and may instead be a word of its own: "instead of
//!   enclosing the pattern in a non-ID character use a white space separated
//!   pattern".
//! - An empty pattern (`//`) is the last one a search used, and the pattern a
//!   `:vimgrep` was given does *not* become the last one.
//! - Without `g` a line is a place of its own once, however often it matches;
//!   with `g` every match is a place.
//! - `j` leaves the cursor where it is; without it the first place is gone to.
//! - A count in front of the command is the most places to find.
//! - The files may hold the wildcards `'path'` uses for `gf` — `*`, `?`, `[...]`
//!   and `**` — but as a whole path: the name is a pattern too, so
//!   `:vimgrep /x/ src/*.rs` is the Rust files in `src`.
//!
//! `!` is accepted and kept nowhere: in vim it means the jump abandons the
//! changes in the buffer it leaves, and a jump here leaves the document open
//! instead, so there is nothing for it to throw away.

/// A `:vimgrep` line, parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Vimgrep {
    /// The pattern as it was typed, which is what a message shows.
    pub(crate) pattern: String,
    /// The same pattern in the engine's dialect.
    pub(crate) translated: String,
    /// `Some(true)` after a `\c`, `Some(false)` after a `\C`, and `None` when
    /// the pattern said nothing about case.
    pub(crate) ignore_case: Option<bool>,
    /// Whether every match on a line is a place of its own (`g`).
    pub(crate) all: bool,
    /// Whether to go to the first place (`j` says not to).
    pub(crate) jump: bool,
    /// The most places to find, from a count in front of the command.
    pub(crate) limit: Option<usize>,
    /// The files to look in, as they were typed.
    pub(crate) files: Vec<String>,
}

/// Why a `:vimgrep` line did not parse.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum VimgrepError {
    /// Vim's own words for a `:vimgrep` with nothing to search: a pattern or a
    /// file name is missing.
    #[error("E683: File name missing or invalid pattern")]
    Argument,
    /// A range, which this command has no use for: the number in front of it is
    /// the count of places to find, and vim refuses the comma.
    #[error("E481: No range allowed")]
    Range,
    /// A flag that is not one of vim's.
    #[error("E488: Trailing characters: {0}")]
    Flag(char),
    /// `f`, which asks for fuzzy matching: vim has it, this editor does not.
    #[error("fuzzy matching is not implemented")]
    Fuzzy,
    /// A pattern that asks for something the dialect cannot do.
    #[error("E54: {0}")]
    Dialect(crate::dialect::DialectError),
}

impl Vimgrep {
    /// Whether a `:` line is a `:vimgrep` rather than a command.
    ///
    /// Vim's abbreviation is `:vim`, `:vimg`, `:vimgr`, `:vimgre` or `:vimgrep`
    /// — and nothing longer, so that `:vimage` stays a command name that happens
    /// to start the same way.
    pub(crate) fn looks_like(line: &str) -> bool {
        let (_, rest, _) = strip_count(line.trim());
        command_word(rest).is_some()
    }

    /// Parse a `:` line that [`Vimgrep::looks_like`] accepted.
    pub(crate) fn parse(line: &str) -> Result<Self, VimgrepError> {
        let (limit, rest, ranged) = strip_count(line.trim());
        let rest = command_word(rest).ok_or(VimgrepError::Argument)?;
        if ranged {
            return Err(VimgrepError::Range);
        }
        // The `!` of `:vimgrep[!]`, which is about abandoning what the jump
        // leaves behind — see the note at the top of this file.
        let rest = rest.strip_prefix('!').unwrap_or(rest).trim_start();
        // The pattern: a word of its own when it starts with an identifier
        // character, and otherwise what is between a pair of delimiters.
        let Some(first) = rest.chars().next() else {
            return Err(VimgrepError::Argument);
        };
        let (pattern, flags, files) = if is_identifier(first) {
            let (pattern, tail) = rest
                .split_once(char::is_whitespace)
                .ok_or(VimgrepError::Argument)?;
            (pattern.to_string(), String::new(), tail.to_string())
        } else {
            let (pattern, after) = crate::global::split_at(&rest[first.len_utf8()..], first);
            let flags: String = after
                .chars()
                .take_while(char::is_ascii_alphabetic)
                .collect();
            let files = after[flags.len()..].to_string();
            (pattern, flags, files)
        };
        let (mut all, mut jump) = (false, true);
        for flag in flags.chars() {
            match flag {
                'g' => all = true,
                'j' => jump = false,
                'f' => return Err(VimgrepError::Fuzzy),
                other => return Err(VimgrepError::Flag(other)),
            }
        }
        let files: Vec<String> = files.split_whitespace().map(str::to_string).collect();
        if files.is_empty() {
            return Err(VimgrepError::Argument);
        }
        let translated = crate::dialect::translate(&pattern).map_err(VimgrepError::Dialect)?;
        Ok(Self {
            pattern,
            translated: translated.pattern,
            ignore_case: translated.ignore_case,
            all,
            jump,
            limit,
            files,
        })
    }
}

/// The number in front of a `:` command, what follows it, and whether a *range*
/// was written instead.
///
/// A count is a number and then the command: `:5vimgrep` is "five places at
/// most". A range — `:1,5vimgrep` — is a different thing, and vim refuses it
/// rather than reading the `5` as a count.
fn strip_count(line: &str) -> (Option<usize>, &str, bool) {
    let digits = line.chars().take_while(char::is_ascii_digit).count();
    if digits == 0 {
        return (None, line, false);
    }
    let (number, rest) = line.split_at(digits);
    if let Some(after) = rest.strip_prefix(',') {
        let more = after.chars().take_while(char::is_ascii_digit).count();
        return (None, &after[more..], true);
    }
    (number.parse().ok(), rest, false)
}

/// The rest of a `:` line after the command's own name, if the name is one.
///
/// `:vim`, `:vimg`, `:vimgr`, `:vimgre` and `:vimgrep` are the spellings, and
/// what comes back is the line from `!` or from the first space on. A name that
/// runs into an identifier character is a different command altogether.
fn command_word(line: &str) -> Option<&str> {
    let mut rest = line.strip_prefix("vim")?;
    for letter in "grep".chars() {
        match rest.strip_prefix(letter) {
            Some(after) => rest = after,
            None => break,
        }
    }
    match rest.chars().next() {
        Some(c) if is_identifier(c) => None,
        _ => Some(rest),
    }
}

/// Whether a character can be part of a name — vim's "ID character" for the
/// pattern a `:vimgrep` may write without a delimiter in front of it.
fn is_identifier(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_vimgrep_is_told_from_a_command_that_starts_the_same_way() {
        assert!(Vimgrep::looks_like("vimgrep /one/ a.txt"));
        assert!(Vimgrep::looks_like("vim /one/ a.txt"));
        assert!(Vimgrep::looks_like("vimg /one/ a.txt"));
        assert!(Vimgrep::looks_like("vimgre /one/ a.txt"));
        assert!(Vimgrep::looks_like("5vimgrep /one/ a.txt"));
        assert!(Vimgrep::looks_like("1,5vimgrep /one/ a.txt"));
        assert!(Vimgrep::looks_like("vimgrep Error a.c"));
        assert!(!Vimgrep::looks_like("vimage"), "a command name");
        assert!(!Vimgrep::looks_like("vimgrepx /one/ a.txt"), "and longer");
        assert!(
            Vimgrep::looks_like("vim"),
            "the command with nothing after it, which parse refuses"
        );
        assert!(!Vimgrep::looks_like("v/one/d"), "a global");
    }

    #[test]
    fn the_pattern_the_flags_and_the_files_come_apart() {
        let spec = Vimgrep::parse("vimgrep /one/ a.txt b.txt").expect("parses");
        assert_eq!(spec.pattern, "one");
        assert_eq!(spec.translated, "one");
        assert_eq!(spec.files, vec!["a.txt", "b.txt"]);
        assert!(
            !spec.all && spec.jump,
            "no flags is one place a line, and a jump"
        );
        assert_eq!(spec.limit, None);
        assert_eq!(spec.ignore_case, None);

        // The flags, in any order, and a count in front of the command.
        let spec = Vimgrep::parse("3vimgrep /one/gj a.txt").expect("parses");
        assert!(spec.all, "`g`: every match on a line");
        assert!(!spec.jump, "`j`: no jump");
        assert_eq!(spec.limit, Some(3));

        // Any character that is not a name's can be the delimiter, and a
        // backslash escapes it.
        let spec = Vimgrep::parse("vimgrep #a/b# a.txt").expect("parses");
        assert_eq!(spec.pattern, "a/b");
        let spec = Vimgrep::parse("vimgrep #a\\#b# a.txt").expect("parses");
        assert_eq!(spec.pattern, "a#b");

        // The pattern may be a word of its own — "a white space separated
        // pattern" — which is what `:vimgrep Error *.c` is.
        let spec = Vimgrep::parse("vimgrep Error *.c").expect("parses");
        assert_eq!(spec.pattern, "Error");
        assert_eq!(spec.files, vec!["*.c"]);
        // And then nothing is a flag: the word is the whole pattern.
        let spec = Vimgrep::parse("vimgrep Errorj *.c").expect("parses");
        assert_eq!(spec.pattern, "Errorj");
        assert!(spec.jump, "there were no flags to read");

        // `!` is accepted, and the dialect is the search one.
        let spec = Vimgrep::parse("vimgrep! /one/ a.txt").expect("parses");
        assert_eq!(spec.pattern, "one");
        let spec = Vimgrep::parse("vimgrep /\\cone/ a.txt").expect("parses");
        assert_eq!(spec.pattern, "\\cone");
        assert_eq!(spec.translated, "one");
        assert_eq!(spec.ignore_case, Some(true));
    }

    #[test]
    fn a_line_that_makes_no_sense_says_so() {
        assert_eq!(
            Vimgrep::parse("vimgrep /one/").unwrap_err(),
            VimgrepError::Argument,
            "no files"
        );
        assert_eq!(
            Vimgrep::parse("vimgrep /one").unwrap_err(),
            VimgrepError::Argument
        );
        assert_eq!(
            Vimgrep::parse("vimgrep").unwrap_err(),
            VimgrepError::Argument
        );
        assert_eq!(
            Vimgrep::parse("vimgrep /one/x a.txt").unwrap_err(),
            VimgrepError::Flag('x')
        );
        assert_eq!(
            Vimgrep::parse("vimgrep /one/f a.txt").unwrap_err(),
            VimgrepError::Fuzzy,
            "fuzzy matching is a feature of its own"
        );
        assert_eq!(
            Vimgrep::parse("1,5vimgrep /one/ a.txt").unwrap_err(),
            VimgrepError::Range
        );
        // A pattern the dialect cannot do is refused, named, and the refusal is
        // already an `E54`.
        assert_eq!(
            Vimgrep::parse("vimgrep /[\\zs]/ a.txt")
                .unwrap_err()
                .to_string(),
            "E54: \\zs is not supported inside a collection: vim reads it there as characters"
        );
    }
}
