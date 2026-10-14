//! The editor's settings: which ones exist, and how a script reads and writes
//! them.
//!
//! The whole thing is generated from one table by [`settings!`](crate::settings),
//! so the enum, the struct, the defaults and the `set`/`get` pair cannot drift
//! apart.

use inkstone_rpc::Param;

/// A type that can be used for a setting.
///
/// Implementing this is what it takes to introduce a *new kind* of setting; a
/// setting that reuses an existing type is a single line in the table.
pub trait SettingValue: Clone {
    /// The value as a script sees it.
    fn into_param(self) -> Param;

    /// Parse a script's value, explaining what was wrong when it does not fit.
    fn from_param(value: Param) -> Result<Self, String>
    where
        Self: Sized;
}

impl SettingValue for bool {
    fn into_param(self) -> Param {
        Param::Bool(self)
    }

    fn from_param(value: Param) -> Result<Self, String> {
        match value {
            Param::Bool(value) => Ok(value),
            _ => Err("must be true or false".to_string()),
        }
    }
}

/// A count where zero means something rather than nothing — columns for
/// `textwidth`, lines for `scrolloff`.
///
/// `textwidth = 0` is vim's way of saying "do not wrap", so the type that holds
/// it has to allow zero — which is why it is not the same type as the counts
/// behind `tabstop`, whose values are positive by convention. The unit is in the
/// setting's name, not in this type: what it knows is that zero is a value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Count(pub usize);

impl SettingValue for Count {
    fn into_param(self) -> Param {
        Param::Int(self.0 as i64)
    }

    fn from_param(value: Param) -> Result<Self, String> {
        match value {
            Param::Int(n) if n >= 0 => Ok(Count(n as usize)),
            _ => Err("must be zero or more".to_string()),
        }
    }
}

impl SettingValue for usize {
    fn into_param(self) -> Param {
        Param::Int(self as i64)
    }

    fn from_param(value: Param) -> Result<Self, String> {
        // Counts are positive by convention. A setting that legitimately wants
        // zero would need its own type rather than a sentinel meaning "off".
        match value {
            Param::Int(value) if value >= 1 => Ok(value as usize),
            _ => Err("must be a positive integer".to_string()),
        }
    }
}

/// A list of directories, as `'path'` is: what `gf` searches when the name it is
/// given is not beside the file that is open.
///
/// It goes to and from a script as one comma-separated string, which is how vim's
/// `:set path=a,b,c` reads — the parsing happens here so that a name with a comma in
/// it cannot quietly become two directories.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PathList(pub Vec<std::path::PathBuf>);

impl SettingValue for PathList {
    fn into_param(self) -> Param {
        Param::Str(
            self.0
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<String>>()
                .join(","),
        )
    }

    fn from_param(value: Param) -> Result<Self, String> {
        match value {
            Param::Str(text) => Ok(PathList(
                text.split(',')
                    .map(str::trim)
                    .filter(|part| !part.is_empty())
                    .map(std::path::PathBuf::from)
                    .collect(),
            )),
            _ => Err("must be a comma-separated list of directories".to_string()),
        }
    }
}

impl SettingValue for String {
    fn into_param(self) -> Param {
        Param::Str(self)
    }

    fn from_param(value: Param) -> Result<Self, String> {
        match value {
            // A script may hand over a number or a flag where a string is wanted:
            // `:set foldignore=` is an empty string, and that is what comes here.
            Param::Str(text) => Ok(text),
            Param::Int(n) => Ok(n.to_string()),
            Param::Bool(b) => Ok(b.to_string()),
            other => Err(format!("must be text, not {other:?}")),
        }
    }
}

/// How folds are defined: by hand, or by the indent of the lines.
///
/// Vim has six methods (`manual`, `indent`, `expr`, `syntax`, `diff`, `marker`).
/// This has two, and the others are on the roadmap: `expr` and `syntax` need the
/// machinery they are named after, and `diff` and `marker` are methods of their own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FoldMethod {
    /// Folds are what a person made with `zf`.
    #[default]
    Manual,
    /// Folds are worked out from the indent of the lines: a line that is indented
    /// further than the one before it starts a fold, which ends at the last line
    /// that is at least as far in.
    Indent,
}

impl SettingValue for FoldMethod {
    fn into_param(self) -> Param {
        Param::Str(
            match self {
                FoldMethod::Manual => "manual",
                FoldMethod::Indent => "indent",
            }
            .to_string(),
        )
    }

    fn from_param(value: Param) -> Result<Self, String> {
        match value {
            Param::Str(text) => match text.trim() {
                "manual" => Ok(FoldMethod::Manual),
                "indent" => Ok(FoldMethod::Indent),
                other => Err(format!(
                    "must be manual or indent (this editor has no {other})"
                )),
            },
            _ => Err("must be manual or indent".to_string()),
        }
    }
}

settings! {
    /// Columns a tab advances to.
    Tabstop as tabstop: usize = 4,
    /// Whether to draw a line-number gutter.
    Number as number: bool = false,
    /// Whether a search ignores case.
    IgnoreCase as ignorecase: bool = false,
    /// Whether to colour code by what it is.
    Highlight as highlight: bool = true,
    /// Whether to speak to a language server for the file that is open. Off by
    /// default: starting a process is a bigger thing than a colour, and it is
    /// the daemon that does it.
    Lsp as lsp: bool = false,
    /// How wide `gq` makes a line, and zero to leave lines alone.
    TextWidth as textwidth: Count = Count(79),
    /// How many lines of context to keep above and below the cursor: the
    /// viewport scrolls early rather than letting the cursor reach the edge.
    /// Zero, as in vim, means the cursor may sit on the first or last row.
    ScrollOff as scrolloff: Count = Count(0),
    /// Whether a new line starts where the last one did. Off by default, as in
    /// vim: an editor that indents for you is a taste, not a default.
    AutoIndent as autoindent: bool = false,
    /// Whether a line that follows an opener — `{`, `(`, `[` — gets one more
    /// level, and a `}` typed alone gives one back. It carries `autoindent` with
    /// it, the way vim's does.
    SmartIndent as smartindent: bool = false,
    /// How folds are defined: `manual` (the default, as in vim) or `indent`.
    FoldMethod as foldmethod: FoldMethod = FoldMethod::Manual,
    /// How deep the folds are *shown* closed: a fold nested deeper than this is
    /// drawn as one line even when it is not closed itself. `zm` lowers it and `zr`
    /// raises it, and zero — vim's default — means the folds are shown as they are.
    FoldLevel as foldlevel: Count = Count(0),
    /// How deep folds may nest with `'foldmethod'` set to `indent`: a fold deeper
    /// than this is not made. Vim's default is twenty.
    FoldNestMax as foldnestmax: usize = 20,
    /// Characters that make a line *ignored* by `indent` folding: such a line gets
    /// the lower of the levels around it rather than a level of its own, which is
    /// what `#` is for in C — a preprocessor line is not indented like the code
    /// inside it. Empty by default, as in vim.
    FoldIgnore as foldignore: String = String::new(),
    /// Where `gf` looks when the file it is given is not beside the one that is
    /// open: a comma-separated list of directories, searched in order. Empty by
    /// default, as in vim.
    Path as path: PathList = PathList(Vec::new()),
    /// Whether joining lines puts two spaces after a full stop, a question mark or
    /// an exclamation mark — vim's `'joinspaces'`, on by default there and here.
    JoinSpaces as joinspaces: bool = true,
    /// Whether saving asks a language server to lay the file out first, and
    /// writes what it answered. Off by default, and doing nothing at all unless
    /// a server is attached.
    FormatOnSave as formatonsave: bool = false,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_setting_round_trips_through_its_name() {
        for setting in Setting::ALL.iter().copied() {
            let mut options = Options::default();
            let changed = match options.get(setting) {
                Param::Bool(b) => Param::Bool(!b),
                Param::Int(n) => Param::Int(n + 1),
                // A list is one string to a script, so a different string is a
                // different value of it. A setting whose values are *named* — the
                // fold method — cannot be probed this way: it is asked below.
                Param::Str(text) => Param::Str(format!("{text}probe")),
                ref other => panic!("{setting:?} has an unprobeable type: {other:?}"),
            };
            if options.set(setting, changed.clone()).is_err() {
                // The probe was refused, so this setting's values are a fixed set:
                // it is set to each of its own names and read back instead.
                continue;
            }
            assert_eq!(
                options.get(setting),
                changed,
                "{setting:?} did not survive a set/get round trip"
            );
            assert_eq!(
                Setting::from_name(setting.name()),
                Some(setting),
                "{setting:?} cannot be looked up by name"
            );
        }

        let mut names: Vec<&str> = Setting::ALL.iter().map(|s| s.name()).collect();
        let count = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), count, "two settings share a name");
    }

    #[test]
    fn a_named_setting_round_trips_through_its_names() {
        // `'foldmethod'` takes names rather than a number or a flag, so it is probed
        // by its own names: each one is set, read back, and looked up by name.
        let mut options = Options::default();
        assert_eq!(
            options.get(Setting::FoldMethod),
            Param::Str("manual".into())
        );
        for name in ["manual", "indent"] {
            options
                .set(Setting::FoldMethod, Param::Str(name.into()))
                .unwrap_or_else(|e| panic!("{name} cannot be set: {e}"));
            assert_eq!(options.get(Setting::FoldMethod), Param::Str(name.into()));
        }
        assert!(
            options
                .set(Setting::FoldMethod, Param::Str("syntax".into()))
                .is_err(),
            "a method this editor does not have is refused, and says which"
        );
        assert_eq!(
            options.get(Setting::FoldMethod),
            Param::Str("indent".into()),
            "and the value it had is left alone"
        );
    }

    #[test]
    fn a_setting_rejects_values_of_the_wrong_kind() {
        let mut options = Options::default();
        assert_eq!(
            options
                .set(Setting::Tabstop, Param::Bool(true))
                .unwrap_err(),
            "tabstop must be a positive integer"
        );
        assert_eq!(
            options.set(Setting::Tabstop, Param::Int(0)).unwrap_err(),
            "tabstop must be a positive integer"
        );
        assert_eq!(
            options
                .set(Setting::Number, Param::Str("yes".into()))
                .unwrap_err(),
            "number must be true or false"
        );
        assert_eq!(options.tabstop, 4, "a rejected set leaves the option alone");
        assert_eq!(Setting::from_name("nope"), None);
    }

    #[test]
    fn defaults_are_what_the_editor_starts_with() {
        let options = Options::default();
        assert_eq!(options.tabstop, 4);
        assert!(!options.number);
        assert!(!options.ignorecase, "a search is case-sensitive by default");
        assert!(
            options.highlight,
            "code is coloured unless it is turned off"
        );
    }
}
