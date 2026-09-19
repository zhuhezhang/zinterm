use super::*;

pub(super) fn set_terminal_row(
    win: &AppWindow,
    tab_id: &str,
    mutator: impl Fn(&mut TerminalState),
) {
    let terminals = win.get_terminals();
    let Some(model) = terminals.as_any().downcast_ref::<VecModel<TerminalState>>() else {
        return;
    };
    for i in 0..model.row_count() {
        if let Some(mut row) = model.row_data(i) {
            if row.id.as_str() == tab_id {
                mutator(&mut row);
                model.set_row_data(i, row);
                break;
            }
        }
    }
}

/// Build the editor's line-number gutter text: "1\n2\n…\nN", one number per line
/// of `content`, matching its (newline-separated) line count (#81).
pub(super) fn line_numbers_for(content: &str) -> String {
    use std::fmt::Write;
    let lines = content.split('\n').count().max(1);
    let mut s = String::with_capacity(lines * 4);
    for i in 1..=lines {
        if i > 1 {
            s.push('\n');
        }
        let _ = write!(s, "{i}");
    }
    s
}

/// Write `text` to the system clipboard. Call from a dedicated thread, never the
/// UI thread (arboard pumps the Win32 message loop / blocks).
///
/// On Linux the clipboard selection only persists while the owning client stays
/// alive, so we use arboard's `set().wait()`, which blocks this thread until
/// another app takes ownership — otherwise the copied text vanishes the moment
/// the `Clipboard` handle is dropped. Combined with the `wayland-data-control`
/// feature this is also what makes copy work on Wayland sessions (issue #47).
pub(super) fn clipboard_set_text(text: String) {
    #[cfg(target_os = "linux")]
    let result = {
        use arboard::SetExtLinux as _;
        arboard::Clipboard::new().and_then(|mut cb| cb.set().wait().text(text))
    };
    #[cfg(not(target_os = "linux"))]
    let result = arboard::Clipboard::new().and_then(|mut cb| cb.set_text(text));
    if let Err(e) = result {
        tracing::warn!("clipboard set_text error: {}", e);
    }
}

pub(super) fn wire_terminal_ops(
    window: &AppWindow,
    handles: Rc<RefCell<HashMap<String, SessionHandle>>>,
    bufs: TermBuffers,
    last_term_size: Arc<Mutex<(u32, u32)>>,
    store: Rc<RefCell<ConfigStore>>,
    tabs_model: Rc<VecModel<TabInfo>>,
) {
    // Propagate PTY resize to the SSH worker and vt100 parser. Pixel
    // dimensions come from Slint; we approximate col/row counts using
    // Consolas 13px metrics.
    //
    // terminal_view.slint now passes the FocusScope height (not the full
    // TerminalView height), so the SFTP panel is already excluded.
    // Layout breakdown for the FocusScope:
    //   16 px  – bottom strip (TouchArea for focus-regain)
    //    8 px  – y-offset of the output Text element inside the Flickable
    // = 24 px  total vertical chrome within FocusScope
    //
    // Consolas 13 px renders at ≈ 8 px wide × 16 px tall per cell.
    {
        let handles = handles.clone();
        let bufs_resize = bufs.clone(); // keep bufs alive for the copy handler below
        let weak_resize = window.as_weak();
        // The Slint side now measures the real Consolas cell size (via a hidden
        // probe Text) and passes whole column/row counts directly, so there is
        // no pixel→cell guesswork here.  This keeps full-screen programs like
        // nano from over-counting rows and clipping their bottom shortcut bar.
        // Debounce PTY resizes (#163): a layout reflow (a tab becoming visible,
        // the SFTP panel docking, a window drag) can momentarily report a
        // near-zero width, which collapses term-cols to its 10-col floor.
        // Applying that to the remote PTY immediately resizes the server to 10
        // columns and reflows vt100 — garbling running output (e.g. a `git clone`
        // progress meter wraps at 10 chars). Coalesce rapid changes and apply
        // only the size that's still set after a short quiet period, so a
        // transient bad value never reaches the server.
        let pending_size: Rc<RefCell<HashMap<String, (u32, u32)>>> =
            Rc::new(RefCell::new(HashMap::new()));
        let resize_debounce = Rc::new(slint::Timer::default());
        window.on_terminal_resize(move |tab_id: SharedString, cols_f: f32, rows_f: f32| {
            // A hidden terminal (inactive tab, or a split sibling not currently
            // shown) reports 0 width/height. Ignore those: flooring 0 to the 10-col
            // minimum and applying it would shrink that tab's PTY *and* poison
            // `last_term_size`, so the next connection (e.g. "Duplicate connection")
            // would start at 10 cols and wrap its first output to ~10 chars (#v0.5).
            // Only genuine, visible sizes drive a resize.
            if cols_f < 1.0 || rows_f < 1.0 {
                return;
            }
            let cols = (cols_f as u32).max(10);
            let rows = (rows_f as u32).max(5);
            pending_size
                .borrow_mut()
                .insert(tab_id.to_string(), (cols, rows));
            let pending = pending_size.clone();
            let handles = handles.clone();
            let bufs = bufs_resize.clone();
            let last = last_term_size.clone();
            let weak = weak_resize.clone();
            // (Re)arm the single-shot timer; rapid changes keep resetting it so
            // only the final, settled size is applied.
            resize_debounce.start(
                slint::TimerMode::SingleShot,
                std::time::Duration::from_millis(150),
                move || {
                    let settled: Vec<(String, (u32, u32))> = pending.borrow_mut().drain().collect();
                    for (tab, (cols, rows)) in settled {
                        tracing::debug!("terminal_resize tab={} cols={} rows={}", tab, cols, rows);
                        apply_terminal_resize(&handles, &bufs, &last, &tab, cols, rows);
                        // Re-render so the reflowed (or resized) grid shows at once
                        // instead of waiting for the next remote output (#169).
                        if let Some(win) = weak.upgrade() {
                            rebuild_tab_display(&win, &bufs, &tab);
                        }
                    }
                },
            );
        });
    }

    // Ctrl+Shift+C: copy current terminal screen to clipboard.
    {
        let bufs = bufs.clone();
        window.on_copy_terminal_text(move |tab_id: SharedString| {
            let text = term_buf(&bufs, tab_id.as_str())
                .map(|h| {
                    let buf = h.lock().unwrap();
                    // Copy the drag-selection when there is one, else the
                    // whole displayed screen.
                    let sel = buf.extract_selection_text();
                    if sel.is_empty() {
                        buf.displayed_text.join("\n")
                    } else {
                        sel
                    }
                })
                .unwrap_or_default();
            // Run the clipboard write on a dedicated OS thread.  arboard's
            // Windows backend opens the clipboard and pumps Win32 messages;
            // doing that on the Slint/winit event-loop thread re-enters the
            // message loop and dead-locks the whole UI.
            std::thread::spawn(move || clipboard_set_text(text));
        });
    }

    // Save scrollback + live screen to a user-chosen text file; result via MessageDialog.
    {
        let bufs = bufs.clone();
        let tabs_model = tabs_model.clone();
        let weak = window.as_weak();
        window.on_save_terminal_output(move |tab_id: SharedString| {
            let tid = tab_id.to_string();
            let text = term_buf(&bufs, &tid)
                .map(|h| h.lock().unwrap().extract_full_text())
                .unwrap_or_default();
            if text.is_empty() {
                if let Some(w) = weak.upgrade() {
                    w.set_ssh_import_hint(
                        t("没有可保存的终端输出。", "No terminal output to save.").into(),
                    );
                }
                return;
            }
            let tab_title = tab_title_by_id(tabs_model.as_ref(), &tid);
            let default_name = terminal_output_default_filename(&tab_title);
            if let Some(path) = rfd::FileDialog::new()
                .set_title(t("保存终端输出", "Save terminal output"))
                .set_file_name(&default_name)
                .save_file()
            {
                let weak = weak.clone();
                std::thread::spawn(move || {
                    let hint = match std::fs::write(&path, &text) {
                        Ok(()) => {
                            let path_str = path.display().to_string();
                            if crate::i18n::is_en() {
                                format!("Terminal output saved to:\n{path_str}")
                            } else {
                                format!("终端输出已保存到：\n{path_str}")
                            }
                        }
                        Err(e) => {
                            tracing::warn!("save_terminal_output: {}", e);
                            format!("{}: {}", t("保存失败", "Save failed"), e)
                        }
                    };
                    let _ = weak.upgrade_in_event_loop(move |w| {
                        w.set_ssh_import_hint(hint.into());
                    });
                });
            }
        });
    }

    // Middle-click / Ctrl+Shift+V: paste clipboard text into PTY.
    {
        let handles = handles.clone();
        let bufs = bufs.clone();
        let weak = window.as_weak();
        window.on_paste_from_clipboard(move |tab_id: SharedString| {
            // Clone the (Send) command sender for this tab so the clipboard read
            // can run off the UI thread.  Reading arboard on the event-loop
            // thread is what froze the app on middle-click / paste — see the
            // copy handler above for the deadlock explanation.
            let sender = handles
                .borrow()
                .get(tab_id.as_str())
                .map(|h| h.commands.clone());
            let Some(sender) = sender else { return };
            let bracketed = terminal_uses_bracketed_paste(&bufs, tab_id.as_str());
            let confirm_multiline = weak
                .upgrade()
                .map(|w| w.get_paste_confirm_enabled())
                .unwrap_or(true);
            let weak = weak.clone();
            let tab_id = tab_id.to_string();
            std::thread::spawn(move || {
                match arboard::Clipboard::new().and_then(|mut cb| cb.get_text()) {
                    Ok(text) => {
                        let force_review = text.len() > 100 * 1024;
                        if text.contains(['\r', '\n']) && (confirm_multiline || force_review) {
                            let large = paste_requires_large_review(&text);
                            let preview = text.clone();
                            let _ = slint::invoke_from_event_loop(move || {
                                if let Some(w) = weak.upgrade() {
                                    w.set_paste_confirm_tab(tab_id.into());
                                    w.set_paste_confirm_text(text.into());
                                    w.set_paste_confirm_preview(preview.into());
                                    w.set_paste_confirm_large(large);
                                    w.set_paste_confirm_open(true);
                                }
                            });
                        } else {
                            let bytes = encode_pasted_text(&text, bracketed);
                            let _ = sender.send(SessionCommand::RawInput(bytes));
                        }
                    }
                    Err(e) => tracing::warn!("paste_from_clipboard: clipboard error: {}", e),
                }
            });
        });
    }

    // Accept a previously reviewed multi-line paste (#262).
    {
        let handles_paste = handles.clone();
        let bufs_paste = bufs.clone();
        let weak = window.as_weak();
        window.on_paste_confirmed(move |tab_id: SharedString| {
            let Some(sender) = handles_paste
                .borrow()
                .get(tab_id.as_str())
                .map(|h| h.commands.clone())
            else {
                return;
            };
            let Some(w) = weak.upgrade() else { return };
            let text = w.get_paste_confirm_text().to_string();
            let bracketed = terminal_uses_bracketed_paste(&bufs_paste, tab_id.as_str());
            let _ = sender.send(SessionCommand::RawInput(encode_pasted_text(
                &text, bracketed,
            )));
            w.set_paste_confirm_open(false);
        });
    }

    window.on_paste_confirm_cancelled(|| {});

    // Context menu → 清空缓存: reset the local vt100 buffer (drops scrollback),
    // wipe the displayed screen, then nudge the remote to redraw a fresh prompt.
    {
        let bufs_clear = bufs.clone();
        let handles_clear = handles.clone();
        let weak = window.as_weak();
        window.on_clear_terminal(move |tab_id: SharedString| {
            let tid = tab_id.to_string();
            if let Some(h) = term_buf(&bufs_clear, &tid) {
                let mut buf = h.lock().unwrap();
                let (rows, cols) = buf.parser.screen().size();
                buf.parser = vt100::Parser::new(rows, cols, 5000);
                buf.find_query.clear();
                buf.find_options = FindOptions::default();
                buf.history = VecDeque::new(); // recycle the session scrollback
                buf.prev = Vec::new();
                buf.view_offset = 0;
                buf.sel_anchor = None;
                buf.sel_focus = None;
                buf.sel_ranges.clear();
                buf.displayed_text = Vec::new();
                buf.raw.clear();
            }
            if let Some(win) = weak.upgrade() {
                set_terminal_row(&win, &tid, |row| {
                    row.spans = ModelRc::from(Rc::new(VecModel::<TermSpan>::default()));
                    row.find_matches = ModelRc::from(Rc::new(VecModel::<TermMatch>::default()));
                    row.selection = ModelRc::from(Rc::new(VecModel::<TermMatch>::default()));
                    row.cursor_row = 0;
                    row.cursor_col = 0;
                    row.rows_used = 0;
                    row.scroll_max = 0;
                    row.scroll_offset = 0;
                });
            }
            if let Some(h) = handles_clear.borrow().get(&tid) {
                h.send_raw(vec![0x0c]); // Ctrl+L → shell clears + redraws prompt
            }
        });
    }

    // Context menu → 查找: store the query and recompute highlight rectangles.
    {
        let bufs_find = bufs.clone();
        let weak = window.as_weak();
        window.on_find_query_changed(
            move |tab_id: SharedString,
                  query: SharedString,
                  case_sensitive: bool,
                  whole_word: bool,
                  use_regex: bool| {
                let tid = tab_id.to_string();
                let q = query.to_string();
                let opts = FindOptions {
                    case_sensitive,
                    whole_word,
                    regex: use_regex,
                };
                let (matches, jumped) = with_term_buf(&bufs_find, &tid, |buf| {
                    buf.find_query = q.clone();
                    buf.find_options = opts;
                    let mut matches = compute_find_matches(&buf.displayed_text, &q, &opts);
                    let jumped = matches.is_empty() && buf.scroll_to_first_find_match(&q);
                    if jumped {
                        buf.render();
                        matches = compute_find_matches(&buf.displayed_text, &q, &opts);
                    }
                    (matches, jumped)
                })
                .unwrap_or_default();
                if let Some(win) = weak.upgrade() {
                    if jumped {
                        rebuild_tab_display(&win, &bufs_find, &tid);
                        return;
                    }
                    let model = ModelRc::from(Rc::new(VecModel::from(matches)));
                    set_terminal_row(&win, &tid, |row| {
                        row.find_matches = model.clone();
                    });
                }
            },
        );
    }

    // Mouse-wheel → scroll the scrollback history.
    {
        let bufs_scroll = bufs.clone();
        let weak = window.as_weak();
        window.on_terminal_scroll(move |tab_id: SharedString, delta: i32| {
            let tid = tab_id.to_string();
            with_term_buf(&bufs_scroll, &tid, |buf| {
                // Scroll within our own session scrollback (history lines above
                // the live screen).  Offset 0 = live bottom.
                let max_off = buf.history.len() as i64;
                let cur = buf.view_offset as i64;
                buf.view_offset = (cur + delta as i64).clamp(0, max_off) as usize;
            });
            if let Some(win) = weak.upgrade() {
                rebuild_tab_display(&win, &bufs_scroll, &tid);
            }
        });
    }

    // Wheel inside an alt-screen program (tmux / less / vim): forward it to the PTY
    // so the program scrolls, instead of doing nothing (#170 — FinalShell /
    // MobaXterm behave this way). If the app is tracking the mouse (e.g. tmux with
    // `mouse on`), send a real wheel mouse-event in the encoding it asked for;
    // otherwise fall back to arrow keys (xterm "alternate scroll"), which scrolls
    // less / man / vim.
    {
        let bufs_wheel = bufs.clone();
        let handles_wheel = handles.clone();
        window.on_terminal_wheel(move |tab_id: SharedString, dir: i32, col: i32, row: i32| {
            let tid = tab_id.to_string();
            let bytes = term_buf(&bufs_wheel, &tid).map(|h| {
                let buf = h.lock().unwrap();
                let screen = buf.parser.screen();
                if screen.mouse_protocol_mode() != vt100::MouseProtocolMode::None {
                    // 1-based cell under the cursor, clamped to the screen.
                    let (rows, cols) = screen.size();
                    let c = (col.clamp(0, cols.saturating_sub(1) as i32) as u16) + 1;
                    let r = (row.clamp(0, rows.saturating_sub(1) as i32) as u16) + 1;
                    let btn: u16 = if dir > 0 { 64 } else { 65 }; // wheel up / down
                    if screen.mouse_protocol_encoding() == vt100::MouseProtocolEncoding::Sgr {
                        format!("\x1b[<{btn};{c};{r}M").into_bytes()
                    } else {
                        // Legacy X10 encoding: ESC [ M  Cb Cx Cy  (each value + 32).
                        let cb = (btn + 32) as u8;
                        let cx = (c.min(223) + 32) as u8;
                        let cy = (r.min(223) + 32) as u8;
                        vec![0x1b, b'[', b'M', cb, cx, cy]
                    }
                } else {
                    // alternate-scroll: 3 arrow presses per notch, app-cursor aware.
                    let one: &[u8] = if dir > 0 {
                        if screen.application_cursor() {
                            b"\x1bOA"
                        } else {
                            b"\x1b[A"
                        }
                    } else if screen.application_cursor() {
                        b"\x1bOB"
                    } else {
                        b"\x1b[B"
                    };
                    one.repeat(3)
                }
            });
            if let (Some(bytes), Some(h)) = (bytes, handles_wheel.borrow().get(&tid)) {
                h.send_raw(bytes);
            }
        });
    }

    // Scrollbar drag → jump to an absolute scrollback offset (#103).
    {
        let bufs_scroll = bufs.clone();
        let weak = window.as_weak();
        window.on_terminal_scroll_to(move |tab_id: SharedString, offset: i32| {
            let tid = tab_id.to_string();
            with_term_buf(&bufs_scroll, &tid, |buf| {
                let max_off = buf.history.len() as i64;
                buf.view_offset = (offset as i64).clamp(0, max_off) as usize;
            });
            if let Some(win) = weak.upgrade() {
                rebuild_tab_display(&win, &bufs_scroll, &tid);
            }
        });
    }

    // Drag-selection lifecycle.
    {
        let bufs_sel = bufs.clone();
        let weak = window.as_weak();
        window.on_term_select_start(
            move |tab_id: SharedString, row: i32, col: i32, ctrl: bool, shift: bool| {
                let tid = tab_id.to_string();
                with_term_buf(&bufs_sel, &tid, |buf| {
                    let (rows, cols) = buf.parser.screen().size();
                    let r = row.clamp(0, rows.saturating_sub(1) as i32) as u16;
                    let c = col.clamp(0, cols.saturating_sub(1) as i32) as u16;
                    // Anchor + focus in absolute scrollback coordinates.
                    let abs = buf.vis_to_abs(r);
                    let point = (abs, c);
                    if ctrl && !shift {
                        buf.sel_ranges.push((point, point));
                    } else if shift && !buf.sel_ranges.is_empty() {
                        let anchor = buf.sel_ranges.last().map(|range| range.0).unwrap_or(point);
                        if let Some(range) = buf.sel_ranges.last_mut() {
                            *range = (anchor, point);
                        }
                    } else {
                        buf.sel_ranges.clear();
                        buf.sel_ranges.push((point, point));
                    }
                    let (anchor, focus) = buf.sel_ranges.last().copied().unwrap_or((point, point));
                    buf.sel_anchor = Some(anchor);
                    buf.sel_focus = Some(focus);
                });
                if let Some(win) = weak.upgrade() {
                    refresh_terminal_selection(&win, &bufs_sel, &tid);
                }
            },
        );
    }
    {
        let bufs_sel = bufs.clone();
        let weak = window.as_weak();
        window.on_term_select_update(move |tab_id: SharedString, row: i32, col: i32| {
            let tid = tab_id.to_string();
            with_term_buf(&bufs_sel, &tid, |buf| {
                let (rows, cols) = buf.parser.screen().size();
                let r = row.clamp(0, rows.saturating_sub(1) as i32) as u16;
                let c = col.clamp(0, cols.saturating_sub(1) as i32) as u16;
                if buf.sel_anchor.is_some() {
                    let abs = buf.vis_to_abs(r);
                    buf.sel_focus = Some((abs, c));
                    if let Some(range) = buf.sel_ranges.last_mut() {
                        range.1 = (abs, c);
                    }
                }
            });
            if let Some(win) = weak.upgrade() {
                refresh_terminal_selection(&win, &bufs_sel, &tid);
            }
        });
    }
    {
        let bufs_sel = bufs.clone();
        let store_sel = store.clone();
        let weak = window.as_weak();
        window.on_term_select_end(move |tab_id: SharedString| {
            let tid = tab_id.to_string();
            let auto_copy = store_sel.borrow().select_copy_right_paste_enabled();
            // Extract the selected text; a zero-area selection (a plain click)
            // is cleared instead of copied.
            let text = with_term_buf(&bufs_sel, &tid, |buf| {
                // Selection endpoints are inclusive, so extracting an
                // anchor-only range returns the character under a plain click.
                // Compare coordinates instead of using extracted text as the
                // click-vs-drag signal (#319).
                if !buf.selection_has_extent() {
                    buf.sel_anchor = None;
                    buf.sel_focus = None;
                    buf.sel_ranges.clear();
                    return None;
                }
                let extracted = buf.extract_selection_text();
                if extracted.is_empty() {
                    // Zero-area selection (a plain click) → clear it.
                    buf.sel_anchor = None;
                    buf.sel_focus = None;
                    buf.sel_ranges.clear();
                    None
                } else {
                    Some(extracted)
                }
            })
            .flatten();
            match text {
                Some(t) if !t.is_empty() && auto_copy => {
                    // Auto-copy on release (select-to-copy, PuTTY style).
                    std::thread::spawn(move || clipboard_set_text(t));
                }
                _ => {}
            }
            if let Some(win) = weak.upgrade() {
                refresh_terminal_selection(&win, &bufs_sel, &tid);
            }
        });
    }
    {
        let bufs_sel = bufs.clone();
        let store_sel = store.clone();
        let weak = window.as_weak();
        window.on_term_select_word(move |tab_id: SharedString, row: i32, col: i32| {
            let tid = tab_id.to_string();
            let auto_copy = store_sel.borrow().select_copy_right_paste_enabled();
            let text = with_term_buf(&bufs_sel, &tid, |buf| {
                let (rows, cols) = buf.parser.screen().size();
                let row = row.clamp(0, rows.saturating_sub(1) as i32) as u16;
                let col = col.clamp(0, cols.saturating_sub(1) as i32) as u16;
                buf.select_word_at(row, col)
            })
            .flatten();
            if let Some(text) = text.filter(|text| !text.is_empty() && auto_copy) {
                std::thread::spawn(move || clipboard_set_text(text));
            }
            if let Some(win) = weak.upgrade() {
                refresh_terminal_selection(&win, &bufs_sel, &tid);
            }
        });
    }
    // Auto-scroll while drag-selecting past the visible top/bottom edge.  The
    // anchor is in absolute coordinates so it stays pinned no matter how far the
    // view moves; we only advance the scrollback view and re-point the focus at
    // the absolute row now sitting on the edge the mouse is parked against.
    {
        let bufs_sel = bufs.clone();
        let weak = window.as_weak();
        window.on_term_select_autoscroll(move |tab_id: SharedString, dir: i32| {
            let tid = tab_id.to_string();
            let Some(h) = term_buf(&bufs_sel, &tid) else {
                return;
            };
            {
                let mut buf = h.lock().unwrap();
                // No scrollback on the alternate screen (vim/btop own the view).
                if buf.parser.screen().alternate_screen() {
                    return;
                }
                if buf.sel_anchor.is_none() {
                    return;
                }
                let rows = buf.parser.screen().size().0;
                let last = rows.saturating_sub(1);
                let max_off = buf.history.len();
                let step = 2usize;
                // Keep the focus column the user last dragged to.
                let focus_col = buf.sel_focus.map(|f| f.1).unwrap_or(0);
                let edge_vis = if dir < 0 {
                    // Mouse above the top → reveal older lines.
                    let new_off = (buf.view_offset + step).min(max_off);
                    if new_off == buf.view_offset {
                        return; // already at the oldest line
                    }
                    buf.view_offset = new_off;
                    0u16
                } else if dir > 0 {
                    // Mouse below the bottom → move toward the live tail.
                    let new_off = buf.view_offset.saturating_sub(step);
                    if new_off == buf.view_offset {
                        return; // already at the live bottom
                    }
                    buf.view_offset = new_off;
                    last
                } else {
                    return;
                };
                let abs = buf.vis_to_abs(edge_vis);
                buf.sel_focus = Some((abs, focus_col));
                if let Some(range) = buf.sel_ranges.last_mut() {
                    range.1 = (abs, focus_col);
                }
            }
            if let Some(win) = weak.upgrade() {
                rebuild_tab_display(&win, &bufs_sel, &tid);
            }
        });
    }
}
