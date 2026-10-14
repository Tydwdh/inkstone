use inkstone_rpc::{KeyMsg, Param};

use crate::command::{self, binding_motion};
use crate::keymap::{encode_key, Binding, Lookup};
use crate::marks::MarkJump;
use crate::mode::{Mode, VisualKind};
use crate::register::{is_register_name, Kind, Prefix};

use super::*;

impl Editor {
    /// Feed one key press through the mode state machine.
    pub fn handle_key(&mut self, key: KeyMsg) {
        // `<C-c>` in a command-line window continues in Command-line mode with
        // the line under the cursor — "works both in Insert and in Normal mode"
        // — which is why it is handled before the mode is looked at. The window
        // stays where it is: vim's own note is "there is no redraw, thus the
        // window will remain visible".
        if self.in_history_window() && key.code == KeyCode::Char('c') && key.is_ctrl() {
            self.continue_from_history_window();
            return;
        }
        // A message lives until the next key *outside* command mode; the key
        // that finishes a `:` command must not erase what it just reported. A
        // `:s///c` is the other exception: its question *is* the message, and
        // the keys that answer it must not erase it before they are read.
        if self.mode != Mode::Command && self.confirm.is_none() {
            self.message.clear();
        }
        // A macro being recorded keeps every key typed *into* it, but not the keys
        // this editor is replaying: a macro that contained itself would never end.
        if !self.replaying {
            if let Some((_, keys)) = self.recording.as_mut() {
                keys.push_str(&encode_key(&key));
            }
        }
        match self.mode {
            Mode::Normal => self.handle_normal(key),
            Mode::Visual => self.handle_visual(key),
            Mode::Insert => {
                self.handle_insert(key.clone());
                self.replay_block_insert(key);
            }
            Mode::Command => self.handle_command_line(key),
        }
        // "When in Insert mode, the cursor line is never folded. That allows you to see
        // what you type!" — so entering insert opens the fold the cursor is on.
        if self.mode == Mode::Insert {
            self.open_fold_under_cursor();
        }
        self.redraw = true;
    }

    /// Open the fold the cursor's line is in, if a closed one is: what insert mode
    /// needs, and what a person asking to type on a folded line means.
    pub(crate) fn open_fold_under_cursor(&mut self) {
        let row = self.cursor.row;
        let Some(index) = (0..self.folds.len()).find(|index| {
            let fold = self.folds[*index];
            self.fold_is_shut(*index) && row >= fold.start && row <= fold.end
        }) else {
            return;
        };
        self.set_fold_closed_at(index, false);
    }

    /// A programmatic command, as sent over RPC.
    pub fn handle_command(&mut self, name: &str, params: &[Param]) {
        self.run_command(name, params);
        self.redraw = true;
    }

    /// Run a named command, then drain anything Lua queued while running it.
    pub(crate) fn run_command(&mut self, name: &str, params: &[Param]) {
        let name = name.to_string();
        // "The command-line window is not a normal window. It is not possible to
        // move to another window or edit another buffer. All commands that would
        // do this are disabled in the command-line window." Vim marks the
        // commands that *are* allowed (its `EX_CMDWIN` flag) and refuses the rest
        // with `E11`; this lists the ones it refuses, because the set of commands
        // that change the window or the buffer is the smaller one to keep in step
        // with the registry.
        if self.in_history_window() && moves_window_or_buffer(&name) {
            self.message =
                "E11: Invalid in command-line window; :q<CR> closes the window".to_string();
            return;
        }
        // One command is one step in the history — a counted `3x` is a single
        // change to a user — and so is a whole insert session. The two nest:
        // the group opened here closes before this returns, and entering or
        // leaving insert mode opens or closes the session's group around it.
        let was_insert = self.mode == Mode::Insert;
        let edits_before = self.history.edits();
        self.history.begin_group();
        let mut next = Some((name.clone(), params.to_vec()));
        let mut depth = 0;
        while let Some((name, params)) = next.take() {
            if let Err(message) = self.run_named(&name, &params) {
                self.message = message;
                break;
            }
            depth += 1;
            if depth > MAX_COMMAND_DEPTH {
                self.message = "E5108: command nesting too deep".to_string();
                break;
            }
            next = self.lua.take_pending();
        }
        let entered_insert = self.mode == Mode::Insert && !was_insert;
        let left_insert = self.mode != Mode::Insert && was_insert;
        // A `:s///c` keeps the group its command opened: the answers are keys,
        // and every replacement they make has to join that one step.
        let asking = self.confirm.is_some();
        // What `.` will repeat: a command that changed something, or one that
        // opened an insert session — `ihello<Esc>` changes nothing until the
        // typing arrives, and it is the typing that is the change.
        let recorded = self.history.edits() != edits_before || entered_insert;
        let repeatable = !matches!(name.as_str(), "undo" | "redo" | "repeat" | "escape");
        if recorded && repeatable {
            self.last_command = Some((name.clone(), params.to_vec()));
            if !entered_insert {
                // A change with no session behind it: the text of the last one
                // is not part of this one.
                self.last_insert = None;
            }
        }
        if entered_insert {
            self.insert_log = Some(String::new());
        }
        if left_insert {
            let typed = self.insert_log.take().unwrap_or_default();
            self.last_insert = (!typed.is_empty()).then_some(typed);
        }
        if !entered_insert && !asking {
            self.history.end_group(&mut self.buffer);
        }
        if left_insert {
            // The session's group closes here, so everything typed in it is one
            // step. A command that *entered* insert mode does not close its
            // group above: that group is the session's.
            self.history.end_group(&mut self.buffer);
        }
        self.redraw = true;
    }

    pub(super) fn run_named(&mut self, name: &str, params: &[Param]) -> Result<(), String> {
        // Copy the function pointer out so the registry borrow ends before the
        // editor is borrowed mutably.
        if let Some(f) = self.registry.get(name) {
            return f(self, params).map_err(|e| e.to_string());
        }
        if self.lua.has_command(name) {
            return self
                .lua
                .call_command(name, params)
                .map_err(|e| e.to_string());
        }
        Err(CommandError::Unknown(name.to_string()).to_string())
    }

    // -- mode handlers ------------------------------------------------------

    pub(super) fn handle_normal(&mut self, key: KeyMsg) {
        self.clamp_normal_cursor();
        let encoded = encode_key(&key);

        // A `:s///c` is asking a question and every key is an answer to it.
        if self.confirm.is_some() {
            self.answer_confirm(&encoded);
            return;
        }
        // While a macro is recording, `q` is what stops it — vim's rule, and the
        // one key that means two things at once: `q:` and `q/` are the history
        // windows, and a recording ends before either can be reached. (Vim would
        // record the pair and open the window; ending the recording is the reading
        // a person expects, and the roadmap says so.)
        if self.recording.is_some() && encoded == "q" {
            let (recording, keys) = self.recording.take().expect("just looked at it");
            let keys = keys.strip_suffix('q').unwrap_or(&keys).to_string();
            let index = (recording as u8).wrapping_sub(b'a') as usize;
            self.macros[index] = Some(keys);
            self.message = format!("recorded @{recording}");
            return;
        }
        // `q` and `@` are waiting for a register letter, and until it comes no
        // other key means anything. (`:` and `/` after a `q` are not a register
        // but the history windows: the keymap is given those.)
        if self.macro_ask.is_some() && self.take_macro_letter(&encoded) {
            return;
        }
        // `m`, `` ` `` and `'` are waiting for a letter, and until it comes no
        // other key means anything.
        if self.mark_prefix.is_some() {
            self.take_mark_target(&encoded);
            return;
        }

        // A `"` names the register for the next operator, yank or put: `"ayy`,
        // `"ap`, `\"_dd`. It comes before the operator, never after.
        match self.register_prefix {
            Prefix::Awaiting => {
                match encoded.chars().next() {
                    Some(name) if is_register_name(name) => {
                        self.register_prefix = Prefix::Named(name);
                    }
                    _ => {
                        self.message = format!("E354: Invalid register name: {encoded}");
                        self.register_prefix = Prefix::None;
                    }
                }
                return;
            }
            Prefix::None if encoded == "\"" && self.operator.is_none() => {
                self.register_prefix = Prefix::Awaiting;
                return;
            }
            _ => {}
        }

        // `f{char}` and its relatives wait for their character, which the
        // keymap cannot carry.
        if let Some(name) = pending_find(&self.pending) {
            self.take_find_target(&encoded, name);
            return;
        }
        if self.operator.is_none()
            && self.register_prefix == Prefix::None
            && (self.starts_find(&encoded) || self.starts_mark(&encoded))
        {
            return;
        }

        // `@` plays a macro and is not in the keymap — the letter that follows is
        // any letter, which a table of names cannot say.
        if self.pending.is_empty() && self.operator.is_none() && encoded == "@" {
            self.macro_ask = Some(MacroAsk::Play);
            return;
        }

        if self.operator.is_some() {
            self.handle_operator_pending(key, &encoded);
        } else if let Some(digit) = count_digit(&key, !self.pending_count.is_empty()) {
            if self.pending_count.len() < MAX_COUNT_DIGITS {
                self.pending_count.push(digit);
            }
        } else {
            self.dispatch_normal(&encoded);
        }
        // Motions and edits may have left the cursor on the terminator.
        self.clamp_normal_cursor();
    }

    /// The keymap's turn: resolve the pending sequence, and run what it binds.
    pub(super) fn dispatch_normal(&mut self, encoded: &str) {
        let sequence = if self.pending.is_empty() {
            encoded.to_string()
        } else {
            format!("{} {}", self.pending, encoded)
        };
        match self.lookup_normal(&sequence) {
            Lookup::Exact(binding) => {
                self.pending.clear();
                let count = self.take_count();
                self.run_binding(binding, count);
            }
            Lookup::Prefix => self.pending = sequence,
            Lookup::None => {
                if self.pending.is_empty() {
                    self.unmapped(encoded);
                    return;
                }
                // `q` and a letter the table does not name is a macro: the
                // recording starts (or stops) here rather than in a second pending
                // mechanism beside the keymap's.
                if self.pending == "q" {
                    let key = encoded.to_string();
                    self.pending.clear();
                    self.macro_ask = Some(MacroAsk::Record);
                    if self.take_macro_letter(&key) {
                        return;
                    }
                }
                // The pending key did not start a binding; retry this key alone.
                self.pending.clear();
                match self.lookup_normal(encoded) {
                    Lookup::Exact(binding) => {
                        let count = self.take_count();
                        self.run_binding(binding, count);
                    }
                    Lookup::Prefix => self.pending = encoded.to_string(),
                    Lookup::None => self.unmapped(encoded),
                }
            }
        }
    }

    /// Visual mode's turn: the keys that act on the selection, and everything
    /// else which extends it.
    ///
    /// Motions are the normal mode's, so the selection is extended by the very
    /// same commands — including a user's remapping of them.
    pub(super) fn handle_visual(&mut self, key: KeyMsg) {
        if let Some(digit) = count_digit(&key, !self.pending_count.is_empty()) {
            if self.pending_count.len() < MAX_COUNT_DIGITS {
                self.pending_count.push(digit);
            }
            return;
        }
        let encoded = encode_key(&key);

        // `l` and `h` in a *block* move the column and nothing else: a rectangle may
        // reach past the end of a short line — that is the whole point of one, and
        // what vim's `'virtualedit'` allows there — so the motion must not stop at
        // the line's end the way `move_right` does everywhere else.
        if self.is_making_a_block() && matches!(encoded.as_str(), "l" | "h") {
            let count = self.take_count();
            for _ in 0..count {
                if encoded == "l" {
                    self.cursor.col += 1;
                } else {
                    self.cursor.col = self.cursor.col.saturating_sub(1);
                }
            }
            self.after_edit();
            return;
        }

        // `"` names the register for a yank out of the selection: `"+y`.
        match self.register_prefix {
            Prefix::Awaiting => {
                match encoded.chars().next() {
                    Some(name) if is_register_name(name) => {
                        self.register_prefix = Prefix::Named(name);
                    }
                    _ => {
                        self.message = format!("E354: Invalid register name: {encoded}");
                        self.register_prefix = Prefix::None;
                    }
                }
                return;
            }
            Prefix::None if encoded == "\"" => {
                self.register_prefix = Prefix::Awaiting;
                return;
            }
            _ => {}
        }

        // A find moves the selection's end: `vf,`. The character it waits for
        // comes in the same way it does in normal mode.
        if let Some(name) = pending_find(&self.pending) {
            self.take_find_target(&encoded, name);
            return;
        }
        if self.pending.is_empty() && self.starts_find(&encoded) {
            return;
        }

        // `i` and `a` in a selection ask for a text object as well: `viw`
        // selects the word, `va"` the string and its quotes.
        if self.pending.is_empty() {
            if self.object.is_none() && (encoded == "i" || encoded == "a") {
                self.object = Some(if encoded == "i" {
                    Object::Inner
                } else {
                    Object::Around
                });
                return;
            }
            if let Some(object) = self.object {
                self.object = None;
                match command::object_span(self, object, &encoded) {
                    Some(span) if span.start < span.end => {
                        self.select_object(span);
                    }
                    Some(_) => {}
                    None => {
                        self.message = format!("{encoded} is not a text object");
                    }
                }
                return;
            }
        }

        let sequence = if self.pending.is_empty() {
            encoded.clone()
        } else {
            format!("{} {}", self.pending, encoded)
        };

        // A visual binding wins, and the normal mode's motions are what is left.
        match self.lookup_visual(&sequence) {
            Lookup::Exact(binding) => {
                self.pending.clear();
                let count = self.take_count();
                self.run_binding(binding, count);
            }
            Lookup::Prefix => self.pending = sequence,
            Lookup::None => {
                // A multi-key motion is the normal mode's too (`gg`, `dd` is not
                // a motion, but `gg` is), so the fallback looks the sequence up
                // exactly as the normal mode would.
                match self.lookup_normal(&sequence) {
                    Lookup::Exact(binding) => {
                        self.pending.clear();
                        let count = self.take_count();
                        self.run_binding(binding, count);
                    }
                    Lookup::Prefix => self.pending = sequence,
                    Lookup::None => {
                        self.pending.clear();
                        self.unmapped(&encoded);
                    }
                }
            }
        }
        self.clamp_normal_cursor();
    }

    pub(super) fn lookup_visual(&self, sequence: &str) -> Lookup {
        let keymap = self.keymap.borrow();
        keymap.lookup(Mode::Visual, sequence)
    }

    /// The key after `d`, `y` or `c`: a motion, or the operator's own key
    /// again for whole lines.
    pub(super) fn handle_operator_pending(&mut self, key: KeyMsg, encoded: &str) {
        let operator = self.operator.expect("only called with one pending");

        // `v`, `V` and `<C-V>` after an operator do not start a selection: they say
        // the *shape* the motion's range will be — vim's `o_v`, `o_V` and
        // `o_CTRL-V`. None of the three is a motion, so all are answered before the
        // motion lookup, and the cursor now is one corner of what follows.
        if self.pending.is_empty() {
            let forced = match encoded {
                "v" => Some(ForcedShape::Charwise),
                "V" => Some(ForcedShape::Linewise),
                "<C-v>" => Some(ForcedShape::Blockwise),
                _ => None,
            };
            if let Some(shape) = forced {
                self.forced_shape = Some((shape, self.cursor));
                return;
            }
        }

        // A find is a motion an operator can take: `df,` deletes through the
        // comma. It waits for its character just as the bare motion does.
        if self.pending.is_empty() && (self.starts_find(encoded) || self.starts_mark(encoded)) {
            return;
        }

        // `i` and `a` ask for a text object: `diw`, `ci"`, `ya(`. What follows
        // names the object, and the operator runs over the range it gives.
        if self.pending.is_empty() {
            if self.object.is_none() && (encoded == "i" || encoded == "a") {
                self.object = Some(if encoded == "i" {
                    Object::Inner
                } else {
                    Object::Around
                });
                return;
            }
            if let Some(object) = self.object {
                let (operator, _count) = self.take_operator();
                match command::object_span(self, object, encoded) {
                    Some(span) if span.start < span.end => {
                        self.apply_operator(operator, span);
                    }
                    Some(_) => {}
                    None => {
                        self.message = format!("{encoded} is not a text object");
                    }
                }
                return;
            }
        }

        // `dd`, `yy`, `cc`, `gUU`, `guu`, `g~~`: whole lines, as many as the
        // counts say.
        if encoded == operator.key() {
            let (operator, count) = self.take_operator();
            self.run_command(operator.line_command(), &[Param::Int(count as i64)]);
            return;
        }

        // A digit counts the motion: `d3w`. A `0` with no count running is the
        // motion `move_line_start`, which is what `d0` means.
        if let Some(digit) = count_digit(&key, !self.pending_count.is_empty()) {
            if self.pending_count.len() < MAX_COUNT_DIGITS {
                self.pending_count.push(digit);
            }
            return;
        }

        // Otherwise it is a motion, and motions can be multi-key (`d g g`).
        let sequence = if self.pending.is_empty() {
            encoded.to_string()
        } else {
            format!("{} {}", self.pending, encoded)
        };
        let motion = match self.lookup_normal(&sequence) {
            Lookup::Exact(binding) => match binding_motion(&binding) {
                Some(name) => name.to_string(),
                None => {
                    // A binding that is not a motion ends the operator, the way
                    // vim beeps: `d` then `i` does not enter insert mode.
                    self.message = format!("{sequence} is not a motion");
                    self.clear_pending();
                    return;
                }
            },
            Lookup::Prefix => {
                self.pending = sequence;
                return;
            }
            Lookup::None => {
                self.unmapped(encoded);
                self.clear_pending();
                return;
            }
        };

        let (operator, count) = self.take_operator();
        // `d<C-V>` — vim's `o_CTRL-V`: the operator works blockwise, and the
        // block's corners are where the cursor was and where the motion took it.
        // So the motion runs as a motion, the rectangle is made into a selection,
        // and the operator's *visual* command applies it — the same path every
        // other block operation goes through.
        if let Some((ForcedShape::Blockwise, anchor)) = self.forced_shape {
            self.run_command(&motion, &[Param::Int(count as i64)]);
            self.selection = Some(Selection {
                anchor,
                kind: VisualKind::Blockwise,
                to_end: false,
            });
            self.run_command(operator.visual_command(), &[]);
            return;
        }
        self.run_command(
            operator.command(),
            &[Param::Int(count as i64), Param::Str(motion)],
        );
    }

    /// Select what a text object named.
    ///
    /// A span of characters becomes a characterwise selection with the cursor on
    /// its last character. A span of whole lines has to become a *linewise*
    /// selection: the last character of a line is its terminator, and a
    /// normal-mode cursor cannot sit on one, so the two ends of a linewise range
    /// are lines and not characters.
    pub(crate) fn select_object(&mut self, span: crate::command::Span) {
        let mut anchor = self.cursor;
        anchor.set_char_idx(&self.buffer, span.start);
        let mut cursor = self.cursor;
        if span.linewise {
            let last = self.buffer.char_to_line_col(span.end.saturating_sub(1)).0;
            cursor.set_char_idx(&self.buffer, self.buffer.line_col_to_char(last, 0));
        } else {
            cursor.set_char_idx(&self.buffer, span.end.saturating_sub(1).max(span.start));
        }
        self.selection = Some(Selection {
            anchor,
            to_end: false,
            kind: if span.linewise {
                VisualKind::Linewise
            } else {
                VisualKind::Charwise
            },
        });
        self.cursor = cursor;
        self.mode = Mode::Visual;
        self.after_edit();
    }

    /// Whether this key starts a character find, which then waits for the one
    /// key that names its character.
    ///
    /// A find is two keys and the second is any key at all, so it cannot be a
    /// keymap binding: the keymap is a table of names, and the character is not
    /// a name.
    pub(super) fn starts_find(&mut self, encoded: &str) -> bool {
        let name = match encoded {
            "f" => "find_forward",
            "F" => "find_backward",
            "t" => "till_forward",
            "T" => "till_backward",
            _ => return false,
        };
        // Only from a clean slate: after another pending key (`z` before `t`)
        // this key belongs to that sequence, not to a find of its own.
        if !self.pending.is_empty() {
            return false;
        }
        self.pending = name.to_string();
        true
    }

    /// Whether `q` or `@` is waiting for a register letter, and what to do with
    /// the key that arrived.
    ///
    /// Returns whether the key was taken. `q` and `@` are *not* a second pending
    /// mechanism beside the keymap's: they are what the keymap's pending state
    /// means when the key that follows is one the table cannot name, because a
    /// register letter is any letter. The one exception is the pair the table
    /// *does* name — `q:` and `q/` — which is handed back to it.
    pub(super) fn take_macro_letter(&mut self, encoded: &str) -> bool {
        let Some(ask) = self.macro_ask else {
            return false;
        };
        // `@@` plays the macro `@` was last given, which is the whole point of
        // remembering it.
        if ask == MacroAsk::Play && encoded == "@" {
            self.macro_ask = None;
            let count = self.take_count();
            match self.last_macro {
                Some(letter) => self.play_macro(letter, count),
                None => self.message = "E748: No previous macro".to_string(),
            }
            return true;
        }
        let letter = encoded
            .chars()
            .next()
            .filter(|c| c.is_ascii_lowercase() && encoded.chars().count() == 1);
        if letter.is_none() {
            // `q:` and `q/` are the history windows, and the keymap knows them.
            if ask == MacroAsk::Record && matches!(encoded, ":" | "/") {
                return false;
            }
            self.macro_ask = None;
            return true;
        }
        let letter = letter.expect("just checked");
        self.macro_ask = None;
        match ask {
            MacroAsk::Record => {
                if let Some((recording, keys)) = self.recording.take() {
                    // A `q` while recording ends it — whichever letter came with
                    // it, since the register is the one being recorded into. The
                    // `q` itself is not part of the macro: it was pushed on the way
                    // in, so it comes off.
                    let keys = keys.strip_suffix('q').unwrap_or(&keys).to_string();
                    let index = (recording as u8).wrapping_sub(b'a') as usize;
                    self.macros[index] = Some(keys);
                    self.message = format!("recorded @{recording}");
                    return true;
                }
                self.recording = Some((letter, String::new()));
                self.message = format!("recording @{letter}");
            }
            // A count before `@` is how many times: `3@a`.
            MacroAsk::Play => {
                let count = self.take_count();
                self.play_macro(letter, count);
            }
        }
        true
    }

    /// Play a macro, `count` times.
    ///
    /// The keys go back through [`Editor::handle_key`], which is what makes a
    /// macro a macro: it is the same state machine every key goes through. The
    /// replay is flagged, so a macro cannot record itself.
    pub(super) fn play_macro(&mut self, letter: char, count: usize) {
        let index = (letter as u8).wrapping_sub(b'a') as usize;
        let Some(keys) = self.macros.get(index).and_then(|held| held.clone()) else {
            self.message = format!("E748: No such macro: {letter}");
            return;
        };
        self.last_macro = Some(letter);
        let parsed = crate::keymap::parse_key(&keys).unwrap_or_default();
        self.replaying = true;
        for _ in 0..count {
            for key in &parsed {
                self.handle_key(key.clone());
            }
        }
        self.replaying = false;
    }

    /// Whether a key is one of the three that wait for a mark's letter, and what
    /// it will do with it.
    ///
    /// Like `f{char}`, these are keys and not commands: there is no letter to
    /// give a command until the next key arrives.
    pub(super) fn starts_mark(&mut self, encoded: &str) -> bool {
        let jump = match encoded {
            "m" => MarkJump::Set,
            "`" => MarkJump::Exact,
            "'" => MarkJump::Line,
            _ => return false,
        };
        // Only from a clean slate: a `z` before a `t` belongs to the `zt`.
        if !self.pending.is_empty() {
            return false;
        }
        self.mark_prefix = Some(jump);
        true
    }

    /// The letter of a mark that is waiting for one.
    ///
    /// Any letter does, and anything else calls it off the way vim does. The
    /// three keys are told apart by what they do with the letter: `m` puts a
    /// mark down, and the two jumps run the motion, over an operator if one is
    /// waiting — which is what makes `d'a` take the lines and `d`a` the text up
    /// to the mark.
    pub(super) fn take_mark_target(&mut self, encoded: &str) {
        let Some(jump) = self.mark_prefix.take() else {
            return;
        };
        let mut chars = encoded.chars();
        let Some(letter) = chars.next().filter(|_| chars.next().is_none()) else {
            self.clear_pending();
            return;
        };
        // `''` and `` `` `` are the last jump rather than a mark, which is why
        // the key is taken here and not by the address parser.
        if letter == '\'' || letter == '`' {
            let name = if jump == MarkJump::Line {
                "jump_back_line"
            } else {
                "jump_back_exact"
            };
            if self.operator.is_some() {
                let (operator, count) = self.take_operator();
                self.run_command(
                    operator.command(),
                    &[Param::Int(count as i64), Param::Str(name.to_string())],
                );
                return;
            }
            let count = self.take_count();
            let _ = count;
            self.pending.clear();
            self.run_command(name, &[]);
            return;
        }
        if !letter.is_ascii_lowercase() {
            self.clear_pending();
            return;
        }
        self.last_mark = Some((letter, jump));
        let params = [Param::Str(letter.to_string())];
        if jump == MarkJump::Set {
            let count = self.take_count();
            let _ = count;
            self.run_command("set_mark", &params);
            self.pending.clear();
            return;
        }
        let name = match jump {
            MarkJump::Exact => "mark_exact",
            _ => "mark_line",
        };
        match self.operator {
            Some(_) => {
                let (operator, count) = self.take_operator();
                self.run_command(
                    operator.command(),
                    &[Param::Int(count as i64), Param::Str(name.to_string())],
                );
            }
            None => {
                let count = self.take_count();
                self.pending.clear();
                self.run_command(
                    name,
                    &[Param::Int(count as i64), Param::Str(letter.to_string())],
                );
            }
        }
    }

    /// The key that names the character of a find that is waiting for one.
    ///
    /// `Escape` is not a character: it calls the find off, the way vim does.
    pub(super) fn take_find_target(&mut self, encoded: &str, name: &str) -> bool {
        if encoded == "<Esc>" {
            self.clear_pending();
            return true;
        }
        let mut chars = encoded.chars();
        let Some(target) = chars.next().filter(|_| chars.next().is_none()) else {
            self.clear_pending();
            return true;
        };
        // The find remembers its character here, before anything runs: the
        // motion reads it from there, and so does the operator path, which is
        // handed the motion's *name* and not its character.
        if let Some(kind) = command::find_of(name) {
            self.last_find = Some((target, kind));
        }
        match self.operator {
            Some(_) => {
                let (operator, count) = self.take_operator();
                self.run_command(
                    operator.command(),
                    &[Param::Int(count as i64), Param::Str(name.to_string())],
                );
            }
            None => {
                let count = self.take_count();
                self.pending.clear();
                self.run_command(
                    name,
                    &[Param::Int(count as i64), Param::Str(target.to_string())],
                );
            }
        }
        true
    }

    /// The pending operator and its count, spent.
    ///
    /// The count is the operator's times the motion's, which is what makes
    /// `2d3w` six words without either of them knowing about the other.
    pub(super) fn take_operator(&mut self) -> (Operator, usize) {
        let operator = self.operator.expect("only called with one pending");
        let count = self.operator_count * self.take_count();
        self.operator = None;
        self.object = None;
        self.operator_count = 1;
        self.pending.clear();
        (operator, count)
    }

    /// Run a pending operator over a range a text object named.
    ///
    /// Most objects are a stretch of a line, and a paragraph is whole lines: the
    /// span says which, so the register that `dip` fills pastes as lines the way
    /// `dd` does.
    pub(super) fn apply_operator(&mut self, operator: Operator, span: crate::command::Span) {
        // A closed fold is a whole, as the manual puts it: an operator that reaches
        // into one takes all of it — "`dl` deletes the whole closed fold under the
        // cursor". The range is widened to the fold's own lines before anything is
        // done with it.
        let span = self.widen_over_folds(span);
        let kind = if span.linewise {
            Kind::Linewise
        } else {
            Kind::Charwise
        };
        let (start, end) = (span.start, span.end);
        match operator {
            Operator::Fold => {
                // A fold is a range of *lines*, whatever the motion's shape was.
                let first = self.buffer.char_to_line_col(start).0;
                let last = self.buffer.char_to_line_col(end.saturating_sub(1)).0;
                self.fold_lines(first, last);
            }
            Operator::Delete => self.cut(start, end, kind, Some(start)),
            Operator::Yank => {
                self.copy(start, end, kind);
                // `yiw` leaves the cursor where the object begins.
                self.cursor.set_char_idx(&self.buffer, start);
                self.after_edit();
            }
            Operator::Change => {
                // Insert mode first: the cut ends with the cursor clamped to a
                // real character for normal mode, and the text about to be typed
                // belongs where the object began — which for a word at the end
                // of a line is one past the last character.
                self.mode = Mode::Insert;
                self.cut(start, end, kind, Some(start));
                self.cursor.set_char_idx(&self.buffer, start);
            }
            // A case change is not a cut: the text stays where it is, and the
            // register keeps what it had. The object's range is characters, so
            // the linewise flag only ever says how `d` and `y` should treat what
            // they take.
            Operator::ToggleCase => {
                self.change_case(start, end, crate::command::CaseChange::Toggle)
            }
            // A rewrap is about lines, so it takes the whole of them however the
            // range was named — an object gives a run of characters, and the
            // lines those characters are on are what is rewritten.
            Operator::Format => {
                let from = self.buffer.char_to_line_col(start).0;
                let to = self.buffer.char_to_line_col(end.saturating_sub(1)).0;
                self.format_lines(from, to);
            }
            Operator::Reindent => {
                let from = self.buffer.char_to_line_col(start).0;
                let to = self.buffer.char_to_line_col(end.saturating_sub(1)).0;
                self.reindent_lines(from, to);
            }
            Operator::Indent => {
                let from = self.buffer.char_to_line_col(start).0;
                let to = self.buffer.char_to_line_col(end.saturating_sub(1)).0;
                self.shift_indent(from, to, true);
            }
            Operator::Outdent => {
                let from = self.buffer.char_to_line_col(start).0;
                let to = self.buffer.char_to_line_col(end.saturating_sub(1)).0;
                self.shift_indent(from, to, false);
            }
            Operator::Lower => self.change_case(start, end, crate::command::CaseChange::Lower),
            Operator::Upper => self.change_case(start, end, crate::command::CaseChange::Upper),
        }
    }

    /// Forget a half-typed command: the pending keys, the count and the
    /// operator. Called whenever the mode changes, so `d` then Escape leaves
    /// nothing behind.
    pub(crate) fn clear_pending(&mut self) {
        // What a mode was doing is remembered here, where a mode *stops*: the
        // last selection is what `gv` comes back to, and the last place insert
        // mode was is what `gi` does. Both are per document, and both are read
        // before the mode changes rather than after.
        if self.mode == Mode::Visual {
            if let Some(selection) = self.selection {
                self.last_visual = Some((selection, self.cursor));
            }
        }
        if self.mode == Mode::Insert {
            self.insert_again_at = Some(self.cursor);
        }
        self.pending.clear();
        self.insert_pending.clear();
        self.pending_count.clear();
        self.operator = None;
        self.object = None;
        self.mark_prefix = None;
        self.register_prefix = Prefix::None;
        self.operator_count = 1;
        if self.mode != Mode::Visual {
            self.selection = None;
        }
    }

    /// The count the user typed, and the count is spent.
    pub(super) fn take_count(&mut self) -> usize {
        let count = self
            .pending_count
            .parse::<usize>()
            .ok()
            .filter(|count| *count > 0)
            .unwrap_or(1);
        self.pending_count.clear();
        count
    }

    pub(super) fn lookup_normal(&self, sequence: &str) -> Lookup {
        let keymap = self.keymap.borrow();
        keymap.lookup(Mode::Normal, sequence)
    }

    pub(super) fn run_binding(&mut self, binding: Binding, count: usize) {
        let params = count_params(count);
        for name in binding.commands {
            self.run_command(&name, &params);
            if self.should_quit {
                break;
            }
        }
    }

    pub(super) fn unmapped(&mut self, encoded: &str) {
        self.message = format!("unmapped key: {encoded}");
    }
}

/// Whether a command moves to another window, or edits another buffer, which is
/// what a command-line window refuses.
///
/// The list is vim's own rule written the other way round: it flags the commands
/// that are *allowed* there and refuses the rest, while this names the ones that
/// are refused, because that is the set a registry this size can keep in step.
fn moves_window_or_buffer(name: &str) -> bool {
    matches!(
        name,
        // Another file in this window.
        "edit_file"
            | "edit_file_force"
            | "buffer_go"
            | "buffer_next"
            | "buffer_previous"
            | "buffer_delete"
            | "buffer_next_force"
            | "buffer_previous_force"
            | "buffer_delete_force"
            | "open_place"
            // Another window, or a change to this one's place on the screen.
            | "split_rows"
            | "split_columns"
            | "tab_split"
            | "window_only"
            | "window_next"
            | "window_left"
            | "window_right"
            | "window_up"
            | "window_down"
            | "close_other_windows"
            | "equalize_windows"
            // Another tab.
            | "tab_new"
            | "tab_close"
            | "tab_next"
            | "tab_previous"
            | "list_tabs"
            // A list window of their own.
            | "open_places"
            | "close_places"
            | "open_diagnostics"
            | "close_diagnostics"
            | "list_older"
            | "list_newer"
            | "hover"
            | "goto_definition"
            | "goto_references"
    )
}
