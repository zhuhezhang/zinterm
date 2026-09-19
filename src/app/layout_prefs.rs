use super::*;

pub(super) fn apply_session_group_collapse(
    weak: &slint::Weak<AppWindow>,
    store: &Rc<RefCell<ConfigStore>>,
    sessions_model: &Rc<VecModel<SessionInfo>>,
    search_query: &str,
    mutate: impl FnOnce(&mut ConfigStore),
) {
    {
        let mut store = store.borrow_mut();
        mutate(&mut store);
        store.save_later(SaveKind::UI);
        sync_welcome_sessions(&store, sessions_model, search_query);
    }
    if let Some(w) = weak.upgrade() {
        let _ = w.get_sessions();
    }
}

pub(super) fn save_layout(win: &AppWindow, store: &Rc<RefCell<ConfigStore>>) {
    let scale = win.window().scale_factor().max(0.01);
    let size = win.window().size();
    let w = size.width as f32 / scale;
    let h = size.height as f32 / scale;
    let mut s = store.borrow_mut();
    s.set_sftp_panel_width(win.get_sftp_panel_width());
    s.set_sftp_panel_height(win.get_sftp_panel_height());
    s.set_sftp_dock(win.get_sftp_dock().to_string());
    s.set_quick_panel_open(win.get_quick_panel_open());
    s.set_quick_panel_collapsed(win.get_quick_panel_collapsed());
    s.set_quick_panel_width(win.get_quick_panel_width());
    s.set_quick_panel_height(win.get_quick_panel_height());
    s.set_quick_panel_dock(win.get_quick_panel_dock().to_string());
    s.set_welcome_sidebar_width(win.get_welcome_sidebar_width());
    s.set_welcome_sidebar_dock(win.get_welcome_sidebar_dock().to_string());
    s.set_welcome_collapsed(win.get_welcome_collapsed());
    // A maximized size isn't a useful "preferred" size to restore to, so only
    // remember the windowed size. Ask the native window too, because the Slint
    // property can lag during startup/shutdown on frameless Windows (#234).
    let native_maximized = win
        .window()
        .with_winit_window(|ww| ww.is_maximized())
        .unwrap_or_else(|| win.get_window_maximized());
    let (saved_w, saved_h) = s.window_size();
    if !native_maximized && (saved_w <= 0.0 || saved_h <= 0.0) && w > 200.0 && h > 200.0 {
        // Normal resize events keep this cache current. Only fall back to the
        // close-time geometry for a first run where no valid resize was seen;
        // do not issue a new native resize while the window is shutting down.
        s.set_window_size(w, h);
    }
    let _ = s.save_parts(SaveKind::UI);
    let _ = ConfigStore::flush_persist();
}

/// Every quick-command group name (used to start with all groups collapsed, #55):
/// "default" when any ungrouped command exists, plus explicit quick-groups and any
/// group referenced by a command.
pub(super) fn tabs_eq(a: &ModelRc<TabInfo>, b: &ModelRc<TabInfo>) -> bool {
    if a.row_count() != b.row_count() {
        return false;
    }
    (0..a.row_count()).all(|i| match (a.row_data(i), b.row_data(i)) {
        (Some(x), Some(y)) => x.id == y.id,
        _ => false,
    })
}

/// Find the terminal row with `tab_id`, apply `mutator`, and write it back.
pub(super) fn update_terminal_row(
    model: &VecModel<TerminalState>,
    tab_id: &str,
    mutator: impl FnOnce(&mut TerminalState),
) {
    for i in 0..model.row_count() {
        if let Some(mut row) = model.row_data(i) {
            if row.id.as_str() == tab_id {
                mutator(&mut row);
                model.set_row_data(i, row);
                return;
            }
        }
    }
}

/// Push Settings-panel preferences from `store` onto the window (and live
/// terminal side-effects). Used after Restore defaults and Cancel.
pub(super) fn apply_settings_prefs_to_window(
    w: &AppWindow,
    s: &ConfigStore,
    bufs: &TermBuffers,
    sftp_follow_cd: &Arc<std::sync::atomic::AtomicBool>,
    ssh_keepalive_secs: &Arc<std::sync::atomic::AtomicU32>,
    ssh_algorithm_prefs: &Arc<std::sync::Mutex<crate::config::AlgorithmPreferences>>,
    tabs_model: &VecModel<TabInfo>,
) {
    let lang_pref = s.language().to_string();
    crate::i18n::set_language(&lang_pref);
    crate::i18n::apply_to_slint();
    w.set_language_pref(lang_pref.into());
    w.set_lang_en(crate::i18n::is_en());
    for i in 0..tabs_model.row_count() {
        if let Some(mut row) = tabs_model.row_data(i) {
            if row.id.as_str() == "welcome" {
                row.title = t("欢迎页", "Welcome page").into();
                tabs_model.set_row_data(i, row);
            }
        }
    }

    let fam = s.font_family();
    w.set_term_font_family(if fam.is_empty() {
        "MeatShell Mono".into()
    } else {
        fam.into()
    });
    w.set_term_font_size(s.font_size() as f32);
    w.set_terminal_line_spacing(s.terminal_line_spacing());
    w.set_term_font_bold(s.terminal_bold());
    w.set_term_cursor_style(s.terminal_cursor_style().into());
    let dark = theme_pref_is_dark(s);
    let (hex, color) = if dark {
        ("#D4D4D4", slint::Color::from_rgb_u8(0xD4, 0xD4, 0xD4))
    } else {
        ("#2D2D2F", slint::Color::from_rgb_u8(0x2D, 0x2D, 0x2F))
    };
    if let Some(custom) = parse_hex_color(s.terminal_cursor_color()) {
        w.set_term_cursor_color_hex(s.terminal_cursor_color().into());
        w.set_term_cursor_color(custom);
    } else {
        w.set_term_cursor_color_hex(hex.into());
        w.set_term_cursor_color(color);
    }
    w.set_output_highlight_enabled(s.output_highlight_enabled());
    w.set_json_format_output(s.json_format_output());
    w.set_output_highlight_preset(s.output_highlight_preset().into());
    w.set_output_highlight_rules(output_highlight_rule_model(s));
    w.set_ui_scale(s.ui_scale() as f32 / 100.0);
    w.set_panel_font(s.panel_font() as f32 / 100.0);
    w.set_renderer_mode(s.renderer_mode().into());

    apply_wallpaper(w, s, bufs, s.wallpaper(), false);
    apply_output_highlight(
        w,
        bufs,
        s.output_highlight_enabled(),
        s.output_highlight_preset(),
    );
    apply_custom_output_rules(w, bufs, s.output_highlight_rules());
    for buffer in bufs.lock().unwrap().values() {
        buffer.lock().unwrap().json_format_output = s.json_format_output();
    }

    let follow = s.sftp_follow_cd();
    sftp_follow_cd.store(follow, std::sync::atomic::Ordering::Relaxed);
    w.set_sftp_follow_cd(follow);
    let keepalive = s.ssh_keepalive_secs();
    ssh_keepalive_secs.store(keepalive, std::sync::atomic::Ordering::Relaxed);
    w.set_ssh_keepalive_secs(keepalive as i32);
    let algorithms = s.algorithm_preferences();
    if let Ok(mut g) = ssh_algorithm_prefs.lock() {
        *g = algorithms.clone();
    }
    let cat = w.get_ssh_algorithm_category();
    w.set_ssh_algorithm_rows(ssh_algorithm_row_model(&algorithms, cat.as_str()));

    w.set_download_always_ask(s.download_always_ask());
    w.set_paste_confirm_enabled(s.paste_confirm_enabled());
    w.set_extra_paste_shortcuts_enabled(s.extra_paste_shortcuts_enabled());
    w.set_select_copy_right_paste_enabled(s.select_copy_right_paste_enabled());
    w.set_zen_mode(s.zen_mode());
    w.set_confirm_delete_group_enabled(s.confirm_delete_group());
    w.set_confirm_delete_session_enabled(s.confirm_delete_session());
    w.set_welcome_single_click_connect(s.welcome_single_click_connect());
    w.set_save_passwords(s.save_passwords());
    w.set_credentials_vault_available(crate::config::is_encryption_available());
    w.set_update_check_enabled(s.update_check_enabled());
    w.set_wallpaper_overlay(s.wallpaper_overlay());

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
    w.set_collapse_sftp_default(collapse_sftp);
    // Preference toggles + settings-owned paths/sizes. Other live panel geometry
    // (SFTP/quick docks) stays as layout chrome so Cancel does not undo
    // resizes made outside the Settings prefs snapshot.
    w.set_download_dir(s.download_dir().into());
    w.set_welcome_sidebar_width(s.welcome_sidebar_width());
    w.set_quick_commands_as_sidebar(quick_commands_as_sidebar);
    if !quick_commands_as_sidebar {
        w.set_quick_panel_open(false);
        w.set_quick_panel_collapsed(false);
    } else {
        w.set_quick_panel_open(quick_panel_open);
        w.set_quick_panel_collapsed(quick_panel_collapsed);
    }
    w.set_welcome_as_sidebar(welcome_as_sidebar);
    w.set_welcome_collapsed(welcome_collapsed);
}

pub(super) fn update_welcome_tab(layout: &mut crate::layout::Layout, as_sidebar: bool) {
    if as_sidebar {
        layout.remove_tab("welcome");
    } else if layout.leaf_of_tab("welcome").is_none() {
        layout.add_tab("welcome".into());
    }
}

pub(super) fn refresh_panes(
    window: &AppWindow,
    layout: &crate::layout::Layout,
    content: (f32, f32),
    tabs_model: &VecModel<TabInfo>,
    panes_model: &VecModel<PaneInfo>,
    splitters_model: &VecModel<SplitterInfo>,
) {
    let (cw, ch) = (content.0.max(1.0), content.1.max(1.0));
    let (panes, splits) = layout.flatten(0.0, 0.0, cw, ch);

    let pane_infos: Vec<PaneInfo> = panes
        .iter()
        .map(|p| {
            // Map this pane's tab ids to their TabInfo rows (skipping any not yet
            // in the model).
            let tabs: Vec<TabInfo> = p
                .tabs
                .iter()
                .filter_map(|tid| {
                    (0..tabs_model.row_count()).find_map(|i| {
                        let row = tabs_model.row_data(i)?;
                        (row.id.as_str() == tid.as_str()).then_some(row)
                    })
                })
                .collect();
            // Toolbar icons live in the title bar now, so no tab-row reserve.
            PaneInfo {
                id: p.id as i32,
                x: p.x,
                y: p.y,
                w: p.w,
                h: p.h,
                active_id: p.active.clone().into(),
                focused: p.focused,
                reserve_right: 0.0,
                tabs: ModelRc::from(Rc::new(VecModel::from(tabs))),
            }
        })
        .collect();

    // Update the models IN PLACE rather than replacing them, so the `for pane` /
    // `for sp` elements are reused: this keeps terminals from being recreated on
    // every refresh AND preserves the splitter's pointer-grab during a drag (a
    // fresh model would destroy the element mid-drag and drop the grab). When the
    // structure changes (split/close → different row count) a full rebuild is fine
    // since no drag is in flight.
    if panes_model.row_count() == pane_infos.len() {
        for (i, mut r) in pane_infos.into_iter().enumerate() {
            if let Some(old) = panes_model.row_data(i) {
                // Reuse the existing tab sub-model when the tabs are unchanged so a
                // geometry-only refresh doesn't churn the tab strips.
                let same_tabs = old.id == r.id && tabs_eq(&old.tabs, &r.tabs);
                let unchanged = same_tabs
                    && old.x == r.x
                    && old.y == r.y
                    && old.w == r.w
                    && old.h == r.h
                    && old.active_id == r.active_id
                    && old.focused == r.focused
                    && old.reserve_right == r.reserve_right;
                if same_tabs {
                    r.tabs = old.tabs;
                }
                if unchanged {
                    continue;
                }
            }
            panes_model.set_row_data(i, r);
        }
    } else {
        panes_model.set_vec(pane_infos);
    }

    let split_infos: Vec<SplitterInfo> = splits
        .iter()
        .map(|s| SplitterInfo {
            split_id: s.split_id as i32,
            x: s.x,
            y: s.y,
            w: s.w,
            h: s.h,
            vertical: s.vertical,
        })
        .collect();
    if splitters_model.row_count() == split_infos.len() {
        for (i, r) in split_infos.into_iter().enumerate() {
            let unchanged = splitters_model.row_data(i).is_some_and(|old| {
                old.split_id == r.split_id
                    && old.x == r.x
                    && old.y == r.y
                    && old.w == r.w
                    && old.h == r.h
                    && old.vertical == r.vertical
            });
            if !unchanged {
                splitters_model.set_row_data(i, r);
            }
        }
    } else {
        splitters_model.set_vec(split_infos);
    }

    if let Some(fp) = panes.iter().find(|p| p.focused) {
        if window.get_active_tab_id().as_str() != fp.active.as_str() {
            window.set_active_tab_id(fp.active.clone().into());
        }
        let active = fp.active.as_str();
        let panel_avail = if active.is_empty() || active == "welcome" {
            true
        } else {
            let terms = window.get_terminals();
            (0..terms.row_count())
                .filter_map(|i| terms.row_data(i))
                .find(|t| t.id.as_str() == active)
                .map(|t| t.command_panel_available)
                .unwrap_or(false)
        };
        if window.get_active_command_panel_available() != panel_avail {
            window.set_active_command_panel_available(panel_avail);
        }
    }
}

/// Hit-test a drag point (pane-area coords) to a target pane + drop zone, plus
/// the highlight rect the dropped tab would affect. Zone is one of
/// "tabstrip"/"left"/"right"/"up"/"down"/"center"; `None` when the point is
/// outside every pane. The 30% edge bands trigger a split; the tab strip and
/// middle drop into the pane's tab group.
pub(super) type DragTarget = (u64, &'static str, (f32, f32, f32, f32));

pub(super) fn drag_target(
    layout: &crate::layout::Layout,
    content: (f32, f32),
    x: f32,
    y: f32,
) -> Option<DragTarget> {
    const STRIP: f32 = 36.0;
    const EDGE: f32 = 0.30;
    let (cw, ch) = (content.0.max(1.0), content.1.max(1.0));
    let (panes, _) = layout.flatten(0.0, 0.0, cw, ch);
    let p = panes
        .iter()
        .find(|p| x >= p.x && x < p.x + p.w && y >= p.y && y < p.y + p.h)?;
    let body_top = p.y + STRIP;
    if y < body_top {
        let ix = x.clamp(p.x + 3.0, p.x + p.w - 3.0) - 3.0;
        return Some((p.id, "tabstrip", (ix, p.y + 4.0, 6.0, STRIP - 8.0)));
    }
    let bw = p.w.max(1.0);
    let bh = (p.h - STRIP).max(1.0);
    let rx = (x - p.x) / bw;
    let ry = (y - body_top) / bh;
    let (dl, dr, dt, db) = (rx, 1.0 - rx, ry, 1.0 - ry);
    let m = dl.min(dr).min(dt).min(db);
    let (zone, rect) = if m > EDGE {
        ("center", (p.x, p.y, p.w, p.h))
    } else if m == dl {
        ("left", (p.x, p.y, p.w * 0.5, p.h))
    } else if m == dr {
        ("right", (p.x + p.w * 0.5, p.y, p.w * 0.5, p.h))
    } else if m == dt {
        ("up", (p.x, p.y, p.w, p.h * 0.5))
    } else {
        ("down", (p.x, p.y + p.h * 0.5, p.w, p.h * 0.5))
    };
    Some((p.id, zone, rect))
}

// ---------------------------------------------------------------------------
// Tab callbacks
// ---------------------------------------------------------------------------
