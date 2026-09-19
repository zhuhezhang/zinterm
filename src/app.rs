//! Top-level UI state machine.
//!
//! Responsibilities:
//!   * Load the config store and expose sessions to the UI.
//!   * Manage the tab list + per-tab `SessionHandle` map.
//!   * Route Slint callbacks to the right domain module.
mod auth_dialogs;
mod command_bar_callbacks;
mod layout_prefs;
mod main_surface;
mod pointer_geometry;
mod quick_commands;
mod render_throttle;
mod send_key;
mod session_dialog_connect;
mod session_event;
mod session_models;
mod session_runtime;
mod session_tree_callbacks;
mod settings_callbacks;
mod sftp_callbacks;
mod sftp_ui;
mod small_helpers;
mod tab_callbacks;
mod terminal_ops_callbacks;
mod terminal_ui;
mod window;
mod window_events;

use self::auth_dialogs::*;
use self::command_bar_callbacks::*;
use self::layout_prefs::*;
use self::main_surface::*;
use self::pointer_geometry::*;
use self::quick_commands::*;
use self::render_throttle::*;
use self::send_key::*;
use self::session_dialog_connect::*;
use self::session_event::*;
use self::session_models::*;
use self::session_runtime::*;
use self::session_tree_callbacks::*;
use self::settings_callbacks::*;
use self::sftp_callbacks::*;
use self::sftp_ui::*;
use self::small_helpers::*;
use self::tab_callbacks::*;
use self::terminal_ops_callbacks::*;
use self::terminal_ui::*;
use self::window::*;
use self::window_events::*;

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::rc::Rc;
use std::sync::{Arc, Mutex};

#[cfg(test)]
#[path = "../tests/app/terminal_ingest/mod.rs"]
mod ingest_frame_tests;

use anyhow::{Context, Result};
use i_slint_backend_winit::WinitWindowAccessor;
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};
use tokio::runtime::Runtime;

use crate::config::{
    group_join, group_parent_path, group_path_segment, is_reserved_session_group,
    is_valid_group_segment, AuthMethod, ConfigStore, OutputHighlightRule, SaveKind, Secret,
    Session, SessionKind,
};
use crate::i18n::t;
use crate::layout::{LogicalRect, TerminalWheelHit};
use crate::session::{ConnectCtx, PendingCred, PendingHostKey, TabStatus, TabStatuses};
use crate::sftp::{download_target_path, spawn_sftp, DownloadConflict, SftpHandles, SftpLastCwd};
use crate::ssh::{
    format_mtime, format_size, spawn_session, AlgorithmCategory, SessionCommand, SessionEvent,
    SessionHandle,
};
#[cfg(windows)]
use crate::terminal::c0_letter_key_down;
use crate::terminal::{
    apply_backspace_mode, bare_ctrl_marker_workaround_enabled, compile_output_rules,
    compute_find_matches, encode_command_bar_input, encode_pasted_text, key_to_pty_bytes,
    normalize_backspace_mode, paste_requires_large_review, should_drop_bare_ctrl_marker,
    terminal_uses_bracketed_paste, CsiState, FindOptions, OutputHighlightPreset, RenderGates,
    TabRenderGate, TermBuffer, TermBufferHandle, TermBuffers,
};
#[cfg(test)]
use crate::terminal::{
    build_row, highlight_plain_output, log_level_marker, normalize_pasted_newlines,
    text_cell_width, vt_span_colors, CompiledOutputRule, HistSpan, Line,
};
#[cfg(any(target_os = "windows", test))]
use crate::terminal::{windows_process_ctrl_release, CtrlKeySide};
use crate::ui::*;

pub fn run() -> Result<()> {
    // Load the renderer preference before creating any Slint window. Reuse the
    // same store for the rest of the app so startup does not read the config
    // twice merely to select a backend (#280).
    let config = ConfigStore::load().context("failed to load config")?;

    // Windows frameless-window attributes must be fixed before the first Slint
    // window is created; doing it afterwards leaves some Win10 machines with an
    // invisible frame that shifts mouse hit testing (#193).
    #[cfg(windows)]
    setup_windows_platform(config.renderer_mode());

    #[cfg(target_os = "linux")]
    setup_linux_platform(config.renderer_mode());

    // Immersive native title bar on macOS (must precede the first window).
    #[cfg(target_os = "macos")]
    setup_macos_platform(config.renderer_mode());

    // --- Runtime + store -------------------------------------------------
    let runtime = Arc::new(Runtime::new().context("failed to start tokio runtime")?);
    let store = Rc::new(RefCell::new(config));
    // Reachable from the Slint-thread event handler for recording terminal
    // commands into history (#113).
    HISTORY_STORE.with(|s| *s.borrow_mut() = Some(store.clone()));

    // Per-tab SSH handles (shell only; lives on Slint thread via Rc).
    let handles: Rc<RefCell<HashMap<String, SessionHandle>>> =
        Rc::new(RefCell::new(HashMap::new()));

    // Per-tab SFTP handles — Arc<Mutex> so the event-pump OS thread and the
    // Slint UI thread can both post SftpCommands.
    let sftp_handles: SftpHandles = Arc::new(Mutex::new(HashMap::new()));
    // Per-tab cwd the SFTP panel last followed (see SftpLastCwd).
    let sftp_last_cwd: SftpLastCwd = Arc::new(Mutex::new(HashMap::new()));

    // Per-tab vt100 parsers + history logs (Arc<Mutex> so they can be cloned
    // into the thread that pumps session events into invoke_from_event_loop).
    let bufs: TermBuffers = Arc::new(Mutex::new(HashMap::new()));
    let render_gates: RenderGates = Arc::new(Mutex::new(HashMap::new()));

    // Last-known terminal pixel dimensions, updated by every terminal-resize
    // callback.  Shared so on_connect_session can pass a sensible initial PTY
    // size to spawn_session before the first resize callback fires.
    // Default: 80 cols × 24 rows (SSH spec minimum).
    let last_term_size: Arc<Mutex<(u32, u32)>> = Arc::new(Mutex::new((80, 24)));

    // --- Build window + models ------------------------------------------
    // Set the Wayland app_id / X11 WM_CLASS *before* the window is created so
    // the Linux desktop shell can match the running window to the installed
    // `zinterm.desktop` entry and show our icon in the dock/taskbar.  (On
    // Windows the icon comes from the embedded .ico, so this is a no-op there.)
    let _ = slint::set_xdg_app_id("zinterm");
    let window = AppWindow::new().context("failed to build Slint window")?;
    // Slint applies preferred-width/height while the native window is being
    // created. Do not treat those startup Resized events as user adjustments;
    // otherwise they overwrite the persisted size before restoration (#278).
    let window_size_tracking_ready = Rc::new(Cell::new(false));
    let pending_window_size_restore = Rc::new(Cell::new(None::<(f32, f32)>));

    // Show the crate version (from Cargo.toml at compile time) in About.
    window.set_app_version(env!("CARGO_PKG_VERSION").into());

    // Set the window icon from the PNG embedded in the binary so the dock
    // shows the correct icon even without a system-installed .desktop entry
    // (e.g. AppImage without AppImageLauncher, or plain binary in ~/bin).
    #[cfg(target_os = "linux")]
    set_window_icon(&window);

    // The window defaults to frameless + custom title bar (#119). macOS keeps
    // its native decorations, so turn the custom bar off there.
    #[cfg(target_os = "macos")]
    window.set_custom_titlebar(false);

    // Apply the saved UI language preference (auto / zh / en).  The Rust-side
    // flag drives `i18n::t(...)`; `apply_to_slint` selects the bundled `.po`
    // for the static `@tr(...)` text (must run after the first component exists).
    {
        let pref = store.borrow().language().to_string();
        crate::i18n::set_language(&pref);
        crate::i18n::apply_to_slint();
        window.set_language_pref(pref.into());
        window.set_lang_en(crate::i18n::is_en());
    }

    // Apply the saved (or system-detected) theme.
    // "dark" / "light" → use that directly; "system" or unset → ask the OS;
    // OS unknown → fall back to dark.
    {
        let is_dark = theme_pref_is_dark(&store.borrow());
        window.set_dark_mode(is_dark);
    }
    // On macOS, app shortcuts use Cmd (⌘) so physical Ctrl stays free for the
    // shell (#158); on Windows/Linux they stay Ctrl-based.
    window.set_is_mac(cfg!(target_os = "macos"));
    window.set_is_windows(cfg!(windows));

    // Apply the saved terminal font (Interface settings). An empty family keeps
    // the built-in default; the size always applies (defaults to 13).
    {
        let s = store.borrow();
        let fam = s.font_family().to_string();
        if !fam.is_empty() {
            window.set_term_font_family(fam.into());
        }
        window.set_term_font_size(s.font_size() as f32);
        window.set_terminal_line_spacing(s.terminal_line_spacing());
        window.set_term_font_bold(s.terminal_bold());
        window.set_term_cursor_style(s.terminal_cursor_style().into());
        if let Some(color) = parse_hex_color(s.terminal_cursor_color()) {
            window.set_term_cursor_color_hex(s.terminal_cursor_color().into());
            window.set_term_cursor_color(color);
        }
        window.set_output_highlight_enabled(s.output_highlight_enabled());
        window.set_json_format_output(s.json_format_output());
        window.set_output_highlight_preset(s.output_highlight_preset().into());
        window.set_output_highlight_rules(output_highlight_rule_model(&s));
        window.set_ui_scale(s.ui_scale() as f32 / 100.0); // global UI zoom (#100)
        window.set_panel_font(s.panel_font() as f32 / 100.0); // settings-panel font scale
        window.set_renderer_mode(s.renderer_mode().into());
    }

    // Apply the saved immersive wallpaper (overrides dark/light when set; a
    // missing custom file falls back to the plain theme).
    {
        let id = store.borrow().wallpaper().to_string();
        // Restoring a saved wallpaper must not override the user's persisted
        // light/dark preference. Built-in wallpapers only suggest their paired
        // theme when the user actively selects them (#theme-persistence).
        apply_wallpaper(&window, &store.borrow(), &bufs, &id, false);
    }
    // Editable inputs (e.g. the SFTP path bar) need a CJK-capable font: the
    // embedded mono font has no Chinese glyphs and native TextInput doesn't
    // glyph-fallback like Text does, so typed Chinese would render as tofu (#54).
    //
    // We must NOT hard-code one system font name: on macOS 26 (Tahoe) fontdb
    // failed to register "PingFang SC", so the UI default font resolved to nothing
    // and *all* text vanished (#129) — icons survived only because they use an
    // embedded font. Instead probe what fontdb actually loaded and pick the first
    // resolvable CJK family, falling back to the embedded "MeatShell Mono" so the
    // window is never fully blank even when the system font DB is unreadable.
    window.set_ui_font_family(resolve_ui_font_family());
    // Populate the Interface font picker with installed monospace families.
    window.set_term_fonts(ModelRc::from(Rc::new(VecModel::from(
        system_monospace_fonts(),
    ))));

    // Command bar (#55): seed quick commands + history from the config. Groups
    // start collapsed by default (#55).
    let initial_collapsed = all_quick_group_names(&store.borrow());
    sync_quick_command_models(&window, &store.borrow(), &initial_collapsed, "", "");
    window.set_command_history(history_model(&store.borrow()));
    window.set_history_view(history_view_model(&store.borrow(), "")); // #101

    // Interface setting: SFTP follows the terminal's cd. The shell event pumps
    // read this AtomicBool on every CwdChanged, so toggling applies live to
    // already-open sessions too.

    let (sftp_follow_cd, ssh_keepalive_secs, ssh_algorithm_prefs) =
        wire_settings_callbacks(&window, &store, &bufs, &pending_window_size_restore);

    wire_main_surface(
        &window,
        store.clone(),
        bufs.clone(),
        handles.clone(),
        sftp_handles.clone(),
        sftp_last_cwd,
        render_gates,
        runtime,
        last_term_size,
        sftp_follow_cd,
        ssh_keepalive_secs,
        ssh_algorithm_prefs,
    );

    run_window_events(
        window,
        store,
        bufs,
        handles,
        sftp_handles,
        pending_window_size_restore,
        window_size_tracking_ready,
    )
}

#[allow(clippy::too_many_arguments)]
fn wire_session_callbacks(
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
) {
    wire_session_tree(
        window,
        store.clone(),
        sessions_model.clone(),
        welcome_session_query.clone(),
    );
    wire_session_dialog(
        window,
        store,
        sessions_model,
        welcome_session_query,
        tabs_model,
        terminals_model,
        layout,
        content_size,
        panes_model,
        splitters_model,
        handles,
        bufs,
        render_gates,
        runtime,
        last_term_size,
        sftp_handles,
        sftp_last_cwd,
        tab_statuses,
        sftp_follow_cd,
        ssh_keepalive_secs,
        ssh_algorithm_prefs,
    );
}

fn wire_key_input(
    window: &AppWindow,
    handles: Rc<RefCell<HashMap<String, SessionHandle>>>,
    bufs: TermBuffers,
    last_term_size: Arc<Mutex<(u32, u32)>>,
    store: Rc<RefCell<ConfigStore>>,
    tabs_model: Rc<VecModel<TabInfo>>,
    ctx: ConnectCtx,
) {
    wire_command_bar(window, handles.clone(), store.clone());
    wire_send_key(
        window,
        handles.clone(),
        bufs.clone(),
        store.clone(),
        ctx.clone(),
    );
    wire_terminal_ops(window, handles, bufs, last_term_size, store, tabs_model);
}

#[cfg(test)]
#[path = "../tests/app/welcome_sidebar/mod.rs"]
mod welcome_sidebar_tests;

#[cfg(test)]
#[path = "../tests/app/terminal_input/mod.rs"]
mod key_tests;

#[cfg(test)]
#[path = "../tests/app/terminal_rendering/mod.rs"]
mod selection_tests;

#[cfg(test)]
#[path = "../tests/app/output_highlighting/mod.rs"]
mod log_highlight_tests;
