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
use crate::render::{compose, status_line, Composed, View};
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

    /// The buffer changed: re-clamp the cursor, scroll and schedule a redraw.
    pub fn after_edit(&mut self) {
        self.cursor.clamp(&self.buffer);
        self.reposition();
        self.redraw = true;
    }

    /// The cursor moved: same bookkeeping as [`Editor::after_edit`].
    pub fn cursor_settled(&mut self) {
        self.after_edit();
    }

    fn reposition(&mut self) {
        scroll_to_cursor(&mut self.viewport, self.cursor, &self.buffer);
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
        let start = self.buffer.prev_grapheme_boundary(idx);
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
        assert_eq!(ed.cursor.col, 7);
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
        assert_eq!(ed.cursor, Cursor::new(1, 2));
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
    fn delete_char_at_the_line_end_joins_lines() {
        let mut ed = Editor::for_tests();
        ed.buffer = Buffer::from_text("ab\ncd\n");
        ed.cursor = Cursor::new(0, 2);
        feed(&mut ed, "x");
        assert_eq!(ed.buffer.text(), "abcd\n");
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
        assert_eq!(ed.cursor, Cursor::new(0, 3));
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
}
