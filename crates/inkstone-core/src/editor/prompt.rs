use inkstone_rpc::{KeyCode, KeyMsg};

use crate::mode::Mode;
use crate::search::Direction;

use super::*;

impl Editor {
    pub(super) fn handle_command_line(&mut self, key: KeyMsg) {
        match key.code {
            KeyCode::Esc => {
                // A cancelled search leaves the cursor where it started and puts
                // back the search that was in force.
                self.end_preview(false);
                self.mode = Mode::Normal;
                self.cmdline.clear();
                self.cmdline_cursor = 0;
                self.forget_recall();
            }
            KeyCode::Enter => {
                // The preview moved the cursor to the match it found; the search
                // itself runs from where the prompt was opened, so it lands on
                // that same match rather than on the one after it.
                self.end_preview(true);
                let line = std::mem::take(&mut self.cmdline);
                self.cmdline_cursor = 0;
                self.remember_prompt(&line);
                self.forget_recall();
                let prompt = self.prompt;
                self.prompt = Prompt::Command;
                self.mode = Mode::Normal;
                match prompt.direction() {
                    Some(direction) => self.run_search(&line, direction),
                    None => self.execute_cmdline(&line),
                }
            }
            KeyCode::Backspace => {
                self.cmdline_delete_backward();
                self.preview_search();
            }
            KeyCode::Delete => {
                self.cmdline_delete_forward();
                self.preview_search();
            }
            KeyCode::Left => self.cmdline_cursor = self.cmdline_cursor.saturating_sub(1),
            KeyCode::Right => {
                self.cmdline_cursor = (self.cmdline_cursor + 1).min(self.cmdline_len())
            }
            KeyCode::Home => self.cmdline_cursor = 0,
            KeyCode::End => self.cmdline_cursor = self.cmdline_len(),
            KeyCode::Up => self.cmdline_recall(true),
            KeyCode::Down => self.cmdline_recall(false),
            // While the pattern is being typed, `<C-g>` and `<C-t>` walk the
            // matches it has so far, which is what they do in vim.
            KeyCode::Char('g') if key.is_ctrl() => self.preview_next(Direction::Forward),
            KeyCode::Char('t') if key.is_ctrl() => self.preview_next(Direction::Backward),
            KeyCode::Char(c) if !key.is_ctrl() && !key.is_alt() => {
                self.cmdline_push(c);
                self.preview_search();
            }
            _ => {}
        }
    }

    /// How long the line being typed is, in characters.
    pub(super) fn cmdline_len(&self) -> usize {
        self.cmdline.chars().count()
    }

    /// Type a character at the prompt's cursor.
    pub(super) fn cmdline_push(&mut self, c: char) {
        let at = byte_index(&self.cmdline, self.cmdline_cursor);
        self.cmdline.insert(at, c);
        self.cmdline_cursor += 1;
        // Typing again means the line is no longer a line from the history.
        self.prompt_index = None;
    }

    /// Delete the character before the prompt's cursor.
    pub(super) fn cmdline_delete_backward(&mut self) {
        if self.cmdline_cursor == 0 {
            return;
        }
        self.cmdline_cursor -= 1;
        self.cmdline_delete_forward();
    }

    /// Delete the character at the prompt's cursor.
    pub(super) fn cmdline_delete_forward(&mut self) {
        let from = byte_index(&self.cmdline, self.cmdline_cursor);
        let to = byte_index(&self.cmdline, self.cmdline_cursor + 1);
        if from == to {
            return;
        }
        self.cmdline.replace_range(from..to, "");
        self.prompt_index = None;
    }

    /// Walk the history this prompt keeps: `backwards` is `Up`.
    ///
    /// The line being typed is saved the first time and comes back when the walk
    /// runs off the end, so a half-typed command is not lost by looking at what
    /// was run before it.
    pub(super) fn cmdline_recall(&mut self, backwards: bool) {
        let history: &[String] = match self.prompt {
            Prompt::Command => &self.command_history,
            _ => &self.search_history,
        };
        if history.is_empty() {
            return;
        }
        let next = match (self.prompt_index, backwards) {
            (None, true) => {
                self.prompt_draft = self.cmdline.clone();
                Some(history.len() - 1)
            }
            (None, false) => None,
            (Some(0), true) => Some(0),
            (Some(index), true) => Some(index - 1),
            (Some(index), false) => (index + 1 < history.len()).then_some(index + 1),
        };
        let line = match next {
            Some(index) => history[index].clone(),
            None => self.prompt_draft.clone(),
        };
        self.prompt_index = next;
        self.cmdline = line;
        self.cmdline_cursor = self.cmdline_len();
    }

    /// Remember a line that was run, so `Up` finds it again.
    ///
    /// Empty lines are not worth keeping, and neither is the same line twice in
    /// a row.
    pub(super) fn remember_prompt(&mut self, line: &str) {
        if line.is_empty() {
            return;
        }
        let history = match self.prompt {
            Prompt::Command => &mut self.command_history,
            _ => &mut self.search_history,
        };
        if history.last().map(String::as_str) != Some(line) {
            history.push(line.to_string());
        }
    }

    /// Stop reviewing the history, and forget the saved draft.
    pub(super) fn forget_recall(&mut self) {
        self.prompt_index = None;
        self.prompt_draft.clear();
    }

    /// Search for `pattern`, and move the cursor to the first match after it.
    ///
    /// An empty pattern repeats the last search, which is what `/` then
    /// `<Enter>` means in vim; with nothing to repeat it says so rather than
    /// doing nothing quietly.
    pub(crate) fn run_search(&mut self, pattern: &str, direction: Direction) {
        let ignore_case = self.search_ignore_case(pattern);
        // Using a search brings the highlighting back: `:nohlsearch` is about
        // the display, not about the pattern.
        self.search_highlight_on();
        if pattern.is_empty() {
            if self.search.is_empty() {
                self.message = "E35: No previous search pattern".to_string();
                return;
            }
        } else {
            match self
                .search
                .run(&self.buffer, pattern, direction, ignore_case)
            {
                // A pattern that does not compile is not a search that found
                // nothing: the two answers are different, so they say so.
                Err(error) => {
                    self.message = format!("E54: {error}");
                    return;
                }
                Ok(0) => {
                    self.message = format!("E486: Pattern not found: {pattern}");
                    return;
                }
                Ok(_) => {}
            }
        }

        // A search from the cursor stays on a match the cursor is already in
        // (`n` is what moves on); past the last one it wraps, the way vim does.
        //
        // On a closed fold it starts *past* it, which is the manual's rule: "when
        // starting a search in a closed fold it will not find a match in the current
        // fold — a forward search always starts from the end of the closed fold, while
        // a backwards search starts from the start of the closed fold".
        let from = self.search_start(direction);
        let wrap = self.options.borrow().wrapscan;
        let found = self
            .search
            .match_at(from)
            .or_else(|| self.search.next_from(from, direction, wrap));
        match found {
            Some((start, _)) if start != from => {
                // A search that moves the cursor is a jump: `''` comes back here.
                self.record_jump();
                self.goto_match(start);
            }
            Some((start, _)) => self.goto_match(start),
            None => self.message = format!("E486: Pattern not found: {}", self.search.pattern()),
        }
    }

    /// Where a search starts from: the cursor, or past a closed fold the cursor is on.
    pub(super) fn search_start(&self, direction: Direction) -> usize {
        let row = self.cursor.row;
        let fold = (0..self.folds.len()).find(|index| {
            let fold = self.folds[*index];
            self.fold_is_shut(*index) && row >= fold.start && row <= fold.end
        });
        match (fold, direction) {
            (Some(index), Direction::Forward) => {
                let end = self.folds[index].end + 1;
                let last = self.buffer.line_count().saturating_sub(1);
                self.buffer.line_col_to_char(end.min(last), 0)
            }
            (Some(index), Direction::Backward) => {
                self.buffer.line_col_to_char(self.folds[index].start, 0)
            }
            _ => self.cursor.char_idx(&self.buffer),
        }
    }

    /// Move the cursor to the start of a match, and put it on screen.
    pub(super) fn goto_match(&mut self, start: usize) {
        self.cursor.set_char_idx(&self.buffer, start);
        self.after_edit();
    }

    /// Whether a search for `pattern` ignores case: `'ignorecase'`, unless
    /// `'smartcase'` and an upper case letter in the pattern take it back.
    pub(crate) fn search_ignore_case(&self, pattern: &str) -> bool {
        let options = self.options.borrow();
        crate::search::ignore_case_for(pattern, options.ignorecase, options.smartcase)
    }

    /// Whether the matches of the search are drawn: `'hlsearch'` unless
    /// `:nohlsearch` has turned them off since the last search.
    pub(crate) fn search_is_highlighted(&self) -> bool {
        self.options.borrow().hlsearch && !self.nohlsearch
    }

    /// `:nohlsearch`: stop drawing the matches until a search is used again.
    ///
    /// The setting is not touched — that is the difference from
    /// `:set nohlsearch`, and the reason both exist.
    pub(crate) fn search_highlight_off(&mut self) {
        self.nohlsearch = true;
        self.redraw = true;
    }

    /// A search was used: the highlighting comes back, whatever `:nohlsearch`
    /// said.
    pub(crate) fn search_highlight_on(&mut self) {
        self.nohlsearch = false;
    }

    /// Show where the pattern being typed matches — `'incsearch'`.
    ///
    /// The search is run for real, so what is highlighted is the pattern being
    /// typed rather than the last one's, and the search that was in force is
    /// kept beside it for a cancellation to put back.
    ///
    /// Every keystroke searches *from where the search started* again, which is
    /// vim's rule and the reason typing more of a pattern keeps the cursor on
    /// the first match from there instead of walking through the matches.
    pub(super) fn preview_search(&mut self) {
        let Some(direction) = self.prompt.direction() else {
            return;
        };
        if !self.options.borrow().incsearch {
            return;
        }
        // A preview is a search being used, so it brings the highlighting back.
        if !self.cmdline.is_empty() {
            self.search_highlight_on();
        }
        let start = match &self.incsearch {
            Some(preview) => preview.start,
            None => {
                let previous = (!self.search.is_empty())
                    .then(|| (self.search.pattern().to_string(), self.search.direction()));
                let start = self.cursor;
                self.incsearch = Some(IncSearch {
                    start,
                    previous,
                    showing: false,
                });
                start
            }
        };
        let pattern = self.cmdline.clone();
        let ignore_case = self.search_ignore_case(&pattern);
        // A pattern that does not compile, or one that matches nothing, shows
        // nothing and says nothing: this is not the search yet, and a `:` line
        // that complained on every keystroke would be unusable.
        if pattern.is_empty() {
            self.search.clear();
            self.cursor = start;
            self.showing(false);
            return;
        }
        if self
            .search
            .run(&self.buffer, &pattern, direction, ignore_case)
            .is_err()
        {
            self.search.clear();
            self.cursor = start;
            self.showing(false);
            return;
        }
        if self.search.matches().is_empty() {
            self.cursor = start;
            self.showing(false);
            return;
        }
        // The search starts where the prompt was opened, not where the last
        // preview left the cursor.
        let here = self.cursor;
        self.cursor = start;
        let from = self.search_start(direction);
        self.cursor = here;
        let wrap = self.options.borrow().wrapscan;
        let found = self
            .search
            .match_at(from)
            .or_else(|| self.search.next_from(from, direction, wrap));
        match found {
            Some((at, _)) => {
                self.goto_match(at);
                self.showing(true);
            }
            None => {
                self.cursor = start;
                self.showing(false);
            }
        }
    }

    /// Move the preview on to the next match (`<C-g>`) or the previous one
    /// (`<C-t>`), while the pattern is still being typed.
    pub(super) fn preview_next(&mut self, direction: Direction) {
        // `is_none_or` is newer than this workspace's MSRV; `is_some_and` is not.
        if !self
            .incsearch
            .as_ref()
            .is_some_and(|preview| preview.showing)
        {
            return;
        }
        let here = self.search.match_at(self.cursor.char_idx(&self.buffer));
        let wrap = self.options.borrow().wrapscan;
        if let Some((at, _)) = here.and_then(|(at, _)| self.search.next_from(at, direction, wrap)) {
            self.goto_match(at);
        }
    }

    /// Note whether the preview has a match on screen, which is what the
    /// drawing needs to know: the cursor belongs to the match, not to the line
    /// being typed.
    pub(super) fn showing(&mut self, showing: bool) {
        if let Some(preview) = self.incsearch.as_mut() {
            preview.showing = showing;
        }
    }

    /// Forget the preview of a search, if one is up — the line it belonged to is
    /// gone. The cursor is left where it is: this is for a prompt that is being
    /// opened, not one that is being cancelled.
    pub(crate) fn forget_preview(&mut self) {
        self.incsearch = None;
    }

    /// Put the cursor back where the preview's search started and forget the
    /// preview.
    ///
    /// `keep` is whether the preview's search stands: it does when the line is
    /// accepted, and does not when it is cancelled — a cancelled search puts
    /// back the one that was in force, so `n` still walks what it walked.
    pub(super) fn end_preview(&mut self, keep: bool) {
        let Some(preview) = self.incsearch.take() else {
            return;
        };
        self.cursor = preview.start;
        if keep {
            return;
        }
        match preview.previous {
            Some((pattern, direction)) => {
                let ignore_case = self.search_ignore_case(&pattern);
                let _ = self
                    .search
                    .run(&self.buffer, &pattern, direction, ignore_case);
            }
            None => self.search.clear(),
        }
    }
}
