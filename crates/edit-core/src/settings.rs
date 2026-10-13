//! The editor's settings: which ones exist, and how a script reads and writes
//! them.
//!
//! The whole thing is generated from one table by [`settings!`](crate::settings),
//! so the enum, the struct, the defaults and the `set`/`get` pair cannot drift
//! apart.

use edit_rpc::Param;

/// A type that can be used for a setting.
///
/// Implementing this is what it takes to introduce a *new kind* of setting; a
/// setting that reuses an existing type is a single line in the table.
pub trait SettingValue: Copy {
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

settings! {
    /// Columns a tab advances to.
    Tabstop as tabstop: usize = 4,
    /// Whether to draw a line-number gutter.
    Number as number: bool = false,
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
                ref other => panic!("{setting:?} has an unprobeable type: {other:?}"),
            };
            options
                .set(setting, changed.clone())
                .unwrap_or_else(|e| panic!("{setting:?} cannot be set: {e}"));
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
    }
}
