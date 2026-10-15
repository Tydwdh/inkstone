//! Fuzzy matching: how well a line matches a pattern whose characters are
//! merely *in order*.
//!
//! ```text
//! pattern: "vim"
//! "vim"        -> perfect
//! "vimeo"      -> good (v i m)
//! "voice mail" -> weaker (v _ i _ _ m)
//! "vintage"    -> no match (no "m")
//! ```
//!
//! The algorithm is fzy's — <https://github.com/jhawthorn/fzy> — which is what
//! vim's own fuzzy matching is based on, so that the place this editor reports
//! for a line is the place vim reports: the *best scoring* match, which is not
//! the leftmost one. The two tables below are fzy's: a character scores higher at
//! the start of the line, at the start of a word (a space, `-`, `_`, a `.` or a
//! capital after a lower-case letter), and directly after the previous match; a
//! gap between two matches costs, and so does a pattern that starts late.
//!
//! Two rules are vim's own and were measured rather than read, because the
//! manual is one sentence for both of them:
//!
//! - **Case.** "case is ignored if the pattern is all lower case" is true one
//!   character at a time: `/Get/f` finds `GETPAT` and `GetPat` but not `getpat`.
//!   A capital in the pattern wants a capital, and everything else is folded.
//! - **Words.** A pattern with several words has every one of them matched
//!   separately, anywhere in the line and in any order, and they may overlap:
//!   `/a a/f` is happy with a line holding one `a`. The place that comes back is
//!   the first word's best match, which the terminal showed on `PatternGet`:
//!   `/get pat/f` reports the `Get` at the end rather than the `Pat` at the
//!   start.
//!
//! The other words are only asked *whether* they match: a fuzzy `:vimgrep` sorts
//! its places by file and line, which the terminal also showed, so their scores
//! are never looked at.

/// The score a match cannot reach, so that a value no match has can be told from
/// one that is merely bad — fzy's own sentinel.
const SCORE_MIN: f64 = f64::MIN;

// fzy's table: what a matched character is worth, and what a gap costs.
const SCORE_GAP_LEADING: f64 = -0.005;
const SCORE_GAP_TRAILING: f64 = -0.005;
const SCORE_GAP_INNER: f64 = -0.01;
const SCORE_MATCH_CONSECUTIVE: f64 = 1.0;
const SCORE_MATCH_SLASH: f64 = 0.9;
const SCORE_MATCH_WORD: f64 = 0.8;
const SCORE_MATCH_CAPITAL: f64 = 0.7;
const SCORE_MATCH_DOT: f64 = 0.6;

/// Where the first word of `pattern` matches `line` best, in characters, or
/// `None` when a word of it does not match at all.
///
/// The place is a character index, the same currency as the cursor and the
/// columns of a list of places.
pub(crate) fn match_start(line: &str, pattern: &str) -> Option<usize> {
    let mut words = pattern.split_whitespace();
    // The first word is the one whose place comes back, so it is the one the
    // scoring has to run for — and every word has to be there, this one
    // included.
    let first = words.next()?;
    if !has_match(line, first) || !words.all(|word| has_match(line, word)) {
        return None;
    }
    best_match(first, line)
}

/// Whether every character of `word` is in `line`, in order — the cheap test
/// that keeps the scoring away from most lines of most files.
fn has_match(line: &str, word: &str) -> bool {
    let mut wanted = word.chars();
    let mut next = wanted.next();
    for found in line.chars() {
        if let Some(pattern) = next {
            if same(pattern, found) {
                next = wanted.next();
            }
        } else {
            return true;
        }
    }
    next.is_none()
}

/// Whether a pattern character can match a line character.
///
/// Case is folded, except that a capital in the pattern wants a capital — which
/// is the measured rule above, and the reason this is not a plain lowercase
/// comparison.
fn same(pattern: char, found: char) -> bool {
    if pattern.is_uppercase() {
        pattern == found
    } else {
        pattern.to_lowercase().eq(found.to_lowercase())
    }
}

/// What a matched character at `ch` is worth, given what comes before it.
///
/// fzy's table: the start of the line — which fzy spells as "the character
/// before is a `/`" — is worth the most, then the start of a word, then a
/// capital after a lower-case letter (the `camelCase` this editor's own names
/// are made of), then the character after a `.`.
fn bonus(before: char, ch: char) -> f64 {
    if !ch.is_ascii_alphanumeric() {
        return 0.0;
    }
    match before {
        '/' => SCORE_MATCH_SLASH,
        '-' | '_' | ' ' => SCORE_MATCH_WORD,
        '.' => SCORE_MATCH_DOT,
        _ if ch.is_ascii_uppercase() && before.is_ascii_lowercase() => SCORE_MATCH_CAPITAL,
        _ => 0.0,
    }
}

/// The character before position `at`, which is the start of the line's own
/// beginning: fzy counts the line as if it followed a `/`, so its first
/// character is worth the same as the start of a path's last part.
fn before(line: &[char], at: usize) -> char {
    match at.checked_sub(1) {
        Some(index) => line[index],
        None => '/',
    }
}

/// Where the best match of `word` in `line` starts, or `None` if it is not
/// there at all.
fn best_match(word: &str, line: &str) -> Option<usize> {
    let word: Vec<char> = word.chars().collect();
    let line: Vec<char> = line.chars().collect();
    let (n, m) = (word.len(), line.len());
    if n > m {
        return None;
    }
    if n == m {
        // The strings are the same length and the word is in the line, so the
        // word is the line — fzy's own shortcut, which keeps the whole thing
        // from having to run.
        return Some(0);
    }

    // `d[i][j]` is the best score for the word's `i`th character ending at the
    // line's `j`th; `m[i][j]` the best score anywhere up to it. The rows are
    // kept because the place of the match is found by walking them backwards.
    let mut scores: Vec<f64> = vec![SCORE_MIN; n * m];
    let mut best: Vec<f64> = vec![SCORE_MIN; n * m];
    for i in 0..n {
        // A gap after the last character of the word is cheaper than one in the
        // middle of it: what is left of the line after the match matters less
        // than what is skipped inside it.
        let gap = if i + 1 == n {
            SCORE_GAP_TRAILING
        } else {
            SCORE_GAP_INNER
        };
        let mut running = SCORE_MIN;
        for j in 0..m {
            let score = if same(word[i], line[j]) {
                if i == 0 {
                    (j as f64) * SCORE_GAP_LEADING + bonus(before(&line, j), line[j])
                } else if j > 0 {
                    let after = best[(i - 1) * m + j - 1] + bonus(line[j - 1], line[j]);
                    let joined = scores[(i - 1) * m + j - 1] + SCORE_MATCH_CONSECUTIVE;
                    after.max(joined)
                } else {
                    SCORE_MIN
                }
            } else {
                SCORE_MIN
            };
            scores[i * m + j] = score;
            running = score.max(running + gap);
            best[i * m + j] = running;
        }
    }

    // Backwards, from the best place the word can end, stepping back through the
    // rows to find where it starts. When more than one path scores the same, fzy
    // takes the first one it meets — the latest in the line — and so does this.
    // A row that finds nothing means the word is not in the line after all,
    // which the caller has already ruled out: the backtrace answers `None`
    // rather than a place from a row that did match.
    let mut required = false;
    let mut start = None;
    let mut j = m;
    for i in (0..n).rev() {
        let mut found = None;
        while j > 0 {
            j -= 1;
            let here = scores[i * m + j];
            if here != SCORE_MIN && (required || here == best[i * m + j]) {
                required = i > 0
                    && j > 0
                    && best[i * m + j] == scores[(i - 1) * m + j - 1] + SCORE_MATCH_CONSECUTIVE;
                found = Some(j);
                break;
            }
        }
        let found = found?;
        start = Some(found);
    }
    start
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Where a match starts, as the column a list of places would show it: the
    /// numbers here came from vim's own `getqflist()`.
    fn start(line: &str, pattern: &str) -> Option<usize> {
        match_start(line, pattern).map(|at| at + 1)
    }

    #[test]
    fn a_pattern_is_in_the_line_in_order() {
        assert_eq!(start("vim", "vim"), Some(1), "perfect");
        assert_eq!(start("vimeo", "vim"), Some(1), "v i m");
        assert_eq!(start("voice mail", "vim"), Some(1), "v _ i _ _ m");
        assert_eq!(start("vintage", "vim"), None, "no m");
        // The characters are in order, not merely present.
        assert_eq!(start("miv", "vim"), None);
    }

    #[test]
    fn the_best_match_is_not_the_leftmost_one() {
        // Vim reports the *best* place, which is what its fzy-based scoring
        // says — a word start or a run of matches beats being early.
        assert_eq!(
            start("xabc a_bc", "abc"),
            Some(6),
            "the `a_bc`, not the `abc`"
        );
        assert_eq!(
            start("aXbXc a_b_c", "abc"),
            Some(1),
            "here the first one *is* the better one: it starts the line"
        );
        assert_eq!(start("gxxet get", "get"), Some(7), "the second `get`");
        assert_eq!(start("cabcabXbc", "abc"), Some(2));
        assert_eq!(start("PatternGet", "tg"), Some(4), "t…G, the tighter one");
        assert_eq!(start("patGetter", "tg"), Some(3), "the adjacent `tG`");
        assert_eq!(start("MatchpatternGet", "tg"), Some(9));
    }

    #[test]
    fn case_is_folded_one_character_at_a_time() {
        // "case is ignored if the pattern is all lower case" — and a capital in
        // the pattern wants a capital, which the terminal showed.
        assert_eq!(start("GETPAT", "get"), Some(1));
        assert_eq!(start("getpat", "get"), Some(1));
        assert_eq!(start("GETPAT", "Get"), Some(1), "G matches, e and t fold");
        assert_eq!(start("GetPat", "Get"), Some(1));
        assert_eq!(start("getpat", "Get"), None, "there is no G in it");
        assert_eq!(start("GETPAT", "GET"), Some(1));
        assert_eq!(start("GetPat", "GET"), None, "the pattern's own case");
    }

    #[test]
    fn several_words_are_matched_separately_in_any_order() {
        // The manual's own examples for "get pat", all six of them.
        for line in [
            "GetPattern",
            "PatternGet",
            "getPattern",
            "patGetter",
            "getSomePattern",
            "MatchpatternGet",
        ] {
            assert!(match_start(line, "get pat").is_some(), "{line}");
        }
        // The place that comes back is the *first* word's: on `PatternGet` the
        // `Pat` is earlier but the terminal reports the `Get`.
        assert_eq!(start("PatternGet", "get pat"), Some(8));
        assert_eq!(start("GetPattern", "get pat"), Some(1));
        assert_eq!(start("MatchpatternGet", "get pat"), Some(13));
        // The words may overlap: one `a` is enough for both of them.
        assert!(match_start("aa", "a a").is_some());
        // And "a a" matches in the first word's best place, the word start.
        assert_eq!(start("xabc a_bc", "a a"), Some(6));
    }
}
