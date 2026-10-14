//! The Lua script host.
//!
//! `init.lua` may remap keys, define new commands and set options — the emacs
//! half of the design. Everything is exposed on one global table, `ed`:
//!
//! ```lua
//! ed.set("number", true)
//! ed.set("tabstop", 8)
//!
//! ed.command("duplicate_line", function()
//!   ed.run("delete_line")
//! end)
//!
//! ed.map("n", "<C-s>", "save")                       -- bind to a built-in
//! ed.map("n", "Q", function() ed.run("quit") end)    -- bind to a function
//! ```
//!
//! # Why `ed.run` queues instead of calling
//! A Rust-side editor cannot be borrowed by a Lua callback that is itself
//! running *inside* a command (`Editor` owns the `Lua`, so the borrows would
//! alias). `ed.run` therefore pushes onto a queue that
//! `Editor::run_command` drains once the Lua
//! call returns. Nesting is bounded by `MAX_COMMAND_DEPTH`.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use mlua::{Function, Lua, Table, Value};

use inkstone_rpc::Param;

use crate::command::CommandRegistry;
use crate::keymap::Keymap;
use crate::mode::Mode;
use crate::settings::{Options, Setting};
use crate::CoreError;

/// Commands the Lua layer has registered, by name.
type LuaCommands = Rc<RefCell<HashMap<String, Function>>>;
/// Commands a Lua callback asked for, in order, to run once it returns.
type PendingCalls = Rc<RefCell<VecDeque<(String, Vec<Param>)>>>;

/// The Lua VM, and the handles that let scripts reach the editor.
///
/// See the module docs for why script calls are queued rather than borrowing the
/// editor.
pub struct LuaHost {
    lua: Lua,
    keymap: Rc<RefCell<Keymap>>,
    options: Rc<RefCell<Options>>,
    commands: LuaCommands,
    pending: PendingCalls,
}

impl LuaHost {
    /// Create a host with the `ed` API installed.
    ///
    /// `keymap` and `options` are shared with the editor so that scripts can
    /// change them at any time, not just while `init.lua` is being sourced.
    pub fn new(
        _registry: &CommandRegistry,
        keymap: Rc<RefCell<Keymap>>,
        options: Rc<RefCell<Options>>,
    ) -> Result<Self, CoreError> {
        let host = Self {
            lua: Lua::new(),
            keymap,
            options,
            commands: Rc::new(RefCell::new(HashMap::new())),
            pending: Rc::new(RefCell::new(VecDeque::new())),
        };
        host.install_api()?;
        Ok(host)
    }

    /// Where `init.lua` lives on this platform.
    pub fn default_init_path() -> Option<PathBuf> {
        directories::ProjectDirs::from("", "", "inkstone")
            .map(|dirs| dirs.config_dir().join("init.lua"))
    }

    /// Source a file as Lua.
    pub(crate) fn load_init(&self, path: &Path) -> Result<(), CoreError> {
        let source = std::fs::read_to_string(path)?;
        self.load_str(&path.to_string_lossy(), &source)
    }

    /// Evaluate Lua source, with `name` used in error messages.
    pub(crate) fn load_str(&self, name: &str, source: &str) -> Result<(), CoreError> {
        self.lua.load(source).set_name(name).exec()?;
        Ok(())
    }

    /// Whether a Lua command with this name exists.
    pub(crate) fn has_command(&self, name: &str) -> bool {
        self.commands.borrow().contains_key(name)
    }

    /// The names of every command registered from Lua.
    /// Only the tests ask this, to see what a script registered.
    #[cfg(test)]
    pub(crate) fn command_names(&self) -> Vec<String> {
        let mut names: Vec<_> = self.commands.borrow().keys().cloned().collect();
        names.sort();
        names
    }

    /// Call a Lua command. Anything it asks to run is queued, not executed.
    pub(crate) fn call_command(&self, name: &str, params: &[Param]) -> Result<(), CoreError> {
        let function = match self.commands.borrow().get(name).cloned() {
            Some(f) => f,
            None => return Err(CoreError::Message(format!("unknown Lua command: {name}"))),
        };
        let table = self.lua.create_table()?;
        for (i, param) in params.iter().enumerate() {
            table.set(i + 1, param_to_value(&self.lua, param)?)?;
        }
        function.call::<()>(table)?;
        Ok(())
    }

    /// The next command a Lua callback asked for, if any.
    pub fn take_pending(&self) -> Option<(String, Vec<Param>)> {
        self.pending.borrow_mut().pop_front()
    }

    fn install_api(&self) -> Result<(), CoreError> {
        let ed = self.lua.create_table()?;

        let commands = Rc::clone(&self.commands);
        ed.set(
            "command",
            self.lua
                .create_function(move |_, (name, function): (String, Function)| {
                    commands.borrow_mut().insert(name, function);
                    Ok(())
                })?,
        )?;

        let keymap = Rc::clone(&self.keymap);
        let map_commands = Rc::clone(&self.commands);
        ed.set(
            "map",
            self.lua
                .create_function(move |_, (mode, lhs, rhs): (String, String, Value)| {
                    let mode = parse_mode(&mode)?;
                    let commands = match rhs {
                        Value::String(command) => vec![command.to_string_lossy()],
                        Value::Function(function) => {
                            let name = format!("<map:{}:{}>", mode.name(), lhs);
                            map_commands.borrow_mut().insert(name.clone(), function);
                            vec![name]
                        }
                        Value::Nil => Vec::new(),
                        other => {
                            return Err(mlua::Error::RuntimeError(format!(
                                "ed.map: rhs must be a string or function, got {}",
                                other.type_name()
                            )));
                        }
                    };
                    keymap
                        .borrow_mut()
                        .bind(mode, &lhs, commands)
                        .map_err(|e| mlua::Error::RuntimeError(e.to_string()))?;
                    Ok(())
                })?,
        )?;

        let pending = Rc::clone(&self.pending);
        ed.set(
            "run",
            self.lua
                .create_function(move |_, (name, params): (String, Option<Table>)| {
                    let params = match params {
                        Some(table) => table_to_params(&table)?,
                        None => Vec::new(),
                    };
                    pending.borrow_mut().push_back((name, params));
                    Ok(())
                })?,
        )?;

        // The list of options lives in `Options`; this layer only converts
        // between Lua values and `Param`.
        let options = Rc::clone(&self.options);
        ed.set(
            "set",
            self.lua
                .create_function(move |_, (name, value): (String, Value)| {
                    let setting = Setting::from_name(&name).ok_or_else(|| {
                        mlua::Error::RuntimeError(format!("ed.set: unknown option {name}"))
                    })?;
                    let value = value_to_param(&value)?;
                    options
                        .borrow_mut()
                        .set(setting, value)
                        .map_err(|e| mlua::Error::RuntimeError(format!("ed.set: {e}")))
                })?,
        )?;

        let options = Rc::clone(&self.options);
        ed.set(
            "get",
            self.lua
                .create_function(move |lua, name: String| match Setting::from_name(&name) {
                    Some(setting) => param_to_value(lua, &options.borrow().get(setting)),
                    None => Err(mlua::Error::RuntimeError(format!(
                        "ed.get: unknown option {name}"
                    ))),
                })?,
        )?;

        self.lua.globals().set("ed", ed)?;
        Ok(())
    }
}

fn parse_mode(mode: &str) -> mlua::Result<Mode> {
    match mode {
        "n" | "normal" => Ok(Mode::Normal),
        "v" | "visual" => Ok(Mode::Visual),
        "i" | "insert" => Ok(Mode::Insert),
        other => Err(mlua::Error::RuntimeError(format!(
            "ed.map: unknown mode {other:?} (expected \"n\", \"v\" or \"i\")"
        ))),
    }
}

fn param_to_value(lua: &Lua, param: &Param) -> mlua::Result<Value> {
    Ok(match param {
        Param::Nil => Value::Nil,
        Param::Bool(b) => Value::Boolean(*b),
        Param::Int(n) => Value::Integer(*n),
        Param::Str(s) => Value::String(lua.create_string(s)?),
    })
}

fn value_to_param(value: &Value) -> mlua::Result<Param> {
    Ok(match value {
        Value::Nil => Param::Nil,
        Value::Boolean(b) => Param::Bool(*b),
        Value::Integer(n) => Param::Int(*n),
        Value::Number(n) => Param::Int(*n as i64),
        Value::String(s) => Param::Str(s.to_string_lossy()),
        other => {
            return Err(mlua::Error::RuntimeError(format!(
                "unsupported command parameter of type {}",
                other.type_name()
            )));
        }
    })
}

fn table_to_params(table: &Table) -> mlua::Result<Vec<Param>> {
    let mut params = Vec::new();
    for value in table.sequence_values::<Value>() {
        params.push(value_to_param(&value?)?);
    }
    Ok(params)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cursor::Cursor;
    use crate::editor::Editor;
    use crate::keymap::{parse_key, Lookup};

    fn feed(editor: &mut Editor, notation: &str) {
        for key in parse_key(notation).unwrap() {
            editor.handle_key(key);
        }
    }

    /// Write `source` to a unique temp `init.lua` and build an editor from it.
    fn editor_with_init(test_name: &str, source: &str) -> Editor {
        let dir = std::env::temp_dir()
            .join("inkstone-lua-tests")
            .join(test_name);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("init.lua");
        std::fs::write(&path, source).unwrap();
        Editor::new(Some(&path)).unwrap()
    }

    #[test]
    fn init_can_remap_a_visual_mode_key() {
        let mut ed = editor_with_init("remap-visual", r#"ed.map("v", "l", "yank_visual")"#);
        ed.buffer = crate::buffer::Buffer::from_text("abc\n");
        ed.cursor = Cursor::new(0, 0);
        feed(&mut ed, "vl");
        assert_eq!(ed.buffer.text(), "abc\n", "`l` yanks the selection now");
        assert_eq!(ed.register.text(), "a");
        assert_eq!(ed.mode, crate::mode::Mode::Normal);
    }

    #[test]
    fn init_can_remap_a_normal_mode_key() {
        let mut ed = editor_with_init("remap", r#"ed.map("n", "x", "move_right")"#);
        ed.buffer = crate::buffer::Buffer::from_text("abc\n");
        ed.cursor = Cursor::new(0, 0);
        feed(&mut ed, "x");
        assert_eq!(ed.cursor.col, 1, "x now moves right instead of deleting");
        assert_eq!(ed.buffer.text(), "abc\n");
    }

    #[test]
    fn init_can_bind_a_function_to_a_key() {
        let mut ed = editor_with_init(
            "bind_function",
            r#"
ed.map("n", "L", function()
  ed.run("move_right")
  ed.run("move_right")
end)
"#,
        );
        ed.buffer = crate::buffer::Buffer::from_text("abcdef\n");
        ed.cursor = Cursor::new(0, 0);
        feed(&mut ed, "L");
        assert_eq!(ed.cursor.col, 2, "both queued commands ran, in order");
    }

    #[test]
    fn lua_commands_can_be_registered_and_invoked() {
        let mut ed = editor_with_init(
            "command",
            r#"
ed.command("to_end_of_word", function()
  ed.run("move_word_forward")
end)
ed.map("n", "Q", "to_end_of_word")
"#,
        );
        assert_eq!(ed.lua.command_names(), vec!["to_end_of_word".to_string()]);
        ed.buffer = crate::buffer::Buffer::from_text("one two\n");
        ed.cursor = Cursor::new(0, 0);
        feed(&mut ed, "Q");
        assert_eq!(ed.cursor.col, 4);
    }

    #[test]
    fn lua_commands_receive_parameters() {
        let mut ed = editor_with_init(
            "params",
            r#"
ed.command("go", function(params)
  ed.run("move_down", { params[1] })
end)
ed.map("n", "Q", "go")
"#,
        );
        ed.buffer = crate::buffer::Buffer::from_text("a\nb\nc\nd\n");
        ed.cursor = Cursor::new(0, 0);
        ed.run_command("go", &[Param::Int(3)]);
        assert_eq!(ed.cursor.row, 3);
    }

    #[test]
    fn options_set_from_lua_affect_rendering() {
        let mut ed = editor_with_init(
            "options",
            r#"
ed.set("number", true)
ed.set("tabstop", 8)
"#,
        );
        ed.handle_resize(20, 4);
        ed.buffer = crate::buffer::Buffer::from_text("a\tb\n");
        assert_eq!(
            ed.options.borrow().tabstop,
            8,
            "the Lua setting reached the editor"
        );
        let out = ed.compose();
        // Gutter, then "a", then the tab filling columns 1..8, then "b".
        assert_eq!(out.grid.row_text(0).trim_end(), "1 a       b");
    }

    #[test]
    fn options_are_readable_and_validated() {
        let host_keymap = Rc::new(RefCell::new(Keymap::new()));
        let host_options = Rc::new(RefCell::new(Options::default()));
        let registry = CommandRegistry::with_builtins();
        let host =
            LuaHost::new(&registry, Rc::clone(&host_keymap), Rc::clone(&host_options)).unwrap();

        host.load_str(
            "test",
            r#"assert(ed.get("tabstop") == 4); ed.set("tabstop", 2)"#,
        )
        .unwrap();
        assert_eq!(host_options.borrow().tabstop, 2);

        let err = host.load_str("test", r#"ed.set("nope", 1)"#).unwrap_err();
        assert!(err.to_string().contains("unknown option nope"), "{err}");

        let err = host
            .load_str("test", r#"ed.set("tabstop", "four")"#)
            .unwrap_err();
        assert!(
            err.to_string()
                .contains("tabstop must be a positive integer"),
            "{err}"
        );

        let err = host
            .load_str("test", r#"ed.set("tabstop", 0)"#)
            .unwrap_err();
        assert!(err.to_string().contains("positive integer"), "{err}");

        let err = host
            .load_str("test", r#"ed.set("number", "yes")"#)
            .unwrap_err();
        assert!(
            err.to_string().contains("number must be true or false"),
            "{err}"
        );
    }

    #[test]
    fn insert_mappings_can_be_sequences() {
        let mut ed = editor_with_init("insert-sequence", r#"ed.map("i", "jk", "escape")"#);
        feed(&mut ed, "ijk");
        assert_eq!(ed.buffer.text(), "\n", "`jk` typed nothing");
        assert_eq!(ed.mode, crate::mode::Mode::Normal, "and left insert mode");
    }

    #[test]
    fn an_insert_sequence_that_is_not_a_mapping_is_typed_after_all() {
        let mut ed = editor_with_init("insert-flush", r#"ed.map("i", "jk", "escape")"#);
        feed(&mut ed, "ijx");
        assert_eq!(
            ed.buffer.text(),
            "jx\n",
            "the held-back `j` is typed when `x` proves the sequence is not a mapping"
        );
        assert_eq!(ed.mode, crate::mode::Mode::Insert);
    }

    #[test]
    fn bad_mappings_are_rejected() {
        let host_keymap = Rc::new(RefCell::new(Keymap::new()));
        let host_options = Rc::new(RefCell::new(Options::default()));
        let registry = CommandRegistry::with_builtins();
        let host =
            LuaHost::new(&registry, Rc::clone(&host_keymap), Rc::clone(&host_options)).unwrap();

        let err = host
            .load_str("test", r#"ed.map("n", "<Nope>", "move_left")"#)
            .unwrap_err();
        assert!(err.to_string().contains("bad key notation"), "{err}");

        let err = host
            .load_str("test", r#"ed.map("wibble", "x", "move_left")"#)
            .unwrap_err();
        assert!(err.to_string().contains("unknown mode"), "{err}");
    }

    #[test]
    fn mapping_to_a_function_records_a_command() {
        let host_keymap = Rc::new(RefCell::new(Keymap::new()));
        let host_options = Rc::new(RefCell::new(Options::default()));
        let registry = CommandRegistry::with_builtins();
        let host =
            LuaHost::new(&registry, Rc::clone(&host_keymap), Rc::clone(&host_options)).unwrap();
        host.load_str("test", r#"ed.map("n", "Q", function() end)"#)
            .unwrap();
        assert_eq!(host.command_names(), vec!["<map:NORMAL:Q>".to_string()]);
        assert!(matches!(
            host_keymap.borrow().lookup(Mode::Normal, "Q"),
            Lookup::Exact(_)
        ));
    }

    #[test]
    fn nil_rhs_unbinds() {
        let host_keymap = Rc::new(RefCell::new(Keymap::new()));
        let host_options = Rc::new(RefCell::new(Options::default()));
        let registry = CommandRegistry::with_builtins();
        let host =
            LuaHost::new(&registry, Rc::clone(&host_keymap), Rc::clone(&host_options)).unwrap();
        host.load_str("test", r#"ed.map("n", "Q", "noop"); ed.map("n", "Q", nil)"#)
            .unwrap();
        assert_eq!(host_keymap.borrow().lookup(Mode::Normal, "Q"), Lookup::None);
    }

    #[test]
    fn a_broken_init_reports_instead_of_killing_the_editor() {
        let ed = editor_with_init("broken", "this is not lua");
        assert!(ed.message.starts_with("init.lua:"), "{}", ed.message);
    }

    #[test]
    fn running_an_unknown_command_from_lua_reports_e492() {
        let mut ed = editor_with_init(
            "unknown",
            r#"ed.command("bad", function() ed.run("frobnicate") end)"#,
        );
        ed.run_command("bad", &[]);
        assert_eq!(ed.message, "E492: Not a command: frobnicate");
    }

    #[test]
    fn runaway_recursion_is_bounded() {
        let mut ed = editor_with_init(
            "recursion",
            r#"ed.command("loop", function() ed.run("loop") end)"#,
        );
        ed.run_command("loop", &[]);
        assert_eq!(ed.message, "E5108: command nesting too deep");
    }
}
