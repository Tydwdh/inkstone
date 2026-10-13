//! Cursor position and movement.
//!
//! A cursor is a `(row, col)` pair in *logical* coordinates: `row` is a buffer
//! line and `col` is a character offset within that line, excluding the
//! newline. It is not a screen position; the viewport translates it.
//!
//! `col` is an *insertion point*, so it ranges over `0..=line_len(row)`. A
//! column of `line_len(row)` sits just past the last character, on the
//! terminating newline. Normal mode does not clamp one character back (as vim
//! does); the model stays uniform and mode-independent for milestone 1.

use crate::buffer::Buffer;

/// A character is part of a "word" if it is alphanumeric or `_`.
fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Cursor {
    pub row: usize,
    pub col: usize,
}

impl Cursor {
    pub fn new(row: usize, col: usize) -> Self {
        Self { row, col }
    }

    /// Pull the cursor back inside the buffer.
    pub fn clamp(&mut self, buf: &Buffer) {
        self.row = self.row.min(buf.line_count().saturating_sub(1));
        self.col = self.col.min(buf.line_len(self.row));
    }

    /// The character index of this cursor within the buffer.
    pub fn char_idx(&self, buf: &Buffer) -> usize {
        buf.line_col_to_char(self.row, self.col)
    }

    pub fn set_char_idx(&mut self, buf: &Buffer, char_idx: usize) {
        let (row, col) = buf.char_to_line_col(char_idx);
        self.row = row;
        self.col = col;
    }

    pub fn move_left(&mut self, buf: &Buffer) {
        if self.col > 0 {
            self.col -= 1;
        } else if self.row > 0 {
            self.row -= 1;
            self.col = buf.line_len(self.row);
        }
    }

    pub fn move_right(&mut self, buf: &Buffer) {
        if self.col < buf.line_len(self.row) {
            self.col += 1;
        } else if self.row + 1 < buf.line_count() {
            self.row += 1;
            self.col = 0;
        }
    }

    pub fn move_up(&mut self, buf: &Buffer) {
        if self.row > 0 {
            self.row -= 1;
            self.col = self.col.min(buf.line_len(self.row));
        }
    }

    pub fn move_down(&mut self, buf: &Buffer) {
        if self.row + 1 < buf.line_count() {
            self.row += 1;
            self.col = self.col.min(buf.line_len(self.row));
        }
    }

    pub fn move_line_start(&mut self, _buf: &Buffer) {
        self.col = 0;
    }

    pub fn move_line_end(&mut self, buf: &Buffer) {
        self.col = buf.line_len(self.row);
    }

    /// Move to the start of the next word (vim's `w`).
    pub fn move_word_forward(&mut self, buf: &Buffer) {
        let len = buf.len_chars();
        let mut idx = self.char_idx(buf);
        if idx + 1 >= len {
            self.set_char_idx(buf, len.saturating_sub(1));
            return;
        }
        idx += 1;
        while idx < len {
            let prev = buf.char_at(idx - 1).expect("index in range");
            let cur = buf.char_at(idx).expect("index in range");
            let entering_word = is_word_char(cur) && !is_word_char(prev);
            let leaving_space = !cur.is_whitespace() && prev.is_whitespace();
            if entering_word || leaving_space {
                break;
            }
            idx += 1;
        }
        let idx = idx.min(len - 1);
        self.set_char_idx(buf, idx);
    }

    /// Move to the start of the current or previous word (vim's `b`).
    pub fn move_word_backward(&mut self, buf: &Buffer) {
        let mut idx = self.char_idx(buf);
        if idx == 0 {
            return;
        }
        idx -= 1;
        while idx > 0 {
            let prev = buf.char_at(idx - 1).expect("index in range");
            let cur = buf.char_at(idx).expect("index in range");
            let entering_word = is_word_char(cur) && !is_word_char(prev);
            let leaving_space = !cur.is_whitespace() && prev.is_whitespace();
            if entering_word || leaving_space {
                break;
            }
            idx -= 1;
        }
        self.set_char_idx(buf, idx);
    }

    /// Move to the last line's start (vim's `G`).
    pub fn goto_last_line(&mut self, buf: &Buffer) {
        self.row = buf.line_count().saturating_sub(1);
        self.col = 0;
    }

    pub fn goto_first_line(&mut self, _buf: &Buffer) {
        self.row = 0;
        self.col = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buf(text: &str) -> Buffer {
        Buffer::from_text(text)
    }

    #[test]
    fn horizontal_movement_wraps_lines() {
        let b = buf("ab\ncd\n");
        let mut c = Cursor::new(0, 0);
        c.move_left(&b);
        assert_eq!(c, Cursor::new(0, 0), "left at the very start is a no-op");

        c.col = 2;
        c.move_right(&b);
        assert_eq!(c, Cursor::new(1, 0), "right past the line end wraps down");

        c.move_left(&b);
        assert_eq!(
            c,
            Cursor::new(0, 2),
            "left at column 0 wraps up to the line end"
        );
    }

    #[test]
    fn vertical_movement_clamps_the_column() {
        let b = buf("long line\nab\n");
        let mut c = Cursor::new(0, 8);
        c.move_down(&b);
        assert_eq!(c, Cursor::new(1, 2), "column clamps to the shorter line");
        c.move_up(&b);
        assert_eq!(
            c,
            Cursor::new(0, 2),
            "the clamped column is not restored (M1)"
        );
    }

    #[test]
    fn vertical_movement_stops_at_the_edges() {
        let b = buf("a\nb\n");
        let mut c = Cursor::new(0, 0);
        c.move_up(&b);
        assert_eq!(c.row, 0);
        c.goto_last_line(&b);
        assert_eq!(c, Cursor::new(1, 0));
        c.move_down(&b);
        assert_eq!(c, Cursor::new(1, 0));
    }

    #[test]
    fn line_start_and_end() {
        let b = buf("héllo\n");
        let mut c = Cursor::new(0, 3);
        c.move_line_end(&b);
        assert_eq!(c.col, 5, "columns are characters, not bytes");
        c.move_line_start(&b);
        assert_eq!(c.col, 0);
    }

    #[test]
    fn cursor_clamps_into_range() {
        let b = buf("ab\n");
        let mut c = Cursor::new(99, 99);
        c.clamp(&b);
        assert_eq!(c, Cursor::new(0, 2));
    }

    #[test]
    fn word_motion_forward() {
        let b = buf("foo bar_baz qux\n");
        let mut c = Cursor::new(0, 0);
        c.move_word_forward(&b);
        assert_eq!(c.col, 4, "to the start of `bar_baz`");
        c.move_word_forward(&b);
        assert_eq!(c.col, 12, "to the start of `qux`");
        c.move_word_forward(&b);
        assert_eq!(c.col, 15, "stops on the last character of the line");
    }

    #[test]
    fn word_motion_backward() {
        let b = buf("foo bar_baz qux\n");
        let mut c = Cursor::new(0, 15);
        c.move_word_backward(&b);
        assert_eq!(c.col, 12);
        c.move_word_backward(&b);
        assert_eq!(c.col, 4);
        c.move_word_backward(&b);
        assert_eq!(c.col, 0);
    }

    #[test]
    fn word_motion_reaches_the_next_line() {
        let b = buf("alpha\nbeta\n");
        let mut c = Cursor::new(0, 0);
        c.move_word_forward(&b);
        assert_eq!(c, Cursor::new(1, 0));
    }

    #[test]
    fn char_index_round_trip() {
        let b = buf("ab\ncdef\n");
        let mut c = Cursor::new(1, 2);
        assert_eq!(c.char_idx(&b), 5);
        c.set_char_idx(&b, 1);
        assert_eq!(c, Cursor::new(0, 1));
    }
}
