use std::fs;
use std::path::PathBuf;
use std::sync::OnceLock;

use directories::ProjectDirs;

// ── Data directory resolution (per-user OS config) ───────────────────────────
//
// All user data — sessions.json, credentials vault, known-hosts JSON — lives in
// ONE directory resolved here. Diagnostic logs use a `log/` subdir under it.

static DATA_DIR: OnceLock<PathBuf> = OnceLock::new();

/// The single directory holding all user data (sessions, credentials vault,
/// known-hosts). Resolved once and cached.
///
/// Always the per-user OS config dir (`%APPDATA%/zinterm`,
/// `~/.config/zinterm`, `~/Library/Application Support/zinterm`).
pub fn data_dir() -> PathBuf {
    DATA_DIR.get_or_init(resolve_data_dir).clone()
}

/// Directory for diagnostic logs (`error-YYYY-MM.log`): `<data_dir>/log`.
pub fn log_dir() -> PathBuf {
    let dir = data_dir().join("log");
    let _ = fs::create_dir_all(&dir);
    dir
}

pub(super) fn resolve_data_dir() -> PathBuf {
    // Use a plain `zinterm` fragment (not `dev.zinterm.zinterm`) so macOS lands in
    // `~/Library/Application Support/zinterm`, matching Linux `~/.config/zinterm`.
    let dir = ProjectDirs::from_path(PathBuf::from("zinterm"))
        .map(|d| d.config_dir().to_path_buf())
        .unwrap_or_else(|| std::env::temp_dir().join("zinterm"));
    let _ = fs::create_dir_all(&dir);
    dir
}
