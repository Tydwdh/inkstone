use std::path::PathBuf;

use crate::mode::{Mode, VisualKind};

use super::*;

impl Editor {
    /// Fold the lines `from..=to`, and close the fold — which is what `zf` does.
    ///
    /// A fold of one line is not a fold: there is nothing to hide. Folds are kept
    /// sorted by where they start, which is what lets the drawing walk them in
    /// order.
    pub fn fold_lines(&mut self, from: usize, to: usize) -> bool {
        let last = self.buffer.line_count().saturating_sub(1);
        let (start, end) = (from.min(last), to.min(last));
        if end <= start {
            // A fold of one line hides nothing, so there is nothing to make. (Vim
            // beeps; this says why, without borrowing an error number it does not
            // mean.)
            self.message = "cannot fold one line".to_string();
            return false;
        }
        // Folds nest: a fold inside another is a fold of its own, which is what
        // vim means by a fold's *level* being its nesting. The one thing that is
        // replaced is a fold over exactly the same lines — `zf` twice is one fold.
        self.folds
            .retain(|fold| !(fold.start == start && fold.end == end));
        self.folds.push(Fold {
            start,
            end,
            closed: true,
        });
        self.folds.sort_by_key(|fold| fold.start);
        // The cursor goes to the fold's first line, which is the one line of it
        // that is still on screen.
        self.cursor.row = start;
        true
    }

    /// Move the cursor to the start of the next fold, or the `count`th one — `zj`.
    ///
    /// Every fold counts, open or closed: `zj` is "go to the next fold", and a fold
    /// that is open is still a fold. (The first attempt filtered on `closed`, on the
    /// reasoning that an open fold is not in the way — the test's trace showed what
    /// that costs: with the only fold before the cursor open, `zk` said there was no
    /// fold at all.)
    pub fn goto_next_fold(&mut self, count: usize) -> bool {
        let row = self.cursor.row;
        let mut target = None;
        for _ in 0..count.max(1) {
            let next = self
                .folds
                .iter()
                .filter(|fold| fold.start > target.unwrap_or(row))
                .map(|fold| fold.start)
                .min();
            match next {
                Some(start) => target = Some(start),
                None => break,
            }
        }
        match target {
            Some(row) => {
                self.cursor.row = row;
                self.cursor.col = 0;
                true
            }
            None => {
                self.message = "E490: No fold found".to_string();
                false
            }
        }
    }

    /// The same backwards — `zk`.
    pub fn goto_previous_fold(&mut self, count: usize) -> bool {
        let row = self.cursor.row;
        let mut target = None;
        for _ in 0..count.max(1) {
            let previous = self
                .folds
                .iter()
                .filter(|fold| fold.start < target.unwrap_or(row))
                .map(|fold| fold.start)
                .max();
            match previous {
                Some(start) => target = Some(start),
                None => break,
            }
        }
        match target {
            Some(row) => {
                self.cursor.row = row;
                self.cursor.col = 0;
                true
            }
            None => {
                self.message = "E490: No fold found".to_string();
                false
            }
        }
    }

    /// Whether a blockwise selection is being made, which is the one place the
    /// cursor may go past the end of a line.
    pub fn is_making_a_block(&self) -> bool {
        self.mode == Mode::Visual
            && self
                .selection
                .is_some_and(|selection| selection.kind == VisualKind::Blockwise)
    }

    /// The file a view of this document is kept in.
    ///
    /// Vim keeps views in `~/.vim/view` under a mangled name; this keeps them in the
    /// config directory under the file's own name and a hash of its full path, so
    /// that two files called `main.rs` in different projects do not share one.
    pub fn view_path(&self) -> Option<PathBuf> {
        let name = self.buffer.path()?;
        let dir = match &self.view_dir {
            Some(dir) => dir.clone(),
            None => directories::ProjectDirs::from("", "", "inkstone")?
                .config_dir()
                .join("views"),
        };
        // FNV-1a over the path's bytes: a small hash with no dependency, and enough
        // to keep two same-named files apart.
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in name.to_string_lossy().as_bytes() {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        let stem = name
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "unnamed".to_string());
        Some(dir.join(format!("{stem}-{hash:016x}")))
    }

    /// Write this window's view — its folds and where the cursor is — to the file
    /// `:loadview` reads.
    pub fn make_view(&mut self) -> std::io::Result<()> {
        let Some(path) = self.view_path() else {
            self.message = "E474: No file name, so nowhere to keep a view".to_string();
            return Ok(());
        };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut text = format!(
            "cursor {} {}\ntop {}\n",
            self.cursor.row, self.cursor.col, self.viewport.top_line
        );
        for fold in &self.folds {
            text.push_str(&format!(
                "fold {} {} {}\n",
                fold.start,
                fold.end,
                if fold.closed { "closed" } else { "open" }
            ));
        }
        std::fs::write(&path, text)?;
        self.message = format!("view written: {}", path.display());
        Ok(())
    }

    /// Read a view back: the folds and the position it had.
    pub fn load_view(&mut self) -> std::io::Result<()> {
        let Some(path) = self.view_path() else {
            self.message = "E474: No file name, so nowhere to look for a view".to_string();
            return Ok(());
        };
        let Ok(text) = std::fs::read_to_string(&path) else {
            self.message = format!("no view for {}", self.buffer.display_name());
            return Ok(());
        };
        let last = self.buffer.line_count().saturating_sub(1);
        self.folds.clear();
        for line in text.lines() {
            let mut parts = line.split_whitespace();
            match parts.next() {
                Some("fold") => {
                    let start = parts.next().and_then(|n| n.parse::<usize>().ok());
                    let end = parts.next().and_then(|n| n.parse::<usize>().ok());
                    let closed = parts.next() == Some("closed");
                    if let (Some(start), Some(end)) = (start, end) {
                        // A view made before the file changed can name lines that are
                        // not there any more: the ones that are not are dropped.
                        if start < end && end <= last {
                            self.folds.push(Fold { start, end, closed });
                        }
                    }
                }
                Some("cursor") => {
                    let row = parts.next().and_then(|n| n.parse::<usize>().ok());
                    let col = parts.next().and_then(|n| n.parse::<usize>().ok());
                    if let (Some(row), Some(col)) = (row, col) {
                        self.cursor.row = row.min(last);
                        self.cursor.col = col;
                    }
                }
                Some("top") => {
                    if let Some(top) = parts.next().and_then(|n| n.parse::<usize>().ok()) {
                        self.viewport.top_line = top.min(last);
                    }
                }
                _ => {}
            }
        }
        self.folds.sort_by_key(|fold| fold.start);
        self.settle_cursor();
        self.after_edit();
        self.message = format!("{} fold(s) restored", self.folds.len());
        Ok(())
    }

    /// Work the folds out from the indent of the lines — `'foldmethod'` is `indent`.
    ///
    /// Vim's rule, in its own words: the fold level of a line is its indent divided by
    /// `'shiftwidth'`, rounded down (this editor indents by `'tabstop'`, so that is
    /// what divides); a line indented further than the one before it starts a fold,
    /// which ends at the last line that is at least as far in; and a blank line gets
    /// the *lower* of the levels around it, so it belongs to the fold that encloses
    /// it rather than starting one of its own.
    ///
    /// The folds are worked out when the method is set and by `zx`, not after every
    /// keystroke: a file's indentation does not usually change shape while it is
    /// being edited, and the alternative is walking the whole buffer per character.
    /// (The *line numbers* of existing folds are kept up to date by every edit, so a
    /// fold made this way does not drift.)
    pub fn compute_indent_folds(&mut self) -> usize {
        let options = self.options.borrow().clone();
        let tabstop = options.tabstop.max(1);
        let ignore: Vec<char> = options.foldignore.chars().collect();
        let nestmax = options.foldnestmax.max(1);
        let lines = self.buffer.line_count();
        // A line's indent, or `None` when it is *ignored*: blank, or starting with a
        // character in `'foldignore'` once its own white space is skipped — which is
        // what `#` is for in C, where a preprocessor line is not indented like the
        // code inside it.
        let indent_of = |row: usize| -> Option<usize> {
            let text = self.buffer.line_string(row);
            let indent = text.chars().take_while(|c| c.is_whitespace()).count();
            match text.chars().nth(indent) {
                None => None,
                Some(first) if ignore.contains(&first) => None,
                Some(_) => Some(indent),
            }
        };
        let mut levels = Vec::with_capacity(lines);
        for row in 0..lines {
            match indent_of(row) {
                Some(indent) => levels.push(indent / tabstop),
                // A blank line takes the lower of the levels around it, which is what
                // keeps a blank line inside the fold it is written in.
                None => {
                    let above = (0..row).rev().find_map(&indent_of).map(|i| i / tabstop);
                    let below = (row + 1..lines).find_map(&indent_of).map(|i| i / tabstop);
                    levels.push(match (above, below) {
                        (Some(a), Some(b)) => a.min(b),
                        (Some(a), None) => a,
                        (None, Some(b)) => b,
                        (None, None) => 0,
                    });
                }
            }
        }
        self.folds.clear();
        for row in 0..lines {
            let level = levels[row];
            if level == 0 || (row > 0 && levels[row - 1] >= level) {
                continue;
            }
            // `'foldnestmax'`: a fold deeper than that is not made. The level of a
            // fold *is* its nesting, so the level is what is compared.
            if level > nestmax {
                continue;
            }
            // The fold runs to the last line that is at least as far in.
            let end = (row + 1..lines)
                .take_while(|r| levels[*r] >= level)
                .last()
                .unwrap_or(row);
            if end > row {
                self.folds.push(Fold {
                    start: row,
                    end,
                    // Not closed *by hand*: with `indent` folding what is shown is
                    // decided by `'foldlevel'`, which `zm` and `zr` move. (A fold a
                    // person made with `zf` is closed by hand, and stays closed
                    // whatever the level says.)
                    closed: false,
                });
            }
        }
        self.folds.sort_by_key(|fold| fold.start);
        self.settle_cursor();
        self.folds.len()
    }

    /// Whether the folds are worked out from the indent, which is what `zx` asks
    /// about and what `:set foldmethod=indent` turns on.
    pub fn foldmethod_is_indent(&self) -> bool {
        self.options.borrow().foldmethod == crate::settings::FoldMethod::Indent
    }

    /// The innermost fold the cursor's line is inside, if it is inside one.
    ///
    /// Without levels the innermost is the smallest range, and a fold whose first
    /// line is the cursor's counts: that line is the one that is drawn.
    pub fn fold_at_cursor(&self) -> Option<usize> {
        let row = self.cursor.row;
        self.folds
            .iter()
            .enumerate()
            .filter(|(_, fold)| row >= fold.start && row <= fold.end)
            .min_by_key(|(_, fold)| fold.end - fold.start)
            .map(|(index, _)| index)
    }

    /// Open or close the fold the cursor is in — `zo` and `zc`.
    pub fn set_fold_closed(&mut self, closed: bool) -> bool {
        let Some(index) = self.fold_at_cursor() else {
            self.message = "E490: No fold found".to_string();
            return false;
        };
        self.set_fold_closed_at(index, closed);
        true
    }

    /// Open or close one fold, by index.
    pub(super) fn set_fold_closed_at(&mut self, index: usize, closed: bool) {
        self.folds[index].closed = closed;
        // A fold nested deeper than `'foldlevel'` stays shut whatever this says, so
        // opening one has to move the level out of its way — which is what makes `zo`
        // open the fold a person is looking at.
        if !closed && self.fold_depth(index) > self.options.borrow().foldlevel.0 {
            let depth = self.fold_depth(index);
            self.options.borrow_mut().foldlevel = crate::settings::Count(depth);
        }
        if closed {
            self.cursor.row = self.folds[index].start;
        }
        self.after_edit();
    }

    /// Open or close every fold — `zR` and `zM`.
    pub fn set_all_folds_closed(&mut self, closed: bool) -> bool {
        if self.folds.is_empty() {
            self.message = "E490: No fold found".to_string();
            return false;
        }
        for fold in &mut self.folds {
            fold.closed = closed;
        }
        // `zR` and `zM` are about *all* of them, so they move the level out of the
        // way as well: a fold nested deeper than `'foldlevel'` is drawn closed
        // whatever its own flag says, and `zR` has to open those too.
        self.options.borrow_mut().foldlevel =
            crate::settings::Count(if closed { 0 } else { self.folds.len() + 1 });
        if closed {
            self.settle_cursor();
        }
        self.after_edit();
        true
    }

    /// Take away the fold the cursor is in — `zd`.
    pub fn delete_fold_at_cursor(&mut self) -> bool {
        let Some(index) = self.fold_at_cursor() else {
            self.message = "E490: No fold found".to_string();
            return false;
        };
        self.folds.remove(index);
        true
    }

    /// Remember the folds as they are *before* an edit, at the place in the history
    /// that edit is about to take.
    ///
    /// `fold_states[i]` is the state *after* `i` steps, so the stack reads the way the
    /// history does: whatever `steps` the history lands on, the entry at that index is
    /// the folds that belong to it. Each edit writes two entries — the state it found
    /// and the state it leaves — and the truncate drops the ones an undone step can no
    /// longer reach.
    pub(super) fn record_fold_state(&mut self) {
        let steps = self.history.steps();
        self.fold_states.truncate(steps);
        self.fold_states.push(self.folds.clone());
    }

    /// Put the folds back to what they were at the step the history has landed on.
    ///
    /// Every entry is written by the *edit* that follows it — `fold_states[i]` is the
    /// folds before step `i + 1`, which is also the state step `i` left — so an undo
    /// and a redo have nothing to write: they only have to read the entry for the step
    /// they land on. What they must *not* do is leave entries for steps that can no
    /// longer be reached, which is what the truncate is for.
    pub(super) fn restore_folds(&mut self) {
        let steps = self.history.steps();
        // One step *ahead* is kept as well: a redo goes there, and the entry for it is
        // the state the step being undone left. An edit made after an undo drops it
        // again — its own truncate is `steps + 1` — which is what keeps an undone step
        // from being redone to a stale fold.
        self.fold_states.truncate(steps + 2);
        if let Some(folds) = self.fold_states.get(steps).cloned() {
            self.folds = folds;
        }
    }

    /// A `:s` line with its range adjusted to any closed fold it touches.
    ///
    /// The line is rebuilt with the range spelled out in front of it, which is where a
    /// substitute reads its range from — the one the dispatcher resolved is not passed
    /// on. A line whose range does not change is left exactly as it was written, so
    /// `:%s/…/` stays `%`.
    pub(super) fn substitute_line_with_fold_range(&self, line: &str) -> String {
        let written = self.resolve_range(line);
        let adjusted = match written {
            Some((from, to)) => self.widen_lines_over_folds(from, to),
            None => {
                // No range of its own: a substitute runs over the cursor's line, so
                // that is what is adjusted — which is the case the manual's example is
                // about.
                let row = self.cursor.row;
                match (0..self.folds.len()).find(|index| {
                    let fold = self.folds[*index];
                    self.fold_is_shut(*index) && row >= fold.start && row <= fold.end
                }) {
                    Some(index) => (self.folds[index].start, self.folds[index].end),
                    None => return line.to_string(),
                }
            }
        };
        if written == Some(adjusted) {
            return line.to_string();
        }
        let rest = crate::substitute::strip_range(line).unwrap_or(line);
        let (from, to) = adjusted;
        format!("{},{}{rest}", from + 1, to + 1)
    }

    /// Widen a range of lines to cover every closed fold it touches.
    pub(crate) fn widen_lines_over_folds(&self, from: usize, to: usize) -> (usize, usize) {
        let (mut first, mut last) = (from.min(to), from.max(to));
        for (index, fold) in self.folds.iter().enumerate() {
            if !self.fold_is_shut(index) {
                continue;
            }
            if fold.start <= last && fold.end >= first {
                first = first.min(fold.start);
                last = last.max(fold.end);
            }
        }
        (first, last)
    }

    /// A span widened to cover every *closed* fold it touches.
    ///
    /// The manual's rule: "when using an operator, a closed fold is included as a
    /// whole. Thus `dl` deletes the whole closed fold under the cursor." The span's
    /// lines are what is compared, since a fold is a range of lines.
    pub(crate) fn widen_over_folds(&self, span: crate::command::Span) -> crate::command::Span {
        let mut first = self.buffer.char_to_line_col(span.start).0;
        let mut last = self.buffer.char_to_line_col(span.end.saturating_sub(1)).0;
        let mut linewise = span.linewise;
        for (index, fold) in self.folds.iter().enumerate() {
            if !self.fold_is_shut(index) {
                continue;
            }
            // Touching is enough: a range that starts or ends inside the fold takes
            // the whole of it.
            if fold.start <= last && fold.end >= first {
                first = first.min(fold.start);
                last = last.max(fold.end);
                linewise = true;
            }
        }
        if first == self.buffer.char_to_line_col(span.start).0
            && last == self.buffer.char_to_line_col(span.end.saturating_sub(1)).0
        {
            return span;
        }
        let lines = last - first + 1;
        let (start, end) = crate::command::line_span(self, first, lines);
        crate::command::Span {
            start,
            end,
            linewise,
        }
    }

    /// How deeply a fold is nested: one plus the number of folds that contain it.
    ///
    /// A fold's *level* is its nesting — the manual says so in as many words — and
    /// that is what `'foldlevel'` is compared against.
    pub fn fold_depth(&self, index: usize) -> usize {
        let Some(fold) = self.folds.get(index).copied() else {
            return 0;
        };
        let outer = self
            .folds
            .iter()
            .filter(|other| other.start < fold.start && other.end >= fold.end)
            .count();
        outer + 1
    }

    /// Whether a fold is *drawn* closed: either it was closed, or it is nested
    /// deeper than `'foldlevel'` — which is what `zm` and `zr` move.
    pub fn fold_is_shut(&self, index: usize) -> bool {
        fold_is_shut(&self.folds, self.options.borrow().foldlevel.0, index)
    }

    /// `zm` — show one level fewer; `zr` — one more.
    ///
    /// Vim's `'foldlevel'`: a fold nested deeper than it is drawn as one line. Zero
    /// means "as the folds are", and `zR` is as open as it goes.
    pub fn step_fold_level(&mut self, more: bool) -> bool {
        if self.folds.is_empty() {
            self.message = "E490: No fold found".to_string();
            return false;
        }
        let deepest = (0..self.folds.len())
            .map(|i| self.fold_depth(i))
            .max()
            .unwrap_or(0);
        let current = self.options.borrow().foldlevel.0;
        let next = if more {
            (current + 1).min(deepest)
        } else {
            current.saturating_sub(1)
        };
        self.options.borrow_mut().foldlevel = crate::settings::Count(next);
        self.settle_cursor();
        self.after_edit();
        self.message = format!("foldlevel={next}");
        true
    }

    /// Whether a line is hidden inside a closed fold.
    pub fn line_is_folded(&self, row: usize) -> bool {
        (0..self.folds.len()).any(|index| {
            self.fold_is_shut(index)
                && row > self.folds[index].start
                && row <= self.folds[index].end
        })
    }

    /// The fold that *starts* at `row` and is drawn closed, if one is: the line that
    /// stands in place of the range.
    ///
    /// The folds are kept sorted by where they start, so the first one found is the
    /// outermost — which is the one that decides how far the walk skips, since a
    /// closed fold takes everything inside it with it.
    pub fn closed_fold_at(&self, row: usize) -> Option<Fold> {
        (0..self.folds.len())
            .find(|index| self.fold_is_shut(*index) && self.folds[*index].start == row)
            .map(|index| self.folds[index])
    }

    /// Put the cursor on a line that is on screen: a closed fold hides the lines
    /// after its first one, and a cursor inside one has nowhere to be.
    pub fn settle_cursor(&mut self) {
        let came_from = self.last_cursor_row;
        self.settle_cursor_from(came_from);
    }

    /// The same, told which line the cursor came from.
    ///
    /// Which way it was going is what decides where it lands: a cursor moving *down*
    /// into a closed fold is on its way past it and comes out at the line after —
    /// which is what makes `j` step over a fold rather than into it — and one moving
    /// *up* lands on the fold's first line. An edit that did not move the cursor at
    /// all (closing a fold, say) puts it on that first line, which is the line that
    /// is drawn.
    pub(super) fn settle_cursor_from(&mut self, came_from: usize) {
        let row = self.cursor.row;
        let Some(fold) = (0..self.folds.len())
            .filter(|index| {
                let fold = self.folds[*index];
                self.fold_is_shut(*index) && row > fold.start && row <= fold.end
            })
            .min_by_key(|index| {
                let fold = self.folds[*index];
                fold.end - fold.start
            })
            .map(|index| self.folds[index])
        else {
            return;
        };
        self.cursor.row = if row > came_from {
            (fold.end + 1).min(self.buffer.line_count().saturating_sub(1))
        } else {
            fold.start
        };
        // (The manual's "the cursor is always *displayed* in the first column" while it
        // is on folded lines is about the drawing and not the cursor's value: setting
        // the column here would undo the motion an operator is about to take. The
        // roadmap has it, with the renderer as the place it belongs.)
    }

    /// The line an edit starting at `start` is on, or `None` when there are no folds
    /// to keep up to date.
    pub(super) fn fold_line_for(&self, start: usize) -> Option<usize> {
        if self.folds.is_empty() {
            return None;
        }
        Some(
            self.buffer
                .char_to_line_col(start.min(self.buffer.len_chars()))
                .0,
        )
    }

    /// Move the folds to follow an edit of `delta` lines starting at line `at`: the
    /// folds after it move by the difference, and one that *contains* it grows or
    /// shrinks by it — which is what keeps a fold on the text it was put on.
    pub(super) fn shift_folds(&mut self, at: usize, delta: isize) {
        let last = self.buffer.line_count().saturating_sub(1);
        for fold in &mut self.folds {
            if fold.start > at {
                // Entirely after the edit: the whole fold moves.
                fold.start = fold.start.saturating_add_signed(delta).min(last);
                fold.end = fold.end.saturating_add_signed(delta).min(last);
            } else if fold.end >= at {
                // The edit is inside it (or at its first line): the fold grows or
                // shrinks by the lines that came and went.
                fold.end = fold.end.saturating_add_signed(delta).min(last);
            }
            if fold.end < fold.start {
                fold.end = fold.start;
            }
        }
        // A fold of one line hides nothing, so the edit may have made one pointless.
        self.folds.retain(|fold| fold.end > fold.start);
    }

    /// The lines of the window that are drawn, from `top_line` on, for as many rows
    /// as the caller has room for.
    ///
    /// A closed fold is one line — its first — and the rest of it is not there. This
    /// is the whole of what folds do to drawing: the rows and the lines stop being
    /// the same thing.
    pub fn visible_lines(&self, top_line: usize, rows: usize) -> Vec<usize> {
        let last = self.buffer.line_count().saturating_sub(1);
        let mut out = Vec::with_capacity(rows);
        let mut row = top_line.min(last);
        while out.len() < rows && row <= last {
            out.push(row);
            // A closed fold takes its own range with it; anything else takes one
            // line.
            row = match self.closed_fold_at(row) {
                Some(fold) => fold.end + 1,
                None => row + 1,
            };
        }
        out
    }

    /// How many *visible* lines there are from `top_line` to `row`, which is what
    /// scrolling counts: a closed fold is one line and not the several it hides.
    pub fn visible_rows_between(&self, top_line: usize, row: usize) -> usize {
        let visible = self.visible_lines(top_line, self.buffer.line_count());
        visible.iter().take_while(|line| **line < row).count()
    }
}
