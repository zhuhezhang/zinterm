use super::*;

pub(super) fn session_from_draft(draft: &SessionDraft) -> Session {
    let id = draft.id.to_string();
    let auth = AuthMethod::from_str(draft.auth.as_ref());

    // Login password and key passphrase are separate fields. The editor echoes
    // stored values when opening a session; an empty field means clear (same as
    // the private-key box), not "keep the previous secret".
    let (password, key_passphrase) = match auth {
        AuthMethod::Key => (
            Secret::default(),
            Secret::new(draft.key_passphrase.to_string()),
        ),
        _ => (Secret::new(draft.password.to_string()), Secret::default()),
    };

    // Unified private_key: path or pasted body (classified by content).
    let private_key = if auth == AuthMethod::Key {
        let key_raw = {
            let inline = draft.private_key_inline.trim();
            let path = draft.private_key_path.trim();
            if !inline.is_empty() {
                inline.to_string()
            } else if !path.is_empty() {
                path.to_string()
            } else {
                String::new()
            }
        };
        // Key material is echoed into the dialog when editing; blank means clear.
        if key_raw.is_empty() {
            Secret::default()
        } else if crate::config::looks_like_private_key_content(&key_raw) {
            Secret::new(key_raw)
        } else {
            Secret::new(key_raw.replace('\\', "/"))
        }
    } else {
        Secret::default()
    };
    let kind = crate::config::SessionKind::from_str(draft.kind.as_ref());
    // Auto-name: serial → port label; local → shell/Local; otherwise
    // user@host, or just the host when no username was given (#110).
    let auto_name = match kind {
        crate::config::SessionKind::Serial => {
            format!("{} @{}", draft.serial_port, draft.baud_rate)
        }
        crate::config::SessionKind::Local => {
            let shell = draft.shell.trim();
            if shell.is_empty() {
                t("本地终端", "Local").to_string()
            } else {
                std::path::Path::new(shell)
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or(shell)
                    .to_string()
            }
        }
        _ if draft.user.trim().is_empty() => draft.host.to_string(),
        _ => format!("{}@{}", draft.user, draft.host),
    };
    // Telnet defaults to port 23, SSH to 22; serial/local ignore port.
    let default_port = if kind == crate::config::SessionKind::Telnet {
        23
    } else {
        22
    };
    let mut session = Session {
        id,
        name: if draft.name.is_empty() {
            auto_name
        } else {
            draft.name.to_string()
        },
        host: draft.host.to_string(),
        port: if draft.port <= 0 {
            default_port
        } else {
            draft.port as u16
        },
        user: draft.user.to_string(),
        auth,
        password,
        key_passphrase,
        private_key,
        last_used: None,
        group: draft.group.to_string(),
        kind,
        saved_at: 0,
        serial_port: draft.serial_port.to_string(),
        baud_rate: if draft.baud_rate <= 0 {
            9_600
        } else {
            draft.baud_rate as u32
        },
        data_bits: draft.data_bits as u8,
        stop_bits: draft.stop_bits as u8,
        parity: normalize_parity(&draft.parity),
        flow_control: normalize_flow_control(&draft.flow_control),
        encoding: if draft.encoding.trim().is_empty() {
            "UTF-8".to_string()
        } else {
            draft.encoding.to_string()
        },
        backspace_mode: normalize_backspace_mode(&draft.backspace_mode).to_string(),
        shell: draft.shell.to_string(),
        working_directory: draft.working_directory.to_string(),
        enable_sftp: draft.enable_sftp,
        enable_prompt_setup: draft.enable_prompt_setup,
        enable_command_panel: draft.enable_command_panel,
    };
    session.sanitize_for_kind();
    session
}

/// When Settings › Data › save passwords is off, keep already-stored secrets
/// / key material but do not write newly typed passwords, passphrases, or keys
/// to disk. Clearing a field (empty draft) is intentional and is allowed to
/// wipe the stored secret.
pub(super) fn apply_password_save_policy(
    session: &mut Session,
    draft: &SessionDraft,
    store: &ConfigStore,
    save_passwords: bool,
) {
    if save_passwords {
        return;
    }
    let existing = store.get(&session.id);
    let auth = AuthMethod::from_str(draft.auth.as_ref());
    match auth {
        AuthMethod::Key => {
            // Non-empty typed passphrase stays out of disk; empty means clear.
            if !draft.key_passphrase.is_empty() {
                session.key_passphrase = existing
                    .map(|s| s.key_passphrase.clone())
                    .unwrap_or_default();
            }
        }
        _ => {
            if !draft.password.is_empty() {
                session.password = existing.map(|s| s.password.clone()).unwrap_or_default();
            }
        }
    }
    // Key material only applies to key auth; password sessions must not keep
    // leftover private-key draft text (or restore stale keys from disk).
    if auth != AuthMethod::Key {
        session.private_key = Secret::default();
        return;
    }
    let key_raw = {
        let inline = draft.private_key_inline.trim();
        let path = draft.private_key_path.trim();
        if !inline.is_empty() {
            inline.to_string()
        } else if !path.is_empty() {
            path.to_string()
        } else {
            String::new()
        }
    };
    // Newly entered key material stays out of the on-disk session; an empty
    // field clears. Non-empty typed values fall back to whatever was stored.
    if !key_raw.is_empty() {
        session.private_key = existing.map(|s| s.private_key.clone()).unwrap_or_default();
    }
}

/// Open a terminal tab for `session` (saved or ephemeral) and start connecting.
///
/// When `cred_source_tab` is set (Duplicate connection), copy that tab's
/// in-memory credential cache onto the new tab and prefer it over any
/// disk-saved password.
#[allow(clippy::too_many_arguments)]
pub(super) fn open_session_in_new_tab(
    mut session: Session,
    ctx: &ConnectCtx,
    store: &Rc<RefCell<ConfigStore>>,
    tabs_model: &Rc<VecModel<TabInfo>>,
    terminals_model: &Rc<VecModel<TerminalState>>,
    layout: &Rc<RefCell<crate::layout::Layout>>,
    content_size: &Rc<std::cell::Cell<(f32, f32)>>,
    panes_model: &Rc<VecModel<PaneInfo>>,
    splitters_model: &Rc<VecModel<SplitterInfo>>,
    cred_source_tab: Option<&str>,
) {
    let tab_id = format!("term-{}", uuid::Uuid::new_v4());
    if let Some(src) = cred_source_tab {
        copy_tab_credentials(src, &tab_id);
        apply_cached_credentials_for_reconnect(&mut session, &tab_id);
    }
    let tab_title = session.name.clone();
    let session_id = session.id.clone();

    // Serial / Telnet / Local have no SFTP; SSH only when the session opts in.
    let has_sftp = session.kind == SessionKind::Ssh && session.enable_sftp;
    let has_command_panel = session.enable_command_panel;

    ctx.tab_statuses.lock().unwrap().insert(
        tab_id.clone(),
        TabStatus {
            session_id,
            state: 0,
        },
    );

    // Register tab + terminal state (SFTP fields start empty/loading).
    tabs_model.push(TabInfo {
        id: tab_id.clone().into(),
        title: tab_title.into(),
        kind: "terminal".into(),
        conn_kind: session.kind.as_str().into(),
        endpoint: tab_endpoint(&session).into(),
        connected: false,
        conn_state: 0,
        backspace_mode: normalize_backspace_mode(&session.backspace_mode).into(),
    });
    // Each session keeps its own SFTP collapse state + sizes, seeded from
    // the global defaults (the "collapse SFTP by default" pref and the
    // persisted panel sizes) so they no longer bleed across panes (#v0.5).
    let (sftp_collapsed_default, sftp_h_default, sftp_w_default) = ctx
        .weak
        .upgrade()
        .map(|w| {
            (
                w.get_collapse_sftp_default(),
                w.get_sftp_panel_height(),
                w.get_sftp_panel_width(),
            )
        })
        .unwrap_or((false, 220.0, 380.0));
    terminals_model.push(TerminalState {
        id: tab_id.clone().into(),
        status: t("连接中...", "Connecting...").into(),
        spans: ModelRc::from(std::rc::Rc::new(VecModel::<TermSpan>::default())),
        cursor_row: 0,
        cursor_col: 0,
        rows_used: 0,
        scroll_max: 0,
        scroll_offset: 0,
        is_alt_screen: false,
        find_matches: ModelRc::from(std::rc::Rc::new(VecModel::<TermMatch>::default())),
        selection: ModelRc::from(std::rc::Rc::new(VecModel::<TermMatch>::default())),
        sftp_path: "/".into(),
        sftp_entries: ModelRc::from(std::rc::Rc::new(VecModel::<SftpEntry>::default())),
        sftp_status: if has_sftp {
            t("SFTP 连接中...", "SFTP connecting...").into()
        } else if session.kind == SessionKind::Ssh {
            t("此会话未启用 SFTP", "SFTP is disabled for this session").into()
        } else {
            t(
                "此会话类型不支持 SFTP",
                "SFTP not available for this session",
            )
            .into()
        },
        sftp_loading: has_sftp,
        sftp_tree_nodes: ModelRc::from(std::rc::Rc::new(VecModel::<SftpTreeNode>::default())),
        sftp_selected_count: 0,
        sftp_sort_key: "".into(),
        sftp_sort_dir: 0,
        sftp_available: has_sftp,
        sftp_ready: false,
        sftp_collapsed: !has_sftp || sftp_collapsed_default,
        sftp_panel_height: sftp_h_default,
        sftp_panel_width: sftp_w_default,
        sftp_saved_height: sftp_h_default,
        command_panel_available: has_command_panel,
    });
    // Create vt100 parser for this tab (default 24×80; resized on first
    // terminal-resize callback). 5000-line scrollback is stored for
    // future scroll-navigation support.
    let is_dark_now = ctx
        .weak
        .upgrade()
        .map(|w| w.get_dark_mode())
        .unwrap_or(true);
    let (output_highlight, custom_highlight_rules) = {
        let settings = store.borrow();
        (
            OutputHighlightPreset::from_settings(
                settings.output_highlight_enabled(),
                settings.output_highlight_preset(),
            ),
            compile_output_rules(settings.output_highlight_rules()),
        )
    };
    ctx.bufs.lock().unwrap().insert(
        tab_id.clone(),
        Arc::new(Mutex::new(TermBuffer {
            parser: vt100::Parser::new(24, 80, 5000),
            find_query: String::new(),
            find_options: FindOptions::default(),
            is_dark: is_dark_now,
            output_highlight,
            custom_highlight_rules,
            json_format_output: store.borrow().json_format_output(),
            interactive_echo_until: std::time::Instant::now(),
            sel_anchor: None,
            sel_focus: None,
            sel_ranges: Vec::new(),
            history: VecDeque::new(),
            prev: Vec::new(),
            view_offset: 0,
            displayed_text: Vec::new(),
            csi_state: CsiState::Normal,
            csi_pending: Vec::new(),
            raw: std::collections::VecDeque::new(),
        })),
    );
    ctx.render_gates.lock().unwrap().insert(
        tab_id.clone(),
        Arc::new(TabRenderGate::new(RENDER_MIN_INTERVAL)),
    );
    // No followed-cwd yet: the first OSC 7 always triggers a follow.
    ctx.sftp_last_cwd.lock().unwrap().remove(&tab_id);
    // Add the new tab to the focused pane and re-flatten (this also sets
    // active-tab-id to the new tab via refresh_panes).
    layout.borrow_mut().add_tab(tab_id.clone());
    if let Some(w) = ctx.weak.upgrade() {
        refresh_panes(
            &w,
            &layout.borrow(),
            content_size.get(),
            tabs_model,
            panes_model,
            splitters_model,
        );
    }

    // Spawn the shell (+ SFTP) workers and their event-pump threads.
    // Shared with in-place reconnect (#79) via start_session_in_tab.
    start_session_in_tab(&tab_id, session, ctx);
}

pub(super) fn open_new_session_dialog(
    win: &AppWindow,
    store: &ConfigStore,
    group: &str,
    host: &str,
) {
    win.set_session_groups(session_groups_model(store));
    win.set_dialog_id("".into());
    win.set_dialog_name("".into());
    win.set_dialog_host(host.trim().into());
    win.set_dialog_port("22".into());
    // No default username (#110): leaving it blank makes the connect-time
    // prompt ask for it, Xshell-style.
    win.set_dialog_user("".into());
    win.set_dialog_auth("password".into());
    win.set_dialog_password("".into());
    win.set_dialog_key_passphrase("".into());
    win.set_dialog_key_path("".into());
    win.set_dialog_key_inline("".into());
    win.set_dialog_key_inline_mode(false);
    win.set_dialog_key_saved_inline(false);
    win.set_dialog_group(group.into());
    win.set_dialog_kind("ssh".into());
    win.set_dialog_serial_port("".into());
    win.set_dialog_baud("9600".into());
    win.set_dialog_data_bits("8".into());
    win.set_dialog_stop_bits("1".into());
    win.set_dialog_parity("None".into());
    win.set_dialog_flow("None".into());
    win.set_dialog_encoding("UTF-8".into());
    win.set_dialog_backspace_mode("auto".into());
    win.set_dialog_shell("".into());
    win.set_dialog_working_directory("".into());
    win.set_dialog_enable_sftp(false);
    win.set_dialog_enable_prompt_setup(false);
    win.set_dialog_enable_command_panel(false);
    win.set_dialog_editing(false);
    win.set_dialog_open(true);
}

#[allow(clippy::too_many_arguments)]
pub(super) fn wire_session_dialog(
    window: &AppWindow,
    store: Rc<RefCell<ConfigStore>>,
    sessions_model: Rc<VecModel<SessionInfo>>,
    welcome_session_query: Rc<RefCell<String>>,
    tabs_model: Rc<VecModel<TabInfo>>,
    terminals_model: Rc<VecModel<TerminalState>>,
    layout: Rc<RefCell<crate::layout::Layout>>,
    content_size: Rc<std::cell::Cell<(f32, f32)>>,
    panes_model: Rc<VecModel<PaneInfo>>,
    splitters_model: Rc<VecModel<SplitterInfo>>,
    handles: Rc<RefCell<HashMap<String, SessionHandle>>>,
    bufs: TermBuffers,
    render_gates: RenderGates,
    runtime: Arc<Runtime>,
    last_term_size: Arc<Mutex<(u32, u32)>>,
    sftp_handles: SftpHandles,
    sftp_last_cwd: SftpLastCwd,
    tab_statuses: TabStatuses,
    sftp_follow_cd: Arc<std::sync::atomic::AtomicBool>,
    ssh_keepalive_secs: Arc<std::sync::atomic::AtomicU32>,
    ssh_algorithm_prefs: Arc<std::sync::Mutex<crate::config::AlgorithmPreferences>>,
    session_logs: SessionLoggers,
) {
    // Dialog submit -> persist and/or connect (Save / Connect / Save-and-connect).
    {
        let weak = window.as_weak();
        let store = store.clone();
        let sessions_model = sessions_model.clone();
        let welcome_session_query = welcome_session_query.clone();
        let tab_statuses = tab_statuses.clone();
        let tabs_model = tabs_model.clone();
        let terminals_model = terminals_model.clone();
        let layout = layout.clone();
        let content_size = content_size.clone();
        let panes_model = panes_model.clone();
        let splitters_model = splitters_model.clone();
        let handles = handles.clone();
        let bufs = bufs.clone();
        let render_gates = render_gates.clone();
        let runtime = runtime.clone();
        let last_term_size = last_term_size.clone();
        let sftp_handles = sftp_handles.clone();
        let sftp_last_cwd = sftp_last_cwd.clone();
        let sftp_follow_cd = sftp_follow_cd.clone();
        let ssh_keepalive_secs = ssh_keepalive_secs.clone();
        let ssh_algorithm_prefs = ssh_algorithm_prefs.clone();
        let session_logs = session_logs.clone();
        window.on_session_dialog_submit(
            move |draft: SessionDraft, persist: bool, connect: bool| {
                let mut new_session = session_from_draft(&draft);

                if persist {
                    // Disk copy may strip secrets when save-passwords is off; keep
                    // `new_session` intact so Save-and-connect still authenticates.
                    let mut to_save = new_session.clone();
                    {
                        let s = store.borrow();
                        apply_password_save_policy(&mut to_save, &draft, &s, s.save_passwords());
                    }
                    let saved_id = {
                        let mut s = store.borrow_mut();
                        let id = s.upsert(to_save);
                        clear_session_ephemeral(&id);
                        s.save_later(SaveKind::sessions_and_vault());
                        // Pick up disambiguated name / id / saved_at from the store,
                        // but keep the in-memory secrets / key path for connect.
                        if let Some(saved) = s.get(&id) {
                            let password = new_session.password.clone();
                            let key_passphrase = new_session.key_passphrase.clone();
                            let private_key = new_session.private_key.clone();
                            new_session = saved.clone();
                            new_session.password = password;
                            new_session.key_passphrase = key_passphrase;
                            new_session.private_key = private_key;
                        }
                        id
                    };
                    sync_welcome_sessions(
                        &store.borrow(),
                        &sessions_model,
                        &welcome_session_query.borrow(),
                    );
                    if let Some(w) = weak.upgrade() {
                        let saved_backspace = new_session.backspace_mode.clone();
                        let affected: Vec<String> = tab_statuses
                            .lock()
                            .unwrap()
                            .iter()
                            .filter(|(_, st)| st.session_id == saved_id)
                            .map(|(id, _)| id.clone())
                            .collect();
                        for tid in affected {
                            update_tab_backspace_mode(&w, &tid, &saved_backspace);
                        }
                    }
                } else if new_session.id.trim().is_empty() {
                    // Connect-without-save still needs a stable in-memory id.
                    new_session.id = crate::config::Session::new_temp_id();
                }

                let saved_id = new_session.id.clone();

                if let Some(w) = weak.upgrade() {
                    w.set_dialog_open(false);
                }

                if connect {
                    if !persist {
                        // "Connect without saving": never write prompt answers back.
                        mark_session_ephemeral(&saved_id);
                    }
                    let ctx = ConnectCtx {
                        weak: weak.clone(),
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
                        session_logs: session_logs.clone(),
                    };
                    open_session_in_new_tab(
                        new_session,
                        &ctx,
                        &store,
                        &tabs_model,
                        &terminals_model,
                        &layout,
                        &content_size,
                        &panes_model,
                        &splitters_model,
                        None,
                    );
                }
            },
        );
    }

    // Cancel dialog.
    {
        let weak = window.as_weak();
        window.on_session_dialog_cancel(move || {
            if let Some(w) = weak.upgrade() {
                w.set_dialog_open(false);
            }
        });
    }

    // Private-key file picker: pick the private key and store its path with
    // forward-slash separators (uniform across Windows/Linux; russh accepts them).
    {
        let weak = window.as_weak();
        window.on_session_dialog_pick_key(move || {
            let mut dialog =
                rfd::FileDialog::new().set_title(t("选择私钥文件", "Choose private key file"));
            // OpenSSH's standard macOS key names (id_ed25519, id_rsa, …) have
            // no extension. A native macOS extension filter makes those files
            // visible but disabled, so leave the picker unfiltered there (#325).
            // Other platforms retain the narrower existing filter.
            #[cfg(not(target_os = "macos"))]
            {
                dialog =
                    dialog.add_filter(t("SSH 私钥", "SSH private keys"), &["ppk", "pem", "key"]);
            }
            // Start in ~/.ssh if it exists.
            if let Some(home) = directories::UserDirs::new().map(|u| u.home_dir().join(".ssh")) {
                if home.is_dir() {
                    dialog = dialog.set_directory(home);
                }
            }
            if let Some(file) = dialog.pick_file() {
                let path = file.to_string_lossy().replace('\\', "/");
                if let Some(w) = weak.upgrade() {
                    // Unified key field lives in dialog-key-inline.
                    w.set_dialog_key_inline(path.into());
                    w.set_dialog_key_path("".into());
                    w.set_dialog_key_inline_mode(false);
                    w.set_dialog_key_saved_inline(false);
                }
            }
        });
    }

    // Re-enumerate OS serial ports for the session dialog combo.
    {
        let weak = window.as_weak();
        window.on_session_dialog_refresh_serial_ports(move || {
            if let Some(w) = weak.upgrade() {
                w.set_serial_ports(serial_ports_model());
            }
        });
    }

    // Connect session -> open a new terminal tab.
    {
        let weak = window.as_weak();
        let store = store.clone();
        let tabs_model = tabs_model.clone();
        let terminals_model = terminals_model.clone();
        let layout = layout.clone();
        let content_size = content_size.clone();
        let handles = handles.clone();
        let bufs = bufs.clone();
        let render_gates = render_gates.clone();
        let runtime = runtime.clone();
        let last_term_size = last_term_size.clone();
        let sftp_handles = sftp_handles.clone();
        let sftp_last_cwd = sftp_last_cwd.clone();
        let tab_statuses = tab_statuses.clone();
        let sftp_follow_cd = sftp_follow_cd.clone();
        let ssh_keepalive_secs = ssh_keepalive_secs.clone();
        let ssh_algorithm_prefs = ssh_algorithm_prefs.clone();
        let session_logs = session_logs.clone();
        let panes_model = panes_model.clone();
        let splitters_model = splitters_model.clone();
        window.on_connect_session(move |id: SharedString| {
            let id = id.to_string();
            let session = match store.borrow().get(&id).cloned() {
                Some(s) => s,
                None => return,
            };
            let ctx = ConnectCtx {
                weak: weak.clone(),
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
                session_logs: session_logs.clone(),
            };
            open_session_in_new_tab(
                session,
                &ctx,
                &store,
                &tabs_model,
                &terminals_model,
                &layout,
                &content_size,
                &panes_model,
                &splitters_model,
                None,
            );
        });
    }

    // Duplicate a tab's connection (#v0.5): open a fresh tab to the same saved
    // session, landing in the same pane as the source tab. Copies the source
    // tab's in-memory credential cache (independent entry) and does not use
    // the disk-saved password.
    {
        let weak = window.as_weak();
        let store = store.clone();
        let tab_statuses = tab_statuses.clone();
        let layout = layout.clone();
        let content_size = content_size.clone();
        let tabs_model = tabs_model.clone();
        let terminals_model = terminals_model.clone();
        let handles = handles.clone();
        let bufs = bufs.clone();
        let render_gates = render_gates.clone();
        let runtime = runtime.clone();
        let last_term_size = last_term_size.clone();
        let sftp_handles = sftp_handles.clone();
        let sftp_last_cwd = sftp_last_cwd.clone();
        let sftp_follow_cd = sftp_follow_cd.clone();
        let ssh_keepalive_secs = ssh_keepalive_secs.clone();
        let ssh_algorithm_prefs = ssh_algorithm_prefs.clone();
        let session_logs = session_logs.clone();
        let panes_model = panes_model.clone();
        let splitters_model = splitters_model.clone();
        window.on_tab_duplicate(move |tab_id: SharedString| {
            let source_tab = tab_id.to_string();
            let session_id = tab_statuses
                .lock()
                .unwrap()
                .get(&source_tab)
                .map(|s| s.session_id.clone())
                .unwrap_or_default();
            if session_id.is_empty() {
                return;
            }
            let Some(session) = store.borrow().get(&session_id).cloned() else {
                return;
            };
            // Land the new tab in the same pane as the source. Read the pane id
            // into a local first so the immutable borrow is dropped before the
            // borrow_mut (else RefCell panics on the overlapping borrow).
            let pane = layout.borrow().leaf_of_tab(&source_tab);
            if let Some(pane) = pane {
                layout.borrow_mut().focused = pane;
            }
            let ctx = ConnectCtx {
                weak: weak.clone(),
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
                session_logs: session_logs.clone(),
            };
            open_session_in_new_tab(
                session,
                &ctx,
                &store,
                &tabs_model,
                &terminals_model,
                &layout,
                &content_size,
                &panes_model,
                &splitters_model,
                Some(&source_tab),
            );
        });
    }

    // Live Backspace-mode switch from the tab context submenu: update the
    // saved session, persist, and refresh open-tab checkmarks. send_key reads
    // the store on every keystroke so the new mode applies immediately.
    {
        let weak = window.as_weak();
        let store = store.clone();
        let tab_statuses = tab_statuses.clone();
        window.on_tab_set_backspace_mode(move |tab_id: SharedString, mode: SharedString| {
            let tab_id = tab_id.to_string();
            let mode = normalize_backspace_mode(&mode).to_string();
            let session_id = tab_statuses
                .lock()
                .unwrap()
                .get(&tab_id)
                .map(|s| s.session_id.clone())
                .unwrap_or_default();
            if session_id.is_empty() {
                return;
            }
            {
                let mut s = store.borrow_mut();
                let Some(mut session) = s.get(&session_id).cloned() else {
                    return;
                };
                session.backspace_mode = mode.clone();
                s.upsert(session);
                s.save_later(SaveKind::SESSIONS);
            }
            if let Some(w) = weak.upgrade() {
                // Keep every open tab of this session's checkmark in sync.
                let affected: Vec<String> = tab_statuses
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|(_, st)| st.session_id == session_id)
                    .map(|(id, _)| id.clone())
                    .collect();
                for tid in affected {
                    update_tab_backspace_mode(&w, &tid, &mode);
                }
            }
        });
    }
}
