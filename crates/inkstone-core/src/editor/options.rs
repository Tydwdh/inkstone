use std::path::PathBuf;

use inkstone_rpc::Param;

use crate::lua::LuaHost;
use crate::settings::Setting;

use super::*;

impl Editor {
    /// Set an option from what a person typed, which is what `:set` does.
    ///
    /// The value is read the way it reads in a script — `true`, `false`, a number
    /// — so the `:` line and `init.lua` cannot disagree about what a value means.
    pub(crate) fn set_option(
        &mut self,
        setting: Setting,
        value: &str,
        scope: Scope,
    ) -> Result<(), String> {
        let param = match value {
            "true" => Param::Bool(true),
            "false" => Param::Bool(false),
            number => match number.parse::<i64>() {
                Ok(n) => Param::Int(n),
                Err(_) => Param::Str(value.to_string()),
            },
        };
        // The manual's table: `:set option=value` writes both, `:setlocal` only
        // the file's own value, `:setglobal` only the one outside every file.
        if scope != Scope::Global {
            // The live options are what everything reads, and the list is what
            // travels with the file.
            self.options.borrow_mut().set(setting, param.clone())?;
            self.local_options.retain(|(name, _)| *name != setting);
            self.local_options.push((setting, param.clone()));
        }
        if scope != Scope::Local {
            self.global_options.set(setting, param.clone())?;
            // A file with a value of its own keeps it: `:setglobal` is about
            // every *other* file, which is the whole reason to have it.
            if !self.local_options.iter().any(|(name, _)| *name == setting) {
                self.options.borrow_mut().set(setting, param)?;
            }
        }
        Ok(())
    }

    /// Put the values of the file that is leaving back to the global ones.
    ///
    /// The list is not cleared here: the caller moves it into the document, which
    /// is where it has to be for the file to get its values back.
    pub(super) fn reset_local_options(&mut self) {
        let mut options = self.options.borrow_mut();
        for (setting, _) in &self.local_options {
            let value = self.global_options.get(*setting);
            let _ = options.set(*setting, value);
        }
    }

    /// Put the values of the file that has arrived in force.
    pub(super) fn apply_local_options(&mut self) {
        let mut options = self.options.borrow_mut();
        for (setting, value) in &self.local_options {
            let _ = options.set(*setting, value.clone());
        }
    }

    /// Whether the file that is open has a value of its own for a setting, which
    /// is the difference between `:setlocal x?` answering with an `=` and with a
    /// `--` in front of it.
    pub(super) fn has_local_option(&self, setting: Setting) -> bool {
        self.local_options.iter().any(|(name, _)| *name == setting)
    }

    /// What a scope differs from the defaults in, as one line: the answer to
    /// `:setlocal` or `:setglobal` with no argument, which in vim lists "all
    /// local options which are different from the default".
    ///
    /// One line rather than a list window: it is a sentence about a handful of
    /// settings, and the settings window is what `:set all` is for.
    pub(super) fn scoped_settings_by_name(&self, scope: Scope) -> String {
        let defaults = Options::default();
        let mut parts = Vec::new();
        for setting in Setting::ALL.iter().copied() {
            let (value, differs) = match scope {
                // A local value that is not the default, or one that is not the
                // global value either way round.
                Scope::Local => {
                    let local = self
                        .local_options
                        .iter()
                        .find(|(name, _)| *name == setting)
                        .map(|(_, value)| value.clone());
                    match local {
                        Some(value) => (value, true),
                        None => continue,
                    }
                }
                // The global value, when it is not the default.
                Scope::Global => {
                    let value = self.global_options.get(setting);
                    if value == defaults.get(setting) {
                        continue;
                    }
                    (value, true)
                }
                Scope::Both => continue,
            };
            let _ = differs;
            let text = match value {
                Param::Bool(value) => value.to_string(),
                Param::Int(value) => value.to_string(),
                Param::Str(value) => value,
                Param::Nil => String::new(),
            };
            parts.push(format!("{}={text}", setting.name()));
        }
        if parts.is_empty() {
            return match scope {
                Scope::Local => "no local values".to_string(),
                _ => "no global values differ from the defaults".to_string(),
            };
        }
        format!(
            "{} value{}: {}",
            parts.len(),
            if parts.len() == 1 { "" } else { "s" },
            parts.join(" ")
        )
    }

    /// The value outside any file: what `:setglobal {name}?` shows.
    pub(crate) fn global_option_text(&self, setting: Setting) -> String {
        match self.global_options.get(setting) {
            Param::Bool(value) => value.to_string(),
            Param::Int(value) => value.to_string(),
            Param::Str(value) => value,
            Param::Nil => String::new(),
        }
    }

    /// What a setting is in force as: the file's own value if it has one, the
    /// global value otherwise. This is what `:setlocal {name}?` shows.
    pub(crate) fn local_option_text(&self, setting: Setting) -> String {
        self.local_options
            .iter()
            .find(|(name, _)| *name == setting)
            .map(|(_, value)| match value {
                Param::Bool(value) => value.to_string(),
                Param::Int(value) => value.to_string(),
                Param::Str(value) => value.clone(),
                Param::Nil => String::new(),
            })
            .unwrap_or_else(|| self.option_text(setting))
    }

    /// What an option is set to, for `:set {name}?` and for the message that
    /// follows a set.
    pub(crate) fn option_text(&self, setting: Setting) -> String {
        match self.options.borrow().get(setting) {
            Param::Bool(value) => value.to_string(),
            Param::Int(value) => value.to_string(),
            other => format!("{other:?}"),
        }
    }

    /// The default `init.lua` location, if the platform has one.
    pub fn default_init_path() -> Option<PathBuf> {
        LuaHost::default_init_path()
    }
}
