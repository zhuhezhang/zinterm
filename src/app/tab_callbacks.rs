use super::*;

struct TabCloseCtx {
    weak: slint::Weak<AppWindow>,
    layout: Rc<RefCell<crate::layout::Layout>>,
    content_size: Rc<std::cell::Cell<(f32, f32)>>,
    tabs_model: Rc<VecModel<TabInfo>>,
    terminals_model: Rc<VecModel<TerminalState>>,
    handles: Rc<RefCell<HashMap<String, SessionHandle>>>,
    bufs: TermBuffers,
    render_gates: RenderGates,
    sftp_handles: SftpHandles,
    sftp_last_cwd: SftpLastCwd,
    panes_model: Rc<VecModel<PaneInfo>>,
    splitters_model: Rc<VecModel<SplitterInfo>>,
}

fn close_tab_id(ctx: &TabCloseCtx, id: &str) {
    if id == "welcome" {
        return;
    }
    clear_tab_credentials(id);
    if let Some(handle) = ctx.handles.borrow_mut().remove(id) {
        handle.close();
    }
    if let Some(sftp) = ctx.sftp_handles.lock().unwrap().remove(id) {
        sftp.close();
    }
    ctx.sftp_last_cwd.lock().unwrap().remove(id);
    if let Some(gate) = ctx.render_gates.lock().unwrap().remove(id) {
        gate.close();
    }
    ctx.bufs.lock().unwrap().remove(id);

    let mut idx = None;
    for i in 0..ctx.tabs_model.row_count() {
        if ctx
            .tabs_model
            .row_data(i)
            .map(|r| r.id.as_str() == id)
            .unwrap_or(false)
        {
            idx = Some(i);
            break;
        }
    }
    if let Some(i) = idx {
        ctx.tabs_model.remove(i);
    }
    let mut tidx = None;
    for i in 0..ctx.terminals_model.row_count() {
        if ctx
            .terminals_model
            .row_data(i)
            .map(|r| r.id.as_str() == id)
            .unwrap_or(false)
        {
            tidx = Some(i);
            break;
        }
    }
    if let Some(i) = tidx {
        ctx.terminals_model.remove(i);
    }

    ctx.layout.borrow_mut().remove_tab(id);
}

fn refresh_after_tab_close(ctx: &TabCloseCtx) {
    if let Some(w) = ctx.weak.upgrade() {
        refresh_panes(
            &w,
            &ctx.layout.borrow(),
            ctx.content_size.get(),
            &ctx.tabs_model,
            &ctx.panes_model,
            &ctx.splitters_model,
        );
    }
}

fn tabs_to_close_left(lay: &crate::layout::Layout, pane_id: u64, tab_id: &str) -> Vec<String> {
    lay.leaf(pane_id)
        .and_then(|l| {
            let pos = l.tabs.iter().position(|t| t == tab_id)?;
            Some(
                l.tabs
                    .iter()
                    .take(pos)
                    .filter(|t| t.as_str() != "welcome")
                    .cloned()
                    .collect(),
            )
        })
        .unwrap_or_default()
}

fn tabs_to_close_right(lay: &crate::layout::Layout, pane_id: u64, tab_id: &str) -> Vec<String> {
    lay.leaf(pane_id)
        .and_then(|l| {
            let pos = l.tabs.iter().position(|t| t == tab_id)?;
            Some(
                l.tabs
                    .iter()
                    .skip(pos + 1)
                    .filter(|t| t.as_str() != "welcome")
                    .cloned()
                    .collect(),
            )
        })
        .unwrap_or_default()
}

fn tabs_to_close_others(lay: &crate::layout::Layout, pane_id: u64, tab_id: &str) -> Vec<String> {
    lay.leaf(pane_id)
        .map(|l| {
            l.tabs
                .iter()
                .filter(|t| t.as_str() != "welcome" && t.as_str() != tab_id)
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

fn tabs_to_close_all(lay: &crate::layout::Layout, pane_id: u64) -> Vec<String> {
    lay.leaf(pane_id)
        .map(|l| {
            l.tabs
                .iter()
                .filter(|t| t.as_str() != "welcome")
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

pub(super) fn wire_tab_callbacks(
    window: &AppWindow,
    tabs_model: Rc<VecModel<TabInfo>>,
    terminals_model: Rc<VecModel<TerminalState>>,
    layout: Rc<RefCell<crate::layout::Layout>>,
    content_size: Rc<std::cell::Cell<(f32, f32)>>,
    panes_model: Rc<VecModel<PaneInfo>>,
    splitters_model: Rc<VecModel<SplitterInfo>>,
    handles: Rc<RefCell<HashMap<String, SessionHandle>>>,
    bufs: TermBuffers,
    render_gates: RenderGates,
    sftp_handles: SftpHandles,
    sftp_last_cwd: SftpLastCwd,
) {
    // ⌘/Ctrl+Tab / ⌘⇧/Ctrl+Shift+Tab cycle within the currently focused pane (#294).
    {
        let weak = window.as_weak();
        let layout = layout.clone();
        let content_size = content_size.clone();
        let tabs_model = tabs_model.clone();
        let panes_model = panes_model.clone();
        let splitters_model = splitters_model.clone();
        let bufs_cycle = bufs.clone();
        window.on_cycle_tab(move |reverse: bool| {
            let next = layout.borrow_mut().cycle_focused_tab(reverse);
            let Some(id) = next else {
                return;
            };
            if let Some(w) = weak.upgrade() {
                refresh_panes(
                    &w,
                    &layout.borrow(),
                    content_size.get(),
                    &tabs_model,
                    &panes_model,
                    &splitters_model,
                );
                rebuild_tab_display(&w, &bufs_cycle, &id);
            }
        });
    }

    // Select a tab inside a pane: make it that pane's active tab and focus the
    // pane. refresh_panes propagates active-tab-id.
    {
        let weak = window.as_weak();
        let layout = layout.clone();
        let content_size = content_size.clone();
        let tabs_model = tabs_model.clone();
        let panes_model = panes_model.clone();
        let splitters_model = splitters_model.clone();
        let bufs_tab_sel = bufs.clone();
        window.on_pane_tab_selected(move |pane_id: i32, id: SharedString| {
            let id = id.to_string();
            {
                let mut lay = layout.borrow_mut();
                lay.focused = pane_id as u64;
                if let Some(l) = lay.leaf_mut(pane_id as u64) {
                    if l.tabs.iter().any(|t| t == &id) {
                        l.active = id.clone();
                    }
                }
            }
            if let Some(w) = weak.upgrade() {
                refresh_panes(
                    &w,
                    &layout.borrow(),
                    content_size.get(),
                    &tabs_model,
                    &panes_model,
                    &splitters_model,
                );
                // Tab just became visible — render any output ingested while it
                // was in the background (e.g. another session was unzipping).
                rebuild_tab_display(&w, &bufs_tab_sel, &id);
            }
        });
    }

    // Drag-to-reorder within a pane's strip: move `tab_id` by `dir` slots
    // (negative = left, positive = right). Updates the existing pane tab
    // VecModel in place via set_row_data rotation so the TouchArea that holds
    // the pointer grab survives and the user can hop across multiple tabs in
    // one gesture.
    {
        let weak = window.as_weak();
        let layout = layout.clone();
        let panes_model = panes_model.clone();
        window.on_pane_tab_reorder(move |pane_id: i32, tab_id: SharedString, dir: i32| {
            let tab_id = tab_id.to_string();
            if dir == 0 || tab_id.is_empty() {
                return;
            }
            let (from, to) = {
                let mut lay = layout.borrow_mut();
                let Some(l) = lay.leaf_mut(pane_id as u64) else {
                    return;
                };
                let n = l.tabs.len() as i32;
                if n <= 1 {
                    return;
                }
                let Some(from) = l.tabs.iter().position(|t| t.as_str() == tab_id) else {
                    return;
                };
                let from = from as i32;
                let to = (from + dir).clamp(0, n - 1);
                if from == to {
                    return;
                }
                let item = l.tabs.remove(from as usize);
                l.tabs.insert(to as usize, item);
                (from as usize, to as usize)
            };
            // Mirror the move onto the live UI model without replacing ModelRc
            // (a full refresh_panes would destroy the drag source mid-gesture).
            for i in 0..panes_model.row_count() {
                let Some(pane) = panes_model.row_data(i) else {
                    continue;
                };
                if pane.id != pane_id {
                    continue;
                }
                let Some(tm) = pane.tabs.as_any().downcast_ref::<VecModel<TabInfo>>() else {
                    break;
                };
                // Prefer indices from the layout hop; fall back to a fresh id lookup
                // if the UI model somehow drifted.
                let from = (0..tm.row_count())
                    .find(|&j| {
                        tm.row_data(j)
                            .map(|t| t.id.as_str() == tab_id)
                            .unwrap_or(false)
                    })
                    .unwrap_or(from);
                let to = to.min(tm.row_count().saturating_sub(1));
                if from == to || from >= tm.row_count() {
                    break;
                }
                let Some(moving) = tm.row_data(from) else {
                    break;
                };
                if from < to {
                    for j in from..to {
                        if let Some(next) = tm.row_data(j + 1) {
                            tm.set_row_data(j, next);
                        }
                    }
                    tm.set_row_data(to, moving);
                } else {
                    for j in (to..from).rev() {
                        if let Some(prev) = tm.row_data(j) {
                            tm.set_row_data(j + 1, prev);
                        }
                    }
                    tm.set_row_data(to, moving);
                }
                break;
            }
            if let Some(w) = weak.upgrade() {
                // Same-pane hop: clear the cross-pane insertion caret if any.
                w.set_drag_active(false);
            }
        });
    }

    // Close a tab: tear down its session / buffers, drop it from the models, then
    // remove it from the split tree (which re-homes the pane's active tab and
    // collapses the pane if it becomes empty).
    {
        let close_ctx = Rc::new(TabCloseCtx {
            weak: window.as_weak(),
            layout: layout.clone(),
            content_size: content_size.clone(),
            tabs_model: tabs_model.clone(),
            terminals_model: terminals_model.clone(),
            handles: handles.clone(),
            bufs: bufs.clone(),
            render_gates: render_gates.clone(),
            sftp_handles: sftp_handles.clone(),
            sftp_last_cwd: sftp_last_cwd.clone(),
            panes_model: panes_model.clone(),
            splitters_model: splitters_model.clone(),
        });
        {
            let close_ctx = close_ctx.clone();
            window.on_pane_tab_closed(move |_pane_id: i32, id: SharedString| {
                close_tab_id(&close_ctx, &id.to_string());
                refresh_after_tab_close(&close_ctx);
            });
        }
        {
            let close_ctx = close_ctx.clone();
            window.on_pane_tab_close_others(move |pane_id: i32, tab_id: SharedString| {
                let to_close =
                    tabs_to_close_others(&close_ctx.layout.borrow(), pane_id as u64, tab_id.as_str());
                let any = !to_close.is_empty();
                for id in to_close {
                    close_tab_id(&close_ctx, &id);
                }
                if any {
                    refresh_after_tab_close(&close_ctx);
                }
            });
        }
        {
            let close_ctx = close_ctx.clone();
            window.on_pane_tab_close_left(move |pane_id: i32, tab_id: SharedString| {
                let to_close =
                    tabs_to_close_left(&close_ctx.layout.borrow(), pane_id as u64, tab_id.as_str());
                let any = !to_close.is_empty();
                for id in to_close {
                    close_tab_id(&close_ctx, &id);
                }
                if any {
                    refresh_after_tab_close(&close_ctx);
                }
            });
        }
        {
            let close_ctx = close_ctx.clone();
            window.on_pane_tab_close_right(move |pane_id: i32, tab_id: SharedString| {
                let to_close =
                    tabs_to_close_right(&close_ctx.layout.borrow(), pane_id as u64, tab_id.as_str());
                let any = !to_close.is_empty();
                for id in to_close {
                    close_tab_id(&close_ctx, &id);
                }
                if any {
                    refresh_after_tab_close(&close_ctx);
                }
            });
        }
        {
            let close_ctx = close_ctx.clone();
            window.on_pane_tab_close_all(move |pane_id: i32| {
                let to_close = tabs_to_close_all(&close_ctx.layout.borrow(), pane_id as u64);
                let any = !to_close.is_empty();
                for id in to_close {
                    close_tab_id(&close_ctx, &id);
                }
                if any {
                    refresh_after_tab_close(&close_ctx);
                }
            });
        }
    }

    // Click anywhere in a pane → focus it (drives which terminal the sidebar and
    // key routing follow). A single pane is always focused, so this is a no-op
    // until splits exist.
    {
        let weak = window.as_weak();
        let layout = layout.clone();
        let content_size = content_size.clone();
        let tabs_model = tabs_model.clone();
        let panes_model = panes_model.clone();
        let splitters_model = splitters_model.clone();
        window.on_pane_focus(move |pane_id: i32| {
            {
                let mut lay = layout.borrow_mut();
                if lay.leaf(pane_id as u64).is_some() {
                    lay.focused = pane_id as u64;
                }
            }
            if let Some(w) = weak.upgrade() {
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
    }

    // Drag a splitter to re-balance the two panes it divides. `pos` is the new
    // boundary position in content coordinates along the split's axis; we look
    // the split's axis window up from a fresh flatten and convert it to a ratio.
    {
        let weak = window.as_weak();
        let layout = layout.clone();
        let content_size = content_size.clone();
        let tabs_model = tabs_model.clone();
        let panes_model = panes_model.clone();
        let splitters_model = splitters_model.clone();
        window.on_splitter_drag(move |split_id: i32, pos: f32, _vertical: bool| {
            {
                let mut lay = layout.borrow_mut();
                let (cw, ch) = content_size.get();
                let extent = {
                    let (_, splits) = lay.flatten(0.0, 0.0, cw.max(1.0), ch.max(1.0));
                    splits
                        .iter()
                        .find(|s| s.split_id == split_id as u64)
                        .map(|s| (s.axis_start, s.axis_len))
                };
                if let Some((start, len)) = extent {
                    lay.set_ratio(split_id as u64, start, len, pos);
                }
            }
            if let Some(w) = weak.upgrade() {
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
    }

    // Split a pane: peel `tab-id` out of pane `pane-id` into a new pane on the
    // given side ("left"/"right"/"up"/"down"). Needs >1 tab so the source pane
    // doesn't empty and immediately collapse back.
    {
        let weak = window.as_weak();
        let layout = layout.clone();
        let content_size = content_size.clone();
        let tabs_model = tabs_model.clone();
        let panes_model = panes_model.clone();
        let splitters_model = splitters_model.clone();
        window.on_pane_split(
            move |pane_id: i32, tab_id: SharedString, dir: SharedString| {
                let tab_id = tab_id.to_string();
                {
                    let mut lay = layout.borrow_mut();
                    let can = lay
                        .leaf(pane_id as u64)
                        .map(|l| l.tabs.len() > 1 && l.tabs.iter().any(|t| t == &tab_id))
                        .unwrap_or(false);
                    if !can {
                        return;
                    }
                    let (d, before) = match dir.as_str() {
                        "left" => (crate::layout::Dir::Horizontal, true),
                        "right" => (crate::layout::Dir::Horizontal, false),
                        "up" => (crate::layout::Dir::Vertical, true),
                        _ => (crate::layout::Dir::Vertical, false), // "down"
                    };
                    lay.split(pane_id as u64, d, &tab_id, before);
                }
                if let Some(w) = weak.upgrade() {
                    refresh_panes(
                        &w,
                        &layout.borrow(),
                        content_size.get(),
                        &tabs_model,
                        &panes_model,
                        &splitters_model,
                    );
                }
            },
        );
    }

    // Merge a split pane back into another pane. The source pane's tabs are
    // appended to the first remaining pane, then the emptied source collapses.
    {
        let weak = window.as_weak();
        let layout = layout.clone();
        let content_size = content_size.clone();
        let tabs_model = tabs_model.clone();
        let panes_model = panes_model.clone();
        let splitters_model = splitters_model.clone();
        window.on_pane_merge(move |pane_id: i32| {
            {
                let mut lay = layout.borrow_mut();
                lay.merge_leaf_into_other(pane_id as u64);
            }
            if let Some(w) = weak.upgrade() {
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
    }

    // Drag-to-split: while a tab is dragged over the pane area, highlight the
    // drop zone the cursor is in (an edge band → split, the middle → move).
    {
        let weak = window.as_weak();
        let layout = layout.clone();
        let content_size = content_size.clone();
        window.on_tab_drag_move(move |_tab_id: SharedString, x: f32, y: f32| {
            if let Some(w) = weak.upgrade() {
                match drag_target(&layout.borrow(), content_size.get(), x, y) {
                    Some((_, _, (hx, hy, hw, hh))) => {
                        w.set_drag_active(true);
                        w.set_drag_hl_x(hx);
                        w.set_drag_hl_y(hy);
                        w.set_drag_hl_w(hw);
                        w.set_drag_hl_h(hh);
                    }
                    None => w.set_drag_active(false),
                }
            }
        });
    }

    // Drop: split the target pane toward the dropped-on edge (peeling the tab
    // into the new pane), or drop into another pane's tab group from the middle
    // / tab strip (IDEA-style merge by dragging onto the tab row).
    {
        let weak = window.as_weak();
        let layout = layout.clone();
        let content_size = content_size.clone();
        let tabs_model = tabs_model.clone();
        let panes_model = panes_model.clone();
        let splitters_model = splitters_model.clone();
        window.on_tab_drag_drop(move |tab_id: SharedString, x: f32, y: f32| {
            let tab_id = tab_id.to_string();
            let target = drag_target(&layout.borrow(), content_size.get(), x, y);
            if let Some((pane, zone, _)) = target {
                let mut lay = layout.borrow_mut();
                let src = lay.leaf_of_tab(&tab_id);
                match zone {
                    "left" => {
                        lay.split(pane, crate::layout::Dir::Horizontal, &tab_id, true);
                    }
                    "right" => {
                        lay.split(pane, crate::layout::Dir::Horizontal, &tab_id, false);
                    }
                    "up" => {
                        lay.split(pane, crate::layout::Dir::Vertical, &tab_id, true);
                    }
                    "down" => {
                        lay.split(pane, crate::layout::Dir::Vertical, &tab_id, false);
                    }
                    "tabstrip" => {
                        if src != Some(pane) {
                            lay.move_tab(&tab_id, pane);
                        }
                    }
                    _ => {
                        if src != Some(pane) {
                            lay.move_tab(&tab_id, pane);
                        }
                    }
                }
            }
            if let Some(w) = weak.upgrade() {
                w.set_drag_active(false);
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
    }
}

// ---------------------------------------------------------------------------
// SFTP callbacks
// ---------------------------------------------------------------------------
