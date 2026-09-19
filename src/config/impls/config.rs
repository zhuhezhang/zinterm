//! Session / application configuration.
//!
//! Persists split JSON files in the app's **per-user OS config directory**.
//! See [`data_dir`].

#[path = "defaults_migration.rs"]
mod defaults_migration;
#[path = "export_import.rs"]
mod export_import;
#[path = "normalize.rs"]
mod normalize;
#[path = "paths.rs"]
mod paths;
#[path = "prefs_appearance.rs"]
mod prefs_appearance;
#[path = "prefs_layout.rs"]
mod prefs_layout;
#[path = "prefs_terminal.rs"]
mod prefs_terminal;
#[path = "quick_commands.rs"]
mod quick_commands;
#[path = "session_groups.rs"]
mod session_groups;
#[path = "sessions.rs"]
mod sessions;
#[path = "settings_admin.rs"]
mod settings_admin;
#[path = "store_load.rs"]
mod store_load;
#[path = "store_persist.rs"]
mod store_persist;

#[cfg(test)]
#[path = "test_support.rs"]
mod test_support;

pub(crate) use normalize::*;
pub use paths::{data_dir, log_dir};
