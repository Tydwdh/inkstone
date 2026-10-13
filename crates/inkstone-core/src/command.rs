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

use inkstone_rpc::Param;

use crate::editor::Editor;
use crate::mode::Mode;

/// What a command returns: `Err` carries the message the user should see.
pub type Result<T, E = CommandError> = std::result::Result<T, E>;

/// Why a command did not run.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CommandError {
    /// No command by that name, in the registry or in Lua.
    #[error("E492: Not a command: {0}")]
    Unknown(String),
    /// The command ran and refused, for example a write that failed. The message is
    /// shown to the user as-is.
    #[error("{0}")]
    Failed(String),
}

/// The shape of every built-in command.
///
/// Plain functions, not closures, which is why the registry stays a table a script
/// layer can enumerate and dispatch into.
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

    commands! {
        noop,
        move_left,
        move_right,
        move_up,
        move_down,
        move_word_forward,
        move_word_backward,
        move_line_start,
        move_line_end,
        goto_first_line,
        goto_last_line,
        page_up,
        page_down,
        enter_insert,
        enter_insert_before,
        append,
        append_eol,
        open_line_below,
        open_line_above,
        delete_char,
        delete_line,
        enter_command,
        escape,
        save,
        quit,
        save_quit,
        quit_force,
        edit_file,
        edit_file_force,
    }

    /// Install (or replace) a command.
    ///
    /// Built-ins go through the `commands!` macro instead, so the name and the
    /// function cannot disagree.
    pub fn register(&mut self, name: &'static str, f: CommandFn) {
        self.map.insert(name, f);
    }

    /// Whether a command by this name exists.
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

fn noop(_editor: &mut Editor, _params: &[Param]) -> Result<()> {
    Ok(())
}

fn move_left(editor: &mut Editor, params: &[Param]) -> Result<()> {
    for _ in 0..count(params) {
        editor.cursor.move_left(&editor.buffer);
    }
    editor.after_edit();
    Ok(())
}

fn move_right(editor: &mut Editor, params: &[Param]) -> Result<()> {
    for _ in 0..count(params) {
        editor.cursor.move_right(&editor.buffer);
    }
    editor.after_edit();
    Ok(())
}

fn move_up(editor: &mut Editor, params: &[Param]) -> Result<()> {
    for _ in 0..count(params) {
        editor.cursor.move_up(&editor.buffer);
    }
    editor.after_edit();
    Ok(())
}

fn move_down(editor: &mut Editor, params: &[Param]) -> Result<()> {
    for _ in 0..count(params) {
        editor.cursor.move_down(&editor.buffer);
    }
    editor.after_edit();
    Ok(())
}

fn move_word_forward(editor: &mut Editor, params: &[Param]) -> Result<()> {
    for _ in 0..count(params) {
        editor.cursor.move_word_forward(&editor.buffer);
    }
    editor.after_edit();
    Ok(())
}

fn move_word_backward(editor: &mut Editor, params: &[Param]) -> Result<()> {
    for _ in 0..count(params) {
        editor.cursor.move_word_backward(&editor.buffer);
    }
    editor.after_edit();
    Ok(())
}

fn move_line_start(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    editor.cursor.move_line_start(&editor.buffer);
    editor.after_edit();
    Ok(())
}

fn move_line_end(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    editor.cursor.move_line_end(&editor.buffer);
    editor.after_edit();
    Ok(())
}

fn goto_first_line(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    editor.cursor.goto_first_line(&editor.buffer);
    editor.after_edit();
    Ok(())
}

fn goto_last_line(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    editor.cursor.goto_last_line(&editor.buffer);
    editor.after_edit();
    Ok(())
}

/// How far a page scroll moves: one screen less a line of overlap, so the
/// reader keeps their bearings. A repeat count scrolls that many screens.
fn page_step(editor: &Editor, params: &[Param]) -> usize {
    let rows = editor.viewport.text_rows() as usize;
    rows.saturating_sub(1).max(1) * count(params)
}

fn page_down(editor: &mut Editor, params: &[Param]) -> Result<()> {
    for _ in 0..page_step(editor, params) {
        editor.cursor.move_down(&editor.buffer);
    }
    editor.after_edit();
    Ok(())
}

fn page_up(editor: &mut Editor, params: &[Param]) -> Result<()> {
    for _ in 0..page_step(editor, params) {
        editor.cursor.move_up(&editor.buffer);
    }
    editor.after_edit();
    Ok(())
}

fn enter_insert(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    editor.mode = Mode::Insert;
    Ok(())
}

fn enter_insert_before(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    editor.cursor.col = 0;
    editor.mode = Mode::Insert;
    editor.after_edit();
    Ok(())
}

fn append(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    let end = editor.buffer.line_len(editor.cursor.row);
    editor.cursor.col = (editor.cursor.col + 1).min(end);
    editor.mode = Mode::Insert;
    editor.after_edit();
    Ok(())
}

fn append_eol(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    editor.cursor.col = editor.buffer.line_len(editor.cursor.row);
    editor.mode = Mode::Insert;
    editor.after_edit();
    Ok(())
}

fn open_line_below(editor: &mut Editor, _params: &[Param]) -> Result<()> {
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

fn open_line_above(editor: &mut Editor, _params: &[Param]) -> Result<()> {
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

fn delete_char(editor: &mut Editor, params: &[Param]) -> Result<()> {
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

fn delete_line(editor: &mut Editor, params: &[Param]) -> Result<()> {
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

fn enter_command(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    editor.mode = Mode::Command;
    editor.cmdline.clear();
    editor.pending.clear();
    editor.message.clear();
    Ok(())
}

fn escape(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    let was_insert = editor.mode == Mode::Insert;
    editor.mode = Mode::Normal;
    editor.pending.clear();
    if was_insert {
        editor.cursor.move_left(&editor.buffer);
        editor.after_edit();
    }
    Ok(())
}

fn save(editor: &mut Editor, params: &[Param]) -> Result<()> {
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

fn quit(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    if editor.buffer.is_dirty() {
        let msg = "E37: No write since last change (add ! to override)".to_string();
        editor.message = msg.clone();
        return Err(CommandError::Failed(msg));
    }
    editor.should_quit = true;
    Ok(())
}

fn quit_force(editor: &mut Editor, _params: &[Param]) -> Result<()> {
    editor.should_quit = true;
    Ok(())
}

fn save_quit(editor: &mut Editor, params: &[Param]) -> Result<()> {
    save(editor, params)?;
    editor.should_quit = true;
    Ok(())
}

fn edit_file(editor: &mut Editor, params: &[Param]) -> Result<()> {
    if editor.buffer.is_dirty() {
        let msg = "E37: No write since last change (add ! to override)".to_string();
        editor.message = msg.clone();
        return Err(CommandError::Failed(msg));
    }
    reload_file(editor, params)
}

/// `:e!` — reload from disk, discarding unsaved changes on purpose.
fn edit_file_force(editor: &mut Editor, params: &[Param]) -> Result<()> {
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
    fn builtins_are_present() {
        let registry = CommandRegistry::with_builtins();
        for name in [
            "delete_line",
            "save_quit",
            "edit_file_force",
            "page_down",
            "move_word_backward",
        ] {
            assert!(registry.contains(name), "{name} is missing");
        }
        assert!(!registry.contains("nope"));
        // Every default binding must resolve to a real command.
        let keymap = crate::keymap::Keymap::with_defaults();
        for (_, _, commands) in keymap.bindings() {
            for command in commands {
                assert!(registry.contains(&command), "unbound command: {command}");
            }
        }
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
