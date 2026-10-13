//! The screen model.
//!
//! The screen model itself ([`Grid`], [`Cell`], [`Color`]) is defined once in
//! `edit-rpc`, because the daemon and every frontend must agree on it
//! byte-for-byte. This module re-exports it and adds the *logical* geometry
//! ([`Viewport`]) that mapping the cursor onto a grid requires.

pub use edit_rpc::{Cell, Color, Grid, ATTR_BOLD, ATTR_REVERSE, ATTR_UNDERLINE};

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

    /// The last buffer line visible in this viewport (inclusive).
    pub fn bottom_line(&self) -> usize {
        self.top_line + self.text_rows().saturating_sub(1) as usize
    }
}

/// Scroll `vp` the minimum amount needed to bring `cur` back into view.
pub fn ensure_cursor_visible(vp: &mut Viewport, cur: Cursor) {
    let rows = vp.text_rows() as usize;
    if rows == 0 {
        vp.top_line = cur.row;
        vp.left_col = cur.col;
        return;
    }
    if cur.row < vp.top_line {
        vp.top_line = cur.row;
    } else if cur.row >= vp.top_line + rows {
        vp.top_line = cur.row + 1 - rows;
    }
    if cur.col < vp.left_col {
        vp.left_col = cur.col;
    } else if cur.col >= vp.left_col + vp.width as usize {
        vp.left_col = cur.col + 1 - vp.width as usize;
    }
}

/// Build a viewport from scratch that contains both `cur` and (optionally) the
/// previous viewport, preferring the previous scroll position when possible.
pub fn scroll_to_cursor(vp: &mut Viewport, cur: Cursor, buf: &Buffer) {
    ensure_cursor_visible(vp, cur);
    // Never scroll past the end of the document if it fits.
    let lines = buf.line_count();
    let rows = vp.text_rows() as usize;
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
        assert_eq!(vp.bottom_line(), 21);

        let tiny = Viewport::new(80, 1);
        assert_eq!(tiny.text_rows(), 0);
    }

    #[test]
    fn ensure_visible_scrolls_down_minimally() {
        let mut vp = Viewport::new(80, 12); // 10 text rows
        vp.top_line = 0;
        ensure_cursor_visible(&mut vp, Cursor::new(10, 0));
        assert_eq!(vp.top_line, 1);
        ensure_cursor_visible(&mut vp, Cursor::new(11, 0));
        assert_eq!(vp.top_line, 2);
        // Already visible: no movement.
        ensure_cursor_visible(&mut vp, Cursor::new(5, 0));
        assert_eq!(vp.top_line, 2);
    }

    #[test]
    fn ensure_visible_scrolls_up() {
        let mut vp = Viewport::new(80, 12);
        vp.top_line = 8;
        ensure_cursor_visible(&mut vp, Cursor::new(3, 0));
        assert_eq!(vp.top_line, 3);
    }

    #[test]
    fn ensure_visible_scrolls_right() {
        let mut vp = Viewport::new(10, 12);
        vp.top_line = 0;
        ensure_cursor_visible(&mut vp, Cursor::new(0, 9));
        assert_eq!(vp.left_col, 0, "column 9 is the last visible one");
        ensure_cursor_visible(&mut vp, Cursor::new(0, 10));
        assert_eq!(vp.left_col, 1);
    }

    #[test]
    fn scroll_to_cursor_does_not_over_scroll_short_documents() {
        let buf = Buffer::from_text("a\nb\nc\n");
        let mut vp = Viewport::new(80, 12); // 10 text rows
        vp.top_line = 7;
        scroll_to_cursor(&mut vp, Cursor::new(0, 0), &buf);
        assert_eq!(vp.top_line, 0, "3 lines fit, so the view starts at the top");

        let long = Buffer::from_text("a\nb\nc\nd\ne\nf\ng\nh\ni\nj\nk\nl\n");
        let mut vp = Viewport::new(80, 12);
        scroll_to_cursor(&mut vp, Cursor::new(11, 0), &long);
        assert_eq!(
            vp.top_line, 2,
            "12 lines in 10 rows: the last screen starts at 2"
        );
    }
}
