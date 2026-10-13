//! The screen model.
//!
//! The screen model itself ([`Grid`], [`Cell`], [`Color`]) is defined once in
//! `inkstone-rpc`, because the daemon and every frontend must agree on it
//! byte-for-byte. This module re-exports it and adds the *logical* geometry
//! ([`Viewport`]) that mapping the cursor onto a grid requires.

pub use inkstone_rpc::{Cell, Color, Grid, ATTR_BOLD, ATTR_REVERSE, ATTR_UNDERLINE};

use crate::buffer::Buffer;
use crate::cursor::Cursor;

/// Rows reserved at the bottom of the screen: one status line and one
/// command/message line.
pub const CHROME_ROWS: u16 = 2;

/// The visible window onto the buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Viewport {
    /// Index of the first visible buffer line.
    pub top_line: usize,
    /// Character offset of the first visible column (horizontal scroll).
    pub left_col: usize,
    /// Terminal width in columns.
    pub width: u16,
    /// Terminal height in rows.
    pub height: u16,
}

impl Viewport {
    pub fn new(width: u16, height: u16) -> Self {
        Self {
            top_line: 0,
            left_col: 0,
            width,
            height,
        }
    }

    pub fn resize(&mut self, width: u16, height: u16) {
        self.width = width;
        self.height = height;
    }

    /// Rows available for text.
    pub fn text_rows(&self) -> u16 {
        self.height.saturating_sub(CHROME_ROWS)
    }
}

/// Scroll `vp` the minimum amount needed to bring `cur` back into view.
///
/// `gutter` is the width of the line-number column: it eats into the columns
/// available for text, so a viewport that ignored it would scroll too little
/// and leave the cursor stranded past the right edge.
pub fn ensure_cursor_visible(vp: &mut Viewport, cur: Cursor, gutter: u16) {
    // A degenerate viewport has no room to scroll into, so the cursor's own
    // position becomes the origin rather than an invented offset.
    let rows = vp.text_rows() as usize;
    if rows == 0 || cur.row < vp.top_line {
        vp.top_line = cur.row;
    } else if cur.row >= vp.top_line + rows {
        vp.top_line = cur.row + 1 - rows;
    }

    let columns = vp.width.saturating_sub(gutter) as usize;
    if columns == 0 || cur.col < vp.left_col {
        vp.left_col = cur.col;
    } else if cur.col >= vp.left_col + columns {
        vp.left_col = cur.col + 1 - columns;
    }
}

/// Build a viewport from scratch that contains both `cur` and (optionally) the
/// previous viewport, preferring the previous scroll position when possible.
pub fn scroll_to_cursor(vp: &mut Viewport, cur: Cursor, buf: &Buffer, gutter: u16) {
    ensure_cursor_visible(vp, cur, gutter);
    // Never scroll past the end of the document if it fits.
    let lines = buf.line_count();
    let rows = vp.text_rows() as usize;
    if rows == 0 {
        // Nothing is on screen, so there is no sensible scroll offset.
        return;
    }
    if lines <= rows {
        vp.top_line = 0;
    } else if vp.top_line + rows > lines {
        vp.top_line = lines - rows;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn viewport_geometry() {
        let vp = Viewport::new(80, 24);
        assert_eq!(vp.text_rows(), 22);

        let tiny = Viewport::new(80, 1);
        assert_eq!(tiny.text_rows(), 0);
    }

    #[test]
    fn ensure_visible_scrolls_down_minimally() {
        let mut vp = Viewport::new(80, 12); // 10 text rows
        vp.top_line = 0;
        ensure_cursor_visible(&mut vp, Cursor::new(10, 0), 0);
        assert_eq!(vp.top_line, 1);
        ensure_cursor_visible(&mut vp, Cursor::new(11, 0), 0);
        assert_eq!(vp.top_line, 2);
        // Already visible: no movement.
        ensure_cursor_visible(&mut vp, Cursor::new(5, 0), 0);
        assert_eq!(vp.top_line, 2);
    }

    #[test]
    fn ensure_visible_scrolls_up() {
        let mut vp = Viewport::new(80, 12);
        vp.top_line = 8;
        ensure_cursor_visible(&mut vp, Cursor::new(3, 0), 0);
        assert_eq!(vp.top_line, 3);
    }

    #[test]
    fn a_line_number_gutter_reduces_the_room_for_text() {
        let mut vp = Viewport::new(10, 12);
        // 2 columns go to the gutter, so column 8 is the last fully visible one.
        ensure_cursor_visible(&mut vp, Cursor::new(0, 7), 2);
        assert_eq!(vp.left_col, 0);
        ensure_cursor_visible(&mut vp, Cursor::new(0, 8), 2);
        assert_eq!(vp.left_col, 1);
    }

    #[test]
    fn a_degenerate_viewport_pins_the_cursor_to_the_visible_origin() {
        let mut vp = Viewport::new(0, 0);
        ensure_cursor_visible(&mut vp, Cursor::new(7, 4), 0);
        assert_eq!((vp.top_line, vp.left_col), (7, 4));

        let buf = Buffer::from_text(&"x\n".repeat(10));
        let mut vp = Viewport::new(80, 0);
        vp.top_line = 3;
        scroll_to_cursor(&mut vp, Cursor::new(3, 0), &buf, 0);
        assert_eq!(
            vp.top_line, 3,
            "a zero-height viewport keeps the cursor line"
        );
    }

    #[test]
    fn ensure_visible_scrolls_right() {
        let mut vp = Viewport::new(10, 12);
        vp.top_line = 0;
        ensure_cursor_visible(&mut vp, Cursor::new(0, 9), 0);
        assert_eq!(vp.left_col, 0, "column 9 is the last visible one");
        ensure_cursor_visible(&mut vp, Cursor::new(0, 10), 0);
        assert_eq!(vp.left_col, 1);
    }

    #[test]
    fn scroll_to_cursor_does_not_over_scroll_short_documents() {
        let buf = Buffer::from_text("a\nb\nc\n");
        let mut vp = Viewport::new(80, 12); // 10 text rows
        vp.top_line = 7;
        scroll_to_cursor(&mut vp, Cursor::new(0, 0), &buf, 0);
        assert_eq!(vp.top_line, 0, "3 lines fit, so the view starts at the top");

        let long = Buffer::from_text("a\nb\nc\nd\ne\nf\ng\nh\ni\nj\nk\nl\n");
        let mut vp = Viewport::new(80, 12);
        scroll_to_cursor(&mut vp, Cursor::new(11, 0), &long, 0);
        assert_eq!(
            vp.top_line, 2,
            "12 lines in 10 rows: the last screen starts at 2"
        );
    }
}
