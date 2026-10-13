//! Editing modes: the heart of a modal editor.

/// The mode the editor is currently in.
///
/// Milestone 1 wires `Normal`, `Insert` and `Command`. `Insert` is driven by
/// dedicated logic (typing text is not a keymap entry); `Normal` and `Command`
/// dispatch through the keymap and the command registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Mode {
    /// Keys are commands.
    Normal,
    /// Keys insert text.
    Insert,
    /// Keys edit a `:` command line.
    Command,
}

impl Mode {
    /// The label shown on the status line.
    pub fn name(self) -> &'static str {
        match self {
            Mode::Normal => "NORMAL",
            Mode::Insert => "INSERT",
            Mode::Command => "COMMAND",
        }
    }
}
