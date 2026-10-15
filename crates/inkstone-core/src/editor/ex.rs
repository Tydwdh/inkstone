use std::path::{Path, PathBuf};

use inkstone_rpc::Param;

use crate::buffer::Buffer;
use crate::mode::Mode;
use crate::register::Kind;
use crate::settings::Setting;
use crate::substitute::{Address, Substitute};

use super::*;

/// The built-in command a `:` name runs, or `None` when there is no such
/// name. A table rather than code: the names are data, and the one arm that
/// is not (`:set`, which has its own spelling rules) is handled before this.
fn command_name(cmd: &str) -> Option<&'static str> {
    Some(match cmd {
        "d" | "delete" => "delete_line",
        "marks" => "list_marks",
        "delmarks" => "delete_marks",
        "delmarks!" => "delete_all_marks",
        "jumps" => "list_jumps",
        "changes" => "list_changes",
        "symbols" => "list_symbols",
        "signature" => "signature_help",
        "cnext" | "cn" => "place_next",
        "cprev" | "cp" => "place_previous",
        "clist" => "place_list",
        "copen" => "open_places",
        "cclose" => "close_places",
        "colder" => "list_older",
        "cnewer" => "list_newer",
        "format" | "fmt" => "format_document",
        "wsymbols" | "ws" => "workspace_symbols",
        "lopen" => "open_diagnostics",
        "lclose" => "close_diagnostics",
        "clearjumps" => "clear_jumps",
        "kill" => "kill_command",
        "p" | "print" => "print_line",
        "w" => "save",
        "w!" => "save_force",
        "q" => "quit",
        "wq" | "x" => "save_quit",
        "q!" => "quit_force",
        "e" => "edit_file",
        "e!" => "edit_file_force",
        "ls" | "buffers" | "files" => "list_buffers",
        "b" | "buffer" => "buffer_go",
        "bn" | "bnext" => "buffer_next",
        "bp" | "bprevious" => "buffer_previous",
        "bd" | "bdelete" => "buffer_delete",
        "bn!" | "bnext!" => "buffer_next_force",
        "bp!" | "bprevious!" => "buffer_previous_force",
        "bd!" | "bdelete!" => "buffer_delete_force",
        "sp" | "split" => "split_rows",
        "vs" | "vsplit" => "split_columns",
        "on" | "only" => "window_only",
        "reg" | "registers" => "list_registers",
        "diagnostics" | "diags" => "list_diagnostics",
        "setdiag" => "set_diagnostics",
        // `set {name}={value}`, and `set {name}?` to ask. The options layer
        // already knows every name and every kind of value; this is the
        // shortest way to reach it from the `:` line.
        "mkview" => "make_view",
        "loadview" => "load_view",
        "tabnew" | "tabedit" => "tab_new",
        "tabclose" | "tabc" => "tab_close",
        "tabnext" | "tabn" => "tab_next",
        "tabprevious" | "tabp" => "tab_previous",
        "tabs" => "list_tabs",
        "resize" | "res" => "resize",
        "vresize" => "resize_vertical",
        "tabsplit" => "tab_split",
        "nohlsearch" | "noh" => "nohlsearch",
        "undolist" | "undol" => "undolist",
        "undo" | "u" => "undo",
        "redo" | "red" => "redo",
        "earlier" | "ea" => "earlier",
        "later" | "lat" => "later",
        "&" => "substitute_again",
        "&&" => "substitute_again_flags",
        _ => return None,
    })
}

impl Editor {
    /// Rewrap lines to `textwidth`, as one edit.
    ///
    /// The lines are joined and filled again, which is what vim's `gq` does
    /// without its comment handling: a word that is longer than the width is left
    /// on a line of its own rather than broken, and `textwidth = 0` is not a
    /// width at all — the command then does nothing, which is what "do not wrap"
    /// has to mean.
    pub(crate) fn format_lines(&mut self, from: usize, to: usize) -> usize {
        let last = self.buffer.line_count().saturating_sub(1);
        let (from, to) = (from.min(last), to.min(last));
        let (from, to) = (from.min(to), from.max(to));
        let width = self.options.borrow().textwidth.0;
        // What every line of the range has in front of it is what each line of
        // the rewrap gets in front of it: a paragraph of `// …` stays a
        // paragraph of `// …`, and a person reads what comes after the marker,
        // so the marker does not count towards the width.
        let leader = self.common_leader(from, to);
        let mut words: Vec<String> = Vec::new();
        for row in from..=to {
            let line = self.buffer.line_string(row);
            let rest = line.strip_prefix(leader.as_str()).unwrap_or(&line);
            words.extend(rest.split_whitespace().map(str::to_string));
        }
        if words.is_empty() || width == 0 {
            return to - from + 1;
        }
        let width = width.saturating_sub(leader.chars().count()).max(1);
        let mut lines: Vec<String> = Vec::new();
        let mut current = String::new();
        for word in words {
            if current.is_empty() {
                current = word;
                continue;
            }
            if current.chars().count() + 1 + word.chars().count() > width {
                lines.push(std::mem::take(&mut current));
                current = word;
            } else {
                current.push(' ');
                current.push_str(&word);
            }
        }
        lines.push(current);
        // A list keeps its marker on the first line and lines the rest up under
        // the text: `- one two` then spaces, not `- one two` then `- three`. The
        // marker is a bullet or a number, and what follows it is spaces of the
        // same width — which is what a person writing prose means by a list.
        // The marker may be in the shared leader, or — for a range of one line,
        // where nothing is shared — at the start of the line itself.
        let marker = Self::list_marker_width(&leader)
            .or_else(|| Self::list_marker_width(&self.buffer.line_string(from)));
        let continuation = match marker {
            Some(width) => " ".repeat(width),
            None => leader.clone(),
        };
        let lines: Vec<String> = lines
            .into_iter()
            .enumerate()
            .map(|(index, line)| {
                if index == 0 {
                    format!("{leader}{line}")
                } else {
                    format!("{continuation}{line}")
                }
            })
            .collect();
        let mut text = lines.join("\n");
        // The line break the range ended on belongs to the text after it, not to
        // the last line: a range that stopped before one does not gain one.
        let terminator = self.buffer.line_terminator(to);
        text.push_str(&terminator);
        let start = self.buffer.line_col_to_char(from, 0);
        let end = self.buffer.line_col_to_char(to, 0)
            + self.buffer.line_len(to)
            + self.buffer.line_terminator_len(to);
        let count = lines.len();
        self.replace_range(start, end, &text, Some(start));
        count
    }

    /// How wide a list marker is, if the leader is one: `- `, `* `, `+ `, `1. `.
    ///
    /// What a wrapped list line is indented by is the marker's *width*, since the
    /// marker belongs to the item and not to every line of it.
    pub(super) fn list_marker_width(leader: &str) -> Option<usize> {
        let trimmed = leader.trim_start();
        let indent = leader.chars().count() - trimmed.chars().count();
        let mut chars = trimmed.chars();
        match chars.next()? {
            '-' | '*' | '+' => chars.next().filter(|c| *c == ' ').map(|_| indent + 2),
            first if first.is_ascii_digit() => {
                let digits = trimmed.chars().take_while(char::is_ascii_digit).count();
                let after = trimmed.chars().nth(digits)?;
                let space = trimmed.chars().nth(digits + 1)?;
                (matches!(after, '.' | ')') && space == ' ').then_some(indent + digits + 2)
            }
            _ => None,
        }
    }

    /// What every line of a range has in front of it, and nothing when they do
    /// not agree.
    ///
    /// The leader is the longest run of spaces, markers and markers' spaces that
    /// all the lines share — `// `, `# `, `> `, or plain indentation — trimmed
    /// back to a space so that a leader cannot end in the middle of a word.
    pub(super) fn common_leader(&self, from: usize, to: usize) -> String {
        let lines: Vec<String> = (from..=to)
            .map(|row| self.buffer.line_string(row))
            .collect();
        let first = match lines.first() {
            Some(line) => line,
            None => return String::new(),
        };
        let mut len = first.chars().count();
        for line in &lines {
            let common = first
                .chars()
                .zip(line.chars())
                .take_while(|(a, b)| a == b && (*a == ' ' || !a.is_alphanumeric()))
                .count();
            len = len.min(common);
        }
        if len == 0 {
            return String::new();
        }
        let leader: String = first.chars().take(len).collect();
        // A leader that ends in a marker's space keeps it; one that ends in the
        // middle of anything else is cut back to its last space.
        if leader.ends_with(' ') {
            return leader;
        }
        match leader.rfind(' ') {
            Some(at) => leader[..=at].to_string(),
            None if lines.len() > 1 => String::new(),
            None => leader,
        }
    }

    /// The lines a substitute looks at: the range it named, or the one the
    /// cursor is on when it named none.
    ///
    /// Addresses are resolved against the buffer *now* — a line number in a `:`
    /// line means the line of the file as it is, which is how `:5s/a/b/` can be
    /// typed again after an edit moved that line.
    pub(super) fn substitute_rows(&self, spec: &Substitute) -> Vec<usize> {
        let last = self.buffer.line_count().saturating_sub(1);
        let resolve = |address: Address| self.resolve_address(address).min(last);
        match (spec.from, spec.to) {
            (None, _) => vec![self.cursor.row],
            (Some(from), Some(to)) => {
                let (from, to) = (resolve(from), resolve(to));
                let (from, to) = (from.min(to), from.max(to));
                (from..=to).collect()
            }
            // One address is one line: `:5s/a/b/` is line five in vim, and
            // `:.s` is the cursor's line rather than the rest of the file.
            (Some(from), None) => vec![resolve(from)],
        }
    }

    /// Replace what the pattern in `spec` finds.
    ///
    /// Returns how many places were replaced and how many lines that was — the
    /// two numbers a person wants back — or the reason the pattern did not
    /// compile.
    ///
    /// Every line that changes is put back in *one* replacement, spanning the
    /// first and the last of them: a substitute is one change, so one `u` undoes
    /// one, however many lines it touched. The lines between them ride along
    /// unchanged, which is the price of a single step and a small one.
    pub(crate) fn substitute(
        &mut self,
        spec: &Substitute,
    ) -> Result<(usize, usize), crate::search::PatternError> {
        let pattern = regex::RegexBuilder::new(&spec.translated)
            .case_insensitive(spec.ignore_case)
            .build()
            .map_err(crate::search::PatternError::from)?;

        let rows = self.substitute_rows(spec);

        // Which lines change, and what they become.
        let mut replacements: Vec<(usize, String)> = Vec::new();
        let mut count = 0;
        for row in rows {
            let line = self.buffer.line_string(row);
            // What is replaced is the part of a match the pattern reports,
            // which `\zs` and `\ze` can make shorter than the text it matched.
            let (replaced, found) = crate::substitute::replace_line(
                &pattern,
                &spec.extent,
                &spec.replacement,
                &line,
                spec.all,
            );
            if found == 0 || replaced == line.as_str() {
                continue;
            }
            count += found;
            replacements.push((row, replaced));
        }
        let Some((first, _)) = replacements.first() else {
            return Ok((0, 0));
        };
        let first = *first;
        let last = replacements.last().expect("not empty").0;

        let start = self.buffer.line_col_to_char(first, 0);
        let end = self.buffer.line_col_to_char(last, 0)
            + self.buffer.line_len(last)
            + self.buffer.line_terminator_len(last);
        let mut text = String::new();
        let mut cursor = start;
        let mut row = first;
        for (changed, replacement) in &replacements {
            // The lines that did not change keep their own text, terminator and
            // all: they are inside the range, not part of the change.
            while row < *changed {
                text.push_str(&self.buffer.line_string(row));
                text.push_str(&self.buffer.line_terminator(row));
                row += 1;
            }
            cursor = start + text.chars().count();
            text.push_str(replacement);
            text.push_str(&self.buffer.line_terminator(row));
            row += 1;
        }

        // The cursor ends on the last line the substitute changed, the way vim
        // leaves it.
        self.replace_range(start, end, &text, Some(cursor));
        Ok((count, replacements.len()))
    }

    /// The line an address names, in the text as it is now.
    ///
    /// A mark that was never set has no line of its own, and a range that names
    /// one falls back to the cursor's line rather than 0 or the end.
    pub(crate) fn resolve_address(&self, address: Address) -> usize {
        let last = self.buffer.line_count().saturating_sub(1);
        match address {
            Address::Line(number) => number.saturating_sub(1).min(last),
            Address::Current => self.cursor.row.min(last),
            Address::Last => last,
            Address::Mark(letter) => self
                .marks
                .get(letter)
                .map_or(self.cursor.row, |mark| mark.row)
                .min(last),
        }
    }

    /// The lines an address range covers, in order, and nothing when the line
    /// named none.
    ///
    /// The second address defaults to the first, which is what `:5d` means, and
    /// to the last line when the first is the cursor's — `:.` is one line, and
    /// vim's `:.` is the same. A `:` with no range at all is `None`.
    pub(crate) fn resolve_range(&self, line: &str) -> Option<(usize, usize)> {
        let (from, to) = crate::substitute::address_range(line);
        let from = from?;
        let from = self.resolve_address(from);
        let to = to.map_or(from, |to| self.resolve_address(to));
        Some((from.min(to), from.max(to)))
    }

    /// Read a file in below the cursor's line, and say how many lines it had.
    ///
    /// The text is inserted after the *whole* line the cursor is on — its
    /// terminator included, which is added if it was the last line and had none
    /// — so that what is read comes in as lines of its own rather than joined to
    /// what was there.
    pub(crate) fn read_file_below(&mut self, path: &Path) -> Result<usize, BufferError> {
        // Unlike `:e`, a file that is not there is an error: `:read` is about
        // what is in a file, and there is nothing in this one.
        if !path.exists() {
            return Err(BufferError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                path.display().to_string(),
            )));
        }
        let text = Buffer::from_path(path)?.text();
        let lines = text.lines().count();
        let row = self.cursor.row;
        let mut at = self.buffer.line_col_to_char(row, 0) + self.buffer.line_len(row);
        let terminator = self.buffer.line_terminator_len(row);
        let mut insert = String::new();
        if terminator == 0 {
            insert.push('\n');
        }
        at += terminator;
        insert.push_str(&text);
        // A file that ends without a line break would leave the next line joined
        // to it, so one is added — which is what reading a file into a line means.
        if !insert.ends_with('\n') {
            insert.push('\n');
        }
        self.replace_range(at, at, &insert, Some(at));
        Ok(lines)
    }

    /// Sort the lines an address range covers, and say how many there were.
    ///
    /// The range is rewritten as one edit — the lines' own terminators and all,
    /// so the file's line endings survive and the whole thing is one step to
    /// undo — and the cursor goes to the first line of it, which is where a
    /// person looks afterwards.
    pub(crate) fn sort_lines(&mut self, from: usize, to: usize, spec: &SortSpec) -> usize {
        let last = self.buffer.line_count().saturating_sub(1);
        let (from, to) = (from.min(last), to.min(last));
        let (from, to) = (from.min(to), from.max(to));
        let mut lines: Vec<String> = (from..=to)
            .map(|row| self.buffer.line_string(row))
            .collect();
        if spec.unique {
            // Equal by the same rule the sort uses, keeping the first of each.
            let mut seen: Vec<String> = Vec::new();
            lines.retain(|line| {
                let key = spec.key(line);
                if seen.contains(&key) {
                    false
                } else {
                    seen.push(key);
                    true
                }
            });
        }
        lines.sort_by_key(|line| spec.key(line));
        if spec.reverse {
            lines.reverse();
        }
        let count = lines.len();
        let mut text = String::new();
        for (index, line) in lines.iter().enumerate() {
            text.push_str(line);
            // The terminator of the line being replaced in this position, so a
            // file whose last line has none still has none.
            text.push_str(&self.buffer.line_terminator(from + index));
        }
        let start = self.buffer.line_col_to_char(from, 0);
        let end = self.buffer.line_col_to_char(to, 0)
            + self.buffer.line_len(to)
            + self.buffer.line_terminator_len(to);
        self.replace_range(start, end, &text, Some(start));
        count
    }

    /// Run a `:` command line.
    pub fn execute_cmdline(&mut self, line: &str) {
        let line = line.trim();
        if line.is_empty() {
            return;
        }
        if let Some(letter) = self.unset_range_mark(line) {
            self.message = format!("E20: Mark not set: {letter}");
            return;
        }
        // `:s/…/…/`, `:%s/…/…/g` and their friends: a substitute rather than a
        // command whose name happens to start with an `s`.
        if Substitute::looks_like(line) {
            // A substitute carries its range *in the line*, so this is where the
            // manual's rule has to be applied: "for Ex commands that operate on buffer
            // lines, the range is adjusted to always start at the first line of a closed
            // fold and end at the last line of a closed fold. Thus `:s/foo/bar/g` when
            // used with the cursor on a closed fold will replace 'foo' with 'bar' in all
            // lines of the fold."
            let line = self.substitute_line_with_fold_range(line);
            self.run_command("substitute", &[Param::Str(line)]);
            return;
        }
        // Likewise `:g/…/…` and `:v/…/…`, which `:goto` and `:vsplit` are not.
        if crate::global::Global::looks_like(line) {
            match crate::global::Global::parse(line) {
                Ok(spec) => self.run_global(&spec),
                Err(error) => self.message = error.to_string(),
            }
            return;
        }
        // And `:vimgrep`, whose list is the one `:g` and the language server's
        // answers fill as well. It is checked before the range below, because
        // the number in front of it is a count and not an address.
        if crate::vimgrep::Vimgrep::looks_like(line) {
            match crate::vimgrep::Vimgrep::parse(line) {
                Ok(spec) => self.run_vimgrep(&spec),
                Err(error) => self.message = error.to_string(),
            }
            return;
        }
        // An address or a range in front of a command that acts on lines: `:5d`,
        // `:'a,'bd`. Other commands ignore it, which is what vim does with a
        // range they have no use for — `:1,5w` writes the file, not five lines.
        let range = self.resolve_range(line);
        let line = crate::substitute::strip_range(line).unwrap_or(line);
        // `:!{cmd}` is the one command whose argument is the whole line — spaces
        // and all — which is why it cannot be read by the split below: `:!cargo
        // init` is a command line, not a command called `!cargo`.
        let (mut cmd, mut arg) = match line.strip_prefix('!') {
            Some(command) => ("!", command.trim()),
            None => match line.split_once(char::is_whitespace) {
                Some((cmd, arg)) => (cmd, arg.trim()),
                None => (line, ""),
            },
        };
        // `:tab split` and `:tab new` are two words for the same things as
        // `:tabsplit` and `:tabnew`.
        // `:vertical resize 10` is two words for `:verticalresize`, the same
        // way `:tab split` is two words for one command.
        if cmd == "vertical" {
            match arg.split_once(char::is_whitespace) {
                Some(("resize" | "res", rest)) => {
                    cmd = "vresize";
                    arg = rest.trim();
                }
                None if arg == "resize" || arg == "res" => {
                    cmd = "vresize";
                    arg = "";
                }
                _ => {}
            }
        }
        if cmd == "tab" {
            match arg.split_once(char::is_whitespace) {
                Some((sub, rest)) => {
                    cmd = match sub {
                        "split" => "tabsplit",
                        "new" | "edit" => "tabnew",
                        _ => "tab",
                    };
                    arg = rest.trim();
                }
                None if arg == "split" => {
                    cmd = "tabsplit";
                    arg = "";
                }
                None => {}
            }
        }
        let params = if arg.is_empty() {
            Vec::new()
        } else {
            vec![Param::Str(arg.to_string())]
        };
        // Commands that act on a range, or on the line exactly as typed, have a
        // handler of their own; the rest are a name and a parameter, and go
        // through the table below.
        if self.ex_write_append(cmd, arg, range)
            || self.ex_write_range(cmd, arg, range)
            || self.ex_read(cmd, arg, range)
            || self.ex_shell(cmd, arg, range)
            || self.ex_normal(cmd, line, range)
            || self.ex_format_range(cmd, range)
            || self.ex_sort(cmd, arg, range)
            || self.ex_delete_range(cmd, range)
        {
            return;
        }
        // `:set` and its two scoped spellings have spellings of their own —
        // `nonumber`, `number?`, `all`, `inv{name}` — so they are handled before
        // the table of names. The scope is the difference between them: the
        // manual's table says `:set` writes both values, `:setlocal` only the
        // file's own and `:setglobal` only the one outside every file.
        let scope = match cmd {
            "set" => Some(Scope::Both),
            "setlocal" | "setl" => Some(Scope::Local),
            "setglobal" | "setg" => Some(Scope::Global),
            _ => None,
        };
        if let Some(scope) = scope {
            self.ex_set(arg, scope);
            return;
        }

        let Some(name) = command_name(cmd) else {
            self.message = format!("E492: Not a command: {cmd}");
            return;
        };
        self.run_command(name, &params);
    }

    /// Run `:vimgrep`: every place a pattern matches in a set of files, into the
    /// places list.
    ///
    /// "Search for {pattern} in the files {file} ... and set the error list to
    /// the matches" — the list `:copen` shows, `<CR>` goes to a line of, and
    /// `:cnext` walks. Nothing is gone to unless the line asked for it: `j`
    /// leaves the cursor where it is, and without it the first place is where
    /// the cursor ends up, which is vim's rule.
    pub(crate) fn run_vimgrep(&mut self, spec: &crate::vimgrep::Vimgrep) {
        // `//` is the last pattern a search used. A `:vimgrep` does *not*
        // become that pattern itself: vim says so by keeping its own, which is
        // a thing a person notices only when they have typed both.
        let pattern = if spec.pattern.is_empty() {
            self.search.pattern().to_string()
        } else {
            spec.pattern.clone()
        };
        if pattern.is_empty() {
            self.message = "E35: No previous search pattern".to_string();
            return;
        }
        // "'smartcase' is not used" is the manual's own line about this command,
        // so only a `\c` or `\C` in the pattern and `'ignorecase'` are asked.
        // A fuzzy pattern is a literal string, so none of this applies to it and
        // there is nothing to compile.
        let ignore_case = spec
            .ignore_case
            .unwrap_or_else(|| self.options.borrow().ignorecase);
        let translated = match crate::dialect::translate(&pattern) {
            Ok(translated) => translated,
            Err(error) => {
                self.message = format!("E54: {error}");
                return;
            }
        };
        let compiled = match (
            spec.fuzzy,
            regex::RegexBuilder::new(&translated.pattern)
                .case_insensitive(ignore_case)
                .build(),
        ) {
            (true, _) => None,
            (false, Ok(compiled)) => Some(compiled),
            (false, Err(error)) => {
                self.message = format!("E54: {error}");
                return;
            }
        };

        let limit = spec.limit.unwrap_or(usize::MAX);
        let (ignore, suffixes) = {
            let options = self.options.borrow();
            (
                crate::command::split_list(&options.wildignore),
                crate::command::split_list(&options.suffixes),
            )
        };
        // Every file the arguments name, before any of them is read: "files in
        // 'suffixes' are searched last" is about the *list* and not about one
        // argument — the terminal showed a `.zz` named first still coming last.
        let named: Vec<PathBuf> = spec
            .files
            .iter()
            .flat_map(|argument| crate::command::files_named(argument, &ignore))
            .collect();
        let suffixed = |path: &Path| {
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            suffixes
                .iter()
                .any(|suffix| name.ends_with(suffix.as_str()))
        };
        let mut files: Vec<PathBuf> = named
            .iter()
            .filter(|path| !suffixed(path))
            .cloned()
            .collect();
        files.extend(named.iter().filter(|path| suffixed(path)).cloned());

        let mut places: Vec<(PathBuf, usize, usize)> = Vec::new();
        let mut searched = 0;
        for path in files {
            if places.len() >= limit {
                break;
            }
            // A file that cannot be read is skipped rather than reported: an
            // argument is a name, and a name that is not there is not a reason
            // for a search to stop. vim says the same thing in the end, with
            // "E480: No match" when nothing at all was found.
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let buffer = Buffer::from_text(&text);
            let mut asked = None;
            let mut here = 0;
            // Where the matches are, as character offsets. A fuzzy pattern has
            // one place a line — the best one — and nothing compiled to walk
            // with; a regular expression is the same walk a search uses.
            let found: Vec<usize> = if let Some(compiled) = &compiled {
                crate::search::find_places(&buffer, compiled, &translated.extent)
                    .into_iter()
                    .map(|(start, _)| start)
                    .collect()
            } else {
                (0..buffer.line_count())
                    .filter_map(|row| {
                        let line = buffer.line_string(row);
                        let at = crate::fuzzy::match_start(&line, &pattern)?;
                        Some(buffer.line_col_to_char(row, at))
                    })
                    .collect()
            };
            for start in found {
                let (row, col) = buffer.char_to_line_col(start);
                // Without `g` a line is a place of its own once, however often
                // the pattern matches in it.
                let place = spec.all || asked != Some(row);
                asked = Some(row);
                if !place {
                    continue;
                }
                places.push((path.clone(), row, col));
                here += 1;
                if places.len() >= limit {
                    break;
                }
            }
            if here > 0 {
                searched += 1;
            }
        }
        if places.is_empty() {
            // The list is emptied rather than left as it was: vim sets the list
            // to the matches, and no matches is an empty list — checked against
            // `getqflist()` after a `:vimgrep` that found nothing.
            self.set_places(Vec::new());
            self.message = format!("E480: No match: {pattern}");
            return;
        }
        let count = places.len();
        self.set_places(places);
        if spec.jump {
            // The walk is what knows how to go to a place in a list, so it is
            // what goes to the first one — one message style for every list.
            self.walk_places(true, 0);
        } else {
            self.message = format!(
                "{count} match{} in {searched} file{}",
                if count == 1 { "" } else { "es" },
                if searched == 1 { "" } else { "s" }
            );
            self.redraw = true;
        }
    }

    /// Run `:g` or `:v`: a command on every line a pattern matches (or every
    /// line it does not).
    ///
    /// The lines are worked through from the bottom up, because the command is
    /// usually one that takes lines out — `:g/TODO/d` — and removing a line does
    /// not move the ones above it. A command that prints is served by the message
    /// row's one line rather than by running it per line: the matching lines are
    /// collected into that line, the way `:diagnostics` lists what it has.
    pub(crate) fn run_global(&mut self, spec: &crate::global::Global) {
        // `:g` is a search over lines, so `'ignorecase'` and `'smartcase'` are
        // the ones a `/` would use — vim's own list for `'smartcase'` names it —
        // and a `\c` or `\C` in the pattern settles it first, as it does there.
        let ignore_case = spec
            .ignore_case
            .unwrap_or_else(|| self.search_ignore_case(&spec.pattern));
        let pattern = match regex::RegexBuilder::new(&spec.translated)
            .case_insensitive(ignore_case)
            .build()
        {
            Ok(pattern) => pattern,
            Err(error) => {
                self.message = crate::search::PatternError::from(error).to_string();
                return;
            }
        };
        let last = self.buffer.line_count().saturating_sub(1);
        let resolve = |address: crate::substitute::Address| self.resolve_address(address).min(last);
        let (from, to) = match (spec.from, spec.to) {
            (None, _) => (0, last),
            (Some(from), Some(to)) => {
                let (from, to) = (resolve(from), resolve(to));
                (from.min(to), from.max(to))
            }
            (Some(from), None) => (resolve(from), resolve(from)),
        };
        let rows: Vec<usize> = (from..=to)
            .filter(|row| {
                let line = self.buffer.line_string(*row);
                pattern.is_match(&line) != spec.invert
            })
            .collect();
        if rows.is_empty() {
            self.message = format!("Pattern not found: {}", spec.pattern);
            return;
        }
        let command = spec.command.trim();
        if command.is_empty() || command == "p" || command == "print" {
            let mut parts = vec![format!("{} lines", rows.len())];
            for row in rows.iter().take(8) {
                parts.push(format!(
                    "{} {}",
                    row + 1,
                    self.buffer.line_string(*row).trim()
                ));
            }
            if rows.len() > 8 {
                parts.push(format!("... {} more", rows.len() - 8));
            }
            self.message = parts.join("   ");
            return;
        }
        // Whatever each run has to say is left standing: a command that fails
        // says why, and only when there was nothing to say does the count show.
        self.message.clear();
        for row in rows.iter().rev() {
            self.cursor
                .set_char_idx(&self.buffer, self.buffer.line_col_to_char(*row, 0));
            self.execute_cmdline(command);
        }
        if self.message.is_empty() {
            self.message = format!("{} lines", rows.len());
        }
    }

    // `:{range}write >> {file}`: part of the buffer, added to a file.
    fn ex_write_append(&mut self, cmd: &str, arg: &str, range: Option<(usize, usize)>) -> bool {
        if !(matches!(cmd, "w" | "write") && arg.trim_start().starts_with(">>")) {
            return false;
        }

        let rest = arg.trim_start().trim_start_matches(">>").trim();
        let Some(path) = rest.split_whitespace().next().map(PathBuf::from) else {
            self.message = "E471: Argument required".to_string();
            return true;
        };
        let last = self.buffer.line_count().saturating_sub(1);
        let (from, to) = range.unwrap_or((0, last));
        match self.append_range(from, to, &path) {
            Ok(count) => {
                self.message = format!(
                    "{count} line{} appended to \"{}\"",
                    if count == 1 { "" } else { "s" },
                    path.display()
                );
            }
            Err(error) => self.message = error.to_string(),
        }
        true
    }

    // `:{range}write {file}`: part of the buffer, to a file.
    fn ex_write_range(&mut self, cmd: &str, arg: &str, range: Option<(usize, usize)>) -> bool {
        if !(matches!(cmd, "w" | "write") && range.is_some()) {
            return false;
        }

        let Some(path) = arg.split_whitespace().next().map(PathBuf::from) else {
            self.message = "E471: Argument required".to_string();
            return true;
        };
        let last = self.buffer.line_count().saturating_sub(1);
        let (from, to) = range.unwrap_or((0, last));
        match self.write_range(from, to, &path) {
            Ok(count) => {
                self.message = format!(
                    "{count} line{} written to \"{}\"",
                    if count == 1 { "" } else { "s" },
                    path.display()
                );
            }
            Err(error) => self.message = error.to_string(),
        }
        true
    }

    // `:read {file}`: the file's lines, below the one the cursor is on — or
    // below the range's last line, when the line named one.
    fn ex_read(&mut self, cmd: &str, arg: &str, range: Option<(usize, usize)>) -> bool {
        if !(matches!(cmd, "r" | "read")) {
            return false;
        }

        if let Some((_, to)) = range {
            let last = self.buffer.line_count().saturating_sub(1);
            self.cursor.row = to.min(last);
        }
        // `:r !cmd`: what a program says rather than what a file holds, and the
        // `!` in front of the argument is what tells the one from the other. The
        // command is the whole argument — a command line has spaces in it.
        if let Some(command) = arg.trim().strip_prefix('!') {
            self.read_command(command);
            return true;
        }
        let Some(path) = arg.split_whitespace().next().map(PathBuf::from) else {
            self.message = "E471: Argument required".to_string();
            return true;
        };
        match self.read_file_below(&path) {
            Ok(count) => {
                self.message = format!(
                    "{count} line{} read from \"{}\"",
                    if count == 1 { "" } else { "s" },
                    path.display()
                );
            }
            Err(error) => self.message = error.to_string(),
        }
        true
    }

    // `:!{cmd}`, and `:{range}!{cmd}`: a program to run.
    //
    // With a range the lines are the program's input and its output replaces
    // them — a filter — and without one it is a command on its own whose output
    // becomes a document. Both are one thing to the editor: something to run,
    // and what to do with what it says.
    fn ex_shell(&mut self, cmd: &str, arg: &str, range: Option<(usize, usize)>) -> bool {
        if cmd != "!" {
            return false;
        }
        self.run_shell(arg, range);
        true
    }

    // `:normal {keys}`: the keys as if typed, once per line of the range.
    //
    // The keys are what is left of the line *verbatim* — a space in them is
    // a key, not a separator — and they are read with the same notation
    // everything else uses, so `<Esc>` in them is the Escape key rather than
    // five characters. A run that is still inserting when its keys end is
    // put back into normal mode, which is what makes the old idiom
    // `:%normal A;` append to every line rather than to the first.
    fn ex_normal(&mut self, cmd: &str, line: &str, range: Option<(usize, usize)>) -> bool {
        if !(cmd == "normal" || cmd == "norm") {
            return false;
        }

        let Some(keys) = line.split_once(char::is_whitespace).map(|(_, keys)| keys) else {
            self.message = "E471: Argument required".to_string();
            return true;
        };
        let last = self.buffer.line_count().saturating_sub(1);
        let (from, to) = range.unwrap_or((self.cursor.row.min(last), self.cursor.row.min(last)));
        let count = to.saturating_sub(from) + 1;
        // Whatever the keys have to say is left standing: `:normal n` past the
        // last match says `E486` rather than covering it with a count — the same
        // rule `:g` follows for the command it runs on each line.
        self.message.clear();
        for row in from..=to {
            if row > self.buffer.line_count().saturating_sub(1) {
                break;
            }
            let at = self.buffer.line_col_to_char(row, 0);
            self.cursor.set_char_idx(&self.buffer, at);
            self.type_normal_keys(keys);
        }
        if self.message.is_empty() {
            self.message = format!(
                "{count} line{} normalised",
                if count == 1 { "" } else { "s" }
            );
        }
        true
    }

    /// Type `keys` at the cursor, as if they had been pressed.
    ///
    /// The one place the keys of a `:normal` become keys, so that a script can
    /// type the way a person does: `ed.run("normal", { "ihello<Esc>" })` is the
    /// door Lua has to the text, and it is the door with the undo history behind
    /// it.
    pub(crate) fn type_normal_keys(&mut self, keys: &str) {
        for key in crate::keymap::parse_normal_keys(keys) {
            self.handle_key(key);
        }
        // A run that is still inserting when its keys end is put back into normal
        // mode, which is what makes the old idiom `:%normal A;` append to every
        // line rather than to the first.
        if self.mode == Mode::Insert {
            self.run_command("escape", &[]);
        }
    }

    // `:{range}format` is the server's formatting, for the lines a range
    // names; `:format` with no range is the whole file.
    fn ex_format_range(&mut self, cmd: &str, range: Option<(usize, usize)>) -> bool {
        if !(matches!(cmd, "format" | "fmt") && range.is_some()) {
            return false;
        }

        let last = self.buffer.line_count().saturating_sub(1);
        let (from, to) = range.unwrap_or((0, last));
        self.run_command(
            "format_range_command",
            &[Param::Int(from as i64), Param::Int(to as i64)],
        );
        true
    }

    // `:sort` and its flags: the whole file when no range was named, and one
    // edit however many lines it moves.
    // The range was read before the line was stripped of it.
    // `:sort!` has no space between the name and the flag, and
    // `:sort n` has one: they are the same list either way.
    fn ex_sort(&mut self, cmd: &str, arg: &str, range: Option<(usize, usize)>) -> bool {
        let Some(flags) = cmd.strip_prefix("sort") else {
            return false;
        };

        let last = self.buffer.line_count().saturating_sub(1);
        // The range was read before the line was stripped of it.
        let (from, to) = range.unwrap_or((0, last));
        let mut spec = SortSpec::default();
        // `:sort!` has no space between the name and the flag, and
        // `:sort n` has one: they are the same list either way.
        let flags = format!("{flags}{}", arg.trim());
        for flag in flags.chars() {
            match flag {
                '!' => spec.reverse = true,
                'n' => spec.numeric = true,
                'i' => spec.ignore_case = true,
                'u' => spec.unique = true,
                other => {
                    self.message = format!("E474: Invalid argument: {other}");
                    return true;
                }
            }
        }
        let count = self.sort_lines(from, to, &spec);
        self.message = format!("{count} line{} sorted", if count == 1 { "" } else { "s" });
        true
    }

    /// `:{range}d`: a delete with a range is that many lines rather than the
    /// cursor's one. Without a range this is not the handler's business — the
    /// table below runs `delete_line` for a bare `:d`.
    fn ex_delete_range(&mut self, cmd: &str, range: Option<(usize, usize)>) -> bool {
        if !(matches!(cmd, "d" | "delete")) {
            return false;
        }
        let Some((from, to)) = range else {
            return false;
        };
        let start = self.buffer.line_col_to_char(from, 0);
        let end = self.buffer.line_col_to_char(to, 0)
            + self.buffer.line_len(to)
            + self.buffer.line_terminator_len(to);
        self.cut(start, end, Kind::Linewise, Some(start));
        true
    }

    /// `:set`, `:setlocal` and `:setglobal`: a switch on, a switch off, and a
    /// question about any setting, in whichever scope the command asks for.
    /// `:set all` opens the settings window.
    fn ex_set(&mut self, arg: &str, scope: Scope) {
        let (name, value) = match arg.split_once('=') {
            Some((name, value)) => (name.trim().to_string(), value.trim().to_string()),
            None => (arg.trim().to_string(), String::new()),
        };
        // `:set all` is every setting in a window of its own, and
        // `:set inv{name}` turns a switch the other way — vim's two ways of
        // looking at the whole set and of flipping one of it.
        if name == "all" && value.is_empty() {
            self.open_settings_window();
            return;
        }
        // With no argument at all, a scoped `:setlocal`/`:setglobal` says what
        // that scope differs from the defaults in — "display local values for all
        // local options which are different from the default", in the manual's
        // words. `:set` on its own is not one of them: there is nothing to
        // compare it with.
        if name.is_empty() && matches!(scope, Scope::Local | Scope::Global) {
            self.message = self.scoped_settings_by_name(scope);
            return;
        }
        if value.is_empty() {
            if let Some(rest) = name.strip_prefix("inv") {
                match crate::settings::Setting::from_name(rest) {
                    None => {
                        self.message = format!("E518: Unknown option: {rest}");
                    }
                    Some(setting) => {
                        let flipped = match self.options.borrow().get(setting) {
                            Param::Bool(value) => (!value).to_string(),
                            _ => {
                                self.message = format!("E474: {rest} is not a switch");
                                return;
                            }
                        };
                        match self.set_option(setting, &flipped, scope) {
                            Ok(()) => self.message = format!("{rest}={flipped}"),
                            Err(error) => {
                                self.message = format!("E474: {rest} {error}");
                            }
                        }
                    }
                }
                return;
            }
        }
        // The three spellings vim has, since they are the three anybody
        // types: `:set number` turns a switch on, `:set nonumber` turns it
        // off, and `:set number?` asks what it is. A bare name that is not
        // a switch asks too, there being no "on" for a number.
        let asked = name.ends_with('?');
        let (name, value) = match (asked, value.is_empty(), name.strip_prefix("no")) {
            (false, true, Some(rest)) => match Setting::from_name(rest) {
                Some(setting) if matches!(self.options.borrow().get(setting), Param::Bool(_)) => {
                    (rest.to_string(), "false".to_string())
                }
                _ => (name, value),
            },
            (false, true, None) => match Setting::from_name(&name) {
                Some(setting) if matches!(self.options.borrow().get(setting), Param::Bool(_)) => {
                    (name, "true".to_string())
                }
                _ => (name, value),
            },
            _ => (name, value),
        };
        let name = name.trim_end_matches('?').to_string();
        match crate::settings::Setting::from_name(&name) {
            None => self.message = format!("E518: Unknown option: {name}"),
            Some(setting) if value.is_empty() => {
                // What is asked about depends on the spelling: `:setlocal?` is
                // this file's value, `:setglobal?` the one outside every file,
                // and `:set?` the value in force here.
                let text = match scope {
                    // "When the global value is being used, `--` is displayed
                    // before the option name": the file has no value of its own
                    // and the answer is the one every file gets.
                    Scope::Local
                        if self.local_option_text(setting).is_empty()
                            || !self.has_local_option(setting) =>
                    {
                        format!("--{}={}", name, self.global_option_text(setting))
                    }
                    Scope::Local => format!("{name}={}", self.local_option_text(setting)),
                    Scope::Global => format!("{name}={}", self.global_option_text(setting)),
                    Scope::Both => format!("{name}={}", self.option_text(setting)),
                };
                self.message = text;
            }
            Some(setting) => match self.set_option(setting, &value, scope) {
                Ok(()) => {
                    self.message = format!("{name}={value}");
                    // With `'foldmethod'` set to `indent` the folds are a
                    // function of the settings — the method itself, the
                    // characters ignored, how deep they may nest, and what a
                    // tab counts as — so setting *any* of them works the folds
                    // out again. A `:set` is not a keystroke, and a promise of
                    // folds is not folds.
                    if self.foldmethod_is_indent() {
                        let count = self.compute_indent_folds();
                        self.message = format!(
                            "{name}={value} ({count} fold{})",
                            if count == 1 { "" } else { "s" }
                        );
                    }
                }
                Err(error) => self.message = format!("E474: {name} {error}"),
            },
        }
    }
}
