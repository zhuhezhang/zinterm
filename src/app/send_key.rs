use super::*;

/// Convert a Slint `KeyEvent.text` + modifier flags into the byte sequence
/// that the remote PTY expects.
///
/// Slint uses Unicode Private Use Area (`\u{F700}`…) for special keys.
/// Regular printable characters and C0 control characters are passed as-is.
///
/// Render a key string for diagnostic logs WITHOUT leaking its content (#15).
///
/// Any printable character could be a password character, so we never emit it.
/// Only C0/C1 control code points (Backspace, Esc, the IME-injected 0x10/0x15
/// markers, …) are revealed — those are exactly what the Shift/Backspace IME
/// diagnostics need and are never password material. Printable characters are
/// collapsed to a count, so the logs stay useful without exposing keystrokes.
pub(super) fn redact_key(key: &str) -> String {
    if key.is_empty() {
        return "(empty)".to_string();
    }
    let mut parts: Vec<String> = Vec::new();
    let mut printable = 0usize;
    for c in key.chars() {
        let cp = c as u32;
        if cp < 0x20 || (0x7f..=0x9f).contains(&cp) {
            parts.push(format!("U+{cp:04X}"));
        } else {
            printable += 1;
        }
    }
    if printable > 0 {
        parts.push(format!("<{printable} printable redacted>"));
    }
    parts.join(",")
}

/// macOS/IME combinations may report bare physical Control as a C0 character:
/// U+0017 opens nano search before Ctrl+X (#312), while U+0008 is encoded as
/// Backspace and deletes the preceding character during Ctrl+Space (#348).
pub(super) fn should_drop_macos_bare_ctrl_marker(key: &str, ctrl: bool, is_macos: bool) -> bool {
    is_macos
        && ctrl
        && matches!(
            key.chars().collect::<Vec<_>>().as_slice(),
            ['\u{0008}'] | ['\u{0017}']
        )
}

// `app_cursor` mirrors the remote terminal's DECCKM mode (`\x1b[?1h/l`):
// when true the four arrow keys must use SS3 sequences (`\x1bOA`…) instead
// of the default CSI sequences (`\x1b[A`…).  Full-screen apps like nano and
// vim set this mode on startup.
pub(super) fn wire_send_key(
    window: &AppWindow,
    handles: Rc<RefCell<HashMap<String, SessionHandle>>>,
    bufs: TermBuffers,
    store: Rc<RefCell<ConfigStore>>,
    ctx: ConnectCtx,
) {
    // Forward each keystroke as raw bytes to the SSH PTY. The server's bash /
    // readline handles echo, history (↑↓), Tab completion, Ctrl+C, etc.
    {
        // Capture Slint's raw modifier mapping before app-shortcut routing.
        // WARN is deliberate: packaged builds persist WARN+ to monthly error logs.
        window.on_diagnose_key_event(
            move |tab_id: SharedString,
                  key: SharedString,
                  raw_control: bool,
                  raw_meta: bool,
                  alt: bool,
                  shift: bool| {
                if cfg!(target_os = "macos")
                    && (raw_control
                        || raw_meta
                        || key.chars().any(|c| (0x10..=0x18).contains(&(c as u32))))
                {
                    tracing::warn!(
                        "[KEY_DIAG_312] stage=slint tab={} key={} raw_control={} raw_meta={} alt={} shift={}",
                        tab_id,
                        redact_key(key.as_str()),
                        raw_control,
                        raw_meta,
                        alt,
                        shift
                    );
                }
            },
        );

        let handles = handles.clone();
        let bufs = bufs.clone();
        let store_send = store.clone();
        // Shared timestamp: the last time the Shift key alone was pressed
        // (key="", shift=true).  Used by the time-based Backspace filter below.
        let last_shift_time: Arc<Mutex<Option<std::time::Instant>>> = Arc::new(Mutex::new(None));
        window.on_send_key(move |tab_id: SharedString, key: SharedString, ctrl: bool, alt: bool, shift: bool| {
            // ── R on a disconnected tab → reconnect in place (#79) ─────────
            // FinalShell-style: the tab shows "按 R 重新连接";
            // pressing R re-spawns the shell + SFTP workers in the SAME tab,
            // keeping scrollback and prior output intact.
            if key.eq_ignore_ascii_case("r") && !ctrl && !alt {
                let dead_session = {
                    let statuses = ctx.tab_statuses.lock().unwrap();
                    statuses
                        .get(tab_id.as_str())
                        .filter(|st| st.state == 2)
                        .map(|st| st.session_id.clone())
                };
                if let Some(session_id) = dead_session {
                    let Some(mut session) = store_send.borrow().get(&session_id).cloned() else {
                        return;
                    };
                    // Prefer this tab's in-memory password cache; do not use the
                    // disk-saved password for R-reconnect.
                    apply_cached_credentials_for_reconnect(&mut session, tab_id.as_str());
                    // Drop the dead shell/SFTP handles for this tab.
                    ctx.handles.borrow_mut().remove(tab_id.as_str());
                    if let Some(h) =
                        ctx.sftp_handles.lock().unwrap().remove(tab_id.as_str())
                    {
                        h.close();
                    }
                    // Snap back to the live bottom; new shell output appends below.
                    if let Some(h) = term_buf(&ctx.bufs, tab_id.as_str()) {
                        let mut b = h.lock().unwrap();
                        b.view_offset = 0;
                        b.sel_anchor = None;
                        b.sel_focus = None;
                        b.sel_ranges.clear();
                    }
                    if let Some(st) =
                        ctx.tab_statuses.lock().unwrap().get_mut(tab_id.as_str())
                    {
                        st.state = 0;
                    }
                    // Fresh session: the first OSC 7 after reconnect follows.
                    ctx.sftp_last_cwd.lock().unwrap().remove(tab_id.as_str());
                    start_session_in_tab(tab_id.as_str(), session, &ctx);
                    return;
                }
            }
            // Check whether the remote PTY switched to application cursor mode
            // (DECCKM, set by nano/vim via \x1b[?1h). In that mode the terminal
            // must send \x1bOA/B/C/D instead of \x1b[A/B/C/D.
            let app_cursor = if let Some(h) = term_buf(&bufs, tab_id.as_str()) {
                let mut b = h.lock().unwrap();
                // Typing snaps the view back to the live bottom so the
                // user always sees what they're entering.
                b.view_offset = 0;
                b.parser.screen().application_cursor()
            } else {
                false
            };
            // Never log the raw key string — it can be a password character
            // (#15). redact_key keeps control codes but masks printable text.
            tracing::debug!(
                "send_key tab={} key={} ctrl={} alt={} shift={} app_cursor={}",
                tab_id, redact_key(key.as_str()), ctrl, alt, shift, app_cursor
            );

            // ── Shift / Backspace 诊断日志 (info 级, 无需 RUST_LOG=debug) ─────
            // 每个 Shift 相关事件都打印 key 的 Unicode 码位，方便对比
            // 左Shift / 右Shift 是否产生不同的 key 字符串。
            if shift || key.as_str() == "\u{0008}" {
                // INFO level (no RUST_LOG needed) — must not leak the key text.
                // redact_key reveals only control code points (the IME markers
                // this diagnostic cares about), masking any printable char that
                // could be part of a Shift-typed password symbol (#15).
                let codepoints = redact_key(key.as_str());
                let elapsed_ms = last_shift_time
                    .lock()
                    .unwrap()
                    .map(|t| format!("{}ms ago", t.elapsed().as_millis()))
                    .unwrap_or_else(|| "never".to_string());
                tracing::info!(
                    "[KEY_DIAG] key={} shift={} ctrl={} alt={} | last_shift={}",
                    codepoints, shift, ctrl, alt, elapsed_ms
                );
            }

            // ── Track lone-Shift presses for the time-based Backspace filter ──
            // Slint sends key="" (empty string) when a bare modifier key (Shift,
            // Ctrl, Alt) is pressed.  We record the timestamp whenever Shift
            // alone fires so the filter below can catch IME-injected Backspace
            // events even if they arrive with shift=false.
            if key.as_str().is_empty() && shift && !ctrl && !alt {
                *last_shift_time.lock().unwrap() = Some(std::time::Instant::now());
                tracing::info!("[KEY_DIAG] lone-Shift recorded → timestamp saved");
            }

            // ── 拦截百度拼音注入的 Shift 标记字符（核心修复）────────────────────
            // 诊断日志证实，百度拼音通过 WH_KEYBOARD_LL 钩子，在 Shift 键按下时
            // 向消息队列注入一个 C0 控制字符，而非空字符串：
            //
            //   左 Shift → U+0015 (Ctrl+U / NAK), shift=true, ctrl=false
            //   右 Shift → U+0010 (Ctrl+P / DLE), shift=true, ctrl=false
            //              紧接着注入: U+0008 (Backspace), shift=false
            //
            // 这些字符绝对不应送入 PTY：
            //   0x15 (Ctrl+U) 在 bash/vim 中会清空当前输入行 → "左Shift替换字符"
            //   0x10 (Ctrl+P) 在 vim 中翻历史/触发补全     → "右Shift乱跳"
            //   0x08 (Backspace) 紧随其后                   → "右Shift删除字符"
            //
            // 合法独立 C0 键（Backspace=0x08, Tab=0x09, LF=0x0A, CR=0x0D,
            // ESC=0x1B）不受此过滤影响，由下方代码单独处理。
            //
            // 检测到 IME Shift 标记后，记录时间戳，让 Layer 2 在 1500ms 内
            // 拦截随后可能到来的 Backspace（右Shift场景，日志显示间隔约 914ms）。
            if !ctrl && !alt {
                if let Some(c) = key.as_str().chars().next() {
                    let cp = c as u32;
                    let is_standalone = matches!(cp, 0x08 | 0x09 | 0x0A | 0x0D | 0x1B);
                    if key.as_str().chars().count() == 1
                        && (0x01..=0x1f).contains(&cp)
                        && !is_standalone
                    {
                        *last_shift_time.lock().unwrap() = Some(std::time::Instant::now());
                        tracing::info!(
                            "[KEY_DIAG] DROPPED IME C0 marker U+{:04X} (shift={}) → timestamp saved",
                            cp, shift
                        );
                        return;
                    }
                }
            }

            // ── Windows: filter synthetic Ctrl+char injections ──────────────
            // Some keyboards / IME drivers (e.g. Aula F99 + Baidu Pinyin)
            // inject a synthetic WM_CHAR 0x11 (Ctrl+Q) when Left Ctrl is
            // briefly tapped, WITHOUT sending a WM_KEYDOWN VK_Q beforehand.
            //
            // FinalShell avoids this because it builds Ctrl+letter from
            // WM_KEYDOWN (virtual-key codes).  Slint uses WM_CHAR, so it
            // sees the injected byte and forwards it straight to us.
            //
            // Fix: for C0 control chars (Ctrl+A…Ctrl+Z, i.e. 0x01–0x1A),
            // use GetKeyState — which returns the key state *as of the last
            // processed message*, not the live hardware state — to verify
            // the corresponding letter VK was actually queued as a keydown
            // before this WM_CHAR arrived.  If Q was never keyed down,
            // GetKeyState(VK_Q) = 0 → the event is synthetic → drop it.
            #[cfg(windows)]
            if ctrl {
                if let Some(ch) = key.as_str().chars().next() {
                    let cp = ch as u32;
                    // Always let Enter / Tab pass through regardless of Ctrl
                    // state.  These C0 codes (0x09 Tab, 0x0a LF, 0x0d CR) are
                    // "double-duty" keys: pressing Enter while Ctrl is still
                    // physically held (e.g. just after Ctrl+O in nano) generates
                    // Ctrl+M (0x0d) with ctrl=true — but GetKeyState(VK_M) is 0
                    // because the user never pressed M.  Without this exemption
                    // the filter would silently drop the Enter, making it
                    // impossible to confirm nano's "File Name to Write:" prompt.
                    let always_pass = matches!(cp, 0x09 | 0x0a | 0x0d);
                    if !always_pass
                        && key.as_str().chars().count() == 1
                        && (0x01..=0x1a).contains(&cp)
                        && !c0_letter_key_down(cp)
                    {
                        tracing::debug!(
                            "send_key: dropped synthetic Ctrl+{} \
                             (VK_{:02X} not down per GetKeyState)",
                            (0x40u8 + cp as u8) as char,
                            cp + 0x40
                        );
                        return;
                    }
                }
            }

            // ── Filter synthetic Backspace injected by Chinese IME ────────────
            // Baidu Pinyin (and similar Chinese IMEs) hooks the keyboard at the
            // driver level via WH_KEYBOARD_LL, below Win32's ImmDisableIME.
            // When the user presses Shift to switch from Chinese to English mode
            // while a pinyin syllable is in-flight, the IME:
            //   1. Cancels the composition (discards the syllable).
            //   2. Posts WM_KEYDOWN VK_BACK + WM_CHAR 0x08 to erase whatever
            //      character it had already forwarded to the app.
            //
            // Two-layer defence:
            //
            //   Layer 1 – shift=true guard.
            //     The synthetic Backspace arrives during Shift keydown, so
            //     GetKeyState(VK_SHIFT) is still "down" → Slint reports shift=true.
            //     Drop any Backspace (0x08) arriving while Shift is flagged.
            //
            //   Layer 2 – time-based guard.
            //     Baidu Pinyin posts WM_CHAR 0x08 asynchronously, so by the time
            //     the message is dequeued Shift may already read as "up"
            //     → shift=false defeats Layer 1.
            //     Mitigation: we recorded the timestamp when the Shift key alone
            //     was pressed (key="", shift=true) a few lines above. Drop a
            //     Backspace arriving within the guarded interval unless a real
            //     intervening key has already cleared the marker.
            // Any real intervening key proves a previous Shift/IME marker is no
            // longer paired with this Backspace. Without clearing it, the broad
            // safety window drops legitimate Vim insert-mode Backspace (#319).
            if key.as_str() != "\u{0008}" && !key.as_str().is_empty() {
                *last_shift_time.lock().unwrap() = None;
            }

            if key.as_str() == "\u{0008}" && !ctrl && !alt {
                // Layer 1
                if shift {
                    tracing::info!("[KEY_DIAG] Backspace DROPPED by layer-1 (shift=true)");
                    return;
                }
                // Layer 2 — 时间窗口 1500ms
                // 日志显示百度拼音注入 U+0010(右Shift标记) 到 Backspace 之间
                // 间隔约 914ms，因此窗口设为 1500ms 以覆盖该场景。
                let (shift_just_pressed, elapsed_ms) = {
                    let guard = last_shift_time.lock().unwrap();
                    match *guard {
                        Some(t) => {
                            let ms = t.elapsed().as_millis();
                            (ms < 1500, ms)
                        }
                        None => (false, 0),
                    }
                };
                if shift_just_pressed {
                    tracing::info!(
                        "[KEY_DIAG] Backspace DROPPED by layer-2 ({}ms after IME Shift marker)",
                        elapsed_ms
                    );
                    return;
                }
                // Layer 3
                // Do not consult the live VK_BACK state here. Under UI/SSH
                // backlog the key-up can be processed before this callback, so
                // that test drops a genuine queued Backspace (#319).
                tracing::info!("[KEY_DIAG] Backspace PASSED all filters → sent to PTY");
            }

            if should_drop_bare_ctrl_marker(
                key.as_str(),
                ctrl,
                bare_ctrl_marker_workaround_enabled(),
            ) || should_drop_macos_bare_ctrl_marker(key.as_str(), ctrl, cfg!(target_os = "macos"))
            {
                tracing::debug!(
                    "send_key: dropped Slint bare Ctrl modifier marker {}",
                    redact_key(key.as_str())
                );
                if cfg!(target_os = "macos") {
                    tracing::warn!(
                        "[KEY_DIAG_312] stage=filter tab={} key={} ctrl={} alt={} shift={} action=drop_bare_marker",
                        tab_id, redact_key(key.as_str()), ctrl, alt, shift
                    );
                }
                return;
            }

            let mut bytes = key_to_pty_bytes(key.as_str(), ctrl, alt, app_cursor);
            if !bytes.is_empty() {
                // Remap DEL/BS per the saved session's backspace mode (hot-switchable).
                let (mode, kind) = {
                    let session_id = ctx
                        .tab_statuses
                        .lock()
                        .unwrap()
                        .get(tab_id.as_str())
                        .map(|st| st.session_id.clone());
                    session_id
                        .and_then(|sid| store_send.borrow().get(&sid).cloned())
                        .map(|s| (s.backspace_mode, s.kind))
                        .unwrap_or_else(|| ("auto".into(), SessionKind::Ssh))
                };
                bytes = apply_backspace_mode(bytes, &mode, kind);
            }
            if cfg!(target_os = "macos")
                && (ctrl || key.chars().any(|c| (0x10..=0x18).contains(&(c as u32))))
            {
                tracing::warn!(
                    "[KEY_DIAG_312] stage=pty tab={} key={} ctrl={} alt={} shift={} app_cursor={} encoded={} action={}",
                    tab_id,
                    redact_key(key.as_str()),
                    ctrl,
                    alt,
                    shift,
                    app_cursor,
                    redact_key(&String::from_utf8_lossy(&bytes)),
                    if bytes.is_empty() { "drop_empty" } else { "send" }
                );
            }
            // Log only the length — never the keystroke bytes, which can be
            // password characters (#15).
            tracing::debug!(
                "send_key len={} handle_exists={}",
                bytes.len(),
                handles.borrow().contains_key(tab_id.as_str()),
            );
            if !bytes.is_empty() {
                let h = handles.borrow();
                if let Some(handle) = h.get(tab_id.as_str()) {
                    if let Some(buffer) = term_buf(&bufs, tab_id.as_str()) {
                        buffer.lock().unwrap().interactive_echo_until =
                            std::time::Instant::now() + INTERACTIVE_ECHO_WINDOW;
                    }
                    handle.send_raw(bytes);
                }
            }
        });
    }
}
