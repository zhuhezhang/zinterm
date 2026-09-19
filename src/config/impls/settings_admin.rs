use directories::UserDirs;

use super::super::structs::*;
use super::defaults_migration::*;

impl ConfigStore {
    /// Reset Interface / appearance preferences to the current new-user defaults
    /// while keeping sessions, groups, quick commands, command history, and
    /// saved credentials intact. Does not touch known-hosts (separate file).
    /// Also resets layout chrome that lives in Settings (welcome sidebar width)
    /// and the SFTP preset download directory (system Downloads when available).
    pub fn restore_settings_defaults(&mut self) {
        let sessions = std::mem::take(&mut self.cache.sessions);
        let groups = std::mem::take(&mut self.cache.empty_groups);
        let collapsed_session_groups = self.cache.collapsed_session_groups.take();
        let quick_commands = std::mem::take(&mut self.cache.quick_commands);
        let quick_groups = std::mem::take(&mut self.cache.quick_empty_groups);
        let command_history = std::mem::take(&mut self.cache.command_history);

        self.cache = fresh_config();
        self.cache.sessions = sessions;
        self.cache.empty_groups = groups;
        self.cache.collapsed_session_groups = collapsed_session_groups;
        self.cache.quick_commands = quick_commands;
        self.cache.quick_empty_groups = quick_groups;
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
}

#[cfg(test)]
#[allow(unused_imports)]
mod tests {
    use super::super::super::structs::*;
    use super::super::defaults_migration::{
        fresh_config, migrate_defaults, normalize_macos_renderer_mode,
        normalize_reserved_session_groups,
    };
    use super::super::test_support::{encrypt_export, sample_session, temp_store};

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

        let session = Session {
            password: Secret::new("secret"),
            key_passphrase: Secret::new("kp"),
            private_key: Secret::new("-----BEGIN OPENSSH PRIVATE KEY-----\n"),
            ..Default::default()
        };
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
}
