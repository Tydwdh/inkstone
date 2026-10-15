//! Searching the buffer: the pattern, and every place it matches.
//!
//! # Lines, not the whole document
//!
//! A match is searched for *within a line*, never across a terminator. That is
//! what makes a match something the screen can show: a highlight is a row and a
//! span of columns, and a match that ran from one line into the next would have
//! to be drawn as two pieces of a thing the user typed as one. It also means a
//! pattern can never match a line break, which is a limitation vim does not
//! have — and one worth naming out loud rather than discovering.
//!
//! # Patterns are vim's, translated
//!
//! A pattern is typed the way vim types one — `\<word\>`, `s/\(a\|b\)/x/`, and
//! the `\zs` and `\ze` that move where a match starts and ends — and goes
//! through `dialect` before it is compiled, so the engine sees a pattern of its
//! own spelling and a person types vim's. A pattern that does not
//! compile is reported rather than silently matching nothing, because "nothing
//! found" and "I do not understand you" are different answers to the same
//! keystroke.
//!
//! # Character indices
//!
//! Every match is a `(start, end)` pair of *character* indices into the buffer,
//! the same currency as the cursor, the line model and the history. The regex
//! engine counts bytes, so every offset it hands back is converted here — and a
//! pattern with a CJK character in it is a good test that the conversion is
//! right.

use regex::{Regex, RegexBuilder};

use crate::buffer::Buffer;
use crate::dialect::Extent;

/// Which way a search runs, and which way `n` runs after it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Direction {
    /// Forward, from the cursor to the end and round again (`/`).
    #[default]
    Forward,
    /// Backward, from the cursor to the start and round again (`?`).
    Backward,
}

impl Direction {
    /// The other way round, for `N`.
    pub(crate) fn reversed(self) -> Self {
        match self {
            Direction::Forward => Direction::Backward,
            Direction::Backward => Direction::Forward,
        }
    }
}

/// A pattern that could not be compiled, with the engine's own explanation.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct PatternError(String);

impl From<regex::Error> for PatternError {
    fn from(error: regex::Error) -> Self {
        Self(error.to_string())
    }
}

/// A search: the pattern as it was typed, and where it matches.
///
/// The case rule the two settings make, for a pattern that is already known.
///
/// `'ignorecase'` says to ignore case; `'smartcase'` takes that back when the
/// pattern itself has an upper case letter in it, which is what makes `/Foo`
/// find `Foo` and `/foo` find either. Vim applies this to `/`, `?`, `n`, `N`,
/// `:g` and `:s`, so it is a free function rather than a method: the substitute
/// parses its own pattern and has no editor to ask.
pub(crate) fn ignore_case_for(pattern: &str, ignorecase: bool, smartcase: bool) -> bool {
    ignorecase && !(smartcase && pattern.chars().any(char::is_uppercase))
}

/// The matches are recomputed after every edit, because they are character
/// ranges and every edit moves them. The *compiled* pattern is kept, because
/// compiling one is the expensive part and an edit does not change it.
#[derive(Debug, Clone, Default)]
pub struct Search {
    pattern: String,
    /// The pattern, compiled. `None` until a search has been run.
    compiled: Option<Regex>,
    /// Where `\zs` and `\ze` put the ends of a match, if the pattern has any.
    extent: Extent,
    direction: Direction,
    matches: Vec<(usize, usize)>,
    ignore_case: bool,
}

impl Search {
    /// A search that has not found anything yet.
    pub fn new() -> Self {
        Self {
            pattern: String::new(),
            compiled: None,
            extent: Extent::default(),
            direction: Direction::Forward,
            matches: Vec::new(),
            ignore_case: false,
        }
    }

    /// The pattern, as typed. Empty means no search has been run.
    pub(crate) fn pattern(&self) -> &str {
        &self.pattern
    }

    /// Which way this search runs.
    pub(crate) fn direction(&self) -> Direction {
        self.direction
    }

    /// Where the pattern matches, in document order.
    pub fn matches(&self) -> &[(usize, usize)] {
        &self.matches
    }

    /// Whether anything has been searched for.
    pub fn is_empty(&self) -> bool {
        self.pattern.is_empty()
    }

    /// Search `buffer` for `pattern`, `direction` from now on.
    ///
    /// Returns how many places it matches. An empty pattern is not a search: it
    /// leaves everything as it was and reports nothing, so `/<Enter>` repeating
    /// the last pattern is the caller's business.
    pub fn run(
        &mut self,
        buffer: &Buffer,
        pattern: &str,
        direction: Direction,
        ignore_case: bool,
    ) -> Result<usize, PatternError> {
        if pattern.is_empty() {
            return Ok(0);
        }
        // The pattern is kept as it was typed — that is what `n` repeats and
        // what a message shows — and compiled in the engine's dialect.
        let translated =
            crate::dialect::translate(pattern).map_err(|error| PatternError(error.to_string()))?;
        let compiled = RegexBuilder::new(&translated.pattern)
            .case_insensitive(translated.ignore_case.unwrap_or(ignore_case))
            .build()?;
        self.pattern = pattern.to_string();
        self.compiled = Some(compiled);
        self.extent = translated.extent;
        self.direction = direction;
        self.ignore_case = ignore_case;
        self.refresh(buffer);
        Ok(self.matches.len())
    }

    /// Find the matches again, after the buffer changed.
    pub(crate) fn refresh(&mut self, buffer: &Buffer) {
        self.matches = match &self.compiled {
            Some(regex) => find(buffer, regex, &self.extent),
            None => Vec::new(),
        };
    }

    /// Forget the search entirely: a different document is being edited.
    pub fn clear(&mut self) {
        self.pattern.clear();
        self.compiled = None;
        self.extent = Extent::default();
        self.matches.clear();
    }

    /// The match the cursor would land on when searching from `from`, going
    /// `direction`, wrapping round the end of the document the way vim does.
    ///
    /// `from` is a character index; a match that starts exactly at it is skipped
    /// for a forward search, so `n` moves on rather than standing still. `wrap`
    /// is `'wrapscan'`: without it, a search that runs off the end of the
    /// document finds nothing rather than coming round again.
    pub fn next_from(
        &self,
        from: usize,
        direction: Direction,
        wrap: bool,
    ) -> Option<(usize, usize)> {
        if self.matches.is_empty() {
            return None;
        }
        match direction {
            Direction::Forward => self
                .matches
                .iter()
                .find(|(start, _)| *start > from)
                .or_else(|| wrap.then(|| self.matches.first()).flatten())
                .copied(),
            Direction::Backward => self
                .matches
                .iter()
                .rev()
                .find(|(start, _)| *start < from)
                .or_else(|| wrap.then(|| self.matches.last()).flatten())
                .copied(),
        }
    }

    /// The match containing `at`, which is the one the cursor is on.
    pub(crate) fn match_at(&self, at: usize) -> Option<(usize, usize)> {
        self.matches
            .iter()
            .find(|(start, end)| *start <= at && at < *end)
            .copied()
    }
}

/// Every place `regex` matches in `buffer`, line by line.
///
/// Matches do not overlap; the engine decides that, and it is the same rule for
/// every pattern. A match of nothing at all — `^` on its own — is not collected:
/// it has no cells to highlight and nowhere for the cursor to go.
///
/// What is collected is the part of the match the pattern *reports*, which
/// `\zs` and `\ze` can make shorter than what the engine matched — that is what
/// [`Extent`] reads back from the groups those markers became. A pattern
/// without them is read with `find_iter`, which does not have to remember where
/// any group landed.
fn find(buffer: &Buffer, regex: &Regex, extent: &Extent) -> Vec<(usize, usize)> {
    let mut found = Vec::new();
    for line in 0..buffer.line_count() {
        let text = buffer.line_string(line);
        let line_start = buffer.line_col_to_char(line, 0);
        let mut keep = |start: usize, end: usize| {
            if start == end {
                return;
            }
            found.push((
                line_start + text[..start].chars().count(),
                line_start + text[..end].chars().count(),
            ));
        };
        if extent.is_empty() {
            for hit in regex.find_iter(&text) {
                keep(hit.start(), hit.end());
            }
        } else {
            for caps in regex.captures_iter(&text) {
                let (start, end) = extent.span(&caps);
                keep(start, end);
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matches_of(text: &str, pattern: &str, ignore_case: bool) -> Vec<(usize, usize)> {
        let mut search = Search::new();
        search
            .run(
                &Buffer::from_text(text),
                pattern,
                Direction::Forward,
                ignore_case,
            )
            .expect("the test's pattern compiles");
        search.matches().to_vec()
    }

    #[test]
    fn a_match_is_a_character_range() {
        // "héllo wörld" — the offsets are characters, not bytes.
        assert_eq!(matches_of("héllo wörld\n", "wörld", false), vec![(6, 11)]);
    }

    #[test]
    fn every_match_on_a_line_is_found_and_none_overlap() {
        assert_eq!(
            matches_of("aaaa\n", "aa", false),
            vec![(0, 2), (2, 4)],
            "two matches, not three"
        );
        assert_eq!(
            matches_of("one two one\ntwo\n", "two", false),
            vec![(4, 7), (12, 15)]
        );
    }

    #[test]
    fn a_match_never_straddles_a_terminator() {
        // The pattern is in the document only as the end of one line followed by
        // the start of the next; searched per line, it does not match at all.
        assert!(matches_of("one\ntwo\n", "e\nt", false).is_empty());
        // And a pattern that matches on each line matches twice, at the
        // character index the second one really has.
        assert_eq!(matches_of("one\ntwo\n", "o", false), vec![(0, 1), (6, 7)]);
    }

    #[test]
    fn case_matters_unless_it_is_told_not_to() {
        assert_eq!(
            matches_of("One one\n", "one", false),
            vec![(4, 7)],
            "the capitalised one is not the lower-case pattern"
        );
        assert_eq!(matches_of("One one\n", "one", true), vec![(0, 3), (4, 7)]);
        // Unicode case folding, one character at a time.
        assert_eq!(
            matches_of("Ärger ärger\n", "ärger", true),
            vec![(0, 5), (6, 11)]
        );
    }

    #[test]
    fn a_crlf_file_matches_the_content_and_not_the_carriage_return() {
        assert_eq!(matches_of("one\r\ntwo\r\n", "one", false), vec![(0, 3)]);
        assert!(
            matches_of("one\r\ntwo\r\n", "e\r", false).is_empty(),
            "the terminator is not content"
        );
    }

    #[test]
    fn searching_forward_and_backward_wraps_round() {
        let mut search = Search::new();
        search
            .run(
                &Buffer::from_text("one two one two\n"),
                "two",
                Direction::Forward,
                false,
            )
            .expect("compiles");
        assert_eq!(search.next_from(0, Direction::Forward, true), Some((4, 7)));
        assert_eq!(
            search.next_from(4, Direction::Forward, true),
            Some((12, 15))
        );
        assert_eq!(
            search.next_from(12, Direction::Forward, true),
            Some((4, 7)),
            "past the last match it starts again"
        );
        assert_eq!(
            search.next_from(12, Direction::Backward, true),
            Some((4, 7))
        );
        assert_eq!(
            search.next_from(0, Direction::Backward, true),
            Some((12, 15)),
            "before the first it wraps to the last"
        );
        // `'nowrapscan'`: running off the end finds nothing rather than coming
        // round again.
        assert_eq!(search.next_from(12, Direction::Forward, false), None);
        assert_eq!(search.next_from(0, Direction::Backward, false), None);
        assert_eq!(
            search.next_from(0, Direction::Forward, false),
            Some((4, 7)),
            "and a match that is there is still found"
        );
    }

    #[test]
    fn the_match_under_the_cursor_is_the_one_it_is_on() {
        let mut search = Search::new();
        search
            .run(
                &Buffer::from_text("one two\n"),
                "two",
                Direction::Forward,
                false,
            )
            .expect("compiles");
        assert_eq!(search.match_at(4), Some((4, 7)));
        assert_eq!(search.match_at(5), Some((4, 7)), "inside the match");
        assert_eq!(search.match_at(7), None, "past its end");
        assert_eq!(search.match_at(0), None);
    }

    #[test]
    fn an_empty_pattern_is_not_a_search() {
        let mut search = Search::new();
        assert_eq!(
            search.run(&Buffer::from_text("one\n"), "", Direction::Forward, false),
            Ok(0)
        );
        assert!(search.is_empty());
        assert!(search.matches().is_empty());
        assert_eq!(search.next_from(0, Direction::Forward, true), None);
    }

    #[test]
    fn a_pattern_is_a_regular_expression() {
        assert_eq!(
            matches_of("one 1 two 22\n", "\\d+", false),
            vec![(4, 5), (10, 12)],
            "digits, of any length"
        );
        assert_eq!(
            matches_of("one\ntwo\n", "^\\w+", false),
            vec![(0, 3), (4, 7)],
            "`^` is the line's own start, not the document's"
        );
        assert_eq!(
            matches_of("one\ntwo\n", "e$", false),
            vec![(2, 3)],
            "and `$` the line's own end"
        );
        assert_eq!(
            matches_of("ab ab\n", "(ab) \\w", false),
            vec![(0, 4)],
            "a group is just part of the pattern"
        );
        assert_eq!(
            matches_of("a.c\n", "a\\.c", false),
            vec![(0, 3)],
            "an escaped dot is a dot"
        );
    }

    #[test]
    fn a_match_can_say_where_it_starts_and_ends() {
        // `\zs` and `\ze` move the ends of the match, and what is found is
        // what the pattern reports rather than what the engine matched.
        assert_eq!(matches_of("ab\n", "a\\zsb", false), vec![(1, 2)]);
        assert_eq!(
            matches_of("endif endfor\n", "end\\ze\\(if\\|for\\)", false),
            vec![(0, 3), (6, 9)],
            "`\\ze` on its own: the match ends where it says"
        );
        assert_eq!(
            matches_of("abc\n", "b\\zs", false),
            vec![],
            "a match that ends where it starts highlights nothing"
        );
        assert_eq!(
            matches_of("abc\n", "\\zsabc", false),
            vec![(0, 3)],
            "and one that starts where the whole match does"
        );
        // Vim's own example: `\(.\{-}\zsFab\)\{3}` finds the *third* "Fab",
        // because the marker inside the group that repeats is read back from
        // the last time the group matched.
        assert_eq!(
            matches_of("FabFabFab\n", "\\(.\\{-}\\zsFab\\)\\{3}", false),
            vec![(6, 9)]
        );
        // And the last one *encountered* wins in a pattern with branches,
        // which is not the last one written: this matches "ab", "cd", "ab",
        // and the marker in the branch that matched last is the one that
        // counts — where the `b` of the third group begins.
        assert_eq!(
            matches_of("abcdab\n", "\\(a\\zsb\\|c\\zsd\\)\\{3}", false),
            vec![(5, 6)]
        );
        // A `\ze` before the `\zs` is no end at all, so the match ends where
        // it always did.
        assert_eq!(matches_of("abc\n", "a\\zeb\\zsc", false), vec![(2, 3)]);
        assert_eq!(matches_of("abc\n", "\\zsa\\zeb\\zsc", false), vec![(2, 3)]);
        // A marker that took no part does not count.
        assert_eq!(
            matches_of("b\n", "\\(a\\zs\\)\\?b", false),
            vec![(0, 1)],
            "the group was never entered, so nothing was moved"
        );
    }

    #[test]
    fn vims_character_classes_match_the_characters_they_name() {
        // `\k` and `\i` are a name's characters, `\I` and `\K` the same
        // without digits, and `\h` is where a name starts.
        assert_eq!(
            matches_of("a1 b_2\n", "\\k\\+", false),
            vec![(0, 2), (3, 6)],
            "two names"
        );
        assert_eq!(matches_of("a1 23\n", "\\I\\+", false), vec![(0, 1)]);
        assert_eq!(
            matches_of("a1 _x\n", "\\h\\k\\+", false),
            vec![(0, 2), (3, 5)]
        );
        // A Unicode name is a name: the rule the highlighter uses, and the
        // reason these are the engine's classes and not vim's bytes.
        assert_eq!(matches_of("héllo!\n", "\\k\\+", false), vec![(0, 5)]);
        // `\a` is a letter and `\A` is what a letter is not — the engine's own
        // `\A` is the start of the text, which is what this used to mean.
        assert_eq!(matches_of("ab\n", "\\a\\+", false), vec![(0, 2)]);
        assert_eq!(matches_of("a1\n", "\\A", false), vec![(1, 2)]);
        assert_eq!(matches_of("aBc\n", "\\l\\+", false), vec![(0, 1), (2, 3)]);
        assert_eq!(matches_of("aBc\n", "\\u\\+", false), vec![(1, 2)]);
        // Hex and octal digits are ASCII, and where the run stops says so.
        assert_eq!(matches_of("f0g\n", "\\x\\+", false), vec![(0, 2)]);
        assert_eq!(matches_of("0719\n", "\\o\\+", false), vec![(0, 3)]);
        // A file name's characters are what a path is made of.
        assert_eq!(
            matches_of("../a-b.c\n", "\\f\\+", false),
            vec![(0, 8)],
            "the whole path"
        );
    }

    #[test]
    fn a_pattern_that_does_not_compile_says_why() {
        let mut search = Search::new();
        let error = search
            .run(
                &Buffer::from_text("one\n"),
                "(unclosed",
                Direction::Forward,
                false,
            )
            .unwrap_err();
        assert!(!error.to_string().is_empty(), "the engine explains itself");
        assert!(search.is_empty(), "and nothing was searched for");
        assert!(search.matches().is_empty());
    }

    #[test]
    fn a_match_of_nothing_is_not_a_match() {
        // `^` matches at the start of every line, which highlights nothing and
        // gives `n` nowhere to go.
        assert!(matches_of("one\ntwo\n", "^", false).is_empty());
        assert_eq!(
            matches_of("one\ntwo\n", "o*", false).len(),
            2,
            "but `o*` finds the two `o`s"
        );
    }

    #[test]
    fn offsets_are_characters_even_though_the_engine_counts_bytes() {
        // Two CJK characters are six bytes and two characters.
        assert_eq!(matches_of("你好 there\n", "there", false), vec![(3, 8)]);
        assert_eq!(matches_of("你好 there\n", "好", false), vec![(1, 2)]);
        assert_eq!(
            matches_of("héllo wörld\n", "wörld", false),
            vec![(6, 11)],
            "and a pattern with a two-byte character in it"
        );
    }

    #[test]
    fn refreshing_finds_the_matches_where_they_are_now() {
        let mut search = Search::new();
        let mut buffer = Buffer::from_text("one\n");
        search
            .run(&buffer, "one", Direction::Forward, false)
            .expect("compiles");
        assert_eq!(search.matches(), &[(0, 3)]);

        // The same text, one line further down: the match moved with it.
        buffer.replace_range(0, 0, "zero\n");
        search.refresh(&buffer);
        assert_eq!(search.matches(), &[(5, 8)]);
    }
}
