use std::path::Path;

use crate::buffer::Buffer;
use crate::cursor::Cursor;
use crate::highlight::Language;
use crate::mode::Mode;
use crate::render::status_line;

use super::*;

impl Editor {
    /// Where every window is on screen, in cells, for the keys that move
    /// between them and for the renderer that draws them.
    ///
    /// The tree is walked with the whole screen at the root; a split hands its
    /// children equal shares of its rectangle along its axis, keeping one row
    /// or column for the separator between them.
    pub(crate) fn window_rects(&self) -> Vec<Rect> {
        let mut rects = vec![Rect::default(); self.windows.len()];
        layout_rects(
            &self.layout,
            Rect {
                x: 0,
                y: 0,
                width: self.viewport.width,
                // The bottom row of the screen is the message row: the windows
                // divide what is above it.
                height: self.viewport.height.saturating_sub(1),
            },
            &mut rects,
        );
        rects
    }

    /// Which document the window that is on screen shows.
    pub fn current_document(&self) -> usize {
        self.windows[self.current_window].document
    }

    /// Put the window that is on screen back into its slot. The size in the
    /// viewport is the screen's and stays where it is: only the scroll and the
    /// cursor belong to the window.
    pub(super) fn save_window(&mut self) {
        let folds = std::mem::take(&mut self.folds);
        let window = &mut self.windows[self.current_window];
        window.cursor = self.cursor;
        window.top_line = self.viewport.top_line;
        window.left_col = self.viewport.left_col;
        window.folds = folds;
    }

    /// The other half of [`Editor::save_window`].
    pub(super) fn load_window(&mut self) {
        let window = &self.windows[self.current_window];
        self.cursor = window.cursor;
        self.viewport.top_line = window.top_line;
        self.viewport.left_col = window.left_col;
        self.folds = window.folds.clone();
    }

    /// Show the document at `index` in the window that is up. Its position is
    /// the window's, kept and clamped into the text that arrives.
    pub(crate) fn show_document(&mut self, index: usize) -> bool {
        if index >= self.documents.len() || index == self.current_document() {
            return false;
        }
        self.save_document();
        self.windows[self.current_window].document = index;
        self.load_document();
        // The folds were line numbers in the file that was here, and mean nothing in
        // this one. (Vim's manual says the same: "the manual folds are lost when you
        // abandon the file".)
        self.folds.clear();
        self.windows[self.current_window].folds.clear();
        self.clamp_cursor();
        self.after_switch();
        true
    }

    /// Put the tab that is on screen back into its slot.
    pub(super) fn save_tab(&mut self) {
        let tab = &mut self.tabs[self.current_tab];
        tab.windows = std::mem::take(&mut self.windows);
        tab.layout = self.layout.clone();
        tab.current_window = self.current_window;
    }

    /// The other half of [`Editor::save_tab`].
    pub(super) fn load_tab(&mut self) {
        let tab = &mut self.tabs[self.current_tab];
        self.windows = std::mem::take(&mut tab.windows);
        self.layout = tab.layout.clone();
        self.current_window = tab.current_window;
    }

    /// Show the tab at `index`. Says whether anything changed.
    pub(crate) fn switch_to_tab(&mut self, index: usize) -> bool {
        if index >= self.tabs.len() || index == self.current_tab {
            return false;
        }
        self.save_window();
        self.save_document();
        self.save_tab();
        self.current_tab = index;
        self.load_tab();
        self.load_document();
        self.load_window();
        self.after_switch();
        true
    }

    /// Open a tab: one window on `document`, or on an empty file when nothing
    /// was named. The new tab becomes the current one, the way vim leaves it.
    pub(crate) fn open_tab(&mut self, document: Option<usize>) -> usize {
        self.save_window();
        self.save_document();
        self.save_tab();
        let document = match document {
            Some(document) => document,
            None => {
                self.documents.push(Document::placeholder());
                self.documents.len() - 1
            }
        };
        self.tabs
            .push(Tab::new(vec![Window::new(document)], Layout::Leaf(0)));
        self.current_tab = self.tabs.len() - 1;
        self.load_tab();
        self.load_document();
        self.load_window();
        self.after_switch();
        self.current_tab
    }

    /// Close the tab that is on screen. The last one is not closed: a session
    /// with no tab has nowhere to show anything.
    pub(crate) fn close_tab(&mut self) -> bool {
        if self.tabs.len() < 2 {
            return false;
        }
        self.save_window();
        self.save_document();
        self.save_tab();
        self.tabs.remove(self.current_tab);
        if self.current_tab >= self.tabs.len() {
            self.current_tab = self.tabs.len() - 1;
        }
        self.load_tab();
        self.load_document();
        self.load_window();
        self.after_switch();
        true
    }

    /// Show the next or previous tab, wrapping around.
    pub(crate) fn step_tab(&mut self, step: isize) -> bool {
        let count = self.tabs.len() as isize;
        if count < 2 {
            return false;
        }
        let next = (self.current_tab as isize + step).rem_euclid(count) as usize;
        self.switch_to_tab(next)
    }

    /// Show the window at `index`. Says whether anything changed.
    pub(crate) fn switch_to_window(&mut self, index: usize) -> bool {
        if index >= self.windows.len() || index == self.current_window {
            return false;
        }
        self.save_window();
        self.save_document();
        self.current_window = index;
        self.load_document();
        self.load_window();
        self.after_switch();
        true
    }

    /// What has to be let go of when the text underneath it changes: anything
    /// holding character indices into the document that was on screen.
    pub(super) fn after_switch(&mut self) {
        self.mode = Mode::Normal;
        self.selection = None;
        self.mouse_dragged = false;
        self.clear_pending();
        self.cmdline.clear();
        self.cmdline_cursor = 0;
        self.forget_recall();
        self.prompt = Prompt::Command;
        self.message = format!("\"{}\"", self.buffer.display_name());
        self.redraw = true;
    }

    /// Open `path` as a document of its own and show it in the window that is
    /// up. The file is read first, so a path that is not there leaves the editor
    /// as it was.
    pub(crate) fn open_file(&mut self, path: &Path) -> Result<(), BufferError> {
        let buffer = Buffer::from_path(path)?;
        let language = Language::from_path(path);
        self.save_document();
        self.documents.push(Document::new(buffer, language));
        self.windows[self.current_window].document = self.documents.len() - 1;
        self.load_document();
        // The folds belonged to the file that was here: they are line numbers in
        // *its* text, and mean nothing in this one. (Vim's manual says the same —
        // "the manual folds are lost when you abandon the file".)
        self.folds.clear();
        self.windows[self.current_window].folds.clear();
        // A file that arrives is looked at from the top: the position the
        // window had belongs to whatever it was showing before.
        self.cursor = Cursor::default();
        self.viewport.top_line = 0;
        self.viewport.left_col = 0;
        self.after_switch();
        Ok(())
    }

    /// Open `path` as a document of its own, without showing it anywhere.
    ///
    /// This is what `:sp file` needs: the file has to join the list, and the
    /// window that asked for the split keeps what it had.
    pub(crate) fn add_file(&mut self, path: &Path) -> Result<usize, BufferError> {
        let buffer = Buffer::from_path(path)?;
        let language = Language::from_path(path);
        self.documents.push(Document::new(buffer, language));
        Ok(self.documents.len() - 1)
    }

    /// Split the window that is up along `axis`, showing `document` (or the
    /// same document as the window it came from) in the new one. The new window
    /// becomes the current one, the way vim leaves it.
    pub(crate) fn split_window(&mut self, axis: Axis, document: Option<usize>) -> usize {
        self.save_window();
        self.save_document();
        let beside = self.current_window;
        let document = document.unwrap_or_else(|| self.windows[beside].document);
        let window = Window {
            document,
            cursor: self.cursor,
            top_line: self.viewport.top_line,
            left_col: self.viewport.left_col,
            // A window that is made from this one starts with its folds: they are
            // about the file, and a person who split to see two places in it did
            // not ask for the folds to be forgotten.
            folds: self.folds.clone(),
        };
        self.windows.push(window);
        let window = self.windows.len() - 1;
        self.layout.split(beside, axis, window);
        self.current_window = window;
        // The new window shows a document that may or may not be the one that
        // was live: bring its state out.
        self.load_document();
        self.clamp_cursor();
        self.after_switch();
        window
    }

    /// Close the window at `index`. Closing the last window leaves an empty one
    /// behind, the way a window that has lost its file keeps its place on
    /// screen — the session quits by other means.
    pub(crate) fn close_window(&mut self, index: usize) {
        // A window showing a *list* has no file behind it: the document was made
        // for the window, and closing the window is what takes it away. Without
        // this every list a session opened would stay in `documents` for the rest
        // of the session, which is a leak a person can see in `:ls`.
        //
        // The document on screen keeps its state in the live fields and only a
        // placeholder in its slot — the line this project keeps meeting — so the
        // question "is this a list?" is asked of the live field when it is the
        // live document.
        let showing = self.windows.get(index).map(|window| window.document);
        if let Some(document) = showing {
            let is_list = if document == self.current_document() {
                self.list.is_some()
            } else {
                self.documents
                    .get(document)
                    .is_some_and(|d| d.list.is_some())
            };
            if is_list {
                self.close_document(document);
                return;
            }
        }
        let was_current = index == self.current_window;
        if was_current {
            // The live window and its document go back to their slots: the
            // document is still open even when nothing shows it.
            self.save_document();
            self.drop_window();
        }
        self.windows.remove(index);
        self.layout.remove(index);
        renumber(&mut self.layout, index);
        if self.windows.is_empty() {
            // Nothing is left to show a document: a window on whatever there
            // is, or an empty document if there is nothing at all.
            if self.documents.is_empty() {
                self.documents.push(Document::placeholder());
            }
            self.windows.push(Window::new(0));
            self.layout = Layout::Leaf(0);
            self.current_window = 0;
            self.load_document();
            self.load_window();
        } else {
            if index < self.current_window {
                self.current_window -= 1;
            }
            self.current_window = self.current_window.min(self.windows.len() - 1);
            // The window that is up now may show a different document.
            self.load_document();
            self.load_window();
            self.clamp_cursor();
        }
        self.after_switch();
    }

    /// Close a document, and every window that was showing it.
    pub(crate) fn close_document(&mut self, index: usize) {
        if self.current_document() == index {
            // The document is going: nothing keeps its state, and the live
            // window is one of the windows that goes with it.
            self.drop_document();
            self.drop_window();
        }
        let doomed: Vec<usize> = self
            .windows
            .iter()
            .enumerate()
            .filter(|(_, window)| window.document == index)
            .map(|(at, _)| at)
            .collect();
        // From the highest down, so the positions of the ones still to go hold.
        for at in doomed.iter().rev() {
            self.windows.remove(*at);
            self.layout.remove(*at);
            renumber(&mut self.layout, *at);
        }
        self.documents.remove(index);
        for window in &mut self.windows {
            if window.document > index {
                window.document -= 1;
            }
        }
        if self.windows.is_empty() {
            if self.documents.is_empty() {
                self.documents.push(Document::placeholder());
            }
            self.windows.push(Window::new(0));
            self.layout = Layout::Leaf(0);
            self.current_window = 0;
        } else {
            self.current_window = self.current_window.min(self.windows.len() - 1);
        }
        self.load_document();
        self.load_window();
        self.clamp_cursor();
        self.after_switch();
    }

    /// The status line text.
    pub fn status(&self) -> String {
        let status = status_line(
            &self.mode_label(),
            &self.buffer.display_name(),
            self.is_dirty(),
            self.cursor,
            &self.diagnostics,
        );
        // Vim's tabline, in the one row there is for it: which tab of how many,
        // and only when there is more than one to be in.
        if self.tabs.len() > 1 {
            return format!("{status}  [{}/{}]", self.current_tab + 1, self.tabs.len());
        }
        status
    }

    /// Whether the document on screen has been changed since it was last
    /// written.
    ///
    /// Nothing is stored for this. The undo history hands out an identity to
    /// every state its steps produce, and the last write remembers which state
    /// that was: `u` back to it clears the marker and redoing away from it puts
    /// the marker back, with nothing comparing text and nothing to get out of
    /// date. An edit that happens to restore the old text is still an edit —
    /// which is what vim does, and what keeps `:q` honest.
    pub fn is_dirty(&self) -> bool {
        self.history.is_dirty()
    }

    /// How many edits this document has had. It only ever grows, which is what
    /// makes it a version number: a client that has sent the text as of edit N
    /// has nothing to send until the count moves.
    pub fn edit_count(&self) -> u64 {
        self.history.edits()
    }

    /// Whether the line at the bottom is being typed into.
    pub(super) fn in_prompt(&self) -> bool {
        self.mode == Mode::Command
    }

    /// What the status line calls the current state: the mode, and what a visual
    /// selection covers, which is the difference between taking characters and
    /// taking whole lines.
    pub(super) fn mode_label(&self) -> String {
        match (self.mode, self.selection) {
            (Mode::Visual, Some(selection)) => {
                format!("{}{}", self.mode.name(), selection.kind.name())
            }
            _ => self.mode.name().to_string(),
        }
    }
}
