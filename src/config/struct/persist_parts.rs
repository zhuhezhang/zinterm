//! On-disk split of [`super::ConfigFile`]: sessions, commands, settings, and UI chrome.
//!
//! In memory we still keep one [`ConfigFile`]. On disk the pieces live in
//! sibling JSON files so a folder toggle does not rewrite hundreds of sessions.

use serde::{Deserialize, Serialize};

use super::{
    default_quick_panel_height, default_quick_panel_width, default_sftp_height,
    default_sftp_tree_width, default_sftp_width, default_wallpaper,
    default_welcome_session_col_host, default_welcome_session_col_name, AlgorithmPreferences,
    ConfigFile, OutputHighlightRule, QuickCommand, Session,
};

/// Which sibling files (and optional vault) a persist pass should touch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SaveKind(u8);

impl SaveKind {
    pub const SESSIONS: Self = Self(1);
    pub const SETTINGS: Self = Self(2);
    pub const UI: Self = Self(4);
    pub const VAULT: Self = Self(8);
    pub const COMMANDS: Self = Self(16);
    pub const ALL: Self = Self(1 | 2 | 4 | 8 | 16);

    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Sessions tree + vault (credentials follow session edits).
    pub const fn sessions_and_vault() -> Self {
        Self(Self::SESSIONS.0 | Self::VAULT.0)
    }

    /// Settings panel prefs; vault when "save passwords" may need a full sync.
    pub const fn settings_and_vault() -> Self {
        Self(Self::SETTINGS.0 | Self::VAULT.0)
    }
}

impl std::ops::BitOr for SaveKind {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        self.union(rhs)
    }
}

impl std::ops::BitOrAssign for SaveKind {
    fn bitor_assign(&mut self, rhs: Self) {
        *self = self.union(rhs);
    }
}

/// `sessions.json` — saved connections and empty session groups.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SessionsFile {
    #[serde(default)]
    pub sessions: Vec<Session>,
    #[serde(default)]
    pub empty_groups: Vec<String>,
}

impl SessionsFile {
    pub fn from_config(cfg: &ConfigFile) -> Self {
        Self {
            sessions: cfg.sessions.clone(),
            empty_groups: cfg.empty_groups.clone(),
        }
    }

    pub fn apply_to(&self, cfg: &mut ConfigFile) {
        cfg.sessions = self.sessions.clone();
        cfg.empty_groups = self.empty_groups.clone();
    }
}

/// `commands.json` — quick commands and terminal command history.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CommandsFile {
    #[serde(default)]
    pub quick_commands: Vec<QuickCommand>,
    #[serde(default)]
    pub quick_empty_groups: Vec<String>,
    #[serde(default)]
    pub command_history: Vec<String>,
}

impl CommandsFile {
    pub fn from_config(cfg: &ConfigFile) -> Self {
        Self {
            quick_commands: cfg.quick_commands.clone(),
            quick_empty_groups: cfg.quick_empty_groups.clone(),
            command_history: cfg.command_history.clone(),
        }
    }

    pub fn apply_to(&self, cfg: &mut ConfigFile) {
        cfg.quick_commands = self.quick_commands.clone();
        cfg.quick_empty_groups = self.quick_empty_groups.clone();
        cfg.command_history = self.command_history.clone();
    }
}

/// `settings.json` — Settings-panel preferences.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SettingsFile {
    #[serde(default)]
    pub download_dir: String,
    #[serde(default)]
    pub language: String,
    #[serde(default)]
    pub theme_pref: String,
    #[serde(default)]
    pub renderer_mode: String,
    #[serde(default)]
    pub font_family: String,
    #[serde(default)]
    pub font_size: u32,
    #[serde(default)]
    pub terminal_line_spacing: f32,
    #[serde(default)]
    pub terminal_bold: bool,
    #[serde(default)]
    pub terminal_cursor_style: String,
    #[serde(default)]
    pub terminal_cursor_color: String,
    #[serde(default)]
    pub output_highlight_disabled: bool,
    #[serde(default)]
    pub output_highlight_preset: String,
    #[serde(default)]
    pub output_highlight_rules: Vec<OutputHighlightRule>,
    #[serde(default)]
    pub json_format_disabled: bool,
    #[serde(default)]
    pub ui_scale: u32,
    #[serde(default = "default_wallpaper")]
    pub wallpaper: String,
    #[serde(default)]
    pub sftp_no_follow_cd: bool,
    #[serde(default)]
    pub download_always_ask: bool,
    #[serde(default)]
    pub paste_confirm_disabled: bool,
    #[serde(default)]
    pub extra_paste_shortcuts_disabled: bool,
    #[serde(default)]
    pub select_copy_right_paste_disabled: bool,
    #[serde(default)]
    pub zen_mode: bool,
    #[serde(default)]
    pub quick_commands_as_sidebar: bool,
    #[serde(default)]
    pub collapse_sftp_default: bool,
    #[serde(default)]
    pub welcome_as_sidebar: bool,
    #[serde(default)]
    pub confirm_delete_group_disabled: bool,
    #[serde(default)]
    pub confirm_delete_session: bool,
    #[serde(default)]
    pub welcome_single_click_connect: bool,
    #[serde(default)]
    pub wallpaper_overlay: f32,
    #[serde(default)]
    pub panel_font: u32,
    #[serde(default)]
    pub update_check_disabled: bool,
    #[serde(default)]
    pub ssh_keepalive_secs: u32,
    #[serde(default)]
    pub algorithm_preferences: AlgorithmPreferences,
    #[serde(default)]
    pub save_passwords: bool,
    #[serde(default)]
    pub defaults_rev: u32,
}

impl Default for SettingsFile {
    fn default() -> Self {
        Self::from_config(&ConfigFile::default())
    }
}

impl SettingsFile {
    pub fn from_config(cfg: &ConfigFile) -> Self {
        Self {
            download_dir: cfg.download_dir.clone(),
            language: cfg.language.clone(),
            theme_pref: cfg.theme_pref.clone(),
            renderer_mode: cfg.renderer_mode.clone(),
            font_family: cfg.font_family.clone(),
            font_size: cfg.font_size,
            terminal_line_spacing: cfg.terminal_line_spacing,
            terminal_bold: cfg.terminal_bold,
            terminal_cursor_style: cfg.terminal_cursor_style.clone(),
            terminal_cursor_color: cfg.terminal_cursor_color.clone(),
            output_highlight_disabled: cfg.output_highlight_disabled,
            output_highlight_preset: cfg.output_highlight_preset.clone(),
            output_highlight_rules: cfg.output_highlight_rules.clone(),
            json_format_disabled: cfg.json_format_disabled,
            ui_scale: cfg.ui_scale,
            wallpaper: cfg.wallpaper.clone(),
            sftp_no_follow_cd: cfg.sftp_no_follow_cd,
            download_always_ask: cfg.download_always_ask,
            paste_confirm_disabled: cfg.paste_confirm_disabled,
            extra_paste_shortcuts_disabled: cfg.extra_paste_shortcuts_disabled,
            select_copy_right_paste_disabled: cfg.select_copy_right_paste_disabled,
            zen_mode: cfg.zen_mode,
            quick_commands_as_sidebar: cfg.quick_commands_as_sidebar,
            collapse_sftp_default: cfg.collapse_sftp_default,
            welcome_as_sidebar: cfg.welcome_as_sidebar,
            confirm_delete_group_disabled: cfg.confirm_delete_group_disabled,
            confirm_delete_session: cfg.confirm_delete_session,
            welcome_single_click_connect: cfg.welcome_single_click_connect,
            wallpaper_overlay: cfg.wallpaper_overlay,
            panel_font: cfg.panel_font,
            update_check_disabled: cfg.update_check_disabled,
            ssh_keepalive_secs: cfg.ssh_keepalive_secs,
            algorithm_preferences: cfg.algorithm_preferences.clone(),
            save_passwords: cfg.save_passwords,
            defaults_rev: cfg.defaults_rev,
        }
    }

    pub fn apply_to(&self, cfg: &mut ConfigFile) {
        cfg.download_dir = self.download_dir.clone();
        cfg.language = self.language.clone();
        cfg.theme_pref = self.theme_pref.clone();
        cfg.renderer_mode = self.renderer_mode.clone();
        cfg.font_family = self.font_family.clone();
        cfg.font_size = self.font_size;
        cfg.terminal_line_spacing = self.terminal_line_spacing;
        cfg.terminal_bold = self.terminal_bold;
        cfg.terminal_cursor_style = self.terminal_cursor_style.clone();
        cfg.terminal_cursor_color = self.terminal_cursor_color.clone();
        cfg.output_highlight_disabled = self.output_highlight_disabled;
        cfg.output_highlight_preset = self.output_highlight_preset.clone();
        cfg.output_highlight_rules = self.output_highlight_rules.clone();
        cfg.json_format_disabled = self.json_format_disabled;
        cfg.ui_scale = self.ui_scale;
        cfg.wallpaper = self.wallpaper.clone();
        cfg.sftp_no_follow_cd = self.sftp_no_follow_cd;
        cfg.download_always_ask = self.download_always_ask;
        cfg.paste_confirm_disabled = self.paste_confirm_disabled;
        cfg.extra_paste_shortcuts_disabled = self.extra_paste_shortcuts_disabled;
        cfg.select_copy_right_paste_disabled = self.select_copy_right_paste_disabled;
        cfg.zen_mode = self.zen_mode;
        cfg.quick_commands_as_sidebar = self.quick_commands_as_sidebar;
        cfg.collapse_sftp_default = self.collapse_sftp_default;
        cfg.welcome_as_sidebar = self.welcome_as_sidebar;
        cfg.confirm_delete_group_disabled = self.confirm_delete_group_disabled;
        cfg.confirm_delete_session = self.confirm_delete_session;
        cfg.welcome_single_click_connect = self.welcome_single_click_connect;
        cfg.wallpaper_overlay = self.wallpaper_overlay;
        cfg.panel_font = self.panel_font;
        cfg.update_check_disabled = self.update_check_disabled;
        cfg.ssh_keepalive_secs = self.ssh_keepalive_secs;
        cfg.algorithm_preferences = self.algorithm_preferences.clone();
        cfg.save_passwords = self.save_passwords;
        cfg.defaults_rev = self.defaults_rev;
    }
}

/// `ui-state.json` — layout chrome and Quick Connect fold state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiStateFile {
    #[serde(default)]
    pub collapsed_session_groups: Option<Vec<String>>,
    #[serde(default)]
    pub quick_panel_open: bool,
    #[serde(default)]
    pub quick_panel_collapsed: bool,
    #[serde(default = "default_quick_panel_width")]
    pub quick_panel_width: f32,
    #[serde(default = "default_quick_panel_height")]
    pub quick_panel_height: f32,
    #[serde(default)]
    pub quick_panel_dock: String,
    #[serde(default = "default_sftp_width")]
    pub sftp_panel_width: f32,
    #[serde(default = "default_sftp_height")]
    pub sftp_panel_height: f32,
    #[serde(default = "default_sftp_tree_width")]
    pub sftp_tree_width: f32,
    #[serde(default)]
    pub sftp_dock: String,
    #[serde(default)]
    pub window_width: f32,
    #[serde(default)]
    pub window_height: f32,
    #[serde(default)]
    pub welcome_sidebar_width: f32,
    #[serde(default)]
    pub welcome_sidebar_dock: String,
    #[serde(default)]
    pub welcome_collapsed: Option<bool>,
    #[serde(default = "default_welcome_session_col_name")]
    pub welcome_session_col_name: f32,
    #[serde(default = "default_welcome_session_col_host")]
    pub welcome_session_col_host: f32,
}

impl Default for UiStateFile {
    fn default() -> Self {
        Self::from_config(&ConfigFile::default())
    }
}

impl UiStateFile {
    pub fn from_config(cfg: &ConfigFile) -> Self {
        Self {
            collapsed_session_groups: cfg.collapsed_session_groups.clone(),
            quick_panel_open: cfg.quick_panel_open,
            quick_panel_collapsed: cfg.quick_panel_collapsed,
            quick_panel_width: cfg.quick_panel_width,
            quick_panel_height: cfg.quick_panel_height,
            quick_panel_dock: cfg.quick_panel_dock.clone(),
            sftp_panel_width: cfg.sftp_panel_width,
            sftp_panel_height: cfg.sftp_panel_height,
            sftp_tree_width: cfg.sftp_tree_width,
            sftp_dock: cfg.sftp_dock.clone(),
            window_width: cfg.window_width,
            window_height: cfg.window_height,
            welcome_sidebar_width: cfg.welcome_sidebar_width,
            welcome_sidebar_dock: cfg.welcome_sidebar_dock.clone(),
            welcome_collapsed: cfg.welcome_collapsed,
            welcome_session_col_name: cfg.welcome_session_col_name,
            welcome_session_col_host: cfg.welcome_session_col_host,
        }
    }

    pub fn apply_to(&self, cfg: &mut ConfigFile) {
        cfg.collapsed_session_groups = self.collapsed_session_groups.clone();
        cfg.quick_panel_open = self.quick_panel_open;
        cfg.quick_panel_collapsed = self.quick_panel_collapsed;
        cfg.quick_panel_width = self.quick_panel_width;
        cfg.quick_panel_height = self.quick_panel_height;
        cfg.quick_panel_dock = self.quick_panel_dock.clone();
        cfg.sftp_panel_width = self.sftp_panel_width;
        cfg.sftp_panel_height = self.sftp_panel_height;
        cfg.sftp_tree_width = self.sftp_tree_width;
        cfg.sftp_dock = self.sftp_dock.clone();
        cfg.window_width = self.window_width;
        cfg.window_height = self.window_height;
        cfg.welcome_sidebar_width = self.welcome_sidebar_width;
        cfg.welcome_sidebar_dock = self.welcome_sidebar_dock.clone();
        cfg.welcome_collapsed = self.welcome_collapsed;
        cfg.welcome_session_col_name = self.welcome_session_col_name;
        cfg.welcome_session_col_host = self.welcome_session_col_host;
    }
}
