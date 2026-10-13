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

use inkstone_rpc::{Cell, Grid, ATTR_REVERSE};

use crate::buffer::Buffer;
use crate::cursor::Cursor;
use crate::mode::Mode;
use crate::screen::Viewport;

/// Everything `compose` needs. Borrowed: composing never mutates editor state.
pub struct View<'a> {
    /// The document to draw.
    pub buffer: &'a Buffer,
    /// Where the cursor is, in the buffer.
    pub cursor: Cursor,
    /// The mode: it decides what the status line says and how the command line is
    /// drawn.
    pub mode: Mode,
    /// Which part of the buffer, and of each line, is on screen.
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
    /// The screen.
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
    let gutter = gutter_width(view.buffer.line_count(), view.number);
    let layout = TextLayout {
        gutter,
        left_col: vp.left_col,
        width: vp.width.saturating_sub(gutter) as usize,
        tabstop: view.tabstop,
    };

    // The cursor position comes out of the same walk that draws the line, so
    // it can never disagree with where the character actually landed.
    let mut cursor_at = None;
    for row in 0..vp.text_rows() {
        let line_no = vp.top_line + row as usize;
        if line_no >= view.buffer.line_count() {
            break;
        }
        if view.number {
            draw_line_number(&mut grid, row, gutter, line_no + 1);
        }
        let line = view.buffer.line_string(line_no);
        let cursor_char = if view.cursor.row == line_no && view.cursor.col >= vp.left_col {
            Some(view.cursor.col)
        } else {
            None
        };
        let x = draw_line(&mut grid, row, &line, &layout, cursor_char);
        if let (Some(x), Some(_)) = (x, cursor_char) {
            cursor_at = Some((x, row));
        }
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

    let (cursor, cursor_visible) = cursor_position(view, cursor_at);
    Composed {
        grid,
        cursor,
        cursor_visible,
    }
}

/// Place the cursor, given the screen column the line walk reported for it.
fn cursor_position(view: &View<'_>, found: Option<(u16, u16)>) -> ((u16, u16), bool) {
    let vp = view.viewport;
    if vp.width == 0 || vp.text_rows() == 0 {
        return ((0, 0), false);
    }
    match found {
        Some((x, y)) => ((x.min(vp.width - 1), y), true),
        None => ((0, 0), false),
    }
}

fn digits(n: usize) -> usize {
    n.max(1).to_string().len()
}

/// How many screen columns the line-number gutter takes.
///
/// The single source of truth: both the renderer and the scroll maths use it,
/// so the cursor cannot end up in a column the gutter has taken.
pub fn gutter_width(line_count: usize, number: bool) -> u16 {
    if number {
        (digits(line_count) + 1) as u16
    } else {
        0
    }
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

/// How the text area is laid out: everything `draw_line` needs to know that is
/// the same for every row.
struct TextLayout {
    /// Screen columns reserved for line numbers.
    gutter: u16,
    /// First visible character of each line.
    left_col: usize,
    /// Screen columns available for text.
    width: usize,
    /// Columns a tab advances to.
    tabstop: usize,
}

/// Draw one buffer line, honouring the horizontal scroll offset and tab stops.
///
/// When `cursor_char` is set, this returns the screen column that character
/// starts at — the whole point being that the cursor is placed by the same walk
/// that draws, so tabs and double-width graphemes cannot push it off the
/// character it is supposed to be sitting on.
fn draw_line(
    grid: &mut Grid,
    row: u16,
    line: &str,
    layout: &TextLayout,
    cursor_char: Option<usize>,
) -> Option<u16> {
    let left = layout.gutter as usize;
    let right = left + layout.width;
    let left_col = layout.left_col;
    let tabstop = layout.tabstop.max(1);
    let mut x = left;
    let mut char_idx = 0usize;
    let mut found = None;

    for grapheme in line.graphemes(true) {
        let grapheme_chars = grapheme.chars().count();
        if char_idx < left_col {
            char_idx += grapheme_chars;
            continue;
        }
        if Some(char_idx) == cursor_char {
            found = Some(x as u16);
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

    // The cursor past the end of the line (insert mode) or beyond the
    // truncation point belongs at the end of the drawn text.
    match (&found, cursor_char) {
        (None, Some(_)) => Some(x.min(u16::MAX as usize) as u16),
        _ => found,
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
    fn the_cursor_follows_display_columns_not_character_counts() {
        // A tab pushes `b` to screen column 4, even though it is character 2.
        let mut c = Case::new("a\tb\n", 20, 4);
        c.tabstop = 4;
        c.cursor = Cursor::new(0, 2);
        assert_eq!(c.compose().cursor, (4, 0));
        c.cursor = Cursor::new(0, 1);
        assert_eq!(
            c.compose().cursor,
            (1, 0),
            "the tab itself starts at column 1"
        );

        // A double-width grapheme takes two columns, so the insertion point
        // after it is column 2, not 1.
        let mut wide = Case::new("你好\n", 20, 4);
        wide.cursor = Cursor::new(0, 1);
        assert_eq!(wide.compose().cursor, (2, 0));
        wide.cursor = Cursor::new(0, 2);
        assert_eq!(wide.compose().cursor, (4, 0), "past the end of the line");
    }

    #[test]
    fn the_cursor_is_clamped_to_the_edge_when_the_text_is_truncated() {
        let mut c = Case::new("abcdefghij\n", 5, 3);
        c.cursor = Cursor::new(0, 9);
        let out = c.compose();
        assert!(out.cursor_visible);
        assert_eq!(
            out.cursor,
            (4, 0),
            "the cursor sits on the last visible column"
        );
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
