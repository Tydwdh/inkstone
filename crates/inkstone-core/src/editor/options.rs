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
