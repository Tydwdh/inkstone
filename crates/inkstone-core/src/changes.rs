//! The change list: what `g;` and `g,` walk.
//!
//! Every edit leaves the place it happened in this list, and `:changes` shows
//! them. Vim's list has one entry per *line changed*, not one per keystroke —
//! typing a word into a line is one entry — so a place already in the list is
//! moved to the end rather than added again, and the list is bounded the way the
//! jump list is.
//!
//! Undo and redo do not add to it: they put text back where it was, and an entry
//! for that would say an edit happened where none did. The places are rows and
//! columns as they were when the edit was made, so a change that inserts lines
//! above one of them moves it out from under itself — the same limitation the
//! marks have, and handled the same way: the place is clamped into the text when
//! it is walked to.

use crate::cursor::Cursor;

/// How many changes are remembered. Vim's list is bounded too, by its
/// `'undolevels'`-ish bookkeeping rather than a number; a hundred is enough to
/// walk back through a session's work.
pub const MAX_CHANGES: usize = 100;

/// The places edits happened, oldest first.
#[derive(Debug, Clone, Default)]
pub struct Changes {
    places: Vec<Cursor>,
    /// Which entry the walk is on: the most recent change, until `g;` moves it.
    index: usize,
}

impl Changes {
    /// Note that an edit happened at `cursor`.
    ///
    /// A place already in the list is moved to the end instead of being added
    /// again, so that a line typed into is one entry; anything ahead of the walk
    /// is dropped, since editing from there makes a new list from there.
    pub fn record(&mut self, cursor: Cursor) {
        if let Some(at) = self.places.iter().position(|place| place.row == cursor.row) {
            self.places.remove(at);
        }
        // Everything up to the walk is kept, and everything after it is dropped:
        // editing from the middle of the list makes a new list from there.
        let keep = (self.index + 1).min(self.places.len());
        self.places.truncate(keep);
        self.places.push(cursor);
        if self.places.len() > MAX_CHANGES {
            self.places.remove(0);
        }
        self.index = self.places.len().saturating_sub(1);
    }

    /// Step back to the change before this one, and say where it is.
    pub fn back(&mut self) -> Option<Cursor> {
        if self.index == 0 {
            return None;
        }
        self.index -= 1;
        self.places.get(self.index).copied()
    }

    /// Step forward again.
    pub fn forward(&mut self) -> Option<Cursor> {
        if self.index + 1 >= self.places.len() {
            return None;
        }
        self.index += 1;
        self.places.get(self.index).copied()
    }

    /// Whether no edit has been made.
    pub fn is_empty(&self) -> bool {
        self.places.is_empty()
    }

    /// Which entry the walk is on, counting from one.
    pub fn position(&self) -> usize {
        self.index + 1
    }

    /// Every change, oldest first, with its number.
    pub fn iter(&self) -> impl Iterator<Item = (usize, &Cursor)> + '_ {
        self.places.iter().enumerate()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_changed_twice_is_one_entry() {
        let mut changes = Changes::default();
        assert!(changes.is_empty());
        changes.record(Cursor::new(0, 0));
        changes.record(Cursor::new(0, 4));
        assert_eq!(changes.iter().count(), 1, "the same line, one entry");
        assert_eq!(
            changes.iter().next().map(|(_, c)| c.col),
            Some(4),
            "the latest"
        );
        changes.record(Cursor::new(2, 0));
        assert_eq!(changes.iter().count(), 2);
        assert_eq!(changes.position(), 2, "the walk is on the newest");
    }

    #[test]
    fn the_walk_goes_back_and_forward() {
        let mut changes = Changes::default();
        for row in 0..3 {
            changes.record(Cursor::new(row, 0));
        }
        assert_eq!(changes.back(), Some(Cursor::new(1, 0)));
        assert_eq!(changes.back(), Some(Cursor::new(0, 0)));
        assert_eq!(changes.back(), None, "nothing older");
        assert_eq!(changes.forward(), Some(Cursor::new(1, 0)));
        assert_eq!(changes.forward(), Some(Cursor::new(2, 0)));
        assert_eq!(changes.forward(), None, "nothing newer");
    }

    #[test]
    fn editing_after_walking_starts_a_new_list_from_there() {
        let mut changes = Changes::default();
        for row in 0..3 {
            changes.record(Cursor::new(row, 0));
        }
        changes.back();
        changes.back();
        changes.record(Cursor::new(9, 9));
        assert_eq!(changes.iter().count(), 2, "(0,0) and (9,9)");
        assert_eq!(changes.forward(), None);
        assert_eq!(changes.position(), 2);
    }

    #[test]
    fn the_list_stops_growing_at_the_hundredth_change() {
        let mut changes = Changes::default();
        for row in 0..(MAX_CHANGES + 10) {
            changes.record(Cursor::new(row, 0));
        }
        assert_eq!(changes.iter().count(), MAX_CHANGES);
        assert_eq!(changes.iter().next().map(|(_, c)| c.row), Some(10));
    }
}
