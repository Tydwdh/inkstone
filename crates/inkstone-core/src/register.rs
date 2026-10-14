//! The register: what the last yank or delete put aside for `p` and `P`.
//!
//! # Two kinds, because pasting differs
//!
//! A register holds text and says whether it is whole lines. `yy` takes a line
//! *with its terminator*, and pasting that puts a line above or below the cursor's
//! line; `dw` takes characters with no terminator, and pasting that puts them
//! into the current line. Vim's two kinds, for vim's reason.
//!
//! # Line endings
//!
//! The text is stored exactly as the buffer held it, terminators included. A
//! register filled from a CRLF file therefore pastes CRLF, which is what the
//! line model already promises everywhere else: opening, editing and saving never
//! rewrite line endings behind the user's back.
//!
//! A linewise register always ends with a terminator, because the buffer's own
//! invariant guarantees every line it took has one.
//!
//! # Names
//!
//! One register is the *unnamed* one: what the last yank or delete put aside
//! when nobody said where to put it. The rest are named by what comes after a
//! `"`: `a` to `z` are the user's, `0` keeps the last yank, `1` to `9` keep the
//! last deletes newest-first, `_` is the black hole that keeps nothing, and `+`
//! and `*` are the system clipboard as far as the editor can know it.

use std::collections::HashMap;

/// Whether a key names a register the editor keeps.
pub(crate) fn is_register_name(c: char) -> bool {
    c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_' | '+' | '*')
}

/// Whether a register holds whole lines or characters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Characters, taken from within a line.
    Charwise,
    /// Whole lines, terminator and all.
    Linewise,
    /// A rectangle: the rows are in the text as lines, and putting it back
    /// means putting them in *columns* — one row of the register per row of the
    /// buffer, starting at the cursor's column.
    Blockwise,
}

/// What a yank or a delete put aside.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Register {
    text: String,
    kind: Kind,
}

impl Default for Register {
    fn default() -> Self {
        Self::empty()
    }
}

impl Register {
    /// An empty register: `p` has nothing to put, and does nothing.
    pub fn empty() -> Self {
        Self {
            text: String::new(),
            kind: Kind::Charwise,
        }
    }

    /// A register holding `text`, taken as whole lines or as characters.
    pub fn new(text: impl Into<String>, kind: Kind) -> Self {
        Self {
            text: text.into(),
            kind,
        }
    }

    /// The text, exactly as it was taken.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Whether this is whole lines.
    pub fn kind(&self) -> Kind {
        self.kind
    }

    /// Whether there is nothing to put.
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Whether this holds a rectangle.
    pub(crate) fn is_blockwise(&self) -> bool {
        self.kind == Kind::Blockwise
    }

    /// The rows of a blockwise register, as lines.
    pub fn rows(&self) -> Vec<&str> {
        self.text.split('\n').collect()
    }

    /// Whether this holds whole lines.
    pub(crate) fn is_linewise(&self) -> bool {
        self.kind == Kind::Linewise
    }
}

/// What a `"` was typed for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Prefix {
    /// No `"` is outstanding. The unnamed register is the one in play.
    None,
    /// A `"` was typed and the name has not come yet.
    Awaiting,
    /// The register the next operator, yank or put will use.
    Named(char),
}

/// Every register but the unnamed one.
///
/// The unnamed register lives on the editor, because it is the one `p` uses
/// without being asked and the one every edit fills; this is everything a `"`
/// can name.
#[derive(Debug, Clone)]
pub(crate) struct Registers {
    named: HashMap<char, Register>,
    last_yank: Register,
    clipboard: Register,
    black_hole: Register,
}

impl Default for Registers {
    fn default() -> Self {
        Self::new()
    }
}

impl Registers {
    /// No named registers, an empty clipboard and a black hole that is always
    /// empty.
    pub fn new() -> Self {
        Self {
            named: HashMap::new(),
            last_yank: Register::empty(),
            clipboard: Register::empty(),
            black_hole: Register::empty(),
        }
    }

    /// What a name holds. `None` for a name nothing has ever been put in.
    pub fn get(&self, name: char) -> Option<&Register> {
        match name {
            '0' => Some(&self.last_yank),
            '_' => Some(&self.black_hole),
            '+' | '*' => Some(&self.clipboard),
            _ => self.named.get(&name),
        }
    }

    /// Put `register` in the register `name` says.
    pub fn set(&mut self, name: char, register: Register) {
        match name {
            '0' => self.last_yank = register,
            '_' => {}
            '+' | '*' => self.clipboard = register,
            _ => {
                self.named.insert(name, register);
            }
        }
    }

    /// Record a yank: `"0` always holds the last one.
    pub(crate) fn note_yank(&mut self, register: &Register) {
        self.last_yank = register.clone();
    }

    /// Record a delete, shifting the numbered registers along: `"1` is the most
    /// recent, `"2` the one before it, and so on down to `"9`.
    pub(crate) fn note_delete(&mut self, register: &Register) {
        if register.is_empty() {
            return;
        }
        for number in (2..=9).rev() {
            if let Some(previous) = self.named.get(&((b'0' + number - 1) as char)).cloned() {
                self.named.insert((b'0' + number) as char, previous);
            }
        }
        self.named.insert('1', register.clone());
    }

    /// The clipboard, as far as the editor knows it. It is what a paste from the
    /// terminal put there, or what `"+y` last sent out.
    /// Only the tests ask for the system clipboard's register by name.
    #[cfg(test)]
    pub fn clipboard(&self) -> &Register {
        &self.clipboard
    }

    /// Take the clipboard as the frontend reported it: a bracketed paste *is*
    /// the system clipboard arriving, and `"+p` should be able to put it back.
    pub(crate) fn set_clipboard(&mut self, text: impl Into<String>) {
        self.clipboard = Register::new(text, Kind::Charwise);
    }

    /// Every name that holds something, for `:reg`.
    pub fn named(&self) -> Vec<(char, &Register)> {
        let mut out: Vec<(char, &Register)> = self
            .named
            .iter()
            .filter(|(_, register)| !register.is_empty())
            .map(|(name, register)| (*name, register))
            .collect();
        out.sort_by_key(|(name, _)| *name);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_register_has_nothing_to_put() {
        let register = Register::empty();
        assert!(register.is_empty());
        assert_eq!(register.text(), "");
        assert!(!register.is_linewise());
    }

    #[test]
    fn the_numbered_registers_keep_the_last_deletes() {
        let mut registers = Registers::new();
        registers.note_delete(&Register::new("one\n", Kind::Linewise));
        registers.note_delete(&Register::new("two\n", Kind::Linewise));
        registers.note_delete(&Register::new("three\n", Kind::Linewise));
        assert_eq!(registers.get('1').map(Register::text), Some("three\n"));
        assert_eq!(registers.get('2').map(Register::text), Some("two\n"));
        assert_eq!(registers.get('3').map(Register::text), Some("one\n"));
        assert_eq!(registers.get('4'), None, "and nothing past the shift");

        // A yank goes to `"0` and leaves the deletes alone.
        registers.note_yank(&Register::new("yanked", Kind::Charwise));
        assert_eq!(registers.get('0').map(Register::text), Some("yanked"));
        assert_eq!(registers.get('1').map(Register::text), Some("three\n"));
    }

    #[test]
    fn the_black_hole_keeps_nothing() {
        let mut registers = Registers::new();
        registers.set('_', Register::new("gone", Kind::Charwise));
        assert_eq!(registers.get('_').map(Register::text), Some(""));
    }

    #[test]
    fn a_register_keeps_its_text_exactly() {
        // Terminators are part of the text, and a linewise register is whole
        // lines — the two facts `p` needs.
        let register = Register::new("one\r\ntwo\r\n", Kind::Linewise);
        assert_eq!(register.text(), "one\r\ntwo\r\n");
        assert!(register.is_linewise());
        assert!(!register.is_empty());
    }
}
