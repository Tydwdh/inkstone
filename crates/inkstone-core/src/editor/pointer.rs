use inkstone_rpc::{PointerKind, PointerMsg};

use crate::cursor::Cursor;
use crate::mode::{Mode, VisualKind};
use crate::render::{gutter_width, position_at};
use crate::screen::scroll_to_cursor;

use super::*;

impl Editor {
    /// The terminal changed size.
    pub fn handle_resize(&mut self, cols: u16, rows: u16) {
        self.viewport.resize(cols, rows);
        // A program that draws was told how big its terminal is, and it needs to
        // be told again: a full-screen program that keeps painting an old size is
        // wrong in a way a person sees immediately.
        self.resize_terminal();
        self.reposition();
        self.redraw = true;
    }

    /// The pointer did something, in screen cells.
    ///
    /// A press puts the cursor where it was pointed and starts a selection; a
    /// drag extends it; a release that never dragged collapses it again, which
    /// is what makes a click a click. The wheel moves the view and leaves the
    /// cursor where the user left it.
    pub fn handle_pointer(&mut self, msg: &PointerMsg) {
        match msg.kind {
            PointerKind::Press => {
                // A press on the line between two windows moves that line, and
                // does not start a selection: the separator is not text.
                if let Some(resizing) = self.separator_at(msg.x, msg.y) {
                    self.resizing = Some(resizing);
                    self.selection = None;
                    self.mode = Mode::Normal;
                    self.clear_pending();
                    self.redraw = true;
                    return;
                }
                self.resizing = None;
                // Anything that was selected is done with: a click starts over.
                self.selection = None;
                self.mode = Mode::Normal;
                self.clear_pending();
                // A click, a double click and the rest are one gesture with a
                // count, and the count says what is selected: "double → word or
                // % match, triple → line, quadruple → rectangular block".
                self.pointer_clicks = msg.clicks.max(1);
                if let Some((window, at)) = self.position_at_screen(msg.x, msg.y) {
                    // A press in another window goes to that window first, the
                    // way a click in vim does.
                    self.switch_to_window(window);
                    self.cursor = at;
                    self.clamp_cursor();
                    match self.pointer_clicks {
                        1 => {
                            self.selection = Some(Selection {
                                anchor: self.cursor,
                                kind: VisualKind::Charwise,
                                to_end: false,
                            })
                        }
                        2 => self.select_clicked_word(),
                        3 => self.select_clicked_line(),
                        _ => self.select_clicked_block(),
                    }
                    self.mode = Mode::Visual;
                    self.mouse_dragged = false;
                    self.after_edit();
                }
            }
            PointerKind::Drag => {
                if let Some(resizing) = self.resizing {
                    // The size is the one it started at, moved by how far the
                    // pointer has come — not by how many events arrived.
                    let delta = match resizing.axis {
                        Axis::Columns => msg.x as i32 - resizing.at as i32,
                        Axis::Rows => msg.y as i32 - resizing.at as i32,
                    };
                    let size = (resizing.base as i32 + delta).max(1);
                    self.resize_window_at(
                        resizing.window,
                        resizing.axis,
                        Resize::Set(size.min(u16::MAX as i32) as u16),
                    );
                    self.redraw = true;
                    return;
                }
                if self.selection.is_some() {
                    if let Some((window, at)) = self.position_at_screen(msg.x, msg.y) {
                        // A drag that leaves this window is still this window's
                        // selection: the anchor is here.
                        if window == self.current_window {
                            self.mouse_dragged = true;
                            self.cursor = at;
                            // "Dragging the mouse with a double click … will
                            // result in whole words to be selected. This
                            // continues until the button is released, at which
                            // point the selection is per character again."
                            if self.pointer_clicks == 2 {
                                self.snap_to_word_edges();
                            }
                            self.clamp_cursor();
                            self.after_edit();
                        }
                    }
                }
            }
            PointerKind::Release => {
                self.resizing = None;
                if self.selection.is_some() && !self.mouse_dragged && self.pointer_clicks == 1 {
                    // A press and a release with no drag between them is a
                    // click: the cursor stays, the selection goes. A double
                    // click is not that — what it selected stays selected, or
                    // the selection would end the moment the button came up.
                    self.selection = None;
                    self.mode = Mode::Normal;
                }
                self.mouse_dragged = false;
                self.after_edit();
            }
            PointerKind::ScrollUp => self.scroll_view(-MOUSE_SCROLL_LINES),
            PointerKind::ScrollDown => self.scroll_view(MOUSE_SCROLL_LINES),
        }
        self.redraw = true;
    }

    /// What a double click selects: the word under the pointer, or — when the
    /// character clicked is one `%` matches — everything up to its pair.
    ///
    /// "A double click on a word selects that word. … A double click on a
    /// character that has a match selects until that match (like using `v%`)."
    pub(super) fn select_clicked_word(&mut self) {
        let at = self.cursor.char_idx(&self.buffer);
        let row = self.cursor.row;
        // A bracket is the exception: `%` knows where its pair is. The character
        // clicked is the one asked about — a click on a word that happens to be
        // on the same line as a bracket is still a click on the word.
        if let Some((from, Some(target))) = crate::command::bracket_at(&self.buffer, row, at) {
            if from == at {
                self.selection = Some(Selection {
                    anchor: self.cursor,
                    kind: VisualKind::Charwise,
                    to_end: false,
                });
                self.cursor.set_char_idx(&self.buffer, target);
                return;
            }
        }
        // The word under the pointer, and the cursor at its end so the selection
        // is the whole word rather than from where in it the click landed.
        let Some((start, end)) = crate::command::word_object(&self.buffer, at, Object::Inner)
        else {
            self.selection = Some(Selection {
                anchor: self.cursor,
                kind: VisualKind::Charwise,
                to_end: false,
            });
            return;
        };
        let last = end.saturating_sub(1).max(start);
        let (anchor_row, anchor_col) = self.buffer.char_to_line_col(start);
        self.selection = Some(Selection {
            anchor: Cursor::new(anchor_row, anchor_col),
            kind: VisualKind::Charwise,
            to_end: false,
        });
        self.cursor.set_char_idx(&self.buffer, last);
    }

    /// What a triple click selects: the line under the pointer.
    pub(super) fn select_clicked_line(&mut self) {
        self.selection = Some(Selection {
            anchor: Cursor::new(self.cursor.row, 0),
            kind: VisualKind::Linewise,
            to_end: false,
        });
        let row = self.cursor.row;
        let end = self.buffer.line_col_to_char(row, self.buffer.line_len(row));
        self.cursor
            .set_char_idx(&self.buffer, end.saturating_sub(1));
    }

    /// What a quadruple click selects: a rectangle, which is the whole line
    /// until a drag gives it a width.
    pub(super) fn select_clicked_block(&mut self) {
        self.selection = Some(Selection {
            anchor: Cursor::new(self.cursor.row, 0),
            kind: VisualKind::Blockwise,
            to_end: false,
        });
        let row = self.cursor.row;
        let end = self.buffer.line_col_to_char(row, self.buffer.line_len(row));
        self.cursor
            .set_char_idx(&self.buffer, end.saturating_sub(1));
    }

    /// Put both ends of the selection on the near edges of the words they are
    /// in, which is what makes a drag after a double click take whole words.
    ///
    /// Both ends, because a selection is a span: a backwards drag that only
    /// moved its cursor would leave the anchor's word half in.
    pub(super) fn snap_to_word_edges(&mut self) {
        let Some(selection) = self.selection else {
            return;
        };
        let anchor_at = selection.anchor.char_idx(&self.buffer);
        let cursor_at = self.cursor.char_idx(&self.buffer);
        let backwards = cursor_at < anchor_at;
        let word = |at: usize| crate::command::word_object(&self.buffer, at, Object::Inner);
        if let Some((start, end)) = word(anchor_at) {
            let at = if backwards {
                end.saturating_sub(1).max(start)
            } else {
                start
            };
            let (row, col) = self.buffer.char_to_line_col(at);
            self.selection = Some(Selection {
                anchor: Cursor::new(row, col),
                ..selection
            });
        }
        if let Some((start, end)) = word(cursor_at) {
            // Backwards drags end at the start of the word, forwards ones at its
            // end, so the selection is whole words whichever way it is going.
            let at = if backwards {
                start
            } else {
                end.saturating_sub(1).max(start)
            };
            self.cursor.set_char_idx(&self.buffer, at);
        }
    }

    /// Put what the editor is holding on its way to the clipboard: the
    /// selection if one is up, the register otherwise.
    pub fn request_copy(&mut self) {
        self.clipboard = Some(self.text_for_clipboard());
    }

    /// What a copy would send: the selection, or what the register holds.
    ///
    /// Bytes exactly as the buffer has them, terminators included, so a linewise
    /// selection pastes as whole lines and a CRLF file copies CRLF.
    pub(crate) fn text_for_clipboard(&self) -> String {
        match self.visual_span() {
            Some((start, end)) => self.buffer.slice_text(start, end),
            None => self.register.text().to_string(),
        }
    }

    /// The buffer position a screen cell shows, if it shows text at all.
    /// The window and the position a screen cell shows. A click in a window
    /// that is not the current one has to go there first, which the caller does
    /// with the window that comes back.
    pub(super) fn position_at_screen(&self, x: u16, y: u16) -> Option<(usize, Cursor)> {
        position_at(&self.view(), x, y)
    }

    /// Move what is on screen, without moving the cursor.
    pub(super) fn scroll_view(&mut self, lines: i64) {
        let last = self.buffer.line_count().saturating_sub(1) as i64;
        self.viewport.top_line = (self.viewport.top_line as i64 + lines).clamp(0, last) as usize;
    }

    /// Keep the cursor on a position that exists in the document that is up.
    /// A window looking at another file keeps the position it had, clamped.
    pub(super) fn clamp_cursor(&mut self) {
        let last = self.buffer.line_count().saturating_sub(1);
        self.cursor.row = self.cursor.row.min(last);
        let len = self.buffer.line_len(self.cursor.row);
        self.cursor.col = self.cursor.col.min(len);
        if self.mode != Mode::Insert {
            self.clamp_normal_cursor();
        }
    }

    /// Make the window that is up bigger or smaller along `axis`.
    ///
    /// `Some(n)` sets its size to `n` rows or columns, `Some(-n)` takes `n`
    /// away, and `None` adds one — vim's `:resize`, `:resize -2` and `:resize`
    /// with nothing after it. The nearest division that runs along the axis is
    /// the one that moves; a window with no such division is the whole of its
    /// rectangle and does not move at all.
    pub(crate) fn resize_window(&mut self, axis: Axis, size: Resize) -> bool {
        let window = self.current_window;
        self.resize_window_at(window, axis, size)
    }

    /// The same, for a window named rather than the one that is up: a dragged
    /// separator moves the window on one side of it.
    pub(crate) fn resize_window_at(&mut self, window: usize, axis: Axis, size: Resize) -> bool {
        let Some(path) = layout_path(&self.layout, window) else {
            return false;
        };
        // What the window is now, for the first `:resize`: a window that has
        // never been given a size takes the size it is drawn at, so the first
        // one starts from what is on screen rather than from nothing.
        let now = self
            .window_rects()
            .get(window)
            .map(|rect| match axis {
                Axis::Rows => rect.height.saturating_sub(1),
                Axis::Columns => rect.width,
            })
            .unwrap_or(1)
            .max(1);
        let mut node = &mut self.layout;
        for step in path {
            let Layout::Split {
                axis: here,
                sizes,
                children,
            } = node
            else {
                return false;
            };
            if *here == axis {
                let slot = &mut sizes[step];
                *slot = match size {
                    Resize::One => slot.saturating_add(1),
                    Resize::Set(n) => n,
                    Resize::By(n) if n >= 0 => slot.saturating_add(n.min(u16::MAX as i32) as u16),
                    Resize::By(n) => {
                        slot.saturating_sub(n.unsigned_abs().min(u16::MAX as u32) as u16)
                    }
                };
                if *slot == 0 {
                    *slot = now;
                }
                self.redraw = true;
                return true;
            }
            node = &mut children[step];
        }
        false
    }

    /// Give every window in this tab its equal share again.
    pub(crate) fn equalize_windows(&mut self) {
        clear_sizes(&mut self.layout);
        self.redraw = true;
    }

    /// Close every window but this one.
    pub(crate) fn close_other_windows(&mut self) {
        self.save_window();
        self.save_document();
        let kept = self.windows[self.current_window].clone();
        self.windows = vec![kept];
        self.layout = Layout::Leaf(0);
        self.current_window = 0;
        self.load_document();
        self.load_window();
        self.after_switch();
    }

    /// Forget the window that is on screen, leaving a placeholder in the live
    /// fields. What was there is dropped, not stored.
    pub(super) fn drop_window(&mut self) {
        self.cursor = Cursor::default();
        self.viewport.top_line = 0;
        self.viewport.left_col = 0;
    }

    // -- rendering ----------------------------------------------------------

    /// The separator at a screen cell, if there is one, and the window on the
    /// near side of it.
    ///
    /// The answer comes from the rectangles rather than from the tree, because
    /// the rectangles are what was drawn: a vertical separator is the column
    /// just left of a window that is not the first, and a horizontal one is the
    /// last row of a window that has another below it — which is also its status
    /// line, since a window keeps its own.
    pub(crate) fn separator_at(&self, x: u16, y: u16) -> Option<Resizing> {
        let rects = self.window_rects();
        for (index, rect) in rects.iter().enumerate() {
            if rect.width == 0 || rect.height == 0 {
                continue;
            }
            // The column between two side-by-side windows.
            if rect.x > 0 && x == rect.x - 1 && y >= rect.y && y < rect.y + rect.height {
                // The window on the near side ends *at* the separator column.
                let left = rects
                    .iter()
                    .position(|other| other.x + other.width == rect.x - 1)?;
                let width = rects[left].width;
                return Some(Resizing {
                    window: left,
                    axis: Axis::Columns,
                    base: width,
                    at: x,
                });
            }
            // The row between two stacked windows: the upper one's status row.
            let bottom = rect.y + rect.height;
            if x >= rect.x
                && x < rect.x + rect.width
                && y + 1 == bottom
                && rects.iter().any(|other| other.y == bottom)
            {
                return Some(Resizing {
                    window: index,
                    axis: Axis::Rows,
                    base: rect.height.saturating_sub(1),
                    at: y,
                });
            }
        }
        None
    }

    pub(super) fn reposition(&mut self) {
        let options = self.options.borrow().clone();
        let gutter = gutter_width(self.buffer.line_count(), options.number);
        scroll_to_cursor(
            &mut self.viewport,
            self.cursor,
            &self.buffer,
            options.tabstop.max(1),
            gutter,
            options.scrolloff.0,
        );
        // A fold hides the lines after its first one, and the view cannot start on
        // a line that is not drawn: the top snaps to the nearest line that is. (The
        // scroll is worked out in lines and not in drawn rows, so `'scrolloff'` can
        // be a line or two out when folds are closed — the cursor is always on
        // screen, which is what the setting is for.)
        if self.line_is_folded(self.viewport.top_line) {
            let row = (0..self.viewport.top_line)
                .rev()
                .find(|row| !self.line_is_folded(*row));
            self.viewport.top_line = row.unwrap_or(0);
        }
    }

    /// Scroll the view a line at a time, taking the cursor with it when the
    /// screen would otherwise leave it behind.
    ///
    /// This is the one place where the screen moves on its own: `<C-e>` and
    /// `<C-y>` read *ahead* of where the cursor is, which is what a person does
    /// when they are looking rather than editing. The cursor goes with the text
    /// by the same amount — vim's rule, and the reason it stays on the same
    /// screen row — and stops `scrolloff` lines short of the edge, so the context
    /// the setting promises is kept even here. At the ends of the file the view
    /// does not move, and so neither does the cursor.
    pub(crate) fn scroll_view_by(&mut self, lines: i64) {
        let before = self.viewport.top_line;
        self.scroll_view(lines);
        let moved = self.viewport.top_line as i64 - before as i64;
        let rows = self.viewport.text_rows() as usize;
        if rows == 0 || moved == 0 {
            return;
        }
        let last = self.buffer.line_count().saturating_sub(1);
        self.cursor.row = (self.cursor.row as i64 + moved).clamp(0, last as i64) as usize;
        let margin = self
            .options
            .borrow()
            .scrolloff
            .0
            .min(rows.saturating_sub(1) / 2);
        let top = self.viewport.top_line;
        let bottom = top + rows - 1;
        if self.cursor.row < top + margin {
            self.cursor.row = (top + margin).min(bottom);
        } else if self.cursor.row + margin > bottom {
            self.cursor.row = bottom.saturating_sub(margin).max(top);
        }
    }

    /// Put the cursor on the first character of its line that is not blank.
    ///
    /// `^`, `{letter}'` and the screen motions `H`/`M`/`L` all mean this rather
    /// than column zero: a line that is indented is *about* the text, and a
    /// cursor left in the indentation is not where a person meant to be.
    pub(crate) fn move_to_first_non_blank(&mut self) {
        let row = self.cursor.row;
        let len = self.buffer.line_len(row);
        let line_start = self.buffer.line_col_to_char(row, 0);
        let col = (0..len)
            .find(|index| {
                self.buffer
                    .char_at(line_start + index)
                    .is_some_and(|c| !c.is_whitespace())
            })
            .unwrap_or(0);
        self.cursor.col = col;
    }

    /// Keep the cursor off the line terminator.
    ///
    /// Insert mode belongs *between* characters, so `col` may sit one past the
    /// last one. Normal mode does not: vim puts the cursor on the last
    /// character, which is also what makes `x`, `$` and `l` behave.
    pub(crate) fn clamp_normal_cursor(&mut self) {
        if !matches!(self.mode, Mode::Normal | Mode::Visual) {
            return;
        }
        // The row first: a command that takes lines away — `J` joins several into
        // one — leaves the cursor's row past the end until something clamps it, and
        // asking for the length of a line that is not there is a panic rather than
        // an answer.
        let last = self.buffer.line_count().saturating_sub(1);
        self.cursor.row = self.cursor.row.min(last);
        // While a *block* is being made the column is free: the cursor may sit past
        // the last character of a short line, which is what a rectangle reaching into
        // one needs. Normal mode never lets it past the last character.
        if self.is_making_a_block() {
            return;
        }
        let line_len = self.buffer.line_len(self.cursor.row);
        self.cursor.col = if line_len == 0 {
            0
        } else {
            self.cursor.col.min(line_len - 1)
        };
    }
}
