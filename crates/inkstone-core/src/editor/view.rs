use crate::render::{compose, Composed, View};

use super::*;

impl Editor {
    /// The view of the editor that drawing and hit-testing share.
    pub(crate) fn view(&self) -> View<'_> {
        let options = self.options.borrow().clone();
        // `'hlsearch'` is what draws the matches, and `:nohlsearch` turns that
        // off until a search is used again. Neither gates the match a search
        // being typed is showing: `'incsearch'` highlights that either way,
        // which is the whole point of a preview.
        let lighting = self.search_is_highlighted();
        let previewing = self
            .incsearch
            .as_ref()
            .is_some_and(|preview| preview.showing);
        View {
            buffer: &self.buffer,
            cursor: self.cursor,
            mode: self.mode,
            selection: self.selection.and_then(|_| self.visual_span()),
            matches: if lighting { self.search.matches() } else { &[] },
            current_match: (lighting || previewing)
                .then(|| self.search.match_at(self.cursor.char_idx(&self.buffer)))
                .flatten(),
            incsearch: previewing,
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
