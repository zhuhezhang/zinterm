//! Session / application configuration.
//!
//! Persists split JSON files in the app's **per-user OS config directory**
//! (e.g. `%APPDATA%/zinterm`, `~/.config/zinterm`,
//! `~/Library/Application Support/zinterm`). See [`data_dir`].
//!
//! | File | Contents |
//! |------|----------|
//! | `sessions.json` | sessions, groups, quick commands, command history |
//! | `settings.json` | Settings-panel preferences |
//! | `ui-state.json` | layout chrome + Quick Connect fold state |
//!
//! ## Password / key storage
//!
//! Secrets (`password`, `key_passphrase`, `private_key`) are **never** written
//! to `sessions.json`. When Settings › Data › save passwords is on, they go
//! into `zinterm-credentials-vault.json` as ChaCha20-Poly1305 ciphertext, with
//! the master key held by the OS keyring (`keyring` crate). See [`crate::config::vault`].
//!
//! UI code should prefer [`ConfigStore::save_later`] so disk I/O runs on the
//! background persist thread. [`ConfigStore::save`] / [`ConfigStore::save_parts`]
//! write synchronously (tests, migration, shutdown).

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{Context, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chacha20poly1305::{
    aead::{Aead, KeyInit},
    ChaCha20Poly1305,
};
use directories::{ProjectDirs, UserDirs};

use super::structs::*;

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

fn resolve_data_dir() -> PathBuf {
    // Use a plain `zinterm` fragment (not `dev.zinterm.zinterm`) so macOS lands in
    // `~/Library/Application Support/zinterm`, matching Linux `~/.config/zinterm`.
    let dir = ProjectDirs::from_path(PathBuf::from("zinterm"))
        .map(|d| d.config_dir().to_path_buf())
        .unwrap_or_else(|| std::env::temp_dir().join("zinterm"));
    let _ = fs::create_dir_all(&dir);
    dir
}

pub(crate) fn normalize_hex_color(value: &str) -> Option<String> {
    let digits = value.trim().strip_prefix('#').unwrap_or(value.trim());
    if digits.len() != 6 || !digits.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    Some(format!("#{}", digits.to_ascii_uppercase()))
}

/// Built-in starter custom highlight rules (aligned with zauterm defaults):
/// error / success / warning keywords and IPv4 addresses.
fn default_output_highlight_rules() -> Vec<OutputHighlightRule> {
    vec![
        OutputHighlightRule {
            name: "error".into(),
            pattern: r"(\berror\b)|(\bfailed\b)|(\bdenied\b)|(\bunauthorized\b)|(\bdown\b)"
                .into(),
            regex: true,
            case_sensitive: false,
            whole_line: false,
            color: "#F1250E".into(),
            enabled: true,
        },
        OutputHighlightRule {
            name: "success".into(),
            pattern: r"(\bsuccess\b)|(\bconnected\b)|(\bready\b)|(\bok\b)|(\bup\b)".into(),
            regex: true,
            case_sensitive: false,
            whole_line: false,
            color: "#4ADE80".into(),
            enabled: true,
        },
        OutputHighlightRule {
            name: "warning".into(),
            pattern: r"(\bwarning\b)|(\bnotice\b)|(\binfo\b)|(\bdebug\b)|(\bdisabled\b)"
                .into(),
            regex: true,
            case_sensitive: false,
            whole_line: false,
            color: "#F1C40F".into(),
            enabled: true,
        },
        OutputHighlightRule {
            name: "IP".into(),
            pattern:
                r"\b(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\b"
                    .into(),
            regex: true,
            case_sensitive: false,
            whole_line: false,
            color: "#C717D3".into(),
            enabled: true,
        },
    ]
}

/// A brand-new config (no file yet, or the old one was corrupt). Seeds the
/// new-user default layout (#new-user-defaults): no wallpaper, welcome page as
/// a left sidebar, 15% wallpaper transparency, bar cursor, collapsed SFTP,
/// quick-command sidebar enabled, single-click connect, update check off,
/// four starter custom highlight rules — and marks the migration done so it
/// isn't re-applied.
fn fresh_config() -> ConfigFile {
    ConfigFile {
        wallpaper: String::new(),
        welcome_as_sidebar: true,
        wallpaper_overlay: DEFAULT_WALLPAPER_OVERLAY,
        terminal_cursor_style: "bar".to_string(),
        collapse_sftp_default: true,
        quick_commands_as_sidebar: true,
        welcome_single_click_connect: true,
        update_check_disabled: true,
        output_highlight_rules: default_output_highlight_rules(),
        defaults_rev: DEFAULTS_REV,
        ..ConfigFile::default()
    }
}

/// One-time push of the new default layout to *existing* users — but only for
/// each item they're still leaving at the old default, so deliberate choices are
/// never clobbered. Runs once (gated by `defaults_rev`); returns whether anything
/// changed so the caller can persist it. (#new-user-defaults)
fn migrate_defaults(cfg: &mut ConfigFile) -> bool {
    if cfg.defaults_rev >= DEFAULTS_REV {
        return false;
    }
    // rev 1: miku / welcome-as-sidebar / wallpaper overlay.
    if cfg.defaults_rev < 1 {
        // Old default wallpaper → miku. A custom path, "none" (""), or any other
        // built-in means the user chose it, so leave it.
        if cfg.wallpaper == "builtin:tech" {
            cfg.wallpaper = "builtin:miku".to_string();
        }
        // Overlay still unset -> current default.
        if cfg.wallpaper_overlay <= 0.0 {
            cfg.wallpaper_overlay = DEFAULT_WALLPAPER_OVERLAY;
        }
        // Never enabled the welcome sidebar → enable it.
        if !cfg.welcome_as_sidebar {
            cfg.welcome_as_sidebar = true;
        }
    }
    // rev 2: settings show wallpaper transparency, while rev 1 accidentally
    // stored the default as panel alpha 0.38, so it displayed as ~62%.
    if cfg.defaults_rev < 2
        && (cfg.wallpaper_overlay - PREVIOUS_DEFAULT_WALLPAPER_TRANSPARENCY).abs() < 0.005
    {
        cfg.wallpaper_overlay = DEFAULT_WALLPAPER_OVERLAY;
    }
    // rev 3: reduce the default transparency from 38% to 15%. Only advance
    // users still on the previous default; preserve every custom slider value.
    if cfg.defaults_rev < 3
        && (cfg.wallpaper_overlay - PREVIOUS_DEFAULT_WALLPAPER_OVERLAY).abs() < 0.005
    {
        cfg.wallpaper_overlay = DEFAULT_WALLPAPER_OVERLAY;
    }
    // rev 4: none wallpaper, bar cursor, collapse SFTP, quick-command sidebar,
    // single-click connect, update check off — only for still-at-old-default.
    if cfg.defaults_rev < 4 {
        if cfg.wallpaper == "builtin:ms" {
            cfg.wallpaper = String::new();
        }
        if cfg.terminal_cursor_style.is_empty() {
            cfg.terminal_cursor_style = "bar".to_string();
        }
        if !cfg.collapse_sftp_default {
            cfg.collapse_sftp_default = true;
        }
        if !cfg.quick_commands_as_sidebar {
            cfg.quick_commands_as_sidebar = true;
        }
        if !cfg.welcome_single_click_connect {
            cfg.welcome_single_click_connect = true;
        }
        if !cfg.update_check_disabled {
            cfg.update_check_disabled = true;
        }
    }
    // rev 5: seed the four starter custom highlight rules when the list is
    // still empty (old default). Users who already added/removed rules keep
    // their list as-is.
    if cfg.defaults_rev < 5 && cfg.output_highlight_rules.is_empty() {
        cfg.output_highlight_rules = default_output_highlight_rules();
    }
    cfg.defaults_rev = DEFAULTS_REV;
    true
}

/// Normalize a highlight colour to `#RRGGBB`. Accepts hex or legacy palette ids
/// (red/yellow/green/cyan/magenta/gray) used before free-form colours.
pub(crate) fn normalize_highlight_color(color: &str) -> String {
    if let Some(hex) = normalize_hex_color(color) {
        return hex;
    }
    match color.trim().to_ascii_lowercase().as_str() {
        "yellow" => "#F5F543".to_string(),
        "green" => "#23D18B".to_string(),
        "cyan" => "#29B8DB".to_string(),
        "magenta" => "#D670D6".to_string(),
        "gray" | "grey" => "#666666".to_string(),
        // "red" and anything unrecognised → bright red (former ANSI idx 9).
        _ => "#F14C4C".to_string(),
    }
}

fn migrate_output_highlight_rules(cfg: &mut ConfigFile) -> bool {
    let mut changed = false;
    for rule in &mut cfg.output_highlight_rules {
        let color = normalize_highlight_color(&rule.color);
        if rule.color != color {
            rule.color = color;
            changed = true;
        }
        let name = rule.name.trim().to_string();
        if rule.name != name {
            rule.name = name;
            changed = true;
        }
    }
    changed
}

/// Remove duplicate entries in place, keeping the *last* (most recent)
/// occurrence of each and preserving relative order (#113). The list is capped
/// at 200, so the quadratic scan is trivial.
fn dedup_keep_last(items: &mut Vec<String>) {
    let mut i = 0;
    while i < items.len() {
        if items[i + 1..].contains(&items[i]) {
            items.remove(i);
        } else {
            i += 1;
        }
    }
}

/// Expand zsh `fc -ln` newline escapes stored as the two-character sequence
/// `\n` (common before the shell hook ran `printf %b`). Leave real newlines
/// and intentional lone `echo \n` alone.
fn repair_history_newlines(cmd: String) -> String {
    if cmd.contains('\n') || cmd.contains('\r') || !cmd.contains('\\') {
        return cmd;
    }
    let needs = cmd.contains("<<") || {
        let b = cmd.as_bytes();
        let mut i = 0;
        let mut found = false;
        while i + 1 < b.len() {
            if b[i] == b'\\' && (b[i + 1] == b'n' || b[i + 1] == b'r') {
                let doubled = i > 0 && b[i - 1] == b'\\';
                if !doubled {
                    let left = cmd[..i].chars().any(|c| !c.is_whitespace());
                    let right = cmd[i + 2..].chars().any(|c| !c.is_whitespace());
                    if left && right {
                        found = true;
                        break;
                    }
                }
            }
            i += 1;
        }
        found
    };
    if !needs {
        return cmd;
    }
    let mut out = String::with_capacity(cmd.len());
    let mut chars = cmd.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some('\\') => out.push('\\'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Legacy display-only group names that must never be persisted as user folders.
/// Older builds used `default` for ungrouped sessions and `system` for built-in
/// local shells; both now map to an empty (root) group.
pub(crate) fn is_reserved_session_group(name: &str) -> bool {
    name.eq_ignore_ascii_case("default") || name.eq_ignore_ascii_case("system")
}

/// Empty / reserved display names all mean the Quick Connect root.
pub(crate) fn normalize_session_group(group: &str) -> String {
    let g = group.trim();
    if g.is_empty() || is_reserved_session_group(g) {
        String::new()
    } else {
        g.to_string()
    }
}

/// Last path segment of a nested group (`"a/b/c"` → `"c"`).
pub(crate) fn group_path_segment(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Parent path (`"a/b/c"` → `"a/b"`, top-level → `""`).
pub(crate) fn group_parent_path(path: &str) -> String {
    path.rfind('/')
        .map(|idx| path[..idx].to_string())
        .unwrap_or_default()
}

/// Join a parent path and a single segment (`("", "x")` → `"x"`).
pub(crate) fn group_join(parent: &str, segment: &str) -> String {
    let parent = parent.trim();
    let segment = segment.trim();
    if parent.is_empty() {
        segment.to_string()
    } else {
        format!("{parent}/{segment}")
    }
}

/// User-entered segment: non-empty, no `/`, not a reserved name.
pub(crate) fn is_valid_group_segment(segment: &str) -> bool {
    let s = segment.trim();
    !s.is_empty() && !s.contains('/') && !is_reserved_session_group(s)
}

/// Repair configurations created before #316/#324, when the Move-to menu exposed
/// the built-in `system` group as a destination for saved server sessions.
fn normalize_reserved_session_groups(cfg: &mut ConfigFile) -> bool {
    let old_group_count = cfg.groups.len();
    cfg.groups
        .retain(|group| !is_reserved_session_group(group.trim()));
    let mut changed = cfg.groups.len() != old_group_count;
    for session in &mut cfg.sessions {
        if is_reserved_session_group(session.group.trim()) {
            session.group.clear();
            changed = true;
        }
    }
    changed
}

#[cfg(any(target_os = "macos", test))]
fn normalize_macos_renderer_mode(mode: &str) -> &'static str {
    match mode {
        "femtovg" => "femtovg",
        "skia" => "skia",
        _ => "software",
    }
}

impl ConfigStore {
    /// Marks a password encrypted with the **portable export key** (issue #46).
    /// Kept only so hand-edited / older import files can still decrypt.
    const EXPORT_PREFIX: &'static str = "enc:exp:v1:";

    /// Fixed 32-byte key for portable exports. Baked into the binary so an
    /// exported file decrypts on any machine. Obfuscation only — see `ExportFile`.
    const EXPORT_KEY: [u8; 32] = *b"zinterm.export.portable.key.v01!";

    fn data_dir_path(&self) -> Result<PathBuf> {
        self.path
            .parent()
            .map(|p| p.to_path_buf())
            .context("config path has no parent directory")
    }

    /// Hydrate in-memory secrets from the OS-keyring vault.
    fn hydrate_secrets_from_vault(config_dir: &Path, sessions: &mut [Session]) {
        for session in sessions {
            // sessions.json must never carry secrets; drop anything leftover.
            session.password = Secret::default();
            session.key_passphrase = Secret::default();
            session.private_key = Secret::default();
            match crate::config::vault::get_secrets(config_dir, &session.id) {
                Ok(Some(secrets)) => {
                    session.password = Secret::new(secrets.password);
                    session.private_key = Secret::new(secrets.private_key);
                    session.key_passphrase = Secret::new(secrets.passphrase);
                }
                Ok(None) => {}
                Err(e) => tracing::warn!(
                    "failed to load vault secrets for session {}: {e:#}",
                    session.id
                ),
            }
        }
    }

    // ── Public API ────────────────────────────────────────────────────────

    /// Load (or initialise) the config files. On any parse error we back up the
    /// broken file and start fresh — losing saved sessions is better than
    /// crashing at launch.
    ///
    /// Supports the legacy monolithic `sessions.json` (everything in one file)
    /// and migrates to the split layout on the next save.
    pub fn load() -> Result<Self> {
        let path = Self::config_path()?;
        let config_dir = path
            .parent()
            .context("config path has no parent directory")?
            .to_path_buf();

        fs::create_dir_all(&config_dir)
            .with_context(|| format!("failed to create config dir {}", config_dir.display()))?;

        let settings_path = crate::config::persist::settings_path(&config_dir);
        let ui_path = crate::config::persist::ui_state_path(&config_dir);
        let split_layout = settings_path.exists() || ui_path.exists();

        let mut migrated = false;
        let cache = if split_layout || path.exists() {
            let mut cfg = if split_layout {
                Self::load_split(&path, &settings_path, &ui_path)?
            } else {
                match Self::load_legacy_monolithic(&path)? {
                    Some(cfg) => {
                        migrated = true;
                        cfg
                    }
                    None => fresh_config(),
                }
            };

            if cfg.save_passwords && !crate::config::vault::is_encryption_available() {
                tracing::warn!(
                    "save_passwords was on but credentials vault is unavailable; disabling"
                );
                cfg.save_passwords = false;
            }
            Self::hydrate_secrets_from_vault(&config_dir, &mut cfg.sessions);
            for session in &mut cfg.sessions {
                if session.sanitize_for_kind() {
                    migrated = true;
                }
            }
            for cmd in &mut cfg.command_history {
                *cmd = repair_history_newlines(std::mem::take(cmd));
            }
            dedup_keep_last(&mut cfg.command_history);
            migrated |= normalize_reserved_session_groups(&mut cfg);
            migrated |= migrate_defaults(&mut cfg);
            migrated |= migrate_output_highlight_rules(&mut cfg);
            let sanitized = crate::ssh::sanitize_algorithm_preferences(&cfg.algorithm_preferences);
            if sanitized != cfg.algorithm_preferences {
                cfg.algorithm_preferences = sanitized;
                migrated = true;
            }
            cfg
        } else {
            fresh_config()
        };

        let store = Self { path, cache };
        if migrated {
            if let Err(e) = store.save_parts(SaveKind::ALL) {
                tracing::warn!("failed to persist config migration: {e:#}");
            }
        }
        Ok(store)
    }

    fn load_split(sessions_path: &Path, settings_path: &Path, ui_path: &Path) -> Result<ConfigFile> {
        let mut cfg = ConfigFile::default();

        if sessions_path.exists() {
            let raw = fs::read_to_string(sessions_path)
                .with_context(|| format!("failed to read {}", sessions_path.display()))?;
            match serde_json::from_str::<SessionsFile>(&raw) {
                Ok(sessions) => sessions.apply_to(&mut cfg),
                Err(_) => match serde_json::from_str::<ConfigFile>(&raw) {
                    Ok(full) => cfg = full,
                    Err(err) => {
                        let backup = sessions_path.with_extension("json.broken");
                        let _ = fs::rename(sessions_path, &backup);
                        tracing::warn!(
                            "sessions file was corrupt ({err}); backed up to {}",
                            backup.display()
                        );
                    }
                },
            }
        }

        match crate::config::persist::read_json_file::<SettingsFile>(settings_path) {
            Ok(Some(settings)) => settings.apply_to(&mut cfg),
            Ok(None) => {}
            Err(err) => {
                tracing::warn!("settings file unreadable ({err:#}); keeping defaults");
            }
        }

        match crate::config::persist::read_json_file::<UiStateFile>(ui_path) {
            Ok(Some(ui)) => ui.apply_to(&mut cfg),
            Ok(None) => {}
            Err(err) => {
                tracing::warn!("ui-state file unreadable ({err:#}); keeping defaults");
            }
        }

        Ok(cfg)
    }

    fn load_legacy_monolithic(path: &Path) -> Result<Option<ConfigFile>> {
        let raw = fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        match serde_json::from_str::<ConfigFile>(&raw) {
            Ok(cfg) => Ok(Some(cfg)),
            Err(err) => {
                let backup = path.with_extension("json.broken");
                let _ = fs::rename(path, &backup);
                tracing::warn!(
                    "config file was corrupt ({err}); backed up to {}",
                    backup.display()
                );
                Ok(None)
            }
        }
    }

    fn config_path() -> Result<PathBuf> {
        Ok(data_dir().join("sessions.json"))
    }

    fn persist_snapshot(&self, kind: SaveKind) -> Result<crate::config::persist::PersistSnapshot> {
        let data_dir = self.data_dir_path()?;
        Ok(crate::config::persist::build_snapshot(
            data_dir,
            self.path.clone(),
            &self.cache,
            kind,
        ))
    }

    /// Synchronously write the selected parts (and optional vault). Used by
    /// tests, first-run migration, and shutdown flushes.
    pub fn save_parts(&self, kind: SaveKind) -> Result<()> {
        let snap = self.persist_snapshot(kind)?;
        crate::config::persist::write_snapshot(&snap)
    }

    /// Queue a background persist so the UI thread is not blocked on disk I/O.
    pub fn save_later(&self, kind: SaveKind) {
        match self.persist_snapshot(kind) {
            Ok(snap) => {
                if let Err(e) = crate::config::persist::schedule(snap) {
                    tracing::warn!("failed to schedule config persist: {e:#}");
                }
            }
            Err(e) => tracing::warn!("failed to build config persist snapshot: {e:#}"),
        }
    }

    /// Block until the background persist worker has finished pending writes.
    pub fn flush_persist() -> Result<()> {
        crate::config::persist::flush()
    }

    pub fn sessions(&self) -> &[Session] {
        &self.cache.sessions
    }

    #[allow(dead_code)] // reserved for an upcoming reorder/drag-drop feature
    pub fn sessions_mut(&mut self) -> &mut Vec<Session> {
        &mut self.cache.sessions
    }

    /// Insert or replace a session. Assigns a `saved-…` id when empty, and
    /// always refreshes [`Session::saved_at`]. Returns the final session id.
    pub fn upsert(&mut self, mut session: Session) -> String {
        session.sanitize_for_kind();
        if session.id.trim().is_empty() {
            session.id = Session::new_saved_id();
        }
        session.saved_at = Session::now_saved_at();
        if is_reserved_session_group(session.group.trim()) {
            session.group.clear();
        }
        let exclude = Some(session.id.as_str());
        session.name = self.disambiguate_session_name(
            session.group.trim(),
            session.name.trim(),
            exclude,
        );
        let id = session.id.clone();
        if let Some(existing) = self.cache.sessions.iter_mut().find(|s| s.id == session.id) {
            *existing = session;
        } else {
            self.cache.sessions.push(session);
        }
        id
    }

    pub fn remove(&mut self, id: &str) {
        self.cache.sessions.retain(|s| s.id != id);
        if let Ok(dir) = self.data_dir_path() {
            if let Err(e) = crate::config::vault::remove_secrets(&dir, id) {
                tracing::warn!("failed to remove vault secrets for {id}: {e:#}");
            }
        }
    }

    /// Delete every saved session and group folder (Quick Connect tree).
    /// When `clear_credentials` is true, also wipe the encrypted passwords /
    /// private-keys vault; otherwise vault entries are left in place (e.g. so a
    /// later restore of the same session ids can still unlock them).
    pub fn clear_sessions_and_groups(&mut self, clear_credentials: bool) {
        self.cache.sessions.clear();
        self.cache.groups.clear();
        self.cache.collapsed_session_groups = None;
        if clear_credentials {
            if let Ok(dir) = self.data_dir_path() {
                if let Err(e) = crate::config::vault::clear_all(&dir) {
                    tracing::warn!("failed to clear credentials vault: {e:#}");
                }
            }
        }
    }

    /// Reset Interface / appearance preferences to the current new-user defaults
    /// while keeping sessions, groups, quick commands, command history, and
    /// saved credentials intact. Does not touch known-hosts (separate file).
    /// Also resets layout chrome that lives in Settings (welcome sidebar width)
    /// and the SFTP preset download directory (system Downloads when available).
    pub fn restore_settings_defaults(&mut self) {
        let sessions = std::mem::take(&mut self.cache.sessions);
        let groups = std::mem::take(&mut self.cache.groups);
        let collapsed_session_groups = self.cache.collapsed_session_groups.take();
        let quick_commands = std::mem::take(&mut self.cache.quick_commands);
        let quick_groups = std::mem::take(&mut self.cache.quick_groups);
        let command_history = std::mem::take(&mut self.cache.command_history);

        self.cache = fresh_config();
        self.cache.sessions = sessions;
        self.cache.groups = groups;
        self.cache.collapsed_session_groups = collapsed_session_groups;
        self.cache.quick_commands = quick_commands;
        self.cache.quick_groups = quick_groups;
        self.cache.command_history = command_history;
        // Same first-run seed as startup: empty → user's Downloads folder.
        if self.cache.download_dir.is_empty() {
            if let Some(dl) = UserDirs::new()
                .and_then(|u| u.download_dir().map(|p| p.to_string_lossy().to_string()))
            {
                self.cache.download_dir = dl;
            }
        }
    }

    /// Snapshot of Settings-panel preferences (excludes sessions, commands, and
    /// runtime layout chrome such as panel sizes / dock edges).
    pub fn snapshot_settings_prefs(&self) -> ConfigFile {
        self.cache.clone()
    }

    /// Revert Settings-panel preferences from a snapshot taken when the panel
    /// opened. Leaves sessions, commands, and most layout chrome untouched so
    /// Cancel does not undo dock/panel resizes made elsewhere. Does restore
    /// download_dir and welcome_sidebar_width so Cancel can undo Restore defaults.
    pub fn restore_settings_prefs(&mut self, snap: &ConfigFile) {
        let c = &mut self.cache;
        c.language = snap.language.clone();
        c.theme_pref = snap.theme_pref.clone();
        c.renderer_mode = snap.renderer_mode.clone();
        c.font_family = snap.font_family.clone();
        c.font_size = snap.font_size;
        c.terminal_line_spacing = snap.terminal_line_spacing;
        c.terminal_bold = snap.terminal_bold;
        c.terminal_cursor_style = snap.terminal_cursor_style.clone();
        c.terminal_cursor_color = snap.terminal_cursor_color.clone();
        c.output_highlight_disabled = snap.output_highlight_disabled;
        c.output_highlight_preset = snap.output_highlight_preset.clone();
        c.output_highlight_rules = snap.output_highlight_rules.clone();
        c.json_format_disabled = snap.json_format_disabled;
        c.ui_scale = snap.ui_scale;
        c.wallpaper = snap.wallpaper.clone();
        c.sftp_no_follow_cd = snap.sftp_no_follow_cd;
        c.download_always_ask = snap.download_always_ask;
        c.paste_confirm_disabled = snap.paste_confirm_disabled;
        c.extra_paste_shortcuts_disabled = snap.extra_paste_shortcuts_disabled;
        c.select_copy_right_paste_disabled = snap.select_copy_right_paste_disabled;
        c.zen_mode = snap.zen_mode;
        c.quick_commands_as_sidebar = snap.quick_commands_as_sidebar;
        c.collapse_sftp_default = snap.collapse_sftp_default;
        c.welcome_as_sidebar = snap.welcome_as_sidebar;
        c.confirm_delete_group_disabled = snap.confirm_delete_group_disabled;
        c.confirm_delete_session = snap.confirm_delete_session;
        c.welcome_single_click_connect = snap.welcome_single_click_connect;
        c.wallpaper_overlay = snap.wallpaper_overlay;
        c.panel_font = snap.panel_font;
        c.update_check_disabled = snap.update_check_disabled;
        c.ssh_keepalive_secs = snap.ssh_keepalive_secs;
        c.algorithm_preferences = snap.algorithm_preferences.clone();
        c.save_passwords = snap.save_passwords;
        // Included so Cancel can undo Restore defaults (deferred until Save).
        c.download_dir = snap.download_dir.clone();
        c.welcome_sidebar_width = snap.welcome_sidebar_width;
    }

    pub fn get(&self, id: &str) -> Option<&Session> {
        self.cache.sessions.iter().find(|s| s.id == id)
    }

    /// True when another session in `group` already uses `name` (exact match).
    pub fn session_name_taken_in_group(
        &self,
        group: &str,
        name: &str,
        exclude_id: Option<&str>,
    ) -> bool {
        let group = normalize_session_group(group);
        let name = name.trim();
        if name.is_empty() {
            return false;
        }
        self.cache.sessions.iter().any(|s| {
            exclude_id.map(|id| s.id != id).unwrap_or(true)
                && normalize_session_group(&s.group) == group
                && s.name == name
        })
    }

    /// Keep `desired` when unique among sessions in `group`; else append
    /// `（1）`, `（2）`, … until free. `exclude_id` ignores that session (self).
    pub fn disambiguate_session_name(
        &self,
        group: &str,
        desired: &str,
        exclude_id: Option<&str>,
    ) -> String {
        let desired = desired.trim();
        if desired.is_empty() {
            return String::new();
        }
        if !self.session_name_taken_in_group(group, desired, exclude_id) {
            return desired.to_string();
        }
        for n in 1.. {
            let candidate = format!("{desired}（{n}）");
            if !self.session_name_taken_in_group(group, &candidate, exclude_id) {
                return candidate;
            }
        }
        unreachable!("unbounded session name suffix search");
    }

    /// All known group paths, including ancestors inferred from nested paths.
    fn known_group_paths(&self) -> std::collections::BTreeSet<String> {
        let mut paths = std::collections::BTreeSet::new();
        let mut insert = |raw: &str| {
            let g = raw.trim();
            if g.is_empty() || is_reserved_session_group(g) {
                return;
            }
            paths.insert(g.to_string());
            let mut rest = g;
            while let Some(idx) = rest.rfind('/') {
                rest = &rest[..idx];
                if !rest.is_empty() {
                    paths.insert(rest.to_string());
                }
            }
        };
        for group in &self.cache.groups {
            insert(group);
        }
        for session in &self.cache.sessions {
            insert(&session.group);
        }
        paths
    }

    /// Whether `path` is already a known group (excluding `exclude` full path).
    pub fn group_path_taken(&self, path: &str, exclude: Option<&str>) -> bool {
        let path = path.trim();
        if path.is_empty() {
            return false;
        }
        self.known_group_paths().iter().any(|g| {
            exclude
                .map(|e| !g.eq_ignore_ascii_case(e.trim()))
                .unwrap_or(true)
                && g.eq_ignore_ascii_case(path)
        })
    }

    /// Keep `desired` segment under `parent` when unique; else append `（1）`…
    pub fn disambiguate_group_segment(
        &self,
        parent: &str,
        desired: &str,
        exclude_full_path: Option<&str>,
    ) -> String {
        let desired = desired.trim();
        if desired.is_empty() {
            return String::new();
        }
        let parent = normalize_session_group(parent);
        let candidate = |seg: &str| group_join(&parent, seg);
        if !self.group_path_taken(&candidate(desired), exclude_full_path) {
            return desired.to_string();
        }
        for n in 1.. {
            let seg = format!("{desired}（{n}）");
            if !self.group_path_taken(&candidate(&seg), exclude_full_path) {
                return seg;
            }
        }
        unreachable!("unbounded group segment suffix search");
    }

    /// Move a session into `target_group` ("" = root). Renames on conflict.
    pub fn move_session_to_group(&mut self, id: &str, target_group: &str) -> bool {
        let target = normalize_session_group(target_group);
        let Some(idx) = self.cache.sessions.iter().position(|s| s.id == id) else {
            return false;
        };
        let current = normalize_session_group(&self.cache.sessions[idx].group);
        if current == target {
            return false;
        }
        let name = self.cache.sessions[idx].name.clone();
        let name = self.disambiguate_session_name(&target, &name, Some(id));
        self.cache.sessions[idx].group = target;
        self.cache.sessions[idx].name = name;
        true
    }

    /// Re-parent a group under `new_parent` ("" = top-level). Renames the
    /// segment on conflict. Rejects moves into self or a descendant.
    pub fn move_group_to_parent(&mut self, old: &str, new_parent: &str) -> bool {
        let old = old.trim();
        let new_parent = normalize_session_group(new_parent);
        if old.is_empty() || is_reserved_session_group(old) {
            return false;
        }
        if !new_parent.is_empty()
            && (new_parent.eq_ignore_ascii_case(old)
                || new_parent
                    .to_ascii_lowercase()
                    .starts_with(&format!("{}/", old.to_ascii_lowercase())))
        {
            return false;
        }
        if group_parent_path(old) == new_parent {
            return false;
        }
        let segment = group_path_segment(old).to_string();
        let segment = self.disambiguate_group_segment(&new_parent, &segment, Some(old));
        let new_full = group_join(&new_parent, &segment);
        if new_full.eq_ignore_ascii_case(old) {
            return false;
        }
        self.rename_group(old, new_full);
        true
    }

    pub fn download_dir(&self) -> &str {
        &self.cache.download_dir
    }

    pub fn set_download_dir(&mut self, dir: String) {
        self.cache.download_dir = dir;
    }

    /// UI language preference ("auto" default / "zh" / "en").
    pub fn language(&self) -> &str {
        crate::i18n::normalize_pref(&self.cache.language)
    }

    pub fn set_language(&mut self, lang: String) {
        self.cache.language = crate::i18n::normalize_pref(&lang).to_string();
    }

    /// Theme preference: "system" (default) | "dark" | "light".
    pub fn theme_pref(&self) -> &str {
        if self.cache.theme_pref.is_empty() {
            "system"
        } else {
            &self.cache.theme_pref
        }
    }

    pub fn set_theme_pref(&mut self, pref: String) {
        self.cache.theme_pref = pref;
    }

    /// Renderer preference for the current platform.
    #[cfg(target_os = "macos")]
    pub fn renderer_mode(&self) -> &str {
        normalize_macos_renderer_mode(&self.cache.renderer_mode)
    }

    /// Missing and invalid Windows values deliberately use software so upgrades
    /// preserve the high-DPI/VM compatibility from #224.
    #[cfg(target_os = "windows")]
    pub fn renderer_mode(&self) -> &str {
        match self.cache.renderer_mode.as_str() {
            "auto" => "auto",
            "gpu" => "gpu",
            _ => "software",
        }
    }

    #[cfg(target_os = "macos")]
    pub fn set_renderer_mode(&mut self, mode: String) {
        self.cache.renderer_mode = normalize_macos_renderer_mode(&mode).into();
    }

    /// Linux previously used Slint's automatic renderer selection and had no
    /// settings entry. Keep that behaviour for existing configurations.
    #[cfg(target_os = "linux")]
    pub fn renderer_mode(&self) -> &str {
        match self.cache.renderer_mode.as_str() {
            "gpu" => "gpu",
            "software" => "software",
            _ => "auto",
        }
    }

    #[cfg(target_os = "windows")]
    pub fn set_renderer_mode(&mut self, mode: String) {
        self.cache.renderer_mode = match mode.as_str() {
            "auto" => "auto".into(),
            "gpu" => "gpu".into(),
            _ => "software".into(),
        };
    }

    #[cfg(target_os = "linux")]
    pub fn set_renderer_mode(&mut self, mode: String) {
        self.cache.renderer_mode = match mode.as_str() {
            "gpu" => "gpu".into(),
            "software" => "software".into(),
            _ => "auto".into(),
        };
    }

    /// Terminal font family ("" = built-in default).
    pub fn font_family(&self) -> &str {
        &self.cache.font_family
    }

    pub fn set_font_family(&mut self, family: String) {
        self.cache.font_family = family;
    }

    /// Terminal font size in px (falls back to 13 when unset).
    pub fn font_size(&self) -> u32 {
        if self.cache.font_size == 0 {
            13
        } else {
            self.cache.font_size
        }
    }

    pub fn set_font_size(&mut self, size: u32) {
        self.cache.font_size = size.clamp(8, 32);
    }

    pub fn terminal_line_spacing(&self) -> f32 {
        let value = self.cache.terminal_line_spacing;
        if value <= 0.0 {
            1.0
        } else {
            value.clamp(0.8, 1.5)
        }
    }

    pub fn set_terminal_line_spacing(&mut self, value: f32) {
        self.cache.terminal_line_spacing = value.clamp(0.8, 1.5);
    }

    pub fn paste_confirm_enabled(&self) -> bool {
        !self.cache.paste_confirm_disabled
    }

    pub fn set_paste_confirm_enabled(&mut self, enabled: bool) {
        self.cache.paste_confirm_disabled = !enabled;
    }

    pub fn extra_paste_shortcuts_enabled(&self) -> bool {
        !self.cache.extra_paste_shortcuts_disabled
    }

    pub fn set_extra_paste_shortcuts_enabled(&mut self, enabled: bool) {
        self.cache.extra_paste_shortcuts_disabled = !enabled;
    }

    pub fn select_copy_right_paste_enabled(&self) -> bool {
        !self.cache.select_copy_right_paste_disabled
    }

    pub fn set_select_copy_right_paste_enabled(&mut self, enabled: bool) {
        self.cache.select_copy_right_paste_disabled = !enabled;
    }

    pub fn zen_mode(&self) -> bool {
        self.cache.zen_mode
    }

    pub fn set_zen_mode(&mut self, enabled: bool) {
        self.cache.zen_mode = enabled;
    }

    /// Force regular terminal text to render with a bold face (#262).
    pub fn terminal_bold(&self) -> bool {
        self.cache.terminal_bold
    }

    pub fn set_terminal_bold(&mut self, bold: bool) {
        self.cache.terminal_bold = bold;
    }

    /// Selected terminal insertion cursor shape. Legacy empty and invalid values
    /// use the bar cursor (current default).
    pub fn terminal_cursor_style(&self) -> &str {
        match self.cache.terminal_cursor_style.as_str() {
            "block" => "block",
            "underline" => "underline",
            _ => "bar",
        }
    }

    pub fn set_terminal_cursor_style(&mut self, style: String) {
        self.cache.terminal_cursor_style = match style.as_str() {
            "block" => "block".into(),
            "underline" => "underline".into(),
            _ => "bar".into(),
        };
    }

    pub fn terminal_cursor_color(&self) -> &str {
        if normalize_hex_color(&self.cache.terminal_cursor_color).is_some() {
            &self.cache.terminal_cursor_color
        } else {
            ""
        }
    }

    pub fn set_terminal_cursor_color(&mut self, color: &str) -> bool {
        let Some(normalized) = normalize_hex_color(color) else {
            return false;
        };
        self.cache.terminal_cursor_color = normalized;
        true
    }

    /// Whether client-side highlighting of otherwise unstyled output is active.
    pub fn output_highlight_enabled(&self) -> bool {
        !self.cache.output_highlight_disabled
    }

    pub fn set_output_highlight_enabled(&mut self, enabled: bool) {
        self.cache.output_highlight_disabled = !enabled;
    }

    pub fn json_format_output(&self) -> bool {
        !self.cache.json_format_disabled
    }

    pub fn set_json_format_output(&mut self, enabled: bool) {
        self.cache.json_format_disabled = !enabled;
    }

    /// Selected built-in rule set. Unknown values safely fall back to the
    /// conservative log-level preset for forward/backward compatibility.
    pub fn output_highlight_preset(&self) -> &str {
        match self.cache.output_highlight_preset.as_str() {
            "devops" => "devops",
            _ => "log",
        }
    }

    pub fn set_output_highlight_preset(&mut self, preset: String) {
        self.cache.output_highlight_preset = match preset.as_str() {
            "devops" => "devops".to_string(),
            _ => "log".to_string(),
        };
    }

    pub fn output_highlight_rules(&self) -> &[OutputHighlightRule] {
        &self.cache.output_highlight_rules
    }

    pub fn add_output_highlight_rule(&mut self, mut rule: OutputHighlightRule) {
        rule.name = rule.name.trim().to_string();
        rule.pattern = rule.pattern.trim().to_string();
        rule.color = normalize_highlight_color(&rule.color);
        self.cache.output_highlight_rules.push(rule);
    }

    pub fn update_output_highlight_rule(&mut self, index: usize, mut rule: OutputHighlightRule) {
        let Some(slot) = self.cache.output_highlight_rules.get_mut(index) else {
            return;
        };
        rule.name = rule.name.trim().to_string();
        rule.pattern = rule.pattern.trim().to_string();
        rule.color = normalize_highlight_color(&rule.color);
        // Preserve enabled unless the caller set it explicitly via the dedicated API.
        rule.enabled = slot.enabled;
        *slot = rule;
    }

    pub fn remove_output_highlight_rule(&mut self, index: usize) {
        if index < self.cache.output_highlight_rules.len() {
            self.cache.output_highlight_rules.remove(index);
        }
    }

    pub fn set_output_highlight_rule_enabled(&mut self, index: usize, enabled: bool) {
        if let Some(rule) = self.cache.output_highlight_rules.get_mut(index) {
            rule.enabled = enabled;
        }
    }

    /// Global UI scale in percent (#100). Defaults to 100.
    pub fn ui_scale(&self) -> u32 {
        if self.cache.ui_scale == 0 {
            100
        } else {
            self.cache.ui_scale
        }
    }

    pub fn set_ui_scale(&mut self, percent: u32) {
        self.cache.ui_scale = percent.clamp(80, 200);
    }

    /// Immersive wallpaper id ("" = none).
    pub fn wallpaper(&self) -> &str {
        &self.cache.wallpaper
    }

    pub fn set_wallpaper(&mut self, id: impl Into<String>) {
        self.cache.wallpaper = id.into();
    }

    /// Whether the SFTP panel follows the terminal's cd (default true).
    pub fn sftp_follow_cd(&self) -> bool {
        !self.cache.sftp_no_follow_cd
    }

    pub fn set_sftp_follow_cd(&mut self, follow: bool) {
        self.cache.sftp_no_follow_cd = !follow;
    }

    /// Saved quick commands (#55).
    pub fn quick_commands(&self) -> &[QuickCommand] {
        &self.cache.quick_commands
    }

    pub fn set_quick_commands(&mut self, cmds: Vec<QuickCommand>) {
        self.cache.quick_commands = cmds;
    }

    pub fn quick_panel_open(&self) -> bool {
        self.cache.quick_panel_open
    }

    pub fn quick_commands_as_sidebar(&self) -> bool {
        self.cache.quick_commands_as_sidebar
    }

    pub fn set_quick_commands_as_sidebar(&mut self, enabled: bool) {
        self.cache.quick_commands_as_sidebar = enabled;
        if !enabled {
            self.cache.quick_panel_open = false;
        }
    }

    pub fn set_quick_panel_open(&mut self, open: bool) {
        self.cache.quick_panel_open = open;
    }

    pub fn quick_panel_collapsed(&self) -> bool {
        self.cache.quick_panel_collapsed
    }

    pub fn set_quick_panel_collapsed(&mut self, collapsed: bool) {
        self.cache.quick_panel_collapsed = collapsed;
    }

    pub fn quick_panel_width(&self) -> f32 {
        let width = self.cache.quick_panel_width;
        if width <= 0.0 {
            default_quick_panel_width()
        } else {
            width
        }
    }

    pub fn set_quick_panel_width(&mut self, width: f32) {
        self.cache.quick_panel_width = width;
    }

    pub fn quick_panel_height(&self) -> f32 {
        let height = self.cache.quick_panel_height;
        if height <= 0.0 {
            default_quick_panel_height()
        } else {
            height
        }
    }

    pub fn set_quick_panel_height(&mut self, height: f32) {
        self.cache.quick_panel_height = height;
    }

    pub fn quick_panel_dock(&self) -> String {
        match self.cache.quick_panel_dock.trim() {
            "left" | "right" | "top" | "bottom" => self.cache.quick_panel_dock.clone(),
            _ => "right".into(),
        }
    }

    pub fn set_quick_panel_dock(&mut self, dock: String) {
        self.cache.quick_panel_dock = dock;
    }

    /// Explicit quick-command groups (#55) — parallels [`groups`](Self::groups).
    pub fn quick_groups(&self) -> &[String] {
        &self.cache.quick_groups
    }

    /// Create an empty quick-command group. Ignores blank, "default", duplicates.
    pub fn add_quick_group(&mut self, name: String) {
        let n = name.trim().to_string();
        if n.is_empty() || n.eq_ignore_ascii_case("default") {
            return;
        }
        if !self.cache.quick_groups.iter().any(|g| g == &n) {
            self.cache.quick_groups.push(n);
        }
    }

    /// Delete a quick-command group; any command still in it falls back to
    /// ungrouped (the UI only offers delete on empty groups, but clear defensively).
    pub fn remove_quick_group(&mut self, name: &str) {
        self.cache.quick_groups.retain(|g| g != name);
        for c in &mut self.cache.quick_commands {
            if c.group == name {
                c.group.clear();
            }
        }
    }

    /// Rename a quick-command group, moving its commands along. No-op for
    /// blank / "default".
    pub fn rename_quick_group(&mut self, old: &str, new: String) {
        let n = new.trim().to_string();
        if n.is_empty() || n.eq_ignore_ascii_case("default") || n == old {
            return;
        }
        for g in &mut self.cache.quick_groups {
            if g == old {
                *g = n.clone();
            }
        }
        for c in &mut self.cache.quick_commands {
            if c.group == old {
                c.group = n.clone();
            }
        }
        self.cache.quick_groups.dedup();
    }

    /// Ordered named quick-command groups: explicit `quick_groups` first, then
    /// any group referenced by a command that is not yet listed (first-seen order).
    pub fn materialized_quick_groups(&self) -> Vec<String> {
        let mut ordered: Vec<String> = self.cache.quick_groups.clone();
        for c in &self.cache.quick_commands {
            let g = c.group.trim();
            if !g.is_empty() && !ordered.iter().any(|x| x == g) {
                ordered.push(g.to_string());
            }
        }
        ordered
    }

    /// Move a named quick-command group to sit immediately before `before` in the
    /// display order. `before` empty appends to the end. "default" cannot move.
    pub fn reorder_quick_group(&mut self, from: &str, before: &str) -> bool {
        if from.trim().is_empty() || from.eq_ignore_ascii_case("default") {
            return false;
        }
        let mut groups = self.materialized_quick_groups();
        let Some(from_idx) = groups.iter().position(|g| g == from) else {
            return false;
        };
        if before.is_empty() {
            let item = groups.remove(from_idx);
            groups.push(item);
        } else if before.eq_ignore_ascii_case("default") {
            return false;
        } else if from == before {
            return false;
        } else {
            let Some(mut to_idx) = groups.iter().position(|g| g == before) else {
                return false;
            };
            let item = groups.remove(from_idx);
            if to_idx > from_idx {
                to_idx -= 1;
            }
            groups.insert(to_idx, item);
        }
        self.cache.quick_groups = groups;
        true
    }

    /// Update one quick command in place by index (#55).
    pub fn update_quick_command(&mut self, index: usize, cmd: QuickCommand) {
        if let Some(slot) = self.cache.quick_commands.get_mut(index) {
            *slot = cmd;
        }
    }

    /// Recent command-box history, oldest first (#55).
    pub fn command_history(&self) -> &[String] {
        &self.cache.command_history
    }

    /// Append a command to the history: skips blanks, de-duplicates globally so
    /// each command appears once, and re-appends at the end so the most-recently
    /// used command is always last. Capped so it can't grow without bound (#113).
    pub fn push_command_history(&mut self, cmd: String) {
        let cmd = repair_history_newlines(cmd);
        if cmd.trim().is_empty() {
            return;
        }
        // Drop any earlier occurrence, then push → no duplicates and "last used"
        // moves to the end (bash `HISTCONTROL=erasedups` semantics).
        self.cache.command_history.retain(|c| c != &cmd);
        const CAP: usize = 200;
        self.cache.command_history.push(cmd);
        let len = self.cache.command_history.len();
        if len > CAP {
            self.cache.command_history.drain(0..len - CAP);
        }
    }

    /// Remove a single command-history entry by storage index (#96).
    pub fn remove_command_history(&mut self, index: usize) {
        if index < self.cache.command_history.len() {
            self.cache.command_history.remove(index);
        }
    }

    pub fn welcome_as_sidebar(&self) -> bool {
        self.cache.welcome_as_sidebar
    }
    pub fn set_welcome_as_sidebar(&mut self, v: bool) {
        self.cache.welcome_as_sidebar = v;
    }
    pub fn confirm_delete_group(&self) -> bool {
        !self.cache.confirm_delete_group_disabled
    }
    pub fn set_confirm_delete_group(&mut self, enabled: bool) {
        self.cache.confirm_delete_group_disabled = !enabled;
    }
    pub fn confirm_delete_session(&self) -> bool {
        self.cache.confirm_delete_session
    }
    pub fn set_confirm_delete_session(&mut self, enabled: bool) {
        self.cache.confirm_delete_session = enabled;
    }
    pub fn welcome_single_click_connect(&self) -> bool {
        self.cache.welcome_single_click_connect
    }
    pub fn set_welcome_single_click_connect(&mut self, enabled: bool) {
        self.cache.welcome_single_click_connect = enabled;
    }
    pub fn welcome_sidebar_width(&self) -> f32 {
        let w = self.cache.welcome_sidebar_width;
        if w <= 0.0 {
            350.0
        } else {
            w
        }
    }
    pub fn set_welcome_sidebar_width(&mut self, v: f32) {
        self.cache.welcome_sidebar_width = v;
    }
    pub fn welcome_sidebar_dock(&self) -> String {
        let d = self.cache.welcome_sidebar_dock.trim();
        if d.is_empty() {
            "left".into()
        } else {
            d.to_string()
        }
    }
    pub fn set_welcome_sidebar_dock(&mut self, v: String) {
        self.cache.welcome_sidebar_dock = v;
    }
    pub fn welcome_collapsed(&self) -> Option<bool> {
        self.cache.welcome_collapsed
    }
    pub fn set_welcome_collapsed(&mut self, v: bool) {
        self.cache.welcome_collapsed = Some(v);
    }
    pub fn welcome_session_col_name(&self) -> f32 {
        let w = self.cache.welcome_session_col_name;
        if w > 0.0 {
            w
        } else {
            default_welcome_session_col_name()
        }
    }
    pub fn set_welcome_session_col_name(&mut self, v: f32) {
        self.cache.welcome_session_col_name = v.clamp(64.0, 600.0);
    }
    pub fn welcome_session_col_host(&self) -> f32 {
        let w = self.cache.welcome_session_col_host;
        if w > 0.0 {
            w
        } else {
            default_welcome_session_col_host()
        }
    }
    pub fn set_welcome_session_col_host(&mut self, v: f32) {
        self.cache.welcome_session_col_host = v.clamp(64.0, 600.0);
    }
    /// Whether the startup new-version check is enabled (#184).
    pub fn update_check_enabled(&self) -> bool {
        !self.cache.update_check_disabled
    }
    pub fn set_update_check_enabled(&mut self, enabled: bool) {
        self.cache.update_check_disabled = !enabled;
    }
    /// SSH keepalive interval in seconds. 0 disables keepalive.
    pub fn ssh_keepalive_secs(&self) -> u32 {
        self.cache.ssh_keepalive_secs.min(SSH_KEEPALIVE_SECS_MAX)
    }
    pub fn set_ssh_keepalive_secs(&mut self, secs: u32) {
        self.cache.ssh_keepalive_secs = secs.min(SSH_KEEPALIVE_SECS_MAX);
    }

    pub fn algorithm_preferences(&self) -> AlgorithmPreferences {
        crate::ssh::sanitize_algorithm_preferences(&self.cache.algorithm_preferences)
    }

    pub fn set_algorithm_preferences(&mut self, prefs: AlgorithmPreferences) {
        self.cache.algorithm_preferences = crate::ssh::sanitize_algorithm_preferences(&prefs);
    }
    /// Whether newly entered passwords / key paths / key material may be written
    /// to the credentials vault.
    pub fn save_passwords(&self) -> bool {
        self.cache.save_passwords
    }
    pub fn set_save_passwords(&mut self, enabled: bool) {
        // Refuse enabling when the OS keyring cannot hold a master key.
        self.cache.save_passwords = enabled && crate::config::vault::is_encryption_available();
        if enabled && !self.cache.save_passwords {
            tracing::warn!("save passwords requested but credentials vault is unavailable");
        }
    }
    /// Wipe every session's stored password/passphrase, pasted private key, and
    /// private-key file path (memory + vault).
    pub fn clear_saved_passwords_and_keys(&mut self) {
        for session in &mut self.cache.sessions {
            session.password = Secret::default();
            session.key_passphrase = Secret::default();
            session.private_key = Secret::default();
        }
        if let Ok(dir) = self.data_dir_path() {
            if let Err(e) = crate::config::vault::clear_all(&dir) {
                tracing::warn!("failed to clear credentials vault: {e:#}");
            }
        }
    }
    pub fn wallpaper_overlay(&self) -> f32 {
        let a = self.cache.wallpaper_overlay;
        // Floor lowered 0.40 -> 0.30 so more see-through panels are reachable.
        if a <= 0.0 {
            DEFAULT_WALLPAPER_OVERLAY
        } else {
            a.clamp(0.30, 1.0)
        }
    }
    pub fn set_wallpaper_overlay(&mut self, v: f32) {
        self.cache.wallpaper_overlay = v.clamp(0.30, 1.0);
    }
    pub fn panel_font(&self) -> u32 {
        if self.cache.panel_font == 0 {
            100
        } else {
            self.cache.panel_font
        }
    }
    pub fn set_panel_font(&mut self, percent: u32) {
        self.cache.panel_font = percent.clamp(80, 160);
    }
    pub fn sftp_panel_width(&self) -> f32 {
        let w = self.cache.sftp_panel_width;
        if w <= 0.0 {
            default_sftp_width()
        } else {
            w
        }
    }
    pub fn set_sftp_panel_width(&mut self, v: f32) {
        self.cache.sftp_panel_width = v;
    }
    pub fn sftp_panel_height(&self) -> f32 {
        let h = self.cache.sftp_panel_height;
        if h <= 0.0 {
            default_sftp_height()
        } else {
            h
        }
    }
    pub fn set_sftp_panel_height(&mut self, v: f32) {
        self.cache.sftp_panel_height = v;
    }
    pub fn sftp_tree_width(&self) -> f32 {
        let width = self.cache.sftp_tree_width;
        if width <= 0.0 {
            default_sftp_tree_width()
        } else {
            width.clamp(120.0, 420.0)
        }
    }
    pub fn set_sftp_tree_width(&mut self, width: f32) {
        self.cache.sftp_tree_width = width.clamp(120.0, 420.0);
    }
    pub fn sftp_dock(&self) -> String {
        let d = self.cache.sftp_dock.trim();
        if d.is_empty() {
            "bottom".into()
        } else {
            d.to_string()
        }
    }
    pub fn set_sftp_dock(&mut self, v: String) {
        self.cache.sftp_dock = v;
    }
    /// Last window size in logical px; `(0,0)` means unset (use the default).
    pub fn window_size(&self) -> (f32, f32) {
        (self.cache.window_width, self.cache.window_height)
    }
    pub fn set_window_size(&mut self, w: f32, h: f32) {
        self.cache.window_width = w;
        self.cache.window_height = h;
    }

    /// Collapse the SFTP panel on startup (default true for new installs) (#78).
    pub fn collapse_sftp_default(&self) -> bool {
        self.cache.collapse_sftp_default
    }

    pub fn set_collapse_sftp_default(&mut self, v: bool) {
        self.cache.collapse_sftp_default = v;
    }

    /// Whether each download prompts for a save location (default false) (#87).
    pub fn download_always_ask(&self) -> bool {
        self.cache.download_always_ask
    }

    pub fn set_download_always_ask(&mut self, ask: bool) {
        self.cache.download_always_ask = ask;
    }

    // ── Session groups / folders (#41) ────────────────────────────────────

    /// Explicit groups (empty folders included). "default" is implicit.
    pub fn groups(&self) -> &[String] {
        &self.cache.groups
    }

    pub fn collapsed_session_groups(&self) -> Option<&[String]> {
        self.cache.collapsed_session_groups.as_deref()
    }

    /// Remember a Quick Connect folder's open/closed state. On the first
    /// interaction, materialise the default-collapsed state for every existing
    /// folder so expanding one folder does not accidentally expand the rest.
    pub fn set_session_group_collapsed(&mut self, name: &str, collapsed: bool) {
        self.ensure_collapsed_session_groups();

        let groups = self.cache.collapsed_session_groups.as_mut().unwrap();
        groups.retain(|group| group != name);
        if collapsed {
            groups.push(name.to_string());
            groups.sort();
            groups.dedup();
        }
    }

    /// Expand or collapse every Quick Connect folder at once.
    pub fn set_all_session_groups_collapsed(&mut self, collapsed: bool) {
        let all = self.collect_session_group_paths();
        self.cache.collapsed_session_groups = Some(if collapsed { all } else { Vec::new() });
    }

    /// Expand or collapse every descendant of `path`. When expanding, the
    /// folder itself is also opened so the children become visible; when
    /// collapsing, only descendants are closed so their headers stay shown.
    pub fn set_session_group_children_collapsed(&mut self, path: &str, collapsed: bool) {
        let path = path.trim();
        if path.is_empty() || is_reserved_session_group(path) {
            return;
        }
        let all = self.collect_session_group_paths();
        self.ensure_collapsed_session_groups();
        let prefix = format!("{path}/");
        let groups = self.cache.collapsed_session_groups.as_mut().unwrap();
        if collapsed {
            groups.retain(|group| group != path && !group.starts_with(&prefix));
            for group in &all {
                if group.starts_with(&prefix) {
                    groups.push(group.clone());
                }
            }
            groups.sort();
            groups.dedup();
        } else {
            groups.retain(|group| group != path && !group.starts_with(&prefix));
        }
    }

    fn ensure_collapsed_session_groups(&mut self) {
        if self.cache.collapsed_session_groups.is_some() {
            return;
        }
        self.cache.collapsed_session_groups = Some(self.collect_session_group_paths());
    }

    fn collect_session_group_paths(&self) -> Vec<String> {
        let mut groups = self.cache.groups.clone();
        groups.extend(
            self.cache
                .sessions
                .iter()
                .filter(|session| {
                    let group = session.group.trim();
                    !group.is_empty() && !is_reserved_session_group(group)
                })
                .map(|session| session.group.clone()),
        );
        // Also materialise ancestor folders implied by nested paths.
        let nested: Vec<String> = groups
            .iter()
            .flat_map(|group| {
                let mut out = Vec::new();
                let mut rest = group.as_str();
                while let Some(idx) = rest.rfind('/') {
                    rest = &rest[..idx];
                    if !rest.is_empty() {
                        out.push(rest.to_string());
                    }
                }
                out
            })
            .collect();
        groups.extend(nested);
        groups.sort();
        groups.dedup();
        groups
    }

    /// Whether a user group already exists, including groups inferred from
    /// sessions that were created before explicit group records were added.
    pub fn session_group_exists(&self, name: &str) -> bool {
        let target = name.trim();
        if target.is_empty() {
            return false;
        }
        self.cache
            .groups
            .iter()
            .any(|group| group.trim().eq_ignore_ascii_case(target))
            || self.cache.sessions.iter().any(|session| {
                !session.group.trim().is_empty()
                    && session.group.trim().eq_ignore_ascii_case(target)
            })
    }

    /// Create an empty group. Ignores blank/reserved names and duplicates.
    pub fn add_group(&mut self, name: String) {
        let n = name.trim().to_string();
        if n.is_empty() || is_reserved_session_group(&n) || self.session_group_exists(&n) {
            return;
        }
        self.cache.groups.push(n.clone());
        if let Some(groups) = &mut self.cache.collapsed_session_groups {
            groups.push(n);
            groups.sort();
            groups.dedup();
        }
    }

    /// Delete a group and cascade: nested child groups and all sessions in this
    /// group or any descendant are removed as well.
    pub fn remove_group(&mut self, name: &str) {
        let name = name.trim();
        if name.is_empty() || is_reserved_session_group(name) {
            return;
        }
        let prefix = format!("{name}/");
        self.cache
            .groups
            .retain(|g| g != name && !g.starts_with(&prefix));
        if let Some(groups) = &mut self.cache.collapsed_session_groups {
            groups.retain(|g| g != name && !g.starts_with(&prefix));
        }
        self.cache
            .sessions
            .retain(|s| s.group != name && !s.group.starts_with(&prefix));
    }

    /// Rename a group path, moving its sessions and nested descendants along.
    /// No-op for reserved names.
    pub fn rename_group(&mut self, old: &str, new: String) {
        let old = old.trim();
        let n = new.trim().to_string();
        if n.is_empty()
            || is_reserved_session_group(old)
            || is_reserved_session_group(&n)
            || n == old
            || (!n.eq_ignore_ascii_case(old) && self.session_group_exists(&n))
        {
            return;
        }
        let old_prefix = format!("{old}/");
        let new_prefix = format!("{n}/");
        for g in &mut self.cache.groups {
            if g == old {
                *g = n.clone();
            } else if g.starts_with(&old_prefix) {
                *g = format!("{new_prefix}{}", g.strip_prefix(&old_prefix).unwrap_or(g));
            }
        }
        for s in &mut self.cache.sessions {
            if s.group == old {
                s.group = n.clone();
            } else if s.group.starts_with(&old_prefix) {
                s.group = format!("{new_prefix}{}", s.group.strip_prefix(&old_prefix).unwrap_or(&s.group));
            }
        }
        if let Some(groups) = &mut self.cache.collapsed_session_groups {
            for group in groups.iter_mut() {
                if group == old {
                    *group = n.clone();
                } else if group.starts_with(&old_prefix) {
                    *group = format!(
                        "{new_prefix}{}",
                        group.strip_prefix(&old_prefix).unwrap_or(group)
                    );
                }
            }
            groups.sort();
            groups.dedup();
        }
        self.cache.groups.sort();
        self.cache.groups.dedup();
    }

    /// Synchronously write every config file + vault.
    pub fn save(&self) -> Result<()> {
        self.save_parts(SaveKind::ALL)
    }

    // ── Portable export / import (issue #46) ──────────────────────────────

    /// `Date.toString()`-style local timestamp for the export file header.
    fn format_export_timestamp() -> String {
        let now = chrono::Local::now();
        let offset_secs = now.offset().local_minus_utc();
        let sign = if offset_secs >= 0 { '+' } else { '-' };
        let abs = offset_secs.unsigned_abs();
        let oh = abs / 3600;
        let om = (abs % 3600) / 60;
        let zone = if offset_secs == 8 * 3600 {
            "中国标准时间"
        } else {
            "Local"
        };
        format!(
            "{} GMT{}{:02}{:02} ({})",
            now.format("%a %b %d %Y %H:%M:%S"),
            sign,
            oh,
            om,
            zone
        )
    }

    /// Decrypt a legacy export ciphertext (`enc:exp:v1:…`); `None` if it isn't one.
    fn decrypt_export(s: &str) -> Option<String> {
        let b64 = s.strip_prefix(Self::EXPORT_PREFIX)?;
        let blob = URL_SAFE_NO_PAD.decode(b64).ok()?;
        if blob.len() < 12 {
            return None;
        }
        let (nonce_bytes, ciphertext) = blob.split_at(12);
        let cipher = ChaCha20Poly1305::new((&Self::EXPORT_KEY).into());
        let nonce = chacha20poly1305::Nonce::from_slice(nonce_bytes);
        let plain = cipher.decrypt(nonce, ciphertext).ok()?;
        String::from_utf8(plain).ok()
    }

    /// Export all sessions to a portable JSON file. Connection metadata stays
    /// plaintext and editable; password / private-key fields are cleared so the
    /// export never carries secrets. Returns the number of sessions.
    pub fn export_json(&self) -> Result<(String, usize)> {
        let empty_groups = self.collect_empty_groups();
        let mut sessions = self.cache.sessions.clone();
        for s in &mut sessions {
            s.sanitize_for_kind();
            // Never put secrets in the portable export; hand-fill on import.
            s.password = Secret::default();
            s.key_passphrase = Secret::default();
            s.private_key = Secret::default();
            // `last_used` is machine-local noise — don't carry it across.
            s.last_used = None;
        }
        let count = sessions.len();
        let out = ExportFile {
            zinterm_export: "sessions".into(),
            version: 1,
            exported_at: Self::format_export_timestamp(),
            empty_groups,
            sessions,
        };
        Ok((serde_json::to_string_pretty(&out)?, count))
    }

    /// Explicit `cache.groups` entries that currently have no session in that
    /// folder or any descendant path.
    fn collect_empty_groups(&self) -> Vec<String> {
        self.cache
            .groups
            .iter()
            .filter(|g| {
                let g = g.trim();
                if g.is_empty() || is_reserved_session_group(g) {
                    return false;
                }
                let prefix = format!("{g}/");
                !self.cache.sessions.iter().any(|s| {
                    let sg = s.group.trim();
                    sg == g || sg.starts_with(&prefix)
                })
            })
            .cloned()
            .collect()
    }

    /// Export all sessions to a portable JSON file. Connection metadata stays
    /// plaintext and editable; password / private-key fields are cleared so the
    /// export never carries secrets. Returns the number of sessions.
    pub fn export_to(&self, path: &Path) -> Result<usize> {
        let (raw, count) = self.export_json()?;
        fs::write(path, raw).with_context(|| format!("failed to write {}", path.display()))?;
        Ok(count)
    }

    /// Import sessions from a string produced by [`Self::export_json`].
    ///
    /// Each session must include `kind`; SSH/Telnet also need a non-empty
    /// `host`, and Serial needs a non-empty `serial_port`. Other missing or
    /// malformed fields follow new-session dialog defaults. Optional plaintext
    /// `password` (login, password auth), `key_passphrase` / `private_key`
    /// (key auth; multi-line keys use `\n`) are kept only when Settings › Data ›
    /// save passwords is on; otherwise they are dropped before persist. A
    /// session is skipped only when the same group already has that `id` or the
    /// same `name`. Empty groups are restored first. Returns `(added, skipped)`.
    /// The store is saved if anything was added (sessions or empty groups).
    pub fn import_json(&mut self, raw: &str) -> Result<(usize, usize)> {
        let file: ExportFileImport =
            serde_json::from_str(raw).context("not a valid zinterm export file")?;
        if file.zinterm_export != "sessions" {
            anyhow::bail!(
                "invalid export: zinterm_export must be \"sessions\" (got {:?})",
                file.zinterm_export
            );
        }
        if file.version != 1 {
            anyhow::bail!(
                "unsupported export version {} (expected 1)",
                file.version
            );
        }

        // Restore empty folders before sessions so the Quick Connect tree
        // already has those paths when connections land in sibling groups.
        let mut groups_added = false;
        for group in &file.empty_groups {
            let before = self.cache.groups.len();
            self.add_group(group.clone());
            if self.cache.groups.len() > before {
                groups_added = true;
            }
        }

        let save_passwords = self.save_passwords();
        let mut added = 0usize;
        let mut skipped = 0usize;
        for (i, raw_session) in file.sessions.iter().enumerate() {
            let mut s = Session::from_import_value(raw_session)
                .with_context(|| format!("session[{i}]"))?;
            // Recover plaintext secrets for in-memory use / vault sync.
            // Accept an older export blob or hand-edited plaintext.
            if let Some(plain) = Self::decrypt_export(s.password.as_str()) {
                s.password = Secret::new(plain);
            }
            if let Some(plain) = Self::decrypt_export(s.key_passphrase.as_str()) {
                s.key_passphrase = Secret::new(plain);
            }
            if let Some(plain) = Self::decrypt_export(s.private_key.as_str()) {
                s.private_key = Secret::new(plain);
            }
            // Match Settings › Data › save passwords: ignore newly imported
            // secrets when the switch is off.
            if !save_passwords {
                s.password = Secret::default();
                s.key_passphrase = Secret::default();
                s.private_key = Secret::default();
            }
            s.sanitize_for_kind();
            if is_reserved_session_group(s.group.trim()) {
                s.group.clear();
            }

            let group = normalize_session_group(&s.group);
            let name = s.name.trim();
            let dup = self.cache.sessions.iter().any(|x| {
                normalize_session_group(&x.group) == group
                    && ((!s.id.is_empty() && x.id == s.id) || x.name.trim() == name)
            });
            if dup {
                skipped += 1;
                continue;
            }

            // Keep export id / saved_at; only fill blanks from broken files.
            if s.id.trim().is_empty() {
                s.id = Session::new_saved_id();
            }
            if s.saved_at == 0 {
                s.saved_at = Session::now_saved_at();
            }
            self.cache.sessions.push(s);
            added += 1;
        }
        if added > 0 || groups_added {
            self.save()?;
        }
        Ok((added, skipped))
    }

    /// Import sessions from a file produced by [`Self::export_to`].
    pub fn import_from(&mut self, path: &Path) -> Result<(usize, usize)> {
        let raw = fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        self.import_json(&raw)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chacha20poly1305::aead::{AeadCore, OsRng};
    use uuid::Uuid;

    fn temp_store() -> ConfigStore {
        let dir = std::env::temp_dir().join(format!("ms-test-{}", Uuid::new_v4()));
        let _ = std::fs::create_dir_all(&dir);
        ConfigStore {
            path: dir.join("sessions.json"),
            cache: ConfigFile::default(),
        }
    }

    /// Build a legacy `enc:exp:v1:…` blob so import can still decrypt older exports.
    fn encrypt_export(plaintext: &str) -> Result<String> {
        let cipher = ChaCha20Poly1305::new((&ConfigStore::EXPORT_KEY).into());
        let nonce = ChaCha20Poly1305::generate_nonce(&mut OsRng);
        let ciphertext = cipher
            .encrypt(&nonce, plaintext.as_bytes())
            .map_err(|e| anyhow::anyhow!("export encrypt error: {e}"))?;
        let mut blob = nonce.to_vec();
        blob.extend_from_slice(&ciphertext);
        Ok(format!(
            "{}{}",
            ConfigStore::EXPORT_PREFIX,
            URL_SAFE_NO_PAD.encode(&blob)
        ))
    }

    #[test]
    fn terminal_cursor_style_defaults_and_validates() {
        let mut store = temp_store();
        assert_eq!(store.terminal_cursor_style(), "bar");

        store.set_terminal_cursor_style("block".into());
        assert_eq!(store.terminal_cursor_style(), "block");
        store.set_terminal_cursor_style("underline".into());
        assert_eq!(store.terminal_cursor_style(), "underline");
        store.set_terminal_cursor_style("unexpected".into());
        assert_eq!(store.terminal_cursor_style(), "bar");

        store.cache = serde_json::from_str("{}").expect("legacy config must deserialize");
        assert_eq!(store.terminal_cursor_style(), "bar");
    }

    #[test]
    fn ssh_keepalive_secs_defaults_to_off_and_clamps() {
        let mut store = temp_store();
        assert_eq!(store.ssh_keepalive_secs(), 0);

        store.set_ssh_keepalive_secs(30);
        assert_eq!(store.ssh_keepalive_secs(), 30);
        store.set_ssh_keepalive_secs(0);
        assert_eq!(store.ssh_keepalive_secs(), 0);
        store.set_ssh_keepalive_secs(u32::MAX);
        assert_eq!(store.ssh_keepalive_secs(), SSH_KEEPALIVE_SECS_MAX);

        store.cache = serde_json::from_str("{}").expect("legacy config must deserialize");
        assert_eq!(store.ssh_keepalive_secs(), 0);
    }

    #[test]
    fn algorithm_preferences_default_and_sanitize_on_set() {
        let mut store = temp_store();
        assert!(store.algorithm_preferences().is_builtin_default());

        let mut prefs = store.algorithm_preferences();
        prefs.cipher = vec!["nope".into(), "aes128-ctr".into()];
        store.set_algorithm_preferences(prefs);
        assert_eq!(
            store.algorithm_preferences().cipher,
            vec!["aes128-ctr".to_string()]
        );
        assert!(!store.algorithm_preferences().is_builtin_default());

        store.cache = serde_json::from_str("{}").expect("legacy config must deserialize");
        assert!(store.algorithm_preferences().is_builtin_default());
    }

    #[test]
    fn save_passwords_defaults_off_and_clear_wipes_key_paths() {
        let _guard = crate::config::vault::tests::with_test_master_key();
        let mut store = temp_store();
        assert!(!store.save_passwords());

        store.set_save_passwords(true);
        assert!(store.save_passwords());
        store.set_save_passwords(false);
        assert!(!store.save_passwords());

        let mut session = Session::default();
        session.password = Secret::new("secret");
        session.key_passphrase = Secret::new("kp");
        session.private_key = Secret::new("-----BEGIN OPENSSH PRIVATE KEY-----\n");
        store.upsert(session);

        store.clear_saved_passwords_and_keys();
        let cleared = store.sessions().first().expect("session kept");
        assert!(cleared.password.is_empty());
        assert!(cleared.key_passphrase.is_empty());
        assert!(cleared.private_key.is_empty());

        store.cache = serde_json::from_str("{}").expect("legacy config must deserialize");
        assert!(!store.save_passwords());
    }

    #[test]
    #[cfg(target_os = "windows")]
    fn renderer_mode_preserves_compatibility_default_and_validates() {
        let mut store = temp_store();
        assert_eq!(store.renderer_mode(), "software");

        store.set_renderer_mode("auto".into());
        assert_eq!(store.renderer_mode(), "auto");
        store.set_renderer_mode("gpu".into());
        assert_eq!(store.renderer_mode(), "gpu");
        store.set_renderer_mode("unexpected".into());
        assert_eq!(store.renderer_mode(), "software");

        store.cache = serde_json::from_str("{}").expect("legacy config must deserialize");
        assert_eq!(store.renderer_mode(), "software");
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn renderer_mode_preserves_linux_automatic_default_and_validates() {
        let mut store = temp_store();
        assert_eq!(store.renderer_mode(), "auto");

        store.set_renderer_mode("gpu".into());
        assert_eq!(store.renderer_mode(), "gpu");
        store.set_renderer_mode("software".into());
        assert_eq!(store.renderer_mode(), "software");
        store.set_renderer_mode("unexpected".into());
        assert_eq!(store.renderer_mode(), "auto");

        store.cache = serde_json::from_str("{}").expect("legacy config must deserialize");
        assert_eq!(store.renderer_mode(), "auto");
    }

    #[test]
    fn quick_connect_groups_default_collapsed_and_remember_expansion() {
        let mut store = temp_store();
        store.cache.groups = vec!["production".into(), "staging".into()];
        store.cache.sessions.push(Session {
            group: "production".into(),
            ..sample_session("server")
        });

        assert!(store.collapsed_session_groups().is_none());
        store.set_session_group_collapsed("production", false);

        let collapsed = store.collapsed_session_groups().unwrap();
        assert!(!collapsed.iter().any(|group| group == "production"));
        assert!(collapsed.iter().any(|group| group == "staging"));
        assert!(!collapsed.iter().any(|group| group == "system"));
        assert!(!collapsed.iter().any(|group| group == "default"));

        store.set_session_group_collapsed("production", true);
        assert!(store
            .collapsed_session_groups()
            .unwrap()
            .iter()
            .any(|group| group == "production"));
    }

    #[test]
    fn expand_collapse_all_and_children_session_groups() {
        let mut store = temp_store();
        store.cache.groups = vec![
            "prod".into(),
            "prod/web".into(),
            "prod/web/edge".into(),
            "staging".into(),
        ];

        store.set_all_session_groups_collapsed(false);
        assert!(store.collapsed_session_groups().unwrap().is_empty());

        store.set_all_session_groups_collapsed(true);
        let collapsed = store.collapsed_session_groups().unwrap();
        assert!(collapsed.iter().any(|g| g == "prod"));
        assert!(collapsed.iter().any(|g| g == "prod/web"));
        assert!(collapsed.iter().any(|g| g == "prod/web/edge"));
        assert!(collapsed.iter().any(|g| g == "staging"));

        store.set_session_group_children_collapsed("prod", false);
        let collapsed = store.collapsed_session_groups().unwrap();
        assert!(!collapsed.iter().any(|g| g == "prod"));
        assert!(!collapsed.iter().any(|g| g == "prod/web"));
        assert!(!collapsed.iter().any(|g| g == "prod/web/edge"));
        assert!(collapsed.iter().any(|g| g == "staging"));

        store.set_session_group_children_collapsed("prod", true);
        let collapsed = store.collapsed_session_groups().unwrap();
        assert!(!collapsed.iter().any(|g| g == "prod"));
        assert!(collapsed.iter().any(|g| g == "prod/web"));
        assert!(collapsed.iter().any(|g| g == "prod/web/edge"));
        assert!(collapsed.iter().any(|g| g == "staging"));
    }

    #[test]
    fn issue_316_reserved_system_groups_are_repaired_and_rejected() {
        let mut system_session = sample_session("misfiled");
        system_session.group = "system".into();
        system_session.password = Secret::default();
        let mut default_session = sample_session("legacy-default");
        default_session.group = "Default".into();
        let mut cfg = ConfigFile {
            sessions: vec![system_session, default_session],
            groups: vec![
                "system".into(),
                "System".into(),
                "default".into(),
                "prod".into(),
            ],
            collapsed_session_groups: Some(vec!["system".into(), "prod".into()]),
            ..ConfigFile::default()
        };

        assert!(normalize_reserved_session_groups(&mut cfg));
        assert_eq!(cfg.groups, ["prod"]);
        assert!(cfg.sessions.iter().all(|session| session.group.is_empty()));
        assert!(cfg.sessions[0].password.is_empty());
        // Collapse preferences for legacy reserved labels are display state and
        // may linger; normalization only repairs groups/sessions.
        assert_eq!(
            cfg.collapsed_session_groups.as_deref(),
            Some(["system".to_string(), "prod".to_string()].as_slice())
        );

        let mut store = temp_store();
        store.add_group("system".into());
        store.add_group("DEFAULT".into());
        store.add_group("prod".into());
        store.rename_group("prod", "System".into());
        assert_eq!(store.groups(), ["prod"]);

        let mut session = sample_session("server");
        session.group = "SYSTEM".into();
        let id = store.upsert(session);
        assert_eq!(store.get(&id).unwrap().group, "");
    }

    #[test]
    fn session_group_names_are_unique_case_insensitively() {
        let mut store = temp_store();
        store.add_group("Production".into());
        store.add_group("production".into());
        assert_eq!(store.groups(), ["Production"]);
        assert!(store.session_group_exists(" PRODUCTION "));

        let mut session = sample_session("staging-server");
        session.group = "Staging".into();
        store.upsert(session);
        assert!(store.session_group_exists("staging"));

        store.rename_group("Production", "STAGING".into());
        assert_eq!(store.groups(), ["Production"]);

        // Changing only the spelling/case of the same group remains valid.
        store.rename_group("Production", "production".into());
        assert_eq!(store.groups(), ["production"]);
    }

    #[test]
    fn macos_renderer_mode_defaults_to_cpu_and_preserves_gpu_choices() {
        assert_eq!(normalize_macos_renderer_mode(""), "software");
        assert_eq!(normalize_macos_renderer_mode("software"), "software");
        assert_eq!(normalize_macos_renderer_mode("femtovg"), "femtovg");
        assert_eq!(normalize_macos_renderer_mode("skia"), "skia");
        assert_eq!(normalize_macos_renderer_mode("unexpected"), "software");
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn renderer_mode_uses_macos_backends_and_validates() {
        let mut store = temp_store();
        assert_eq!(store.renderer_mode(), "software");

        store.set_renderer_mode("skia".into());
        assert_eq!(store.renderer_mode(), "skia");
        store.set_renderer_mode("femtovg".into());
        assert_eq!(store.renderer_mode(), "femtovg");
        store.set_renderer_mode("software".into());
        assert_eq!(store.renderer_mode(), "software");
        store.set_renderer_mode("unexpected".into());
        assert_eq!(store.renderer_mode(), "software");

        store.cache = serde_json::from_str("{}").expect("legacy config must deserialize");
        assert_eq!(store.renderer_mode(), "software");
    }

    #[test]
    fn terminal_cursor_color_normalizes_and_rejects_invalid_values() {
        let mut store = temp_store();
        assert_eq!(store.terminal_cursor_color(), "");

        assert!(store.set_terminal_cursor_color("#1a2B3c"));
        assert_eq!(store.terminal_cursor_color(), "#1A2B3C");
        assert!(store.set_terminal_cursor_color("abcdef"));
        assert_eq!(store.terminal_cursor_color(), "#ABCDEF");

        assert!(!store.set_terminal_cursor_color("#12345"));
        assert_eq!(store.terminal_cursor_color(), "#ABCDEF");
        assert!(!store.set_terminal_cursor_color("#GG0000"));
        assert_eq!(store.terminal_cursor_color(), "#ABCDEF");
    }

    fn sample_session(name: &str) -> Session {
        Session {
            name: name.into(),
            host: "192.168.100.2".into(),
            port: 22,
            user: "root".into(),
            ..Session::default()
        }
    }

    #[test]
    fn wallpaper_defaults_to_none_but_keeps_explicit_choice() {
        // Fresh install (no file).
        let fresh = fresh_config();
        assert_eq!(fresh.wallpaper, "");
        assert_eq!(fresh.terminal_cursor_style, "bar");
        assert!(fresh.collapse_sftp_default);
        assert!(fresh.quick_commands_as_sidebar);
        assert!(fresh.welcome_single_click_connect);
        assert!(fresh.update_check_disabled);
        assert!((fresh.wallpaper_overlay - 0.85).abs() < f32::EPSILON);
        // User upgrading from before the feature: JSON without the key.
        let cfg: ConfigFile = serde_json::from_str("{}").unwrap();
        assert_eq!(cfg.wallpaper, "builtin:tech");
        // An explicit "无"/none (stored as "") is preserved, not re-defaulted.
        let cfg: ConfigFile = serde_json::from_str(r#"{"wallpaper":""}"#).unwrap();
        assert_eq!(cfg.wallpaper, "");
        // A custom choice is preserved.
        let cfg: ConfigFile = serde_json::from_str(r#"{"wallpaper":"builtin:light"}"#).unwrap();
        assert_eq!(cfg.wallpaper, "builtin:light");

        let mut cfg = ConfigFile {
            wallpaper: "builtin:miku".to_string(),
            defaults_rev: DEFAULTS_REV,
            ..ConfigFile::default()
        };
        assert!(!migrate_defaults(&mut cfg));
        assert_eq!(cfg.wallpaper, "builtin:miku");
    }

    #[test]
    fn defaults_rev4_migrates_previous_new_user_layout() {
        let mut cfg = ConfigFile {
            wallpaper: "builtin:ms".to_string(),
            defaults_rev: 3,
            ..ConfigFile::default()
        };
        assert!(migrate_defaults(&mut cfg));
        assert_eq!(cfg.wallpaper, "");
        assert_eq!(cfg.terminal_cursor_style, "bar");
        assert!(cfg.collapse_sftp_default);
        assert!(cfg.quick_commands_as_sidebar);
        assert!(cfg.welcome_single_click_connect);
        assert!(cfg.update_check_disabled);
        assert_eq!(cfg.defaults_rev, DEFAULTS_REV);

        let mut custom = ConfigFile {
            wallpaper: "builtin:dark".to_string(),
            terminal_cursor_style: "block".to_string(),
            collapse_sftp_default: false,
            quick_commands_as_sidebar: false,
            welcome_single_click_connect: false,
            update_check_disabled: false,
            defaults_rev: 3,
            ..ConfigFile::default()
        };
        // Bool old-defaults are advanced; an explicit non-default wallpaper /
        // cursor style is preserved.
        assert!(migrate_defaults(&mut custom));
        assert_eq!(custom.wallpaper, "builtin:dark");
        assert_eq!(custom.terminal_cursor_style, "block");
        assert!(custom.collapse_sftp_default);
        assert!(custom.quick_commands_as_sidebar);
        assert!(custom.welcome_single_click_connect);
        assert!(custom.update_check_disabled);
    }

    #[test]
    fn restore_settings_defaults_keeps_sessions_commands_and_secrets() {
        let _guard = crate::config::vault::tests::with_test_master_key();
        let mut store = temp_store();
        store.set_save_passwords(true);
        let mut session = sample_session("keep-me");
        session.password = Secret::new("secret");
        store.upsert(session);
        store.cache.groups = vec!["lab".into()];
        store.set_quick_commands(vec![crate::config::QuickCommand {
            name: "ll".into(),
            command: "ls -la".into(),
            group: String::new(),
            send_enter: true,
        }]);
        store.add_quick_group("ops".into());
        store.cache.command_history = vec!["echo hi".into()];
        store.set_font_size(20);
        store.set_wallpaper("builtin:dark");
        store.set_zen_mode(true);
        store.set_update_check_enabled(true);
        store.set_welcome_sidebar_width(520.0);
        store.set_download_dir("/tmp/custom-downloads".into());

        store.restore_settings_defaults();

        assert_eq!(store.sessions().len(), 1);
        assert_eq!(store.sessions()[0].name, "keep-me");
        assert_eq!(store.sessions()[0].password.as_str(), "secret");
        assert_eq!(store.groups(), &["lab".to_string()]);
        assert_eq!(store.quick_commands().len(), 1);
        assert_eq!(store.quick_commands()[0].name, "ll");
        assert!(store.quick_groups().iter().any(|g| g == "ops"));
        assert_eq!(store.cache.command_history, vec!["echo hi".to_string()]);
        assert_eq!(store.font_size(), 13);
        assert_eq!(store.wallpaper(), "");
        assert!(!store.zen_mode());
        assert!(!store.update_check_enabled());
        assert_eq!(store.terminal_cursor_style(), "bar");
        assert_eq!(store.language(), "auto");
        assert!(store.collapse_sftp_default());
        assert!(store.quick_commands_as_sidebar());
        assert!(store.welcome_single_click_connect());
        assert!(!store.save_passwords());
        assert_eq!(store.welcome_sidebar_width(), 350.0);
        let expected_dl = directories::UserDirs::new()
            .and_then(|u| u.download_dir().map(|p| p.to_string_lossy().to_string()))
            .unwrap_or_default();
        assert_eq!(store.download_dir(), expected_dl);
    }

    #[test]
    fn wallpaper_transparency_default_migrates_without_overwriting_custom_value() {
        let mut old_default = ConfigFile {
            wallpaper_overlay: PREVIOUS_DEFAULT_WALLPAPER_OVERLAY,
            defaults_rev: 2,
            ..ConfigFile::default()
        };
        assert!(migrate_defaults(&mut old_default));
        assert!((old_default.wallpaper_overlay - 0.85).abs() < f32::EPSILON);

        let mut custom = ConfigFile {
            wallpaper_overlay: 0.70,
            defaults_rev: 2,
            ..ConfigFile::default()
        };
        assert!(migrate_defaults(&mut custom));
        assert!((custom.wallpaper_overlay - 0.70).abs() < f32::EPSILON);
    }

    #[test]
    fn output_highlight_defaults_and_preset_validation() {
        let fresh = fresh_config();
        assert_eq!(fresh.output_highlight_rules.len(), 4);
        assert_eq!(fresh.output_highlight_rules[0].name, "error");
        assert_eq!(fresh.output_highlight_rules[1].name, "success");
        assert_eq!(fresh.output_highlight_rules[2].name, "warning");
        assert_eq!(fresh.output_highlight_rules[3].name, "IP");
        assert!(fresh.output_highlight_rules.iter().all(|r| r.regex && r.enabled));

        let mut store = temp_store();
        assert!(store.output_highlight_enabled());
        assert!(store.json_format_output());
        assert_eq!(store.output_highlight_preset(), "log");

        store.set_output_highlight_enabled(false);
        store.set_output_highlight_preset("devops".to_string());
        assert!(!store.output_highlight_enabled());
        store.set_json_format_output(false);
        assert!(!store.json_format_output());
        assert_eq!(store.output_highlight_preset(), "devops");

        store.set_output_highlight_preset("future-preset".to_string());
        assert_eq!(store.output_highlight_preset(), "log");

        store.add_output_highlight_rule(OutputHighlightRule {
            name: "  timeout  ".to_string(),
            pattern: "  connection refused  ".to_string(),
            regex: false,
            case_sensitive: false,
            whole_line: true,
            color: "unknown".to_string(),
            enabled: true,
        });
        assert_eq!(store.output_highlight_rules().len(), 1);
        assert_eq!(store.output_highlight_rules()[0].name, "timeout");
        assert_eq!(
            store.output_highlight_rules()[0].pattern,
            "connection refused"
        );
        assert_eq!(store.output_highlight_rules()[0].color, "#F14C4C");
        store.update_output_highlight_rule(
            0,
            OutputHighlightRule {
                name: "refused".to_string(),
                pattern: "refused".to_string(),
                regex: false,
                case_sensitive: true,
                whole_line: false,
                color: "#29B8DB".to_string(),
                enabled: false, // ignored; enabled kept from existing slot
            },
        );
        assert_eq!(store.output_highlight_rules()[0].name, "refused");
        assert_eq!(store.output_highlight_rules()[0].pattern, "refused");
        assert_eq!(store.output_highlight_rules()[0].color, "#29B8DB");
        assert!(store.output_highlight_rules()[0].enabled);
        assert!(store.output_highlight_rules()[0].case_sensitive);
        store.set_output_highlight_rule_enabled(0, false);
        assert!(!store.output_highlight_rules()[0].enabled);
        store.remove_output_highlight_rule(0);
        assert!(store.output_highlight_rules().is_empty());

        // An older settings file without either field retains the feature that
        // shipped in the previous version: enabled with the log preset.
        let legacy: ConfigFile = serde_json::from_str("{}").unwrap();
        store.cache = legacy;
        assert!(store.output_highlight_enabled());
        assert_eq!(store.output_highlight_preset(), "log");
    }

    #[test]
    fn defaults_rev5_seeds_empty_highlight_rules() {
        let mut empty = ConfigFile {
            defaults_rev: 4,
            ..ConfigFile::default()
        };
        assert!(empty.output_highlight_rules.is_empty());
        assert!(migrate_defaults(&mut empty));
        assert_eq!(empty.output_highlight_rules.len(), 4);
        assert_eq!(empty.output_highlight_rules[0].name, "error");
        assert_eq!(empty.defaults_rev, DEFAULTS_REV);

        let mut custom = ConfigFile {
            output_highlight_rules: vec![OutputHighlightRule {
                name: "mine".into(),
                pattern: "mine".into(),
                regex: false,
                case_sensitive: false,
                whole_line: false,
                color: "#ABCDEF".into(),
                enabled: true,
            }],
            defaults_rev: 4,
            ..ConfigFile::default()
        };
        assert!(migrate_defaults(&mut custom));
        assert_eq!(custom.output_highlight_rules.len(), 1);
        assert_eq!(custom.output_highlight_rules[0].name, "mine");
        assert_eq!(custom.defaults_rev, DEFAULTS_REV);
    }

    #[test]
    fn split_files_roundtrip_keeps_sessions_and_settings() {
        let mut store = temp_store();
        store.cache.sessions.push(sample_session("alpha"));
        store.cache.groups = vec!["prod".into()];
        store.set_theme_pref("dark".into());
        store.set_session_group_collapsed("prod", false);
        store.save().unwrap();

        let dir = store.path.parent().unwrap().to_path_buf();
        assert!(dir.join("settings.json").exists());
        assert!(dir.join("ui-state.json").exists());
        let sessions_raw = std::fs::read_to_string(&store.path).unwrap();
        assert!(sessions_raw.contains("alpha"));
        assert!(!sessions_raw.contains("\"theme_pref\""));
        let settings_raw = std::fs::read_to_string(dir.join("settings.json")).unwrap();
        assert!(settings_raw.contains("dark"));

        // Simulate split load into a fresh store path under the same dir.
        let reloaded = {
            let mut s = ConfigStore {
                path: store.path.clone(),
                cache: ConfigFile::default(),
            };
            // Re-read using the same helpers as load().
            let settings_path = crate::config::persist::settings_path(&dir);
            let ui_path = crate::config::persist::ui_state_path(&dir);
            s.cache = ConfigStore::load_split(&store.path, &settings_path, &ui_path).unwrap();
            s
        };
        assert_eq!(reloaded.sessions().len(), 1);
        assert_eq!(reloaded.theme_pref(), "dark");
        let collapsed = reloaded.collapsed_session_groups().unwrap();
        assert!(!collapsed.iter().any(|g| g == "prod"));

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn saved_password_goes_to_vault_not_sessions_json() {
        let _guard = crate::config::vault::tests::with_test_master_key();
        let mut store = temp_store();
        store.set_save_passwords(true);
        assert!(store.save_passwords());
        let password = "p@ss word!^&*中文";
        let id = store.upsert(Session {
            name: "windows-password".into(),
            host: "192.168.100.2".into(),
            port: 22,
            user: "root".into(),
            password: Secret::new(password),
            ..Session::default()
        });

        store.save().unwrap();
        let raw = std::fs::read_to_string(&store.path).unwrap();
        assert!(!raw.contains(password));
        // `save_passwords` may appear; session secret fields must not.
        assert!(!raw.contains("\"password\":"));
        assert!(!raw.contains("\"private_key\""));
        assert!(!raw.contains("\"key_passphrase\""));

        let dir = store.path.parent().unwrap();
        let vault_raw =
            std::fs::read_to_string(dir.join(crate::config::vault::VAULT_FILE)).unwrap();
        assert!(!vault_raw.contains(password));
        let loaded = crate::config::vault::get_secrets(dir, &id)
            .unwrap()
            .expect("vault entry");
        assert_eq!(loaded.password, password);

        let _ = std::fs::remove_file(&store.path);
        let _ = std::fs::remove_file(dir.join(crate::config::vault::VAULT_FILE));
    }

    #[test]
    fn save_passwords_switch_gates_key_material_vault_write() {
        let _guard = crate::config::vault::tests::with_test_master_key();
        let dir_cleanup;
        {
            let mut store = temp_store();
            assert!(!store.save_passwords());
            let key_body = "-----BEGIN OPENSSH PRIVATE KEY-----\nAAAA\n-----END OPENSSH PRIVATE KEY-----\n";
            let id = store.upsert(Session {
                name: "key-prompt".into(),
                host: "10.0.0.8".into(),
                user: "ubuntu".into(),
                auth: AuthMethod::Key,
                private_key: Secret::new(key_body),
                key_passphrase: Secret::new("kp-secret"),
                ..Session::default()
            });
            // Switch off: in-memory secrets must not be pushed to the vault
            // (same rule as the welcome-page credential dialog).
            store.save().unwrap();
            let dir = store.path.parent().unwrap().to_path_buf();
            dir_cleanup = dir.clone();
            assert!(
                crate::config::vault::get_secrets(&dir, &id)
                    .unwrap()
                    .is_none(),
                "vault must stay empty while save-passwords is off"
            );

            store.set_save_passwords(true);
            store.save().unwrap();
            let loaded = crate::config::vault::get_secrets(&dir, &id)
                .unwrap()
                .expect("vault entry after enabling save-passwords");
            assert!(loaded.password.is_empty());
            assert_eq!(loaded.private_key, key_body);
            assert_eq!(loaded.passphrase, "kp-secret");

            let _ = std::fs::remove_file(&store.path);
        }
        let _ = std::fs::remove_file(dir_cleanup.join(crate::config::vault::VAULT_FILE));
    }

    #[test]
    fn export_omits_password_and_key_fields() {
        let mut a = temp_store();
        let id = "saved-1700000000000-ab12".to_string();
        let saved_at = 1_700_000_000_000u64;
        a.cache.sessions.push(Session {
            id: id.clone(),
            saved_at,
            name: "pve".into(),
            host: "192.168.100.2".into(),
            port: 22,
            user: "root".into(),
            auth: AuthMethod::Key,
            password: Secret::new("s3cr3t"),
            key_passphrase: Secret::new("kp"),
            private_key: Secret::new("-----BEGIN OPENSSH PRIVATE KEY-----\nAAAA\n-----END OPENSSH PRIVATE KEY-----\n"),
            ..Session::default()
        });

        let export_path = std::env::temp_dir().join(format!("ms-exp-{}.json", Uuid::new_v4()));
        assert_eq!(a.export_to(&export_path).unwrap(), 1);

        let raw = std::fs::read_to_string(&export_path).unwrap();
        assert!(raw.contains("\"zinterm_export\": \"sessions\""));
        assert!(raw.contains("\"version\": 1"));
        assert!(raw.contains("\"empty_groups\""));
        assert!(raw.contains("\"sessions\""));
        assert!(raw.contains("192.168.100.2"));
        assert!(!raw.contains("s3cr3t"));
        assert!(!raw.contains("\"password\""));
        assert!(!raw.contains("key_passphrase"));
        assert!(!raw.contains("private_key"));
        assert!(!raw.contains(ConfigStore::EXPORT_PREFIX));

        // Round-trip restores connection metadata without secrets.
        let mut b = temp_store();
        assert_eq!(b.import_from(&export_path).unwrap(), (1, 0));
        assert_eq!(b.cache.sessions.len(), 1);
        assert!(b.cache.sessions[0].password.is_empty());
        assert!(b.cache.sessions[0].key_passphrase.is_empty());
        assert!(b.cache.sessions[0].private_key.is_empty());
        assert_eq!(b.cache.sessions[0].host, "192.168.100.2");
        assert_eq!(b.cache.sessions[0].id, id);
        assert_eq!(b.cache.sessions[0].saved_at, saved_at);

        assert_eq!(b.import_from(&export_path).unwrap(), (0, 1));

        let _ = std::fs::remove_file(&export_path);
        let _ = std::fs::remove_file(&a.path);
        let _ = std::fs::remove_file(&b.path);
    }

    #[test]
    fn import_plaintext_secrets_honor_save_passwords_switch() {
        let _guard = crate::config::vault::tests::with_test_master_key();
        let wrap = |sessions: &str| {
            format!(
                r#"{{"zinterm_export":"sessions","version":1,"exported_at":"t","empty_groups":[],"sessions":[{sessions}]}}"#
            )
        };
        let body = r#"{
            "kind":"ssh",
            "name":"box",
            "host":"10.0.0.9",
            "user":"root",
            "auth":"password",
            "password":"p@ss"
        }"#;

        let mut off = temp_store();
        assert!(!off.save_passwords());
        assert_eq!(off.import_json(&wrap(body)).unwrap(), (1, 0));
        assert!(off.cache.sessions[0].password.is_empty());

        let mut on = temp_store();
        on.set_save_passwords(true);
        assert_eq!(on.import_json(&wrap(body)).unwrap(), (1, 0));
        assert_eq!(on.cache.sessions[0].password.as_str(), "p@ss");

        let key_body = r#"{
            "kind":"ssh",
            "name":"keybox",
            "host":"10.0.0.10",
            "user":"root",
            "auth":"key",
            "key_passphrase":"key-pass",
            "private_key":"-----BEGIN OPENSSH PRIVATE KEY-----\\nAAAA\\n-----END OPENSSH PRIVATE KEY-----"
        }"#;
        let mut key_on = temp_store();
        key_on.set_save_passwords(true);
        assert_eq!(key_on.import_json(&wrap(key_body)).unwrap(), (1, 0));
        let s = &key_on.cache.sessions[0];
        assert!(s.password.is_empty());
        assert_eq!(s.key_passphrase.as_str(), "key-pass");
        assert_eq!(
            s.private_key.as_str(),
            "-----BEGIN OPENSSH PRIVATE KEY-----\nAAAA\n-----END OPENSSH PRIVATE KEY-----"
        );

        let mut key_off = temp_store();
        assert_eq!(key_off.import_json(&wrap(key_body)).unwrap(), (1, 0));
        assert!(key_off.cache.sessions[0].password.is_empty());
        assert!(key_off.cache.sessions[0].key_passphrase.is_empty());
        assert!(key_off.cache.sessions[0].private_key.is_empty());

        let _ = std::fs::remove_file(&off.path);
        let _ = std::fs::remove_file(&on.path);
        let _ = std::fs::remove_file(&key_on.path);
        let _ = std::fs::remove_file(&key_off.path);
    }

    #[test]
    fn import_accepts_legacy_export_encrypted_password_when_save_passwords_on() {
        let _guard = crate::config::vault::tests::with_test_master_key();
        let mut store = temp_store();
        store.set_save_passwords(true);
        let enc = encrypt_export("legacy-secret").unwrap();
        let raw = format!(
            r#"{{"zinterm_export":"sessions","version":1,"exported_at":"t","empty_groups":[],"sessions":[{{"kind":"ssh","name":"legacy","host":"1.2.3.4","password":"{enc}"}}]}}"#
        );
        assert_eq!(store.import_json(&raw).unwrap(), (1, 0));
        assert_eq!(store.cache.sessions[0].password.as_str(), "legacy-secret");
        let _ = std::fs::remove_file(&store.path);
    }

    #[test]
    fn import_skips_same_group_name_but_allows_other_group() {
        let mut store = temp_store();
        store.cache.sessions.push(Session {
            id: "saved-1700000000001-aaaa".into(),
            saved_at: 1,
            name: "box".into(),
            host: "1.1.1.1".into(),
            group: "prod".into(),
            ..Session::default()
        });

        let raw = serde_json::to_string_pretty(&ExportFile {
            zinterm_export: "sessions".into(),
            version: 1,
            exported_at: "test".into(),
            empty_groups: vec![],
            sessions: vec![
                Session {
                    id: "saved-1700000000002-bbbb".into(),
                    saved_at: 2,
                    name: "box".into(),
                    host: "2.2.2.2".into(),
                    group: "prod".into(),
                    ..Session::default()
                },
                Session {
                    id: "saved-1700000000003-cccc".into(),
                    saved_at: 3,
                    name: "box".into(),
                    host: "3.3.3.3".into(),
                    group: "lab".into(),
                    ..Session::default()
                },
            ],
        })
        .unwrap();

        assert_eq!(store.import_json(&raw).unwrap(), (1, 1));
        assert_eq!(store.sessions().len(), 2);
        assert!(store.sessions().iter().any(|s| s.group == "lab" && s.id == "saved-1700000000003-cccc"));
        assert!(!store.sessions().iter().any(|s| s.id == "saved-1700000000002-bbbb"));
    }

    #[test]
    fn import_requires_kind_and_kind_specific_endpoints() {
        let mut store = temp_store();
        let wrap = |sessions: &str| {
            format!(
                r#"{{"zinterm_export":"sessions","version":1,"exported_at":"t","empty_groups":[],"sessions":[{sessions}]}}"#
            )
        };

        let err = format!(
            "{:#}",
            store
                .import_json(&wrap(r#"{"name":"a","host":"1.1.1.1"}"#))
                .unwrap_err()
        );
        assert!(err.contains("kind is required"), "{err}");

        let err = format!(
            "{:#}",
            store
                .import_json(&wrap(r#"{"kind":"ssh","name":"a"}"#))
                .unwrap_err()
        );
        assert!(err.contains("host is required"), "{err}");

        let err = format!(
            "{:#}",
            store
                .import_json(&wrap(r#"{"kind":"telnet","name":"a","host":"  "}"#))
                .unwrap_err()
        );
        assert!(err.contains("host is required"), "{err}");

        let err = format!(
            "{:#}",
            store
                .import_json(&wrap(r#"{"kind":"serial","name":"a"}"#))
                .unwrap_err()
        );
        assert!(err.contains("serial_port is required"), "{err}");

        let err = format!(
            "{:#}",
            store
                .import_json(&wrap(r#"{"kind":"ftp","name":"a","host":"1.1.1.1"}"#))
                .unwrap_err()
        );
        assert!(err.contains("unsupported kind"), "{err}");
    }

    #[test]
    fn import_applies_new_session_defaults_for_optional_fields() {
        let mut store = temp_store();
        let raw = r#"{
            "zinterm_export": "sessions",
            "version": 1,
            "exported_at": "t",
            "empty_groups": [],
            "sessions": [
                {
                    "kind": "ssh",
                    "host": "10.0.0.1",
                    "port": "nope",
                    "parity": "weird",
                    "flow_control": "bogus",
                    "backspace_mode": "??",
                    "enable_sftp": "maybe"
                },
                {
                    "kind": "telnet",
                    "host": "10.0.0.2"
                },
                {
                    "kind": "serial",
                    "serial_port": "COM3",
                    "baud_rate": 0,
                    "data_bits": "x",
                    "stop_bits": -1
                },
                {
                    "kind": "local"
                }
            ]
        }"#;
        assert_eq!(store.import_json(raw).unwrap(), (4, 0));
        let sessions = store.sessions();

        let ssh = sessions.iter().find(|s| s.host == "10.0.0.1").unwrap();
        assert_eq!(ssh.kind, SessionKind::Ssh);
        assert_eq!(ssh.port, 22);
        assert_eq!(ssh.user, "");
        assert_eq!(ssh.name, "10.0.0.1");
        assert_eq!(ssh.auth, AuthMethod::Password);
        assert_eq!(ssh.parity, "none");
        assert_eq!(ssh.flow_control, "none");
        assert_eq!(ssh.backspace_mode, "auto");
        assert_eq!(ssh.encoding, "UTF-8");
        assert!(!ssh.enable_sftp);
        assert!(!ssh.enable_command_panel);

        let telnet = sessions.iter().find(|s| s.host == "10.0.0.2").unwrap();
        assert_eq!(telnet.kind, SessionKind::Telnet);
        assert_eq!(telnet.port, 23);
        assert_eq!(telnet.name, "10.0.0.2");

        let serial = sessions.iter().find(|s| s.serial_port == "COM3").unwrap();
        assert_eq!(serial.kind, SessionKind::Serial);
        assert_eq!(serial.baud_rate, 9_600);
        assert_eq!(serial.data_bits, 8);
        assert_eq!(serial.stop_bits, 1);
        assert_eq!(serial.name, "COM3 @9600");

        let local = sessions.iter().find(|s| s.kind == SessionKind::Local).unwrap();
        assert_eq!(local.name, "Local");
        assert!(local.host.is_empty());
    }

    #[test]
    fn export_import_restores_empty_groups_before_sessions() {
        let mut a = temp_store();
        a.add_group("empty-lab".into());
        a.add_group("has-session".into());
        a.cache.sessions.push(Session {
            name: "box".into(),
            host: "10.0.0.1".into(),
            group: "has-session".into(),
            ..Session::default()
        });

        let export_path = std::env::temp_dir().join(format!("ms-exp-groups-{}.json", Uuid::new_v4()));
        assert_eq!(a.export_to(&export_path).unwrap(), 1);
        let raw = std::fs::read_to_string(&export_path).unwrap();
        assert!(raw.contains("\"empty_groups\": [\n    \"empty-lab\"\n  ]") || raw.contains("\"empty-lab\""));
        assert!(!raw.contains("\"has-session\"") || {
            // has-session may appear on the session's group field, but not in empty_groups.
            let file: ExportFile = serde_json::from_str(&raw).unwrap();
            !file.empty_groups.iter().any(|g| g == "has-session")
                && file.empty_groups.iter().any(|g| g == "empty-lab")
        });

        let mut b = temp_store();
        assert_eq!(b.import_from(&export_path).unwrap(), (1, 0));
        assert!(b.session_group_exists("empty-lab"));
        assert!(b.sessions().iter().any(|s| s.group == "has-session"));

        let _ = std::fs::remove_file(&export_path);
        let _ = std::fs::remove_file(&a.path);
        let _ = std::fs::remove_file(&b.path);
    }

    #[test]
    fn issue_300_interface_defaults_and_ranges_are_safe() {
        let mut store = temp_store();

        // Legacy configs keep the safe confirmation and familiar paste aliases.
        store.cache = serde_json::from_str("{}").unwrap();
        assert!(store.paste_confirm_enabled());
        assert!(store.extra_paste_shortcuts_enabled());
        assert!(store.select_copy_right_paste_enabled());
        assert!(!store.zen_mode());
        assert!(store.confirm_delete_group());
        assert!(!store.confirm_delete_session());
        assert!(!store.welcome_single_click_connect());
        assert_eq!(store.terminal_line_spacing(), 1.0);

        store.set_terminal_line_spacing(0.1);
        assert_eq!(store.terminal_line_spacing(), 0.8);
        store.set_terminal_line_spacing(9.0);
        assert_eq!(store.terminal_line_spacing(), 1.5);

        store.set_paste_confirm_enabled(false);
        store.set_extra_paste_shortcuts_enabled(false);
        store.set_select_copy_right_paste_enabled(false);
        store.set_zen_mode(true);
        store.set_confirm_delete_group(false);
        store.set_confirm_delete_session(true);
        store.set_welcome_single_click_connect(true);
        assert!(!store.paste_confirm_enabled());
        assert!(!store.extra_paste_shortcuts_enabled());
        assert!(!store.select_copy_right_paste_enabled());
        assert!(store.zen_mode());
        assert!(!store.confirm_delete_group());
        assert!(store.confirm_delete_session());
        assert!(store.welcome_single_click_connect());
    }

    #[test]
    fn quick_groups_keep_user_order_and_reorder() {
        let mut store = temp_store();
        store.add_quick_group("beta".into());
        store.add_quick_group("alpha".into());
        store.set_quick_commands(vec![crate::config::QuickCommand {
            name: "x".into(),
            command: "x".into(),
            group: "gamma".into(),
            send_enter: true,
        }]);
        assert_eq!(
            store.materialized_quick_groups(),
            vec!["beta", "alpha", "gamma"]
        );
        assert!(store.reorder_quick_group("gamma", "beta"));
        assert_eq!(
            store.materialized_quick_groups(),
            vec!["gamma", "beta", "alpha"]
        );
        assert!(store.reorder_quick_group("beta", ""));
        assert_eq!(
            store.materialized_quick_groups(),
            vec!["gamma", "alpha", "beta"]
        );
    }

    #[test]
    fn session_name_disambiguates_within_group_only() {
        let mut store = temp_store();
        let mut a = sample_session("web");
        a.id = "1".into();
        a.group = "prod".into();
        store.upsert(a);
        let mut b = sample_session("web");
        b.id = "2".into();
        b.group = "dev".into();
        store.upsert(b);

        assert_eq!(
            store.disambiguate_session_name("prod", "web", None),
            "web（1）"
        );
        assert_eq!(
            store.disambiguate_session_name("dev", "web", Some("2")),
            "web"
        );
        assert_eq!(
            store.disambiguate_session_name("other", "web", None),
            "web"
        );

        let mut c = sample_session("web");
        c.id = "3".into();
        c.group = "prod".into();
        store.upsert(c);
        assert_eq!(store.get("3").unwrap().name, "web（1）");

        let mut d = sample_session("web");
        d.id = "4".into();
        d.group = "prod".into();
        store.upsert(d);
        assert_eq!(store.get("4").unwrap().name, "web（2）");
    }

    #[test]
    fn move_session_and_group_disambiguate_on_conflict() {
        let mut store = temp_store();
        store.add_group("prod".into());
        store.add_group("dev".into());
        store.add_group("dev/web".into());

        let mut a = sample_session("api");
        a.id = "s1".into();
        a.group = "prod".into();
        store.upsert(a);
        let mut b = sample_session("api");
        b.id = "s2".into();
        b.group = "dev".into();
        store.upsert(b);

        assert!(store.move_session_to_group("s1", "dev"));
        assert_eq!(store.get("s1").unwrap().group, "dev");
        assert_eq!(store.get("s1").unwrap().name, "api（1）");

        store.add_group("web".into());
        assert!(store.move_group_to_parent("web", "dev"));
        assert!(store.session_group_exists("dev/web（1）"));
        assert!(!store.session_group_exists("web"));

        // Cannot move a group into itself or a descendant.
        assert!(!store.move_group_to_parent("dev", "dev/web"));
        assert!(!store.move_group_to_parent("dev", "dev"));
    }
}
