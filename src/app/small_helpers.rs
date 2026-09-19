use super::*;

pub(super) fn tab_endpoint(session: &Session) -> String {
    match session.kind {
        SessionKind::Serial => session.serial_port.clone(),
        SessionKind::Local => {
            let shell = session.shell.trim();
            if !shell.is_empty() {
                shell.to_string()
            } else {
                session.working_directory.trim().to_string()
            }
        }
        _ => session.host.clone(),
    }
}

/// Canonical serial flow-control keys for new saves.
/// Accepts legacy `"software"` / `"hardware"` and display aliases.
pub(super) fn normalize_flow_control(raw: &str) -> String {
    match raw.trim().to_ascii_lowercase().as_str() {
        "xonxoff" | "software" | "xon/xoff" => "xonxoff".to_string(),
        "rtscts" | "hardware" | "rts/cts" => "rtscts".to_string(),
        "dsrdtr" | "dsr/dtr" => "dsrdtr".to_string(),
        _ => "none".to_string(),
    }
}

/// Dialog combo labels for flow control (must match `dialogs/session_dialog.slint` items).
pub(super) fn flow_control_display(raw: &str) -> String {
    match normalize_flow_control(raw).as_str() {
        "xonxoff" => "Xon/Xoff".to_string(),
        "rtscts" => "Rts/Cts".to_string(),
        "dsrdtr" => "Dsr/Dtr".to_string(),
        _ => "None".to_string(),
    }
}

pub(super) fn normalize_parity(raw: &str) -> String {
    match raw.trim().to_ascii_lowercase().as_str() {
        "odd" => "odd".to_string(),
        "even" => "even".to_string(),
        "mark" => "mark".to_string(),
        "space" => "space".to_string(),
        _ => "none".to_string(),
    }
}

/// Dialog combo labels for parity (must match `dialogs/session_dialog.slint` items).
pub(super) fn parity_display(raw: &str) -> String {
    match normalize_parity(raw).as_str() {
        "odd" => "Odd".to_string(),
        "even" => "Even".to_string(),
        "mark" => "Mark".to_string(),
        "space" => "Space".to_string(),
        _ => "None".to_string(),
    }
}

pub(super) fn tab_title_by_id(tabs_model: &VecModel<TabInfo>, tab_id: &str) -> String {
    use slint::Model as _;
    (0..tabs_model.row_count())
        .find_map(|i| {
            let row = tabs_model.row_data(i)?;
            (row.id.as_str() == tab_id).then(|| row.title.to_string())
        })
        .unwrap_or_else(|| tab_id.to_string())
}

pub(super) fn filename_safe_segment(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '<' | '>' | '"' | '|' | '?' | '*' => '_',
            c if (c as u32) < 0x20 => '_',
            c => c,
        })
        .collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        "terminal".to_string()
    } else {
        trimmed.to_string()
    }
}

pub(super) fn terminal_output_default_filename(tab_title: &str) -> String {
    use chrono::Local;
    let stamp = Local::now().format("%Y%m%d_%H%M%S");
    format!("{}_{}.txt", stamp, filename_safe_segment(tab_title))
}

pub(super) fn should_block_close(exit_confirmed: bool, has_live_sessions: bool) -> bool {
    !exit_confirmed && has_live_sessions
}

/// Tab ids currently shown in a pane (`term.id == pane.active-id` in Slint).
pub(super) fn visible_tab_ids(win: &AppWindow) -> HashSet<String> {
    use slint::Model as _;
    let mut out = HashSet::new();
    let panes = win.get_panes();
    if let Some(pm) = panes.as_any().downcast_ref::<VecModel<PaneInfo>>() {
        for i in 0..pm.row_count() {
            if let Some(pane) = pm.row_data(i) {
                out.insert(pane.active_id.to_string());
            }
        }
    }
    out
}

pub(super) fn sync_quick_command_models(
    win: &AppWindow,
    store: &crate::config::ConfigStore,
    collapsed: &HashSet<String>,
    popup_query: &str,
    manage_query: &str,
) {
    win.set_quick_commands(quick_cmd_model(store, collapsed));
    win.set_quick_view(quick_cmd_view_model(store, collapsed, popup_query));
    win.set_qcm_manage_commands(quick_cmd_view_model(store, collapsed, manage_query));
}

/// Enumerate installed monospace font families for the Interface font picker.
/// Terminals want fixed-width fonts, so non-monospace families are filtered out.
/// Choose a UI font family that fontdb can actually resolve, falling back to the
/// embedded "MeatShell Mono" when the system font database is empty/unreadable.
///
/// macOS 26 (Tahoe) shipped a system where fontdb couldn't register the named
/// CJK font ("PingFang SC"), so hard-coding that name made the whole UI render
/// blank (#129). This probes the loaded faces and picks the first CJK-capable
/// family that exists; if none do, it returns the embedded font so the window is
/// still visible (Latin text shows; CJK may tofu — far better than a blank UI).
///
/// Emits a one-line WARN summary (faces loaded + chosen font) so the choice lands
/// in the monthly error log for diagnostics without needing RUST_LOG.
pub(super) fn resolve_ui_font_family() -> slint::SharedString {
    use fontdb::{Database, Family, Query, Stretch, Style, Weight};

    // Diagnostic / escape hatch (#129): force a specific UI font without a rebuild.
    // e.g. ZINTERM_UI_FONT="MeatShell Mono" to test whether the embedded font
    // renders when system fonts don't. Empty value is ignored.
    if let Some(f) = std::env::var_os("ZINTERM_UI_FONT") {
        let f = f.to_string_lossy().into_owned();
        if !f.trim().is_empty() {
            tracing::debug!(font = %f, "ui-font: overridden via ZINTERM_UI_FONT");
            return f.into();
        }
    }

    let mut db = Database::new();
    db.load_system_fonts();
    let face_count = db.faces().count();

    // CJK-capable system families, most-preferred first, per platform. The UI
    // default font must cover CJK because TextInput doesn't glyph-fallback (#54).
    //
    // macOS note (#129): the modern system CJK fonts (PingFang SC, Hiragino) fail
    // to rasterize under femtovg on some macOS 26 machines — fontdb finds them but
    // every glyph comes out blank. The older Heiti/Songti faces render fine and
    // ship on every macOS, so we prefer them and keep PingFang only as a late
    // fallback. (Verified on an M2/macOS 26: Heiti SC/STHeiti/Songti SC render,
    // PingFang/Hiragino don't.) Power users can still force one via
    // ZINTERM_UI_FONT. Heiti SC is a clean sans-serif (better for UI than the
    // serif Songti), so it leads.
    #[cfg(target_os = "macos")]
    let candidates: &[&str] = &[
        "Heiti SC",
        "STHeiti",
        "Songti SC",
        "PingFang SC",
        "Hiragino Sans GB",
    ];
    #[cfg(target_os = "windows")]
    let candidates: &[&str] = &["Microsoft YaHei UI", "Microsoft YaHei", "SimHei", "SimSun"];
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let candidates: &[&str] = &[
        "Noto Sans CJK SC",
        "Noto Sans CJK",
        "Source Han Sans SC",
        "WenQuanYi Micro Hei",
        "Droid Sans Fallback",
    ];

    for name in candidates {
        let q = Query {
            families: &[Family::Name(name)],
            weight: Weight::NORMAL,
            stretch: Stretch::Normal,
            style: Style::Normal,
        };
        if db.query(&q).is_some() {
            tracing::debug!(
                faces = face_count,
                font = name,
                "ui-font: using system CJK font"
            );
            return (*name).into();
        }
    }

    // No preferred family resolved. List what *is* available (if anything) so the
    // log shows whether enumeration is empty or just missing our candidates (#129).
    if face_count > 0 {
        let mut fams: Vec<String> = db
            .faces()
            .filter_map(|f| f.families.first().map(|(n, _)| n.clone()))
            .collect();
        fams.sort();
        fams.dedup();
        let sample: Vec<String> = fams.into_iter().take(40).collect();
        tracing::warn!(faces = face_count, available = ?sample,
            "ui-font: no preferred CJK font resolved; listing available families");
    }
    tracing::warn!(
        faces = face_count,
        "ui-font: falling back to embedded 'MeatShell Mono' (system fonts unusable, #129)"
    );
    "MeatShell Mono".into()
}

pub(super) fn system_monospace_fonts() -> Vec<slint::SharedString> {
    let mut db = fontdb::Database::new();
    db.load_system_fonts();
    let mut names: Vec<String> = db
        .faces()
        .filter(|f| f.monospaced)
        .filter_map(|f| f.families.first().map(|(n, _)| n.clone()))
        .collect();
    names.sort();
    names.dedup();
    // Surface the built-in glyph-complete font first so it's selectable and the
    // default selection is shown — it isn't a system face so fontdb won't list it
    // (#114).
    names.retain(|n| n != "MeatShell Mono");
    let mut out = vec![slint::SharedString::from("MeatShell Mono")];
    out.extend(names.into_iter().map(slint::SharedString::from));
    out
}

/// Parse a "vX.Y.Z" / "X.Y.Z" tag into a comparable tuple, or None if it isn't
/// a three-part numeric version. A pre-release suffix on the patch (e.g.
/// "3-rc1") is tolerated by taking its leading digits (#48).
pub(super) fn parse_version(s: &str) -> Option<(u32, u32, u32)> {
    let s = s.trim().trim_start_matches('v');
    let mut it = s.split('.');
    let major = it.next()?.parse().ok()?;
    let minor = it.next()?.parse().ok()?;
    let patch = it
        .next()?
        .split(|c: char| !c.is_ascii_digit())
        .next()?
        .parse()
        .ok()?;
    Some((major, minor, patch))
}

pub(super) fn parent_path(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return "/".to_string();
    }
    match trimmed.rfind('/') {
        Some(0) => "/".to_string(),
        Some(i) => trimmed[..i].to_string(),
        None => "/".to_string(),
    }
}
