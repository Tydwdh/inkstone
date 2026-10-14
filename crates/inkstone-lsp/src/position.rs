//! Positions: the protocol counts UTF-16 code units, this editor counts
//! characters.
//!
//! The two agree for everything in the basic multilingual plane — every
//! character in a Latin, Greek, Cyrillic, Hebrew, Arabic, Devanagari or CJK
//! document, which is nearly every document. They disagree for the rest: an
//! emoji is one character and two code units, so a server that says "column 5"
//! after an emoji means the sixth character.
//!
//! The conversion needs the line's text, which is why it is a pair of functions
//! here rather than a field somewhere: nothing else in the editor has to know
//! that UTF-16 exists.

/// Where a UTF-16 offset in `line` lands in characters.
///
/// An offset in the middle of a surrogate pair lands on the character that owns
/// that unit — there is no way to point at half of one, and pointing *before* it
/// is the reading that keeps a range covering the character whole. An offset
/// past the end of the line is the end of the line.
pub fn utf16_to_char(line: &str, utf16: usize) -> usize {
    let mut units = 0usize;
    for (chars, c) in line.chars().enumerate() {
        if units + c.len_utf16() > utf16 {
            return chars;
        }
        units += c.len_utf16();
    }
    line.chars().count()
}

/// Where a character offset in `line` lands in UTF-16 code units.
pub fn char_to_utf16(line: &str, chars: usize) -> usize {
    line.chars().take(chars).map(char::len_utf16).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_is_the_same_on_both_sides() {
        let line = "let x = 1;";
        for count in 0..=line.chars().count() {
            assert_eq!(utf16_to_char(line, count), count);
            assert_eq!(char_to_utf16(line, count), count);
        }
    }

    #[test]
    fn wide_characters_are_one_of_each() {
        let line = "名前 = 1";
        assert_eq!(utf16_to_char(line, 0), 0);
        assert_eq!(utf16_to_char(line, 2), 2, "two CJK characters in");
        assert_eq!(utf16_to_char(line, 3), 3, "then the space");
        // BMP characters are one code unit each, so the two sides agree.
        assert_eq!(char_to_utf16(line, 2), 2);
    }

    #[test]
    fn a_character_outside_the_plane_is_two_units_and_one_character() {
        // U+1F600 is one character and two UTF-16 code units.
        let line = "a\u{1F600}b";
        assert_eq!(line.chars().count(), 3);
        assert_eq!(line.chars().map(char::len_utf16).sum::<usize>(), 4);

        assert_eq!(utf16_to_char(line, 0), 0, "before it");
        assert_eq!(utf16_to_char(line, 1), 1, "at its first unit");
        assert_eq!(utf16_to_char(line, 2), 1, "half way through it is still it");
        assert_eq!(utf16_to_char(line, 3), 2, "after it");
        assert_eq!(char_to_utf16(line, 1), 1);
        assert_eq!(char_to_utf16(line, 2), 3, "one character, two units");
    }

    #[test]
    fn an_offset_past_the_end_is_the_end() {
        let line = "abc";
        assert_eq!(utf16_to_char(line, 99), 3);
        assert_eq!(char_to_utf16(line, 99), 3);
        assert_eq!(utf16_to_char("", 4), 0);
    }

    #[test]
    fn the_two_conversions_are_inverses_at_character_boundaries() {
        let line = "x\u{1F600}é名";
        for chars in 0..=line.chars().count() {
            let units = char_to_utf16(line, chars);
            assert_eq!(utf16_to_char(line, units), chars, "{chars} came back wrong");
        }
    }
}
