use std::path::{Path, PathBuf};

use crate::buffer::Buffer;
use crate::highlight::Language;
use crate::mode::Mode;

use super::*;

impl Editor {
    /// Hand over the places the last answer named, and start the walk at the
    /// first: a list of somewhere to go is no use if it cannot be walked.
    pub fn set_places(&mut self, places: Vec<(PathBuf, usize, usize)>) {
        self.places = places;
        self.place_index = if self.places.is_empty() { 0 } else { 1 };
        self.remember_list(List::Places);
    }

    /// Note what is being shown, so that `:colder` can come back to it.
    ///
    /// The history holds the *lists*, not the windows: what a person wants to
    /// return to is the places a question named, and re-opening one is how they
    /// see it again. Ten of them is more than anybody walks back through.
    pub(super) fn remember_list(&mut self, kind: List) {
        self.list_history.truncate(self.list_index);
        self.list_history.push(ListSnapshot {
            kind,
            places: self.places.clone(),
        });
        if self.list_history.len() > 10 {
            self.list_history.remove(0);
        }
        self.list_index = self.list_history.len();
    }

    /// Show the list `step` older (`-1`) or newer (`+1`), if there is one.
    pub(crate) fn walk_list_history(&mut self, step: isize) -> bool {
        if self.list_history.is_empty() {
            self.message = "E776: No location list".to_string();
            return false;
        }
        let wanted = self.list_index as isize + step;
        if wanted < 1 || wanted > self.list_history.len() as isize {
            self.message = "E553: No more items".to_string();
            return false;
        }
        self.list_index = wanted as usize;
        let snapshot = self.list_history[self.list_index - 1].clone();
        self.places = snapshot.places.clone();
        self.place_index = if self.places.is_empty() { 0 } else { 1 };
        // The window that was showing the other one goes away first, so that
        // walking back and forth does not pile windows up.
        if self.list.is_some() {
            let window = self.current_window;
            self.close_window(window);
        }
        match snapshot.kind {
            List::Places => self.open_places_list(),
            List::Diagnostics => self.open_diagnostics_list(),
            List::Settings => self.open_settings_window(),
            List::Commands => self.open_history_window(false),
            List::Searches => self.open_history_window(true),
        }
        self.list_index = wanted as usize;
        self.message = format!("list {} of {}", self.list_index, self.list_history.len());
        true
    }

    /// Step through the places the last answer named, and go to the one that is
    /// reached. Past either end the walk stops rather than wrapping: a list has
    /// ends, and a person who walks off one wants to know.
    pub(crate) fn walk_places(&mut self, forward: bool, count: usize) -> bool {
        if self.places.is_empty() {
            self.message = "E776: No location list".to_string();
            return false;
        }
        let last = self.places.len();
        let mut index = self.place_index.max(1);
        for _ in 0..count {
            if forward {
                if index >= last {
                    self.message = "E553: No more items".to_string();
                    return false;
                }
                index += 1;
            } else {
                if index <= 1 {
                    self.message = "E553: No more items".to_string();
                    return false;
                }
                index -= 1;
            }
        }
        self.place_index = index;
        let (path, row, col) = self.places[index - 1].clone();
        let _ = self.goto_location(&path, row, col);
        self.message = format!("{index} of {last}: {}", self.places[index - 1].0.display());
        true
    }

    /// Come back to the selection the last visual mode had.
    pub(crate) fn reselect(&mut self) -> bool {
        let Some((selection, cursor)) = self.last_visual else {
            return false;
        };
        self.selection = Some(selection);
        self.cursor = cursor;
        self.mode = Mode::Visual;
        self.clamp_normal_cursor();
        true
    }

    /// Open the list of places in a window of its own, the way a quickfix list
    /// is shown.
    ///
    /// The window holds a document whose lines *are* the places, in order, so the
    /// line the cursor is on names the place to go to — which is what `<CR>`
    /// reads. It is a document like any other, which is why nothing new was
    /// needed to draw it; what is new is that it is not for editing.
    pub fn open_places_list(&mut self) {
        self.open_places_list_where(None)
    }

    /// The same, with only the lines a pattern matches.
    ///
    /// A list with two hundred places in it is a list nobody reads; the filter is
    /// what makes one usable. The lines that survive keep their *places* — the
    /// targets are filtered with them, since a list and where its lines go are one
    /// thing.
    pub(crate) fn open_places_list_where(&mut self, filter: Option<&str>) {
        if self.places.is_empty() {
            self.message = "E776: No location list".to_string();
            return;
        }
        let Some(keep) = self.filter_rows(filter) else {
            return;
        };
        let mut text = String::new();
        let mut targets = Vec::new();
        let mut shown = 0;
        for (index, (path, row, col)) in self.places.iter().enumerate() {
            let line = format!(
                "{}:{}:{}  {}",
                path.display(),
                row + 1,
                col + 1,
                path.file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default()
            );
            if !keep(index, &line) {
                continue;
            }
            shown += 1;
            text.push_str(&format!("{shown} {line}\n"));
            targets.push((path.clone(), *row, *col));
        }
        let count = targets.len();
        if count == 0 {
            self.message = format!("Pattern not found: {}", filter.unwrap_or("").trim());
            return;
        }
        let mut document = Document::new(Buffer::from_text(&text), Language::Plain);
        document.list = Some(List::Places);
        document.list_targets = targets;
        self.documents.push(document);
        let index = self.documents.len() - 1;
        self.split_window(Axis::Rows, Some(index));
        self.message = format!("{count} place{}", if count == 1 { "" } else { "s" });
    }

    /// Show what has been typed on the `:` line (or searched for) in a window of
    /// its own — vim's `q:` and `q/`.
    ///
    /// The lines are the history as it is, oldest first, and `<CR>` runs the one
    /// the cursor is on: a command history is a list of things to *do again*,
    /// which is what makes it a list rather than a message.
    pub(crate) fn open_history_window(&mut self, searches: bool) {
        self.open_command_line_window(searches, "");
    }

    /// The same, with the line being typed as the last line of the window.
    ///
    /// "When the window opens it is filled with the command-line history. The last
    /// line contains the command as typed so far" — which is what `'cedit'` opens
    /// the window for.
    pub(crate) fn open_command_line_window(&mut self, searches: bool, draft: &str) {
        // "Once a command-line window is open it is not possible to open another
        // one": there is no nesting, which vim says with `E1292`.
        if self.history_window().is_some() {
            self.message = "E1292: Command-line window is already open".to_string();
            return;
        }
        let history = if searches {
            &self.search_history
        } else {
            &self.command_history
        };
        if history.is_empty() {
            self.message = if searches {
                "E776: No search history".to_string()
            } else {
                "E776: No command history".to_string()
            };
            return;
        }
        let count = history.len() + usize::from(!draft.is_empty());
        let mut text: String = history.iter().map(|line| format!("{line}\n")).collect();
        if !draft.is_empty() {
            text.push_str(draft);
            text.push('\n');
        }
        let mut document = Document::new(Buffer::from_text(&text), Language::Plain);
        document.list = Some(if searches {
            List::Searches
        } else {
            List::Commands
        });
        // No places to walk to: what `<CR>` does here is *run* the line, which is
        // not a place in a file.
        document.list_targets = Vec::new();
        self.documents.push(document);
        let index = self.documents.len() - 1;
        self.split_window(Axis::Rows, Some(index));
        // "Number of screen lines to use for the command-line window": the split
        // picks a size of its own, and the setting is what says how tall the
        // window a person is going to read should be.
        // The setting counts *screen* lines — the window's rows with its status
        // line, vim's own arithmetic — and a resize sets the text rows, so the
        // status line is taken off here.
        let height = self
            .options
            .borrow()
            .cmdwinheight
            .0
            .max(2)
            .min(u16::MAX as usize) as u16;
        self.resize_window(Axis::Rows, Resize::Set(height - 1));
        // The prompt is done with: the window is a window now, and the line that
        // was being typed is the last line in it.
        if !draft.is_empty() {
            self.mode = Mode::Normal;
            self.cmdline.clear();
            self.cmdline_cursor = 0;
            self.forget_recall();
        }
        self.message = format!("{count} command{}", if count == 1 { "" } else { "s" });
    }

    /// Which document is a command-line window, if one is up.
    ///
    /// The document the current window shows keeps its `list` in the *live* field
    /// — the slots and the editor swap it, like marks and the jump list — so the
    /// slot of the document that is up is the one that says nothing.
    pub(crate) fn history_window(&self) -> Option<usize> {
        let current = self.current_document();
        self.documents
            .iter()
            .enumerate()
            .position(|(index, document)| {
                let list = if index == current {
                    self.list
                } else {
                    document.list
                };
                list.is_some_and(List::is_editable)
            })
    }

    /// Whether the window that is up is a command-line window, which is the one
    /// state some commands are not allowed in.
    pub(crate) fn in_history_window(&self) -> bool {
        self.list.is_some_and(List::is_editable)
    }

    /// `<C-c>` in a command-line window: the line under the cursor becomes the
    /// line being typed, and the window stays where it is.
    pub(crate) fn continue_from_history_window(&mut self) {
        let searches = self.list == Some(List::Searches);
        let row = self.cursor.row;
        let line = self.buffer.line_string(row).trim_end().to_string();
        self.mode = Mode::Command;
        self.prompt = if searches {
            Prompt::SearchForward
        } else {
            Prompt::Command
        };
        self.cmdline = line;
        self.cmdline_cursor = self.cmdline.chars().count();
        self.forget_recall();
        self.clear_pending();
        self.redraw = true;
    }

    /// Run the line the cursor is on in a history window, and close it.
    ///
    /// The line is run as if it had been typed at the prompt it came from: a
    /// command for `q:`, a search for `q/`. What was edited but not run goes with
    /// the window — "other text edits are discarded when closing the window".
    pub(crate) fn run_history_line(&mut self) {
        let row = self.cursor.row;
        let line = self.buffer.line_string(row).trim().to_string();
        if line.is_empty() {
            return;
        }
        let searches = self.list == Some(List::Searches);
        let window = self.current_window;
        self.close_window(window);
        if searches {
            self.run_search(&line, crate::search::Direction::Forward);
        } else {
            self.execute_cmdline(&line);
        }
    }

    /// Show every setting and its value, in a window of its own.
    ///
    /// A list rather than one long message: there are more settings than fit on a
    /// line, and a person reading them wants to scroll. It is read-only for the
    /// same reason every other list is — a list that can be edited is a list that
    /// can lie about what it says. `:set {name}={value}` is how one is changed.
    pub(crate) fn open_settings_window(&mut self) {
        let mut text = String::new();
        for setting in crate::settings::Setting::ALL.iter().copied() {
            text.push_str(&format!(
                "{}\n",
                format_args!("{:<12} {}", setting.name(), self.option_text(setting))
            ));
        }
        let count = crate::settings::Setting::ALL.len();
        let mut document = Document::new(Buffer::from_text(&text), Language::Plain);
        document.list = Some(List::Settings);
        // No places to walk to: what `<CR>` would go to is a setting's name, and
        // there is nowhere to go with it.
        document.list_targets = Vec::new();
        self.documents.push(document);
        let index = self.documents.len() - 1;
        self.split_window(Axis::Rows, Some(index));
        self.message = format!("{count} settings");
    }

    /// The pattern a list filter keeps lines by, or `None` with the reason on the
    /// message row.
    ///
    /// The dialect and the case rule are a *search's*: `\(a\)` is a group, `\zs`
    /// means what it means to a search, and a `\c` or `\C` in the pattern settles
    /// case over `'ignorecase'` and `'smartcase'` — which is what `/`, `:g` and
    /// `:s` do, and what this documentation promised before the code did it.
    fn filter_pattern(&mut self, pattern: &str) -> Option<regex::Regex> {
        let translated = match crate::dialect::translate(pattern) {
            Ok(translated) => translated,
            Err(error) => {
                self.message = format!("E54: {error}");
                return None;
            }
        };
        let ignore_case = translated
            .ignore_case
            .unwrap_or_else(|| self.search_ignore_case(pattern));
        match regex::RegexBuilder::new(&translated.pattern)
            .case_insensitive(ignore_case)
            .build()
        {
            Ok(compiled) => Some(compiled),
            Err(error) => {
                self.message = format!("E54: {error}");
                None
            }
        }
    }

    /// Which lines a filter keeps, or everything when there is none.
    ///
    /// The pattern is the same dialect a search uses, and so is the case rule; a
    /// pattern that does not compile is reported rather than silently keeping
    /// everything, and one that matches nothing says so instead of opening an
    /// empty window.
    pub(super) fn filter_rows(&mut self, filter: Option<&str>) -> Option<RowFilter> {
        let Some(pattern) = filter.map(str::trim).filter(|p| !p.is_empty()) else {
            return Some(Box::new(|_, _| true));
        };
        // The match is remembered so that a pattern that finds nothing can say
        // so: the alternative is a window with no lines in it, which looks like a
        // bug rather than an answer.
        let matched = std::rc::Rc::new(std::cell::Cell::new(false));
        let compiled = self.filter_pattern(pattern)?;
        let seen = matched.clone();
        Some(Box::new(move |_, line| {
            let found = compiled.is_match(line);
            if found {
                seen.set(true);
            }
            found
        }))
    }

    /// Close the window that is showing the list, if that is what it shows.
    pub fn close_places_list(&mut self) {
        if self.list.is_none() {
            self.message = "E776: No location list".to_string();
            return;
        }
        let window = self.current_window;
        self.close_window(window);
    }

    /// Go to the place the cursor's line names, when the document on screen is a
    /// list of places.
    pub(crate) fn open_place_at_cursor(&mut self) {
        // A history window is a list of things to do again rather than places to
        // go to, so `<CR>` runs the line the cursor is on — vim's `q:`. "Works
        // both in Insert and in Normal mode", which is why this is a method of
        // its own rather than part of the normal-mode key handling.
        if self.list.is_some_and(List::is_editable) {
            self.run_history_line();
            return;
        }
        // A listing is a list of places too — the cursor's line names an entry,
        // and `<CR>` goes to it. That is the same gesture as a list of search
        // results, which is why it is the same key.
        if self.buffer.is_listing() {
            self.open_listing_entry();
            return;
        }
        if self.list.is_none() {
            // Anywhere else, `<CR>` is the motion it always was.
            self.run_command("move_down", &[]);
            return;
        }
        let row = self.cursor.row;
        let Some((path, line, col)) = self.list_targets.get(row).cloned() else {
            self.message = "E776: No location list".to_string();
            return;
        };
        // The window that shows the list goes away, the way a quickfix window
        // does, so that the place arrived at is not half a screen.
        let window = self.current_window;
        self.close_window(window);
        if path.as_os_str().is_empty() {
            // A list of what a server said about a file that has no name — an
            // unsaved buffer — has nowhere to go but here.
            self.cursor.row = line.min(self.buffer.line_count().saturating_sub(1));
            self.cursor.col = col;
            self.clamp_normal_cursor();
            return;
        }
        let _ = self.goto_location(&path, line, col);
    }

    /// `<CR>` in a listing: open what the line under the cursor names.
    ///
    /// A directory takes the listing's place, so that walking down and back up
    /// is one document rather than a pile of them. A file opens as a document of
    /// its own, so the listing it came from stays where `:b` finds it again.
    fn open_listing_entry(&mut self) {
        let Some(dir) = self.buffer.path().map(Path::to_path_buf) else {
            return;
        };
        let name = self.buffer.line_string(self.cursor.row);
        // An empty line names nothing: that is what a listing of an empty
        // directory holds, and what a line somebody is about to type into holds.
        if name.is_empty() {
            return;
        }
        // The trailing `/` is how a listing *says* "directory"; it is not part of
        // the name it says it about.
        let target = dir.join(name.strip_suffix('/').unwrap_or(&name));
        let opened = if target.is_dir() {
            self.load_file(&target)
        } else {
            self.open_file(&target)
        };
        if let Err(e) = opened {
            self.message = e.to_string();
        }
    }

    /// `-` in a listing: the listing of the directory above this one.
    ///
    /// Anywhere else the key is vim's motion, up one line. The parent of a path
    /// like `.` is the empty path rather than the directory above it, so the
    /// walk goes through what the directory really is — and through `tidy`, for
    /// the prefix Windows puts on a canonical path.
    pub(crate) fn up_directory(&mut self) {
        if !self.buffer.is_listing() {
            self.run_command("move_up", &[]);
            return;
        }
        let Some(dir) = self.buffer.path().map(Path::to_path_buf) else {
            return;
        };
        let parent = std::fs::canonicalize(&dir)
            .ok()
            .map(crate::command::tidy)
            .and_then(|here| here.parent().map(Path::to_path_buf));
        // The filesystem root has no parent, and `/` is already as far up as a
        // listing goes.
        let Some(parent) = parent else {
            return;
        };
        if let Err(e) = self.load_file(&parent) {
            self.message = e.to_string();
        }
    }

    /// Open what a language server said about this file in a window of its own.
    ///
    /// The same shape the list of places has — a document whose lines are the
    /// list — because a diagnostic is a line of text with a place attached, which
    /// is what a list line is.
    pub(crate) fn open_diagnostics_list(&mut self) {
        self.open_diagnostics_list_where(None)
    }

    /// The same, with only the lines a pattern matches — `:lopen warning` is the
    /// list of warnings.
    pub(crate) fn open_diagnostics_list_where(&mut self, filter: Option<&str>) {
        // Every document that is open, not only the one on screen: a server says
        // what it thinks about each file it has been told about, and a person
        // wants to know where the work is.
        let mut rows: Vec<(PathBuf, usize, usize, String)> = Vec::new();
        let current = self.current_document();
        for (index, document) in self.documents.iter().enumerate() {
            let diagnostics = if index == current {
                &self.diagnostics
            } else {
                &document.diagnostics
            };
            let path = document.buffer.path().map(Path::to_path_buf);
            for diagnostic in diagnostics {
                let name = path
                    .as_ref()
                    .and_then(|path| path.file_name())
                    .map(|name| name.to_string_lossy().to_string())
                    .unwrap_or_else(|| "[No Name]".to_string());
                rows.push((
                    path.clone().unwrap_or_default(),
                    diagnostic.start.0,
                    diagnostic.start.1,
                    format!(
                        "{name}:{}:{} {}",
                        diagnostic.start.0 + 1,
                        diagnostic.start.1 + 1,
                        diagnostic.line()
                    ),
                ));
            }
        }
        if rows.is_empty() {
            self.message = "No diagnostics".to_string();
            return;
        }
        if let Some(pattern) = filter.map(str::trim).filter(|p| !p.is_empty()) {
            let Some(compiled) = self.filter_pattern(pattern) else {
                return;
            };
            rows.retain(|row| compiled.is_match(&row.3));
            if rows.is_empty() {
                self.message = format!("Pattern not found: {pattern}");
                return;
            }
        }
        let count = rows.len();
        let files = {
            let mut paths: Vec<&PathBuf> = rows.iter().map(|row| &row.0).collect();
            paths.sort();
            paths.dedup();
            paths.len()
        };
        let mut text = String::new();
        for (index, (_, _, _, line)) in rows.iter().enumerate() {
            text.push_str(&format!("{} {line}\n", index + 1));
        }
        let mut document = Document::new(Buffer::from_text(&text), Language::Plain);
        document.list = Some(List::Diagnostics);
        // A diagnostic is about a place in a file, which is what `<CR>` goes to.
        document.list_targets = rows
            .into_iter()
            .map(|(path, row, col, _)| (path, row, col))
            .collect();
        self.documents.push(document);
        let index = self.documents.len() - 1;
        self.split_window(Axis::Rows, Some(index));
        self.message = format!(
            "{count} diagnostic{} in {files} file{}",
            if count == 1 { "" } else { "s" },
            if files == 1 { "" } else { "s" }
        );
    }

    /// The places the last answer named, as one line, for `:clist`.
    pub(crate) fn places_text(&self) -> String {
        if self.places.is_empty() {
            return "No location list".to_string();
        }
        let mut parts = vec![format!("{} places", self.places.len())];
        for (index, (path, row, _)) in self.places.iter().enumerate() {
            let here = if index + 1 == self.place_index {
                ">"
            } else {
                " "
            };
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_else(|| path.display().to_string());
            parts.push(format!("{here}{} {name}:{}", index + 1, row + 1));
        }
        parts.join("   ")
    }
}
