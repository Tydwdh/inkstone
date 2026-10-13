//! The command registry: every atomic editor operation, addressable by name.
//!
//! Commands are plain `fn(&mut Editor, &[Param]) -> Result<()>`. Keeping them
//! as functions (rather than closures) means the registry stays a flat table
//! that a script layer can enumerate and dispatch into, and means a command
//! can never capture editor state.
//!
//! Commands with a numeric first parameter take it as a repeat count, so `3x`
//! and `dd` (count 1) go through the same code path.

use std::collections::HashMap;
use std::path::Path;

use edit_rpc::Param;

use crate::editor::Editor;
use crate::mode::Mode;

pub type Result<T, E = CommandError> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CommandError {
    #[error("E492: Not a command: {0}")]
    Unknown(String),
    #[error("{0}")]
    Failed(String),
}

pub type CommandFn = fn(&mut Editor, &[Param]) -> Result<()>;

/// The set of built-in commands.
#[derive(Debug, Clone)]
pub struct CommandRegistry {
    map: HashMap<&'static str, CommandFn>,
}

impl Default for CommandRegistry {
    fn default() -> Self {
        Self::with_builtins()
    }
}

impl CommandRegistry {
    /// An empty registry.
    pub fn new() -> Self {
        Self {
            map: HashMap::new(),
        }
    }

    /// The registry with every built-in command installed.
    pub fn with_builtins() -> Self {
        let mut registry = Self::new();
        registry.register("noop", cmd_noop);
        registry.register("move_left", cmd_move_left);
        registry.register("move_right", cmd_move_right);
        registry.register("move_up", cmd_move_up);
        registry.register("move_down", cmd_move_down);
        registry.register("move_word_forward", cmd_move_word_forward);
        registry.register("move_word_backward", cmd_move_word_backward);
        registry.register("move_line_start", cmd_move_line_start);
        registry.register("move_line_end", cmd_move_line_end);
        registry.register("goto_first_line", cmd_goto_first_line);
        registry.register("goto_last_line", cmd_goto_last_line);
        registry.register("page_up", cmd_page_up);
        registry.register("page_down", cmd_page_down);
        registry.register("enter_insert", cmd_enter_insert);
        registry.register("enter_insert_before", cmd_enter_insert_before);
        registry.register("append", cmd_append);
        registry.register("append_eol", cmd_append_eol);
        registry.register("open_line_below", cmd_open_line_below);
        registry.register("open_line_above", cmd_open_line_above);
        registry.register("delete_char", cmd_delete_char);
        registry.register("delete_line", cmd_delete_line);
        registry.register("enter_command", cmd_enter_command);
        registry.register("escape", cmd_escape);
        registry.register("save", cmd_save);
        registry.register("quit", cmd_quit);
        registry.register("save_quit", cmd_save_quit);
        registry.register("quit_force", cmd_quit_force);
        registry.register("edit_file", cmd_edit_file);
        registry.register("edit_file_force", cmd_edit_file_force);
        registry
    }

    pub fn register(&mut self, name: &'static str, f: CommandFn) {
        self.map.insert(name, f);
    }

    pub fn contains(&self, name: &str) -> bool {
        self.map.contains_key(name)
    }

    /// The function for `name`, if registered.
    ///
    /// Returns the bare function pointer rather than a reference so that a
    /// caller holding `&Editor` can hand `&mut Editor` to the command.
    pub fn get(&self, name: &str) -> Option<CommandFn> {
        self.map.get(name).copied()
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// Every registered command name, sorted.
    pub fn names(&self) -> Vec<&'static str> {
        let mut names: Vec<_> = self.map.keys().copied().collect();
        names.sort_unstable();
        names
    }

    /// Run `name` against `editor`.
    pub fn run(&self, editor: &mut Editor, name: &str, params: &[Param]) -> Result<()> {
        match self.map.get(name) {
            Some(f) => f(editor, params),
            None => Err(CommandError::Unknown(name.to_string())),
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// The repeat count for a command: the first integer parameter, if positive.
fn count(params: &[Param]) -> usize {
    match params.first() {
        Some(Param::Int(n)) if *n > 0 => *n as usize,
        _ => 1,
    }
}

/// The first string parameter, as a path.
fn path_param(params: &[Param]) -> Option<&Path> {
    match params.first() {
        Some(Param::Str(s)) if !s.is_empty() => Some(Path::new(s)),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

fn cmd_noop(_editor: &mut Editor, _params: &[Param]) -> Result<()> {
    Ok(())
}

fn cmd_move_left(editor: &mut Editor, params: &[Param]) -> Result<()> {
    for _ in 0..count(params) {
        editor.cursor.move_left(&editor.buffer);
    }
    editor.cursor_settled();
    Ok(())
}

fn cmd_move_right(editor: &mut Editor, params: &[Param]) -> Result<()> {
    for _ in 0..count(params) {
        editor.cursor.move_right(&editor.buffer);
    }
    editor.cursor_settled();
    Ok(())
}

fn cmd_move_up(editor: &mut Editor, params: &[Param]) -> Result<()> {
    for _ in 0..count(params) {
        editor.cursor.move_up(&editor.buffer);
    }
    editor.cursor_settled();
    Ok(())
}

fn cmd_move_down(editor: &mut Editor, params: &[Param]) -> Result<()> {
    for _ in 0..count(params) {
        editor.cursor.move_down(&editor.buffer);
    }
    editor.cursor_settled();
    Ok(())
}

fn cmd_move_word_forward(editor: &mut Editor, params: &[Param]) -> Result<()> {
    for _ in 0..count(params) {
        editor.cursor.move_word_forward(&editor.buffer);
    }
    editor.cursor_settled();
    Ok(())
}

fn cmd_move_word_backward(editor: &mut Editor, params: &[Param]) -> Result<()> {
    for _ in 0..count(params) {
        editor.cursor.move_word_backward(&editor.buffer);
    }
    editor.cursor_settled();
    Ok(())
}

fn cmd_move_line_start(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    editor.cursor.move_line_start(&editor.buffer);
    editor.cursor_settled();
    Ok(())
}

fn cmd_move_line_end(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    editor.cursor.move_line_end(&editor.buffer);
    editor.cursor_settled();
    Ok(())
}

fn cmd_goto_first_line(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    editor.cursor.goto_first_line(&editor.buffer);
    editor.cursor_settled();
    Ok(())
}

fn cmd_goto_last_line(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    editor.cursor.goto_last_line(&editor.buffer);
    editor.cursor_settled();
    Ok(())
}

/// How far a page scroll moves: one screen less a line of overlap, so the
/// reader keeps their bearings. A repeat count scrolls that many screens.
fn page_step(editor: &Editor, params: &[Param]) -> usize {
    let rows = editor.viewport.text_rows() as usize;
    rows.saturating_sub(1).max(1) * count(params)
}

fn cmd_page_down(editor: &mut Editor, params: &[Param]) -> Result<()> {
    for _ in 0..page_step(editor, params) {
        editor.cursor.move_down(&editor.buffer);
    }
    editor.cursor_settled();
    Ok(())
}

fn cmd_page_up(editor: &mut Editor, params: &[Param]) -> Result<()> {
    for _ in 0..page_step(editor, params) {
        editor.cursor.move_up(&editor.buffer);
    }
    editor.cursor_settled();
    Ok(())
}

fn cmd_enter_insert(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    editor.mode = Mode::Insert;
    Ok(())
}

fn cmd_enter_insert_before(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    editor.cursor.col = 0;
    editor.mode = Mode::Insert;
    editor.cursor_settled();
    Ok(())
}

fn cmd_append(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    let end = editor.buffer.line_len(editor.cursor.row);
    editor.cursor.col = (editor.cursor.col + 1).min(end);
    editor.mode = Mode::Insert;
    editor.cursor_settled();
    Ok(())
}

fn cmd_append_eol(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    editor.cursor.col = editor.buffer.line_len(editor.cursor.row);
    editor.mode = Mode::Insert;
    editor.cursor_settled();
    Ok(())
}

fn cmd_open_line_below(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    let row = editor.cursor.row;
    let end_of_content = editor
        .buffer
        .line_col_to_char(row, editor.buffer.line_len(row));
    let terminator = open_line_terminator(editor, row);
    editor
        .buffer
        .insert(end_of_content + terminator.chars().count(), &terminator);
    editor.cursor.row = row + 1;
    editor.cursor.col = 0;
    editor.mode = Mode::Insert;
    editor.after_edit();
    Ok(())
}

fn cmd_open_line_above(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    let row = editor.cursor.row;
    let start = editor.buffer.line_col_to_char(row, 0);
    let terminator = open_line_terminator(editor, row);
    editor.buffer.insert(start, &terminator);
    editor.cursor.row = row;
    editor.cursor.col = 0;
    editor.mode = Mode::Insert;
    editor.after_edit();
    Ok(())
}

/// The terminator a newly opened line borrows from its neighbour, so that a
/// CRLF file never sprouts a bare LF.
fn open_line_terminator(editor: &Editor, row: usize) -> String {
    let terminator = editor.buffer.line_terminator(row);
    if terminator.is_empty() {
        "\n".to_string()
    } else {
        terminator
    }
}

fn cmd_delete_char(editor: &mut Editor, params: &[Param]) -> Result<()> {
    for _ in 0..count(params) {
        let row = editor.cursor.row;
        let line_len = editor.buffer.line_len(row);
        if editor.cursor.col >= line_len {
            // Sitting on the terminator (only possible on an empty line):
            // remove all of it and join with the line below. The last line has
            // nothing to join to.
            if row + 1 >= editor.buffer.line_count() {
                break;
            }
            let start = editor.buffer.line_col_to_char(row, line_len);
            let terminator = editor.buffer.line_terminator_len(row);
            if terminator == 0 {
                break;
            }
            editor.buffer.remove(start, start + terminator);
        } else {
            let idx = editor.cursor.char_idx(&editor.buffer);
            editor.buffer.remove(idx, idx + 1);
        }
    }
    editor.after_edit();
    Ok(())
}

fn cmd_delete_line(editor: &mut Editor, params: &[Param]) -> Result<()> {
    for _ in 0..count(params) {
        let row = editor.cursor.row.min(editor.buffer.line_count() - 1);
        let start = editor.buffer.line_col_to_char(row, 0);
        // Content plus its own terminator: enough to drop the whole line,
        // whatever the terminator is, without disturbing the line before it.
        let end = start + editor.buffer.line_len(row) + editor.buffer.line_terminator_len(row);
        editor.buffer.remove(start, end);
    }
    editor.after_edit();
    Ok(())
}

fn cmd_enter_command(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    editor.mode = Mode::Command;
    editor.cmdline.clear();
    editor.pending.clear();
    editor.message.clear();
    Ok(())
}

fn cmd_escape(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    let was_insert = editor.mode == Mode::Insert;
    editor.mode = Mode::Normal;
    editor.pending.clear();
    if was_insert {
        editor.cursor.move_left(&editor.buffer);
        editor.cursor_settled();
    }
    Ok(())
}

fn cmd_save(editor: &mut Editor, params: &[Param]) -> Result<()> {
    match editor.buffer.save(path_param(params)) {
        Ok(path) => {
            editor.message = format!("\"{}\" written", path.display());
            Ok(())
        }
        Err(e) => {
            editor.message = e.to_string();
            Err(CommandError::Failed(e.to_string()))
        }
    }
}

fn cmd_quit(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    if editor.buffer.is_dirty() {
        let msg = "E37: No write since last change (add ! to override)".to_string();
        editor.message = msg.clone();
        return Err(CommandError::Failed(msg));
    }
    editor.should_quit = true;
    Ok(())
}

fn cmd_quit_force(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    editor.should_quit = true;
    Ok(())
}

fn cmd_save_quit(editor: &mut Editor, params: &[Param]) -> Result<()> {
    cmd_save(editor, params)?;
    editor.should_quit = true;
    Ok(())
}

fn cmd_edit_file(editor: &mut Editor, params: &[Param]) -> Result<()> {
    if editor.buffer.is_dirty() {
        let msg = "E37: No write since last change (add ! to override)".to_string();
        editor.message = msg.clone();
        return Err(CommandError::Failed(msg));
    }
    reload_file(editor, params)
}

/// `:e!` — reload from disk, discarding unsaved changes on purpose.
fn cmd_edit_file_force(editor: &mut Editor, params: &[Param]) -> Result<()> {
    reload_file(editor, params)
}

fn reload_file(editor: &mut Editor, params: &[Param]) -> Result<()> {
    let path = match path_param(params) {
        Some(p) => p.to_path_buf(),
        None => match editor.buffer.path() {
            Some(p) => p.to_path_buf(),
            None => {
                let msg = "E32: No file name".to_string();
                editor.message = msg.clone();
                return Err(CommandError::Failed(msg));
            }
        },
    };
    match editor.load_file(&path) {
        Ok(()) => Ok(()),
        Err(e) => {
            let msg = e.to_string();
            editor.message = msg.clone();
            Err(CommandError::Failed(msg))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins_are_present_and_enumerable() {
        let registry = CommandRegistry::with_builtins();
        assert!(registry.contains("delete_line"));
        assert!(!registry.contains("nope"));
        let names = registry.names();
        assert!(names.contains(&"save_quit"));
        assert_eq!(names.len(), registry.len());
        assert!(names.windows(2).all(|w| w[0] < w[1]), "names are sorted");
    }

    #[test]
    fn unknown_commands_are_reported() {
        let registry = CommandRegistry::with_builtins();
        let mut editor = Editor::for_tests();
        let err = registry.run(&mut editor, "nope", &[]).unwrap_err();
        assert_eq!(err, CommandError::Unknown("nope".into()));
        assert_eq!(err.to_string(), "E492: Not a command: nope");
    }

    #[test]
    fn count_defaults_to_one() {
        assert_eq!(count(&[]), 1);
        assert_eq!(count(&[Param::Int(3)]), 3);
        assert_eq!(count(&[Param::Int(0)]), 1);
        assert_eq!(count(&[Param::Int(-2)]), 1);
        assert_eq!(count(&[Param::Str("x".into())]), 1);
    }
}
