use super::super::structs::*;
use super::normalize::*;

impl ConfigStore {
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

    /// Whether raw terminal output is appended to per-tab session log files.
    pub fn session_log_enabled(&self) -> bool {
        self.cache.session_log_enabled
    }

    pub fn set_session_log_enabled(&mut self, enabled: bool) {
        self.cache.session_log_enabled = enabled;
    }

    /// Directory for session output logs. Empty means the built-in default.
    pub fn session_log_dir(&self) -> &str {
        &self.cache.session_log_dir
    }

    pub fn set_session_log_dir(&mut self, dir: String) {
        self.cache.session_log_dir = dir;
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

    #[test]
    fn output_highlight_defaults_and_preset_validation() {
        let fresh = fresh_config();
        assert_eq!(fresh.output_highlight_rules.len(), 4);
        assert_eq!(fresh.output_highlight_rules[0].name, "error");
        assert_eq!(fresh.output_highlight_rules[1].name, "success");
        assert_eq!(fresh.output_highlight_rules[2].name, "warning");
        assert_eq!(fresh.output_highlight_rules[3].name, "IP");
        assert!(fresh
            .output_highlight_rules
            .iter()
            .all(|r| r.regex && r.enabled));

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
}
