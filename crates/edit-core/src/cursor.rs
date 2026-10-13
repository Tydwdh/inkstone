//! Cursor position and movement.
//!
//! A cursor is a `(row, col)` pair in *logical* coordinates: `row` is a buffer
//! line and `col` is a character offset within that line, excluding the
//! newline. It is not a screen position; the viewport translates it.
//!
//! `col` is an *insertion point*, so it ranges over `0..=line_len(row)`. A
//! column of `line_len(row)` sits just past the last character, on the
//! terminator — which is exactly where insert mode belongs, and vim keeps the
//! cursor on the last *character* instead. The editor applies that clamp on
//! the normal-mode side; these motions stay mode-independent.

use crate::buffer::Buffer;

/// How a character participates in a word motion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    /// Whitespace, including the line terminator.
    Blank,
    /// Alphanumeric or `_`.
    Word,
    /// Anything else.
    Punct,
}

fn class(c: char) -> Class {
    // `\r` is always part of a terminator here: the line model strips
    // terminators, and the rope splits lines on a lone CR, so a carriage
    // return can never appear inside a line's content.
    if c.is_whitespace() || c == '\r' {
        Class::Blank
    } else if c.is_alphanumeric() || c == '_' {
        Class::Word
    } else {
        Class::Punct
    }
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

    /// Move to the last character of the line (vim's `$`), or column 0 on an
    /// empty line.
    pub fn move_line_end(&mut self, buf: &Buffer) {
        self.col = buf.line_len(self.row).saturating_sub(1);
    }

    /// Move to the start of the next word (vim's `w`).
    pub fn move_word_forward(&mut self, buf: &Buffer) {
        let len = buf.len_chars();
        if len == 0 {
            return;
        }
        let mut idx = self.char_idx(buf);

        // Step over the run the cursor is sitting on.
        let current = class(buf.char_at(idx).expect("index in range"));
        if current != Class::Blank {
            while idx < len && class(buf.char_at(idx).expect("index in range")) == current {
                idx += 1;
            }
        }

        // Step over blanks. Crossing a newline steps onto the next line: a
        // blank line is skipped, an empty one is a word and stops the motion.
        while idx < len {
            let c = buf.char_at(idx).expect("index in range");
            if c == '\n' {
                idx += 1;
                if buf.char_at(idx) == Some('\n') {
                    break;
                }
                continue;
            }
            if class(c) == Class::Blank {
                idx += 1;
                continue;
            }
            break;
        }

        self.set_char_idx(buf, idx.min(len - 1));
    }

    /// Move to the start of the current or previous word (vim's `b`).
    pub fn move_word_backward(&mut self, buf: &Buffer) {
        let mut idx = self.char_idx(buf);
        if idx == 0 {
            return;
        }
        idx -= 1;

        // Step back over blanks; an empty line is a word, so stop on it.
        while idx > 0 {
            let c = buf.char_at(idx).expect("index in range");
            if c == '\n' {
                if buf.char_at(idx - 1) == Some('\n') {
                    break;
                }
                idx -= 1;
                continue;
            }
            if class(c) == Class::Blank {
                idx -= 1;
                continue;
            }
            break;
        }

        // Walk back to the start of the run we landed on.
        let current = class(buf.char_at(idx).expect("index in range"));
        if current != Class::Blank {
            while idx > 0 && class(buf.char_at(idx - 1).expect("index in range")) == current {
                idx -= 1;
            }
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
        assert_eq!(
            c.col, 4,
            "columns are characters, not bytes, and `$` sits on the last one"
        );
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

    #[test]
    fn word_motion_forward_treats_an_empty_line_as_a_word() {
        let b = buf("a\n\nb\n");
        let mut c = Cursor::new(0, 0);
        c.move_word_forward(&b);
        assert_eq!(c, Cursor::new(1, 0), "an empty line counts as a word");
        c.move_word_forward(&b);
        assert_eq!(c, Cursor::new(2, 0));
    }

    #[test]
    fn word_motion_forward_crosses_a_crlf_line_break() {
        let b = buf("one\r\ntwo\r\n");
        let mut c = Cursor::new(0, 0);
        c.move_word_forward(&b);
        assert_eq!(c, Cursor::new(1, 0), "the carriage return is not a word");
        c.move_word_backward(&b);
        assert_eq!(c, Cursor::new(0, 0));
    }

    #[test]
    fn word_motion_forward_skips_blank_lines_that_are_not_empty() {
        let b = buf("a\n   \nb\n");
        let mut c = Cursor::new(0, 0);
        c.move_word_forward(&b);
        assert_eq!(c, Cursor::new(2, 0), "a blank line is not a word");
    }

    #[test]
    fn word_motion_forward_stops_at_the_start_of_a_punctuation_run() {
        let b = buf("foo(bar)\n");
        let mut c = Cursor::new(0, 0);
        c.move_word_forward(&b);
        assert_eq!(c.col, 3);
        c.move_word_forward(&b);
        assert_eq!(c.col, 4);
    }

    #[test]
    fn word_motion_backward_treats_an_empty_line_as_a_word() {
        let b = buf("a\n\nb\n");
        let mut c = Cursor::new(2, 0);
        c.move_word_backward(&b);
        assert_eq!(c, Cursor::new(1, 0));
        c.move_word_backward(&b);
        assert_eq!(c, Cursor::new(0, 0));
    }

    #[test]
    fn line_end_lands_on_the_last_character() {
        let b = buf("hello\n");
        let mut c = Cursor::new(0, 0);
        c.move_line_end(&b);
        assert_eq!(c.col, 4, "vim's `$` sits on the last character");
        c.move_line_end(&b);
        assert_eq!(c.col, 4);
    }

    #[test]
    fn line_end_on_an_empty_line_stays_at_column_zero() {
        let b = buf("\n");
        let mut c = Cursor::new(0, 0);
        c.move_line_end(&b);
        assert_eq!(c.col, 0);
    }
}
