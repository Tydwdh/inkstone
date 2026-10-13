//! `inkstone-core` — the editor kernel.
//!
//! Everything the editor *is* lives here: the document, the cursor, the modes,
//! the command registry, the keymap, the screen model and the Lua host. The
//! daemon (`inkstone-server`) drives this crate over RPC and the frontend (`inkstone`)
//! only draws what the daemon computes.

#![warn(missing_docs)]

#[macro_use]
mod macros;

pub mod buffer;
pub mod command;
pub mod cursor;
pub mod editor;
pub mod keymap;
pub mod lua;
pub mod mode;
pub mod render;
pub mod screen;
pub mod settings;

pub use buffer::{Buffer, BufferError};
pub use command::{CommandError, CommandRegistry};
pub use cursor::Cursor;
pub use editor::Editor;
pub use inkstone_rpc::Param;
pub use keymap::{encode_key, parse_key, Binding, KeyParseError, Keymap, Lookup};
pub use lua::LuaHost;
pub use mode::Mode;
pub use render::{compose, status_line, Composed, View};
pub use screen::{ensure_cursor_visible, scroll_to_cursor, Cell, Color, Grid, Viewport};
pub use settings::{Options, Setting, SettingValue};

/// Errors from setting up or driving the editor kernel.
#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    /// The Lua VM could not be created, or a chunk failed.
    #[error("lua error: {0}")]
    Lua(#[from] mlua::Error),
    /// Anything else, already formatted for the user.
    #[error("{0}")]
    Message(String),
}

impl From<std::io::Error> for CoreError {
    fn from(e: std::io::Error) -> Self {
        CoreError::Message(format!("i/o error: {e}"))
    }
}
