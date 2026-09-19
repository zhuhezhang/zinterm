pub(crate) fn normalize_hex_color(value: &str) -> Option<String> {
    let digits = value.trim().strip_prefix('#').unwrap_or(value.trim());
    if digits.len() != 6 || !digits.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    Some(format!("#{}", digits.to_ascii_uppercase()))
}

/// Normalize a highlight colour to `#RRGGBB`. Accepts hex or legacy palette ids
/// (red/yellow/green/cyan/magenta/gray) used before free-form colours.
pub(crate) fn normalize_highlight_color(color: &str) -> String {
    if let Some(hex) = normalize_hex_color(color) {
        return hex;
    }
    match color.trim().to_ascii_lowercase().as_str() {
        "yellow" => "#F5F543".to_string(),
        "green" => "#23D18B".to_string(),
        "cyan" => "#29B8DB".to_string(),
        "magenta" => "#D670D6".to_string(),
        "gray" | "grey" => "#666666".to_string(),
        // "red" and anything unrecognised → bright red (former ANSI idx 9).
        _ => "#F14C4C".to_string(),
    }
}

/// Legacy display-only group names that must never be persisted as user folders.
/// Older builds used `default` for ungrouped sessions and `system` for built-in
/// local shells; both now map to an empty (root) group.
pub(crate) fn is_reserved_session_group(name: &str) -> bool {
    name.eq_ignore_ascii_case("default") || name.eq_ignore_ascii_case("system")
}

/// Empty / reserved display names all mean the Quick Connect root.
pub(crate) fn normalize_session_group(group: &str) -> String {
    let g = group.trim();
    if g.is_empty() || is_reserved_session_group(g) {
        String::new()
    } else {
        g.to_string()
    }
}

/// Last path segment of a nested group (`"a/b/c"` → `"c"`).
pub(crate) fn group_path_segment(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Parent path (`"a/b/c"` → `"a/b"`, top-level → `""`).
pub(crate) fn group_parent_path(path: &str) -> String {
    path.rfind('/')
        .map(|idx| path[..idx].to_string())
        .unwrap_or_default()
}

/// Join a parent path and a single segment (`("", "x")` → `"x"`).
pub(crate) fn group_join(parent: &str, segment: &str) -> String {
    let parent = parent.trim();
    let segment = segment.trim();
    if parent.is_empty() {
        segment.to_string()
    } else {
        format!("{parent}/{segment}")
    }
}

/// User-entered segment: non-empty, no `/`, not a reserved name.
pub(crate) fn is_valid_group_segment(segment: &str) -> bool {
    let s = segment.trim();
    !s.is_empty() && !s.contains('/') && !is_reserved_session_group(s)
}
