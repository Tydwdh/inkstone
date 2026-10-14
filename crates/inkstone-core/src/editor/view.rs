use crate::render::{compose, Composed, View};

use super::*;

impl Editor {
    /// The view of the editor that drawing and hit-testing share.
    pub(crate) fn view(&self) -> View<'_> {
        let options = self.options.borrow().clone();
        View {
            buffer: &self.buffer,
            cursor: self.cursor,
            mode: self.mode,
            selection: self.selection.and_then(|_| self.visual_span()),
            matches: self.search.matches(),
            current_match: self.search.match_at(self.cursor.char_idx(&self.buffer)),
            incsearch: self
                .incsearch
                .as_ref()
                .is_some_and(|preview| preview.showing),
            highlighter: options.highlight.then_some(&self.highlighter),
            viewport: self.viewport,
            block: self.visual_block(),
            folds: &self.folds,
            foldlevel: self.options.borrow().foldlevel.0,
            rects: self.window_rects(),
            documents: &self.documents,
            windows: &self.windows,
            current_window: self.current_window,
            completion: self.completion.as_ref(),
            diagnostics: &self.diagnostics,
            dirty: self.is_dirty(),
            tabstop: options.tabstop,
            number: options.number,
            status: self.status(),
            cmdline: self
                .in_prompt()
                .then(|| format!("{}{}", self.prompt.spelling(), self.cmdline)),
            cmdline_cursor: self.in_prompt().then_some(self.cmdline_cursor),
            message: self.message.clone(),
        }
    }

    /// Compose the whole screen.
    pub fn compose(&self) -> Composed {
        compose(&self.view())
    }

    // -- undoable edits ----------------------------------------------------
}
