use super::super::structs::*;
use super::normalize::*;

/// Built-in starter custom highlight rules (aligned with zauterm defaults):
/// error / success / warning keywords and IPv4 addresses.
pub(super) fn default_output_highlight_rules() -> Vec<OutputHighlightRule> {
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
pub(super) fn fresh_config() -> ConfigFile {
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
pub(super) fn migrate_defaults(cfg: &mut ConfigFile) -> bool {
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

pub(super) fn migrate_output_highlight_rules(cfg: &mut ConfigFile) -> bool {
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
pub(super) fn dedup_keep_last(items: &mut Vec<String>) {
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
pub(super) fn repair_history_newlines(cmd: String) -> String {
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

/// Repair configurations created before #316/#324, when the Move-to menu exposed
/// the built-in `system` group as a destination for saved server sessions.
pub(super) fn normalize_reserved_session_groups(cfg: &mut ConfigFile) -> bool {
    let old_group_count = cfg.empty_groups.len();
    cfg.empty_groups
        .retain(|group| !is_reserved_session_group(group.trim()));
    let mut changed = cfg.empty_groups.len() != old_group_count;
    for session in &mut cfg.sessions {
        if is_reserved_session_group(session.group.trim()) {
            session.group.clear();
            changed = true;
        }
    }
    changed
}

#[cfg(any(target_os = "macos", test))]
pub(super) fn normalize_macos_renderer_mode(mode: &str) -> &'static str {
    match mode {
        "femtovg" => "femtovg",
        "skia" => "skia",
        _ => "software",
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
        store.cache.empty_groups = vec!["lab".into()];
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
        assert_eq!(store.empty_groups(), &["lab".to_string()]);
        assert_eq!(store.quick_commands().len(), 1);
        assert_eq!(store.quick_commands()[0].name, "ll");
        assert!(store.quick_empty_groups().iter().any(|g| g == "ops"));
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
}
