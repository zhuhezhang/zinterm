use std::fs;
use std::path::Path;

use anyhow::{Context, Result};

use super::super::structs::*;
use super::normalize::*;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chacha20poly1305::{
    aead::{Aead, KeyInit},
    ChaCha20Poly1305,
};

impl ConfigStore {
    /// Marks a password encrypted with the **portable export key** (issue #46).
    /// Kept only so hand-edited / older import files can still decrypt.
    pub(super) const EXPORT_PREFIX: &'static str = "enc:exp:v1:";

    /// Fixed 32-byte key for portable exports. Baked into the binary so an
    /// exported file decrypts on any machine. Obfuscation only — see `ExportFile`.
    pub(super) const EXPORT_KEY: [u8; 32] = *b"zinterm.export.portable.key.v01!";

    // ── Portable export / import (issue #46) ──────────────────────────────

    /// `Date.toString()`-style local timestamp for the export file header.
    pub(super) fn format_export_timestamp() -> String {
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
    pub(super) fn decrypt_export(s: &str) -> Option<String> {
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

    /// Explicit `cache.empty_groups` entries that currently have no session in that
    /// folder or any descendant path.
    pub(super) fn collect_empty_groups(&self) -> Vec<String> {
        self.cache
            .empty_groups
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
            anyhow::bail!("unsupported export version {} (expected 1)", file.version);
        }

        // Restore empty folders before sessions so the Quick Connect tree
        // already has those paths when connections land in sibling groups.
        let mut groups_added = false;
        for group in &file.empty_groups {
            let before = self.cache.empty_groups.len();
            self.add_group(group.clone());
            if self.cache.empty_groups.len() > before {
                groups_added = true;
            }
        }

        let save_passwords = self.save_passwords();
        let mut added = 0usize;
        let mut skipped = 0usize;
        for (i, raw_session) in file.sessions.iter().enumerate() {
            let mut s =
                Session::from_import_value(raw_session).with_context(|| format!("session[{i}]"))?;
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

    // ── Portable quick-command export / import ────────────────────────────

    /// Explicit `cache.quick_empty_groups` entries that currently have no command.
    pub(super) fn collect_quick_empty_groups(&self) -> Vec<String> {
        self.cache
            .quick_empty_groups
            .iter()
            .filter(|g| {
                let g = g.trim();
                if g.is_empty() || g.eq_ignore_ascii_case("default") {
                    return false;
                }
                !self
                    .cache
                    .quick_commands
                    .iter()
                    .any(|c| c.group.trim() == g)
            })
            .cloned()
            .collect()
    }

    /// Export all quick commands to a portable JSON string. Returns `(json, count)`.
    pub fn export_quick_commands_json(&self) -> Result<(String, usize)> {
        let commands = self.cache.quick_commands.clone();
        let count = commands.len();
        let out = QuickCommandsExportFile {
            zinterm_export: "quick_commands".into(),
            version: 1,
            exported_at: Self::format_export_timestamp(),
            empty_groups: self.collect_quick_empty_groups(),
            commands,
        };
        Ok((serde_json::to_string_pretty(&out)?, count))
    }

    /// Export all quick commands to a portable JSON file. Returns the command count.
    pub fn export_quick_commands_to(&self, path: &Path) -> Result<usize> {
        let (raw, count) = self.export_quick_commands_json()?;
        fs::write(path, raw).with_context(|| format!("failed to write {}", path.display()))?;
        Ok(count)
    }

    /// Import quick commands from a string produced by [`Self::export_quick_commands_json`].
    ///
    /// A command is skipped when the same group already has the same `name`
    /// (case-sensitive trim). Empty name/command entries are skipped. Empty
    /// groups are restored first. Returns `(added, skipped)`.
    pub fn import_quick_commands_json(&mut self, raw: &str) -> Result<(usize, usize)> {
        // 先校验导出类型，避免把会话导出文件误当成缺字段的命令文件
        let probe: serde_json::Value =
            serde_json::from_str(raw).context("not a valid zinterm quick-command export file")?;
        let export_kind = probe
            .get("zinterm_export")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if export_kind != "quick_commands" {
            anyhow::bail!(
                "invalid export: zinterm_export must be \"quick_commands\" (got {:?})",
                export_kind
            );
        }
        let file: QuickCommandsExportFile = serde_json::from_value(probe)
            .context("not a valid zinterm quick-command export file")?;
        if file.version != 1 {
            anyhow::bail!("unsupported export version {} (expected 1)", file.version);
        }

        // Restore empty groups before commands so the manage dialog already
        // shows those folders when commands land in sibling groups.
        let mut groups_added = false;
        for group in &file.empty_groups {
            let before = self.cache.quick_empty_groups.len();
            self.add_quick_group(group.clone());
            if self.cache.quick_empty_groups.len() > before {
                groups_added = true;
            }
        }

        let mut added = 0usize;
        let mut skipped = 0usize;
        for mut cmd in file.commands {
            cmd.name = cmd.name.trim().to_string();
            cmd.group = cmd.group.trim().to_string();
            if cmd.group.eq_ignore_ascii_case("default") {
                cmd.group.clear();
            }
            if cmd.name.is_empty() || cmd.command.trim().is_empty() {
                skipped += 1;
                continue;
            }
            let group = cmd.group.as_str();
            let name = cmd.name.as_str();
            let dup = self
                .cache
                .quick_commands
                .iter()
                .any(|x| x.group.trim() == group && x.name.trim() == name);
            if dup {
                skipped += 1;
                continue;
            }
            self.cache.quick_commands.push(cmd);
            added += 1;
        }
        if added > 0 || groups_added {
            self.save_parts(SaveKind::COMMANDS)?;
        }
        Ok((added, skipped))
    }

    /// Import quick commands from a file produced by [`Self::export_quick_commands_to`].
    pub fn import_quick_commands_from(&mut self, path: &Path) -> Result<(usize, usize)> {
        let raw = fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        self.import_quick_commands_json(&raw)
    }

    // ── Portable settings export / import ─────────────────────────────────

    /// Export Settings-panel preferences (including custom highlight rules) to
    /// a portable JSON string. Sessions, quick commands, and UI layout chrome
    /// are not included.
    pub fn export_settings_json(&self) -> Result<String> {
        let out = SettingsExportFile {
            zinterm_export: "settings".into(),
            version: 1,
            exported_at: Self::format_export_timestamp(),
            settings: SettingsFile::from_config(&self.cache),
        };
        Ok(serde_json::to_string_pretty(&out)?)
    }

    /// Export Settings-panel preferences to a portable JSON file.
    pub fn export_settings_to(&self, path: &Path) -> Result<()> {
        let raw = self.export_settings_json()?;
        fs::write(path, raw).with_context(|| format!("failed to write {}", path.display()))?;
        Ok(())
    }

    /// Import settings from a string produced by [`Self::export_settings_json`].
    ///
    /// Preference fields use **overwrite** semantics: present and valid values
    /// replace the current setting; missing or invalid items/values are ignored.
    /// `output_highlight_rules` are **additive**: required `name` + `pattern`
    /// (keyword/regex); same-name rules are skipped; other fields are optional
    /// and fall back to the same defaults as the UI add-rule form when missing
    /// or invalid. Does not touch `defaults_rev`. Does **not** persist — the
    /// caller applies a live preview; disk write waits for Settings › Save.
    pub fn import_settings_json(&mut self, raw: &str) -> Result<SettingsImportStats> {
        let probe: serde_json::Value =
            serde_json::from_str(raw).context("not a valid zinterm settings export file")?;
        let export_kind = probe
            .get("zinterm_export")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if export_kind != "settings" {
            anyhow::bail!(
                "invalid export: zinterm_export must be \"settings\" (got {:?})",
                export_kind
            );
        }
        let version = probe
            .get("version")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        if version != 1 {
            anyhow::bail!("unsupported export version {version} (expected 1)");
        }

        let mut prefs_applied = self.apply_imported_settings_prefs(&probe);

        let (rules_added, rules_skipped) = self.import_highlight_rules_additive(
            probe
                .get("output_highlight_rules")
                .and_then(|v| v.as_array())
                .map(|a| a.as_slice())
                .unwrap_or(&[]),
        );
        if rules_added > 0 {
            prefs_applied = true;
        }

        Ok(SettingsImportStats {
            prefs_applied,
            rules_added,
            rules_skipped,
        })
    }

    /// Import settings from a file produced by [`Self::export_settings_to`].
    pub fn import_settings_from(&mut self, path: &Path) -> Result<SettingsImportStats> {
        let raw = fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        self.import_settings_json(&raw)
    }

    /// Overwrite individual Settings-panel fields when the JSON value is valid.
    /// Skips `output_highlight_rules` (handled additively) and `defaults_rev`.
    fn apply_imported_settings_prefs(&mut self, root: &serde_json::Value) -> bool {
        let mut changed = false;

        if let Some(s) = root.get("download_dir").and_then(|v| v.as_str()) {
            self.set_download_dir(s.to_string());
            changed = true;
        }
        if let Some(s) = root.get("language").and_then(|v| v.as_str()) {
            self.set_language(s.to_string());
            changed = true;
        }
        if let Some(s) = root.get("theme_pref").and_then(|v| v.as_str()) {
            match s.trim() {
                "" | "system" | "dark" | "light" => {
                    self.set_theme_pref(s.trim().to_string());
                    changed = true;
                }
                _ => {}
            }
        }
        if let Some(s) = root.get("renderer_mode").and_then(|v| v.as_str()) {
            if is_valid_renderer_mode_input(s) {
                self.set_renderer_mode(s.to_string());
                changed = true;
            }
        }
        if let Some(s) = root.get("font_family").and_then(|v| v.as_str()) {
            self.set_font_family(s.to_string());
            changed = true;
        }
        if let Some(n) = json_u32(root.get("font_size")) {
            if (8..=32).contains(&n) {
                self.set_font_size(n);
                changed = true;
            }
        }
        if let Some(n) = json_f32(root.get("terminal_line_spacing")) {
            if (0.8..=1.5).contains(&n) {
                self.set_terminal_line_spacing(n);
                changed = true;
            }
        }
        if let Some(b) = root.get("terminal_bold").and_then(|v| v.as_bool()) {
            self.set_terminal_bold(b);
            changed = true;
        }
        if let Some(s) = root.get("terminal_cursor_style").and_then(|v| v.as_str()) {
            match s.trim() {
                "bar" | "block" | "underline" => {
                    self.set_terminal_cursor_style(s.trim().to_string());
                    changed = true;
                }
                _ => {}
            }
        }
        if let Some(s) = root.get("terminal_cursor_color").and_then(|v| v.as_str()) {
            let t = s.trim();
            if t.is_empty() {
                self.cache.terminal_cursor_color.clear();
                changed = true;
            } else if self.set_terminal_cursor_color(t) {
                changed = true;
            }
        }
        if let Some(b) = root
            .get("output_highlight_disabled")
            .and_then(|v| v.as_bool())
        {
            self.set_output_highlight_enabled(!b);
            changed = true;
        }
        if let Some(s) = root
            .get("output_highlight_preset")
            .and_then(|v| v.as_str())
        {
            match s.trim() {
                "log" | "devops" => {
                    self.set_output_highlight_preset(s.trim().to_string());
                    changed = true;
                }
                _ => {}
            }
        }
        if let Some(b) = root.get("json_format_disabled").and_then(|v| v.as_bool()) {
            self.set_json_format_output(!b);
            changed = true;
        }
        if let Some(b) = root.get("session_log_enabled").and_then(|v| v.as_bool()) {
            self.set_session_log_enabled(b);
            changed = true;
        }
        if let Some(s) = root.get("session_log_dir").and_then(|v| v.as_str()) {
            if !s.trim().is_empty() {
                self.set_session_log_dir(s.to_string());
                changed = true;
            }
        }
        if let Some(n) = json_u32(root.get("ui_scale")) {
            if (80..=200).contains(&n) {
                self.set_ui_scale(n);
                changed = true;
            }
        }
        if let Some(s) = root.get("wallpaper").and_then(|v| v.as_str()) {
            self.set_wallpaper(s.to_string());
            changed = true;
        }
        if let Some(b) = root.get("sftp_no_follow_cd").and_then(|v| v.as_bool()) {
            self.set_sftp_follow_cd(!b);
            changed = true;
        }
        if let Some(b) = root.get("download_always_ask").and_then(|v| v.as_bool()) {
            self.set_download_always_ask(b);
            changed = true;
        }
        if let Some(b) = root
            .get("paste_confirm_disabled")
            .and_then(|v| v.as_bool())
        {
            self.set_paste_confirm_enabled(!b);
            changed = true;
        }
        if let Some(b) = root
            .get("extra_paste_shortcuts_disabled")
            .and_then(|v| v.as_bool())
        {
            self.set_extra_paste_shortcuts_enabled(!b);
            changed = true;
        }
        if let Some(b) = root
            .get("select_copy_right_paste_disabled")
            .and_then(|v| v.as_bool())
        {
            self.set_select_copy_right_paste_enabled(!b);
            changed = true;
        }
        if let Some(b) = root.get("zen_mode").and_then(|v| v.as_bool()) {
            self.set_zen_mode(b);
            changed = true;
        }
        if let Some(b) = root
            .get("quick_commands_as_sidebar")
            .and_then(|v| v.as_bool())
        {
            self.set_quick_commands_as_sidebar(b);
            changed = true;
        }
        if let Some(b) = root
            .get("collapse_sftp_default")
            .and_then(|v| v.as_bool())
        {
            self.set_collapse_sftp_default(b);
            changed = true;
        }
        if let Some(b) = root.get("welcome_as_sidebar").and_then(|v| v.as_bool()) {
            self.set_welcome_as_sidebar(b);
            changed = true;
        }
        if let Some(b) = root
            .get("confirm_delete_group_disabled")
            .and_then(|v| v.as_bool())
        {
            self.set_confirm_delete_group(!b);
            changed = true;
        }
        if let Some(b) = root
            .get("confirm_delete_session")
            .and_then(|v| v.as_bool())
        {
            self.set_confirm_delete_session(b);
            changed = true;
        }
        if let Some(b) = root
            .get("welcome_single_click_connect")
            .and_then(|v| v.as_bool())
        {
            self.set_welcome_single_click_connect(b);
            changed = true;
        }
        if let Some(n) = json_f32(root.get("wallpaper_overlay")) {
            if (0.30..=1.0).contains(&n) {
                self.set_wallpaper_overlay(n);
                changed = true;
            }
        }
        if let Some(n) = json_u32(root.get("panel_font")) {
            if (80..=160).contains(&n) {
                self.set_panel_font(n);
                changed = true;
            }
        }
        if let Some(b) = root
            .get("update_check_disabled")
            .and_then(|v| v.as_bool())
        {
            self.set_update_check_enabled(!b);
            changed = true;
        }
        if let Some(n) = json_u32(root.get("ssh_keepalive_secs")) {
            if n <= SSH_KEEPALIVE_SECS_MAX {
                self.set_ssh_keepalive_secs(n);
                changed = true;
            }
        }
        if let Some(v) = root.get("algorithm_preferences") {
            if let Ok(prefs) = serde_json::from_value::<AlgorithmPreferences>(v.clone()) {
                self.set_algorithm_preferences(prefs);
                changed = true;
            }
        }
        if let Some(b) = root.get("save_passwords").and_then(|v| v.as_bool()) {
            self.set_save_passwords(b);
            changed = true;
        }

        changed
    }

    /// Additive import of custom highlight rules. Returns `(added, skipped)`.
    fn import_highlight_rules_additive(
        &mut self,
        rules: &[serde_json::Value],
    ) -> (usize, usize) {
        let mut added = 0usize;
        let mut skipped = 0usize;
        const MAX_RULES: usize = 128;

        for raw in rules {
            if self.cache.output_highlight_rules.len() >= MAX_RULES {
                skipped += 1;
                continue;
            }
            let Some(rule) = parse_imported_highlight_rule(raw) else {
                skipped += 1;
                continue;
            };
            let name = rule.name.as_str();
            let dup = self
                .cache
                .output_highlight_rules
                .iter()
                .any(|x| x.name.trim() == name);
            if dup {
                skipped += 1;
                continue;
            }
            self.add_output_highlight_rule(rule);
            added += 1;
        }
        (added, skipped)
    }
}

/// Parse one highlight-rule object from an import file. Required: non-empty
/// `name` and `pattern` (valid regex when `regex` is true). Optional fields use
/// the same defaults as the UI add-rule form when missing/invalid.
fn parse_imported_highlight_rule(raw: &serde_json::Value) -> Option<OutputHighlightRule> {
    let obj = raw.as_object()?;
    let name = obj.get("name").and_then(|v| v.as_str())?.trim();
    if name.is_empty() || name.chars().count() > 64 {
        return None;
    }
    let pattern = obj.get("pattern").and_then(|v| v.as_str())?.trim();
    if pattern.is_empty() || pattern.chars().count() > 512 {
        return None;
    }

    // 与 UI 新增规则表单一致的默认值
    let regex = obj.get("regex").and_then(|v| v.as_bool()).unwrap_or(false);
    let case_sensitive = obj
        .get("case_sensitive")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let whole_line = obj
        .get("whole_line")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let enabled = obj.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true);

    if regex {
        if regex::RegexBuilder::new(pattern)
            .case_insensitive(!case_sensitive)
            .build()
            .is_err()
        {
            return None;
        }
    }

    let color = match obj.get("color").and_then(|v| v.as_str()) {
        Some(c) if !c.trim().is_empty() => {
            // 合法 #RRGGBB 或旧版调色板名；其余忽略并回落到 UI 默认红色
            if normalize_hex_color(c).is_some() {
                normalize_highlight_color(c)
            } else {
                match c.trim().to_ascii_lowercase().as_str() {
                    "red" | "yellow" | "green" | "cyan" | "magenta" | "gray" | "grey" => {
                        normalize_highlight_color(c)
                    }
                    _ => "#F14C4C".to_string(),
                }
            }
        }
        _ => "#F14C4C".to_string(),
    };

    Some(OutputHighlightRule {
        name: name.to_string(),
        pattern: pattern.to_string(),
        regex,
        case_sensitive,
        whole_line,
        color,
        enabled,
    })
}

fn json_u32(v: Option<&serde_json::Value>) -> Option<u32> {
    v.and_then(|v| v.as_u64())
        .and_then(|n| u32::try_from(n).ok())
}

fn json_f32(v: Option<&serde_json::Value>) -> Option<f32> {
    v.and_then(|v| v.as_f64()).map(|n| n as f32)
}

fn is_valid_renderer_mode_input(mode: &str) -> bool {
    matches!(
        mode.trim(),
        "auto" | "gpu" | "software" | "femtovg" | "skia"
    )
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
    use uuid::Uuid;

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
            private_key: Secret::new(
                "-----BEGIN OPENSSH PRIVATE KEY-----\nAAAA\n-----END OPENSSH PRIVATE KEY-----\n",
            ),
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
        assert!(store
            .sessions()
            .iter()
            .any(|s| s.group == "lab" && s.id == "saved-1700000000003-cccc"));
        assert!(!store
            .sessions()
            .iter()
            .any(|s| s.id == "saved-1700000000002-bbbb"));
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

        let local = sessions
            .iter()
            .find(|s| s.kind == SessionKind::Local)
            .unwrap();
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

        let export_path =
            std::env::temp_dir().join(format!("ms-exp-groups-{}.json", Uuid::new_v4()));
        assert_eq!(a.export_to(&export_path).unwrap(), 1);
        let raw = std::fs::read_to_string(&export_path).unwrap();
        assert!(
            raw.contains("\"empty_groups\": [\n    \"empty-lab\"\n  ]")
                || raw.contains("\"empty-lab\"")
        );
        assert!(
            !raw.contains("\"has-session\"") || {
                // has-session may appear on the session's group field, but not in empty_groups.
                let file: ExportFile = serde_json::from_str(&raw).unwrap();
                !file.empty_groups.iter().any(|g| g == "has-session")
                    && file.empty_groups.iter().any(|g| g == "empty-lab")
            }
        );

        let mut b = temp_store();
        assert_eq!(b.import_from(&export_path).unwrap(), (1, 0));
        assert!(b.session_group_exists("empty-lab"));
        assert!(b.sessions().iter().any(|s| s.group == "has-session"));

        let _ = std::fs::remove_file(&export_path);
        let _ = std::fs::remove_file(&a.path);
        let _ = std::fs::remove_file(&b.path);
    }

    #[test]
    fn quick_commands_export_import_roundtrip_and_skip_duplicates() {
        let mut a = temp_store();
        a.add_quick_group("empty-ops".into());
        a.add_quick_group("ops".into());
        a.set_quick_commands(vec![
            QuickCommand {
                name: "ll".into(),
                command: "ls -la".into(),
                group: "ops".into(),
                send_enter: true,
            },
            QuickCommand {
                name: "df".into(),
                command: "df -h".into(),
                group: String::new(),
                send_enter: false,
            },
        ]);

        let export_path =
            std::env::temp_dir().join(format!("ms-qcm-exp-{}.json", Uuid::new_v4()));
        assert_eq!(a.export_quick_commands_to(&export_path).unwrap(), 2);
        let raw = std::fs::read_to_string(&export_path).unwrap();
        assert!(raw.contains("\"zinterm_export\": \"quick_commands\""));
        assert!(raw.contains("\"empty-ops\""));
        assert!(raw.contains("ls -la"));

        let mut b = temp_store();
        assert_eq!(b.import_quick_commands_from(&export_path).unwrap(), (2, 0));
        assert_eq!(b.quick_commands().len(), 2);
        assert!(b.quick_empty_groups().iter().any(|g| g == "empty-ops"));
        assert_eq!(b.quick_commands()[0].name, "ll");
        assert!(!b.quick_commands()[1].send_enter);

        // 同组同名视为重复，再次导入应全部跳过
        assert_eq!(b.import_quick_commands_from(&export_path).unwrap(), (0, 2));
        assert_eq!(b.quick_commands().len(), 2);

        let _ = std::fs::remove_file(&export_path);
        let _ = std::fs::remove_file(&a.path);
        let _ = std::fs::remove_file(&b.path);
    }

    #[test]
    fn quick_commands_import_rejects_sessions_export() {
        let mut store = temp_store();
        let raw = r#"{"zinterm_export":"sessions","version":1,"exported_at":"t","empty_groups":[],"sessions":[]}"#;
        let err = format!("{:#}", store.import_quick_commands_json(raw).unwrap_err());
        assert!(err.contains("quick_commands"), "{err}");
        let _ = std::fs::remove_file(&store.path);
    }

    #[test]
    fn settings_export_import_overwrites_prefs_and_adds_rules() {
        let mut a = temp_store();
        a.set_font_size(18);
        a.set_theme_pref("dark".into());
        a.set_output_highlight_preset("devops".into());
        a.add_output_highlight_rule(OutputHighlightRule {
            name: "timeout".into(),
            pattern: "timeout".into(),
            regex: false,
            case_sensitive: false,
            whole_line: false,
            color: "#F14C4C".into(),
            enabled: true,
        });

        let export_path =
            std::env::temp_dir().join(format!("ms-settings-exp-{}.json", Uuid::new_v4()));
        a.export_settings_to(&export_path).unwrap();
        let raw = std::fs::read_to_string(&export_path).unwrap();
        assert!(raw.contains("\"zinterm_export\": \"settings\""));
        assert!(raw.contains("\"font_size\": 18"));
        assert!(raw.contains("timeout"));

        let mut b = temp_store();
        b.set_font_size(13);
        b.add_output_highlight_rule(OutputHighlightRule {
            name: "timeout".into(), // 同名应跳过
            pattern: "old".into(),
            regex: false,
            case_sensitive: false,
            whole_line: false,
            color: "#23D18B".into(),
            enabled: true,
        });
        let stats = b.import_settings_from(&export_path).unwrap();
        assert!(stats.prefs_applied);
        assert_eq!(stats.rules_added, 0);
        assert_eq!(stats.rules_skipped, 1);
        assert_eq!(b.font_size(), 18);
        assert_eq!(b.theme_pref(), "dark");
        assert_eq!(b.output_highlight_preset(), "devops");
        assert_eq!(b.output_highlight_rules().len(), 1);
        assert_eq!(b.output_highlight_rules()[0].pattern, "old");

        // 再导入带新规则的文件
        let raw2 = r#"{
            "zinterm_export": "settings",
            "version": 1,
            "exported_at": "t",
            "font_size": 99,
            "theme_pref": "nope",
            "output_highlight_rules": [
                {"name": "warn", "pattern": "WARN"},
                {"name": "", "pattern": "x"},
                {"pattern": "no-name"},
                {"name": "bad-re", "pattern": "(", "regex": true},
                {"name": "ip", "pattern": "\\d+", "regex": true, "color": "not-a-color"}
            ]
        }"#;
        let stats2 = b.import_settings_json(raw2).unwrap();
        assert_eq!(b.font_size(), 18); // 无效 font_size 忽略
        assert_eq!(b.theme_pref(), "dark"); // 无效 theme 忽略
        assert_eq!(stats2.rules_added, 2); // warn + ip
        assert_eq!(stats2.rules_skipped, 3);
        assert!(stats2.prefs_applied);
        let ip = b
            .output_highlight_rules()
            .iter()
            .find(|r| r.name == "ip")
            .unwrap();
        assert_eq!(ip.color, "#F14C4C"); // 无效颜色 → UI 默认
        assert!(!ip.case_sensitive);
        assert!(ip.enabled);

        let _ = std::fs::remove_file(&export_path);
        let _ = std::fs::remove_file(&a.path);
        let _ = std::fs::remove_file(&b.path);
    }

    #[test]
    fn settings_import_rejects_sessions_export() {
        let mut store = temp_store();
        let raw = r#"{"zinterm_export":"sessions","version":1,"exported_at":"t","empty_groups":[],"sessions":[]}"#;
        let err = format!("{:#}", store.import_settings_json(raw).unwrap_err());
        assert!(err.contains("settings"), "{err}");
        let _ = std::fs::remove_file(&store.path);
    }
}
