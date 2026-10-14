//! What might come next: the words already in the buffer, and the list a person
//! picks from.
//!
//! # Where the words come from
//!
//! Without a language server, insert mode can still complete: the words in the
//! file are the only dictionary there is, and it is a good one — the name of a
//! function is typed once and used ten times. They are gathered *nearest first*,
//! from the cursor's line outwards, which is vim's order and the one that puts
//! the name you just wrote at the top of the list.
//!
//! A language server, when there is one, replaces the source and nothing else:
//! the list, the keys and the popup are the same, because what a person is doing
//! — picking from a list — does not change with where the list came from.

/// One thing that could be typed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    /// What the popup shows.
    pub label: String,
    /// What is typed when it is picked. Usually the label; a server may make it
    /// different, and may make it longer.
    pub insert: String,
    /// A word about what it is — `function`, `keyword` — shown beside it.
    pub detail: String,
    /// Whether what is typed is a snippet — text with placeholders a person
    /// steps through — rather than the text itself.
    pub snippet: bool,
}

impl Item {
    /// An item that types what it shows.
    pub fn plain(label: impl Into<String>) -> Self {
        let label = label.into();
        Self {
            insert: label.clone(),
            label,
            detail: String::new(),
            snippet: false,
        }
    }
}

/// One place a snippet wants something typed: where it is in the text, and what
/// may be typed there.
///
/// A stop with defaults or choices has more than one thing it could be; a stop
/// with neither is a place and nothing else. The text holds the *first* of them,
/// which is what is typed until someone picks another.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stop {
    /// Where the stop's text is, in characters of the snippet's text.
    pub span: (usize, usize),
    /// What may be typed there, first one first. Empty for a plain stop.
    pub choices: Vec<String>,
    /// Which stop this is: two stops with the same number are one place written
    /// twice, and what is typed in the first is what the others say.
    pub number: u32,
    /// Whether this is the second or later writing of its number — a place that
    /// follows another rather than one to type in.
    pub mirror: bool,
}

/// A snippet written the way a language server writes one, read into the text to
/// type and the places a person steps through.
///
/// `${1:name}`, `$2` and `$0` are the stops, and a person visits them in
/// *number* order rather than in the order they appear — `a${2}b${1}c` steps to
/// the `1` first — which is what makes a snippet's fields come out in the order
/// they were meant. A stop with a default types that default, and a default may
/// hold stops of its own. `$0` is the end: the place the cursor finishes at, and
/// where the session ends. A dollar meant literally is written `\$`.
pub fn snippet(text: &str) -> (String, Vec<Stop>) {
    let mut stops: Vec<(u32, Stop)> = Vec::new();
    let mut end = None;
    let text = read_snippet(text, 0, &mut stops, &mut end);
    if let Some(end) = end {
        stops.push(end);
    }
    // In the order a person steps through them, with the end last — and a
    // number written twice is one place: the first is where it is typed, and the
    // rest are the same place following it.
    stops.sort_by_key(|(number, _)| if *number == 0 { u32::MAX } else { *number });
    let mut seen: Vec<u32> = Vec::new();
    let stops: Vec<Stop> = stops
        .into_iter()
        .map(|(_, mut stop)| {
            stop.mirror = seen.contains(&stop.number) && stop.number != 0;
            if !stop.mirror {
                seen.push(stop.number);
            }
            stop
        })
        .collect();
    (text, stops)
}

/// The text of a snippet, with every stop it holds recorded as a range.
///
/// `base` is where the text being read starts in the whole: a default's stops
/// are inside the whole snippet's text, so a nested one is recorded at its own
/// place rather than at nothing.
fn read_snippet(
    text: &str,
    base: usize,
    stops: &mut Vec<(u32, Stop)>,
    end: &mut Option<(u32, Stop)>,
) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' && chars.peek() == Some(&'$') {
            chars.next();
            out.push('$');
            continue;
        }
        if c != '$' {
            out.push(c);
            continue;
        }
        // `$` starts a stop when a number follows, or `{` with a number in it.
        let braced = chars.peek() == Some(&'{');
        if braced {
            chars.next();
        }
        let mut digits = String::new();
        while let Some(d) = chars.peek() {
            if d.is_ascii_digit() {
                digits.push(*d);
                chars.next();
            } else {
                break;
            }
        }
        if digits.is_empty() {
            // Not a stop after all: the `$` is text, and the `{` — if there was
            // one — is text as well.
            out.push('$');
            if braced {
                out.push('{');
            }
            continue;
        }
        let number = digits.parse::<u32>().unwrap_or(u32::MAX);
        let start = base + out.chars().count();
        // `${1|one,two|}` is a choice: the first is typed and the rest are
        // there to pick from. Picking is not here yet, so the first stands for
        // all of them — which is also the one a server means.
        if braced && chars.peek() == Some(&'|') {
            chars.next();
            let mut choices: Vec<String> = vec![String::new()];
            let mut done = false;
            for d in chars.by_ref() {
                match d {
                    // The closing `|` ends the choice; the ones before it
                    // separate the options.
                    '|' if !done => done = true,
                    '|' => break,
                    ',' if !done => choices.push(String::new()),
                    _ if !done => choices.last_mut().expect("always one option").push(d),
                    _ => {}
                }
                if done && d == '}' {
                    break;
                }
            }
            if chars.peek() == Some(&'}') {
                chars.next();
            }
            let first = choices.first().cloned().unwrap_or_default();
            out.push_str(&first);
            let stop = Stop {
                span: (start, base + out.chars().count()),
                choices,
                number,
                mirror: false,
            };
            if number == 0 {
                *end = Some((0, stop));
            } else {
                stops.push((number, stop));
            }
            continue;
        }
        // `${1:default}` types the default; `$1` types nothing. A default may
        // hold stops of its own, which is why it is read rather than copied.
        if braced && chars.peek() == Some(&':') {
            chars.next();
            let mut inner = String::new();
            let mut depth = 1usize;
            while let Some(d) = chars.next() {
                if d == '\\' && chars.peek() == Some(&'}') {
                    chars.next();
                    inner.push('}');
                    continue;
                }
                if d == '{' {
                    depth += 1;
                }
                if d == '}' {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                inner.push(d);
            }
            // The default's own stops are recorded before this one is: what a
            // person fills in first is what they see first.
            let mut before = Vec::new();
            let inner = read_snippet(&inner, start, &mut before, end);
            stops.append(&mut before);
            out.push_str(&inner);
        } else if braced && chars.peek() == Some(&'}') {
            chars.next();
        }
        let stop = Stop {
            span: (start, base + out.chars().count()),
            number,
            mirror: false,
            // A default is what may be typed there, and the only thing — and a
            // default with nothing in it is not a choice but a place.
            choices: if braced {
                let text = out[start.saturating_sub(base)..].to_string();
                if text.is_empty() {
                    Vec::new()
                } else {
                    vec![text]
                }
            } else {
                Vec::new()
            },
        };
        if number == 0 {
            *end = Some((0, stop));
        } else {
            stops.push((number, stop));
        }
    }
    out
}

/// Whether a character can be part of a word: the same rule `w` moves by, so a
/// completion and a motion agree about where a word starts.
pub fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// The word the cursor is in the middle of, and where it starts.
///
/// Columns count characters, and the cursor sits *between* two of them: the
/// word being typed is the run of word characters ending right at the cursor,
/// which is what makes a completion replace what has been typed and nothing
/// else. Just after a space the word is empty and starts at the cursor.
pub fn prefix_at(buffer: &crate::buffer::Buffer, at: usize) -> (usize, String) {
    let mut start = at;
    while start > 0 {
        match buffer.char_at(start - 1) {
            Some(c) if is_word_char(c) => start -= 1,
            _ => break,
        }
    }
    let prefix: String = (start..at)
        .filter_map(|index| buffer.char_at(index))
        .collect();
    (start, prefix)
}

/// Every word in the buffer worth offering, nearest the cursor's line first.
///
/// The order is vim's: the cursor's own line, then the lines above and below it
/// in turn, so the name just typed is the one at the top of the list. A word is
/// offered once however often it appears, and one-letter words are not offered
/// at all: they are noise, and the list is short on purpose.
pub fn words(buffer: &crate::buffer::Buffer, row: usize, limit: usize) -> Vec<Item> {
    let mut items: Vec<Item> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let lines = buffer.line_count();
    let mut order: Vec<usize> = Vec::with_capacity(lines);
    for step in 0..lines {
        if step == 0 {
            order.push(row);
            continue;
        }
        if row >= step {
            order.push(row - step);
        }
        if row + step < lines {
            order.push(row + step);
        }
    }
    for line in order {
        for word in words_on(buffer, line) {
            if word.chars().count() < 2 {
                continue;
            }
            if !seen.insert(word.clone()) {
                continue;
            }
            items.push(Item::plain(word));
            if items.len() >= limit {
                return items;
            }
        }
    }
    items
}

/// The words on one line, in the order they appear.
fn words_on(buffer: &crate::buffer::Buffer, line: usize) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    for index in 0..buffer.line_len(line) {
        match buffer.char_at(buffer.line_col_to_char(line, index)) {
            Some(c) if is_word_char(c) => current.push(c),
            _ => {
                if !current.is_empty() {
                    words.push(std::mem::take(&mut current));
                }
            }
        }
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

/// The items whose labels start with what has been typed, in the order they came.
pub fn matching(items: &[Item], prefix: &str) -> Vec<Item> {
    if prefix.is_empty() {
        return items.to_vec();
    }
    items
        .iter()
        .filter(|item| {
            item.label
                .get(..prefix.len())
                .is_some_and(|head| head == prefix)
        })
        .cloned()
        .collect()
}

#[cfg(test)]
mod snippet_tests {
    use super::snippet;

    /// The text and the stops' spans, which is what most of these are about.
    fn read(text: &str) -> (String, Vec<(usize, usize)>) {
        let (text, stops) = snippet(text);
        (text, stops.into_iter().map(|stop| stop.span).collect())
    }

    /// The same, with what each stop allows.
    fn choices(text: &str) -> Vec<Vec<String>> {
        snippet(text)
            .1
            .into_iter()
            .map(|stop| stop.choices)
            .collect()
    }

    #[test]
    fn a_snippet_is_its_text_and_where_to_step() {
        // A stop with a default types the default.
        let (text, stops) = read("fn ${1:name}() {\n    $0\n}");
        assert_eq!(text, "fn name() {\n    \n}");
        assert_eq!(stops, vec![(3, 7), (16, 16)], "the name, then the end");
        // A stop with nothing in it is a place to type.
        let (text, stops) = read("count(${1}, ${2})");
        assert_eq!(text, "count(, )");
        assert_eq!(stops, vec![(6, 6), (8, 8)]);
        // A plain stop is the same thing without braces.
        let (text, stops) = read("$1 + $2 = $0");
        assert_eq!(text, " +  = ");
        assert_eq!(stops, vec![(0, 0), (3, 3), (6, 6)]);
        // Stops are stepped through in *number* order, not in the order they
        // are written: `a${2}b${1}c` fills in the `1` first.
        let (text, stops) = read("a${2}b${1}c");
        assert_eq!(text, "abc");
        assert_eq!(stops, vec![(2, 2), (1, 1)], "the first stop written first");
    }

    #[test]
    fn a_dollar_that_is_not_a_stop_is_text() {
        let (text, stops) = read("cost: $5 and $x");
        assert_eq!(text, "cost:  and $x", "`$5` is a stop with nothing in it");
        assert_eq!(stops, vec![(6, 6)], "and `$x` is not a stop at all");
        let (text, stops) = read("a \\$1 b");
        assert_eq!(text, "a $1 b", "an escaped dollar is a dollar");
        assert_eq!(stops, Vec::new());
        let (text, stops) = read("no stops at all");
        assert_eq!(text, "no stops at all");
        assert_eq!(stops, Vec::new());
    }

    #[test]
    fn a_choice_keeps_every_option() {
        // The reader keeps what may be typed and not only the first: picking
        // between them is a key away.
        let options = choices("${1|one,two|} ${2:word} ${3}");
        assert_eq!(
            options,
            vec![
                vec!["one".to_string(), "two".to_string()],
                vec!["word".to_string()],
                Vec::new(),
            ]
        );
    }

    #[test]
    fn a_choice_types_its_first_one() {
        // `${1|one,two|}` is a choice: what goes in is the first, and the rest
        // are there for a person to pick from — which is a key this does not
        // have yet, so the first stands for all of them.
        let (text, stops) = read("${1|one,two,three|} and ${2:rest}");
        assert_eq!(text, "one and rest");
        assert_eq!(stops, vec![(0, 3), (8, 12)]);
        // A choice with one option is the same as a default.
        let (text, stops) = read("x${1|only|}");
        assert_eq!(text, "xonly");
        assert_eq!(stops, vec![(1, 5)]);
    }

    #[test]
    fn a_nested_default_keeps_its_braces() {
        let (text, stops) = read("if (${1:${2:x}})");
        assert_eq!(text, "if (x)");
        assert_eq!(
            stops,
            vec![(4, 5), (4, 5)],
            "the outer stop is the `x` as well: the default is what it holds"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer::Buffer;

    #[test]
    fn the_word_at_the_cursor_is_what_has_been_typed() {
        let buffer = Buffer::from_text("let counter = counter_one;\n");
        // Columns are insertion points: column 9 is just after the `t`.
        let at = buffer.line_col_to_char(0, 9);
        let (start, prefix) = prefix_at(&buffer, at);
        assert_eq!(prefix, "count");
        assert_eq!(start, 4);

        // Just after a space there is nothing typed yet, and the prefix is empty
        // at the cursor rather than the word before it.
        let at = buffer.line_col_to_char(0, 4);
        let (start, prefix) = prefix_at(&buffer, at);
        assert_eq!(prefix, "");
        assert_eq!(start, 4, "the empty word is at the cursor, not before it");
    }

    #[test]
    fn words_come_from_the_buffer_nearest_the_cursor_first() {
        let buffer = Buffer::from_text("alpha beta\ngamma\ndelta alpha\n");
        let items = words(&buffer, 1, 100);
        let labels: Vec<&str> = items.iter().map(|item| item.label.as_str()).collect();
        // The cursor's own line first, in reading order, and then outwards.
        assert_eq!(labels, ["gamma", "alpha", "beta", "delta"], "{labels:?}");

        // One of each: `alpha` appears twice in the file and once in the list.
        assert_eq!(labels.iter().filter(|label| **label == "alpha").count(), 1);
        // And a limit is a limit.
        assert_eq!(words(&buffer, 1, 2).len(), 2);
    }

    #[test]
    fn short_words_are_not_worth_offering() {
        let buffer = Buffer::from_text("a bb c dddd\n");
        let labels: Vec<String> = words(&buffer, 0, 100)
            .into_iter()
            .map(|item| item.label)
            .collect();
        assert_eq!(labels, ["bb", "dddd"], "one letter is noise");
    }

    #[test]
    fn only_what_the_prefix_starts_is_offered() {
        let items = vec![
            Item::plain("counter"),
            Item::plain("count"),
            Item::plain("colour"),
        ];
        let labels: Vec<String> = matching(&items, "coun")
            .into_iter()
            .map(|item| item.label)
            .collect();
        assert_eq!(labels, ["counter", "count"], "in the order they came in");
        assert_eq!(matching(&items, "").len(), 3, "an empty prefix takes all");
        assert!(matching(&items, "zzz").is_empty());
    }
}
