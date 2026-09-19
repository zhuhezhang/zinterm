use super::*;

pub(super) fn wire_command_bar(
    window: &AppWindow,
    handles: Rc<RefCell<HashMap<String, SessionHandle>>>,
    store: Rc<RefCell<ConfigStore>>,
) {
    // --- Command bar (#55): run command + quick-command management ---------
    {
        let handles_rc = handles.clone();
        let store_rc = store.clone();
        let weak = window.as_weak();
        window.on_run_command(
            move |tab_id: SharedString, cmd: SharedString, to_all: bool| {
                let (history_line, bytes) = encode_command_bar_input(&cmd);
                {
                    let h = handles_rc.borrow();
                    if to_all {
                        for (id, handle) in h.iter() {
                            register_app_command_capture(id, cmd.as_str());
                            handle.send_raw(bytes.clone());
                        }
                    } else if let Some(handle) = h.get(tab_id.as_str()) {
                        register_app_command_capture(tab_id.as_str(), cmd.as_str());
                        handle.send_raw(bytes);
                    }
                }
                if let Some(line) = history_line {
                    let mut s = store_rc.borrow_mut();
                    s.push_command_history(line);
                    s.save_later(SaveKind::COMMANDS);
                    if let Some(w) = weak.upgrade() {
                        w.set_command_history(history_model(&s));
                    }
                }
            },
        );
    }
    // Copy a history command to the clipboard (#96).
    {
        window.on_copy_text(move |text: SharedString| {
            let t = text.to_string();
            std::thread::spawn(move || clipboard_set_text(t));
        });
    }
    // Delete a history entry (#96). The command-history model remains in
    // storage order, so this legacy row index still maps straight through.
    {
        let store_rc = store.clone();
        let weak = window.as_weak();
        window.on_delete_history(move |i: i32| {
            {
                let mut s = store_rc.borrow_mut();
                let idx = i as usize;
                if idx < s.command_history().len() {
                    s.remove_command_history(idx);
                    s.save_later(SaveKind::COMMANDS);
                }
            }
            if let Some(w) = weak.upgrade() {
                w.set_command_history(history_model(&store_rc.borrow()));
            }
        });
    }
    // History search (#101): filter the dropdown by a case-insensitive substring.
    // The current query is shared so a delete from a filtered view re-filters.
    let hist_query: Rc<RefCell<String>> = Rc::new(RefCell::new(String::new()));
    {
        let store_rc = store.clone();
        let weak = window.as_weak();
        let hist_query = hist_query.clone();
        window.on_search_history(move |query: SharedString| {
            *hist_query.borrow_mut() = query.to_string();
            if let Some(w) = weak.upgrade() {
                w.set_history_view(history_view_model(&store_rc.borrow(), &query));
            }
        });
    }
    // Delete a history entry by its command text (#101) — index-free so it works
    // from the filtered dropdown view.
    {
        let store_rc = store.clone();
        let weak = window.as_weak();
        let hist_query = hist_query.clone();
        window.on_delete_history_cmd(move |cmd: SharedString| {
            {
                let mut s = store_rc.borrow_mut();
                if let Some(idx) = s.command_history().iter().position(|c| c == cmd.as_str()) {
                    s.remove_command_history(idx);
                    s.save_later(SaveKind::COMMANDS);
                }
            }
            if let Some(w) = weak.upgrade() {
                let s = store_rc.borrow();
                w.set_command_history(history_model(&s));
                w.set_history_view(history_view_model(&s, &hist_query.borrow()));
            }
        });
    }
    // Runtime-only collapse state for quick-command groups (#55) — like the
    // welcome session groups, this is not persisted across restarts. Starts with
    // every group collapsed (default-collapsed view).
    let collapsed_quick_groups: Rc<RefCell<std::collections::HashSet<String>>> =
        Rc::new(RefCell::new(all_quick_group_names(&store.borrow())));
    let quick_query: Rc<RefCell<String>> = Rc::new(RefCell::new(String::new()));
    let qcm_manage_query: Rc<RefCell<String>> = Rc::new(RefCell::new(String::new()));
    {
        let store_rc = store.clone();
        let weak = window.as_weak();
        let collapsed = collapsed_quick_groups.clone();
        let quick_query = quick_query.clone();
        window.on_search_quick_commands(move |query: SharedString| {
            *quick_query.borrow_mut() = query.to_string();
            if let Some(w) = weak.upgrade() {
                w.set_quick_view(quick_cmd_view_model(
                    &store_rc.borrow(),
                    &collapsed.borrow(),
                    &query,
                ));
            }
        });
    }
    {
        let store_rc = store.clone();
        let weak = window.as_weak();
        let collapsed = collapsed_quick_groups.clone();
        let qcm_manage_query = qcm_manage_query.clone();
        window.on_search_qcm_commands(move |query: SharedString| {
            *qcm_manage_query.borrow_mut() = query.to_string();
            if let Some(w) = weak.upgrade() {
                w.set_qcm_manage_commands(quick_cmd_view_model(
                    &store_rc.borrow(),
                    &collapsed.borrow(),
                    &query,
                ));
            }
        });
    }
    {
        let store_rc = store.clone();
        let weak = window.as_weak();
        let collapsed = collapsed_quick_groups.clone();
        let quick_query = quick_query.clone();
        let qcm_manage_query = qcm_manage_query.clone();
        window.on_add_quick_command(
            move |name: SharedString, command: SharedString, group: SharedString| {
                let name = name.trim().to_string();
                let command = command.to_string();
                let group = group.trim().to_string();
                if name.is_empty() || command.trim().is_empty() {
                    return;
                }
                {
                    let mut s = store_rc.borrow_mut();
                    let mut v = s.quick_commands().to_vec();
                    v.push(crate::config::QuickCommand {
                        name,
                        command,
                        group,
                        send_enter: true,
                    });
                    s.set_quick_commands(v);
                    s.save_later(SaveKind::COMMANDS);
                }
                if let Some(w) = weak.upgrade() {
                    sync_quick_command_models(
                        &w,
                        &store_rc.borrow(),
                        &collapsed.borrow(),
                        &quick_query.borrow(),
                        &qcm_manage_query.borrow(),
                    );
                }
            },
        );
    }
    {
        let store_rc = store.clone();
        let weak = window.as_weak();
        let collapsed = collapsed_quick_groups.clone();
        let quick_query = quick_query.clone();
        let qcm_manage_query = qcm_manage_query.clone();
        window.on_delete_quick_command(move |index: i32| {
            {
                let mut s = store_rc.borrow_mut();
                let mut v = s.quick_commands().to_vec();
                let i = index as usize;
                if i < v.len() {
                    v.remove(i);
                }
                s.set_quick_commands(v);
                s.save_later(SaveKind::COMMANDS);
            }
            if let Some(w) = weak.upgrade() {
                sync_quick_command_models(
                    &w,
                    &store_rc.borrow(),
                    &collapsed.borrow(),
                    &quick_query.borrow(),
                    &qcm_manage_query.borrow(),
                );
            }
        });
    }
    {
        let store_rc = store.clone();
        let weak = window.as_weak();
        let collapsed = collapsed_quick_groups.clone();
        let quick_query = quick_query.clone();
        let qcm_manage_query = qcm_manage_query.clone();
        window.on_toggle_quick_group(move |group: SharedString| {
            let g = group.to_string();
            {
                let mut set = collapsed.borrow_mut();
                if !set.remove(&g) {
                    set.insert(g);
                }
            }
            if let Some(w) = weak.upgrade() {
                sync_quick_command_models(
                    &w,
                    &store_rc.borrow(),
                    &collapsed.borrow(),
                    &quick_query.borrow(),
                    &qcm_manage_query.borrow(),
                );
            }
        });
    }
    // Edit (#55): load the entry into the manage form in edit mode.
    {
        let store_rc = store.clone();
        let weak = window.as_weak();
        window.on_edit_quick_command(move |index: i32| {
            let i = index as usize;
            let cmd = store_rc.borrow().quick_commands().get(i).cloned();
            if let (Some(c), Some(w)) = (cmd, weak.upgrade()) {
                w.set_qcm_name(c.name.into());
                w.set_qcm_command(c.command.into());
                w.set_qcm_group(c.group.into());
                w.set_qcm_edit_index(index);
                w.set_quick_cmd_manage_open(true);
            }
        });
    }
    // Save an edited entry (#55).
    {
        let store_rc = store.clone();
        let weak = window.as_weak();
        let collapsed = collapsed_quick_groups.clone();
        let quick_query = quick_query.clone();
        let qcm_manage_query = qcm_manage_query.clone();
        window.on_save_quick_command(
            move |index: i32, name: SharedString, command: SharedString, group: SharedString| {
                let name = name.trim().to_string();
                let command = command.to_string();
                let group = group.trim().to_string();
                if name.is_empty() || command.trim().is_empty() {
                    return;
                }
                {
                    let mut s = store_rc.borrow_mut();
                    let cmds = s.quick_commands();
                    let name =
                        disambiguate_quick_command_name(cmds, &group, &name, Some(index as usize));
                    s.update_quick_command(
                        index as usize,
                        crate::config::QuickCommand {
                            name,
                            command,
                            group,
                            send_enter: true,
                        },
                    );
                    s.save_later(SaveKind::COMMANDS);
                }
                if let Some(w) = weak.upgrade() {
                    sync_quick_command_models(
                        &w,
                        &store_rc.borrow(),
                        &collapsed.borrow(),
                        &quick_query.borrow(),
                        &qcm_manage_query.borrow(),
                    );
                }
            },
        );
    }
    // Duplicate (#55): clone the entry as a starting point.
    {
        let store_rc = store.clone();
        let weak = window.as_weak();
        let collapsed = collapsed_quick_groups.clone();
        let quick_query = quick_query.clone();
        let qcm_manage_query = qcm_manage_query.clone();
        window.on_duplicate_quick_command(move |index: i32| {
            {
                let mut s = store_rc.borrow_mut();
                let mut v = s.quick_commands().to_vec();
                if let Some(c) = v.get(index as usize).cloned() {
                    let name = duplicate_quick_command_name(&v, &c.group, &c.name);
                    let dup = crate::config::QuickCommand {
                        name,
                        command: c.command,
                        group: c.group,
                        send_enter: c.send_enter,
                    };
                    v.insert(index as usize + 1, dup);
                    s.set_quick_commands(v);
                    s.save_later(SaveKind::COMMANDS);
                }
            }
            if let Some(w) = weak.upgrade() {
                sync_quick_command_models(
                    &w,
                    &store_rc.borrow(),
                    &collapsed.borrow(),
                    &quick_query.borrow(),
                    &qcm_manage_query.borrow(),
                );
            }
        });
    }
    // Move to a group (#55): "default" maps to the empty (ungrouped) group.
    {
        let store_rc = store.clone();
        let weak = window.as_weak();
        let collapsed = collapsed_quick_groups.clone();
        let quick_query = quick_query.clone();
        let qcm_manage_query = qcm_manage_query.clone();
        window.on_move_quick_command(move |index: i32, group: SharedString| {
            let target = group.to_string();
            let target = if target == "default" {
                String::new()
            } else {
                target
            };
            {
                let mut s = store_rc.borrow_mut();
                let mut v = s.quick_commands().to_vec();
                let i = index as usize;
                if let Some(c) = v.get(i).cloned() {
                    let name = disambiguate_quick_command_name(&v, &target, &c.name, Some(i));
                    v[i].group = target;
                    v[i].name = name;
                }
                s.set_quick_commands(v);
                s.save_later(SaveKind::COMMANDS);
            }
            if let Some(w) = weak.upgrade() {
                sync_quick_command_models(
                    &w,
                    &store_rc.borrow(),
                    &collapsed.borrow(),
                    &quick_query.borrow(),
                    &qcm_manage_query.borrow(),
                );
            }
        });
    }
    // Reorder inside the current group (#310). The stored Vec remains the
    // source of truth; the grouped display model preserves this relative order.
    {
        let weak = window.as_weak();
        window.on_qcm_drag_at(
            move |list_top: f32, pointer_y: f32, drag_from: i32, drag_group_from: SharedString| {
                let Some(w) = weak.upgrade() else {
                    return;
                };
                let rows = quick_cmds_from_model(&w.get_qcm_manage_commands());
                if drag_from >= 0 {
                    if let Some(drop) = qcm_command_drop_at(&rows, list_top, pointer_y) {
                        w.set_qcm_drop_group(drop.group.into());
                        w.set_qcm_drop_before(drop.before_orig);
                    }
                } else if !drag_group_from.is_empty() {
                    let before = qcm_group_drop_at(&rows, list_top, pointer_y);
                    w.set_qcm_drop_before_group(before.into());
                }
            },
        );
    }
    {
        let store_rc = store.clone();
        let weak = window.as_weak();
        let collapsed = collapsed_quick_groups.clone();
        let quick_query = quick_query.clone();
        let qcm_manage_query = qcm_manage_query.clone();
        window.on_drop_quick_command(
            move |from: i32, target_group: SharedString, before_orig: i32| {
                let changed = {
                    let mut s = store_rc.borrow_mut();
                    let mut commands = s.quick_commands().to_vec();
                    let changed = drop_quick_command(
                        &mut commands,
                        from as usize,
                        &target_group,
                        before_orig,
                    );
                    if changed {
                        s.set_quick_commands(commands);
                        s.save_later(SaveKind::COMMANDS);
                    }
                    changed
                };
                if changed {
                    if let Some(w) = weak.upgrade() {
                        sync_quick_command_models(
                            &w,
                            &store_rc.borrow(),
                            &collapsed.borrow(),
                            &quick_query.borrow(),
                            &qcm_manage_query.borrow(),
                        );
                    }
                }
            },
        );
    }
    // Reorder quick-command groups in the manage dialog.
    {
        let store_rc = store.clone();
        let weak = window.as_weak();
        let collapsed = collapsed_quick_groups.clone();
        let quick_query = quick_query.clone();
        let qcm_manage_query = qcm_manage_query.clone();
        window.on_drop_quick_group(move |from: SharedString, before: SharedString| {
            let changed = {
                let mut s = store_rc.borrow_mut();
                let changed = s.reorder_quick_group(from.as_ref(), before.as_ref());
                if changed {
                    s.save_later(SaveKind::COMMANDS);
                }
                changed
            };
            if changed {
                if let Some(w) = weak.upgrade() {
                    sync_quick_command_models(
                        &w,
                        &store_rc.borrow(),
                        &collapsed.borrow(),
                        &quick_query.borrow(),
                        &qcm_manage_query.borrow(),
                    );
                }
            }
        });
    }
    // Quick-group create / rename (#55).
    {
        let store_rc = store.clone();
        let weak = window.as_weak();
        let collapsed = collapsed_quick_groups.clone();
        let quick_query = quick_query.clone();
        let qcm_manage_query = qcm_manage_query.clone();
        window.on_submit_quick_group(move |orig: SharedString, name: SharedString| {
            {
                let mut s = store_rc.borrow_mut();
                if orig.is_empty() {
                    s.add_quick_group(name.to_string());
                } else {
                    s.rename_quick_group(orig.as_ref(), name.to_string());
                }
                s.save_later(SaveKind::COMMANDS);
            }
            if let Some(w) = weak.upgrade() {
                sync_quick_command_models(
                    &w,
                    &store_rc.borrow(),
                    &collapsed.borrow(),
                    &quick_query.borrow(),
                    &qcm_manage_query.borrow(),
                );
            }
        });
    }
    // Quick-group delete (#55) — UI only offers this on empty groups.
    {
        let store_rc = store.clone();
        let weak = window.as_weak();
        let collapsed = collapsed_quick_groups.clone();
        let quick_query = quick_query.clone();
        let qcm_manage_query = qcm_manage_query.clone();
        window.on_delete_quick_group(move |name: SharedString| {
            {
                let mut s = store_rc.borrow_mut();
                s.remove_quick_group(name.as_ref());
                s.save_later(SaveKind::COMMANDS);
            }
            if let Some(w) = weak.upgrade() {
                sync_quick_command_models(
                    &w,
                    &store_rc.borrow(),
                    &collapsed.borrow(),
                    &quick_query.borrow(),
                    &qcm_manage_query.borrow(),
                );
            }
        });
    }
}
