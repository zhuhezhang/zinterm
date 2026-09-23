use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) fn wire_main_surface(
    window: &AppWindow,
    store: Rc<RefCell<ConfigStore>>,
    bufs: TermBuffers,
    handles: Rc<RefCell<HashMap<String, SessionHandle>>>,
    sftp_handles: SftpHandles,
    sftp_last_cwd: SftpLastCwd,
    render_gates: RenderGates,
    runtime: Arc<Runtime>,
    last_term_size: Arc<Mutex<(u32, u32)>>,
    sftp_follow_cd: Arc<std::sync::atomic::AtomicBool>,
    ssh_keepalive_secs: Arc<std::sync::atomic::AtomicU32>,
    ssh_algorithm_prefs: Arc<std::sync::Mutex<crate::config::AlgorithmPreferences>>,
) {
    let sessions_model: Rc<VecModel<SessionInfo>> = Rc::new(VecModel::default());
    let welcome_session_query: Rc<RefCell<String>> = Rc::new(RefCell::new(String::new()));
    window.set_sessions(ModelRc::from(sessions_model.clone()));
    sync_welcome_sessions(
        &store.borrow(),
        &sessions_model,
        &welcome_session_query.borrow(),
    );
    {
        let weak = window.as_weak();
        let store = store.clone();
        let sessions_model = sessions_model.clone();
        let welcome_session_query = welcome_session_query.clone();
        window.on_clear_all_sessions(move |clear_credentials| {
            {
                let mut s = store.borrow_mut();
                s.clear_sessions_and_groups(clear_credentials);
                let mut kinds = SaveKind::SESSIONS | SaveKind::UI;
                if clear_credentials {
                    kinds |= SaveKind::VAULT;
                }
                s.save_later(kinds);
            }
            sync_welcome_sessions(
                &store.borrow(),
                &sessions_model,
                &welcome_session_query.borrow(),
            );
            if let Some(w) = weak.upgrade() {
                let _ = w.get_sessions();
            }
        });
    }
    {
        let store = store.clone();
        let sessions_model = sessions_model.clone();
        let welcome_session_query = welcome_session_query.clone();
        window.on_search_welcome_sessions(move |query: SharedString| {
            *welcome_session_query.borrow_mut() = query.to_string();
            sync_welcome_sessions(
                &store.borrow(),
                &sessions_model,
                &welcome_session_query.borrow(),
            );
        });
    }

    let tabs_model: Rc<VecModel<TabInfo>> = Rc::new(VecModel::default());
    tabs_model.push(TabInfo {
        id: "welcome".into(),
        title: t("欢迎页", "Welcome page").into(),
        kind: "welcome".into(),
        conn_kind: "".into(),
        endpoint: "".into(),
        connected: false,
        conn_state: 0,
        backspace_mode: "auto".into(),
    });
    window.set_tabs(ModelRc::from(tabs_model.clone()));
    window.set_active_tab_id("welcome".into());

    // Settings panel: preference edits preview live; disk write waits for Save.
    let settings_snapshot: Rc<RefCell<Option<crate::config::ConfigFile>>> =
        Rc::new(RefCell::new(None));
    {
        let weak = window.as_weak();
        let store = store.clone();
        let bufs = bufs.clone();
        let sftp_follow_cd = sftp_follow_cd.clone();
        let ssh_keepalive_secs = ssh_keepalive_secs.clone();
        let ssh_algorithm_prefs = ssh_algorithm_prefs.clone();
        let tabs_model = tabs_model.clone();
        window.on_restore_settings_defaults(move || {
            {
                let mut s = store.borrow_mut();
                s.restore_settings_defaults();
            }
            let Some(w) = weak.upgrade() else {
                return;
            };
            apply_settings_prefs_to_window(
                &w,
                &store.borrow(),
                &bufs,
                &sftp_follow_cd,
                &ssh_keepalive_secs,
                &ssh_algorithm_prefs,
                &tabs_model,
            );
            // Preview only — disk write waits for Save / Save and close.
            // Keep the open-panel snapshot so Cancel can undo the restore.
        });
    }

    // Export Settings-panel preferences to a portable JSON file.
    {
        let weak = window.as_weak();
        let store = store.clone();
        window.on_export_settings(move || {
            if let Some(path) = rfd::FileDialog::new()
                .set_file_name(
                    chrono::Local::now()
                        .format("zinterm-settings-%Y%m%d-%H%M%S.json")
                        .to_string(),
                )
                .add_filter("JSON", &["json"])
                .save_file()
            {
                let res = store.borrow().export_settings_to(&path);
                if let Some(w) = weak.upgrade() {
                    let hint = match res {
                        Ok(()) => {
                            if crate::i18n::is_en() {
                                "Successfully exported settings".to_string()
                            } else {
                                "已成功导出设置".to_string()
                            }
                        }
                        Err(e) => format!("{}: {}", t("导出失败", "export failed"), e),
                    };
                    w.set_ssh_import_hint(hint.into());
                }
            }
        });
    }

    let terminals_model: Rc<VecModel<TerminalState>> = Rc::new(VecModel::default());
    window.set_terminals(ModelRc::from(terminals_model.clone()));

    // Split-pane layout tree (v0.5). Starts as a single pane owning the welcome
    // tab; tab opens/closes/moves mutate it and re-flatten into the `panes`
    // model. `content_size` is the pane-area px size reported from Slint.
    // In welcome-as-sidebar mode the session list lives in a left panel, so the
    // layout starts empty (no "welcome" tab); otherwise it owns the welcome tab.
    let welcome_sidebar = store.borrow().welcome_as_sidebar();
    let layout: Rc<RefCell<crate::layout::Layout>> = Rc::new(RefCell::new(if welcome_sidebar {
        crate::layout::Layout::new(Vec::new(), String::new())
    } else {
        crate::layout::Layout::new(vec!["welcome".into()], "welcome".into())
    }));
    let content_size: Rc<std::cell::Cell<(f32, f32)>> =
        Rc::new(std::cell::Cell::new((1200.0, 800.0)));
    // Persistent pane / splitter models. refresh_panes updates these IN PLACE so
    // the rendered `for pane` / `for sp` elements are reused (terminals survive,
    // and the splitter keeps its pointer-grab during a drag).
    let panes_model: Rc<VecModel<PaneInfo>> = Rc::new(VecModel::default());
    window.set_panes(ModelRc::from(panes_model.clone()));
    let splitters_model: Rc<VecModel<SplitterInfo>> = Rc::new(VecModel::default());
    window.set_splitters(ModelRc::from(splitters_model.clone()));
    refresh_panes(
        window,
        &layout.borrow(),
        content_size.get(),
        &tabs_model,
        &panes_model,
        &splitters_model,
    );
    {
        let weak = window.as_weak();
        let layout = layout.clone();
        let content_size = content_size.clone();
        let tabs_model = tabs_model.clone();
        let panes_model = panes_model.clone();
        let splitters_model = splitters_model.clone();
        window.on_content_resized(move |w: f32, h: f32| {
            let next = (w.max(1.0), h.max(1.0));
            if content_size.get() == next {
                return;
            }
            content_size.set(next);
            if let Some(win) = weak.upgrade() {
                refresh_panes(
                    &win,
                    &layout.borrow(),
                    content_size.get(),
                    &tabs_model,
                    &panes_model,
                    &splitters_model,
                );
            }
        });
    }
    // Toggle welcome-as-sidebar at runtime: persist, then move the welcome tab in
    // or out of the split-tree (sidebar mode = no welcome tab) and re-flatten.
    {
        let weak = window.as_weak();
        let store = store.clone();
        let layout = layout.clone();
        let content_size = content_size.clone();
        let tabs_model = tabs_model.clone();
        let panes_model = panes_model.clone();
        let splitters_model = splitters_model.clone();
        window.on_set_welcome_as_sidebar(move |v| {
            // The property is two-way-bound through InterfacePanel and changing
            // it destroys/recreates the Welcome subtree that owns the Switch.
            // Defer the *entire* transition until its callback has returned;
            // deferring only refresh_panes still destroys the component tree
            // recursively on Windows (#323).
            let weak = weak.clone();
            let store = store.clone();
            let layout = layout.clone();
            let content_size = content_size.clone();
            let tabs_model = tabs_model.clone();
            let panes_model = panes_model.clone();
            let splitters_model = splitters_model.clone();
            slint::Timer::single_shot(std::time::Duration::ZERO, move || {
                if let Some(w) = weak.upgrade() {
                    w.set_welcome_as_sidebar(v);
                    {
                        let mut s = store.borrow_mut();
                        s.set_welcome_as_sidebar(v);
                    }
                    {
                        let mut lay = layout.borrow_mut();
                        update_welcome_tab(&mut lay, v);
                    }
                    refresh_panes(
                        &w,
                        &layout.borrow(),
                        content_size.get(),
                        &tabs_model,
                        &panes_model,
                        &splitters_model,
                    );
                }
            });
        });
    }
    // Per-session SFTP state: collapse + sizes live in each tab's TerminalState so
    // split panes / other tabs each keep their own (resizing/collapsing one no
    // longer bleeds onto the rest) (#v0.5).
    {
        let terminals_model = terminals_model.clone();
        window.on_set_pane_sftp_collapsed(move |tab_id: SharedString, v: bool| {
            update_terminal_row(&terminals_model, &tab_id, |r| r.sftp_collapsed = v);
        });
    }
    {
        let terminals_model = terminals_model.clone();
        let weak = window.as_weak();
        window.on_set_pane_sftp_height(move |tab_id: SharedString, v: f32| {
            update_terminal_row(&terminals_model, &tab_id, |r| r.sftp_panel_height = v);
            // Mirror to the global default so it persists (saved on close) and
            // seeds new sessions; other open tabs use their own field, unaffected.
            if let Some(w) = weak.upgrade() {
                w.set_sftp_panel_height(v);
            }
        });
    }
    {
        let terminals_model = terminals_model.clone();
        let weak = window.as_weak();
        window.on_set_pane_sftp_width(move |tab_id: SharedString, v: f32| {
            update_terminal_row(&terminals_model, &tab_id, |r| r.sftp_panel_width = v);
            if let Some(w) = weak.upgrade() {
                w.set_sftp_panel_width(v);
            }
        });
    }
    {
        let terminals_model = terminals_model.clone();
        window.on_set_pane_sftp_saved_height(move |tab_id: SharedString, v: f32| {
            update_terminal_row(&terminals_model, &tab_id, |r| r.sftp_saved_height = v);
        });
    }

    // Per-tab connection state used for reconnect and tab duplicate.
    let tab_statuses: TabStatuses = Arc::new(Mutex::new(HashMap::new()));

    // --- Wire callbacks --------------------------------------------------
    wire_session_callbacks(
        window,
        store.clone(),
        sessions_model.clone(),
        welcome_session_query.clone(),
        tabs_model.clone(),
        terminals_model.clone(),
        layout.clone(),
        content_size.clone(),
        panes_model.clone(),
        splitters_model.clone(),
        handles.clone(),
        bufs.clone(),
        render_gates.clone(),
        runtime.clone(),
        last_term_size.clone(),
        sftp_handles.clone(),
        sftp_last_cwd.clone(),
        tab_statuses.clone(),
        sftp_follow_cd.clone(),
        ssh_keepalive_secs.clone(),
        ssh_algorithm_prefs.clone(),
    );

    // Switch UI language at runtime.  Preference is "auto" / "zh" / "en"
    // (auto follows the OS; non-Chinese OS locales → English).  Static
    // `@tr(...)` text updates live via select_bundled_translation; we also
    // refresh Rust-driven dynamic strings (welcome tab title).
    {
        let weak = window.as_weak();
        let store = store.clone();
        let tabs_model = tabs_model.clone();
        window.on_set_language(move |code| {
            let pref = crate::i18n::normalize_pref(&code).to_string();
            crate::i18n::set_language(&pref);
            {
                let mut s = store.borrow_mut();
                s.set_language(pref.clone());
            }
            // Re-translate the welcome tab's dynamic title.
            for i in 0..tabs_model.row_count() {
                if let Some(mut row) = tabs_model.row_data(i) {
                    if row.id.as_str() == "welcome" {
                        row.title = t("欢迎页", "Welcome page").into();
                        tabs_model.set_row_data(i, row);
                    }
                }
            }
            if let Some(w) = weak.upgrade() {
                w.set_language_pref(pref.into());
                w.set_lang_en(crate::i18n::is_en());
            }
        });
    }

    {
        let store = store.clone();
        let settings_snapshot = settings_snapshot.clone();
        window.on_begin_settings_edit(move || {
            *settings_snapshot.borrow_mut() = Some(store.borrow().snapshot_settings_prefs());
        });
    }
    {
        let store = store.clone();
        let settings_snapshot = settings_snapshot.clone();
        window.on_save_settings(move || {
            let s = store.borrow();
            s.save_later(SaveKind::settings_and_vault());
            *settings_snapshot.borrow_mut() = Some(s.snapshot_settings_prefs());
        });
    }
    {
        let weak = window.as_weak();
        let store = store.clone();
        let bufs = bufs.clone();
        let sftp_follow_cd = sftp_follow_cd.clone();
        let ssh_keepalive_secs = ssh_keepalive_secs.clone();
        let ssh_algorithm_prefs = ssh_algorithm_prefs.clone();
        let tabs_model = tabs_model.clone();
        let layout = layout.clone();
        let content_size = content_size.clone();
        let panes_model = panes_model.clone();
        let splitters_model = splitters_model.clone();
        let settings_snapshot = settings_snapshot.clone();
        window.on_cancel_settings(move || {
            let Some(snap) = settings_snapshot.borrow_mut().take() else {
                return;
            };
            {
                let mut s = store.borrow_mut();
                s.restore_settings_prefs(&snap);
            }
            let Some(w) = weak.upgrade() else {
                return;
            };
            apply_settings_prefs_to_window(
                &w,
                &store.borrow(),
                &bufs,
                &sftp_follow_cd,
                &ssh_keepalive_secs,
                &ssh_algorithm_prefs,
                &tabs_model,
            );
            let welcome_as_sidebar = store.borrow().welcome_as_sidebar();
            {
                let mut lay = layout.borrow_mut();
                update_welcome_tab(&mut lay, welcome_as_sidebar);
            }
            refresh_panes(
                &w,
                &layout.borrow(),
                content_size.get(),
                &tabs_model,
                &panes_model,
                &splitters_model,
            );
        });
    }

    // Import settings from a portable JSON file (overwrite prefs; additive rules).
    {
        let weak = window.as_weak();
        let store = store.clone();
        let bufs = bufs.clone();
        let sftp_follow_cd = sftp_follow_cd.clone();
        let ssh_keepalive_secs = ssh_keepalive_secs.clone();
        let ssh_algorithm_prefs = ssh_algorithm_prefs.clone();
        let tabs_model = tabs_model.clone();
        let layout = layout.clone();
        let content_size = content_size.clone();
        let panes_model = panes_model.clone();
        let splitters_model = splitters_model.clone();
        window.on_import_settings(move || {
            if let Some(path) = rfd::FileDialog::new()
                .add_filter("JSON", &["json"])
                .pick_file()
            {
                let res = store.borrow_mut().import_settings_from(&path);
                if let Some(w) = weak.upgrade() {
                    let hint = match res {
                        Ok(stats) => {
                            apply_settings_prefs_to_window(
                                &w,
                                &store.borrow(),
                                &bufs,
                                &sftp_follow_cd,
                                &ssh_keepalive_secs,
                                &ssh_algorithm_prefs,
                                &tabs_model,
                            );
                            // 主题偏好可能已变，同步暗色模式
                            apply_dark_mode(&w, &bufs, theme_pref_is_dark(&store.borrow()));
                            let welcome_as_sidebar = store.borrow().welcome_as_sidebar();
                            {
                                let mut lay = layout.borrow_mut();
                                update_welcome_tab(&mut lay, welcome_as_sidebar);
                            }
                            refresh_panes(
                                &w,
                                &layout.borrow(),
                                content_size.get(),
                                &tabs_model,
                                &panes_model,
                                &splitters_model,
                            );
                            // 仅预览 — 落盘等 Save / Save and close；保留打开面板时的快照以便 Cancel 撤销
                            if crate::i18n::is_en() {
                                format!(
                                    "Import succeeded - settings updated / added {added} rule(s)/skipped {skipped}",
                                    added = stats.rules_added,
                                    skipped = stats.rules_skipped
                                )
                            } else {
                                format!(
                                    "导入成功 - 设置已更新 / 新增{added}条高亮规则/跳过{skipped}条",
                                    added = stats.rules_added,
                                    skipped = stats.rules_skipped
                                )
                            }
                        }
                        Err(e) => format!("{} - {}", t("导入失败", "import failed"), e),
                    };
                    w.set_ssh_import_hint(hint.into());
                }
            }
        });
    }

    // Theme toggle: flip dark ↔ light, persist the preference, and re-render
    // every open terminal with the new ANSI palette so historical output is
    // also recoloured (not just new output).
    {
        let weak = window.as_weak();
        let store = store.clone();
        let bufs_theme = bufs.clone();
        window.on_toggle_theme(move || {
            let Some(w) = weak.upgrade() else { return };
            let next_dark = !w.get_dark_mode();
            // Flip theme + every terminal buffer + re-render (shared with wallpaper).
            apply_dark_mode(&w, &bufs_theme, next_dark);
            let pref = if next_dark { "dark" } else { "light" };
            let mut s = store.borrow_mut();
            s.set_theme_pref(pref.to_string());
            s.save_later(SaveKind::SETTINGS);
        });
    }

    // Host-key confirmation dialog (#109-5): the user trusts (remember / once)
    // or rejects the presented server key; the decision fans back out to the
    // blocked SSH/SFTP handler(s) and the next queued prompt (if any) is shown.
    {
        let weak = window.as_weak();
        window.on_hostkey_accept(move || {
            if let Some(w) = weak.upgrade() {
                resolve_front_hostkey(&w, crate::ssh::HostKeyDecision::AcceptRemember);
            }
        });
    }
    {
        let weak = window.as_weak();
        window.on_hostkey_accept_once(move || {
            if let Some(w) = weak.upgrade() {
                resolve_front_hostkey(&w, crate::ssh::HostKeyDecision::AcceptOnce);
            }
        });
    }
    {
        let weak = window.as_weak();
        window.on_hostkey_reject(move || {
            if let Some(w) = weak.upgrade() {
                resolve_front_hostkey(&w, crate::ssh::HostKeyDecision::Reject);
            }
        });
    }

    // Connect-time credential prompt (#110): the user supplies the missing
    // username/password/key (or cancels); the answer unblocks the SSH/SFTP auth.
    {
        let weak = window.as_weak();
        window.on_cred_accept(move || {
            if let Some(w) = weak.upgrade() {
                resolve_front_cred(&w, true);
            }
        });
    }
    {
        let weak = window.as_weak();
        window.on_cred_reject(move || {
            if let Some(w) = weak.upgrade() {
                resolve_front_cred(&w, false);
            }
        });
    }
    {
        let weak = window.as_weak();
        window.on_cred_pick_key(move || {
            let mut dialog =
                rfd::FileDialog::new().set_title(t("选择私钥文件", "Choose private key file"));
            #[cfg(not(target_os = "macos"))]
            {
                dialog =
                    dialog.add_filter(t("SSH 私钥", "SSH private keys"), &["ppk", "pem", "key"]);
            }
            if let Some(home) = directories::UserDirs::new().map(|u| u.home_dir().join(".ssh")) {
                if home.is_dir() {
                    dialog = dialog.set_directory(home);
                }
            }
            if let Some(file) = dialog.pick_file() {
                let path = file.to_string_lossy().replace('\\', "/");
                if let Some(w) = weak.upgrade() {
                    w.set_cred_key_inline(path.into());
                }
            }
        });
    }

    // Settings: preset download directory (load + pick + open).
    // Default to the user's Downloads folder so files land somewhere sensible
    // without a prompt; only fall back to "ask every time" if we can't locate it
    // (#85). Persist it on first run so the setting reflects the real path.
    if store.borrow().download_dir().is_empty() {
        if let Some(dl) = directories::UserDirs::new()
            .and_then(|u| u.download_dir().map(|p| p.to_string_lossy().to_string()))
        {
            let mut s = store.borrow_mut();
            s.set_download_dir(dl);
            s.save_later(SaveKind::SETTINGS);
        }
    }
    window.set_download_dir(store.borrow().download_dir().to_string().into());
    {
        let weak = window.as_weak();
        let store = store.clone();
        window.on_pick_download_dir(move || {
            if let Some(folder) = rfd::FileDialog::new().pick_folder() {
                let dir = folder.to_string_lossy().to_string();
                {
                    let mut s = store.borrow_mut();
                    s.set_download_dir(dir.clone());
                    s.save_later(SaveKind::SETTINGS);
                }
                if let Some(w) = weak.upgrade() {
                    w.set_download_dir(dir.into());
                }
            }
        });
    }
    {
        let weak = window.as_weak();
        window.on_open_download_dir(move || {
            let Some(w) = weak.upgrade() else { return };
            let dir = w.get_download_dir().to_string();
            if dir.is_empty() {
                return;
            }
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

    // --- In-app update check (#48) -----------------------------------------
    // "Download" on the banner opens the latest-release page in the browser.
    window.on_open_update_url(move || {
        let url = "https://github.com/zhuhezhang/zinterm/releases/latest";
        #[cfg(windows)]
        let _ = std::process::Command::new("explorer").arg(url).spawn();
        #[cfg(target_os = "macos")]
        let _ = std::process::Command::new("open").arg(url).spawn();
        #[cfg(all(not(windows), not(target_os = "macos")))]
        let _ = std::process::Command::new("xdg-open").arg(url).spawn();
    });
    // The open-source link in the About dialog opens the project page.
    window.on_open_repo(move || {
        let url = "https://github.com/zhuhezhang/zinterm";
        #[cfg(windows)]
        let _ = std::process::Command::new("explorer").arg(url).spawn();
        #[cfg(target_os = "macos")]
        let _ = std::process::Command::new("open").arg(url).spawn();
        #[cfg(all(not(windows), not(target_os = "macos")))]
        let _ = std::process::Command::new("xdg-open").arg(url).spawn();
    });
    // Query the GitHub releases API on a background thread; if a newer version
    // exists, flip the banner on. Best-effort: any network/parse error is
    // silently ignored and the app keeps working on the current version.
    // Skipped entirely when the user turned the check off (#184).
    if store.borrow().update_check_enabled() {
        let weak = window.as_weak();
        std::thread::spawn(move || {
            let body =
                match ureq::get("https://api.github.com/repos/zhuhezhang/zinterm/releases/latest")
                    .set("User-Agent", "zinterm-update-check")
                    .timeout(std::time::Duration::from_secs(8))
                    .call()
                {
                    Ok(resp) => resp.into_string().unwrap_or_default(),
                    Err(_) => return,
                };
            let json: serde_json::Value = match serde_json::from_str(&body) {
                Ok(v) => v,
                Err(_) => return,
            };
            let tag = json["tag_name"].as_str().unwrap_or("").to_string();
            let newer = matches!(
                (parse_version(&tag), parse_version(env!("CARGO_PKG_VERSION"))),
                (Some(latest), Some(cur)) if latest > cur
            );
            if !newer {
                return;
            }
            let _ = weak.upgrade_in_event_loop(move |w| {
                w.set_update_version(tag.into());
                w.set_update_available(true);
            });
        });
    }

    // Transfer records (download/upload progress + history) shown in the popup.
    let transfers_model: Rc<VecModel<TransferInfo>> = Rc::new(VecModel::default());
    window.set_transfers(ModelRc::from(transfers_model.clone()));
    {
        let tm = transfers_model.clone();
        window.on_clear_transfers(move || tm.set_vec(Vec::<TransferInfo>::new()));
    }
    {
        // Cancel a transfer by id. The id is a UUID unique across sessions, so we
        // broadcast to every SFTP handle — only the owning one has it registered
        // and will act on it (#100).
        let sftp_handles = sftp_handles.clone();
        window.on_cancel_transfer(move |id: SharedString| {
            if let Ok(handles) = sftp_handles.lock() {
                for h in handles.values() {
                    h.cancel_transfer(id.to_string());
                }
            }
        });
    }

    // Open-source libraries shown in the About popup.
    {
        let libs: Vec<SharedString> = [
            t("Slint — 图形界面框架 (GUI)", "Slint — GUI framework"),
            t("russh — SSH 协议实现", "russh — SSH protocol"),
            t(
                "russh-sftp — SFTP 文件传输",
                "russh-sftp — SFTP file transfer",
            ),
            t("tokio — 异步运行时", "tokio — async runtime"),
            t(
                "vt100 — 终端 (VT100/xterm) 解析",
                "vt100 — terminal (VT100/xterm) parser",
            ),
            t(
                "serde / serde_json — 配置序列化",
                "serde / serde_json — config serialization",
            ),
            t("arboard — 系统剪贴板", "arboard — system clipboard"),
            t("rfd — 原生文件对话框", "rfd — native file dialogs"),
            t(
                "directories — 配置目录定位",
                "directories — config dir lookup",
            ),
            t("chrono — 日期时间处理", "chrono — date/time handling"),
            t("uuid — 唯一标识符", "uuid — unique identifiers"),
            t(
                "anyhow / thiserror — 错误处理",
                "anyhow / thiserror — error handling",
            ),
            t(
                "tracing / tracing-subscriber — 日志",
                "tracing / tracing-subscriber — logging",
            ),
            t(
                "futures / async-trait — 异步辅助",
                "futures / async-trait — async helpers",
            ),
            t("rand — 随机数", "rand — randomness"),
            t(
                "winresource — Windows 图标/资源嵌入",
                "winresource — Windows icon/resource embedding",
            ),
        ]
        .iter()
        .map(|s| (*s).into())
        .collect();
        window.set_about_libs(ModelRc::from(Rc::new(VecModel::from(libs))));
    }

    wire_tab_callbacks(
        window,
        tabs_model.clone(),
        terminals_model.clone(),
        layout.clone(),
        content_size.clone(),
        panes_model.clone(),
        splitters_model.clone(),
        handles.clone(),
        bufs.clone(),
        render_gates.clone(),
        sftp_handles.clone(),
        sftp_last_cwd.clone(),
    );
    wire_sftp_callbacks(window, sftp_handles.clone(), sftp_last_cwd.clone());
    wire_key_input(
        window,
        handles.clone(),
        bufs.clone(),
        last_term_size.clone(),
        store.clone(),
        tabs_model.clone(),
        ConnectCtx {
            weak: window.as_weak(),
            runtime: runtime.clone(),
            handles: handles.clone(),
            sftp_handles: sftp_handles.clone(),
            sftp_last_cwd: sftp_last_cwd.clone(),
            bufs: bufs.clone(),
            render_gates: render_gates.clone(),
            tab_statuses: tab_statuses.clone(),
            last_term_size: last_term_size.clone(),
            sftp_follow_cd: sftp_follow_cd.clone(),
            ssh_keepalive_secs: ssh_keepalive_secs.clone(),
            ssh_algorithm_prefs: ssh_algorithm_prefs.clone(),
        },
    );
}
