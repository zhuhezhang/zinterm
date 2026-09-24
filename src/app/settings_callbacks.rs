use super::*;

pub(super) fn wire_settings_callbacks(
    window: &AppWindow,
    store: &Rc<RefCell<ConfigStore>>,
    bufs: &TermBuffers,
    pending_window_size_restore: &Rc<Cell<Option<(f32, f32)>>>,
) -> (
    Arc<std::sync::atomic::AtomicBool>,
    Arc<std::sync::atomic::AtomicU32>,
    Arc<std::sync::Mutex<crate::config::AlgorithmPreferences>>,
    SessionLoggers,
) {
    let sftp_follow_cd = Arc::new(std::sync::atomic::AtomicBool::new(
        store.borrow().sftp_follow_cd(),
    ));
    window.set_sftp_follow_cd(store.borrow().sftp_follow_cd());
    {
        let store = store.clone();
        let flag = sftp_follow_cd.clone();
        window.on_set_sftp_follow_cd(move |follow| {
            flag.store(follow, std::sync::atomic::Ordering::Relaxed);
            let mut s = store.borrow_mut();
            s.set_sftp_follow_cd(follow);
        });
    }

    let log_dir = {
        let mut s = store.borrow_mut();
        if s.session_log_dir().is_empty() {
            let d = crate::session::default_session_log_dir()
                .to_string_lossy()
                .into_owned();
            s.set_session_log_dir(d.clone());
            s.save_later(crate::config::SaveKind::SETTINGS);
            d
        } else {
            s.session_log_dir().to_string()
        }
    };
    let session_logs = SessionLoggers::new(store.borrow().session_log_enabled(), log_dir.clone());
    window.set_session_log_enabled(store.borrow().session_log_enabled());
    window.set_session_log_dir(log_dir.into());
    {
        let store = store.clone();
        let logs = session_logs.clone();
        window.on_set_session_log_enabled(move |enabled| {
            logs.set_enabled(enabled);
            let mut s = store.borrow_mut();
            s.set_session_log_enabled(enabled);
        });
    }
    {
        let weak = window.as_weak();
        let store = store.clone();
        let logs = session_logs.clone();
        window.on_pick_session_log_dir(move || {
            if let Some(folder) = rfd::FileDialog::new().pick_folder() {
                let dir = folder.to_string_lossy().to_string();
                {
                    let mut s = store.borrow_mut();
                    s.set_session_log_dir(dir.clone());
                }
                logs.set_dir(dir.clone());
                if let Some(w) = weak.upgrade() {
                    w.set_session_log_dir(dir.into());
                }
            }
        });
    }
    {
        let weak = window.as_weak();
        window.on_open_session_log_dir(move || {
            let Some(w) = weak.upgrade() else {
                return;
            };
            let dir = w.get_session_log_dir().to_string();
            let dir = if dir.trim().is_empty() {
                crate::session::default_session_log_dir()
            } else {
                std::path::PathBuf::from(dir)
            };
            if let Err(e) = std::fs::create_dir_all(&dir) {
                tracing::warn!("session log: create_dir_all {}: {e}", dir.display());
                return;
            }
            let dir = dir.to_string_lossy().to_string();
            #[cfg(windows)]
            {
                let _ = std::process::Command::new("explorer").arg(&dir).spawn();
            }
            #[cfg(target_os = "macos")]
            {
                let _ = std::process::Command::new("open").arg(&dir).spawn();
            }
            #[cfg(all(not(windows), not(target_os = "macos")))]
            {
                let _ = std::process::Command::new("xdg-open").arg(&dir).spawn();
            }
        });
    }

    let ssh_keepalive_secs = Arc::new(std::sync::atomic::AtomicU32::new(
        store.borrow().ssh_keepalive_secs(),
    ));
    window.set_ssh_keepalive_secs(store.borrow().ssh_keepalive_secs() as i32);
    {
        let store = store.clone();
        let flag = ssh_keepalive_secs.clone();
        window.on_set_ssh_keepalive_secs(move |secs: i32| {
            let secs = (secs.max(0) as u32).min(crate::config::SSH_KEEPALIVE_SECS_MAX);
            flag.store(secs, std::sync::atomic::Ordering::Relaxed);
            let mut s = store.borrow_mut();
            s.set_ssh_keepalive_secs(secs);
        });
    }

    let ssh_algorithm_prefs = Arc::new(std::sync::Mutex::new(
        store.borrow().algorithm_preferences(),
    ));
    window.set_ssh_algorithm_category("kex".into());
    window.set_ssh_algorithm_rows(ssh_algorithm_row_model(
        &store.borrow().algorithm_preferences(),
        "kex",
    ));
    {
        let store = store.clone();
        let weak = window.as_weak();
        window.on_set_ssh_algorithm_category(move |cat: SharedString| {
            if let Some(w) = weak.upgrade() {
                w.set_ssh_algorithm_category(cat.clone());
                let current = store.borrow().algorithm_preferences();
                w.set_ssh_algorithm_rows(ssh_algorithm_row_model(&current, cat.as_str()));
            }
        });
    }
    {
        let store = store.clone();
        let prefs = ssh_algorithm_prefs.clone();
        let weak = window.as_weak();
        window.on_toggle_ssh_algorithm(move |name: SharedString| {
            if let Some(w) = weak.upgrade() {
                let cat = AlgorithmCategory::from_str_key(w.get_ssh_algorithm_category().as_str())
                    .unwrap_or(AlgorithmCategory::Kex);
                let mut s = store.borrow_mut();
                let mut current = s.algorithm_preferences();
                crate::ssh::toggle_algorithm(&mut current, cat, name.as_str());
                s.set_algorithm_preferences(current.clone());
                if let Ok(mut g) = prefs.lock() {
                    *g = current.clone();
                }
                w.set_ssh_algorithm_rows(ssh_algorithm_row_model(&current, cat.as_str()));
            }
        });
    }
    {
        let store = store.clone();
        let prefs = ssh_algorithm_prefs.clone();
        let weak = window.as_weak();
        window.on_move_ssh_algorithm(move |name: SharedString, delta: i32| {
            if let Some(w) = weak.upgrade() {
                let cat = AlgorithmCategory::from_str_key(w.get_ssh_algorithm_category().as_str())
                    .unwrap_or(AlgorithmCategory::Kex);
                let mut s = store.borrow_mut();
                let mut current = s.algorithm_preferences();
                crate::ssh::move_algorithm(&mut current, cat, name.as_str(), delta);
                s.set_algorithm_preferences(current.clone());
                if let Ok(mut g) = prefs.lock() {
                    *g = current.clone();
                }
                w.set_ssh_algorithm_rows(ssh_algorithm_row_model(&current, cat.as_str()));
            }
        });
    }
    {
        let store = store.clone();
        let prefs = ssh_algorithm_prefs.clone();
        let weak = window.as_weak();
        window.on_reset_ssh_algorithm_section(move || {
            if let Some(w) = weak.upgrade() {
                let cat = AlgorithmCategory::from_str_key(w.get_ssh_algorithm_category().as_str())
                    .unwrap_or(AlgorithmCategory::Kex);
                let mut s = store.borrow_mut();
                let mut current = s.algorithm_preferences();
                crate::ssh::reset_algorithm_section(&mut current, cat);
                s.set_algorithm_preferences(current.clone());
                if let Ok(mut g) = prefs.lock() {
                    *g = current.clone();
                }
                w.set_ssh_algorithm_rows(ssh_algorithm_row_model(&current, cat.as_str()));
            }
        });
    }
    {
        let store = store.clone();
        let prefs = ssh_algorithm_prefs.clone();
        let weak = window.as_weak();
        window.on_reset_ssh_algorithms(move || {
            if let Some(w) = weak.upgrade() {
                let mut s = store.borrow_mut();
                let mut current = s.algorithm_preferences();
                crate::ssh::reset_all_algorithms(&mut current);
                s.set_algorithm_preferences(current.clone());
                if let Ok(mut g) = prefs.lock() {
                    *g = current.clone();
                }
                let cat = w.get_ssh_algorithm_category();
                w.set_ssh_algorithm_rows(ssh_algorithm_row_model(&current, cat.as_str()));
            }
        });
    }

    // Interface setting: always ask where to save on download (#87). Read live
    // by the download handler from the window property, so just set + persist.
    window.set_download_always_ask(store.borrow().download_always_ask());
    window.set_paste_confirm_enabled(store.borrow().paste_confirm_enabled());
    window.set_extra_paste_shortcuts_enabled(store.borrow().extra_paste_shortcuts_enabled());
    window.set_select_copy_right_paste_enabled(store.borrow().select_copy_right_paste_enabled());
    window.set_zen_mode(store.borrow().zen_mode());
    window.set_confirm_delete_group_enabled(store.borrow().confirm_delete_group());
    window.set_confirm_delete_session_enabled(store.borrow().confirm_delete_session());
    window.set_welcome_single_click_connect(store.borrow().welcome_single_click_connect());
    window.set_save_passwords(store.borrow().save_passwords());
    window.set_credentials_vault_available(crate::config::is_encryption_available());
    {
        let store = store.clone();
        window.on_set_download_always_ask(move |ask| {
            let mut s = store.borrow_mut();
            s.set_download_always_ask(ask);
        });
    }
    {
        let store = store.clone();
        window.on_set_paste_confirm_enabled(move |enabled| {
            let mut s = store.borrow_mut();
            s.set_paste_confirm_enabled(enabled);
        });
    }
    {
        let store = store.clone();
        window.on_set_extra_paste_shortcuts_enabled(move |enabled| {
            let mut s = store.borrow_mut();
            s.set_extra_paste_shortcuts_enabled(enabled);
        });
    }
    {
        let store = store.clone();
        window.on_set_select_copy_right_paste_enabled(move |enabled| {
            let mut s = store.borrow_mut();
            s.set_select_copy_right_paste_enabled(enabled);
        });
    }
    {
        let store = store.clone();
        window.on_set_zen_mode(move |enabled| {
            let mut s = store.borrow_mut();
            s.set_zen_mode(enabled);
        });
    }
    {
        let store = store.clone();
        window.on_set_confirm_delete_group_enabled(move |enabled| {
            let mut s = store.borrow_mut();
            s.set_confirm_delete_group(enabled);
        });
    }
    {
        let store = store.clone();
        window.on_set_confirm_delete_session_enabled(move |enabled| {
            let mut s = store.borrow_mut();
            s.set_confirm_delete_session(enabled);
        });
    }
    {
        window.on_clear_known_hosts(|| {
            if let Err(err) = crate::ssh::known_hosts::clear() {
                tracing::warn!("failed to clear known_hosts: {err:#}");
            }
            // Also drop this-run accepts so reconnects re-prompt immediately.
            clear_hostkey_decisions();
        });
    }
    {
        let store = store.clone();
        window.on_set_welcome_single_click_connect(move |enabled| {
            let mut s = store.borrow_mut();
            s.set_welcome_single_click_connect(enabled);
        });
    }
    {
        let weak = window.as_weak();
        let store = store.clone();
        window.on_set_save_passwords(move |enabled| {
            let mut s = store.borrow_mut();
            s.set_save_passwords(enabled);
            if let Some(w) = weak.upgrade() {
                w.set_save_passwords(s.save_passwords());
                w.set_credentials_vault_available(crate::config::is_encryption_available());
            }
        });
    }
    {
        let store = store.clone();
        window.on_clear_saved_passwords(move || {
            let mut s = store.borrow_mut();
            s.clear_saved_passwords_and_keys();
            s.save_later(SaveKind::sessions_and_vault());
        });
    }

    // Interface setting: collapse panels by default (#78). Seed the
    // checkboxes, apply the collapsed state once at startup, and persist toggles.
    {
        let s = store.borrow();
        let collapse_sftp = s.collapse_sftp_default();
        let welcome_as_sidebar = s.welcome_as_sidebar();
        let quick_commands_as_sidebar = s.quick_commands_as_sidebar();
        let quick_panel_open = quick_commands_as_sidebar && s.quick_panel_open();
        let quick_panel_collapsed = s.quick_panel_collapsed();
        let quick_panel_dock = s.quick_panel_dock();
        let welcome_sidebar_dock = s.welcome_sidebar_dock();
        let mut welcome_collapsed = s.welcome_collapsed().unwrap_or(false);
        if quick_panel_open
            && !quick_panel_collapsed
            && welcome_as_sidebar
            && welcome_sidebar_dock == quick_panel_dock
        {
            welcome_collapsed = true;
        }
        window.set_collapse_sftp_default(collapse_sftp);
        // Restore the persisted panel docking layout (#dock).
        window.set_sftp_panel_width(s.sftp_panel_width());
        window.set_sftp_panel_height(s.sftp_panel_height());
        window.set_sftp_tree_width(s.sftp_tree_width());
        window.set_sftp_dock(s.sftp_dock().into());
        window.set_quick_commands_as_sidebar(quick_commands_as_sidebar);
        window.set_quick_panel_open(quick_panel_open);
        window.set_quick_panel_collapsed(quick_panel_collapsed);
        window.set_quick_panel_width(s.quick_panel_width());
        window.set_quick_panel_height(s.quick_panel_height());
        window.set_quick_panel_dock(quick_panel_dock.into());
        window.set_welcome_as_sidebar(welcome_as_sidebar);
        window.set_welcome_sidebar_width(s.welcome_sidebar_width());
        window.set_welcome_sidebar_dock(welcome_sidebar_dock.into());
        window.set_welcome_collapsed(welcome_collapsed);
        window.set_welcome_session_col_name(s.welcome_session_col_name());
        window.set_welcome_session_col_host(s.welcome_session_col_host());
        window.set_wallpaper_overlay(s.wallpaper_overlay());
        window.set_update_check_enabled(s.update_check_enabled()); // #184
        window.set_ssh_keepalive_secs(s.ssh_keepalive_secs() as i32);
        window.set_save_passwords(s.save_passwords());
        if collapse_sftp {
            window.set_sftp_collapsed(true);
            window.set_sftp_saved_height(s.sftp_panel_height());
        }
        // Capture the user's preferred size. The first native Resized event
        // drives restoration below; this is deterministic and avoids guessing
        // how long Slint/window-manager initialization takes (#278).
        let (ww, wh) = s.window_size();
        let preferred = (ww > 0.0 && wh > 0.0).then_some((ww, wh));
        pending_window_size_restore.set(preferred);
    }
    {
        let store = store.clone();
        window.on_set_quick_commands_as_sidebar(move |v| {
            let mut s = store.borrow_mut();
            s.set_quick_commands_as_sidebar(v);
        });
    }
    {
        // Toggle the startup new-version check (#184). Takes effect next launch
        // for the check itself; the banner just won't appear once it's off.
        let store = store.clone();
        window.on_set_update_check_enabled(move |v| {
            let mut s = store.borrow_mut();
            s.set_update_check_enabled(v);
        });
    }
    {
        // Renderer selection is consumed before the first native window exists,
        // so persist it now and apply it on the next launch (#280).
        let store = store.clone();
        window.on_set_renderer_mode(move |mode: SharedString| {
            let mut s = store.borrow_mut();
            s.set_renderer_mode(mode.to_string());
        });
    }
    {
        let store = store.clone();
        window.on_persist_welcome_sidebar_width(move |w| {
            let mut s = store.borrow_mut();
            s.set_welcome_sidebar_width(w);
            s.save_later(SaveKind::UI);
        });
    }
    {
        let store = store.clone();
        window.on_persist_welcome_sidebar_dock(move |dock| {
            let mut s = store.borrow_mut();
            s.set_welcome_sidebar_dock(dock.to_string());
            s.save_later(SaveKind::UI);
        });
    }
    {
        let store = store.clone();
        window.on_set_welcome_collapsed(move |v| {
            let mut s = store.borrow_mut();
            s.set_welcome_collapsed(v);
            s.save_later(SaveKind::UI);
        });
    }
    {
        let store = store.clone();
        window.on_persist_wallpaper_overlay(move |v| {
            let mut s = store.borrow_mut();
            s.set_wallpaper_overlay(v);
        });
    }
    {
        let store = store.clone();
        window.on_set_collapse_sftp_default(move |v| {
            let mut s = store.borrow_mut();
            s.set_collapse_sftp_default(v);
        });
    }

    {
        let weak = window.as_weak();
        let store = store.clone();
        window.on_set_term_cursor_color(move |value: SharedString| {
            let Some(color) = parse_hex_color(value.as_str()) else {
                return false;
            };
            {
                let mut s = store.borrow_mut();
                if !s.set_terminal_cursor_color(value.as_str()) {
                    return false;
                }
            }
            if let Some(w) = weak.upgrade() {
                w.set_term_cursor_color(color);
            }
            true
        });
    }
    {
        let weak = window.as_weak();
        window.on_set_highlight_draft_color(move |value: SharedString| {
            let Some(color) = parse_hex_color(value.as_str()) else {
                return false;
            };
            let Some(normalized) = crate::config::normalize_hex_color(value.as_str()) else {
                return false;
            };
            let Some(w) = weak.upgrade() else {
                return false;
            };
            w.set_highlight_draft_color_hex(normalized.into());
            w.set_highlight_draft_swatch(color);
            true
        });
    }
    {
        let weak = window.as_weak();
        let store = store.clone();
        let bufs = bufs.clone();
        window.on_add_output_highlight_rule(
            move |name: SharedString,
                  pattern: SharedString,
                  is_regex,
                  case_sensitive,
                  whole_line,
                  color: SharedString| {
                let name = name.trim().to_string();
                let pattern = pattern.trim().to_string();
                let Some(w) = weak.upgrade() else {
                    return false;
                };
                if let Err(message) = validate_output_highlight_rule_name(&name) {
                    w.set_output_highlight_rule_status(message.into());
                    return false;
                }
                if let Err(message) =
                    validate_output_highlight_rule(&pattern, is_regex, case_sensitive)
                {
                    w.set_output_highlight_rule_status(message.into());
                    return false;
                }
                let color = match validate_output_highlight_color(color.as_str()) {
                    Ok(color) => color,
                    Err(message) => {
                        w.set_output_highlight_rule_status(message.into());
                        return false;
                    }
                };
                if store.borrow().output_highlight_rules().len() >= 128 {
                    w.set_output_highlight_rule_status(
                        t("自定义规则最多 128 条", "Custom rules are limited to 128").into(),
                    );
                    return false;
                }
                {
                    let mut s = store.borrow_mut();
                    s.add_output_highlight_rule(OutputHighlightRule {
                        name,
                        pattern,
                        regex: is_regex,
                        case_sensitive,
                        whole_line,
                        color,
                        enabled: true,
                    });
                    w.set_output_highlight_rules(output_highlight_rule_model(&s));
                    apply_custom_output_rules(&w, &bufs, s.output_highlight_rules());
                }
                w.set_output_highlight_rule_status("".into());
                true
            },
        );
    }
    {
        let weak = window.as_weak();
        let store = store.clone();
        let bufs = bufs.clone();
        window.on_update_output_highlight_rule(
            move |index,
                  name: SharedString,
                  pattern: SharedString,
                  is_regex,
                  case_sensitive,
                  whole_line,
                  color: SharedString| {
                let name = name.trim().to_string();
                let pattern = pattern.trim().to_string();
                let index = index.max(0) as usize;
                let Some(w) = weak.upgrade() else {
                    return false;
                };
                if store.borrow().output_highlight_rules().get(index).is_none() {
                    w.set_output_highlight_rule_status(t("规则不存在", "Rule not found").into());
                    return false;
                }
                if let Err(message) = validate_output_highlight_rule_name(&name) {
                    w.set_output_highlight_rule_status(message.into());
                    return false;
                }
                if let Err(message) =
                    validate_output_highlight_rule(&pattern, is_regex, case_sensitive)
                {
                    w.set_output_highlight_rule_status(message.into());
                    return false;
                }
                let color = match validate_output_highlight_color(color.as_str()) {
                    Ok(color) => color,
                    Err(message) => {
                        w.set_output_highlight_rule_status(message.into());
                        return false;
                    }
                };
                {
                    let mut s = store.borrow_mut();
                    s.update_output_highlight_rule(
                        index,
                        OutputHighlightRule {
                            name,
                            pattern,
                            regex: is_regex,
                            case_sensitive,
                            whole_line,
                            color,
                            enabled: true,
                        },
                    );
                    w.set_output_highlight_rules(output_highlight_rule_model(&s));
                    apply_custom_output_rules(&w, &bufs, s.output_highlight_rules());
                }
                w.set_output_highlight_rule_status("".into());
                true
            },
        );
    }
    {
        let weak = window.as_weak();
        let store = store.clone();
        let bufs = bufs.clone();
        window.on_remove_output_highlight_rule(move |index| {
            let Some(w) = weak.upgrade() else { return };
            let mut s = store.borrow_mut();
            s.remove_output_highlight_rule(index.max(0) as usize);
            w.set_output_highlight_rules(output_highlight_rule_model(&s));
            apply_custom_output_rules(&w, &bufs, s.output_highlight_rules());
            w.set_output_highlight_rule_status("".into());
        });
    }
    {
        let weak = window.as_weak();
        let store = store.clone();
        let bufs = bufs.clone();
        window.on_set_output_highlight_rule_enabled(move |index, enabled| {
            let Some(w) = weak.upgrade() else { return };
            let mut s = store.borrow_mut();
            s.set_output_highlight_rule_enabled(index.max(0) as usize, enabled);
            w.set_output_highlight_rules(output_highlight_rule_model(&s));
            apply_custom_output_rules(&w, &bufs, s.output_highlight_rules());
        });
    }
    // Interface settings: apply + persist the terminal font family / size.
    {
        let weak = window.as_weak();
        let store = store.clone();
        window.on_set_term_font(move |family: SharedString| {
            {
                let mut s = store.borrow_mut();
                s.set_font_family(family.to_string());
            }
            if let Some(w) = weak.upgrade() {
                w.set_term_font_family(family);
            }
        });
    }
    // Output highlighting: persist the switch/preset and immediately rebuild
    // every open terminal, including scrollback captured before the change.
    {
        let weak = window.as_weak();
        let store = store.clone();
        let bufs = bufs.clone();
        window.on_set_output_highlight(move |enabled, preset: SharedString| {
            let preset = preset.to_string();
            {
                let mut s = store.borrow_mut();
                s.set_output_highlight_enabled(enabled);
                s.set_output_highlight_preset(preset.clone());
            }
            if let Some(w) = weak.upgrade() {
                apply_output_highlight(&w, &bufs, enabled, &preset);
            }
        });
    }
    {
        let store = store.clone();
        let bufs = bufs.clone();
        window.on_set_json_format_output(move |enabled| {
            {
                let mut settings = store.borrow_mut();
                settings.set_json_format_output(enabled);
            }
            for buffer in bufs.lock().unwrap().values() {
                buffer.lock().unwrap().json_format_output = enabled;
            }
        });
    }
    {
        let weak = window.as_weak();
        let store = store.clone();
        window.on_set_term_font_size(move |size: i32| {
            {
                let mut s = store.borrow_mut();
                s.set_font_size(size as u32);
            }
            if let Some(w) = weak.upgrade() {
                w.set_term_font_size(size as f32);
            }
        });
    }
    {
        let store = store.clone();
        window.on_persist_sftp_tree_width(move |width| {
            let mut s = store.borrow_mut();
            s.set_sftp_tree_width(width);
            s.save_later(SaveKind::UI);
        });
    }
    {
        let store = store.clone();
        window.on_persist_welcome_session_cols(move |name, host| {
            let mut s = store.borrow_mut();
            s.set_welcome_session_col_name(name);
            s.set_welcome_session_col_host(host);
            s.save_later(SaveKind::UI);
        });
    }
    {
        let weak = window.as_weak();
        let store = store.clone();
        window.on_set_terminal_line_spacing(move |spacing: f32| {
            let normalized = {
                let mut s = store.borrow_mut();
                s.set_terminal_line_spacing(spacing);
                s.terminal_line_spacing()
            };
            if let Some(w) = weak.upgrade() {
                w.set_terminal_line_spacing(normalized);
            }
        });
    }
    {
        let weak = window.as_weak();
        let store = store.clone();
        window.on_set_term_font_bold(move |bold: bool| {
            {
                let mut s = store.borrow_mut();
                s.set_terminal_bold(bold);
            }
            if let Some(w) = weak.upgrade() {
                w.set_term_font_bold(bold);
            }
        });
    }
    {
        let weak = window.as_weak();
        let store = store.clone();
        window.on_set_term_cursor_style(move |style: SharedString| {
            let normalized = {
                let mut s = store.borrow_mut();
                s.set_terminal_cursor_style(style.to_string());
                s.terminal_cursor_style().to_string()
            };
            if let Some(w) = weak.upgrade() {
                w.set_term_cursor_style(normalized.into());
            }
        });
    }
    // Global UI scale (#100): persist the percent and apply it live.
    {
        let weak = window.as_weak();
        let store = store.clone();
        window.on_set_ui_scale(move |percent: i32| {
            let clamped = (percent.max(0) as u32).clamp(80, 200);
            {
                let mut s = store.borrow_mut();
                s.set_ui_scale(clamped);
            }
            if let Some(w) = weak.upgrade() {
                w.set_ui_scale(clamped as f32 / 100.0);
            }
        });
    }
    {
        let weak = window.as_weak();
        let store = store.clone();
        window.on_set_panel_font(move |percent: i32| {
            let clamped = (percent.max(0) as u32).clamp(80, 160);
            {
                let mut s = store.borrow_mut();
                s.set_panel_font(clamped);
            }
            if let Some(w) = weak.upgrade() {
                w.set_panel_font(clamped as f32 / 100.0);
            }
        });
    }

    // Wallpaper: pick a built-in / none, or open the file dialog for a custom one.
    {
        let weak = window.as_weak();
        let store = store.clone();
        let bufs_wp = bufs.clone();
        window.on_set_wallpaper(move |id: SharedString| {
            let id = id.to_string();
            let mut selected_builtin_theme = None;
            if let Some(w) = weak.upgrade() {
                apply_wallpaper(&w, &store.borrow(), &bufs_wp, &id, true);
                if crate::wallpaper::is_builtin(&id) {
                    selected_builtin_theme = Some(w.get_dark_mode());
                }
            }
            let mut s = store.borrow_mut();
            s.set_wallpaper(id);
            // Choosing a built-in wallpaper applies its recommended palette once;
            // Save persists that so it survives the next launch. A later
            // manual theme toggle will overwrite this preference as expected.
            if let Some(dark) = selected_builtin_theme {
                s.set_theme_pref(if dark { "dark" } else { "light" }.to_string());
            }
        });
    }
    {
        let weak = window.as_weak();
        let store = store.clone();
        let bufs_wp = bufs.clone();
        window.on_pick_wallpaper_file(move || {
            let picked = rfd::FileDialog::new()
                .set_title("选择壁纸 / Choose wallpaper")
                .add_filter("Images", &["png", "jpg", "jpeg", "webp", "bmp"])
                .pick_file();
            if let Some(path) = picked {
                let id = path.to_string_lossy().to_string();
                if let Some(w) = weak.upgrade() {
                    apply_wallpaper(&w, &store.borrow(), &bufs_wp, &id, false);
                }
                let mut s = store.borrow_mut();
                s.set_wallpaper(id);
            }
        });
    }

    (sftp_follow_cd, ssh_keepalive_secs, ssh_algorithm_prefs, session_logs)
}
