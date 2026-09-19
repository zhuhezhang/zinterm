use super::*;

pub(super) fn wire_session_tree(
    window: &AppWindow,
    store: Rc<RefCell<ConfigStore>>,
    sessions_model: Rc<VecModel<SessionInfo>>,
    welcome_session_query: Rc<RefCell<String>>,
) {
    // New session -> open dialog with blank draft (host prefilled from search Enter).
    let weak = window.as_weak();
    let store_ng = store.clone();
    window.on_new_session_clicked(move |host: SharedString| {
        if let Some(w) = weak.upgrade() {
            open_new_session_dialog(&w, &store_ng.borrow(), "", host.as_str());
        }
    });

    // New session rooted in a Quick Connect folder (group context menu).
    {
        let weak = window.as_weak();
        let store = store.clone();
        window.on_new_session_in_group(move |group: SharedString| {
            if let Some(w) = weak.upgrade() {
                open_new_session_dialog(&w, &store.borrow(), group.as_str(), "");
            }
        });
    }

    // Export all sessions to a portable JSON file (issue #46). Password /
    // private-key fields are omitted; host/user/port stay plaintext.
    {
        let weak = window.as_weak();
        let store = store.clone();
        window.on_export_sessions(move || {
            if let Some(path) = rfd::FileDialog::new()
                .set_file_name(
                    chrono::Local::now()
                        .format("zinterm-sessions-%Y%m%d-%H%M%S.json")
                        .to_string(),
                )
                .add_filter("JSON", &["json"])
                .save_file()
            {
                let res = store.borrow().export_to(&path);
                if let Some(w) = weak.upgrade() {
                    let hint = match res {
                        Ok(n) => {
                            if crate::i18n::is_en() {
                                format!("Successfully exported {n} connections")
                            } else {
                                format!("已成功导出{n}个连接")
                            }
                        }
                        Err(e) => format!("{}: {}", t("导出失败", "export failed"), e),
                    };
                    w.set_ssh_import_hint(hint.into());
                }
            }
        });
    }

    // Import sessions from a portable JSON file (issue #46).
    {
        let weak = window.as_weak();
        let store = store.clone();
        let sessions_model = sessions_model.clone();
        let welcome_session_query = welcome_session_query.clone();
        window.on_import_sessions(move || {
            if let Some(path) = rfd::FileDialog::new()
                .add_filter("JSON", &["json"])
                .pick_file()
            {
                let res = store.borrow_mut().import_from(&path);
                if let Some(w) = weak.upgrade() {
                    let hint = match res {
                        Ok((added, skipped)) => {
                            sync_welcome_sessions(&store.borrow(), &sessions_model, &welcome_session_query.borrow());
                            if crate::i18n::is_en() {
                                format!(
                                    "Import succeeded - imported {added} session(s)/skipped {skipped} duplicate(s)"
                                )
                            } else {
                                format!(
                                    "导入成功 - 已导入{added}个会话/跳过{skipped}个重复会话"
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

    // Edit -> open dialog prefilled.
    {
        let weak = window.as_weak();
        let store = store.clone();
        window.on_edit_session(move |id: SharedString| {
            let id = id.to_string();
            let store = store.borrow();
            let Some(session) = store.get(&id) else {
                return;
            };
            if let Some(w) = weak.upgrade() {
                w.set_session_groups(session_groups_model(&store));
                w.set_dialog_id(session.id.clone().into());
                w.set_dialog_name(session.name.clone().into());
                w.set_dialog_host(session.host.clone().into());
                w.set_dialog_port(session.port.to_string().into());
                w.set_dialog_user(session.user.clone().into());
                w.set_dialog_auth(session.auth.as_str().into());
                // Login password and key passphrase are distinct stored fields.
                if session.auth == AuthMethod::Key {
                    w.set_dialog_password("".into());
                    let kp = if session.key_passphrase.is_empty() {
                        String::new()
                    } else {
                        session.key_passphrase.as_str().to_string()
                    };
                    w.set_dialog_key_passphrase(kp.into());
                } else {
                    let pw = if session.password.is_empty() {
                        String::new()
                    } else {
                        session.password.as_str().to_string()
                    };
                    w.set_dialog_password(pw.into());
                    w.set_dialog_key_passphrase("".into());
                }
                // Unified key field: echo path or pasted key as plaintext.
                w.set_dialog_key_path("".into());
                w.set_dialog_key_inline(session.private_key.as_str().into());
                w.set_dialog_key_inline_mode(false);
                w.set_dialog_key_saved_inline(false);
                w.set_dialog_group(session.group.clone().into());
                w.set_dialog_kind(session.kind.as_str().into());
                w.set_dialog_serial_port(session.serial_port.clone().into());
                if session.kind == SessionKind::Serial {
                    w.set_serial_ports(serial_ports_model());
                }
                w.set_dialog_baud(session.baud_rate.to_string().into());
                w.set_dialog_data_bits(session.data_bits.to_string().into());
                w.set_dialog_stop_bits(session.stop_bits.to_string().into());
                w.set_dialog_parity(parity_display(&session.parity).into());
                w.set_dialog_flow(flow_control_display(&session.flow_control).into());
                w.set_dialog_encoding(session.encoding.clone().into());
                w.set_dialog_backspace_mode(
                    normalize_backspace_mode(&session.backspace_mode).into(),
                );
                w.set_dialog_shell(session.shell.clone().into());
                w.set_dialog_working_directory(session.working_directory.clone().into());
                w.set_dialog_enable_sftp(session.enable_sftp);
                w.set_dialog_enable_prompt_setup(session.enable_prompt_setup);
                w.set_dialog_enable_command_panel(session.enable_command_panel);
                w.set_dialog_editing(true);
                w.set_dialog_open(true);
            }
        });
    }

    // Remove session.
    {
        let weak = window.as_weak();
        let store = store.clone();
        let sessions_model = sessions_model.clone();
        let welcome_session_query = welcome_session_query.clone();
        window.on_remove_session(move |id: SharedString| {
            {
                let mut s = store.borrow_mut();
                s.remove(id.as_ref());
                s.save_later(SaveKind::sessions_and_vault());
            }
            sync_welcome_sessions(
                &store.borrow(),
                &sessions_model,
                &welcome_session_query.borrow(),
            );
            if let Some(w) = weak.upgrade() {
                // Touch a property so the list re-renders reliably.
                let _ = w.get_sessions();
            }
        });
    }

    // Duplicate a session: clone it with a fresh id and a " (copy)" name (#41).
    {
        let weak = window.as_weak();
        let store = store.clone();
        let sessions_model = sessions_model.clone();
        let welcome_session_query = welcome_session_query.clone();
        window.on_duplicate_session(move |id: SharedString| {
            {
                let mut s = store.borrow_mut();
                if let Some(orig) = s.get(id.as_ref()).cloned() {
                    let from_id = orig.id.clone();
                    let mut copy = orig;
                    copy.id = crate::config::Session::new_saved_id();
                    copy.name = format!("{} (copy)", copy.name);
                    copy.last_used = None;
                    let to_id = copy.id.clone();
                    s.upsert(copy);
                    // Copy encrypted vault entry when present (no re-encrypt).
                    if let Err(e) = crate::config::vault::duplicate_secrets(
                        &crate::config::data_dir(),
                        &from_id,
                        &to_id,
                    ) {
                        tracing::warn!("failed to duplicate vault secrets: {e:#}");
                    }
                    s.save_later(SaveKind::sessions_and_vault());
                }
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

    // Collapse / expand a group in the welcome list (#41). Persists fold state
    // to ui-state.json on the background thread (not the full sessions file).
    {
        let weak = window.as_weak();
        let store = store.clone();
        let sessions_model = sessions_model.clone();
        let welcome_session_query = welcome_session_query.clone();
        window.on_toggle_group(move |group: SharedString| {
            let target = group.to_string();
            let new_state = {
                let store = store.borrow();
                let collapsed = store
                    .collapsed_session_groups()
                    .map(|groups| groups.iter().any(|g| g == &target))
                    .unwrap_or(true);
                !collapsed
            };
            {
                let mut store = store.borrow_mut();
                store.set_session_group_collapsed(&target, new_state);
                store.save_later(SaveKind::UI);
                sync_welcome_sessions(&store, &sessions_model, &welcome_session_query.borrow());
            }
            if let Some(w) = weak.upgrade() {
                let _ = w.get_sessions();
            }
        });
    }

    // Expand / collapse every Quick Connect folder, or only one folder's children.
    {
        let weak = window.as_weak();
        let store = store.clone();
        let sessions_model = sessions_model.clone();
        let welcome_session_query = welcome_session_query.clone();
        window.on_expand_all_groups(move || {
            apply_session_group_collapse(
                &weak,
                &store,
                &sessions_model,
                &welcome_session_query.borrow(),
                |s| {
                    s.set_all_session_groups_collapsed(false);
                },
            );
        });
    }
    {
        let weak = window.as_weak();
        let store = store.clone();
        let sessions_model = sessions_model.clone();
        let welcome_session_query = welcome_session_query.clone();
        window.on_collapse_all_groups(move || {
            apply_session_group_collapse(
                &weak,
                &store,
                &sessions_model,
                &welcome_session_query.borrow(),
                |s| {
                    s.set_all_session_groups_collapsed(true);
                },
            );
        });
    }
    {
        let weak = window.as_weak();
        let store = store.clone();
        let sessions_model = sessions_model.clone();
        let welcome_session_query = welcome_session_query.clone();
        window.on_expand_group_children(move |group: SharedString| {
            let path = group.to_string();
            apply_session_group_collapse(
                &weak,
                &store,
                &sessions_model,
                &welcome_session_query.borrow(),
                |s| {
                    s.set_session_group_children_collapsed(&path, false);
                },
            );
        });
    }
    {
        let weak = window.as_weak();
        let store = store.clone();
        let sessions_model = sessions_model.clone();
        let welcome_session_query = welcome_session_query.clone();
        window.on_collapse_group_children(move |group: SharedString| {
            let path = group.to_string();
            apply_session_group_collapse(
                &weak,
                &store,
                &sessions_model,
                &welcome_session_query.borrow(),
                |s| {
                    s.set_session_group_children_collapsed(&path, true);
                },
            );
        });
    }

    // Group create / rename (#41).
    {
        let weak = window.as_weak();
        window.on_begin_sibling_group(move |path: SharedString| {
            if let Some(w) = weak.upgrade() {
                w.set_group_dialog_orig("".into());
                w.set_group_dialog_parent(group_parent_path(path.as_str()).into());
                w.set_group_dialog_mode("sibling".into());
                w.set_group_dialog_name("".into());
                w.set_group_dialog_error("".into());
                w.set_group_dialog_open(true);
            }
        });
    }
    {
        let weak = window.as_weak();
        window.on_begin_child_group(move |path: SharedString| {
            if let Some(w) = weak.upgrade() {
                w.set_group_dialog_orig("".into());
                w.set_group_dialog_parent(path);
                w.set_group_dialog_mode("child".into());
                w.set_group_dialog_name("".into());
                w.set_group_dialog_error("".into());
                w.set_group_dialog_open(true);
            }
        });
    }
    {
        let weak = window.as_weak();
        window.on_begin_rename_group(move |path: SharedString, label: SharedString| {
            if let Some(w) = weak.upgrade() {
                w.set_group_dialog_orig(path);
                w.set_group_dialog_parent("".into());
                w.set_group_dialog_mode("rename".into());
                w.set_group_dialog_name(label);
                w.set_group_dialog_error("".into());
                w.set_group_dialog_open(true);
            }
        });
    }
    {
        let weak = window.as_weak();
        let store = store.clone();
        let sessions_model = sessions_model.clone();
        let welcome_session_query = welcome_session_query.clone();
        window.on_submit_group(
            move |orig: SharedString, segment: SharedString, parent: SharedString| {
                let segment = segment.trim();
                let parent = parent.trim();
                if segment.is_empty() {
                    return SharedString::from(t("请输入分组名称", "Enter a group name"));
                }
                let new_full = if orig.is_empty() {
                    group_join(parent, segment)
                } else {
                    group_join(group_parent_path(orig.as_str()).as_str(), segment)
                };
                let error = {
                    let s = store.borrow();
                    if !is_valid_group_segment(segment) {
                        Some(t(
                            "分组名称不能包含“/”或为系统保留名",
                            "Group name cannot contain “/” or be a reserved name",
                        ))
                    } else if s.session_group_exists(&new_full)
                        && (orig.is_empty() || !new_full.eq_ignore_ascii_case(orig.as_str()))
                    {
                        Some(t("分组已存在", "Group already exists"))
                    } else {
                        None
                    }
                };
                if let Some(message) = error {
                    return SharedString::from(message);
                }
                {
                    let mut s = store.borrow_mut();
                    if orig.is_empty() {
                        s.add_group(new_full);
                    } else {
                        s.rename_group(orig.as_str(), new_full);
                    }
                    s.save_later(SaveKind::SESSIONS | SaveKind::UI);
                }
                sync_welcome_sessions(
                    &store.borrow(),
                    &sessions_model,
                    &welcome_session_query.borrow(),
                );
                if let Some(w) = weak.upgrade() {
                    let _ = w.get_sessions();
                }
                SharedString::new()
            },
        );
    }
    // Group delete (#41) — cascades: child groups and sessions inside are removed.
    {
        let weak = window.as_weak();
        let store = store.clone();
        let sessions_model = sessions_model.clone();
        let welcome_session_query = welcome_session_query.clone();
        window.on_delete_group(move |name: SharedString| {
            {
                let mut s = store.borrow_mut();
                s.remove_group(name.as_ref());
                s.save_later(SaveKind::SESSIONS | SaveKind::UI);
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

    // Drag-drop: move a session into another group (root = "").
    {
        let weak = window.as_weak();
        let store = store.clone();
        let sessions_model = sessions_model.clone();
        let welcome_session_query = welcome_session_query.clone();
        window.on_drop_welcome_session(move |id: SharedString, target_group: SharedString| {
            {
                let mut s = store.borrow_mut();
                if !s.move_session_to_group(id.as_str(), target_group.as_str()) {
                    return;
                }
                s.save_later(SaveKind::SESSIONS);
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
    // Drag-drop: re-parent a group under another folder (root = "").
    {
        let weak = window.as_weak();
        let store = store.clone();
        let sessions_model = sessions_model.clone();
        let welcome_session_query = welcome_session_query.clone();
        window.on_drop_welcome_group(move |path: SharedString, target_parent: SharedString| {
            {
                let mut s = store.borrow_mut();
                if !s.move_group_to_parent(path.as_str(), target_parent.as_str()) {
                    return;
                }
                s.save_later(SaveKind::SESSIONS | SaveKind::UI);
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
        let weak = window.as_weak();
        window.on_welcome_drag_at(
            move |list_top: f32,
                  pointer_y: f32,
                  _drag_kind: SharedString,
                  _drag_from: SharedString| {
                let Some(w) = weak.upgrade() else {
                    return;
                };
                let rows = session_infos_from_model(&w.get_sessions());
                let target = welcome_drop_target_at(&rows, list_top, pointer_y);
                w.set_welcome_drop_target(target.into());
            },
        );
    }
}
