//! The screen a program thinks it is drawing on.
//!
//! A terminal is not a pipe that happens to be colourful: the bytes a program
//! writes are text *and* instructions, and a terminal is the thing that knows
//! the difference. [`Screen`] is that thing — a grid of cells, a cursor, and an
//! implementation of [`vte::Perform`], which is the parser that turns the byte
//! stream into the callbacks this applies.
//!
//! The subset is deliberate and documented in `docs/terminal.md`: what a shell, a
//! build and a pager do, plus enough cursor and erase control for a full-screen
//! program to look right. Scroll *regions* are the notable omission — a program
//! that sets one scrolls the whole screen instead, which is wrong in a way that
//! is visible rather than a way that is silent.

use inkstone_rpc::{ATTR_BOLD, ATTR_FAINT, ATTR_ITALIC, ATTR_REVERSE, ATTR_UNDERLINE};
use unicode_width::UnicodeWidthChar;
use vte::{Params, Perform};

use crate::screen::Color;

/// What a cell is drawn with: two colours and a set of attribute bits.
///
/// The bits are `inkstone_rpc`'s, the same ones a frontend is sent, because a
/// cell is a cell: what a program painted and what a highlighter found end up as
/// the same thing on the same screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Attr {
    /// The colour of the character.
    pub fg: Color,
    /// The colour behind it.
    pub bg: Color,
    /// Attribute bits, from [`crate::screen`]'s `ATTR_*`.
    pub attrs: u16,
}

impl Attr {
    /// Whether a bit is set.
    pub fn has(&self, bit: u16) -> bool {
        self.attrs & bit != 0
    }

    /// Set or clear one.
    fn set(&mut self, bit: u16, on: bool) {
        if on {
            self.attrs |= bit;
        } else {
            self.attrs &= !bit;
        }
    }
}

/// One cell of the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    /// The character drawn there. A wide character takes the cell after it as
    /// well, and that one holds a space with `wide` set.
    pub ch: char,
    /// What it is drawn with.
    pub attr: Attr,
    /// Whether this cell is the second half of a wide character.
    pub wide: bool,
}

impl Default for Cell {
    fn default() -> Self {
        Cell {
            ch: ' ',
            attr: Attr::default(),
            wide: false,
        }
    }
}

/// How wide a tab is when a program prints one.
const TAB: usize = 8;

/// What a program has drawn, and where its cursor is.
pub struct Screen {
    cells: Vec<Cell>,
    cols: usize,
    rows: usize,
    /// The cursor, as (column, row) in cells.
    cursor: (usize, usize),
    /// What the next printed character is drawn with.
    attr: Attr,
    /// Where the cursor was saved, by `ESC 7` or `CSI s`.
    saved: Option<(usize, usize)>,
    /// Whether the cursor is shown. A full-screen program hides it while it
    /// paints and shows it again when it is ready to be typed at.
    pub cursor_visible: bool,
    /// The primary screen, while the alternate one is up.
    alternate: Option<Vec<Cell>>,
    /// Whether the cursor is parked in the last column, with the wrap waiting
    /// for the next character rather than happening now.
    pending_wrap: bool,
    /// Bytes the program asked for and has not been given: a reply to "where is
    /// the cursor", which is the one thing a screen cannot answer by itself.
    replies: Vec<u8>,
    /// The parser, which carries the half of an escape sequence that has not
    /// arrived yet from one feed to the next.
    parser: vte::Parser,
}

impl std::fmt::Debug for Screen {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Screen")
            .field("cols", &self.cols)
            .field("rows", &self.rows)
            .field("cursor", &self.cursor)
            .finish_non_exhaustive()
    }
}

impl Screen {
    /// A blank screen of this size.
    pub fn new(cols: usize, rows: usize) -> Self {
        let (cols, rows) = (cols.max(1), rows.max(1));
        Self {
            cells: vec![Cell::default(); cols * rows],
            cols,
            rows,
            cursor: (0, 0),
            attr: Attr::default(),
            saved: None,
            cursor_visible: true,
            alternate: None,
            pending_wrap: false,
            replies: Vec::new(),
            parser: vte::Parser::new(),
        }
    }

    /// How wide the screen is, in cells.
    pub fn cols(&self) -> usize {
        self.cols
    }

    /// How tall it is.
    pub fn rows(&self) -> usize {
        self.rows
    }

    /// Where the cursor is, as (column, row).
    pub fn cursor(&self) -> (usize, usize) {
        self.cursor
    }

    /// One cell, for the renderer.
    pub fn cell(&self, col: usize, row: usize) -> Cell {
        if col >= self.cols || row >= self.rows {
            return Cell::default();
        }
        self.cells[row * self.cols + col]
    }

    /// The screen as text: one line per row, exactly as wide as the screen.
    ///
    /// The trailing blanks are kept, because a terminal's blanks can be painted:
    /// a background colour runs to the end of a line, and a buffer that trimmed
    /// them would lose the half of the screen that was coloured.
    pub fn text(&self) -> String {
        let mut text = String::with_capacity((self.cols + 1) * self.rows);
        for row in 0..self.rows {
            for col in 0..self.cols {
                let cell = self.cell(col, row);
                if !cell.wide {
                    text.push(cell.ch);
                }
            }
            text.push('\n');
        }
        text
    }

    /// Change the size, keeping what was drawn in the corner it was drawn in.
    pub fn resize(&mut self, cols: usize, rows: usize) {
        let (cols, rows) = (cols.max(1), rows.max(1));
        if cols == self.cols && rows == self.rows {
            return;
        }
        let mut cells = vec![Cell::default(); cols * rows];
        for row in 0..rows.min(self.rows) {
            for col in 0..cols.min(self.cols) {
                cells[row * cols + col] = self.cells[row * self.cols + col];
            }
        }
        self.cells = cells;
        self.cols = cols;
        self.rows = rows;
        self.cursor = (self.cursor.0.min(cols - 1), self.cursor.1.min(rows - 1));
    }

    /// Feed the bytes a program wrote.
    ///
    /// Returns nothing: what the program asked for in return — the answer to
    /// "where is the cursor" — is collected by [`Screen::take_reply`], because
    /// this runs inside the kernel and the program's input is somebody else's
    /// business.
    pub fn feed(&mut self, bytes: &[u8]) {
        // The parser and the screen are both `self`, and the parser needs the
        // screen mutable while it holds its own state: it is taken out for the
        // length of the call and put back, which is the one way to lend it out.
        let mut parser = std::mem::take(&mut self.parser);
        parser.advance(self, bytes);
        self.parser = parser;
    }

    /// What the program asked for and has not been told, if anything.
    pub fn take_reply(&mut self) -> Option<Vec<u8>> {
        if self.replies.is_empty() {
            return None;
        }
        Some(std::mem::take(&mut self.replies))
    }

    // -- the screen's own operations ---------------------------------------

    fn index(&self, col: usize, row: usize) -> usize {
        row * self.cols + col
    }

    fn put(&mut self, col: usize, row: usize, cell: Cell) {
        if col < self.cols && row < self.rows {
            let at = self.index(col, row);
            self.cells[at] = cell;
        }
    }

    /// Write one character where the cursor is, and move on.
    fn write(&mut self, ch: char) {
        let width = UnicodeWidthChar::width(ch).unwrap_or(1).max(1);
        if self.pending_wrap {
            // A character printed in the last column leaves the cursor *there*,
            // with the wrap waiting for the next one: that is how a program fills
            // the final cell of a line without scrolling before it has to.
            self.pending_wrap = false;
            self.cursor.0 = 0;
            self.line_feed();
        }
        if self.cursor.0 + width > self.cols {
            self.cursor.0 = 0;
            self.line_feed();
        }
        let (col, row) = self.cursor;
        self.put(
            col,
            row,
            Cell {
                ch,
                attr: self.attr,
                wide: false,
            },
        );
        for offset in 1..width {
            self.put(
                col + offset,
                row,
                Cell {
                    ch: ' ',
                    attr: self.attr,
                    wide: true,
                },
            );
        }
        self.cursor.0 = (col + width).min(self.cols - 1);
        if col + width >= self.cols {
            self.pending_wrap = true;
        }
    }

    /// Down one line, scrolling if the cursor is already at the bottom.
    fn line_feed(&mut self) {
        self.pending_wrap = false;
        if self.cursor.1 + 1 < self.rows {
            self.cursor.1 += 1;
        } else {
            self.scroll_up(1);
        }
    }

    /// Move every line up, filling the bottom with blanks.
    fn scroll_up(&mut self, count: usize) {
        let count = count.min(self.rows);
        let width = self.cols;
        self.cells.copy_within(count * width.., 0);
        let blank = Cell {
            ch: ' ',
            attr: self.attr,
            wide: false,
        };
        let start = (self.rows - count) * width;
        for cell in &mut self.cells[start..] {
            *cell = blank;
        }
    }

    /// Move every line down, throwing away the bottom ones.
    fn scroll_down(&mut self, count: usize) {
        let count = count.min(self.rows);
        let width = self.cols;
        let keep = (self.rows - count) * width;
        self.cells.copy_within(..keep, count * width);
        let blank = Cell {
            ch: ' ',
            attr: self.attr,
            wide: false,
        };
        for cell in &mut self.cells[..count * width] {
            *cell = blank;
        }
    }

    /// Erase part of a row, with the attributes in force.
    ///
    /// A terminal erases with the *current* background, which is how a program
    /// paints a line: it sets a background and clears.
    fn erase_row(&mut self, row: usize, from: usize, to: usize) {
        let blank = Cell {
            ch: ' ',
            attr: self.attr,
            wide: false,
        };
        for col in from..to.min(self.cols) {
            self.put(col, row, blank);
        }
    }

    /// `CSI J` — erase in the display.
    fn erase_display(&mut self, mode: u16) {
        let (col, row) = self.cursor;
        match mode {
            0 => {
                self.erase_row(row, col, self.cols);
                for below in row + 1..self.rows {
                    self.erase_row(below, 0, self.cols);
                }
            }
            1 => {
                for above in 0..row {
                    self.erase_row(above, 0, self.cols);
                }
                self.erase_row(row, 0, col + 1);
            }
            _ => {
                for each in 0..self.rows {
                    self.erase_row(each, 0, self.cols);
                }
            }
        }
    }

    /// `CSI K` — erase in the line.
    fn erase_line(&mut self, mode: u16) {
        let (col, row) = self.cursor;
        match mode {
            0 => self.erase_row(row, col, self.cols),
            1 => self.erase_row(row, 0, col + 1),
            _ => self.erase_row(row, 0, self.cols),
        }
    }

    /// `CSI L` and `CSI M` — lines made and taken away at the cursor.
    fn insert_lines(&mut self, count: usize) {
        let (_, row) = self.cursor;
        let width = self.cols;
        let count = count.min(self.rows - row);
        if count == 0 {
            return;
        }
        self.cells.copy_within(
            row * width..(self.rows - count) * width,
            (row + count) * width,
        );
        for each in row..row + count {
            self.erase_row(each, 0, self.cols);
        }
    }

    fn delete_lines(&mut self, count: usize) {
        let (_, row) = self.cursor;
        let width = self.cols;
        let count = count.min(self.rows - row);
        if count == 0 {
            return;
        }
        self.cells.copy_within((row + count) * width.., row * width);
        for each in self.rows - count..self.rows {
            self.erase_row(each, 0, self.cols);
        }
    }

    /// `CSI P` and `CSI @` — characters taken away and made room for.
    fn delete_chars(&mut self, count: usize) {
        let (col, row) = self.cursor;
        let count = count.min(self.cols - col);
        if count == 0 {
            return;
        }
        let at = self.index(col, row);
        self.cells.copy_within(at + count..at + self.cols, at);
        self.erase_row(row, self.cols - count, self.cols);
    }

    fn insert_chars(&mut self, count: usize) {
        let (col, row) = self.cursor;
        let count = count.min(self.cols - col);
        if count == 0 {
            return;
        }
        let at = self.index(col, row);
        self.cells
            .copy_within(at..at + self.cols - count, at + count);
        self.erase_row(row, col, col + count);
    }

    /// `CSI m` — what the next character is drawn with.
    fn set_attributes(&mut self, params: &Params) {
        let mut numbers = params.iter().map(|group| group[0] as i64).peekable();
        while let Some(number) = numbers.next() {
            match number {
                0 => self.attr = Attr::default(),
                1 => self.attr.set(ATTR_BOLD, true),
                2 => self.attr.set(ATTR_FAINT, true),
                3 => self.attr.set(ATTR_ITALIC, true),
                4 => self.attr.set(ATTR_UNDERLINE, true),
                7 => self.attr.set(ATTR_REVERSE, true),
                22 => {
                    self.attr.set(ATTR_BOLD, false);
                    self.attr.set(ATTR_FAINT, false);
                }
                23 => self.attr.set(ATTR_ITALIC, false),
                24 => self.attr.set(ATTR_UNDERLINE, false),
                27 => self.attr.set(ATTR_REVERSE, false),
                30..=37 => self.attr.fg = Color::Indexed((number - 30) as u8),
                39 => self.attr.fg = Color::Reset,
                40..=47 => self.attr.bg = Color::Indexed((number - 40) as u8),
                49 => self.attr.bg = Color::Reset,
                90..=97 => self.attr.fg = Color::Indexed((number - 90 + 8) as u8),
                100..=107 => self.attr.bg = Color::Indexed((number - 100 + 8) as u8),
                38 | 48 => {
                    // `38;5;n` is one of the palette, `38;2;r;g;b` is 24-bit.
                    let colour = match numbers.next() {
                        Some(5) => numbers.next().map(|n| Color::Indexed(n as u8)),
                        Some(2) => match (numbers.next(), numbers.next(), numbers.next()) {
                            (Some(r), Some(g), Some(b)) => {
                                Some(Color::Rgb(r as u8, g as u8, b as u8))
                            }
                            _ => None,
                        },
                        _ => None,
                    };
                    if let Some(colour) = colour {
                        if number == 38 {
                            self.attr.fg = colour;
                        } else {
                            self.attr.bg = colour;
                        }
                    }
                }
                _ => {}
            }
        }
    }

    /// `CSI ? h` and `CSI ? l` — the modes worth obeying.
    fn set_mode(&mut self, params: &Params, on: bool) {
        for group in params.iter() {
            match group[0] {
                // The alternate screen: a full-screen program paints there and
                // leaves the primary one — with the shell's history on it —
                // exactly as it was.
                1049 => {
                    if on {
                        if self.alternate.is_none() {
                            let blank = vec![Cell::default(); self.cols * self.rows];
                            self.alternate = Some(std::mem::replace(&mut self.cells, blank));
                            // "Save the cursor and switch": a program that paints
                            // on the alternate screen starts at the top left, and
                            // the shell gets its cursor back where it was.
                            self.saved = Some(self.cursor);
                            self.cursor = (0, 0);
                            self.pending_wrap = false;
                        }
                    } else if let Some(primary) = self.alternate.take() {
                        self.cells = primary;
                        if let Some(cursor) = self.saved {
                            self.cursor = cursor;
                        }
                    }
                }
                25 => self.cursor_visible = on,
                _ => {}
            }
        }
    }
}

impl Perform for Screen {
    fn print(&mut self, c: char) {
        self.write(c);
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            // Bell, and the shifts a terminal does not use.
            0x07 | 0x0e | 0x0f => {}
            0x08 => {
                self.pending_wrap = false;
                self.cursor.0 = self.cursor.0.saturating_sub(1);
            }
            0x09 => {
                self.pending_wrap = false;
                let next = (self.cursor.0 / TAB + 1) * TAB;
                self.cursor.0 = next.min(self.cols - 1);
            }
            0x0a..=0x0c => {
                self.pending_wrap = false;
                self.line_feed();
            }
            0x0d => {
                self.pending_wrap = false;
                self.cursor.0 = 0;
            }
            _ => {}
        }
    }

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], _ignore: bool, action: char) {
        let private = intermediates.first() == Some(&b'?');
        // Every cursor command is a *move*, and a move is what a pending wrap is
        // waiting to be cancelled by.
        if matches!(
            action,
            'A' | 'B' | 'C' | 'D' | 'E' | 'F' | 'G' | 'd' | 'H' | 'f' | 'e' | 'a' | '`'
        ) {
            self.pending_wrap = false;
        }
        let mut numbers = params.iter().map(|group| group[0]);
        let first = numbers.next().unwrap_or(0);
        let count = |n: u16| (n.max(1)) as usize;
        match action {
            'A' => self.cursor.1 = self.cursor.1.saturating_sub(count(first)),
            'B' | 'e' => self.cursor.1 = (self.cursor.1 + count(first)).min(self.rows - 1),
            'C' | 'a' => self.cursor.0 = (self.cursor.0 + count(first)).min(self.cols - 1),
            'D' => self.cursor.0 = self.cursor.0.saturating_sub(count(first)),
            'E' => {
                self.cursor.1 = (self.cursor.1 + count(first)).min(self.rows - 1);
                self.cursor.0 = 0;
            }
            'F' => {
                self.cursor.1 = self.cursor.1.saturating_sub(count(first));
                self.cursor.0 = 0;
            }
            'G' => self.cursor.0 = count(first).saturating_sub(1).min(self.cols - 1),
            'd' => self.cursor.1 = count(first).saturating_sub(1).min(self.rows - 1),
            'H' | 'f' => {
                let row = count(first).saturating_sub(1).min(self.rows - 1);
                let col = count(numbers.next().unwrap_or(1))
                    .saturating_sub(1)
                    .min(self.cols - 1);
                self.cursor = (col, row);
            }
            'J' => self.erase_display(first),
            'K' => self.erase_line(first),
            'L' => self.insert_lines(count(first)),
            'M' => self.delete_lines(count(first)),
            'P' => self.delete_chars(count(first)),
            '@' => self.insert_chars(count(first)),
            'S' => self.scroll_up(count(first)),
            'T' => self.scroll_down(count(first)),
            'm' => {
                // `CSI m` with no parameters is a reset, and so is `CSI 0 m`.
                let params = if first == 0 && params.is_empty() {
                    // An empty parameter list: `set_attributes` would see nothing
                    // and change nothing, so the reset is done here.
                    self.attr = Attr::default();
                    return;
                } else {
                    params
                };
                self.set_attributes(params);
            }
            'h' if private => self.set_mode(params, true),
            'l' if private => self.set_mode(params, false),
            's' => self.saved = Some(self.cursor),
            'u' => {
                if let Some(cursor) = self.saved {
                    self.cursor = cursor;
                }
            }
            'n' if first == 6 => {
                // "Where is the cursor" — the one question a screen cannot answer
                // by drawing, and a program that asks it waits for the answer.
                let reply = format!("\x1b[{};{}R", self.cursor.1 + 1, self.cursor.0 + 1);
                self.replies.extend_from_slice(reply.as_bytes());
            }
            _ => {}
        }
    }

    fn esc_dispatch(&mut self, _intermediates: &[u8], _ignore: bool, byte: u8) {
        match byte {
            b'7' => self.saved = Some(self.cursor),
            b'8' => {
                if let Some(cursor) = self.saved {
                    self.cursor = cursor;
                }
            }
            b'D' => self.line_feed(),
            b'E' => {
                self.cursor.0 = 0;
                self.line_feed();
            }
            b'M' => {
                if self.cursor.1 == 0 {
                    self.scroll_down(1);
                } else {
                    self.cursor.1 -= 1;
                }
            }
            b'c' => {
                // A full reset, minus the parser: the parser is mid-dispatch and
                // is at the ground state already, which is exactly where a reset
                // leaves it.
                let (cols, rows) = (self.cols, self.rows);
                self.cells = vec![Cell::default(); cols * rows];
                self.cursor = (0, 0);
                self.attr = Attr::default();
                self.saved = None;
                self.cursor_visible = true;
                self.alternate = None;
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(text: &str) -> Screen {
        let mut screen = Screen::new(10, 3);
        screen.feed(text.as_bytes());
        screen
    }

    fn rows(screen: &Screen) -> Vec<String> {
        screen
            .text()
            .lines()
            .map(|line| line.trim_end().to_string())
            .collect()
    }

    #[test]
    fn printing_writes_where_the_cursor_is() {
        let mut screen = screen("hi");
        assert_eq!(screen.cursor(), (2, 0));
        assert_eq!(rows(&screen), vec!["hi", "", ""]);

        // A carriage return goes back to the start of the line, and a line feed
        // down one — which is what `\r\n` is for.
        screen.feed(b"\r\nbye");
        assert_eq!(rows(&screen), vec!["hi", "bye", ""]);
        assert_eq!(screen.cursor(), (3, 1));
    }

    #[test]
    fn a_character_at_the_right_edge_wraps_when_the_next_one_arrives() {
        let mut screen = Screen::new(4, 3);
        screen.feed(b"abcd");
        assert_eq!(screen.cursor(), (3, 0), "parked on the last cell");
        screen.feed(b"e");
        assert_eq!(rows(&screen), vec!["abcd", "e", ""]);
    }

    #[test]
    fn the_cursor_can_be_put_anywhere() {
        let mut screen = screen("\x1b[3;5Hx");
        assert_eq!(rows(&screen), vec!["", "", "    x"]);
        screen.feed(b"\x1b[1;1Htop");
        assert_eq!(rows(&screen)[0], "top");
        // Relative moves, and the home position with no parameters.
        screen.feed(b"\x1b[2C\x1b[By");
        assert_eq!(rows(&screen)[0], "top");
        // `CSI C` moves *right from where the cursor is*, which after `top` is
        // column three, and `CSI B` down one line.
        assert_eq!(rows(&screen)[1], "     y");
    }

    #[test]
    fn erasing_fills_with_blanks_and_keeps_the_colours() {
        let mut screen = screen("\x1b[41m\x1b[2J");
        let cell = screen.cell(4, 1);
        assert_eq!(cell.ch, ' ');
        assert_eq!(cell.attr.bg, Color::Indexed(1));

        // The cursor is at the start of the first row after a full erase, so
        // `CSI K` is the row that was just painted.
        screen.feed(b"\x1b[0m\x1b[K");
        assert_eq!(screen.cell(4, 0).attr.bg, Color::Reset);
    }

    #[test]
    fn colours_are_remembered_until_they_are_changed() {
        let mut screen = screen("\x1b[1;31mred\x1b[0mplain");
        assert_eq!(screen.cell(0, 0).attr.fg, Color::Indexed(1));
        assert!(screen.cell(0, 0).attr.has(ATTR_BOLD));
        assert_eq!(screen.cell(3, 0).attr, Attr::default());

        screen.feed(b"\x1b[38;5;200mX");
        assert_eq!(screen.cell(8, 0).attr.fg, Color::Indexed(200));
        screen.feed(b"\x1b[48;2;1;2;3mY");
        assert_eq!(screen.cell(9, 0).attr.bg, Color::Rgb(1, 2, 3));
    }

    #[test]
    fn the_alternate_screen_comes_and_goes() {
        let mut screen = screen("shell history");
        screen.feed(b"\x1b[?1049h");
        assert_eq!(
            rows(&screen),
            vec!["", "", ""],
            "a blank screen to paint on"
        );
        screen.feed(b"\x1b[1;1Hpainted");
        assert_eq!(rows(&screen)[0], "painted");

        screen.feed(b"\x1b[?1049l");
        assert_eq!(rows(&screen)[0], "shell hist", "and the old one is back");
    }

    #[test]
    fn scrolling_pushes_the_top_line_off() {
        let mut screen = Screen::new(6, 2);
        screen.feed(b"one\r\ntwo\r\nthree");
        assert_eq!(rows(&screen), vec!["two", "three"]);

        screen.feed(b"\x1b[2S");
        assert_eq!(rows(&screen), vec!["", ""], "CSI S scrolls up");
        screen.feed(b"\x1b[1;1Hbottom\x1b[1T");
        assert_eq!(
            rows(&screen),
            vec!["", "bottom"],
            "and CSI T scrolls the screen down"
        );
    }

    #[test]
    fn lines_and_characters_can_be_made_and_taken_away() {
        let mut screen = Screen::new(8, 3);
        screen.feed(b"aaa\r\nbbb\r\nccc");
        screen.feed(b"\x1b[1;1H\x1b[L");
        assert_eq!(rows(&screen), vec!["", "aaa", "bbb"]);

        screen.feed(b"\x1b[M");
        assert_eq!(rows(&screen), vec!["aaa", "bbb", ""]);

        screen.feed(b"\x1b[1;1H\x1b[P");
        assert_eq!(rows(&screen)[0], "aa");
        screen.feed(b"\x1b[1;1H\x1b[2@");
        assert_eq!(rows(&screen)[0], "  aa");
    }

    #[test]
    fn a_wide_character_takes_two_cells() {
        let mut screen = Screen::new(6, 2);
        screen.feed("你好".as_bytes());
        assert_eq!(screen.cursor(), (4, 0));
        assert_eq!(screen.cell(0, 0).ch, '你');
        assert!(screen.cell(1, 0).wide, "the second cell is its other half");
        assert_eq!(rows(&screen)[0], "你好");
    }

    #[test]
    fn a_program_asking_where_the_cursor_is_gets_an_answer() {
        let mut screen = screen("ab\x1b[6n");
        assert_eq!(
            screen.take_reply().as_deref(),
            Some(b"\x1b[1;3R".as_slice()),
            "row and column, one-based"
        );
        assert!(screen.take_reply().is_none(), "and asked once");
    }

    #[test]
    fn resizing_keeps_what_fits_and_drops_the_rest() {
        let mut screen = Screen::new(4, 2);
        screen.feed(b"abcd\r\nef");
        screen.resize(2, 2);
        assert_eq!(rows(&screen), vec!["ab", "ef"]);

        screen.resize(4, 3);
        assert_eq!(rows(&screen), vec!["ab", "ef", ""]);
    }
}
