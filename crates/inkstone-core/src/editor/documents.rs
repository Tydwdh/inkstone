use std::path::{Path, PathBuf};

use crate::buffer::Buffer;
use crate::cursor::Cursor;
use crate::highlight::Language;
use crate::mode::Mode;

use super::*;

/// A directory as a person would write it: without the `.` components the
/// editor's own joins leave behind, and without the prefix Windows puts on a
/// canonical path.
fn tidy_dir(path: &Path) -> PathBuf {
    let resolved = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    crate::command::tidy(resolved)
}

impl Editor {
    /// Where the session is, for a shell or a program to start in: `:!cargo
    /// build` should build what a person is looking at, not what the daemon
    /// happened to be started in.
    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    /// Where a path the user named leads.
    ///
    /// Relative to the *session's* directory, not to the process's. That is what
    /// makes `:e main.rs` inside a listing of `src/` mean `src/main.rs`: the
    /// directory a person is looking at is the directory they mean, which is
    /// netrw's behaviour and the reason it has one. An absolute path is left
    /// alone, so nothing that already knows where it is goes through here twice.
    pub(crate) fn resolve(&self, path: &Path) -> PathBuf {
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.cwd.join(path)
        }
    }

    /// `:cd [dir]` — where relative paths lead from now on.
    ///
    /// No argument is the home directory, which is vim's rule; a directory that
    /// is not there is refused rather than half-taken, because every path after
    /// it would be relative to somewhere that does not exist.
    pub(crate) fn change_dir(&mut self, dir: Option<&Path>) {
        let target = match dir {
            Some(dir) => self.resolve(dir),
            None => match std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")) {
                Some(home) => PathBuf::from(home),
                None => {
                    self.message = "E344: Can't find directory".to_string();
                    return;
                }
            },
        };
        if !target.is_dir() {
            self.message = format!("E344: Can't find directory: {}", target.display());
            return;
        }
        self.cwd = target;
        self.message = self.cwd.display().to_string();
        self.redraw = true;
    }

    /// `:pwd` — where the session thinks it is.
    pub(crate) fn print_dir(&mut self) {
        self.message = self.cwd.display().to_string();
        self.redraw = true;
    }

    /// A directory that is being looked at is the directory a person is in.
    ///
    /// Vim's netrw changes the local directory when it shows one — that is how a
    /// file manager makes `:e name` mean the name beside it — and this is the
    /// same rule, said once, in the one place every listing arrives through.
    pub(super) fn note_directory(&mut self) {
        if let Some(dir) = self.buffer.path().filter(|_| self.buffer.is_listing()) {
            self.cwd = tidy_dir(dir);
        }
    }

    /// Replace the buffer with the contents of `path`.
    pub fn load_file(&mut self, path: &Path) -> Result<(), BufferError> {
        let path = self.resolve(path);
        let buffer = Buffer::from_path(&path)?;
        self.buffer = buffer;
        self.highlighter.set_language(Language::from_path(&path));
        self.cursor = Cursor::default();
        // The history belongs to the document that was open: its steps hold
        // character indices into it, so they cannot be replayed onto this one.
        self.history.reset();
        self.viewport.top_line = 0;
        self.viewport.left_col = 0;
        self.mode = Mode::Normal;
        self.cmdline.clear();
        self.cmdline_cursor = 0;
        self.forget_recall();
        self.prompt = Prompt::Command;
        self.clear_pending();
        self.search.clear();
        // Diagnostics were about the text that was here, and it is not.
        self.diagnostics.clear();
        self.selection = None;
        self.note_directory();
        self.message = format!("\"{}\"", self.buffer.display_name());
        self.redraw = true;
        Ok(())
    }

    /// Put the document that is on screen back into its slot, so another can
    /// take its place. Everything that follows the text through a file is
    /// swapped in one go, and the slot ends up holding the placeholders.
    pub(super) fn save_document(&mut self) {
        let index = self.current_document();
        // The values of the file that is leaving go back to the global ones
        // first: that borrows the editor, and the slot below borrows it too.
        self.reset_local_options();
        let slot = &mut self.documents[index];
        // The cursor on screen is where this document was last looked at.
        slot.cursor = self.cursor;
        std::mem::swap(&mut slot.buffer, &mut self.buffer);
        std::mem::swap(&mut slot.history, &mut self.history);
        std::mem::swap(&mut slot.search, &mut self.search);
        std::mem::swap(&mut slot.highlighter, &mut self.highlighter);
        std::mem::swap(&mut slot.diagnostics, &mut self.diagnostics);
        std::mem::swap(&mut slot.marks, &mut self.marks);
        std::mem::swap(&mut slot.jumps, &mut self.jumps);
        std::mem::swap(&mut slot.jump_from, &mut self.jump_from);
        std::mem::swap(&mut slot.changes, &mut self.changes);
        std::mem::swap(&mut slot.list, &mut self.list);
        std::mem::swap(&mut slot.list_targets, &mut self.list_targets);
        std::mem::swap(&mut slot.last_visual, &mut self.last_visual);
        std::mem::swap(&mut slot.insert_again_at, &mut self.insert_again_at);
        // The settings `:setlocal` gave this file travel with it, and the ones
        // in force go back to the global value: what is left behind is a file
        // that has no say in what the next one looks like.
        slot.local_options = std::mem::take(&mut self.local_options);
    }

    /// The other half of [`Editor::save_document`]: bring the document of the
    /// current window to the screen.
    pub(super) fn load_document(&mut self) {
        let index = self.current_document();
        let slot = &mut self.documents[index];
        std::mem::swap(&mut slot.buffer, &mut self.buffer);
        std::mem::swap(&mut slot.history, &mut self.history);
        std::mem::swap(&mut slot.search, &mut self.search);
        std::mem::swap(&mut slot.highlighter, &mut self.highlighter);
        std::mem::swap(&mut slot.diagnostics, &mut self.diagnostics);
        std::mem::swap(&mut slot.marks, &mut self.marks);
        std::mem::swap(&mut slot.jumps, &mut self.jumps);
        std::mem::swap(&mut slot.jump_from, &mut self.jump_from);
        std::mem::swap(&mut slot.changes, &mut self.changes);
        std::mem::swap(&mut slot.list, &mut self.list);
        std::mem::swap(&mut slot.list_targets, &mut self.list_targets);
        std::mem::swap(&mut slot.last_visual, &mut self.last_visual);
        std::mem::swap(&mut slot.insert_again_at, &mut self.insert_again_at);
        // Where the document was last looked at. A window that is being
        // switched to puts its own cursor back on top of this one.
        self.cursor = slot.cursor;
        // And what `:setlocal` said about it, which is in force from now on.
        self.local_options = std::mem::take(&mut slot.local_options);
        self.apply_local_options();
    }

    /// Forget the document that is on screen, leaving a placeholder in the live
    /// fields. What was there is dropped, not stored: the document is going
    /// away.
    pub(super) fn drop_document(&mut self) {
        let mut fresh = Document::placeholder();
        std::mem::swap(&mut fresh.buffer, &mut self.buffer);
        std::mem::swap(&mut fresh.history, &mut self.history);
        std::mem::swap(&mut fresh.search, &mut self.search);
        std::mem::swap(&mut fresh.highlighter, &mut self.highlighter);
        std::mem::swap(&mut fresh.diagnostics, &mut self.diagnostics);
        std::mem::swap(&mut fresh.marks, &mut self.marks);
        std::mem::swap(&mut fresh.jumps, &mut self.jumps);
        std::mem::swap(&mut fresh.jump_from, &mut self.jump_from);
        std::mem::swap(&mut fresh.changes, &mut self.changes);
        std::mem::swap(&mut fresh.list, &mut self.list);
        std::mem::swap(&mut fresh.list_targets, &mut self.list_targets);
        std::mem::swap(&mut fresh.last_visual, &mut self.last_visual);
        std::mem::swap(&mut fresh.insert_again_at, &mut self.insert_again_at);
    }

    /// Write the buffer out, and remember that what is on disk is what is being
    /// looked at.
    ///
    /// The one place a write happens, so that `:w` and a language server writing
    /// what it formatted cannot drift apart: both go through here, and both leave
    /// the saved marker where a person would expect it.
    pub fn write_buffer(&mut self, path: Option<&Path>) -> Result<PathBuf, BufferError> {
        let resolved = path.map(|path| self.resolve(path));
        let path = self.buffer.save(resolved.as_deref())?;
        // The state that is on disk now is the one being looked at: say so, so
        // undoing away from it and back to it keeps the marker honest.
        self.history.mark_saved();
        Ok(path)
    }
}
