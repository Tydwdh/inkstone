//! The editor: state, the mode state machine, and the command dispatch loop.
//!
//! `Editor` is the *only* thing the daemon owns. It has no I/O, no threads and
//! no terminal: it consumes key presses and produces a [`Composed`] screen, so
//! the whole editor is exercisable from a unit test.
//!
//! Dispatch order for a named command:
//!
//! 1. the built-in [`CommandRegistry`],
//! 2. a command registered from Lua,
//! 3. otherwise an `E492` message.
//!
//! Lua callbacks cannot borrow the editor, so `ed.run(...)` inside a script
//! *queues* the call; [`Editor::run_command`] drains that queue after every
//! command, which is what makes `ed.map("n", "x", function() ed.run("move_right") end)`
//! work without re-entrancy.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use edit_rpc::{KeyCode, KeyMsg, Param};

use crate::buffer::{Buffer, BufferError};
use crate::command::{CommandError, CommandRegistry};
use crate::cursor::Cursor;
use crate::keymap::{encode_key, Binding, Keymap, Lookup};
use crate::lua::LuaHost;
use crate::mode::Mode;
use crate::render::{compose, gutter_width, status_line, Composed, View};
use crate::screen::{scroll_to_cursor, Viewport};
use crate::CoreError;

/// Guard against a Lua command calling itself forever.
pub const MAX_COMMAND_DEPTH: usize = 100;

/// User-settable editor options (also reachable from Lua via `ed.set`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Options {
    /// Columns a tab advances to.
    pub tabstop: usize,
    /// Whether to draw a line-number gutter.
    pub number: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            tabstop: 4,
            number: false,
        }
    }
}

pub struct Editor {
    pub buffer: Buffer,
    pub cursor: Cursor,
    pub mode: Mode,
    pub viewport: Viewport,
    /// The `:` command line being typed.
    pub cmdline: String,
    /// Transient status message shown on the bottom row.
    pub message: String,
    pub registry: CommandRegistry,
    pub keymap: Rc<RefCell<Keymap>>,
    pub options: Rc<RefCell<Options>>,
    /// Keys of a partially-typed multi-key binding (`d` waiting for `d`).
    pub pending: String,
    pub should_quit: bool,
    /// Set whenever the screen may have changed; the daemon uses it to decide
    /// whether to emit a redraw.
    pub redraw: bool,
    pub lua: LuaHost,
}

impl Editor {
    /// Build an editor, optionally sourcing `init_path` as Lua.
    ///
    /// A broken `init.lua` is reported on the status line rather than being
    /// fatal: a config typo must not stop the editor from opening.
    pub fn new(init_path: Option<&Path>) -> Result<Self, CoreError> {
        let registry = CommandRegistry::with_builtins();
        let keymap = Rc::new(RefCell::new(Keymap::with_defaults()));
        let options = Rc::new(RefCell::new(Options::default()));
        let lua = LuaHost::new(&registry, Rc::clone(&keymap), Rc::clone(&options))?;
        let mut editor = Self {
            buffer: Buffer::empty(),
            cursor: Cursor::default(),
            mode: Mode::Normal,
            viewport: Viewport::new(80, 24),
            cmdline: String::new(),
            message: String::new(),
            registry,
            keymap,
            options,
            pending: String::new(),
            should_quit: false,
            redraw: true,
            lua,
        };
        if let Some(path) = init_path {
            if path.exists() {
                if let Err(e) = editor.lua.load_init(path) {
                    editor.message = format!("init.lua: {e}");
                }
            }
        }
        Ok(editor)
    }

    /// A default editor with no Lua config, for tests.
    #[cfg(test)]
    pub fn for_tests() -> Self {
        Self::new(None).expect("editor construction cannot fail without a config file")
    }

    // -- state transitions --------------------------------------------------

    /// Feed one key press through the mode state machine.
    pub fn handle_key(&mut self, key: KeyMsg) {
        // A message lives until the next key *outside* command mode; the key
        // that finishes a `:` command must not erase what it just reported.
        if self.mode != Mode::Command {
            self.message.clear();
        }
        match self.mode {
            Mode::Normal => self.handle_normal(key),
            Mode::Insert => self.handle_insert(key),
            Mode::Command => self.handle_command_line(key),
        }
        self.redraw = true;
    }

    /// The terminal changed size.
    pub fn handle_resize(&mut self, cols: u16, rows: u16) {
        self.viewport.resize(cols, rows);
        self.reposition();
        self.redraw = true;
    }

    /// A programmatic command, as sent over RPC.
    pub fn handle_command(&mut self, name: &str, params: &[Param]) {
        self.run_command(name, params);
        self.redraw = true;
    }

    /// Replace the buffer with the contents of `path`.
    pub fn load_file(&mut self, path: &Path) -> Result<(), BufferError> {
        let buffer = Buffer::from_path(path)?;
        self.buffer = buffer;
        self.cursor = Cursor::default();
        self.viewport.top_line = 0;
        self.viewport.left_col = 0;
        self.mode = Mode::Normal;
        self.cmdline.clear();
        self.pending.clear();
        self.message = format!("\"{}\"", self.buffer.display_name());
        self.redraw = true;
        Ok(())
    }

    // -- rendering ----------------------------------------------------------

    /// The status line text.
    pub fn status(&self) -> String {
        status_line(
            self.mode,
            &self.buffer.display_name(),
            self.buffer.is_dirty(),
            self.cursor,
        )
    }

    /// Compose the whole screen.
    pub fn compose(&self) -> Composed {
        let options = *self.options.borrow();
        compose(&View {
            buffer: &self.buffer,
            cursor: self.cursor,
            mode: self.mode,
            viewport: self.viewport,
            tabstop: options.tabstop,
            number: options.number,
            status: self.status(),
            cmdline: Some(self.cmdline.clone()),
            message: self.message.clone(),
        })
    }

    // -- helpers used by commands ------------------------------------------

    /// Call after anything that may have moved the cursor or changed the
    /// buffer: it re-clamps the cursor, scrolls it back into view and marks the
    /// screen dirty.
    pub fn after_edit(&mut self) {
        self.cursor.clamp(&self.buffer);
        self.clamp_normal_cursor();
        self.reposition();
        self.redraw = true;
    }

    fn reposition(&mut self) {
        let gutter = gutter_width(self.buffer.line_count(), self.options.borrow().number);
        scroll_to_cursor(&mut self.viewport, self.cursor, &self.buffer, gutter);
    }

    /// Keep the cursor off the line terminator.
    ///
    /// Insert mode belongs *between* characters, so `col` may sit one past the
    /// last one. Normal mode does not: vim puts the cursor on the last
    /// character, which is also what makes `x`, `$` and `l` behave.
    fn clamp_normal_cursor(&mut self) {
        if self.mode != Mode::Normal {
            return;
        }
        let line_len = self.buffer.line_len(self.cursor.row);
        self.cursor.col = if line_len == 0 {
            0
        } else {
            self.cursor.col.min(line_len - 1)
        };
    }

    /// Run a named command, then drain anything Lua queued while running it.
    pub fn run_command(&mut self, name: &str, params: &[Param]) {
        let mut next = Some((name.to_string(), params.to_vec()));
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
        self.redraw = true;
    }

    fn run_named(&mut self, name: &str, params: &[Param]) -> Result<(), String> {
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

    fn handle_normal(&mut self, key: KeyMsg) {
        self.clamp_normal_cursor();
        let encoded = encode_key(&key);
        let sequence = if self.pending.is_empty() {
            encoded.clone()
        } else {
            format!("{} {}", self.pending, encoded)
        };
        match self.lookup_normal(&sequence) {
            Lookup::Exact(binding) => {
                self.pending.clear();
                self.run_binding(binding);
            }
            Lookup::Prefix => self.pending = sequence,
            Lookup::None => {
                if self.pending.is_empty() {
                    self.unmapped(&encoded);
                    return;
                }
                // The pending key did not start a binding; retry this key alone.
                self.pending.clear();
                match self.lookup_normal(&encoded) {
                    Lookup::Exact(binding) => self.run_binding(binding),
                    Lookup::Prefix => self.pending = encoded,
                    Lookup::None => self.unmapped(&encoded),
                }
            }
        }
        // Motions and edits may have left the cursor on the terminator.
        self.clamp_normal_cursor();
    }

    fn lookup_normal(&self, sequence: &str) -> Lookup {
        let keymap = self.keymap.borrow();
        keymap.lookup(Mode::Normal, sequence)
    }

    fn run_binding(&mut self, binding: Binding) {
        for name in binding.commands {
            self.run_command(&name, &[]);
            if self.should_quit {
                break;
            }
        }
    }

    fn unmapped(&mut self, encoded: &str) {
        self.message = format!("unmapped key: {encoded}");
    }

    fn handle_insert(&mut self, key: KeyMsg) {
        let encoded = encode_key(&key);
        // An explicit insert-mode mapping wins over typing the character.
        let lookup = {
            let keymap = self.keymap.borrow();
            keymap.lookup(Mode::Insert, &encoded)
        };
        if let Lookup::Exact(binding) = lookup {
            self.run_binding(binding);
            return;
        }
        if key.is_ctrl() || key.is_alt() {
            return;
        }
        match key.code {
            KeyCode::Esc => self.run_command("escape", &[]),
            KeyCode::Enter => self.insert_text("\n"),
            KeyCode::Tab => self.insert_text("\t"),
            KeyCode::Backspace => self.delete_backward(),
            KeyCode::Char(c) => self.insert_text(&c.to_string()),
            // Everything else (arrows, Home/End, Delete, ...) is an ordinary
            // insert-mode binding and was handled by the lookup above.
            _ => {}
        }
    }

    fn insert_text(&mut self, text: &str) {
        let idx = self.cursor.char_idx(&self.buffer);
        self.buffer.insert(idx, text);
        let end = idx + text.chars().count();
        self.cursor.set_char_idx(&self.buffer, end);
        self.after_edit();
    }

    fn delete_backward(&mut self) {
        let idx = self.cursor.char_idx(&self.buffer);
        if idx == 0 {
            return;
        }
        // At the start of a line, backspace joins it with the line above, and
        // the join takes the whole terminator with it -- otherwise a CRLF file
        // would keep a stray carriage return in the middle of the joined line.
        let start = if self.cursor.col == 0 && self.cursor.row > 0 {
            idx - self.buffer.line_terminator_len(self.cursor.row - 1).max(1)
        } else {
            self.buffer.prev_grapheme_boundary(idx)
        };
        self.buffer.remove(start, idx);
        self.cursor.set_char_idx(&self.buffer, start);
        self.after_edit();
    }

    fn handle_command_line(&mut self, key: KeyMsg) {
        match key.code {
            KeyCode::Esc => {
                self.mode = Mode::Normal;
                self.cmdline.clear();
            }
            KeyCode::Enter => {
                let line = std::mem::take(&mut self.cmdline);
                self.mode = Mode::Normal;
                self.execute_cmdline(&line);
            }
            KeyCode::Backspace => {
                self.cmdline.pop();
            }
            KeyCode::Char(c) if !key.is_ctrl() && !key.is_alt() => self.cmdline.push(c),
            _ => {}
        }
    }

    /// Run a `:` command line.
    pub fn execute_cmdline(&mut self, line: &str) {
        let line = line.trim();
        if line.is_empty() {
            return;
        }
        let (cmd, arg) = match line.split_once(char::is_whitespace) {
            Some((cmd, arg)) => (cmd, arg.trim()),
            None => (line, ""),
        };
        let params = if arg.is_empty() {
            Vec::new()
        } else {
            vec![Param::Str(arg.to_string())]
        };
        let name = match cmd {
            "w" => "save",
            "q" => "quit",
            "wq" | "x" => "save_quit",
            "q!" => "quit_force",
            "e" => "edit_file",
            "e!" => "edit_file_force",
            other => {
                self.message = format!("E492: Not a command: {other}");
                return;
            }
        };
        self.run_command(name, &params);
    }

    /// The default `init.lua` location, if the platform has one.
    pub fn default_init_path() -> Option<PathBuf> {
        LuaHost::default_init_path()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keymap::parse_key;

    fn feed(editor: &mut Editor, notation: &str) {
        for key in parse_key(notation).unwrap() {
            editor.handle_key(key);
        }
    }

    #[test]
    fn typing_in_insert_mode_edits_the_buffer() {
        let mut ed = Editor::for_tests();
        feed(&mut ed, "ihello<Esc>");
        assert_eq!(ed.buffer.text(), "hello\n");
        assert_eq!(ed.mode, Mode::Normal);
        assert_eq!(ed.cursor, Cursor::new(0, 4), "escape steps back one column");
        assert!(ed.buffer.is_dirty());
        assert!(!ed.message.contains("unmapped"), "{}", ed.message);
    }

    #[test]
    fn escape_only_steps_back_when_leaving_insert() {
        let mut ed = Editor::for_tests();
        feed(&mut ed, "ii");
        assert_eq!(ed.buffer.text(), "i\n");
        feed(&mut ed, "<Esc>");
        assert_eq!(ed.cursor, Cursor::new(0, 0));
        // Pressing escape again in normal mode leaves the cursor alone.
        feed(&mut ed, "<Esc>");
        assert_eq!(ed.cursor, Cursor::new(0, 0));
    }

    #[test]
    fn enter_and_backspace_in_insert_mode() {
        let mut ed = Editor::for_tests();
        feed(&mut ed, "iab<CR>cd<Esc>");
        assert_eq!(ed.buffer.text(), "ab\ncd\n");
        assert_eq!(ed.cursor, Cursor::new(1, 1));

        // Backspace eats the character before the cursor, one grapheme at a time.
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("abc\n");
        ed.cursor = Cursor::new(0, 2);
        feed(&mut ed, "i<BS><Esc>");
        assert_eq!(ed.buffer.text(), "ac\n");
        assert_eq!(
            ed.cursor,
            Cursor::new(0, 0),
            "insert mode ended, so the cursor stepped back"
        );
        feed(&mut ed, "li<BS><Esc>");
        assert_eq!(ed.buffer.text(), "c\n");
        assert_eq!(ed.cursor, Cursor::new(0, 0));
    }

    #[test]
    fn backspace_at_the_start_of_a_line_joins_lines() {
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("ab\ncd\n");
        ed.cursor = Cursor::new(1, 0);
        feed(&mut ed, "i<BS><Esc>");
        assert_eq!(ed.buffer.text(), "abcd\n");
        assert_eq!(ed.cursor, Cursor::new(0, 1));
    }

    #[test]
    fn movement_keys() {
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("one two\nthree\n");
        ed.cursor = Cursor::new(0, 0);
        feed(&mut ed, "w");
        assert_eq!(ed.cursor, Cursor::new(0, 4));
        feed(&mut ed, "l");
        assert_eq!(ed.cursor, Cursor::new(0, 5));
        feed(&mut ed, "$");
        assert_eq!(ed.cursor.col, 6, "`$` lands on the last character");
        feed(&mut ed, "0");
        assert_eq!(ed.cursor.col, 0);
        feed(&mut ed, "j");
        assert_eq!(ed.cursor.row, 1);
        feed(&mut ed, "G");
        assert_eq!(ed.cursor.row, 1);
        feed(&mut ed, "gg");
        assert_eq!(ed.cursor, Cursor::new(0, 0));
    }

    #[test]
    fn cursor_keys_work_too() {
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("ab\ncd\n");
        feed(&mut ed, "<Down><Right>");
        assert_eq!(ed.cursor, Cursor::new(1, 1));
        feed(&mut ed, "<Home>");
        assert_eq!(ed.cursor, Cursor::new(1, 0));
        feed(&mut ed, "<End>");
        assert_eq!(ed.cursor, Cursor::new(1, 1), "`<End>` is vim's `$`");
    }

    #[test]
    fn delete_char_and_delete_line() {
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("abc\ndef\n");
        ed.cursor = Cursor::new(0, 0);
        feed(&mut ed, "x");
        assert_eq!(ed.buffer.text(), "bc\ndef\n");
        feed(&mut ed, "dd");
        assert_eq!(ed.buffer.text(), "def\n");
        feed(&mut ed, "dd");
        assert_eq!(ed.buffer.text(), "\n");
        feed(&mut ed, "dd");
        assert_eq!(
            ed.buffer.text(),
            "\n",
            "the last line cannot be removed entirely"
        );
    }

    #[test]
    fn a_normal_mode_cursor_on_the_line_end_snaps_back_to_the_last_character() {
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("ab\ncd\n");
        // Even if something parks the cursor on the terminator...
        ed.cursor = Cursor::new(0, 2);
        feed(&mut ed, "x");
        // ...the next normal-mode key first brings it back onto `b`.
        assert_eq!(ed.buffer.text(), "a\ncd\n");
        assert_eq!(ed.cursor, Cursor::new(0, 0));
    }

    #[test]
    fn open_line_commands_enter_insert_mode() {
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("one\ntwo\n");
        ed.cursor = Cursor::new(0, 1);
        feed(&mut ed, "o");
        assert_eq!(ed.mode, Mode::Insert);
        assert_eq!(ed.buffer.text(), "one\n\ntwo\n");
        assert_eq!(ed.cursor, Cursor::new(1, 0));
        // Leaving insert mode steps back onto the line above, as in vim.
        feed(&mut ed, "<Esc>");
        assert_eq!(ed.cursor, Cursor::new(0, 2));
        feed(&mut ed, "O");
        assert_eq!(ed.buffer.text(), "\none\n\ntwo\n");
        assert_eq!(ed.cursor, Cursor::new(0, 0));
        assert_eq!(ed.mode, Mode::Insert);
    }

    #[test]
    fn append_positions_the_cursor_after_the_current_character() {
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("ab\n");
        ed.cursor = Cursor::new(0, 0);
        feed(&mut ed, "aX<Esc>");
        assert_eq!(ed.buffer.text(), "aXb\n");
        assert_eq!(ed.cursor, Cursor::new(0, 1));
    }

    #[test]
    fn command_line_collects_edits_and_executes_on_enter() {
        let mut ed = Editor::for_tests();
        feed(&mut ed, ":");
        assert_eq!(ed.mode, Mode::Command);
        feed(&mut ed, "wq");
        assert_eq!(ed.cmdline, "wq");
        feed(&mut ed, "<BS>");
        assert_eq!(ed.cmdline, "w");
        feed(&mut ed, "<Esc>");
        assert_eq!(ed.mode, Mode::Normal);
        assert_eq!(ed.cmdline, "");
    }

    #[test]
    fn writing_without_a_file_name_reports_e32() {
        let mut ed = Editor::for_tests();
        feed(&mut ed, ":w<CR>");
        assert_eq!(ed.message, "E32: No file name");
        assert_eq!(ed.mode, Mode::Normal);
        assert!(!ed.should_quit);
    }

    #[test]
    fn quitting_a_dirty_buffer_reports_e37_and_stays() {
        let mut ed = Editor::for_tests();
        feed(&mut ed, "ix<Esc>");
        assert!(ed.buffer.is_dirty());
        feed(&mut ed, ":q<CR>");
        assert!(!ed.should_quit);
        assert!(ed.message.starts_with("E37"), "{}", ed.message);
        feed(&mut ed, ":q!<CR>");
        assert!(ed.should_quit);
    }

    #[test]
    fn unknown_command_line_reports_e492() {
        let mut ed = Editor::for_tests();
        feed(&mut ed, ":frobnicate<CR>");
        assert_eq!(ed.message, "E492: Not a command: frobnicate");
    }

    #[test]
    fn a_clean_buffer_quits_immediately() {
        let mut ed = Editor::for_tests();
        feed(&mut ed, ":q<CR>");
        assert_eq!(ed.mode, Mode::Normal, "we are back in normal mode");
        assert!(ed.cmdline.is_empty());
        assert!(ed.should_quit);
    }

    #[test]
    fn unmapped_keys_report_and_clear_pending() {
        let mut ed = Editor::for_tests();
        feed(&mut ed, "z");
        assert_eq!(ed.message, "unmapped key: z");
        feed(&mut ed, "d");
        assert_eq!(ed.pending, "d");
        feed(&mut ed, "z");
        assert_eq!(ed.pending, "");
        assert_eq!(ed.message, "unmapped key: z");
    }

    #[test]
    fn a_message_survives_until_the_next_key() {
        let mut ed = Editor::for_tests();
        feed(&mut ed, ":q!<CR>");
        assert!(ed.should_quit);
        ed.should_quit = false;
        assert_eq!(ed.message, "", "the quit command does not set a message");
        ed.message = "hello".into();
        feed(&mut ed, "l");
        assert_eq!(ed.message, "");
    }

    #[test]
    fn resize_scrolls_the_cursor_into_view() {
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text(&"line\n".repeat(50));
        ed.cursor = Cursor::new(49, 0);
        ed.viewport = Viewport::new(80, 10);
        ed.handle_resize(80, 10);
        assert_eq!(ed.viewport.text_rows(), 8);
        assert!(ed.viewport.top_line > 0);
        assert!(ed.cursor.row < ed.viewport.top_line + 8);
    }

    #[test]
    fn composing_after_editing_shows_the_text() {
        let mut ed = Editor::for_tests();
        ed.handle_resize(40, 4);
        feed(&mut ed, "ihi<Esc>");
        let out = ed.compose();
        assert_eq!(out.grid.row_text(0).trim_end(), "hi");
        let status = out.grid.row_text(2);
        assert!(status.starts_with("-- NORMAL --"), "{status}");
        assert!(status.contains("[No Name][+]"), "{status}");
    }

    #[test]
    fn cursor_is_visible_inside_the_composed_grid() {
        let mut ed = Editor::for_tests();
        ed.handle_resize(40, 4);
        feed(&mut ed, "ihello<Esc>");
        let out = ed.compose();
        assert!(out.cursor_visible);
        assert_eq!(out.cursor, (4, 0));
    }

    #[test]
    fn normal_mode_never_leaves_the_cursor_past_the_last_character() {
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("abc\n");
        feed(&mut ed, "$");
        assert_eq!(ed.cursor, Cursor::new(0, 2));
        feed(&mut ed, "l");
        assert_eq!(
            ed.cursor,
            Cursor::new(0, 2),
            "`l` cannot step onto the newline"
        );
        feed(&mut ed, "x");
        assert_eq!(
            ed.buffer.text(),
            "ab\n",
            "`x` deletes the character under the cursor"
        );
    }

    #[test]
    fn an_empty_line_pins_the_normal_cursor_to_column_zero() {
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("ab\n\ncd\n");
        ed.cursor = Cursor::new(1, 0);
        feed(&mut ed, "x");
        assert_eq!(
            ed.buffer.text(),
            "ab\ncd\n",
            "`x` on an empty line joins it downward"
        );
        assert_eq!(
            ed.cursor,
            Cursor::new(1, 0),
            "and the cursor stays on that line"
        );
    }

    #[test]
    fn insert_mode_may_sit_one_past_the_last_character() {
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("abc\n");
        ed.cursor = Cursor::new(0, 0);
        feed(&mut ed, "A");
        assert_eq!(ed.cursor, Cursor::new(0, 3));
        feed(&mut ed, "X<Esc>");
        assert_eq!(ed.buffer.text(), "abcX\n");
    }

    #[test]
    fn leaving_insert_mode_steps_back_onto_a_real_character() {
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("abc\n");
        ed.cursor = Cursor::new(0, 0);
        feed(&mut ed, "A<Esc>");
        assert_eq!(ed.cursor, Cursor::new(0, 2));
        assert_eq!(ed.mode, Mode::Normal);
    }

    #[test]
    fn backspace_at_the_start_of_a_crlf_line_takes_the_whole_terminator() {
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("foo\r\nbar\r\n");
        ed.cursor = Cursor::new(1, 0);
        feed(&mut ed, "i<BS><Esc>");
        assert_eq!(ed.buffer.text(), "foobar\r\n", "no stray carriage return");
    }

    #[test]
    fn the_delete_key_deletes_forward_in_insert_mode() {
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("abc\n");
        ed.cursor = Cursor::new(0, 1);
        feed(&mut ed, "i<Del><Esc>");
        assert_eq!(ed.buffer.text(), "ac\n");
    }

    #[test]
    fn arrow_keys_move_the_cursor_in_insert_mode() {
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("one\ntwo\n");
        ed.cursor = Cursor::new(0, 0);
        feed(&mut ed, "i");
        assert_eq!(ed.mode, Mode::Insert);

        feed(&mut ed, "<Right><Down>");
        assert_eq!(ed.cursor, Cursor::new(1, 1));
        assert_eq!(
            ed.mode,
            Mode::Insert,
            "navigating does not leave insert mode"
        );
        assert_eq!(ed.buffer.text(), "one\ntwo\n", "and it does not edit");

        feed(&mut ed, "<Up><Left><Home><End>");
        assert_eq!(ed.cursor, Cursor::new(0, 2));
        assert_eq!(ed.mode, Mode::Insert);

        // Typing after navigating inserts at the new position.
        feed(&mut ed, "X<Esc>");
        assert_eq!(ed.buffer.text(), "onXe\ntwo\n");
    }

    #[test]
    fn page_keys_scroll_a_screenful() {
        let mut ed = Editor::for_tests();
        ed.handle_resize(20, 10); // 8 text rows
        ed.buffer = Buffer::from_text(&"line\n".repeat(30));
        ed.cursor = Cursor::new(0, 0);

        feed(&mut ed, "<PageDown>");
        assert_eq!(ed.cursor.row, 7, "one screen minus a line of overlap");
        feed(&mut ed, "<PageDown>");
        assert_eq!(ed.cursor.row, 14);
        feed(&mut ed, "<PageUp>");
        assert_eq!(ed.cursor.row, 7);
        feed(&mut ed, "<PageDown><PageDown>");
        assert_eq!(ed.cursor.row, 21);
        feed(&mut ed, "<PageDown>");
        assert_eq!(ed.cursor.row, 28, "clamped to the last line");
    }

    #[test]
    fn a_failed_save_quit_does_not_quit() {
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("x\n");
        let dir = std::env::temp_dir().join("edit-editor-tests");
        std::fs::create_dir_all(&dir).unwrap();
        let impossible = dir.join("no-such-dir").join("deep.txt");

        ed.execute_cmdline(&format!("wq {}", impossible.display()));
        assert!(!ed.should_quit, "the session must survive a failed write");
        assert!(ed.message.contains("i/o error"), "{}", ed.message);
    }

    #[test]
    fn append_at_the_end_of_a_crlf_line_inserts_before_the_carriage_return() {
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("foo\r\nbar\r\n");
        ed.cursor = Cursor::new(0, 0);
        feed(&mut ed, "AX<Esc>");
        assert_eq!(ed.buffer.text(), "fooX\r\nbar\r\n");
    }

    #[test]
    fn deleting_a_crlf_line_takes_the_carriage_return_with_it() {
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("foo\r\nbar\r\n");
        ed.cursor = Cursor::new(0, 0);
        feed(&mut ed, "dd");
        assert_eq!(ed.buffer.text(), "bar\r\n");
    }

    #[test]
    fn opening_a_line_below_a_crlf_line_keeps_the_line_endings_intact() {
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("foo\r\nbar\r\n");
        ed.cursor = Cursor::new(0, 0);
        feed(&mut ed, "o<Esc>");
        assert_eq!(ed.buffer.text(), "foo\r\n\r\nbar\r\n");
    }

    #[test]
    fn horizontal_scroll_accounts_for_the_line_number_gutter() {
        let mut ed = Editor::for_tests();
        ed.options.borrow_mut().number = true;
        ed.handle_resize(20, 4);
        ed.buffer = Buffer::from_text(&"0123456789".repeat(10));
        ed.cursor = Cursor::new(0, 99);
        ed.after_edit();

        let out = ed.compose();
        assert!(out.cursor_visible);
        assert_eq!(out.cursor, (19, 0), "the cursor is in the last column");
        assert_eq!(
            out.grid.get(19, 0).text,
            "9",
            "the last column shows the character under the cursor"
        );
        assert_eq!(
            out.grid.get(2, 0).text,
            "2",
            "2 gutter columns, then col 82"
        );
    }

    #[test]
    fn horizontal_scroll_without_a_gutter_uses_the_whole_width() {
        let mut ed = Editor::for_tests();
        ed.handle_resize(20, 4);
        ed.buffer = Buffer::from_text(&"0123456789".repeat(10));
        ed.cursor = Cursor::new(0, 99);
        ed.after_edit();

        let out = ed.compose();
        assert_eq!(out.cursor, (19, 0));
        assert_eq!(out.grid.get(19, 0).text, "9");
    }

    #[test]
    fn a_zero_sized_terminal_does_not_corrupt_the_viewport() {
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text(&"line\n".repeat(10));
        ed.cursor = Cursor::new(5, 0);
        ed.handle_resize(0, 0);
        assert_eq!(ed.viewport.top_line, 5, "no meaningless scroll offset");
        assert_eq!(ed.viewport.left_col, 0);
        assert!(!ed.compose().cursor_visible, "nothing is visible");

        ed.handle_resize(80, 10);
        let out = ed.compose();
        assert!(out.cursor_visible);
        assert_eq!(out.cursor, (0, 3), "the cursor is back on screen");
    }

    #[test]
    fn a_zero_width_terminal_still_tracks_the_cursor_column() {
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text(&"wide line\n".repeat(10));
        ed.cursor = Cursor::new(5, 2);
        ed.handle_resize(0, 10);
        assert_eq!(
            ed.viewport.top_line, 0,
            "the cursor line is already on screen"
        );
        assert_eq!(ed.viewport.left_col, 2, "no room to scroll into, so pin it");
    }

    #[test]
    fn edit_refuses_to_discard_unsaved_changes() {
        let dir = std::env::temp_dir().join("edit-editor-tests");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("reload.txt");
        std::fs::write(&path, "on disk\n").unwrap();

        let mut ed = Editor::for_tests();
        ed.load_file(&path).unwrap();
        feed(&mut ed, "ix<Esc>");
        assert!(ed.buffer.is_dirty());

        ed.execute_cmdline(&format!("e {}", path.display()));
        assert!(ed.message.starts_with("E37"), "{}", ed.message);
        assert_eq!(ed.buffer.text(), "xon disk\n", "the edits are still here");

        ed.execute_cmdline(&format!("e! {}", path.display()));
        assert_eq!(ed.buffer.text(), "on disk\n", "`e!` reloads from disk");
        assert!(!ed.buffer.is_dirty());
    }

    #[test]
    fn edit_on_a_clean_buffer_reloads() {
        let dir = std::env::temp_dir().join("edit-editor-tests");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("clean.txt");
        std::fs::write(&path, "one\n").unwrap();

        let mut ed = Editor::for_tests();
        ed.load_file(&path).unwrap();
        std::fs::write(&path, "two\n").unwrap();
        ed.execute_cmdline(&format!("e {}", path.display()));
        assert_eq!(ed.buffer.text(), "two\n");
    }

    /// Deterministic pseudo-random key sequence: nothing may panic, and the
    /// buffer/viewport invariants must hold after every single key.
    #[test]
    fn random_key_sequences_keep_the_editor_consistent() {
        use edit_rpc::{KeyCode, KeyMsg};

        let alphabet: Vec<KeyMsg> = "iIaAoOxddwbgG:"
            .chars()
            .map(|c| KeyMsg::new(KeyCode::Char(c)))
            .chain([
                KeyMsg::new(KeyCode::Esc),
                KeyMsg::new(KeyCode::Enter),
                KeyMsg::new(KeyCode::Backspace),
                KeyMsg::new(KeyCode::Tab),
                KeyMsg::new(KeyCode::Left),
                KeyMsg::new(KeyCode::Right),
                KeyMsg::new(KeyCode::Up),
                KeyMsg::new(KeyCode::Down),
                KeyMsg::new(KeyCode::Home),
                KeyMsg::new(KeyCode::End),
                KeyMsg::new(KeyCode::Delete),
                KeyMsg::ctrl('w'),
            ])
            .collect();

        let mut ed = Editor::for_tests();
        ed.handle_resize(24, 6);
        let mut seed = 0x2545_f491_4f6c_dd1du64;

        for step in 0..4000 {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let r = seed as usize;

            if r % 19 == 0 {
                ed.handle_resize((r % 40) as u16, (r % 14) as u16);
            }
            let key = alphabet[r % alphabet.len()].clone();
            ed.handle_key(key);
            ed.should_quit = false; // `:q!` must not end the run

            let text = ed.buffer.text();
            assert!(
                text.ends_with('\n'),
                "step {step}: buffer lost its final newline"
            );
            assert!(ed.buffer.line_count() >= 1, "step {step}");
            assert!(
                ed.cursor.row < ed.buffer.line_count(),
                "step {step}: row {} of {}",
                ed.cursor.row,
                ed.buffer.line_count()
            );
            assert!(
                ed.cursor.col <= ed.buffer.line_len(ed.cursor.row),
                "step {step}: col {} past line length {}",
                ed.cursor.col,
                ed.buffer.line_len(ed.cursor.row)
            );
            if ed.mode == Mode::Normal {
                let len = ed.buffer.line_len(ed.cursor.row);
                assert!(
                    ed.cursor.col < len.max(1),
                    "step {step}: normal mode cursor sits past the last character"
                );
            }
            let composed = ed.compose();
            assert_eq!(
                composed.grid.cells.len(),
                composed.grid.width as usize * composed.grid.height as usize
            );
            if composed.cursor_visible {
                assert!(composed.cursor.0 < composed.grid.width.max(1));
                assert!(composed.cursor.1 < composed.grid.height.max(1));
            }
        }
    }
}
