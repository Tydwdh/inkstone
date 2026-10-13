//! Declarative macros that turn a declaration into the code around it.
//!
//! Both of these exist for the same reason: without them, adding a setting or a
//! command means editing several lists that can fall out of step with each
//! other. With them the list *is* the definition, and a mismatch is a build
//! error rather than a runtime surprise.

/// Declare the editor's settings.
///
/// Generates the [`Setting`](crate::Setting) enum (with `ALL`, `name` and
/// `from_name`), the [`Options`](crate::Options) struct, its `Default`, and the
/// `set`/`get` pair — all from one table. Adding a setting is one line here,
/// plus a [`SettingValue`](crate::SettingValue) impl if it uses a new type.
///
/// ```text
/// inkstone_core::settings! {
///     /// Columns a tab advances to.
///     Tabstop as tabstop: usize = 4,
/// }
/// ```
macro_rules! settings {
    (
        $(
            $(#[$meta:meta])*
            $variant:ident as $field:ident: $ty:ty = $default:expr,
        )*
    ) => {
        /// A user-settable option, named.
        ///
        /// The editor, the script layer and the renderer all go through this
        /// enum rather than through option name strings, so they cannot
        /// disagree about which options exist.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum Setting {
            $(
                $(#[$meta])*
                $variant,
            )*
        }

        impl Setting {
            /// Every setting, for name lookup and for tests.
            pub const ALL: &'static [Setting] = &[ $( Setting::$variant, )* ];

            /// The name a script uses.
            pub fn name(self) -> &'static str {
                match self {
                    $( Setting::$variant => stringify!($field), )*
                }
            }

            /// The setting a script's name refers to.
            pub fn from_name(name: &str) -> Option<Setting> {
                Setting::ALL
                    .iter()
                    .copied()
                    .find(|setting| setting.name() == name)
            }
        }

        /// User-settable editor options, also reachable from Lua via `ed.set`.
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub struct Options {
            $(
                $(#[$meta])*
                pub $field: $ty,
            )*
        }

        impl Default for Options {
            fn default() -> Self {
                Self { $( $field: $default, )* }
            }
        }

        impl Options {
            /// Set a setting, rejecting values of the wrong kind.
            pub fn set(&mut self, setting: Setting, value: $crate::Param) -> Result<(), String> {
                match setting {
                    $(
                        Setting::$variant => {
                            self.$field = <$ty as $crate::SettingValue>::from_param(value)
                                .map_err(|reason| format!("{} {reason}", setting.name()))?;
                        }
                    )*
                }
                Ok(())
            }

            /// Read a setting.
            pub fn get(&self, setting: Setting) -> $crate::Param {
                match setting {
                    $( Setting::$variant => self.$field.into_param(), )*
                }
            }
        }
    };
}

/// Declare the built-in commands.
///
/// Expands to `CommandRegistry::with_builtins`, so every name in the list must
/// have a function of the same name in scope: a command cannot be registered
/// under the wrong name, and forgetting to register one is a compile error.
///
/// ```text
/// inkstone_core::commands! {
///     noop,
///     save,
/// }
/// ```
macro_rules! commands {
    ( $( $(#[$meta:meta])* $name:ident ),* $(,)? ) => {
        /// The registry with every built-in command installed.
        ///
        /// Generated from the declaration, which is the only place the list of
        /// commands appears.
        pub fn with_builtins() -> Self {
            let mut registry = Self::new();
            $(
                $(#[$meta])*
                registry.register(stringify!($name), $name);
            )*
            registry
        }
    };
}
