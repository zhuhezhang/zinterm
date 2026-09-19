use super::super::structs::*;

impl ConfigStore {
    pub fn download_dir(&self) -> &str {
        &self.cache.download_dir
    }

    pub fn set_download_dir(&mut self, dir: String) {
        self.cache.download_dir = dir;
    }

    /// Whether the SFTP panel follows the terminal's cd (default true).
    pub fn sftp_follow_cd(&self) -> bool {
        !self.cache.sftp_no_follow_cd
    }

    pub fn set_sftp_follow_cd(&mut self, follow: bool) {
        self.cache.sftp_no_follow_cd = !follow;
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
}
