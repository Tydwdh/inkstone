//! The jump list: where `<C-o>` and `<C-i>` walk, and what `''` asks for.
//!
//! A *jump* is a key that travels — a mark, a search, `%` — and not a motion
//! within a line, which is vim's rule as well. Each one leaves the place it
//! started in this list, and the list has a place in it that is where the cursor
//! is *not*: the entry it came from. Stepping back and forward through the list
//! is what `<C-o>` and `<C-i>` do.
//!
//! The list is written to before the cursor moves, and the place left is only
//! added when it is not already the entry under the index. That is what makes
//! the walk work: after a jump the list holds where it started, and asking to go
//! back adds where it is *now* first — so `<C-o>` and `<C-i>` have both ends to
//! travel between, and a key pressed twice comes back.

use crate::cursor::Cursor;

/// How many places are remembered. Vim keeps a hundred, and the oldest are the
/// ones nobody misses.
pub const MAX_JUMPS: usize = 100;

/// The places a document has been looked at from, and where in them the walk is.
#[derive(Debug, Clone, Default)]
pub struct Jumps {
    places: Vec<Cursor>,
    /// Which entry is the one being walked from. Entries after it are the way
    /// forward and are dropped when a new jump is made from somewhere else.
    index: usize,
}

impl Jumps {
    /// Remember a place, unless it is the one already under the index.
    ///
    /// This is called *before* the cursor moves, with where it is now, and
    /// truncates whatever was ahead: a jump made from the middle of the list
    /// starts a new history there, the way an undo does.
    pub fn record(&mut self, cursor: Cursor) {
        if self.places.get(self.index) == Some(&cursor) {
            return;
        }
        self.places.truncate(self.index + 1);
        self.places.push(cursor);
        if self.places.len() > MAX_JUMPS {
            self.places.remove(0);
        }
        self.index = self.places.len() - 1;
    }

    /// Step back one place in the list, and say where that is.
    pub fn back(&mut self) -> Option<Cursor> {
        if self.index == 0 {
            return None;
        }
        self.index -= 1;
        self.places.get(self.index).copied()
    }

    /// Step forward one place in the list, and say where that is.
    pub fn forward(&mut self) -> Option<Cursor> {
        if self.index + 1 >= self.places.len() {
            return None;
        }
        self.index += 1;
        self.places.get(self.index).copied()
    }

    /// Forget every place, which is what `:clearjumps` does.
    pub fn clear(&mut self) {
        self.places.clear();
        self.index = 0;
    }

    /// Whether there is anything to walk.
    pub fn is_empty(&self) -> bool {
        self.places.is_empty()
    }

    /// Which entry the walk is on, counting from one, for `:jumps`.
    pub fn position(&self) -> usize {
        self.index + 1
    }

    /// Every place, oldest first, and which one the walk is on.
    pub fn iter(&self) -> impl Iterator<Item = (usize, &Cursor)> + '_ {
        self.places.iter().enumerate()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cursor(row: usize, col: usize) -> Cursor {
        Cursor::new(row, col)
    }

    #[test]
    fn a_walk_goes_back_and_forward_between_two_places() {
        let mut jumps = Jumps::default();
        assert!(jumps.is_empty());
        assert_eq!(jumps.back(), None, "nowhere to go");
        // A jump from (0,0) left there, and the walk is at it.
        jumps.record(cursor(0, 0));
        assert_eq!(jumps.position(), 1);
        // Asking to go back from (3,2) remembers that first, then steps.
        jumps.record(cursor(3, 2));
        assert_eq!(jumps.back(), Some(cursor(0, 0)), "where it came from");
        assert_eq!(jumps.forward(), Some(cursor(3, 2)), "and forward again");
        assert_eq!(jumps.forward(), None, "there is no further forward");
        assert_eq!(jumps.back(), Some(cursor(0, 0)));
        assert_eq!(jumps.back(), None, "nor further back");
    }

    #[test]
    fn a_place_already_under_the_index_is_not_added_twice() {
        let mut jumps = Jumps::default();
        jumps.record(cursor(1, 1));
        jumps.record(cursor(1, 1));
        assert_eq!(jumps.iter().count(), 1, "the same place once");
        // A new jump from the middle of the list drops what was ahead.
        jumps.record(cursor(2, 2));
        assert_eq!(jumps.back(), Some(cursor(1, 1)));
        jumps.record(cursor(9, 9));
        assert_eq!(jumps.iter().count(), 2, "(1,1) then (9,9): (2,2) is gone");
        assert_eq!(jumps.forward(), None);
    }

    #[test]
    fn the_list_stops_growing_at_the_hundredth_place() {
        let mut jumps = Jumps::default();
        for row in 0..(MAX_JUMPS + 20) {
            jumps.record(cursor(row, 0));
        }
        assert_eq!(jumps.iter().count(), MAX_JUMPS);
        assert_eq!(
            jumps.position(),
            MAX_JUMPS,
            "the newest is the walk's place"
        );
        // The oldest are the ones dropped.
        assert_eq!(jumps.iter().next().map(|(_, c)| c.row), Some(20));
    }
}
