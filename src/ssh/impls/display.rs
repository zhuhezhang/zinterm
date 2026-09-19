/// Format a byte count as a human-readable string.
pub fn format_size(bytes: u64) -> String {
    if bytes < 1_024 {
        format!("{} B", bytes)
    } else if bytes < 1_024 * 1_024 {
        format!("{:.1} KB", bytes as f64 / 1_024.0)
    } else if bytes < 1_024 * 1_024 * 1_024 {
        format!("{:.1} MB", bytes as f64 / (1_024.0 * 1_024.0))
    } else {
        format!("{:.2} GB", bytes as f64 / (1_024.0 * 1_024.0 * 1_024.0))
    }
}

/// Format a Unix timestamp as `YYYY-MM-DD HH:MM`.
pub fn format_mtime(ts: u32) -> String {
    // SFTP mtime is a Unix timestamp (UTC seconds). Render it in the machine's
    // *local* timezone so the displayed time matches the user's wall clock
    // (e.g. UTC+8) instead of showing UTC — which read 8 h early (#168).
    use chrono::{Local, TimeZone};
    let dt = Local
        .timestamp_opt(ts as i64, 0)
        .single()
        .unwrap_or_else(Local::now);
    dt.format("%Y-%m-%d %H:%M").to_string()
}
