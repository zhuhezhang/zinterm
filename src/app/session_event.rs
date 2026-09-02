use super::*;

/// Clean / user-initiated closes: muted tab dot. Everything else (auth fail,
/// handshake error, write/read failure, …) paints the failure red.
fn is_normal_session_close(reason: &str) -> bool {
    let reason = reason.trim();
    reason.is_empty()
        || reason == crate::i18n::t("连接已关闭", "connection closed")
        || reason == crate::i18n::t("串口已关闭", "serial port closed")
        || reason == crate::i18n::t("本地终端已退出", "local terminal exited")
        || reason == crate::i18n::t("已取消登录", "login cancelled")
}

pub(super) fn update_tab_connection(
    win: &AppWindow,
    tab_id: &str,
    conn_state: i32,
    connected: bool,
) {
    mutate_tab_info(win, tab_id, &|t| {
        t.conn_state = conn_state;
        t.connected = connected;
    });
}

/// Update `backspace_mode` on a tab (root model + every pane copy) so the
/// context-menu checkmark stays in sync after a live switch or session edit.
pub(super) fn update_tab_backspace_mode(win: &AppWindow, tab_id: &str, mode: &str) {
    let mode: SharedString = normalize_backspace_mode(mode).into();
    mutate_tab_info(win, tab_id, &|t| {
        t.backspace_mode = mode.clone();
    });
}

fn mutate_tab_info(win: &AppWindow, tab_id: &str, mutator: &dyn Fn(&mut TabInfo)) {
    let tabs_rc = win.get_tabs();
    let tabs = tabs_rc
        .as_any()
        .downcast_ref::<VecModel<TabInfo>>()
        .expect("tabs model must be a VecModel");

    for i in 0..tabs.row_count() {
        if let Some(mut row) = tabs.row_data(i) {
            if row.id.as_str() == tab_id {
                mutator(&mut row);
                tabs.set_row_data(i, row);
                break;
            }
        }
    }
    let panes = win.get_panes();
    if let Some(pm) = panes.as_any().downcast_ref::<VecModel<PaneInfo>>() {
        for pi in 0..pm.row_count() {
            let Some(pane) = pm.row_data(pi) else {
                continue;
            };
            let Some(tm) = pane.tabs.as_any().downcast_ref::<VecModel<TabInfo>>() else {
                continue;
            };
            for ti in 0..tm.row_count() {
                if let Some(mut row) = tm.row_data(ti) {
                    if row.id.as_str() == tab_id {
                        mutator(&mut row);
                        tm.set_row_data(ti, row);
                        break;
                    }
                }
            }
        }
    }
}

pub(super) fn apply_session_event_to_window(
    win: &AppWindow,
    tab_id: &str,
    event: SessionEvent,
    bufs: &TermBuffers,
    gates: &RenderGates,
    statuses: &TabStatuses,
) {
    let terminals_rc = win.get_terminals();
    let terminals = terminals_rc
        .as_any()
        .downcast_ref::<VecModel<TerminalState>>()
        .expect("terminals model must be a VecModel");

    let update_terminal = |mutator: &dyn Fn(&mut TerminalState)| {
        for i in 0..terminals.row_count() {
            if let Some(mut row) = terminals.row_data(i) {
                if row.id.as_str() == tab_id {
                    mutator(&mut row);
                    terminals.set_row_data(i, row);
                    break;
                }
            }
        }
    };

    match event {
        SessionEvent::Status(status) => {
            // Connection progress / info goes into the terminal like remote
            // output (no separate status strip). Skip a leading newline when the
            // cursor is already at column 0 so "Connecting..." / "Connected..."
            // don't sit under a blank line on a fresh screen.
            let at_col0 = term_buf(bufs, tab_id)
                .map(|h| h.lock().unwrap().parser.screen().cursor_position().1 == 0)
                .unwrap_or(true);
            let chunk = if at_col0 {
                format!("\x1b[90m{status}\x1b[0m\r\n")
            } else {
                format!("\r\n\x1b[90m{status}\x1b[0m\r\n")
            };
            let _ = ingest_terminal_output(bufs, tab_id, chunk.as_bytes());
            request_tab_render_from_ui(win.as_weak(), tab_id, bufs, gates);
        }
        SessionEvent::Output(chunk) => {
            // Synthetic Output (disconnect hint, editor error, …) — rare, already
            // on the UI thread. Live shell output is ingested on the pump thread.
            let _ = ingest_terminal_output(bufs, tab_id, chunk.as_bytes());
            request_tab_render_from_ui(win.as_weak(), tab_id, bufs, gates);
        }
        SessionEvent::Connected => {
            update_tab_connection(win, tab_id, 1, true);
            if let Some(st) = statuses.lock().unwrap().get_mut(tab_id) {
                st.state = 1;
            }
        }
        SessionEvent::Closed(reason) => {
            // Print disconnect info + reconnect hint into the terminal
            // (FinalShell-style), via synthetic Output (#79).
            let hint = crate::i18n::t(
                "按 R 重新连接",
                "Press R to reconnect",
            );
            let body = if reason.trim().is_empty() {
                hint.to_string()
            } else {
                format!(
                    "{} — {reason}\r\n{hint}",
                    crate::i18n::t("连接已断开", "Disconnected")
                )
            };
            apply_session_event_to_window(
                win,
                tab_id,
                SessionEvent::Output(format!("\r\n\x1b[31m{body}\x1b[0m\r\n")),
                bufs,
                gates,
                statuses,
            );
            // Normal closes (user cancel, clean peer/local exit) stay muted;
            // only connect/auth/IO failures paint the tab dot red.
            let conn_state = if is_normal_session_close(&reason) {
                2
            } else {
                3
            };
            update_tab_connection(win, tab_id, conn_state, false);
            if let Some(st) = statuses.lock().unwrap().get_mut(tab_id) {
                st.state = 2;
            }
        }

        // --- SFTP events ---------------------------------------------------
        SessionEvent::CwdChanged(path) => {
            // Just update the displayed path; the pump thread already sent
            // SftpCommand::ListDir so a SftpEntries event is inbound.
            update_terminal(&|t| {
                t.sftp_path = path.clone().into();
                t.sftp_loading = true;
            });
        }
        SessionEvent::SftpEntries { path, entries } => {
            let mut slint_entries: Vec<SftpEntry> = entries
                .iter()
                .map(|e| SftpEntry {
                    name: e.name.clone().into(),
                    full_path: e.full_path.clone().into(),
                    is_dir: e.is_dir,
                    size: if e.is_dir {
                        "".into()
                    } else {
                        format_size(e.size).into()
                    },
                    size_bytes: e.size as f32,
                    modified: format_mtime(e.modified).into(),
                    modified_ts: e.modified as f32,
                    mode: (e.mode & 0o7777) as i32,
                    selected: false,
                })
                .collect();
            let (sort_key, sort_dir) = (0..terminals.row_count())
                .find_map(|i| {
                    let row = terminals.row_data(i)?;
                    (row.id.as_str() == tab_id)
                        .then(|| (row.sftp_sort_key.to_string(), row.sftp_sort_dir))
                })
                .unwrap_or_default();
            sort_sftp_entries(&mut slint_entries, &sort_key, sort_dir);
            let model = ModelRc::from(std::rc::Rc::new(VecModel::from(slint_entries)));
            update_terminal(&|t| {
                t.sftp_path = path.clone().into();
                t.sftp_entries = model.clone();
                t.sftp_loading = false;
                t.sftp_ready = true;
                // Fresh listings always arrive unselected; keep the toolbar
                // batch-action count in sync so the download/delete/count
                // controls hide after refresh or directory change (#100).
                t.sftp_selected_count = 0;
            });
        }
        SessionEvent::SftpStatus(msg) => {
            update_terminal(&|t| t.sftp_status = msg.clone().into());
        }
        SessionEvent::SftpError(msg) => {
            // Show the reason and stop the spinner; leave the current listing in
            // place so a failed navigation doesn't blank the panel (#112).
            update_terminal(&|t| {
                t.sftp_status = msg.clone().into();
                t.sftp_loading = false;
            });
        }
        SessionEvent::SftpFailed(msg) => {
            // Connection-level failure: keep the bar collapsed / disabled.
            update_terminal(&|t| {
                t.sftp_status = msg.clone().into();
                t.sftp_loading = false;
                t.sftp_ready = false;
                t.sftp_collapsed = true;
            });
        }
        SessionEvent::SftpFileText {
            path,
            name,
            content,
            edit,
            error,
        } => {
            if error.is_empty() {
                // Open the built-in viewer/editor (#70).
                win.set_editor_line_numbers(line_numbers_for(&content).into());
                win.set_editor_path(path.into());
                win.set_editor_name(name.into());
                win.set_editor_content(content.into());
                win.set_editor_readonly(!edit);
                win.set_editor_dirty(false);
                win.set_editor_open(true);
            } else {
                // Couldn't open as text. The SFTP status line alone is easy to
                // miss (looks like "nothing happened"), so also print the reason
                // into the terminal via a synthetic Output event (#70).
                apply_session_event_to_window(
                    win,
                    tab_id,
                    SessionEvent::Output(format!(
                        "\r\n[meatshell] {} {}: {}\r\n",
                        crate::i18n::t("无法打开", "Cannot open"),
                        name,
                        error
                    )),
                    bufs,
                    gates,
                    statuses,
                );
                update_terminal(&|t| t.sftp_status = error.clone().into());
            }
        }
        SessionEvent::SftpTreeUpdate(nodes) => {
            let slint_nodes: Vec<SftpTreeNode> = nodes
                .iter()
                .map(|n| SftpTreeNode {
                    path: n.path.clone().into(),
                    name: n.name.clone().into(),
                    depth: n.depth as i32,
                    expanded: n.expanded,
                    has_children: n.has_children,
                })
                .collect();
            let model = ModelRc::from(std::rc::Rc::new(VecModel::from(slint_nodes)));
            update_terminal(&|t| t.sftp_tree_nodes = model.clone());
        }
        SessionEvent::SftpTransfer {
            id,
            name,
            is_upload,
            transferred,
            total,
            state,
            msg,
        } => {
            let detail = match state {
                // On error, show the actual message when we have one.
                2 => {
                    if msg.is_empty() {
                        t("失败", "Failed").to_string()
                    } else {
                        msg
                    }
                }
                1 => t("已完成", "Done").to_string(),
                // Remote-side prep (e.g. tar packing) before bytes start flowing (#100).
                3 => t("文件准备中", "Preparing...").to_string(),
                // User-cancelled transfer (#100).
                4 => t("已取消", "Cancelled").to_string(),
                _ => {
                    if total > 0 {
                        format!("{}/{}", format_size(transferred), format_size(total))
                    } else {
                        format_size(transferred)
                    }
                }
            };
            let percent = if state == 1 {
                1.0
            } else if total > 0 {
                (transferred as f32 / total as f32).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let rec = TransferInfo {
                id: id.clone().into(),
                name: name.into(),
                detail: detail.into(),
                percent,
                state: state as i32,
                is_upload,
            };
            if let Some(model) = win
                .get_transfers()
                .as_any()
                .downcast_ref::<VecModel<TransferInfo>>()
            {
                let mut found = None;
                for i in 0..model.row_count() {
                    if let Some(row) = model.row_data(i) {
                        if row.id.as_str() == id.as_str() {
                            found = Some(i);
                            break;
                        }
                    }
                }
                match found {
                    Some(i) => model.set_row_data(i, rec),
                    None => model.insert(0, rec), // newest at top
                }
            }
        }
        SessionEvent::HostKeyPrompt {
            host,
            port,
            key_type,
            fingerprint,
            changed,
            responder,
        } => {
            enqueue_hostkey_prompt(win, host, port, key_type, fingerprint, changed, responder);
        }
        SessionEvent::CredentialPrompt {
            session_id,
            host,
            user,
            need_user,
            need_password,
            responder,
        } => {
            enqueue_cred_prompt(
                win,
                session_id,
                host,
                user,
                need_user,
                need_password,
                responder,
            );
        }
        SessionEvent::CommandRan(cmd) => {
            // A command typed directly in the terminal, captured via the shell
            // hook (#113). Record it in the same command-box history, reusing the
            // de-dup/move-to-end logic, and refresh the model. Skip captures that
            // mirror a command-box / quick-command submission already stored above.
            if should_suppress_terminal_command_capture(tab_id, &cmd) {
                return;
            }
            HISTORY_STORE.with(|s| {
                if let Some(store) = s.borrow().as_ref() {
                    {
                        let mut st = store.borrow_mut();
                        st.push_command_history(cmd);
                        let _ = st.save();
                    }
                    win.set_command_history(history_model(&store.borrow()));
                }
            });
        }
    }
}

thread_local! {
    /// The config store, made reachable from the Slint-thread event handler so
    /// terminal-captured commands (#113) can be appended to history. Set once at
    /// startup; only touched on the Slint event-loop thread.
    pub(super) static HISTORY_STORE: RefCell<Option<Rc<RefCell<ConfigStore>>>> = const { RefCell::new(None) };
}

// ---------------------------------------------------------------------------
// Host-key confirmation (#109-5)
// ---------------------------------------------------------------------------

thread_local! {
    /// Prompts awaiting a decision; the front one is shown. Lives on the Slint
    /// event-loop thread (all access is from there).
    pub(super) static HOSTKEY_QUEUE: RefCell<VecDeque<PendingHostKey>> = RefCell::new(VecDeque::new());
    /// host:port → decision, remembered for this run so a duplicate prompt
    /// (second connection to the same host) is answered without a new dialog.
    pub(super) static HOSTKEY_DECIDED: RefCell<HashMap<String, bool>> = RefCell::new(HashMap::new());
}
