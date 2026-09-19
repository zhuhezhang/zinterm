use super::super::structs::*;
use super::defaults_migration::*;

impl ConfigStore {
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
}
