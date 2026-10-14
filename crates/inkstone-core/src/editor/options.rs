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
    pub(crate) fn set_option(&mut self, setting: Setting, value: &str) -> Result<(), String> {
        let param = match value {
            "true" => Param::Bool(true),
            "false" => Param::Bool(false),
            number => match number.parse::<i64>() {
                Ok(n) => Param::Int(n),
                Err(_) => Param::Str(value.to_string()),
            },
        };
        self.options.borrow_mut().set(setting, param)
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
