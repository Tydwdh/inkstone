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

use inkstone_rpc::{Cell, Color, Grid, ATTR_BOLD, ATTR_REVERSE, ATTR_UNDERLINE};

use crate::buffer::Buffer;
use crate::cursor::Cursor;
use crate::diagnostic::{Diagnostic, Severity};
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
    /// The visual selection, as a character range `[start, end)`, if one is up.
    /// It is what the highlight is for.
    pub(crate) selection: Option<(usize, usize)>,
    /// A blockwise selection, as rows and character columns — the shape a
    /// rectangle cannot be said in one character range. `selection` is `None`
    /// while this is `Some`: they are two ways of saying where the highlight is.
    pub block: Option<(usize, usize, usize, Option<usize>)>,
    /// The folds of the window that is up. A closed one is drawn as its first line,
    /// and the lines it hides are not drawn at all.
    pub folds: &'a [crate::editor::Fold],
    /// How deep the folds are shown closed — `'foldlevel'`, which `zm` and `zr`
    /// move. A fold nested deeper than it is drawn shut even when it is not closed
    /// by hand.
    pub(crate) foldlevel: usize,
    /// How many lines a fold has to be to be drawn closed — `'foldminlines'`.
    pub(crate) foldminlines: usize,
    /// Every search match in the document, in order. The ones on screen are
    /// highlighted.
    pub matches: &'a [(usize, usize)],
    /// The one match the cursor is inside, if any: it is highlighted more
    /// strongly, because it is the one `n` would move off.
    pub(crate) current_match: Option<(usize, usize)>,
    /// Whether a search being typed is showing its match right now — the cursor
    /// belongs to that match, not to the line the pattern is being typed on.
    pub(crate) incsearch: bool,
    /// The syntax colours, or `None` when highlighting is off. It is asked for
    /// the spans of a line at a time, and remembers what it worked out.
    pub(crate) highlighter: Option<&'a crate::highlight::LineStates>,
    /// Which part of the buffer, and of each line, is on screen.
    pub viewport: Viewport,
    /// Where each window is, in cells. Empty means one window over the whole
    /// screen, which is what a caller that has no windows to speak of wants.
    pub(crate) rects: Vec<crate::editor::Rect>,
    /// Every open document, for the panes that are not the current one. The
    /// document the current window shows is `buffer`, live.
    pub documents: &'a [crate::editor::Document],
    /// Every window, in the same order as `rects`.
    pub windows: &'a [crate::editor::Window],
    /// Which of them is on screen: the only one that draws a cursor.
    pub(crate) current_window: usize,
    /// What a language server said about the document on screen.
    pub diagnostics: &'a [crate::diagnostic::Diagnostic],
    /// The completion popup, if it is up: what to offer and which is picked.
    pub completion: Option<&'a crate::editor::Completion>,
    /// Whether the document on screen has been changed since it was written.
    /// The renderer needs it for a *second* window on that document, whose slot
    /// holds a placeholder and cannot answer.
    pub dirty: bool,
    /// Columns a tab advances to.
    pub tabstop: usize,
    /// Whether to draw a line-number gutter.
    pub number: bool,
    /// Pre-rendered status line text.
    pub status: String,
    /// Command-line buffer, shown while in [`Mode::Command`]: the prompt
    /// character and the line being typed.
    pub(crate) cmdline: Option<String>,
    /// Where the cursor is in that line's *text*, as a character index (the
    /// prompt character is not part of it).
    pub(crate) cmdline_cursor: Option<usize>,
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
    /// Whether the editor is inserting text: the cursor is a bar rather than a
    /// block, and an input method belongs here and nowhere else.
    pub insert: bool,
}

/// One window's worth of the screen, as the renderer needs it: which text, from
/// where, with which colours, and what its status line says.
struct Pane<'a> {
    rect: crate::editor::Rect,
    buffer: &'a Buffer,
    highlighter: Option<&'a crate::highlight::LineStates>,
    matches: &'a [(usize, usize)],
    current_match: Option<(usize, usize)>,
    selection: Option<(usize, usize)>,
    /// The blockwise selection, when that is what is up: rows and character
    /// columns. A rectangle is not one range, so it is not `selection`.
    block: Option<(usize, usize, usize, Option<usize>)>,
    /// The folds of this window.
    folds: &'a [crate::editor::Fold],
    /// And how deep they are shown closed.
    foldlevel: usize,
    /// And how many lines a fold needs to be drawn closed.
    foldminlines: usize,
    cursor: Cursor,
    top_line: usize,
    left_col: usize,
    diagnostics: &'a [crate::diagnostic::Diagnostic],
    status: String,
    /// Whether this is the window that is up. Only that one draws a cursor and
    /// a selection, since only that one has them.
    current: bool,
}

/// The panes to draw. The current window's state is in the view itself and
/// every other one's is in its document, which is where a window that is not on
/// screen keeps it.
fn panes<'a>(view: &'a View<'a>) -> Vec<Pane<'a>> {
    // The last row of the screen is the message row and belongs to nobody.
    let full = crate::editor::Rect {
        x: 0,
        y: 0,
        width: view.viewport.width,
        height: view.viewport.height.saturating_sub(1),
    };
    if view.rects.is_empty() {
        // No windows to speak of: one pane over the whole screen, which is what
        // every caller that is not the editor means.
        return vec![live_pane(view, full)];
    }
    // Which document lives in the view itself rather than in a slot. A second
    // window on that document reads it from here too: its slot holds a
    // placeholder while the document is on screen.
    let live = view
        .windows
        .get(view.current_window)
        .map(|window| window.document);
    let mut out = Vec::with_capacity(view.rects.len());
    for (index, rect) in view.rects.iter().enumerate() {
        let current = index == view.current_window;
        let Some(window) = view.windows.get(index) else {
            continue;
        };
        if current {
            out.push(live_pane(view, *rect));
            continue;
        }
        if Some(window.document) == live {
            // The same document, looked at from somewhere else: the text and
            // the colours come from the view, the position from the window.
            let mut pane = live_pane(view, *rect);
            pane.cursor = window.cursor;
            pane.top_line = window.top_line;
            pane.left_col = window.left_col;
            pane.selection = None;
            pane.block = None;
            pane.current = false;
            pane.status = status_line(
                "NORMAL",
                &view.buffer.display_name(),
                view.dirty,
                window.cursor,
                &[],
            );
            out.push(pane);
            continue;
        }
        let Some(document) = view.documents.get(window.document) else {
            continue;
        };
        out.push(Pane {
            rect: *rect,
            buffer: &document.buffer,
            // Highlighting is off for every window or for none of them, so the
            // current window's answer stands for the others.
            highlighter: view.highlighter.map(|_| &document.highlighter),
            matches: document.search.matches(),
            current_match: document
                .search
                .match_at(document.cursor.char_idx(&document.buffer)),
            selection: None,
            block: None,
            folds: &window.folds,
            foldlevel: view.foldlevel,
            foldminlines: view.foldminlines,
            cursor: window.cursor,
            top_line: window.top_line,
            left_col: window.left_col,
            diagnostics: &document.diagnostics,
            // Only the window that is up can be in a mode other than normal.
            status: status_line(
                "NORMAL",
                &document.buffer.display_name(),
                document.is_dirty(),
                window.cursor,
                &document.diagnostics,
            ),
            current: false,
        });
    }
    out
}

/// The pane of the window that is up: the state in the view itself.
fn live_pane<'a>(view: &'a View<'a>, rect: crate::editor::Rect) -> Pane<'a> {
    Pane {
        rect,
        buffer: view.buffer,
        highlighter: view.highlighter,
        matches: view.matches,
        current_match: view.current_match,
        selection: view.selection,
        block: view.block,
        folds: view.folds,
        foldlevel: view.foldlevel,
        foldminlines: view.foldminlines,
        cursor: view.cursor,
        top_line: view.viewport.top_line,
        left_col: view.viewport.left_col,
        diagnostics: view.diagnostics,
        status: view.status.clone(),
        current: true,
    }
}

/// Compose the whole screen: every window in its own rectangle, then the
/// separators between them, then the row at the bottom of the screen.
pub fn compose(view: &View<'_>) -> Composed {
    let vp = view.viewport;
    let mut grid = Grid::new(vp.width, vp.height);
    let panes = panes(view);
    let mut cursor_at = None;
    for pane in &panes {
        draw_pane(&mut grid, view, pane, &mut cursor_at);
    }
    draw_separators(&mut grid, &panes, vp);
    if let Some(completion) = view.completion {
        draw_completion(&mut grid, view, completion);
    }

    if vp.height >= 1 {
        let bottom_row = vp.height - 1;
        let text = match (view.mode, view.cmdline.as_deref()) {
            // The prompt character is part of the line the editor hands over:
            // `:` command, `/` or `?` search, and the renderer does not have to
            // know which.
            (Mode::Command, Some(line)) => {
                // A line longer than the window scrolls sideways, so the cursor
                // cannot be typed off the edge of the screen.
                let (skip, _) =
                    prompt_layout(line, view.cmdline_cursor.unwrap_or(0), vp.width as usize);
                line.chars().skip(skip).collect::<String>()
            }
            _ => view.message.clone(),
        };
        draw_text(&mut grid, 0, bottom_row, &text, vp.width as usize, 0);
    }

    let (cursor, cursor_visible) = match (view.mode, view.cmdline.as_deref(), view.cmdline_cursor) {
        // While a line is being typed the cursor is *in* that line: that is
        // where the next character will go, and where an input method's window
        // belongs. A search that is showing its match is the exception: the
        // cursor goes to the match, the way vim shows it.
        (Mode::Command, Some(line), Some(at)) if !view.incsearch => prompt_cursor(view, line, at),
        _ => cursor_position(view, cursor_at),
    };
    Composed {
        grid,
        cursor,
        cursor_visible,
        // Where text goes, an input method belongs — and a windowed frontend
        // has to know: an input method left on in normal mode eats the keys
        // that run commands.
        insert: view.mode == Mode::Insert,
    }
}

/// Draw one window: its text, then its status line on its last row.
fn draw_pane(
    grid: &mut Grid,
    view: &View<'_>,
    pane: &Pane<'_>,
    cursor_at: &mut Option<(u16, u16)>,
) {
    let rect = pane.rect;
    if rect.width == 0 || rect.height == 0 || rect.y >= grid.height {
        return;
    }
    // A terminal's colours are the program's, and its cursor is where the
    // program put it — which is the whole of what a terminal is, and the reason
    // it is drawn from the screen rather than from the text.
    if let Some(screen) = pane.buffer.terminal() {
        draw_terminal_pane(grid, view, pane, screen, cursor_at);
        return;
    }
    let gutter = gutter_width(pane.buffer.line_count(), view.number);
    let layout = TextLayout {
        x: rect.x,
        gutter,
        left_col: pane.left_col,
        width: rect.width.saturating_sub(gutter) as usize,
        tabstop: view.tabstop,
    };

    // The last row of a window is its status line.
    let text_rows = rect.height.saturating_sub(1);
    // The cursor position comes out of the same walk that draws the line, so it
    // can never disagree with where the character actually landed.
    // The lines that are *drawn*, which is not the same as the lines that are
    // there: a closed fold takes its own range with it, so the rows and the lines
    // stop being the same thing.
    let visible = visible_lines(pane, text_rows as usize);
    for (row, line_no) in visible.iter().copied().enumerate() {
        let row = row as u16;
        let y = rect.y + row;
        if y >= grid.height {
            break;
        }
        if view.number {
            // The line number is the one part of a row that is the editor's and
            // not the file's, so it is where a severity shows without taking a
            // colour away from the code.
            let severity = pane
                .diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.on_row(line_no).is_some())
                .map(|diagnostic| diagnostic.severity)
                .min();
            let color = severity.map(|severity| severity.color());
            draw_line_number(grid, rect.x, y, gutter, line_no + 1, color);
        }
        let line = match closed_fold_at(pane, line_no) {
            // A closed fold is its first line and a count of what it hides.
            Some(fold) => fold_text(pane, line_no, fold),
            None => pane.buffer.line_string(line_no),
        };
        // "While the cursor is on folded lines, the cursor is always *displayed* in
        // the first column": the fold line is the marker, and a column inside it would
        // point at nothing. (The cursor's own column is left alone — setting it there
        // undoes the motion an operator is about to take.)
        let on_a_fold_line = closed_fold_at(pane, line_no).is_some();
        // The cursor is placed by the walk that draws, which is what knows
        // which cell its character lands in and whether that cell is on screen:
        // one hunted off to the left or the right is simply never reached.
        let cursor_char = match (pane.cursor.row == line_no, on_a_fold_line) {
            (false, _) => None,
            (true, false) => Some(pane.cursor.col),
            // "While the cursor is on folded lines, the cursor is always
            // *displayed* in the first column" — and a fold row is its marker,
            // drawn from its own first cell, so the column the view starts at is
            // the one that lands there. (The marker has no tabs in it, so its
            // cells and its characters are the same thing.)
            (true, true) => Some(pane.left_col.min(line.chars().count().saturating_sub(1))),
        };
        let highlight = RowHighlight {
            selected: selection_columns(pane, line_no),
            matches: match_columns(pane, line_no),
            current: pane.current_match,
            syntax: syntax_columns(pane, line_no),
            diagnostics: diagnostic_columns(pane, line_no),
        };
        let x = draw_line(grid, y, &line, &layout, cursor_char, &highlight);
        if pane.current {
            if let (Some(x), Some(_)) = (x, cursor_char) {
                *cursor_at = Some((x, y));
            }
        }
    }

    draw_status_row(grid, pane);
}

/// The last row of a window: the status line, reverse video, edge to edge.
fn draw_status_row(grid: &mut Grid, pane: &Pane<'_>) {
    let rect = pane.rect;
    if rect.height < 1 {
        return;
    }
    let status_row = rect.y + rect.height - 1;
    if status_row >= grid.height {
        return;
    }
    fill_row(grid, status_row, rect.x, rect.width, ATTR_REVERSE);
    draw_text(
        grid,
        rect.x,
        status_row,
        &pane.status,
        rect.width as usize,
        ATTR_REVERSE,
    );
}

/// A pane whose buffer is a program's screen.
///
/// The text is the same text the ordinary path would draw — a buffer's lines
/// *are* the screen's rows — but every cell carries the colours the program
/// chose, and the cursor is the one the program moved rather than the one a
/// person did.
fn draw_terminal_pane(
    grid: &mut Grid,
    _view: &View<'_>,
    pane: &Pane<'_>,
    screen: &crate::terminal::Screen,
    cursor_at: &mut Option<(u16, u16)>,
) {
    let rect = pane.rect;
    // The last row of a window is its status line, as everywhere else.
    let text_rows = rect.height.saturating_sub(1) as usize;
    let width = rect.width as usize;
    for row in 0..text_rows.min(screen.rows()) {
        let y = rect.y + row as u16;
        if y >= grid.height {
            break;
        }
        for col in 0..width.min(screen.cols()) {
            let cell = screen.cell(col, row);
            if cell.wide {
                // The cell after a wide character is its other half, and the
                // half that was drawn is the one that carries the text.
                continue;
            }
            let wide = screen.cell(col + 1, row).wide;
            let mut drawn = Cell::new(cell.ch.to_string());
            drawn.fg = cell.attr.fg;
            drawn.bg = cell.attr.bg;
            drawn.attrs = cell.attr.attrs;
            grid.put_grapheme(rect.x + col as u16, y, if wide { 2 } else { 1 }, drawn);
        }
        if pane.current && screen.cursor().1 == row {
            let (cursor_col, _) = screen.cursor();
            if cursor_col < width {
                *cursor_at = Some((rect.x + cursor_col as u16, y));
            }
        }
    }
    draw_status_row(grid, pane);
}

/// How many items the popup shows at once.
const COMPLETION_ROWS: usize = 10;

/// The popup: what could be typed, under the cursor.
///
/// It goes under the cursor's line and flips above it when there is no room
/// below, which is vim's rule and the only one that keeps it on the screen at
/// the bottom of a window. It is drawn over the text, on its own background,
/// because a list of words that looks like the text around it is a list nobody
/// finds.
fn draw_completion(grid: &mut Grid, view: &View<'_>, completion: &crate::editor::Completion) {
    if completion.shown.is_empty() || view.viewport.width == 0 || view.viewport.height == 0 {
        return;
    }
    // The widest label, and a floor and ceiling for the box: a popup as wide as
    // the screen is not a popup.
    let widest = completion
        .shown
        .iter()
        .map(|item| item.label.chars().count() + item.detail.chars().count() + 3)
        .max()
        .unwrap_or(8);
    let width = (widest.min(view.viewport.width as usize) as u16).max(8);
    let height = (completion.shown.len().min(COMPLETION_ROWS) as u16).max(1);

    // Where the cursor is on screen, which is where the list belongs. The
    // cursor of the window that is up is the one being typed into.
    let layout = TextLayout {
        x: 0,
        gutter: gutter_width(view.buffer.line_count(), view.number),
        left_col: view.viewport.left_col,
        width: view.viewport.width as usize,
        tabstop: view.tabstop,
    };
    // The popup hangs from the word being completed rather than from the
    // cursor: the cursor moves along as items are typed, and a list that moved
    // with it would jump under the hands. The column is a cell count, like the
    // scroll it is measured against.
    let (_, start_col) = view.buffer.char_to_line_col(completion.start);
    let line = view.buffer.line_string(view.cursor.row);
    let column = crate::screen::cells_of(&line, view.tabstop.max(1), start_col)
        .saturating_sub(layout.left_col)
        .min(view.viewport.width as usize) as u16
        + layout.gutter;
    let row = view.cursor.row.saturating_sub(view.viewport.top_line) as u16;
    let x = column.min(view.viewport.width.saturating_sub(width));
    let below = row + 1;
    // The row above the cursor's, or under it when that is past the bottom.
    let y = if below + height <= view.viewport.height.saturating_sub(1) {
        below
    } else {
        row.saturating_sub(height)
    };

    let first = completion
        .selected
        .saturating_sub(COMPLETION_ROWS.saturating_sub(1) / 2)
        .min(completion.shown.len().saturating_sub(COMPLETION_ROWS));
    for (offset, item) in completion
        .shown
        .iter()
        .skip(first)
        .take(COMPLETION_ROWS)
        .enumerate()
    {
        let at = y + offset as u16;
        if at >= view.viewport.height {
            break;
        }
        let picked = first + offset == completion.selected;
        let attrs = if picked { ATTR_REVERSE } else { 0 };
        for x in x..x.saturating_add(width).min(grid.width) {
            grid.put(x, at, Cell::new(" ").with_attrs(attrs));
        }
        let text = if item.detail.is_empty() {
            item.label.clone()
        } else {
            format!("{}  {}", item.label, item.detail)
        };
        draw_text(grid, x, at, &text, width as usize, attrs);
    }
}

/// The vertical lines between windows that are side by side.
///
/// A window above another needs nothing: the status line of the one above *is*
/// the line between them, which is vim's arrangement and one fewer row spent.
fn draw_separators(grid: &mut Grid, panes: &[Pane<'_>], vp: Viewport) {
    for pane in panes {
        let rect = pane.rect;
        if rect.x == 0 || rect.width == 0 {
            continue;
        }
        let x = rect.x - 1;
        for y in rect.y..rect.y.saturating_add(rect.height).min(vp.height) {
            grid.put(x, y, Cell::new("│"));
        }
    }
}
/// Place the cursor, given the screen column the line walk reported for it.
/// The buffer position a screen cell shows, if it shows any text.
///
/// This is the walk `draw_line` does, run backwards — the same gutter, the
/// same tabstops, the same wide graphemes — so a click lands on the character
/// that is drawn under it, and a click on the second cell of a wide character
/// lands on that character rather than the one after it.
///
/// `None` for a cell that is not text: the line-number gutter, a separator, the
/// status line, the message row, and the empty rows below the end of the file.
///
/// The window the position is in comes back with it, so a click in another
/// window can be answered by going there first.
pub(crate) fn position_at(view: &View<'_>, x: u16, y: u16) -> Option<(usize, Cursor)> {
    let vp = view.viewport;
    if vp.width == 0 || vp.height == 0 {
        return None;
    }
    let panes = panes(view);
    let pane = panes.iter().find(|pane| {
        pane.rect.width > 0
            && pane.rect.height > 1
            && x >= pane.rect.x
            && x < pane.rect.x + pane.rect.width
            && y >= pane.rect.y
            && y < pane.rect.y + pane.rect.height - 1
    })?;
    let window = panes
        .iter()
        .position(|other| std::ptr::eq(other, pane))
        .unwrap_or(0);

    let rect = pane.rect;
    let line_no = pane.top_line + (y - rect.y) as usize;
    if line_no >= pane.buffer.line_count() {
        return None;
    }
    let gutter = gutter_width(pane.buffer.line_count(), view.number);
    if x < rect.x + gutter {
        // The line-number column is not text.
        return None;
    }

    let tabstop = view.tabstop.max(1);
    // The click is in the window's cells; the walk is in the line's, and the
    // scroll is where they meet.
    let target = (x - rect.x - gutter) as usize + pane.left_col;
    let width = rect.width.saturating_sub(gutter) as usize;
    let buffer = pane.buffer;
    let left_col = pane.left_col;
    let line = buffer.line_string(line_no);
    let mut cells = 0usize;
    let mut char_idx = 0usize;
    for grapheme in line.graphemes(true) {
        let chars = grapheme.chars().count();
        let span = crate::screen::cell_advance(grapheme, cells, tabstop);
        if span == 0 {
            // A combining mark is drawn with the character before it, so there
            // is no cell of its own to click.
            char_idx += chars;
            continue;
        }
        if target < cells + span {
            let col = buffer
                .char_to_line_col(buffer.line_col_to_char(line_no, char_idx))
                .1;
            return Some((window, Cursor::new(line_no, col)));
        }
        cells += span;
        if cells >= left_col + width {
            break;
        }
        char_idx += chars;
    }

    // Past the end of the line, or of what is drawn of it: the end of the line.
    Some((window, Cursor::new(line_no, buffer.line_len(line_no))))
}

/// How much of the prompt line to leave out, and which cell the cursor lands
/// on, in cells from the left edge.
///
/// Characters are counted rather than display columns: a command line is being
/// typed into, and a wide character in it is worth the two cells it costs
/// rather than a second layout pass.
fn prompt_layout(line: &str, input_cursor: usize, width: usize) -> (usize, usize) {
    let cells = input_cursor + 1; // the prompt character, then the input
    if width == 0 {
        return (cells, 0);
    }
    let skip = cells.saturating_sub(width - 1);
    let _ = line;
    (skip, cells - skip)
}

/// Where the terminal cursor belongs while a line is being typed.
fn prompt_cursor(view: &View<'_>, line: &str, input_cursor: usize) -> ((u16, u16), bool) {
    let vp = view.viewport;
    if vp.width == 0 || vp.height == 0 {
        return ((0, 0), false);
    }
    let (_, x) = prompt_layout(line, input_cursor, vp.width as usize);
    ((x.min((vp.width - 1) as usize) as u16, vp.height - 1), true)
}

/// Where the terminal cursor belongs, if the cursor is on screen.
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
pub(crate) fn gutter_width(line_count: usize, number: bool) -> u16 {
    if number {
        (digits(line_count) + 1) as u16
    } else {
        0
    }
}

fn draw_line_number(
    grid: &mut Grid,
    x: u16,
    row: u16,
    gutter: u16,
    number: usize,
    color: Option<Color>,
) {
    let field = gutter.saturating_sub(1) as usize;
    if field == 0 {
        return;
    }
    let text = format!("{number:>field$}");
    for (at, ch) in text.chars().enumerate() {
        if at >= field {
            break;
        }
        let mut cell = Cell::new(ch.to_string());
        if let Some(color) = color {
            cell = cell.with_fg(color);
        }
        grid.put(x + at as u16, row, cell);
    }
}

/// How the text area is laid out: everything `draw_line` needs to know that is
/// the same for every row.
struct TextLayout {
    /// Column of the pane's left edge.
    x: u16,
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
/// What the highlight on one row of the screen is: a selection, the search
/// matches, and the syntax colours, all as character columns of that row.
#[derive(Debug, Clone, Default)]
struct RowHighlight {
    /// The visual selection, if it covers any of this row.
    selected: Option<(usize, usize)>,
    /// The search matches on this row, in order.
    matches: Vec<(usize, usize)>,
    /// The one the cursor is inside, which is drawn more strongly.
    current: Option<(usize, usize)>,
    /// The syntax colours of this row: a column range and the style it takes.
    /// Resolved here rather than in the walk, so the walk only compares numbers.
    syntax: Vec<(usize, usize, (Color, u16))>,
    /// Where a language server says something is wrong on this row, and how the
    /// underline is drawn.
    diagnostics: Vec<(usize, usize, u16)>,
}

impl RowHighlight {
    /// The colour and the attribute bits for the character at column `column`.
    ///
    /// Four things at once, and they have to compose rather than replace each
    /// other: a keyword keeps its colour while it is selected, and a search
    /// match inside a string is still underlined. The bits are what the protocol
    /// already carries, so none of this is frontend work.
    fn style(&self, column: usize) -> (Option<Color>, u16) {
        let mut color = None;
        let mut attrs = 0;
        for (from, to, (token_color, token_attrs)) in &self.syntax {
            if column >= *from && column < *to {
                color = Some(*token_color);
                attrs |= *token_attrs;
                break;
            }
        }
        if matches!(self.selected, Some((from, to)) if column >= from && column < to) {
            attrs |= ATTR_REVERSE;
        }
        for (from, to) in &self.matches {
            if column >= *from && column < *to {
                attrs |= ATTR_UNDERLINE;
                if self.current.is_some_and(|(s, e)| *from == s && *to == e) {
                    attrs |= ATTR_BOLD;
                }
                break;
            }
        }
        // A diagnostic underlines too, and composes with everything above: a
        // keyword with a problem in it keeps its colour.
        for (from, to, bits) in &self.diagnostics {
            if column >= *from && column < *to {
                attrs |= *bits;
                break;
            }
        }
        (color, attrs)
    }
}

/// Write a cell, with the highlight bits on it when there are any.
///
/// The highlight is what the protocol has over the wire: the cell's attribute
/// bits, with no second list of screen state to keep in step.
fn put_cell(grid: &mut Grid, x: u16, row: u16, cell: Cell, attrs: u16) {
    grid.put(x, row, cell.with_attrs(attrs));
}

/// Write a grapheme and the cells it covers, highlighted or not.
///
/// A wide grapheme takes its continuation cell with it, so half a CJK character
/// cannot end up highlighted.
fn put_grapheme(grid: &mut Grid, x: u16, row: u16, width: usize, cell: Cell, attrs: u16) {
    grid.put_grapheme(x, row, width, cell.with_attrs(attrs));
}

/// starts at — the whole point being that the cursor is placed by the same walk
/// that draws, so tabs and double-width graphemes cannot push it off the
/// character it is supposed to be sitting on.
fn draw_line(
    grid: &mut Grid,
    row: u16,
    line: &str,
    layout: &TextLayout,
    cursor_char: Option<usize>,
    highlight: &RowHighlight,
) -> Option<u16> {
    let left = layout.x as usize + layout.gutter as usize;
    let right = left + layout.width;
    let left_col = layout.left_col;
    let tabstop = layout.tabstop.max(1);
    let mut x = left;
    let mut char_idx = 0usize;
    // The cell of the line the next grapheme starts at. The scroll is a cell
    // offset, so a grapheme can be half to the left of the view — a tab, whose
    // remaining cells are blank — and the drawing has to walk in the line's own
    // cells rather than the window's.
    let mut cells = 0usize;
    let mut found = None;
    // Whether the cursor's character was passed over on the left, which is off
    // the screen rather than at the end of what is drawn.
    let mut skipped = false;

    for grapheme in line.graphemes(true) {
        let grapheme_chars = grapheme.chars().count();
        let width = crate::screen::cell_advance(grapheme, cells, tabstop);
        let on_cursor = Some(char_idx) == cursor_char;
        if cells + width <= left_col {
            // Wholly to the left of the view.
            skipped = skipped || on_cursor;
            cells += width;
            char_idx += grapheme_chars;
            continue;
        }
        if cells < left_col {
            // The view starts inside this one: only a tab is wide enough for
            // that, and its cells left of the view stay blank. The text after it
            // begins where the tab's cells end.
            if on_cursor {
                found = Some(x as u16);
            }
            x = left + (cells + width - left_col);
            cells += width;
            char_idx += grapheme_chars;
            continue;
        }
        if on_cursor {
            found = Some(x as u16);
        }
        if x >= right {
            break;
        }
        let (color, attrs) = highlight.style(char_idx);
        let color = color.unwrap_or(Color::Reset);
        if grapheme == "\t" {
            for _ in 0..width {
                if x >= right {
                    break;
                }
                put_cell(grid, x as u16, row, Cell::new(" ").with_fg(color), attrs);
                x += 1;
            }
        } else {
            if width == 0 {
                cells += width;
                char_idx += grapheme_chars;
                continue;
            }
            if x + width > right {
                break;
            }
            put_grapheme(
                grid,
                x as u16,
                row,
                width,
                Cell::new(grapheme).with_fg(color),
                attrs,
            );
            x += width;
        }
        cells += width;
        char_idx += grapheme_chars;
    }

    // The cursor past the end of the line (insert mode) or beyond the
    // truncation point belongs at the end of the drawn text; one that the walk
    // passed on the left is off the screen, and says so.
    match (&found, cursor_char) {
        (None, Some(_)) if !skipped => Some(x.min(u16::MAX as usize) as u16),
        _ => found,
    }
}

/// Blank the whole row, so attributes (e.g. reverse video) apply edge to edge.
/// The syntax colours of buffer line `line_no`, resolved to the style each
/// span takes.
/// Where the diagnostics on a row are, as column ranges and the bits they are
/// drawn with.
fn diagnostic_columns(pane: &Pane<'_>, line_no: usize) -> Vec<(usize, usize, u16)> {
    let line_len = pane.buffer.line_len(line_no);
    pane.diagnostics
        .iter()
        .filter_map(|diagnostic| {
            let (from, to) = diagnostic.on_row(line_no)?;
            let from = from.min(line_len);
            let to = to.min(line_len).max(from);
            Some((
                from,
                to.max(from + 1).min(line_len.max(1)),
                diagnostic.severity.attrs(),
            ))
        })
        .collect()
}

fn syntax_columns(pane: &Pane<'_>, line_no: usize) -> Vec<(usize, usize, (Color, u16))> {
    let Some(highlighter) = pane.highlighter else {
        return Vec::new();
    };
    highlighter
        .spans(pane.buffer, line_no)
        .into_iter()
        .map(|span| (span.start, span.end, crate::highlight::style(span.token)))
        .collect()
}

/// The character columns of buffer line `line_no` that the search matches.
///
/// A match never crosses a line (see `search.rs`), so each one is either wholly
/// inside this row or not in it at all.
fn match_columns(pane: &Pane<'_>, line_no: usize) -> Vec<(usize, usize)> {
    let line_start = pane.buffer.line_col_to_char(line_no, 0);
    let line_len = pane.buffer.line_len(line_no);
    pane.matches
        .iter()
        .filter_map(|(start, end)| columns_in(*start, *end, line_start, line_len))
        .collect()
}

/// The columns of a row that `[start, end)` covers, if any of it.
fn columns_in(
    start: usize,
    end: usize,
    line_start: usize,
    line_len: usize,
) -> Option<(usize, usize)> {
    let from = start.saturating_sub(line_start);
    let to = (end.saturating_sub(line_start)).min(line_len);
    (start < end && from < to).then_some((from, to))
}

/// The lines a pane draws, from its top line on, for as many rows as it has.
///
/// A closed fold contributes its first line and nothing else.
fn visible_lines(pane: &Pane<'_>, rows: usize) -> Vec<usize> {
    let last = pane.buffer.line_count().saturating_sub(1);
    let mut out = Vec::with_capacity(rows);
    let mut line = pane.top_line.min(last);
    while out.len() < rows && line <= last {
        out.push(line);
        line = match closed_fold_at(pane, line) {
            Some(fold) => fold.end + 1,
            None => line + 1,
        };
    }
    out
}

/// The closed fold that starts at `line`, if one does.
fn closed_fold_at(pane: &Pane<'_>, line: usize) -> Option<crate::editor::Fold> {
    (0..pane.folds.len())
        .find(|index| {
            crate::editor::fold_is_shut(pane.folds, pane.foldlevel, pane.foldminlines, *index)
                && pane.folds[*index].start == line
        })
        .map(|index| pane.folds[index])
}

/// What a closed fold looks like: its first line's text, with a marker saying how
/// many lines are behind it.
///
/// Vim lets this be replaced by a function (`'foldtext'`); this is the shape of
/// vim's default, and the roadmap has the option.
fn fold_text(pane: &Pane<'_>, line: usize, fold: crate::editor::Fold) -> String {
    // What is *hidden* is the count worth saying: a fold of two lines hides one
    // of them, and "+--  1 line" is what that is.
    let hidden = fold.end - fold.start;
    format!(
        "+--  {hidden} line{}: {}",
        if hidden == 1 { "" } else { "s" },
        pane.buffer.line_string(line)
    )
}

/// The character columns of buffer line `line_no` that the visual selection
/// covers.
///
/// The selection arrives as a character range over the whole buffer, so a row
/// takes whatever part of that range falls inside it. A linewise selection
/// reaches a row's terminator; the columns stop at the line's own length, since
/// there are no cells past the last character to highlight.
fn selection_columns(pane: &Pane<'_>, line_no: usize) -> Option<(usize, usize)> {
    // A block is per-row from the start: the row is inside the rectangle or it
    // is not, and the columns are the rectangle's, clipped to what the line has.
    if let Some((first, last, left, right)) = pane.block {
        if line_no < first || line_no > last {
            return None;
        }
        let len = pane.buffer.line_len(line_no);
        let from = left.min(len);
        // `None` is a block that reaches the end of each line, so the highlight
        // does too.
        let to = match right {
            None => len,
            Some(right) => (right + 1).min(len),
        };
        return (from < to).then_some((from, to));
    }
    let (start, end) = pane.selection?;
    let line_start = pane.buffer.line_col_to_char(line_no, 0);
    let line_len = pane.buffer.line_len(line_no);
    columns_in(start, end, line_start, line_len)
}

fn fill_row(grid: &mut Grid, row: u16, from: u16, width: u16, attrs: u16) {
    for x in from..from.saturating_add(width).min(grid.width) {
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
pub fn status_line(
    label: &str,
    name: &str,
    dirty: bool,
    cursor: Cursor,
    diagnostics: &[Diagnostic],
) -> String {
    format!(
        "-- {label} --  {}{}  {}:{}{}",
        name,
        if dirty { "[+]" } else { "" },
        cursor.row + 1,
        cursor.col + 1,
        diagnostics_count(diagnostics),
    )
}

/// What a language server has said about this file, counted by what it is.
///
/// Only the kinds that are there are shown, and in the order a person cares
/// about them: errors first. A file with nothing said about it says nothing —
/// the status line is not the place for a row of zeroes.
fn diagnostics_count(diagnostics: &[Diagnostic]) -> String {
    let count = |severity: Severity| {
        diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.severity == severity)
            .count()
    };
    let mut parts = Vec::new();
    for (letter, severity) in [
        ("E", Severity::Error),
        ("W", Severity::Warning),
        ("I", Severity::Information),
        ("H", Severity::Hint),
    ] {
        let found = count(severity);
        if found > 0 {
            parts.push(format!("{letter}{found}"));
        }
    }
    if parts.is_empty() {
        return String::new();
    }
    format!("  {}", parts.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Case {
        buffer: Buffer,
        cursor: Cursor,
        mode: Mode,
        selection: Option<(usize, usize)>,
        block: Option<(usize, usize, usize, Option<usize>)>,
        folds: Vec<crate::editor::Fold>,
        foldlevel: usize,
        foldminlines: usize,
        matches: Vec<(usize, usize)>,
        current_match: Option<(usize, usize)>,
        incsearch: bool,
        highlighter: Option<crate::highlight::LineStates>,
        viewport: Viewport,
        tabstop: usize,
        number: bool,
        cmdline: Option<String>,
        cmdline_cursor: Option<usize>,
        message: String,
    }

    impl Case {
        fn new(text: &str, width: u16, height: u16) -> Self {
            Self {
                buffer: Buffer::from_text(text),
                cursor: Cursor::new(0, 0),
                mode: Mode::Normal,
                selection: None,
                block: None,
                folds: Vec::new(),
                foldlevel: 0,
                foldminlines: 1,
                matches: Vec::new(),
                current_match: None,
                incsearch: false,
                highlighter: None,
                viewport: Viewport::new(width, height),
                tabstop: 4,
                number: false,
                cmdline: None,
                cmdline_cursor: None,
                message: String::new(),
            }
        }

        fn view(&self) -> View<'_> {
            View {
                buffer: &self.buffer,
                cursor: self.cursor,
                mode: self.mode,
                selection: self.selection,
                block: self.block,
                folds: &self.folds,
                foldlevel: self.foldlevel,
                foldminlines: self.foldminlines,
                matches: &self.matches,
                current_match: self.current_match,
                incsearch: self.incsearch,
                highlighter: self.highlighter.as_ref(),
                viewport: self.viewport,
                rects: Vec::new(),
                documents: &[],
                windows: &[],
                current_window: 0,
                completion: None,
                diagnostics: &[],
                dirty: false,
                tabstop: self.tabstop,
                number: self.number,
                status: status_line(self.mode.name(), "t.txt", false, self.cursor, &[]),
                cmdline: self.cmdline.clone(),
                cmdline_cursor: self.cmdline_cursor,
                message: self.message.clone(),
            }
        }

        fn compose(&self) -> Composed {
            compose(&self.view())
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
    fn a_horizontal_scroll_starts_at_the_cell_it_says() {
        // For a plain line a cell is a character, so this is the arithmetic
        // underneath the tab case below.
        let mut c = Case::new("abcdefghij\n", 5, 3);
        c.viewport.left_col = 3;
        let out = c.compose();
        assert_eq!(row(&out, 0), "defgh");
        // Cursor hunted off to the left is not drawn.
        c.cursor = Cursor::new(0, 1);
        assert!(!c.compose().cursor_visible);
    }

    #[test]
    fn a_horizontal_scroll_counts_cells_and_not_characters() {
        // With the default tabstop of four, "\t\tabcdefghij" puts the `a` at the
        // *ninth* cell and the third character. A scroll counted in characters
        // would show the middle of the text and leave the cursor — which is on
        // the first cell shown — off the screen entirely.
        let mut c = Case::new("\t\tabcdefghij\n", 10, 3);
        c.viewport.left_col = 8;
        c.cursor = Cursor::new(0, 2);
        let out = c.compose();
        assert_eq!(row(&out, 0), "abcdefghij", "the view starts at the `a`");
        assert!(out.cursor_visible, "and the cursor is on the screen");
        assert_eq!(
            out.cursor,
            (0, 0),
            "at the left edge, where the view starts"
        );
    }

    #[test]
    fn a_view_that_starts_inside_a_tab_draws_what_is_left_of_it() {
        // A tab is a run of cells, so a view can start in the middle of one:
        // what is left of it is blank, and the text after it starts where the
        // tab's cells end.
        let mut c = Case::new("\tabc\n", 10, 3);
        c.viewport.left_col = 2;
        c.cursor = Cursor::new(0, 1);
        let out = c.compose();
        assert_eq!(row(&out, 0), "  abc", "the last two cells of the tab");
        assert_eq!(out.cursor.0, 2, "and the `a` follows them");
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
    fn command_mode_shows_the_line_being_typed() {
        // The prompt character is the editor's (`:` command, `/` or `?`
        // search): the renderer draws the line it is handed.
        let mut c = Case::new("x\n", 20, 3);
        c.mode = Mode::Command;
        c.cmdline = Some(":w".into());
        c.message = "ignored while in command mode".into();
        let out = c.compose();
        assert_eq!(row(&out, 2), ":w");

        c.cmdline = Some("/foo".into());
        assert_eq!(row(&c.compose(), 2), "/foo");
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
    fn a_click_lands_on_the_character_that_is_drawn_there() {
        let mut c = Case::new("one two\nthree\n", 20, 5);
        // The click comes back as (window, position): a click in another window
        // has to go there first.
        let at = |x, y| position_at(&c.view(), x, y).map(|(_, cursor)| cursor);
        assert_eq!(at(0, 0), Some(Cursor::new(0, 0)), "the first character");
        assert_eq!(at(2, 0), Some(Cursor::new(0, 2)), "the third one");
        assert_eq!(
            at(4, 1),
            Some(Cursor::new(1, 4)),
            "the fifth character of the second line"
        );
        assert_eq!(
            at(19, 0),
            Some(Cursor::new(0, 7)),
            "past the end of the line"
        );
        assert_eq!(at(0, 3), None, "the rows below the file are not text");
        assert_eq!(at(0, 4), None, "nor is the status row");

        // A gutter is not text, and shifts all of it along.
        c.number = true;
        let at = |x, y| position_at(&c.view(), x, y).map(|(_, cursor)| cursor);
        assert_eq!(at(0, 0), None, "the line number");
        assert_eq!(
            at(2, 0),
            Some(Cursor::new(0, 0)),
            "the first character is after it"
        );
    }

    #[test]
    fn two_windows_are_drawn_in_their_own_rectangles() {
        use crate::editor::{Document, Rect, Window};
        let mut documents = vec![
            Document::new(
                Buffer::from_text("left\n"),
                crate::highlight::Language::Plain,
            ),
            Document::new(
                Buffer::from_text("right\n"),
                crate::highlight::Language::Plain,
            ),
        ];
        let mut windows = vec![Window::new(0), Window::new(1)];
        windows[1].cursor = Cursor::new(0, 3);
        documents[1].cursor = Cursor::new(0, 3);
        let mut c = Case::new("the current document\n", 61, 4);
        c.cursor = Cursor::new(0, 4);
        let mut view = c.view();
        view.rects = vec![
            Rect {
                x: 0,
                y: 0,
                width: 30,
                height: 2,
            },
            Rect {
                x: 31,
                y: 0,
                width: 30,
                height: 2,
            },
        ];
        view.documents = &documents;
        view.windows = &windows;
        view.current_window = 0;

        let out = compose(&view);
        // The current window's document on the left, the other window's on the
        // right, a separator between them.
        assert_eq!(row(&out, 0), "the current document          │right");
        // Each window has its own status line, and the current one's cursor is
        // the one on it.
        assert!(
            row(&out, 1).starts_with("-- NORMAL --  t.txt  1:5"),
            "{}",
            row(&out, 1)
        );
        assert!(
            row(&out, 1).contains("│-- NORMAL --  [No Name]  1:4"),
            "{}",
            row(&out, 1)
        );
        // The cursor is drawn in the current window only: the first line of the
        // left pane is at row 0, and the tag is in column 4.
        assert_eq!(out.cursor, (4, 0));

        // A click in the right window is answered with that window and the
        // position in *its* document.
        let (window, at) = position_at(&view, 32, 0).expect("the right window");
        assert_eq!(window, 1);
        assert_eq!(at, Cursor::new(0, 1));
        // The column between them is a separator, not text, and the row below
        // the panes is nobody's.
        assert!(position_at(&view, 30, 0).is_none());
        assert!(position_at(&view, 0, 2).is_none());
    }

    #[test]
    fn a_click_on_a_tab_lands_on_the_tab() {
        let c = Case::new("a\tb\n", 20, 3);
        let at = |x, y| position_at(&c.view(), x, y).map(|(_, cursor)| cursor);
        assert_eq!(at(0, 0), Some(Cursor::new(0, 0)), "the `a`");
        assert_eq!(at(1, 0), Some(Cursor::new(0, 1)), "the tab's columns");
        assert_eq!(at(3, 0), Some(Cursor::new(0, 1)), "all of them");
        assert_eq!(at(4, 0), Some(Cursor::new(0, 2)), "then the `b`");
    }

    #[test]
    fn a_click_on_a_wide_character_lands_on_the_character() {
        let c = Case::new("a你b\n", 20, 3);
        let at = |x, y| position_at(&c.view(), x, y).map(|(_, cursor)| cursor);
        assert_eq!(at(1, 0), Some(Cursor::new(0, 1)), "the left half");
        assert_eq!(
            at(2, 0),
            Some(Cursor::new(0, 1)),
            "and the right half is the same character, not the next one"
        );
        assert_eq!(at(3, 0), Some(Cursor::new(0, 2)));
    }

    #[test]
    fn a_click_after_a_horizontal_scroll_counts_from_what_is_drawn() {
        let mut c = Case::new(&format!("{}\n", "abcdefghij".repeat(4)), 12, 3);
        c.viewport.left_col = 5;
        assert_eq!(
            position_at(&c.view(), 0, 0).map(|(_, cursor)| cursor),
            Some(Cursor::new(0, 5)),
            "the first drawn character is the one the scroll starts at"
        );
        assert_eq!(
            position_at(&c.view(), 2, 0).map(|(_, cursor)| cursor),
            Some(Cursor::new(0, 7))
        );
    }

    #[test]
    fn syntax_colours_a_row_by_what_its_text_is() {
        let mut c = Case::new("let x = 42; // the answer\n", 40, 3);
        c.highlighter = Some(crate::highlight::LineStates::new(
            crate::highlight::Language::Rust,
        ));
        let out = c.compose();

        let (keyword_color, keyword_attrs) =
            crate::highlight::style(crate::highlight::Token::Keyword);
        assert_eq!(out.grid.get(0, 0).fg, keyword_color, "`let` is a keyword");
        assert_eq!(out.grid.get(0, 0).attrs & keyword_attrs, keyword_attrs);

        let (number_color, _) = crate::highlight::style(crate::highlight::Token::Number);
        assert_eq!(out.grid.get(8, 0).fg, number_color, "`42` is a number");
        assert_eq!(out.grid.get(7, 0).fg, Color::Reset, "and `=` is neither");

        let (comment_color, comment_attrs) =
            crate::highlight::style(crate::highlight::Token::Comment);
        assert_eq!(out.grid.get(13, 0).fg, comment_color, "the comment");
        assert_eq!(out.grid.get(13, 0).attrs & comment_attrs, comment_attrs);
    }

    #[test]
    fn a_colour_and_a_highlight_compose_rather_than_replace() {
        let mut c = Case::new("let x = 1;\n", 40, 3);
        c.highlighter = Some(crate::highlight::LineStates::new(
            crate::highlight::Language::Rust,
        ));
        // The same cell is a keyword, part of the selection, and a search match.
        c.selection = Some((0, 3));
        c.matches = vec![(0, 3)];
        c.current_match = Some((0, 3));
        let out = c.compose();

        let (keyword_color, keyword_attrs) =
            crate::highlight::style(crate::highlight::Token::Keyword);
        let cell = out.grid.get(0, 0);
        assert_eq!(cell.fg, keyword_color, "it keeps its colour");
        assert_eq!(
            cell.attrs & keyword_attrs,
            keyword_attrs,
            "and its own attributes"
        );
        assert_eq!(cell.attrs & ATTR_REVERSE, ATTR_REVERSE, "and is selected");
        assert_eq!(
            cell.attrs & ATTR_UNDERLINE,
            ATTR_UNDERLINE,
            "and is a search match"
        );
        assert_eq!(
            cell.attrs & ATTR_BOLD,
            ATTR_BOLD,
            "and it is the current one"
        );
    }

    #[test]
    fn a_fold_minlines_keeps_its_lines_on_screen() {
        // `'foldminlines'` is a drawing rule, so this is where it has to be
        // proved: the same fold, drawn shut and then drawn open.
        let mut case = Case::new("one\ntwo\nthree\n", 20, 8);
        case.folds = vec![crate::editor::Fold {
            start: 0,
            end: 1,
            closed: true,
        }];
        let out = case.compose();
        assert_eq!(row(&out, 0), "+--  1 line: one", "shut: the marker");
        assert_eq!(row(&out, 1), "three", "and the line after it");

        // Two lines is not *above* two, so the fold is drawn open — and the fold
        // is still closed, which the last test in `folds.rs` checks in the
        // kernel.
        case.foldminlines = 2;
        let out = case.compose();
        assert_eq!(row(&out, 0), "one", "open: the first line");
        assert_eq!(row(&out, 1), "two", "and the one it would have hidden");
        assert_eq!(row(&out, 2), "three");
    }

    #[test]
    fn the_cursor_is_drawn_in_the_first_column_on_a_fold_line() {
        // "While the cursor is on folded lines, the cursor is always *displayed* in the
        // first column" — and the fold line is the marker, so a column inside it would
        // point at nothing.
        let mut case = Case::new("one\ntwo\nthree\nfour\n", 20, 8);
        case.folds = vec![crate::editor::Fold {
            start: 1,
            end: 2,
            closed: true,
        }];
        case.cursor = Cursor::new(1, 2);
        let out = case.compose();
        assert_eq!(out.cursor, (0, 1), "the first column of the fold's line");
        assert!(out.cursor_visible);
        // A cursor on a line that is *not* folded is drawn where it is — on the row it
        // has on screen, which the fold above it has moved up by one.
        case.cursor = Cursor::new(3, 2);
        let out = case.compose();
        assert_eq!(
            out.cursor,
            (2, 2),
            "its own column, on the row it is drawn on"
        );
    }

    #[test]
    fn a_block_selection_is_highlighted_as_a_rectangle() {
        // Three rows, and the rectangle's columns on each of them: the row that
        // is too short contributes only what it has, and no cells past its end.
        let mut case = Case::new("one two\nsix four\nten six\nab\n", 20, 8);
        case.mode = Mode::Visual;
        case.block = Some((0, 3, 4, Some(6)));
        let out = case.compose();
        let text = |row: u16| {
            (0..20)
                .map(|x| out.grid.get(x, row).text.clone())
                .collect::<String>()
        };
        // The selected cells carry the selection attribute; the text is the same
        // either way, so what is checked is *which* cells are marked.
        let marked = |row: u16| {
            (0..20)
                .filter(|x| out.grid.get(*x, row).attrs != 0)
                .collect::<Vec<u16>>()
        };
        assert!(text(0).starts_with("one two"), "{:?}", text(0));
        assert_eq!(marked(0), vec![4, 5, 6], "`two` on the first row");
        assert_eq!(marked(1), vec![4, 5, 6], "`fou` on the second");
        assert_eq!(marked(2), vec![4, 5, 6], "`six` on the third");
        assert_eq!(marked(3), Vec::<u16>::new(), "the short row has none");
        assert!(text(3).starts_with("ab"), "{:?}", text(3));
    }

    #[test]
    fn a_block_comment_keeps_its_colour_down_the_screen() {
        // The state crosses lines, so this is only true if the cache is used.
        let mut c = Case::new(
            "/* a comment\nstill in it\nnot any more */ code();\n",
            40,
            5,
        );
        c.highlighter = Some(crate::highlight::LineStates::new(
            crate::highlight::Language::Rust,
        ));
        let out = c.compose();

        let (comment_color, _) = crate::highlight::style(crate::highlight::Token::Comment);
        assert_eq!(out.grid.get(0, 1).fg, comment_color, "the second line too");
        assert_eq!(out.grid.get(0, 2).fg, comment_color, "and the third");
        let (function_color, _) = crate::highlight::style(crate::highlight::Token::Function);
        assert_eq!(
            out.grid.get(16, 2).fg,
            function_color,
            "until the comment ends at column 14 and the code starts again"
        );
    }

    #[test]
    fn the_cursor_is_at_the_match_while_a_search_is_being_typed() {
        // A search that is showing its match takes the cursor out of the line
        // being typed and puts it on the match, which is where vim shows it.
        let mut c = Case::new("one two\n", 20, 3);
        c.mode = Mode::Command;
        c.cmdline = Some("/two".into());
        c.cmdline_cursor = Some(4);
        let out = c.compose();
        assert_eq!(out.cursor, (5, 2), "the prompt holds the cursor");
        assert!(out.cursor_visible);

        c.incsearch = true;
        c.cursor = Cursor::new(0, 4);
        let out = c.compose();
        assert_eq!(out.cursor, (4, 0), "and now the match does");
        assert_eq!(row(&out, 2), "/two", "the line is still drawn");
        assert!(out.cursor_visible);
    }

    #[test]
    fn the_prompt_shows_where_the_cursor_is_and_scrolls_to_it() {
        let mut c = Case::new("x\n", 10, 3);
        c.mode = Mode::Command;
        c.cmdline = Some(":wq".into());
        c.cmdline_cursor = Some(1);
        let out = c.compose();
        assert_eq!(row(&out, 2), ":wq");
        assert_eq!(
            out.cursor,
            (2, 2),
            "after the prompt character and one typed character"
        );
        assert!(out.cursor_visible);

        // A line longer than the window scrolls, so the cursor cannot be typed
        // off the edge of the screen.
        c.cmdline = Some(format!(":{}|", "x".repeat(20)));
        c.cmdline_cursor = Some(21);
        let out = c.compose();
        assert!(
            out.cursor.0 < 10,
            "the cursor is inside the window: {:?}",
            out.cursor
        );
        assert!(
            row(&out, 2).ends_with('|'),
            "and the end of the line is on screen: {:?}",
            row(&out, 2)
        );
    }

    #[test]
    fn the_selection_is_highlighted_cell_by_cell() {
        let mut c = Case::new("ab\u{4f60}\u{597d}cd\n", 12, 3);
        // Characters 1..3 are `b` and the wide CJK character, which covers two
        // cells of its own.
        c.selection = Some((1, 3));
        let out = c.compose();

        let reversed: Vec<u16> = (0..12)
            .filter(|x| out.grid.get(*x, 0).attrs & ATTR_REVERSE != 0)
            .collect();
        assert_eq!(
            reversed,
            vec![1, 2, 3],
            "the selected cells, with the wide grapheme's second cell included"
        );
        assert_eq!(
            out.grid.get(0, 0).attrs & ATTR_REVERSE,
            0,
            "and nothing outside them"
        );
        assert_eq!(out.grid.get(4, 0).attrs & ATTR_REVERSE, 0);

        // The status line is reverse video whether or not anything is selected.
        assert_ne!(out.grid.get(0, 1).attrs & ATTR_REVERSE, 0);
    }

    #[test]
    fn a_selection_that_reaches_the_end_of_a_line_stops_at_its_content() {
        let mut c = Case::new("one\ntwo\n", 8, 4);
        // A linewise selection: line 0 and its terminator.
        c.selection = Some((0, 4));
        let out = c.compose();
        let reversed: Vec<u16> = (0..8)
            .filter(|x| out.grid.get(*x, 0).attrs & ATTR_REVERSE != 0)
            .collect();
        assert_eq!(
            reversed,
            vec![0, 1, 2],
            "the line's characters, not the padding after them"
        );
        let next: Vec<u16> = (0..8)
            .filter(|x| out.grid.get(*x, 1).attrs & ATTR_REVERSE != 0)
            .collect();
        assert!(next.is_empty(), "and not the line after it");
    }

    #[test]
    fn the_status_line_counts_what_a_server_said() {
        let cursor = Cursor::new(0, 0);
        let diagnostic = |severity| Diagnostic {
            start: (0, 0),
            end: (0, 1),
            severity,
            message: "x".to_string(),
        };
        // Nothing said, nothing shown: the status line is not a row of zeroes.
        assert_eq!(
            status_line("NORMAL", "a.txt", false, cursor, &[]),
            "-- NORMAL --  a.txt  1:1"
        );
        // Errors first, and only the kinds that are there.
        let said = vec![
            diagnostic(Severity::Warning),
            diagnostic(Severity::Error),
            diagnostic(Severity::Warning),
            diagnostic(Severity::Hint),
        ];
        assert_eq!(
            status_line("NORMAL", "a.txt", false, cursor, &said),
            "-- NORMAL --  a.txt  1:1  E1 W2 H1"
        );
        let only_info = vec![diagnostic(Severity::Information)];
        assert_eq!(
            status_line("NORMAL", "a.txt", true, cursor, &only_info),
            "-- NORMAL --  a.txt[+]  1:1  I1"
        );
    }

    #[test]
    fn status_line_format() {
        let mut c = Case::new("x\n", 20, 3);
        c.cursor = Cursor::new(0, 1);
        assert_eq!(
            status_line("INSERT", "a.txt", true, c.cursor, &[]),
            "-- INSERT --  a.txt[+]  1:2"
        );
        assert_eq!(
            status_line("NORMAL", "[No Name]", false, c.cursor, &[]),
            "-- NORMAL --  [No Name]  1:2"
        );
        // The label is a string, so a selection can say what it covers.
        assert_eq!(
            status_line("VISUAL LINE", "a.txt", false, c.cursor, &[]),
            "-- VISUAL LINE --  a.txt  1:2"
        );
    }
}
