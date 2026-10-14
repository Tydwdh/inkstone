use crate::command::{self};
use crate::mode::VisualKind;
use crate::register::{Kind, Register};

use super::*;

impl Editor {
    /// The character range the visual selection covers, `[start, end)`.
    ///
    /// A characterwise selection includes the character the cursor is on; a
    /// linewise one covers whole lines, terminators and all, which is what makes
    /// `V` + `d` the same thing as `dd`.
    pub(crate) fn visual_span(&self) -> Option<(usize, usize)> {
        let selection = self.selection?;
        let anchor = selection.anchor.char_idx(&self.buffer);
        let cursor = self.cursor.char_idx(&self.buffer);
        let (start, end) = if selection.kind == VisualKind::Linewise {
            let (first, last) = if selection.anchor.row <= self.cursor.row {
                (selection.anchor.row, self.cursor.row)
            } else {
                (self.cursor.row, selection.anchor.row)
            };
            return Some(command::line_span(self, first, last - first + 1));
        } else if anchor <= cursor {
            (anchor, cursor + 1)
        } else {
            (cursor, anchor + 1)
        };
        (start < end).then_some((start, end))
    }

    /// The rows and columns a blockwise selection covers, as a rectangle.
    ///
    /// Rows are buffer rows and columns are *character* columns within each row,
    /// which is what the cursor counts here: a tab is one column, as it is
    /// everywhere else in this editor. `None` unless `<C-v>` is what selected.
    pub(crate) fn visual_block(&self) -> Option<(usize, usize, usize, Option<usize>)> {
        let selection = self.selection?;
        if selection.kind != VisualKind::Blockwise {
            return None;
        }
        let (first, last) = if selection.anchor.row <= self.cursor.row {
            (selection.anchor.row, self.cursor.row)
        } else {
            (self.cursor.row, selection.anchor.row)
        };
        let (left, right) = if selection.anchor.col <= self.cursor.col {
            (selection.anchor.col, self.cursor.col)
        } else {
            (self.cursor.col, selection.anchor.col)
        };
        // `<C-v>$` makes the right edge the end of each line rather than a column,
        // which is the difference between a rectangle and a ragged block.
        Some((first, last, left, (!selection.to_end).then_some(right)))
    }

    /// The character range the block covers on one row, as the buffer is *now*.
    ///
    /// A row shorter than the block contributes only what it has: there are no
    /// characters past the end of a line to take, and inventing spaces would be
    /// inventing text. `None` on the right is the end of the line, which is the
    /// whole of what is left of it.
    pub(super) fn block_span_on(
        &self,
        row: usize,
        left: usize,
        right: Option<usize>,
    ) -> (usize, usize) {
        let len = self.buffer.line_len(row);
        let base = self.buffer.line_col_to_char(row, 0);
        let start = base + left.min(len);
        let end = match right {
            None => base + len,
            Some(right) => base + (right + 1).min(len),
        };
        (start, end)
    }

    /// The character ranges a blockwise selection covers, one per row, in order.
    pub(crate) fn block_spans(&self) -> Vec<(usize, usize)> {
        let Some((first, last, left, right)) = self.visual_block() else {
            return Vec::new();
        };
        (first..=last)
            .map(|row| self.block_span_on(row, left, right))
            .collect()
    }

    /// Take the blockwise selection out into the register, and say where the
    /// cursor goes: the block's top left, as a row and a column.
    ///
    /// The rows go into the register as lines, which is what `p` can put back:
    /// vim's own block register remembers the *shape* and puts it in columns, and
    /// that is a `p` of its own — this one is the text.
    ///
    /// The rows are taken in *order*, from the first down, which is what lets the
    /// whole thing fold into one history step: `History` joins a group's steps
    /// when each one starts after the last, and a block taken from the bottom up
    /// starts before it. Each row's offsets are worked out as the deletion
    /// reaches it, since every earlier deletion moved them.
    pub(crate) fn cut_block(&mut self) -> (usize, usize) {
        let Some((first, last, left, right)) = self.visual_block() else {
            return (self.cursor.row, self.cursor.col);
        };
        let spans: Vec<(usize, usize)> = (first..=last)
            .map(|row| self.block_span_on(row, left, right))
            .collect();
        let text = self.block_text(&spans);
        if !text.is_empty() {
            self.store(Register::new(text, Kind::Blockwise), false);
        }
        for row in first..=last {
            let (start, end) = self.block_span_on(row, left, right);
            if start < end {
                self.replace_range(start, end, "", Some(start));
            }
        }
        (first, left)
    }

    /// Copy the blockwise selection into the register, and say where the cursor
    /// goes, the same way.
    pub(crate) fn copy_block(&mut self) -> (usize, usize) {
        let Some((first, last, left, right)) = self.visual_block() else {
            return (self.cursor.row, self.cursor.col);
        };
        let spans: Vec<(usize, usize)> = (first..=last)
            .map(|row| self.block_span_on(row, left, right))
            .collect();
        let text = self.block_text(&spans);
        if !text.is_empty() {
            self.store(Register::new(text, Kind::Blockwise), true);
        }
        (first, left)
    }

    pub(super) fn block_text(&self, spans: &[(usize, usize)]) -> String {
        let mut text = String::new();
        for (index, (start, end)) in spans.iter().enumerate() {
            if index > 0 {
                text.push('\n');
            }
            text.push_str(&self.buffer.slice_text(*start, *end));
        }
        text
    }
}
