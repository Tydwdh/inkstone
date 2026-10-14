//! Editing modes: the heart of a modal editor.

/// The mode the editor is currently in.
///
/// `Insert` is driven by dedicated logic (typing text is not a keymap entry);
/// `Normal`, `Visual` and `Command` dispatch through the keymap and the command
/// registry, and `Visual` shares `Normal`'s motions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Mode {
    /// Keys are commands.
    Normal,
    /// Keys extend a selection; operators act on it.
    Visual,
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
            Mode::Visual => "VISUAL",
            Mode::Insert => "INSERT",
            Mode::Command => "COMMAND",
        }
    }
}

/// What a visual selection covers.
///
/// `v` selects characters, `V` selects whole lines, and the difference is not
/// cosmetic: an operator on a linewise selection takes the terminators with it,
/// exactly as `dd` does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VisualKind {
    /// From one character to another, inclusive.
    Charwise,
    /// Whole lines, terminators and all.
    Linewise,
    /// A rectangle: the rows between the two ends, and the columns between
    /// them — which is what `<C-v>` selects, and the reason a short line
    /// contributes only what it has.
    Blockwise,
}

impl VisualKind {
    /// The label shown on the status line, after `VISUAL`.
    pub fn name(self) -> &'static str {
        match self {
            VisualKind::Charwise => "",
            VisualKind::Linewise => " LINE",
            VisualKind::Blockwise => " BLOCK",
        }
    }

    /// The other kind: `v` and `V` swap between them while a selection is up.
    ///
    /// A block is neither of the two, and `v` on one is vim's way back to
    /// characters — which is why the answer for it is not a swap.
    /// Only the tests ask this; `v` and `<C-v>` do their own switching.
    #[cfg(test)]
    pub(crate) fn toggled(self) -> Self {
        match self {
            VisualKind::Charwise => VisualKind::Linewise,
            VisualKind::Linewise => VisualKind::Charwise,
            VisualKind::Blockwise => VisualKind::Charwise,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_mode_has_a_label() {
        for mode in [Mode::Normal, Mode::Visual, Mode::Insert, Mode::Command] {
            assert!(!mode.name().is_empty(), "{mode:?} has no label");
        }
        assert_eq!(Mode::Visual.name(), "VISUAL");
    }

    #[test]
    fn the_selection_kinds_are_labelled_and_toggle() {
        assert_eq!(VisualKind::Charwise.name(), "");
        assert_eq!(VisualKind::Linewise.name(), " LINE");
        assert_eq!(VisualKind::Blockwise.name(), " BLOCK");
        assert_eq!(VisualKind::Charwise.toggled(), VisualKind::Linewise);
        assert_eq!(VisualKind::Linewise.toggled(), VisualKind::Charwise);
    }
}
