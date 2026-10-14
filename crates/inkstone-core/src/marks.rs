//! Marks: `m{a-z}` puts one down, `` `{a-z} `` and `'{a-z}` go back to it.
//!
//! A mark is a place in a file, so it belongs to the file and not to the
//! window looking at it: switch documents and the marks go with the text, come
//! back and they are where they were. A mark is *not* adjusted by later edits,
//! which is the one thing vim does that this does not — insert a paragraph
//! above a mark and it is a paragraph short of where it was put. Vim earns
//! that with bookkeeping in every edit; here the position is clamped into the
//! text when it is jumped to, so a mark can never point past the end.

use crate::cursor::Cursor;

/// How a mark is gone to: the exact place `` `a `` was put, or the first
/// non-blank character of its line, which is what `'a` goes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MarkJump {
    /// `m` — put a mark down rather than travelling to one.
    Set,
    /// `` ` `` — the character the mark was put on.
    Exact,
    /// `'` — the line it was put on, at its first non-blank character.
    Line,
}

/// Twenty-six marks, one per lower-case letter.
///
/// The array is fixed because the letters are: `a` is slot zero, and a mark
/// nobody set is `None` rather than a default position that would look set.
#[derive(Debug, Clone, Default)]
pub(crate) struct Marks {
    slots: [Option<Cursor>; 26],
}

impl Marks {
    /// Which slot a letter names, if it is a letter this has slots for.
    fn slot(letter: char) -> Option<usize> {
        letter
            .is_ascii_lowercase()
            .then(|| letter as usize - 'a' as usize)
    }

    /// Where a mark is, if it has been put down.
    pub fn get(&self, letter: char) -> Option<Cursor> {
        self.slots[Self::slot(letter)?]
    }

    /// Put a mark down, or move one that is already down.
    pub fn set(&mut self, letter: char, cursor: Cursor) {
        if let Some(slot) = Self::slot(letter) {
            self.slots[slot] = Some(cursor);
        }
    }

    /// Take a mark away, which is what `:delmarks` does.
    pub fn clear(&mut self, letter: char) {
        if let Some(slot) = Self::slot(letter) {
            self.slots[slot] = None;
        }
    }

    /// Take every mark away, which is what `:delmarks!` does for the buffer
    /// that is on screen.
    pub(crate) fn clear_all(&mut self) {
        self.slots = [None; 26];
    }

    /// Whether nothing has been marked.
    pub fn is_empty(&self) -> bool {
        self.slots.iter().all(Option::is_none)
    }

    /// Every mark, in alphabetical order — which is the order `:marks` wants.
    pub fn iter(&self) -> impl Iterator<Item = (char, Cursor)> + '_ {
        self.slots
            .iter()
            .enumerate()
            .filter_map(|(slot, cursor)| cursor.map(|cursor| ((b'a' + slot as u8) as char, cursor)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mark_is_put_down_found_and_taken_away() {
        let mut marks = Marks::default();
        assert!(marks.is_empty());
        assert_eq!(marks.get('a'), None);
        marks.set('a', Cursor::new(3, 7));
        marks.set('z', Cursor::new(0, 0));
        assert_eq!(marks.get('a'), Some(Cursor::new(3, 7)));
        assert_eq!(marks.get('z'), Some(Cursor::new(0, 0)));
        assert_eq!(marks.get('b'), None, "nobody set that one");
        // Setting one again moves it.
        marks.set('a', Cursor::new(9, 1));
        assert_eq!(marks.get('a'), Some(Cursor::new(9, 1)));
        // In alphabetical order, which is the order the list wants.
        assert_eq!(
            marks
                .iter()
                .map(|(letter, _)| letter)
                .collect::<Vec<char>>(),
            vec!['a', 'z']
        );
        marks.clear('a');
        assert_eq!(marks.get('a'), None);
        assert!(!marks.is_empty(), "the other one is still there");
        // A letter this has no slot for is not a mark and cannot be one.
        marks.set('A', Cursor::new(1, 1));
        marks.set('1', Cursor::new(1, 1));
        assert!(marks.get('A').is_none());
        assert_eq!(marks.iter().count(), 1);
    }
}
