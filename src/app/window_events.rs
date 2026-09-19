use super::*;

pub(super) fn run_window_events(
    window: AppWindow,
    store: Rc<RefCell<ConfigStore>>,
    bufs: TermBuffers,
    handles: Rc<RefCell<HashMap<String, SessionHandle>>>,
    sftp_handles: SftpHandles,
    pending_window_size_restore: Rc<Cell<Option<(f32, f32)>>>,
    window_size_tracking_ready: Rc<Cell<bool>>,
) -> Result<()> {
    // --- Window activity, for idle-CPU throttling (#127) ----------------
    // Idle terminals shouldn't burn CPU: stop the cursor blink whenever the
    // window isn't focused (mirrors what Tabby / Windows Terminal do). The
    // winit event handler below updates this; the blink reads Theme.window-focused.
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum WinActivity {
        Active,     // focused & visible → full rate
        Background, // visible but unfocused → throttled
        Hidden,     // minimized / occluded → paused
    }
    let activity = Rc::new(std::cell::Cell::new(WinActivity::Active));
    // Once the user confirms shutdown, every subsequent native/custom close
    // request must pass through without reopening the modal. Windows Installer
    // and Restart Manager may issue more than one close request while replacing
    // the executable (#267).
    let exit_confirmed = Rc::new(Cell::new(false));

    // OS file drag-and-drop → upload to the active session's SFTP directory,
    // but only when the file is dropped over the file-list area.
    {
        use i_slint_backend_winit::winit::event::{MouseScrollDelta, WindowEvent as WEvent};
        use i_slint_backend_winit::EventResult;
        let weak = window.as_weak();
        let sh = sftp_handles.clone();
        let wheel_bufs = bufs.clone();
        let close_handles = handles.clone();
        let ev_store = store.clone();
        let ev_activity = activity.clone();
        let ev_exit_confirmed = exit_confirmed.clone();
        let ev_window_size_tracking_ready = window_size_tracking_ready.clone();
        let ev_pending_window_size_restore = pending_window_size_restore.clone();
        let mut last_cursor_logical: Option<(f32, f32)> = None;
        let mut macos_wheel_accum = 0.0_f32;
        // Track the inputs that make up WinActivity; recompute on each change.
        let mut focused = true;
        let mut minimized = false;
        let mut occluded = false;
        // Apply the Win11 rounded-corner hint once, on the first event (the HWND
        // reliably exists by then, unlike a pre-run timer) (#166).
        let mut chrome_done = false;
        window
            .window()
            .on_winit_window_event(move |_slint_window, event| {
                if !chrome_done {
                    chrome_done = true;
                    if let Some(win) = weak.upgrade() {
                        apply_window_chrome(win.window());
                    }
                }
                // Recompute window activity, push it to the shared cell, and update
                // Theme.window-focused (gates the cursor blink) (#127).
                let apply_activity = |focused: bool, minimized: bool, occluded: bool| {
                    let act = if minimized || occluded {
                        WinActivity::Hidden
                    } else if focused {
                        WinActivity::Active
                    } else {
                        WinActivity::Background
                    };
                    let prev = ev_activity.get();
                    ev_activity.set(act);
                    if let Some(win) = weak.upgrade() {
                        win.set_window_focused(act == WinActivity::Active);
                        if prev == WinActivity::Hidden && act != WinActivity::Hidden {
                            win.set_terminal_restore_cover(true);
                            let weak2 = weak.clone();
                            slint::Timer::single_shot(
                                std::time::Duration::from_millis(120),
                                move || {
                                    if let Some(w) = weak2.upgrade() {
                                        w.set_terminal_restore_cover(false);
                                    }
                                },
                            );
                        }
                    }
                };
                match event {
                    #[cfg(target_os = "windows")]
                    WEvent::KeyboardInput { event, .. } => {
                        // Microsoft IME can relabel a Ctrl key-up as Process while
                        // retaining the physical Ctrl scan code. Slint drops Process,
                        // so deliver the missing modifier release directly.
                        if let Some(side) = windows_process_ctrl_release(
                            event.state,
                            &event.logical_key,
                            &event.physical_key,
                        ) {
                            let key = match side {
                                CtrlKeySide::Left => slint::platform::Key::Control,
                                CtrlKeySide::Right => slint::platform::Key::ControlR,
                            };
                            _slint_window.dispatch_event(
                                slint::platform::WindowEvent::KeyReleased { text: key.into() },
                            );
                            tracing::debug!(
                                "restored Windows IME Process-key Ctrl release side={side:?}"
                            );
                            return EventResult::PreventDefault;
                        }
                    }
                    #[cfg(target_os = "windows")]
                    WEvent::Ime(i_slint_backend_winit::winit::event::Ime::Disabled) => {
                        // Windows emits Ime::Disabled when a composition ends, including
                        // while switching between Chinese and English input methods. The
                        // Slint winit backend intentionally ignores this notification, so
                        // after several switches the native input context can remain
                        // detached and every TextInput appears to stop accepting keys
                        // (#236). Re-associate the window with its current default IME;
                        // the focused Slint TextInput keeps owning text input as before.
                        _slint_window.with_winit_window(|window| window.set_ime_allowed(true));
                    }
                    WEvent::DroppedFile(path) => {
                        if let Some(win) = weak.upgrade() {
                            handle_file_drop(&win, &sh, path.clone());
                        }
                    }
                    WEvent::CursorMoved { position, .. } => {
                        if let Some(win) = weak.upgrade() {
                            let scale = win.window().scale_factor().max(0.01) as f64;
                            let p = position.to_logical::<f64>(scale);
                            last_cursor_logical = Some((p.x as f32, p.y as f32));
                        }
                    }
                    WEvent::MouseWheel { delta, .. } if cfg!(target_os = "macos") => {
                        let Some((x, y)) = last_cursor_logical else {
                            return EventResult::Propagate;
                        };
                        let Some(win) = weak.upgrade() else {
                            return EventResult::Propagate;
                        };
                        let wheel_lines = match delta {
                            MouseScrollDelta::LineDelta(_, dy) => dy * 3.0,
                            MouseScrollDelta::PixelDelta(p) => {
                                let scale = win.window().scale_factor().max(0.01) as f64;
                                let p = p.to_logical::<f64>(scale);
                                p.y as f32 / 18.0
                            }
                        };
                        if wheel_lines.abs() < f32::EPSILON {
                            return EventResult::Propagate;
                        }
                        macos_wheel_accum += wheel_lines;
                        let whole = macos_wheel_accum.trunc() as i32;
                        if whole == 0 {
                            return EventResult::Propagate;
                        }
                        macos_wheel_accum -= whole as f32;
                        if handle_macos_terminal_wheel(&win, &wheel_bufs, x, y, whole) {
                            return EventResult::PreventDefault;
                        }
                    }
                    WEvent::Focused(f) => {
                        focused = *f;
                        apply_activity(focused, minimized, occluded);
                        if *f {
                            #[cfg(target_os = "windows")]
                            _slint_window.with_winit_window(|window| window.set_ime_allowed(true));

                            // Some window managers deliver the first Resized event
                            // before the native window belongs to a monitor. Focus
                            // is a reliable second opportunity to seed restoration;
                            // request_inner_size will produce the Resized event that
                            // verifies the native window actually reached the target.
                            if !ev_window_size_tracking_ready.get() {
                                if let Some(win) = weak.upgrade() {
                                    if is_wayland_window(win.window()) {
                                        ev_pending_window_size_restore.set(None);
                                        ev_window_size_tracking_ready.set(true);
                                        tracing::info!(
                                        "[WINDOW_SIZE] skipped persisted-size restore on Wayland"
                                    );
                                    } else if let Some(preferred) =
                                        ev_pending_window_size_restore.get()
                                    {
                                        if let Some(target) = clamp_window_size_to_monitor(
                                            win.window(),
                                            Some(preferred),
                                        ) {
                                            tracing::info!(
                                                "[WINDOW_SIZE] focus retry saved={:.0}x{:.0} \
                                             target={:.0}x{:.0}",
                                                preferred.0,
                                                preferred.1,
                                                target.0,
                                                target.1,
                                            );
                                        }
                                    }
                                }
                            }
                            refresh_revealed_main_window(weak.clone());
                        }
                    }
                    WEvent::Occluded(o) => {
                        occluded = *o;
                        apply_activity(focused, minimized, occluded);
                        if !*o {
                            refresh_revealed_main_window(weak.clone());
                        }
                    }
                    WEvent::ScaleFactorChanged { .. } => {
                        // Moving a maximized frameless window between mixed-DPI
                        // monitors can leave Win11 reporting "maximized" while the
                        // native rectangle/render surface still has the old size.
                        refresh_revealed_main_window(weak.clone());
                    }
                    WEvent::Resized(size) => {
                        // A 0-sized resize is how Windows reports a minimize; track it
                        // so idle-CPU throttling can treat the window as inactive (#127).
                        minimized = size.width == 0 || size.height == 0;
                        apply_activity(focused, minimized, occluded);
                        // Keep the maximize/restore icon (and resize-edge gating) in
                        // sync when the OS changes the window state (#119).
                        if let Some(win) = weak.upgrade() {
                            let maxed = win
                                .window()
                                .with_winit_window(|ww| ww.is_maximized())
                                .unwrap_or(false);
                            win.set_window_maximized(maxed);
                            if !ev_window_size_tracking_ready.get()
                                && is_wayland_window(win.window())
                            {
                                // The configure size in this event is authoritative
                                // on Wayland. Accept and persist that actual size;
                                // never chase the advisory saved size (#286).
                                ev_pending_window_size_restore.set(None);
                                ev_window_size_tracking_ready.set(true);
                                tracing::info!(
                                    "[WINDOW_SIZE] accepted compositor size {}x{} on Wayland",
                                    size.width,
                                    size.height
                                );
                            }
                            if !ev_window_size_tracking_ready.get() {
                                if let Some(preferred) = ev_pending_window_size_restore.get() {
                                    let scale = win.window().scale_factor().max(0.01);
                                    let actual =
                                        (size.width as f32 / scale, size.height as f32 / scale);
                                    if let Some(target) =
                                        clamp_window_size_to_monitor(win.window(), Some(preferred))
                                    {
                                        tracing::info!(
                                            "[WINDOW_SIZE] restore requested saved={:.0}x{:.0} \
                                         target={:.0}x{:.0} actual={:.0}x{:.0} scale={:.2}",
                                            preferred.0,
                                            preferred.1,
                                            target.0,
                                            target.1,
                                            actual.0,
                                            actual.1,
                                            scale,
                                        );
                                        if (actual.0 - target.0).abs() <= 2.0
                                            && (actual.1 - target.1).abs() <= 2.0
                                        {
                                            ev_pending_window_size_restore.set(None);
                                            ev_window_size_tracking_ready.set(true);
                                            center_window(&win);
                                            tracing::info!(
                                                "[WINDOW_SIZE] restore settled at {:.0}x{:.0}",
                                                actual.0,
                                                actual.1
                                            );
                                        }
                                    } else {
                                        tracing::warn!(
                                            "[WINDOW_SIZE] restore deferred: no monitor available \
                                         saved={:.0}x{:.0}",
                                            preferred.0,
                                            preferred.1,
                                        );
                                    }
                                } else {
                                    // First run: accept the initialized size as the
                                    // baseline, but do not persist this startup event.
                                    ev_window_size_tracking_ready.set(true);
                                    center_window(&win);
                                }
                                return EventResult::Propagate;
                            }
                            // Record the last user-adjusted windowed size while the
                            // resize event still carries authoritative native
                            // geometry. Persisting only during CloseRequested can
                            // observe an installer/minimize transition instead
                            // (#278). Keep writes in memory here; save_layout flushes
                            // the config on exit.
                            if ev_window_size_tracking_ready.get() && !maxed && !minimized {
                                let scale = win.window().scale_factor().max(0.01);
                                let width = size.width as f32 / scale;
                                let height = size.height as f32 / scale;
                                if width > 200.0 && height > 200.0 {
                                    ev_store.borrow_mut().set_window_size(width, height);
                                    tracing::debug!(
                                        "[WINDOW_SIZE] recorded user size {:.0}x{:.0}",
                                        width,
                                        height
                                    );
                                }
                            }
                        }
                    }
                    WEvent::CloseRequested => {
                        // Confirm before closing if there are open session tabs (#88),
                        // so a stray double-click on the title-bar icon / X / Alt+F4
                        // doesn't silently drop live sessions. Installer/Restart
                        // Manager may send repeated requests, so never intercept
                        // again after the user has confirmed shutdown (#267).
                        if should_block_close(
                            ev_exit_confirmed.get(),
                            !close_handles.borrow().is_empty(),
                        ) {
                            if let Some(win) = weak.upgrade() {
                                win.set_confirm_close_open(true);
                            }
                            return EventResult::PreventDefault;
                        }
                        ev_exit_confirmed.set(true);
                        // No sessions → the window is about to close; persist layout.
                        if let Some(win) = weak.upgrade() {
                            save_layout(&win, &ev_store);
                        }
                    }
                    _ => {}
                }
                EventResult::Propagate
            });
    }
    // Confirm-close dialog "Close" → actually quit the event loop (#88).
    {
        let weak = window.as_weak();
        let cc_store = store.clone();
        let close_handles = handles.clone();
        let close_sftp_handles = sftp_handles.clone();
        let close_exit_confirmed = exit_confirmed.clone();
        window.on_confirm_close_yes(move || {
            // Guard against a double click and against another close request
            // arriving from Windows Installer while shutdown is in progress.
            if close_exit_confirmed.replace(true) {
                return;
            }
            if let Some(w) = weak.upgrade() {
                w.set_confirm_close_open(false);
                save_layout(&w, &cc_store);
                let _ = w.hide();
            }
            // Ask every worker to stop before the runtime/event loop is torn
            // down. Clearing the maps also makes any repeated close request see
            // no live sessions and pass through immediately.
            {
                let mut sessions = close_handles.borrow_mut();
                for handle in sessions.values() {
                    handle.close();
                }
                sessions.clear();
            }
            if let Ok(mut sftp) = close_sftp_handles.lock() {
                for handle in sftp.values() {
                    handle.close();
                }
                sftp.clear();
            }
            let _ = slint::quit_event_loop();
        });
    }

    // --- Custom title-bar window controls (#119) --------------------------
    {
        let weak = window.as_weak();
        window.on_win_minimize(move || {
            if let Some(w) = weak.upgrade() {
                w.window().with_winit_window(|ww| ww.set_minimized(true));
            }
        });
    }
    {
        let weak = window.as_weak();
        window.on_win_maximize_toggle(move || {
            if let Some(w) = weak.upgrade() {
                let now = w.window().with_winit_window(|ww| {
                    let m = !ww.is_maximized();
                    ww.set_maximized(m);
                    m
                });
                if let Some(m) = now {
                    w.set_window_maximized(m);
                }
            }
        });
    }
    {
        let weak = window.as_weak();
        let close_handles = handles.clone();
        let wc_store = store.clone();
        let wc_exit_confirmed = exit_confirmed.clone();
        window.on_win_close(move || {
            if let Some(w) = weak.upgrade() {
                // Mirror the native-X behaviour: confirm if sessions are open.
                if !should_block_close(wc_exit_confirmed.get(), !close_handles.borrow().is_empty())
                {
                    wc_exit_confirmed.set(true);
                    save_layout(&w, &wc_store);
                    let _ = slint::quit_event_loop();
                } else {
                    w.set_confirm_close_open(true);
                }
            }
        });
    }
    {
        let weak = window.as_weak();
        window.on_win_drag(move || {
            if let Some(w) = weak.upgrade() {
                w.window().with_winit_window(|ww| {
                    let _ = ww.drag_window();
                });
                schedule_slint_pointer_ungrab(weak.clone());
            }
        });
    }
    {
        use i_slint_backend_winit::winit::window::ResizeDirection;
        let weak = window.as_weak();
        window.on_win_resize(move |dir: i32| {
            if let Some(w) = weak.upgrade() {
                let d = match dir {
                    0 => ResizeDirection::North,
                    1 => ResizeDirection::South,
                    2 => ResizeDirection::East,
                    3 => ResizeDirection::West,
                    4 => ResizeDirection::NorthEast,
                    5 => ResizeDirection::NorthWest,
                    6 => ResizeDirection::SouthEast,
                    _ => ResizeDirection::SouthWest,
                };
                w.window().with_winit_window(|ww| {
                    let _ = ww.drag_resize_window(d);
                });
                schedule_slint_pointer_ungrab(weak.clone());
            }
        });
    }

    // Re-center after the first frame. Size restore may settle in the Resized
    // handler first; this second pass catches the case where the HWND still
    // had a zero outer size at that moment (common on Windows before DWM
    // finishes mapping the frameless window).
    {
        let weak = window.as_weak();
        slint::Timer::single_shot(std::time::Duration::from_millis(80), move || {
            if let Some(w) = weak.upgrade() {
                center_window(&w);
            }
        });
    }

    window.run().context("event loop exited with error")?;
    Ok(())
}
