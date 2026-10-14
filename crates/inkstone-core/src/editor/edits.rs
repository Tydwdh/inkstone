use std::path::Path;

use crate::history::Change;
use crate::mode::VisualKind;
use crate::register::{Kind, Prefix, Register};
use crate::substitute::Address;

use super::*;

impl Editor {
    /// Replace the characters in `[start, end)` with `text`, as one undoable
    /// change.
    ///
    /// The cursor ends at character index `after`, or stays where it is when
    /// `after` is `None`; either way [`Editor::after_edit`] clamps it
    /// afterwards. Every edit a command makes goes through here, which is what
    /// keeps the history and the buffer in step.
    pub(crate) fn replace_range(
        &mut self,
        start: usize,
        end: usize,
        text: &str,
        after: Option<usize>,
    ) {
        if self.list.is_some() {
            // A list that can be edited is a list that can lie about what it
            // says.
            self.message = "E21: Cannot make changes, 'readonly' option is set".to_string();
            return;
        }
        let change = Change::planned(&self.buffer, start, end, text, self.cursor);
        // A fold holds *line numbers*, and the edit is about to move the lines
        // around it: what is done here is what keeps a fold on the text it was put
        // on. Where the edit starts is read before the change, since after it the
        // index means something else; how many lines it made or unmade is read from
        // the buffer, which is the only thing that knows — typing a character into
        // the middle of a line adds no line at all.
        let fold_at = self.fold_line_for(start);
        self.record_fold_state();
        let lines_before = self.buffer.line_count();
        self.buffer.replace_range(start, end, text);
        if let Some(at) = fold_at {
            let delta = self.buffer.line_count() as isize - lines_before as isize;
            self.shift_folds(at, delta);
        }
        if let Some(index) = after {
            self.cursor.set_char_idx(&self.buffer, index);
        }
        // Every edit in the kernel comes through here, which is what makes this
        // the one place the change list has to be told about.
        self.changes.record(self.cursor);
        // And the one place a snippet's mirrors hear about an edit: what is
        // typed into a stop is what the other writings of that number say.
        self.sync_snippet_mirrors(start, end, text.chars().count());
        self.after_edit();
        // The cursor the caller asked for says where the next text goes: `o`
        // asks for the start of the line break it inserted, and the typing that
        // follows belongs in front of it.
        let caret = after.map_or(0, |index| index.saturating_sub(start));
        self.history
            .push(change.settled(self.cursor).with_caret(caret));
        // The state this edit left is the one the history has just made — or the
        // one it grew — and the folds it produced belong to it. This is *after*
        // the push because that is what says which state it is.
        self.record_fold_state();
        // A match is a pair of character indices, and the edit has just moved
        // them: the search looks again rather than pointing at the wrong text.
        self.search.refresh(&self.buffer);
        // A line's syntax state depends on the lines above it, so an edit
        // forgets the states from its own line onwards — and nothing above it,
        // which is what keeps this cheap.
        self.highlighter
            .invalidate(self.buffer.char_to_line_col(start).0);
    }

    /// Take `[start, end)` out of the buffer and into the register, as one
    /// undoable change.
    ///
    /// This is what every delete an operator makes goes through, so what `p`
    /// will put is always what the last delete took.
    pub fn cut(&mut self, start: usize, end: usize, kind: Kind, after: Option<usize>) {
        let text = self.buffer.slice_text(start, end);
        self.store(Register::new(text, kind), false);
        self.replace_range(start, end, "", after);
    }

    /// Copy `[start, end)` into the register, and leave the buffer alone.
    pub fn copy(&mut self, start: usize, end: usize, kind: Kind) {
        let text = self.buffer.slice_text(start, end);
        self.store(Register::new(text, kind), true);
    }

    /// Put `register` where the keystroke that asked for it says.
    ///
    /// The unnamed register takes everything — it is what `p` uses without being
    /// asked — unless the black hole was named, which exists precisely to take
    /// something without leaving a trace. A yank also fills `"0`; a delete
    /// shifts the numbered registers along. The clipboard register is both kept
    /// here and handed to the frontend, which is the half of it that reaches the
    /// system clipboard.
    pub(super) fn store(&mut self, register: Register, yanked: bool) {
        let name = match self.register_prefix {
            Prefix::Named(name) => Some(name),
            Prefix::None | Prefix::Awaiting => None,
        };
        // The black hole takes text without leaving a trace anywhere: not the
        // unnamed register, not the numbered history, not `"0`.
        let swallowed = name == Some('_');
        if !swallowed {
            self.register = register.clone();
            if yanked {
                self.registers.note_yank(&register);
            } else {
                self.registers.note_delete(&register);
            }
        }
        match name {
            None | Some('_') => {}
            Some('+') | Some('*') => {
                self.registers.set('+', register.clone());
                self.clipboard = Some(register.text().to_string());
            }
            Some(name) => self.registers.set(name, register),
        }
        // The register was asked for by a key that has now been used.
        self.register_prefix = Prefix::None;
    }

    /// What `p` will put: the register a `"x` named, or the unnamed one.
    pub(crate) fn chosen_register(&self) -> &Register {
        match self.register_prefix {
            Prefix::Named(name) => self.registers.get(name).unwrap_or(&self.register),
            Prefix::None | Prefix::Awaiting => &self.register,
        }
    }

    /// Take a bracketed paste as what the system clipboard holds, so `"+p` can
    /// put back what the terminal just pasted.
    ///
    /// The kernel cannot read a clipboard, so this is the only way it learns:
    /// a paste is the clipboard arriving, and it is remembered at that moment.
    /// An input method's commit comes in the same message and is remembered the
    /// same way, which is a small lie about a register nobody asked.
    pub fn note_clipboard_text(&mut self, text: &str) {
        self.registers.set_clipboard(text.to_string());
    }

    /// Undo the most recent change: put the text and the cursor back.
    ///
    /// Returns `false` when there is nothing left to undo.
    pub fn undo(&mut self) -> bool {
        match self.history.undo(&mut self.buffer) {
            Some(cursor) => {
                self.cursor = cursor;
                self.restore_folds();
                self.after_edit();
                // Undo moves the cursor to the change it put back, so the
                // states to forget are the ones from that line onwards.
                self.highlighter.invalidate(self.cursor.row);
                true
            }
            None => false,
        }
    }

    /// Redo the change that was undone last.
    ///
    /// Returns `false` when there is nothing to redo.
    pub fn redo(&mut self) -> bool {
        match self.history.redo(&mut self.buffer) {
            Some(cursor) => {
                self.cursor = cursor;
                self.restore_folds();
                self.after_edit();
                self.highlighter.invalidate(self.cursor.row);
                true
            }
            None => false,
        }
    }

    /// Go to the state `count` older in the order the states were *made* —
    /// `g-`, and `:earlier {count}`.
    ///
    /// Not the same as [`Editor::undo`]: `u` walks the branch the text is on, and
    /// this walks time. Undo twice, edit, and `u` takes the edit back while `g-`
    /// goes to what was undone.
    pub fn goto_older(&mut self, count: usize) -> bool {
        match self.history.earlier(count) {
            Some(target) => self.goto_state(target),
            None => {
                self.message = "E831: Cannot go back that far".to_string();
                false
            }
        }
    }

    /// Go to the state `count` newer in the order the states were made — `g+`,
    /// and `:later {count}`.
    pub fn goto_newer(&mut self, count: usize) -> bool {
        match self.history.later(count) {
            Some(target) => self.goto_state(target),
            None => {
                self.message = "E831: Cannot go forward that far".to_string();
                false
            }
        }
    }

    /// Move the text to the state at `target`, whichever branch it is on.
    fn goto_state(&mut self, target: usize) -> bool {
        match self.history.goto(target, &mut self.buffer) {
            Some(cursor) => {
                self.cursor = cursor;
                self.restore_folds();
                self.after_edit();
                self.highlighter.invalidate(self.cursor.row);
                true
            }
            None => false,
        }
    }

    /// What `:undolist` shows: the states nothing was made from, with how deep
    /// each is and how long ago it was made.
    pub fn undo_list_text(&self) -> String {
        let leaves = self.history.leaves();
        if leaves.is_empty() {
            return "no changes".to_string();
        }
        let mut parts = vec![format!("{} leaves", leaves.len())];
        for (index, age) in leaves.iter().take(8) {
            parts.push(format!(
                "{} changes {}",
                self.history.depth_of(*index),
                ago(*age)
            ));
        }
        if leaves.len() > 8 {
            parts.push(format!("... {} more", leaves.len() - 8));
        }
        parts.join("   ")
    }

    // -- helpers used by commands ------------------------------------------

    /// Call after anything that may have moved the cursor or changed the
    /// buffer: it re-clamps the cursor, scrolls it back into view and marks the
    /// screen dirty.
    pub(crate) fn after_edit(&mut self) {
        let came_from = self.last_cursor_row;
        // The row is always clamped; the column is not while a block is being made,
        // since a rectangle may reach past the end of a short line and `Cursor::clamp`
        // would pull it back to the last character of one.
        self.cursor.row = self
            .cursor
            .row
            .min(self.buffer.line_count().saturating_sub(1));
        if !self.is_making_a_block() {
            self.cursor.col = self.cursor.col.min(self.buffer.line_len(self.cursor.row));
        }
        self.clamp_normal_cursor();
        // A closed fold hides the lines after its first, and a cursor inside one has
        // nowhere to be: every edit settles it, which is what keeps `j` from walking
        // into a fold it cannot see.
        self.settle_cursor_from(came_from);
        self.last_cursor_row = self.cursor.row;
        self.reposition();
        self.redraw = true;
    }

    /// Change the case of a range, in place.
    ///
    /// The register is untouched, and the cursor goes to the start of the range,
    /// which is where an operator leaves it.
    pub(crate) fn change_case(
        &mut self,
        start: usize,
        end: usize,
        change: crate::command::CaseChange,
    ) {
        if start >= end {
            return;
        }
        let text: String = self
            .buffer
            .slice_text(start, end)
            .chars()
            .map(|c| match change {
                crate::command::CaseChange::Toggle if c.is_uppercase() => {
                    c.to_lowercase().next().unwrap_or(c)
                }
                crate::command::CaseChange::Toggle if c.is_lowercase() => {
                    c.to_uppercase().next().unwrap_or(c)
                }
                crate::command::CaseChange::Lower => c.to_lowercase().next().unwrap_or(c),
                crate::command::CaseChange::Upper => c.to_uppercase().next().unwrap_or(c),
                crate::command::CaseChange::Toggle => c,
            })
            .collect();
        self.replace_range(start, end, &text, Some(start));
        self.cursor.set_char_idx(&self.buffer, start);
    }

    /// The indentation a new line should start with, after `row`.
    ///
    /// `autoindent` copies the line; `smartindent` copies it and adds a level
    /// when the line opens something — `{`, `(`, `[` — since the line after an
    /// opener is inside it. The level is a tab, which is what the Tab key types,
    /// so the file keeps one convention rather than two.
    pub(crate) fn indent_for_new_line(&self, row: usize) -> String {
        let options = self.options.borrow();
        let smart = options.smartindent;
        if !options.autoindent && !smart {
            return String::new();
        }
        drop(options);
        let mut indent = self.indent_of(row);
        if smart && self.line_opens_something(row) {
            indent.push('\t');
        }
        indent
    }

    /// Whether a line ends by opening something, which is what the line after it
    /// is inside of.
    pub(super) fn line_opens_something(&self, row: usize) -> bool {
        self.buffer
            .line_string(row)
            .trim_end()
            .chars()
            .next_back()
            .is_some_and(|c| c == '{' || c == '(' || c == '[')
    }

    /// One level of indentation, taken off the front of the line the cursor is
    /// on — what typing a `}` at the start of a line does.
    pub(super) fn outdent_current_line(&mut self) {
        let row = self.cursor.row;
        let indent = self.indent_of(row);
        if indent.is_empty() {
            return;
        }
        let start = self.buffer.line_col_to_char(row, 0);
        let mut end = start;
        while end < self.buffer.len_chars()
            && matches!(self.buffer.char_at(end), Some(' ') | Some('\t'))
        {
            end += 1;
        }
        let level = if indent.ends_with('\t') {
            end - 1
        } else {
            // Spaces: one tab stop's worth, or all of them if there are fewer.
            let stop = self.options.borrow().tabstop;
            (start + stop).min(end)
        };
        let at = self.cursor.char_idx(&self.buffer);
        self.replace_range(level, end, "", Some(at.saturating_sub(end - level)));
    }

    /// Move a range of lines one level in or out, as one edit.
    ///
    /// A level is a tab, since that is what the Tab key types; taking one *off*
    /// removes a tab if the line starts with one, and otherwise as many spaces as
    /// a tab stop is worth — or all of them, when there are fewer, so a line that
    /// is barely indented is not left with a level's worth of nothing. A blank
    /// line is left blank: indenting nothing is not indentation.
    pub(crate) fn shift_indent(&mut self, from: usize, to: usize, forward: bool) -> usize {
        let last = self.buffer.line_count().saturating_sub(1);
        let (from, to) = (from.min(last), to.min(last));
        let (from, to) = (from.min(to), from.max(to));
        let stop = self.options.borrow().tabstop;
        let mut text = String::new();
        let mut count = 0;
        for row in from..=to {
            let line = self.buffer.line_string(row);
            if line.trim().is_empty() {
                text.push_str(&self.buffer.line_terminator(row));
                count += 1;
                continue;
            }
            if forward {
                text.push('\t');
                text.push_str(&line);
            } else {
                let indent = self.indent_of(row);
                let remove = if indent.ends_with('\t') {
                    1
                } else {
                    stop.min(indent.chars().count())
                };

                text.push_str(&line.chars().skip(remove).collect::<String>());
            }
            text.push_str(&self.buffer.line_terminator(row));
            count += 1;
        }
        let start = self.buffer.line_col_to_char(from, 0);
        let end = self.buffer.line_col_to_char(to, 0)
            + self.buffer.line_len(to)
            + self.buffer.line_terminator_len(to);
        let at = self.cursor.char_idx(&self.buffer);
        let caret = at.saturating_sub(if forward { 0 } else { at - start });
        self.replace_range(start, end, &text, Some(caret.max(start)));
        count
    }

    /// Apply a list of edits to the document, as one step to undo.
    ///
    /// The edits are given in the *original* text's character offsets — which is
    /// what a server's answer means — so each one is moved by what the ones before
    /// it changed: they are applied in document order, with a running difference,
    /// which is also the order the history records them in. Applying them
    /// backwards would need no arithmetic and would record the steps backwards,
    /// and a group of steps that runs backwards is a group that cannot be folded
    /// into one — which is what makes one `u` take the whole answer back.
    pub fn apply_edits(&mut self, edits: &[(usize, usize, String)]) -> usize {
        if edits.is_empty() {
            return 0;
        }
        let mut ordered: Vec<&(usize, usize, String)> = edits.iter().collect();
        ordered.sort_by_key(|(start, _, _)| *start);
        let len = self.buffer.len_chars();
        self.history.begin_group();
        let mut applied = 0;
        let mut delta = 0isize;
        for (start, end, text) in ordered {
            let (start, end) = ((*start).min(len), (*end).min(len));
            if start > end {
                continue;
            }
            let at = (start as isize + delta).max(0) as usize;
            let to = (end as isize + delta).max(0) as usize;
            self.replace_range(at, to, text, None);
            delta += text.chars().count() as isize - (end - start) as isize;
            applied += 1;
        }
        self.history.end_group(&mut self.buffer);
        self.cursor
            .set_char_idx(&self.buffer, self.cursor.char_idx(&self.buffer).min(len));
        self.clamp_normal_cursor();
        applied
    }

    /// Reindent a range by what its lines open and close, as one edit.
    ///
    /// The depth is counted from the beginning of the *document*, not from the
    /// start of the range: a block in the middle of a file is inside everything
    /// above it, and reindenting it should not forget that. Brackets are counted
    /// as they are written, without knowing what is a string and what is code —
    /// the highlighter knows, and this deliberately does not ask it, because a
    /// reindent that is wrong about a brace inside a string is a reindent a person
    /// can fix by hand.
    pub(crate) fn reindent_lines(&mut self, from: usize, to: usize) -> usize {
        let last = self.buffer.line_count().saturating_sub(1);
        let (from, to) = (from.min(last), to.min(last));
        let (from, to) = (from.min(to), from.max(to));
        // The depth the first line starts at.
        let mut depth = self.depth_before(from);
        let mut text = String::new();
        let mut count = 0;
        for row in from..=to {
            let line = self.buffer.line_string(row);
            let trimmed = line.trim_start();
            // A line that closes something is written one level out from the
            // line before it, which is what makes `}` line up with its `{`.
            let closes =
                trimmed.starts_with('}') || trimmed.starts_with(')') || trimmed.starts_with(']');
            let level = if closes {
                depth.saturating_sub(1)
            } else {
                depth
            };
            if trimmed.is_empty() {
                // An empty line stays empty: trailing whitespace is not
                // indentation.
                text.push_str(&self.buffer.line_terminator(row));
            } else {
                text.push_str(&"\t".repeat(level));
                text.push_str(trimmed);
                text.push_str(&self.buffer.line_terminator(row));
            }
            depth = level + self.line_delta(&line);
            count += 1;
        }
        let start = self.buffer.line_col_to_char(from, 0);
        let end = self.buffer.line_col_to_char(to, 0)
            + self.buffer.line_len(to)
            + self.buffer.line_terminator_len(to);
        self.replace_range(start, end, &text, Some(start));
        count
    }

    /// How deep the brackets are before a line, counting from the top of the
    /// document.
    pub(super) fn depth_before(&self, row: usize) -> usize {
        let mut depth = 0usize;
        for earlier in 0..row {
            depth = depth
                .saturating_add_signed(self.line_delta_signed(&self.buffer.line_string(earlier)));
        }
        depth
    }

    /// How much one line opens or closes: `{` is one deeper, `}` one shallower.
    pub(super) fn line_delta(&self, line: &str) -> usize {
        self.line_delta_signed(line).max(0) as usize
    }

    pub(super) fn line_delta_signed(&self, line: &str) -> isize {
        let mut delta = 0isize;
        for c in line.chars() {
            match c {
                '{' | '(' | '[' => delta += 1,
                '}' | ')' | ']' => delta -= 1,
                _ => {}
            }
        }
        delta
    }

    /// The indentation of a line: its leading spaces and tabs.
    pub(crate) fn indent_of(&self, row: usize) -> String {
        let line = self.buffer.line_string(row);
        line.chars()
            .take_while(|c| *c == ' ' || *c == '\t')
            .collect()
    }

    /// Whether a new line is indented at all: `autoindent` or `smartindent`.
    pub(super) fn indents_new_lines(&self) -> bool {
        let options = self.options.borrow();
        options.autoindent || options.smartindent
    }

    /// Break the line that was just typed into, if it has grown past
    /// `textwidth`.
    ///
    /// This is what makes `textwidth` a setting rather than a command: a line
    /// that reaches past the width is broken at the last space before it, which
    /// is the space the typist just passed. A word longer than the width is not
    /// broken — there is no space to break at — and the new line is not indented,
    /// since indentation is a thing this editor does not do while typing.
    pub(super) fn wrap_typed_line(&mut self, at: usize) {
        let width = self.options.borrow().textwidth.0;
        if width == 0 {
            return;
        }
        let (row, col) = self.buffer.char_to_line_col(at);
        if self.buffer.line_len(row) <= width {
            return;
        }
        let line_start = self.buffer.line_col_to_char(row, 0);
        let len = self.buffer.line_len(row);
        let _ = col;
        // The last space *before the width*, not before the cursor: a break
        // there does not depend on when the line was noticed, which is what
        // makes the result the same whether a character or a paste arrived.
        let space = (0..len.min(width)).rev().find(|index| {
            self.buffer
                .char_at(line_start + index)
                .is_some_and(|c| c == ' ')
        });
        let Some(space) = space else {
            return;
        };
        let break_at = line_start + space;
        // The cursor does not move: the break takes the place of a space the
        // typist has already passed, so what comes next still belongs where it
        // did — at the end of the text, which is now the end of the next line.
        self.replace_range(break_at, break_at + 1, "\n", Some(at));
    }

    /// The range a selection covers, in characters, if one is up.
    pub(super) fn selected_range(&self) -> Option<(usize, usize)> {
        let selection = self.selection?;
        let anchor = selection.anchor.char_idx(&self.buffer);
        let cursor = self.cursor.char_idx(&self.buffer);
        let (start, end) = if anchor <= cursor {
            (anchor, cursor + 1)
        } else {
            (cursor, anchor + 1)
        };
        (start < end).then_some((start, end))
    }

    /// Select the text of a stop, so that typing into it replaces what is there.
    pub(super) fn select_stop(&mut self, stop: usize) {
        let Some(span) = self.snippet_stops.get(stop).map(|stop| stop.span) else {
            return;
        };
        let (start, end) = span;
        let (start, end) = (
            start.min(self.buffer.len_chars()),
            end.min(self.buffer.len_chars()),
        );
        if end <= start {
            // A place with nothing in it is a place to type, not to replace.
            self.selection = None;
            self.cursor.set_char_idx(&self.buffer, start);
            return;
        }
        let mut anchor = self.cursor;
        anchor.set_char_idx(&self.buffer, start);
        let mut cursor = self.cursor;
        cursor.set_char_idx(&self.buffer, end - 1);
        self.selection = Some(Selection {
            anchor,
            kind: VisualKind::Charwise,
            to_end: false,
        });
        self.cursor = cursor;
    }

    /// Join the lines `from..=to` into one, and say how many joins were made.
    ///
    /// Vim's `J`: each line is joined to the next by taking the terminator and the
    /// next line's leading whitespace out and putting a space in their place — so
    /// `one` and `   two` become `one two`. The whole range is one edit, which is
    /// what makes one `u` take it back.
    ///
    /// The separator is one space, or two after a full stop, a question mark or an
    /// exclamation mark when `'joinspaces'` is on — vim's rule, and the reason a
    /// joined pair of sentences keeps its double space.
    ///
    /// The clause about a bracket or a quote between the stop and the space (vim
    /// allows `.)` and `."`) is not done: the line's *last* character is what is
    /// looked at.
    pub(crate) fn join_lines(&mut self, from: usize, to: usize) -> usize {
        self.join_lines_with(from, to, false)
    }

    /// The same, with `tight` set: `gJ`, which inserts and removes no spaces at
    /// all — the terminators go and the next line follows exactly as it was, indent
    /// and all.
    pub(crate) fn join_lines_tight(&mut self, from: usize, to: usize) -> usize {
        self.join_lines_with(from, to, true)
    }

    pub(super) fn join_lines_with(&mut self, from: usize, to: usize, tight: bool) -> usize {
        let last = self.buffer.line_count().saturating_sub(1);
        let (from, to) = (from.min(last), to.min(last));
        if to <= from {
            return 0;
        }
        // The range runs from the end of the first line to the end of the *last*
        // one's text: everything between them is a terminator or an indent, and the
        // lines themselves go back in the order they were in — each joined to the
        // one before it by a space, with its own indent gone. The last line's
        // terminator is outside the range, so it stays where it was.
        let start = self
            .buffer
            .line_col_to_char(from, self.buffer.line_len(from));
        let end = self.buffer.line_col_to_char(to, self.buffer.line_len(to));
        let joinspaces = self.options.borrow().joinspaces;
        let mut text = String::new();
        for row in from + 1..=to {
            // Two spaces after the end of a sentence, when the option says so. `gJ`
            // puts nothing in and takes nothing out.
            let ends_a_sentence = self
                .buffer
                .line_string(row - 1)
                .trim_end()
                .ends_with(['.', '?', '!']);
            if !tight {
                text.push_str(if joinspaces && ends_a_sentence {
                    "  "
                } else {
                    " "
                });
            }
            let next = self.buffer.line_string(row);
            text.push_str(if tight {
                next.as_str()
            } else {
                next.trim_start()
            });
        }
        let joined = to - from;
        let after = start + text.chars().count();
        self.replace_range(start, end, &text, Some(after.saturating_sub(1)));
        joined
    }

    /// Write the lines an address range covers to a file, and say how many there
    /// were.
    ///
    /// This does not mark the buffer saved: what went to disk is part of it, and
    /// the rest is still unsaved — which is the difference between `:w` and
    /// `:1,5w somewhere`.
    /// Add the lines of an address range to a file, and say how many there were.
    ///
    /// The difference from [`Editor::write_range`] is one call: a file that is
    /// opened to be appended to keeps what it had, and one that is written is
    /// replaced. Both walk the same lines, which is why the text of a range is
    /// built in one place.
    pub(crate) fn append_range(
        &self,
        from: usize,
        to: usize,
        path: &Path,
    ) -> Result<usize, BufferError> {
        let last = self.buffer.line_count().saturating_sub(1);
        let (from, to) = (from.min(last), to.min(last));
        let (from, to) = (from.min(to), from.max(to));
        let mut text = String::new();
        for row in from..=to {
            text.push_str(&self.buffer.line_string(row));
            text.push_str(&self.buffer.line_terminator(row));
        }
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        file.write_all(text.as_bytes())?;
        Ok(to - from + 1)
    }

    /// The lines of a range, terminators and all — what `:w` writes out.
    pub(super) fn range_text(&self, from: usize, to: usize) -> String {
        let mut text = String::new();
        for row in from..=to {
            text.push_str(&self.buffer.line_string(row));
            text.push_str(&self.buffer.line_terminator(row));
        }
        text
    }

    /// Write the lines of an address range to a file, and say how many there
    /// were.
    ///
    /// This does not mark the buffer saved: what went to disk is part of it, and
    /// the rest is still unsaved — which is the difference between `:w` and
    /// `:1,5w somewhere`.
    pub(crate) fn write_range(
        &self,
        from: usize,
        to: usize,
        path: &Path,
    ) -> Result<usize, BufferError> {
        let last = self.buffer.line_count().saturating_sub(1);
        let (from, to) = (from.min(last), to.min(last));
        let (from, to) = (from.min(to), from.max(to));
        std::fs::write(path, self.range_text(from, to))?;
        Ok(to - from + 1)
    }

    /// The mark a `:` line's range names that was never set, if there is one.
    ///
    /// Vim stops a command whose range names a mark that is not there rather
    /// than acting on a line nobody asked for, and so does this: the message is
    /// the same one the mark keys give.
    pub(crate) fn unset_range_mark(&self, line: &str) -> Option<char> {
        let (from, to) = crate::substitute::address_range(line);
        [from, to]
            .into_iter()
            .flatten()
            .find_map(|address| match address {
                Address::Mark(letter) if self.marks.get(letter).is_none() => Some(letter),
                _ => None,
            })
    }

    /// Remember where a jump started, so `<C-o>` and `''` can come back to it.
    ///
    /// Only the keys that travel call this: a motion within a line is not a jump,
    /// and neither is a scroll.
    pub(crate) fn record_jump(&mut self) {
        self.jumps.record(self.cursor);
        // `''` is not a walk through the list: it is the one place the last jump
        // started, so that pressing it twice comes back.
        self.jump_from = Some(self.cursor);
    }
}

/// How long ago, in words: `:undolist` says "3 seconds ago" for anything
/// recent, and the minutes and hours after that.
fn ago(age: std::time::Duration) -> String {
    let seconds = age.as_secs();
    match seconds {
        0..=1 => "now".to_string(),
        2..=59 => format!("{seconds} seconds ago"),
        60..=3599 => format!("{} minutes ago", seconds / 60),
        _ => format!("{} hours ago", seconds / 3600),
    }
}
