//! What a language server has to say about a file.
//!
//! A diagnostic is a range of the text, how bad it is, and what is wrong with
//! it. The range is in *characters* — row and column, the way every other
//! position in this editor is — because the conversion from the protocol's
//! UTF-16 offsets belongs to whoever talks the protocol, in one place, with its
//! own tests.
//!
//! The kernel only draws them: an underline on the range, and a colour on the
//! line number in the gutter, which is the one part of a line that is the
//! editor's rather than the file's.

use inkstone_rpc::{Color, ATTR_BOLD, ATTR_UNDERLINE};

/// How bad a diagnostic is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    /// The code does not work.
    Error,
    /// It works, and probably should not have been written that way.
    Warning,
    /// Something to know.
    Information,
    /// Something that could be nicer.
    Hint,
}

impl Severity {
    /// The name used in `:diagnostics` and in the message row.
    pub fn name(self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Information => "info",
            Severity::Hint => "hint",
        }
    }

    /// The colour the gutter draws this severity in.
    pub fn color(self) -> Color {
        match self {
            Severity::Error => Color::Indexed(167),
            Severity::Warning => Color::Indexed(180),
            Severity::Information => Color::Indexed(75),
            Severity::Hint => Color::Indexed(244),
        }
    }

    /// The attribute bits the range is underlined with. An error is underlined
    /// and bold, so it stands out from a warning without either of them taking
    /// a colour away from the syntax.
    pub fn attrs(self) -> u16 {
        match self {
            Severity::Error => ATTR_UNDERLINE | ATTR_BOLD,
            _ => ATTR_UNDERLINE,
        }
    }
}

/// One thing a server said about a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    /// Where it starts: row, and column in characters.
    pub start: (usize, usize),
    /// One past where it ends. An empty range is a point, which is what a
    /// diagnostic with nothing to underline is.
    pub end: (usize, usize),
    /// How bad it is.
    pub severity: Severity,
    /// What is wrong, in the server's words.
    pub message: String,
}

impl Diagnostic {
    /// A diagnostic covering `start` to `end`.
    pub fn new(
        start: (usize, usize),
        end: (usize, usize),
        severity: Severity,
        message: impl Into<String>,
    ) -> Self {
        Self {
            start,
            end,
            severity,
            message: message.into(),
        }
    }

    /// Whether this diagnostic is on `row`, and where it covers on it: the
    /// characters it underlines on that row, as a range.
    ///
    /// A diagnostic that runs over several lines underlines the part of each.
    /// An empty one underlines the single character after its start, so that
    /// "this line is wrong" has something to show.
    pub fn on_row(&self, row: usize) -> Option<(usize, usize)> {
        if row < self.start.0 || row > self.end.0 {
            return None;
        }
        let from = if row == self.start.0 { self.start.1 } else { 0 };
        let to = if row == self.end.0 {
            self.end.1
        } else {
            usize::MAX
        };
        if to <= from {
            return Some((from, from + 1));
        }
        Some((from, to))
    }

    /// The text a person reads: `error: expected a comma`.
    pub fn line(&self) -> String {
        format!("{}: {}", self.severity.name(), self.message)
    }
}

/// Where a diagnostic sits, for sorting and for finding the one a cursor is in.
pub fn order(diagnostic: &Diagnostic) -> (usize, usize) {
    diagnostic.start
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_diagnostic_covers_its_range_on_every_row_it_touches() {
        let one = Diagnostic::new((2, 4), (2, 9), Severity::Error, "boom");
        assert_eq!(one.on_row(1), None);
        assert_eq!(one.on_row(2), Some((4, 9)));
        assert_eq!(one.on_row(3), None);
        assert_eq!(one.line(), "error: boom");

        // One that runs over lines has something to underline on each.
        let many = Diagnostic::new((1, 3), (3, 2), Severity::Warning, "long");
        assert_eq!(many.on_row(1), Some((3, usize::MAX)));
        assert_eq!(many.on_row(2), Some((0, usize::MAX)));
        assert_eq!(many.on_row(3), Some((0, 2)));

        // An empty range is a point, and still shows.
        let point = Diagnostic::new((0, 0), (0, 0), Severity::Hint, "here");
        assert_eq!(point.on_row(0), Some((0, 1)));
    }

    #[test]
    fn an_error_is_underlined_and_bold() {
        assert_ne!(Severity::Error.attrs() & ATTR_UNDERLINE, 0);
        assert_ne!(Severity::Error.attrs() & ATTR_BOLD, 0);
        assert_ne!(Severity::Warning.attrs() & ATTR_UNDERLINE, 0);
        assert_eq!(Severity::Warning.attrs() & ATTR_BOLD, 0);
        assert_ne!(Severity::Error.color(), Severity::Warning.color());
    }
}
