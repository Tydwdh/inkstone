use inkstone_rpc::{KeyCode, KeyMsg};

use crate::cursor::Cursor;
use crate::keymap::{encode_key, Lookup};
use crate::mode::Mode;

use super::*;

impl Editor {
    /// Type what an insert session typed, and leave insert mode.
    ///
    /// The keys that only moved the cursor around inside the session are not in
    /// the log: what `.` repeats is the text, typed where the cursor is now. A
    /// session that was a paste, or one whose text came from an input method,
    /// repeats the same way a typed one does.
    pub(crate) fn replay_insert(&mut self, log: &str) {
        if self.mode != Mode::Insert {
            self.mode = Mode::Insert;
        }
        for c in log.chars() {
            match c {
                '\u{8}' => self.delete_backward(),
                '\u{7f}' => {
                    self.run_command("delete_forwards", &[]);
                }
                _ => self.type_text(&c.to_string()),
            }
        }
        self.run_command("escape", &[]);
    }

    /// Start a blockwise insert: the same typing on every row of a rectangle.
    ///
    /// `col` is where the typing starts on each row — the block's left column for
    /// `I` and `c`, one past its right one for `A`. A row too short to reach the
    /// column is *padded* with spaces, the way the blockwise put pads: the text
    /// is meant to line up in columns, and a column past the end of a line is a
    /// place that has to exist first.
    /// `rows` are the rows the typing goes on — the caller decides which of the
    /// block's rows are in it, since that depends on the block's *shape* and not
    /// on what the buffer looks like after a deletion. `col` is where it starts on
    /// each, or `None` for the end of the line, and `pad` says whether a row that
    /// does not reach `col` is padded out to it (which is what `A` does, and what
    /// `I` and `c` do not: `v_b_I` leaves a short line unmodified).
    pub(crate) fn start_block_insert(&mut self, rows: Vec<usize>, col: Option<usize>, pad: bool) {
        let mut cursors = Vec::new();
        for row in rows {
            let len = self.buffer.line_len(row);
            // `None` is the end of each line: the typing goes after whatever the
            // line has, so there is nothing to pad.
            let col = match col {
                Some(col) => col,
                None => len,
            };
            if col > len && pad {
                let at = self.buffer.line_col_to_char(row, 0) + len;
                let fill = " ".repeat(col - len);
                self.replace_range(at, at, &fill, Some(at + fill.chars().count()));
            }
            cursors.push(Cursor::new(row, col.min(self.buffer.line_len(row))));
        }
        if cursors.is_empty() {
            return;
        }
        self.cursor = cursors[0];
        self.block_insert = Some((cursors, 0));
        self.clear_pending();
        self.mode = Mode::Insert;
    }

    /// Apply the key that was just handled to every other row of a blockwise
    /// insert, which is what makes `I` on a rectangle type on all of it.
    ///
    /// Each row keeps its own position, since the rows are different lengths and
    /// what is typed on one of them is not where the others are. A key that
    /// leaves insert mode leaves it for all of them, and the session is over.
    pub(super) fn replay_block_insert(&mut self, key: KeyMsg) {
        let Some((rows, current_index)) = self.block_insert.take() else {
            return;
        };
        if self.mode != Mode::Insert {
            return;
        }
        let typed_on = self.cursor;
        let mut updated = rows;
        for (index, position) in updated.iter_mut().enumerate() {
            if index == current_index {
                *position = typed_on;
                continue;
            }
            self.cursor = *position;
            self.handle_insert(key.clone());
            *position = self.cursor;
            if self.mode != Mode::Insert {
                // Something in the key ended the session; the rows that have not
                // been reached are simply not typed on.
                self.cursor = typed_on;
                self.redraw = true;
                return;
            }
        }
        self.cursor = typed_on;
        self.block_insert = Some((updated, current_index));
        self.redraw = true;
    }

    pub(super) fn handle_insert(&mut self, key: KeyMsg) {
        let encoded = encode_key(&key);

        // A sequence that is already part-way typed is either a mapping or
        // literal text: `jk` mapped to `<Esc>` means the `j` was not typing,
        // and `jx` means it was, after all.
        if !self.insert_pending.is_empty() {
            let sequence = format!("{} {}", self.pending_insert_keys(), encoded);
            match self.lookup_insert(&sequence) {
                Lookup::Exact(binding) => {
                    self.insert_pending.clear();
                    self.run_binding(binding, 1);
                    return;
                }
                Lookup::Prefix => {
                    self.insert_pending.push(key);
                    return;
                }
                Lookup::None => self.flush_insert_pending(),
            }
        }

        // An explicit insert-mode mapping — one key, or the start of a longer
        // one — wins over typing the character.
        match self.lookup_insert(&encoded) {
            Lookup::Exact(binding) => {
                self.run_binding(binding, 1);
                return;
            }
            Lookup::Prefix => {
                self.insert_pending.push(key);
                return;
            }
            Lookup::None => {}
        }

        if key.is_ctrl() || key.is_alt() {
            return;
        }
        match key.code {
            KeyCode::Esc => self.run_command("escape", &[]),
            KeyCode::Enter => self.type_text("\n"),
            KeyCode::Tab => self.type_text("\t"),
            KeyCode::Backspace => self.delete_backward(),
            KeyCode::Char(c) => self.type_text(&c.to_string()),
            // Everything else (arrows, Home/End/Delete, ...) is an ordinary
            // insert-mode binding and was handled by the lookup above.
            _ => {}
        }
    }

    pub(super) fn lookup_insert(&self, sequence: &str) -> Lookup {
        let keymap = self.keymap.borrow();
        keymap.lookup(Mode::Insert, sequence)
    }

    /// The keys held back so far, as the notation a binding is looked up by.
    pub(super) fn pending_insert_keys(&self) -> String {
        self.insert_pending
            .iter()
            .map(encode_key)
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Type the held-back keys after all, in the order they were pressed.
    ///
    /// They are typed the way an unmapped press is — a character is a
    /// character, and a `<BS>` deletes — rather than looked up again, since they
    /// have already been shown not to be a mapping between them.
    pub(super) fn flush_insert_pending(&mut self) {
        let keys = std::mem::take(&mut self.insert_pending);
        for key in keys {
            match key.code {
                KeyCode::Char(c) if !key.is_ctrl() && !key.is_alt() => {
                    self.type_text(&c.to_string())
                }
                KeyCode::Enter => self.type_text("\n"),
                KeyCode::Tab => self.type_text("\t"),
                KeyCode::Backspace => self.delete_backward(),
                KeyCode::Esc => self.run_command("escape", &[]),
                _ => {}
            }
        }
    }

    /// Insert `text` at the cursor, as one undoable change.
    ///
    /// This is what a character typed in insert mode does, and what a paste or
    /// an input method's commit does through `ClientMsg::Text`: the cursor ends
    /// after the text, and line endings are kept exactly as they came.
    pub fn type_text(&mut self, text: &str) {
        // A line break typed in insert mode starts the new line where the last
        // one did, when `autoindent` says so. It is part of the same edit as the
        // break, so one `u` undoes both.
        // A `}` typed where a line has nothing but indentation gives a level
        // back, which is the other half of `smartindent`.
        if text == "}" && self.mode == Mode::Insert && self.options.borrow().smartindent {
            let row = self.cursor.row;
            let indent = self.indent_of(row);
            let col = self.cursor.col;
            if col == indent.chars().count() && !indent.is_empty() {
                self.outdent_current_line();
            }
        }
        if text == "\n" && self.mode == Mode::Insert && self.indents_new_lines() {
            let indent = self.indent_for_new_line(self.cursor.row);
            let typed = format!("\n{indent}");
            let idx = self.cursor.char_idx(&self.buffer);
            let end = idx + typed.chars().count();
            self.replace_range(idx, idx, &typed, Some(end));
            self.wrap_typed_line(end);
            self.log_inserted(&typed);
            if self.completion.is_some() {
                self.complete(None);
            }
            return;
        }
        // A selected placeholder is *replaced* by what is typed: that is what
        // selecting it was for, and it is one edit rather than a delete and an
        // insert, so it is one thing to undo.
        if let Some((sel_start, sel_end)) = self.selected_range() {
            self.selection = None;
            let after = sel_start + text.chars().count();
            self.replace_range(sel_start, sel_end, text, Some(after));
            self.log_inserted(text);
            if self.completion.is_some() {
                self.complete(None);
            }
            return;
        }
        let idx = self.cursor.char_idx(&self.buffer);
        let end = idx + text.chars().count();
        self.replace_range(idx, idx, text, Some(end));
        self.wrap_typed_line(end);
        self.log_inserted(text);
        if self.completion.is_some() {
            self.complete(None);
        }
    }

    /// Note text as part of the insert session that is open, if one is.
    pub(super) fn log_inserted(&mut self, text: &str) {
        if self.mode != Mode::Insert {
            return;
        }
        if let Some(log) = self.insert_log.as_mut() {
            log.push_str(text);
        }
    }

    pub(super) fn delete_backward(&mut self) {
        let idx = self.cursor.char_idx(&self.buffer);
        if idx == 0 {
            return;
        }
        // A backspace is part of the session's text as a token of its own:
        // pressing it twice must delete twice when `.` types it all again.
        self.log_inserted("\u{8}");
        // At the start of a line, backspace joins it with the line above, and
        // the join takes the whole terminator with it -- otherwise a CRLF file
        // would keep a stray carriage return in the middle of the joined line.
        let start = if self.cursor.col == 0 && self.cursor.row > 0 {
            idx - self.buffer.line_terminator_len(self.cursor.row - 1).max(1)
        } else {
            self.buffer.prev_grapheme_boundary(idx)
        };
        self.replace_range(start, idx, "", Some(start));
    }

    /// Go back to where insert mode was last left, and insert there.
    pub(crate) fn insert_again(&mut self) -> bool {
        let Some(cursor) = self.insert_again_at else {
            return false;
        };
        self.cursor = cursor;
        self.mode = Mode::Insert;
        self.clamp_normal_cursor();
        true
    }
}
