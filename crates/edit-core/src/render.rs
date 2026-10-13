//! Turning editor state into a screen grid.
//!
//! `compose` is pure: given a buffer, a cursor and a viewport it produces the
//! exact grid the daemon ships to the frontend, plus where the terminal cursor
//! belongs. The frontend does no layout of its own.
//!
//! Screen layout (top to bottom):
//!
//! ```text
//! +------------------------------+
//! | text rows                    |  height - 2 rows, horizontally scrolled
//! | ...                          |
//! +------------------------------+
//! | status line                  |  row height - 2, reverse video
//! | command line / message       |  row height - 1
//! +------------------------------+
//! ```

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use edit_rpc::{Cell, Grid, ATTR_REVERSE};

use crate::buffer::Buffer;
use crate::cursor::Cursor;
use crate::mode::Mode;
use crate::screen::Viewport;

/// Everything `compose` needs. Borrowed: composing never mutates editor state.
pub struct View<'a> {
    pub buffer: &'a Buffer,
    pub cursor: Cursor,
    pub mode: Mode,
    pub viewport: Viewport,
    /// Columns a tab advances to.
    pub tabstop: usize,
    /// Whether to draw a line-number gutter.
    pub number: bool,
    /// Pre-rendered status line text.
    pub status: String,
    /// Command-line buffer, shown while in [`Mode::Command`].
    pub cmdline: Option<String>,
    /// Transient message, shown on the last row outside command mode.
    pub message: String,
}

/// The result of composing a screen.
#[derive(Debug, Clone)]
pub struct Composed {
    pub grid: Grid,
    /// Cursor position in screen coordinates.
    pub cursor: (u16, u16),
    /// Whether the cursor fell inside the visible viewport.
    pub cursor_visible: bool,
}

/// Compose the whole screen.
pub fn compose(view: &View<'_>) -> Composed {
    let vp = view.viewport;
    let mut grid = Grid::new(vp.width, vp.height);
    let gutter = if view.number {
        (digits(view.buffer.line_count()) + 1) as u16
    } else {
        0
    };
    let content_width = vp.width.saturating_sub(gutter) as usize;

    for row in 0..vp.text_rows() {
        let line_no = vp.top_line + row as usize;
        if line_no >= view.buffer.line_count() {
            break;
        }
        if view.number {
            draw_line_number(&mut grid, row, gutter, line_no + 1);
        }
        let line = view.buffer.line_string(line_no);
        draw_line(
            &mut grid,
            row,
            gutter,
            &line,
            vp.left_col,
            content_width,
            view.tabstop,
        );
    }

    if vp.height >= 2 {
        let status_row = vp.height - 2;
        fill_row(&mut grid, status_row, ATTR_REVERSE);
        draw_text(
            &mut grid,
            0,
            status_row,
            &view.status,
            vp.width as usize,
            ATTR_REVERSE,
        );
    }

    if vp.height >= 1 {
        let bottom_row = vp.height - 1;
        let text = match view.mode {
            Mode::Command => format!(":{}", view.cmdline.as_deref().unwrap_or("")),
            _ => view.message.clone(),
        };
        draw_text(&mut grid, 0, bottom_row, &text, vp.width as usize, 0);
    }

    let (cursor, cursor_visible) = cursor_position(view, gutter);
    Composed {
        grid,
        cursor,
        cursor_visible,
    }
}

fn cursor_position(view: &View<'_>, gutter: u16) -> ((u16, u16), bool) {
    let vp = view.viewport;
    let rows = vp.text_rows() as usize;
    let cur = view.cursor;
    if vp.width == 0 || rows == 0 || cur.row < vp.top_line || cur.row - vp.top_line >= rows {
        return ((0, 0), false);
    }
    if cur.col < vp.left_col {
        return ((0, 0), false);
    }
    let y = (cur.row - vp.top_line) as u16;
    let offset = cur.col - vp.left_col;
    let x = gutter.saturating_add(u16::try_from(offset).unwrap_or(u16::MAX));
    if x < vp.width {
        ((x, y), true)
    } else {
        ((vp.width - 1, y), true)
    }
}

fn digits(n: usize) -> usize {
    n.max(1).to_string().len()
}

fn draw_line_number(grid: &mut Grid, row: u16, gutter: u16, number: usize) {
    let field = gutter.saturating_sub(1) as usize;
    if field == 0 {
        return;
    }
    let text = format!("{number:>field$}");
    for (x, ch) in text.chars().enumerate() {
        if x >= field {
            break;
        }
        grid.put(x as u16, row, Cell::new(ch.to_string()));
    }
}

/// Draw one buffer line, honouring the horizontal scroll offset and tab stops.
fn draw_line(
    grid: &mut Grid,
    row: u16,
    gutter: u16,
    line: &str,
    left_col: usize,
    content_width: usize,
    tabstop: usize,
) {
    let left = gutter as usize;
    let right = left + content_width;
    let tabstop = tabstop.max(1);
    let mut x = left;
    let mut char_idx = 0usize;

    for grapheme in line.graphemes(true) {
        let grapheme_chars = grapheme.chars().count();
        if char_idx < left_col {
            char_idx += grapheme_chars;
            continue;
        }
        if x >= right {
            break;
        }
        if grapheme == "\t" {
            let col = x - left;
            let advance = tabstop - (col % tabstop);
            for _ in 0..advance {
                if x >= right {
                    break;
                }
                grid.put(x as u16, row, Cell::new(" "));
                x += 1;
            }
        } else {
            let width = UnicodeWidthStr::width(grapheme);
            if width == 0 {
                char_idx += grapheme_chars;
                continue;
            }
            if x + width > right {
                break;
            }
            grid.put_grapheme(x as u16, row, width, Cell::new(grapheme));
            x += width;
        }
        char_idx += grapheme_chars;
    }
}

/// Blank the whole row, so attributes (e.g. reverse video) apply edge to edge.
fn fill_row(grid: &mut Grid, row: u16, attrs: u16) {
    for x in 0..grid.width {
        grid.put(x, row, Cell::new(" ").with_attrs(attrs));
    }
}

fn draw_text(grid: &mut Grid, x: u16, row: u16, text: &str, max_width: usize, attrs: u16) {
    let mut x = x as usize;
    let right = x + max_width;
    for grapheme in text.graphemes(true) {
        if x >= right {
            break;
        }
        let width = UnicodeWidthStr::width(grapheme);
        if width == 0 {
            continue;
        }
        if x + width > right {
            break;
        }
        grid.put_grapheme(x as u16, row, width, Cell::new(grapheme).with_attrs(attrs));
        x += width;
    }
}

/// Build the status line text: `-- MODE --  name[+]  line:col`.
pub fn status_line(mode: Mode, name: &str, dirty: bool, cursor: Cursor) -> String {
    format!(
        "-- {} --  {}{}  {}:{}",
        mode.name(),
        name,
        if dirty { "[+]" } else { "" },
        cursor.row + 1,
        cursor.col + 1,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Case {
        buffer: Buffer,
        cursor: Cursor,
        mode: Mode,
        viewport: Viewport,
        tabstop: usize,
        number: bool,
        cmdline: Option<String>,
        message: String,
    }

    impl Case {
        fn new(text: &str, width: u16, height: u16) -> Self {
            Self {
                buffer: Buffer::from_text(text),
                cursor: Cursor::new(0, 0),
                mode: Mode::Normal,
                viewport: Viewport::new(width, height),
                tabstop: 4,
                number: false,
                cmdline: None,
                message: String::new(),
            }
        }

        fn compose(&self) -> Composed {
            compose(&View {
                buffer: &self.buffer,
                cursor: self.cursor,
                mode: self.mode,
                viewport: self.viewport,
                tabstop: self.tabstop,
                number: self.number,
                status: status_line(self.mode, "t.txt", false, self.cursor),
                cmdline: self.cmdline.clone(),
                message: self.message.clone(),
            })
        }
    }

    /// The visible text of a screen row, without the padding to full width.
    fn row(out: &Composed, y: u16) -> String {
        out.grid.row_text(y).trim_end().to_string()
    }

    #[test]
    fn lays_out_text_status_and_message_rows() {
        let c = Case::new("alpha\nbeta\n", 24, 4);
        let out = c.compose();
        assert_eq!(out.grid.width, 24);
        assert_eq!(out.grid.height, 4);
        assert_eq!(row(&out, 0), "alpha");
        assert_eq!(row(&out, 1), "beta");
        assert_eq!(row(&out, 2), "-- NORMAL --  t.txt  1:1");
        assert_eq!(row(&out, 3), "");
        assert_eq!(out.cursor, (0, 0));
        assert!(out.cursor_visible);
    }

    #[test]
    fn status_row_is_reverse_video_edge_to_edge() {
        let c = Case::new("x\n", 20, 3);
        let out = c.compose();
        let row = out.grid.height - 2;
        for x in 0..out.grid.width {
            assert_eq!(
                out.grid.get(x, row).attrs & ATTR_REVERSE,
                ATTR_REVERSE,
                "cell {x} of the status row"
            );
        }
        // The text row above is not reversed.
        assert_eq!(out.grid.get(0, 0).attrs & ATTR_REVERSE, 0);
    }

    #[test]
    fn cursor_maps_into_screen_coordinates() {
        let mut c = Case::new("abc\ndefg\n", 20, 4);
        c.cursor = Cursor::new(1, 3);
        let out = c.compose();
        assert_eq!(out.cursor, (3, 1));
        assert!(out.cursor_visible);
    }

    #[test]
    fn cursor_is_hidden_above_the_viewport() {
        let mut c = Case::new("a\nb\nc\nd\ne\nf\n", 20, 4); // 2 text rows
        c.viewport.top_line = 4;
        c.cursor = Cursor::new(0, 0);
        let out = c.compose();
        assert!(!out.cursor_visible);
        assert_eq!(row(&out, 0), "e");
        assert_eq!(row(&out, 1), "f");
    }

    #[test]
    fn tabs_expand_to_the_next_stop() {
        let mut c = Case::new("a\tb\n\tz\n", 20, 4);
        c.tabstop = 4;
        let out = c.compose();
        assert_eq!(row(&out, 0), "a   b");
        assert_eq!(row(&out, 1), "    z");
    }

    #[test]
    fn wide_graphemes_occupy_two_columns() {
        let c = Case::new("你好\n", 8, 3);
        let out = c.compose();
        assert_eq!(out.grid.get(0, 0).text, "你");
        assert!(out.grid.get(1, 0).is_continuation());
        assert_eq!(out.grid.get(2, 0).text, "好");
        assert!(out.grid.get(3, 0).is_continuation());
        assert_eq!(row(&out, 0), "你好");
    }

    #[test]
    fn combining_mark_stays_in_one_cell() {
        let c = Case::new("e\u{301}x\n", 8, 3);
        let out = c.compose();
        assert_eq!(out.grid.get(0, 0).text, "e\u{301}");
        assert_eq!(out.grid.get(1, 0).text, "x");
        assert_eq!(row(&out, 0), "e\u{301}x");
    }

    #[test]
    fn long_lines_are_truncated_not_wrapped() {
        let c = Case::new("abcdefghij\n", 5, 3);
        let out = c.compose();
        assert_eq!(row(&out, 0), "abcde");
    }

    #[test]
    fn horizontal_scroll_is_by_character() {
        let mut c = Case::new("abcdefghij\n", 5, 3);
        c.viewport.left_col = 3;
        let out = c.compose();
        assert_eq!(row(&out, 0), "defgh");
        // Cursor hunted off to the left is not drawn.
        c.cursor = Cursor::new(0, 1);
        assert!(!c.compose().cursor_visible);
    }

    #[test]
    fn line_numbers_are_right_aligned_in_a_gutter() {
        let mut c = Case::new("a\nb\nc\n", 10, 5);
        c.number = true;
        let out = c.compose();
        assert_eq!(row(&out, 0), "1 a");
        assert_eq!(row(&out, 1), "2 b");
        assert_eq!(row(&out, 2), "3 c");
        // The cursor is offset by the gutter.
        assert_eq!(out.cursor, (2, 0));

        let mut wide = Case::new(&"x\n".repeat(12), 10, 5);
        wide.number = true;
        let out = wide.compose();
        assert_eq!(row(&out, 0), " 1 x", "digits right-align in the gutter");
        assert_eq!(row(&out, 1), " 2 x");
        assert_eq!(row(&out, 2), " 3 x");
    }

    #[test]
    fn command_mode_shows_the_colon_prefixed_command_line() {
        let mut c = Case::new("x\n", 20, 3);
        c.mode = Mode::Command;
        c.cmdline = Some("w".into());
        c.message = "ignored while in command mode".into();
        let out = c.compose();
        assert_eq!(row(&out, 2), ":w");
    }

    #[test]
    fn message_shows_on_the_bottom_row_outside_command_mode() {
        let mut c = Case::new("x\n", 20, 3);
        c.message = "E32: No file name".into();
        let out = c.compose();
        assert_eq!(row(&out, 2), "E32: No file name");
    }

    #[test]
    fn short_terminal_still_renders_the_message_row() {
        let c = Case::new("x\n", 20, 1);
        let out = c.compose();
        assert_eq!(out.grid.height, 1);
        assert!(!out.cursor_visible);
    }

    #[test]
    fn status_line_format() {
        let mut c = Case::new("x\n", 20, 3);
        c.cursor = Cursor::new(0, 1);
        assert_eq!(
            status_line(Mode::Insert, "a.txt", true, c.cursor),
            "-- INSERT --  a.txt[+]  1:2"
        );
        assert_eq!(
            status_line(Mode::Normal, "[No Name]", false, c.cursor),
            "-- NORMAL --  [No Name]  1:2"
        );
    }
}
