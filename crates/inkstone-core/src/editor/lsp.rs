use std::path::Path;

use crate::completion::{self, Item as CompletionItem};
use crate::diagnostic::Diagnostic;
use crate::mode::Mode;

use super::*;

impl Editor {
    /// Open the completion list on the words in this buffer, move along it, or
    /// filter what is up.
    ///
    /// `step` is `Some` for `<C-n>` and `<C-p>`, which move the pick and *type*
    /// it — vim's completion types as it goes, and the list is a thing you look
    /// at while typing rather than a menu you confirm afterwards. `None` means a
    /// keystroke happened, and what is in the buffer now is what filters.
    ///
    /// The words are gathered once, when the list opens: a popup that rescanned
    /// the whole file on every keystroke would be a popup nobody could type
    /// through.
    pub fn complete(&mut self, step: Option<isize>) {
        let at = self.cursor.char_idx(&self.buffer);
        let (_, prefix) = completion::prefix_at(&self.buffer, at);
        // A session with a language server may have better words than the file
        // does. It is asked every time the list is opened or moved, and the
        // answer replaces this list when it comes.
        if step.is_some() {
            self.lsp_ask = Some(Ask::Completion {
                row: self.cursor.row,
                col: self.cursor.col,
            });
        }
        match (self.completion.as_ref().is_some(), step) {
            (false, None) => return,
            (false, Some(_)) => self.open_completion(),
            (true, None) => self.filter_completion(&prefix),
            (true, Some(step)) => self.step_completion(step),
        }
        self.redraw = true;
    }

    /// Gather the words, put the list up, and type the first of them.
    pub(super) fn open_completion(&mut self) {
        let at = self.cursor.char_idx(&self.buffer);
        let (start, prefix) = completion::prefix_at(&self.buffer, at);
        let mut all = completion::words(&self.buffer, self.cursor.row, COMPLETION_LIMIT);
        // The word being typed is not a completion of itself — but an empty
        // prefix is not a word, and `<C-n>` on a space offers everything, which
        // is how a person sees what is in a file.
        if !prefix.is_empty() {
            all.retain(|item| item.label != prefix);
        }
        let shown = completion::matching(&all, &prefix);
        if shown.is_empty() {
            return;
        }
        self.completion = Some(Completion {
            start,
            typed: prefix.clone(),
            prefix,
            all,
            shown,
            selected: 0,
            inserted: false,
        });
        // Everything the list types belongs to one step of undo, the way an
        // insert session does.
        self.history.begin_group();
        // The first item, typed: the first one of these is the one that is
        // already picked, so the pick does not move.
        self.step_completion(0);
    }

    /// Move the pick by `step`, and type what is picked.
    pub(super) fn step_completion(&mut self, step: isize) {
        let Some(state) = self.completion.as_ref() else {
            return;
        };
        let count = state.shown.len() as isize;
        if count == 0 {
            return;
        }
        let selected = (state.selected as isize + step).rem_euclid(count) as usize;
        let item = state.shown[selected].clone();
        let (typed, stops) = if item.snippet {
            completion::snippet(&item.insert)
        } else if item.insert.is_empty() {
            (item.label.clone(), Vec::new())
        } else {
            (item.insert.clone(), Vec::new())
        };
        // A snippet's stops are ranges in the buffer once it is typed, which is
        // where they are kept until the session ends or the last one is passed.
        self.snippet_stops = stops
            .iter()
            .map(|stop| {
                let (start, end) = stop.span;
                completion::Stop {
                    span: (state.start + start, state.start + end),
                    choices: stop.choices.clone(),
                    number: stop.number,
                    mirror: stop.mirror,
                }
            })
            .collect();
        self.snippet_choice = 0;
        // Nothing has been stepped to yet: the cursor has just typed the text.
        self.snippet_step = 0;
        let start = state.start;
        let at = self.cursor.char_idx(&self.buffer);
        // What is replaced is the word being completed and nothing else: the
        // text after the cursor belongs to the line, not to the completion.
        self.replace_range(start, at, &typed, Some(start + typed.chars().count()));
        if let Some(state) = self.completion.as_mut() {
            state.selected = selected;
            state.prefix = typed;
            state.inserted = true;
        }
        // The item the list just moved to is the one worth knowing more about:
        // a server is asked for its details lazily, one item at a time.
        self.completion_resolve = Some(item.label);
        self.after_edit();
    }

    /// A keystroke happened: filter by what is in front of the cursor, and put
    /// the list away when nothing is left to offer.
    pub(super) fn filter_completion(&mut self, prefix: &str) {
        let Some(state) = self.completion.as_mut() else {
            return;
        };
        state.prefix = prefix.to_string();
        state.shown = completion::matching(&state.all, prefix);
        state.selected = state.selected.min(state.shown.len().saturating_sub(1));
        if state.shown.is_empty() {
            // Nothing matches what has been typed. The text stays: it is what
            // the person wrote, and being wrong about a word is not a reason to
            // take it away.
            self.close_completion();
        }
    }

    /// Put the list away, keeping whatever it has typed.
    pub(super) fn close_completion(&mut self) {
        if self.completion.take().is_some() {
            self.history.end_group(&mut self.buffer);
            self.redraw = true;
        }
    }

    /// The question that is waiting, if one is: a session that has a language
    /// server takes it and asks.
    pub fn take_lsp_ask(&mut self) -> Option<Ask> {
        self.lsp_ask.take()
    }

    /// Keep what the list has typed, and put it away. This is `<C-y>`, and with
    /// vim's completion it is usually nothing more than "stop".
    pub fn accept_completion(&mut self) {
        self.close_completion();
    }

    /// Step to the next place a snippet wants something typed.
    ///
    /// The stops were recorded when the snippet was typed, as ranges in the
    /// buffer, and `snippet_step` counts how many have been stepped to — so a
    /// fresh snippet is at zero and `<Tab>` goes to the first. Past the last
    /// stop there is nothing to step to, the session is over, and the key means
    /// what it always meant.
    pub fn snippet_step(&mut self, forward: bool) -> bool {
        if self.snippet_stops.is_empty() {
            return false;
        }
        // A place written twice is stepped to once: the first writing is where
        // it is typed, and the others follow it.
        let wanted: Vec<usize> = self
            .snippet_stops
            .iter()
            .enumerate()
            .filter(|(_, stop)| !stop.mirror)
            .map(|(index, _)| index)
            .collect();
        let position = wanted
            .iter()
            .position(|index| *index + 1 == self.snippet_step);
        let step = if forward {
            match position {
                Some(at) if at + 1 < wanted.len() => wanted[at + 1],
                Some(_) => {
                    self.end_snippet();
                    return false;
                }
                // Nothing has been stepped to: the first place is the next one.
                None if self.snippet_step == 0 => wanted[0],
                None => return false,
            }
        } else {
            match position {
                Some(at) if at > 0 => wanted[at - 1],
                _ => return false,
            }
        };
        let next = step;
        self.snippet_step = next + 1;
        self.select_stop(next);
        self.after_edit();
        true
    }

    /// Type the next thing the stop the cursor is on allows.
    ///
    /// A stop written `${1|one,two|}` offers both; the one in the text is the
    /// current choice, and picking another replaces it in place. The stops after
    /// it move by however much the text grew or shrank, which is the whole
    /// complication: they are ranges, and a range is only true until the text in
    /// front of it changes.
    pub fn snippet_choose(&mut self, forward: bool) -> bool {
        if self.snippet_stops.is_empty() {
            return false;
        }
        // The stop the cursor is in: which one that is comes from where it is
        // rather than from how many have been stepped to, so that a cursor moved
        // into a snippet by hand picks from the place it is looking at.
        let at = self.cursor.char_idx(&self.buffer);
        let Some(current) = self
            .snippet_stops
            .iter()
            .rposition(|stop| stop.span.0 <= at)
        else {
            return false;
        };
        let Some(stop) = self.snippet_stops.get(current).cloned() else {
            return false;
        };
        if stop.choices.len() < 2 {
            return false;
        }
        let count = stop.choices.len();
        let choice = if forward {
            (self.snippet_choice + 1) % count
        } else {
            (self.snippet_choice + count - 1) % count
        };
        let text = stop.choices[choice].clone();
        let (start, end) = stop.span;
        if start > self.buffer.len_chars() || end > self.buffer.len_chars() {
            // The text is not where it was: the session is over rather than
            // rewriting something the person is working on.
            self.end_snippet();
            return false;
        }
        // The replace goes through the same path every edit does, which is what
        // keeps the stop's own range and the ones after it true — the delta
        // arithmetic lives in one place, and this is not it.
        self.replace_range(start, end, &text, Some(start + text.chars().count()));
        // What was picked stays selected, so that typing over it picks again.
        self.select_stop(current);
        self.snippet_choice = choice;
        self.after_edit();
        true
    }

    /// Make every other writing of a stop say what the stop says.
    ///
    /// Called for every edit, and does nothing unless one is being typed into a
    /// stop that has mirrors: the text of the stop as it now is goes into each
    /// of them, and the ranges after each of them move by the difference. The
    /// mirrors' own edits come back through here, which is what the flag is for.
    pub(super) fn sync_snippet_mirrors(&mut self, start: usize, end: usize, written: usize) {
        if self.snippet_mirrors_busy || self.snippet_stops.is_empty() {
            return;
        }
        let Some(current) = self.snippet_stops.iter().position(|stop| {
            !stop.mirror && stop.span.0 <= start && start <= stop.span.1.max(stop.span.0)
        }) else {
            return;
        };
        // The stop being typed into is as long as the text in it *now*: the edit
        // that just happened is the newest part of it, and the range is stale
        // until this says otherwise.
        let delta = written as isize - end.saturating_sub(start) as isize;
        if delta != 0 {
            let stop = &mut self.snippet_stops[current];
            stop.span.1 = (stop.span.1 as isize + delta).max(stop.span.0 as isize) as usize;
            // And every range after it moves with it.
            let shift = |value: usize| (value as isize + delta).max(0) as usize;
            let (heading, rest) = self.snippet_stops.split_at_mut(current + 1);
            let _ = heading;
            for stop in rest.iter_mut() {
                stop.span = (shift(stop.span.0), shift(stop.span.1));
            }
        }
        let number = self.snippet_stops[current].number;
        let text = self.buffer.slice_text(
            self.snippet_stops[current].span.0,
            self.snippet_stops[current].span.1,
        );
        let mirrors: Vec<usize> = self
            .snippet_stops
            .iter()
            .enumerate()
            .filter(|(index, stop)| *index != current && stop.mirror && stop.number == number)
            .map(|(index, _)| index)
            .collect();
        if mirrors.is_empty() {
            return;
        }
        let old_len = self.snippet_stops[current]
            .span
            .1
            .saturating_sub(self.snippet_stops[current].span.0);
        let delta = text.chars().count() as isize - old_len as isize;
        self.snippet_mirrors_busy = true;
        for index in mirrors {
            let (from, to) = self.snippet_stops[index].span;
            if from > self.buffer.len_chars() || to > self.buffer.len_chars() {
                continue;
            }
            self.replace_range(from, to, &text, None);
            self.snippet_stops[index].span = (from, from + text.chars().count());
            if delta != 0 {
                let shift = |value: usize| (value as isize + delta).max(0) as usize;
                for stop in self.snippet_stops.iter_mut().skip(index + 1) {
                    stop.span = (shift(stop.span.0), shift(stop.span.1));
                }
            }
        }
        self.snippet_mirrors_busy = false;
    }

    /// Forget a snippet session: leaving insert mode ends it.
    pub fn end_snippet(&mut self) {
        self.snippet_stops.clear();
        self.snippet_step = 0;
        self.snippet_choice = 0;
        // A selection that was only there for the snippet goes with it.
        if self.mode == Mode::Insert {
            self.selection = None;
        }
    }

    /// Put the list away and put back what was typed before it opened. This is
    /// `<C-e>`, the difference between "no thanks" and "not that one".
    pub fn cancel_completion(&mut self) {
        let Some(state) = self.completion.as_ref() else {
            return;
        };
        let (start, typed) = (state.start, state.typed.clone());
        let at = self.cursor.char_idx(&self.buffer);
        if at > start {
            self.replace_range(start, at, &typed, Some(start + typed.chars().count()));
            self.after_edit();
        }
        self.close_completion();
    }

    /// Offer these items instead of what the buffer has. A language server's
    /// answer, arriving late, replaces whatever the popup is showing.
    pub fn set_completions(&mut self, items: Vec<CompletionItem>) {
        // Put back what was typed before this list started writing. A server is
        // answering about the word as it was asked about, and a local pick that
        // has already been typed into the buffer would filter the answer away —
        // the answer is better than the guess, so the guess goes back.
        if let Some(state) = self.completion.as_ref() {
            let (start, typed) = (state.start, state.typed.clone());
            let at = self.cursor.char_idx(&self.buffer);
            if at > start {
                self.replace_range(start, at, &typed, Some(start + typed.chars().count()));
                self.after_edit();
            }
        }
        let at = self.cursor.char_idx(&self.buffer);
        let (start, prefix) = completion::prefix_at(&self.buffer, at);
        let shown = completion::matching(&items, &prefix);
        if shown.is_empty() {
            return;
        }
        self.completion = Some(Completion {
            start,
            typed: prefix.clone(),
            prefix,
            all: items,
            shown,
            selected: 0,
            inserted: false,
        });
        self.history.begin_group();
        self.step_completion(1);
        self.redraw = true;
    }

    /// The item a language server is being asked about, taken by the session.
    ///
    /// A completion answer gives a label and little else; the details — what the
    /// item is, what it does — come from a second question about the one item a
    /// person has actually selected.
    pub fn take_completion_resolve(&mut self) -> Option<String> {
        self.completion_resolve.take()
    }

    /// Fill in what a server said about one item.
    ///
    /// The list holds two copies of an item — every one that arrived, and the
    /// ones still matching what has been typed — and both are the same item, so
    /// both are told.
    pub fn set_completion_detail(&mut self, label: &str, detail: &str) {
        let Some(state) = self.completion.as_mut() else {
            return;
        };
        let mut found = false;
        for item in state.all.iter_mut().chain(state.shown.iter_mut()) {
            if item.label == label && item.detail != detail {
                item.detail = detail.to_string();
                found = true;
            }
        }
        if found {
            self.redraw = true;
        }
    }

    /// Ask what the thing under the cursor is, and where it is defined: a
    /// question the kernel cannot answer by itself.
    pub fn ask_server(&mut self, ask: Ask) {
        self.lsp_ask = Some(ask);
        self.message = "asking the language server...".to_string();
        self.redraw = true;
    }

    /// Go to a place in a file: open it if it is not open, and put the cursor
    /// on the position.
    ///
    /// A position from a server is a row and a column in characters by the time
    /// it gets here — the conversion happens where the protocol does.
    pub fn goto_location(
        &mut self,
        path: &Path,
        row: usize,
        col: usize,
    ) -> Result<(), BufferError> {
        if self.buffer.path().map(|open| open != path).unwrap_or(true) {
            let open = self.documents.iter().position(|document| {
                document
                    .buffer
                    .path()
                    .map(|open| open == path)
                    .unwrap_or(false)
            });
            match open {
                Some(index) => {
                    self.show_document(index);
                }
                None => self.open_file(path)?,
            }
        }
        let row = row.min(self.buffer.line_count().saturating_sub(1));
        let col = col.min(self.buffer.line_len(row));
        let at = self.buffer.line_col_to_char(row, col);
        self.cursor.set_char_idx(&self.buffer, at);
        self.clamp_cursor();
        self.after_edit();
        Ok(())
    }

    /// Replace what a language server has to say about this document.
    ///
    /// The kernel does not care where they came from: a client translates the
    /// protocol's positions into rows and character columns and hands them over.
    /// They are sorted, because that is what `]d` and `[d` walk.
    pub fn set_diagnostics(&mut self, diagnostics: Vec<Diagnostic>) {
        self.diagnostics = diagnostics;
        self.diagnostics.sort_by_key(diagnostic_order);
        self.redraw = true;
    }

    /// The diagnostic under the cursor, if one covers its line.
    pub fn diagnostic_here(&self) -> Option<&Diagnostic> {
        let row = self.cursor.row;
        let col = self.cursor.col;
        self.diagnostics
            .iter()
            .find(|diagnostic| match diagnostic.on_row(row) {
                Some((from, to)) => col >= from && col < to.max(from + 1),
                None => false,
            })
            .or_else(|| {
                // Nothing under the cursor: the first one on its line, so that
                // being on the line is enough to be told about it.
                self.diagnostics
                    .iter()
                    .find(|diagnostic| diagnostic.on_row(row).is_some())
            })
    }

    /// Go to the next or previous diagnostic, wrapping around, and say what it
    /// is when we get there.
    pub fn step_diagnostic(&mut self, forward: bool) -> bool {
        if self.diagnostics.is_empty() {
            self.message = "No diagnostics".to_string();
            return false;
        }
        let here = self.cursor.char_idx(&self.buffer);
        let row = self.cursor.row;
        let found = if forward {
            self.diagnostics
                .iter()
                .find(|diagnostic| diagnostic.start.0 > row)
                .or_else(|| self.diagnostics.first())
        } else {
            self.diagnostics
                .iter()
                .rev()
                .find(|diagnostic| diagnostic.start.0 < row)
                .or_else(|| self.diagnostics.last())
        };
        let Some(diagnostic) = found.cloned() else {
            return false;
        };
        let _ = here;
        let at = self
            .buffer
            .line_col_to_char(diagnostic.start.0.min(self.buffer.line_count() - 1), 0);
        self.cursor.set_char_idx(&self.buffer, at);
        self.clamp_cursor();
        self.message = format!("{}: {}", diagnostic.severity.name(), diagnostic.message);
        self.after_edit();
        true
    }
}
