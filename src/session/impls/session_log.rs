use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use chrono::Local;
use directories::UserDirs;

use crate::terminal::{TermBuffers, TermBuffer};

/// Default folder under the user's Downloads directory for session output logs.
pub(crate) const SESSION_LOG_DIR_NAME: &str = "zinterm-session-log";

/// Shared per-app session output loggers. Writers are keyed by `tab_id` so an
/// R-reconnect keeps appending to the same file until the tab is closed.
///
/// Content is plain text exported from the terminal buffer (WYSIWYG), not raw
/// PTY bytes — matching zauterm's xterm-buffer approach.
#[derive(Clone)]
pub(crate) struct SessionLoggers {
    enabled: Arc<AtomicBool>,
    /// Absolute directory path. Empty falls back to [`default_session_log_dir`].
    dir: Arc<Mutex<String>>,
    writers: Arc<Mutex<HashMap<String, BufWriter<File>>>>,
    /// Last committed plain text written (or watermarked) per tab, used to
    /// compute append-only deltas.
    committed: Arc<Mutex<HashMap<String, String>>>,
}

impl SessionLoggers {
    pub(crate) fn new(enabled: bool, dir: String) -> Self {
        Self {
            enabled: Arc::new(AtomicBool::new(enabled)),
            dir: Arc::new(Mutex::new(dir)),
            writers: Arc::new(Mutex::new(HashMap::new())),
            committed: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub(crate) fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    pub(crate) fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Relaxed);
    }

    pub(crate) fn set_dir(&self, dir: String) {
        if let Ok(mut g) = self.dir.lock() {
            *g = dir;
        }
    }

    pub(crate) fn resolved_dir(&self) -> PathBuf {
        let Ok(g) = self.dir.lock() else {
            return default_session_log_dir();
        };
        if g.trim().is_empty() {
            default_session_log_dir()
        } else {
            PathBuf::from(g.as_str())
        }
    }

    /// Snapshot the tab's terminal buffer and append any newly committed plain
    /// text. No-op while logging is off, on the alternate screen, or when the
    /// committed region is unchanged.
    pub(crate) fn snapshot_tab(
        &self,
        tab_id: &str,
        tab_title: &str,
        bufs: &TermBuffers,
        include_cursor_line: bool,
    ) {
        if !self.is_enabled() {
            return;
        }
        let Some(handle) = bufs.lock().ok().and_then(|m| m.get(tab_id).cloned()) else {
            return;
        };
        let Ok(buf) = handle.lock() else {
            return;
        };
        self.append_from_buffer(tab_id, tab_title, &buf, include_cursor_line);
    }

    fn append_from_buffer(
        &self,
        tab_id: &str,
        tab_title: &str,
        buf: &TermBuffer,
        include_cursor_line: bool,
    ) {
        let Some(current) = buf.export_committed_plain(include_cursor_line) else {
            // Alternate screen: leave the watermark alone so exiting vim
            // resumes cleanly from the pre-TUI committed text.
            return;
        };
        let prev = self
            .committed
            .lock()
            .ok()
            .and_then(|m| m.get(tab_id).cloned())
            .unwrap_or_default();
        let (delta, next) = diff_committed_log_delta(&prev, &current);
        if let Ok(mut map) = self.committed.lock() {
            map.insert(tab_id.to_string(), next);
        }
        if delta.is_empty() {
            return;
        }
        self.write_bytes(tab_id, tab_title, delta.as_bytes());
    }

    fn write_bytes(&self, tab_id: &str, tab_title: &str, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        let Ok(mut map) = self.writers.lock() else {
            return;
        };
        if !map.contains_key(tab_id) {
            let dir = self.resolved_dir();
            let Some(file) = open_new_log_file(&dir, tab_title) else {
                return;
            };
            map.insert(tab_id.to_string(), BufWriter::new(file));
        }
        if let Some(writer) = map.get_mut(tab_id) {
            let _ = writer.write_all(bytes);
            let _ = writer.flush();
        }
    }

    /// Drop the writer and watermark for a closed tab so a future tab with a
    /// new id gets a fresh file (and so file handles are released promptly).
    pub(crate) fn close_tab(&self, tab_id: &str) {
        if let Ok(mut map) = self.writers.lock() {
            if let Some(mut writer) = map.remove(tab_id) {
                let _ = writer.flush();
            }
        }
        if let Ok(mut map) = self.committed.lock() {
            map.remove(tab_id);
        }
    }
}

/// Relative to the last committed text, compute the append-only delta.
/// Handles prefix growth, scrollback trim (line overlap), and clear/redraw
/// (blank-line separator). Ported from zauterm's `diffCommittedLogDelta`.
pub(crate) fn diff_committed_log_delta(prev: &str, next: &str) -> (String, String) {
    if next == prev {
        return (String::new(), prev.to_string());
    }
    if prev.is_empty() {
        return (next.to_string(), next.to_string());
    }
    if let Some(suffix) = next.strip_prefix(prev) {
        return (suffix.to_string(), next.to_string());
    }

    let prev_lines: Vec<&str> = if prev.is_empty() {
        Vec::new()
    } else {
        prev.split('\n').collect()
    };
    let next_lines: Vec<&str> = if next.is_empty() {
        Vec::new()
    } else {
        next.split('\n').collect()
    };
    let max_overlap = prev_lines.len().min(next_lines.len());
    let mut overlap = 0usize;
    for k in (1..=max_overlap).rev() {
        let mut ok = true;
        for i in 0..k {
            if prev_lines[prev_lines.len() - k + i] != next_lines[i] {
                ok = false;
                break;
            }
        }
        if ok {
            overlap = k;
            break;
        }
    }

    let tail = next_lines[overlap..].join("\n");
    if tail.is_empty() {
        return (String::new(), next.to_string());
    }
    if overlap == 0 {
        // Clear / large redraw: keep disk history, separate with a blank line.
        let delta = if prev.is_empty() {
            tail
        } else {
            format!("\n\n{tail}")
        };
        return (delta, next.to_string());
    }
    (format!("\n{tail}"), next.to_string())
}

/// `{Downloads}/zinterm-session-log`, or the system temp dir when Downloads is
/// unavailable.
pub(crate) fn default_session_log_dir() -> PathBuf {
    UserDirs::new()
        .and_then(|u| u.download_dir().map(|p| p.join(SESSION_LOG_DIR_NAME)))
        .unwrap_or_else(|| std::env::temp_dir().join(SESSION_LOG_DIR_NAME))
}

fn open_new_log_file(dir: &Path, tab_title: &str) -> Option<File> {
    if let Err(e) = fs::create_dir_all(dir) {
        tracing::warn!("session log: create_dir_all {}: {e}", dir.display());
        return None;
    }
    let path = unique_log_path(dir, tab_title);
    match OpenOptions::new().create_new(true).append(true).open(&path) {
        Ok(f) => Some(f),
        Err(e) => {
            tracing::warn!("session log: open {}: {e}", path.display());
            None
        }
    }
}

fn unique_log_path(dir: &Path, tab_title: &str) -> PathBuf {
    let stamp = Local::now().format("%Y%m%d-%H%M%S");
    let safe = filename_safe_segment(tab_title);
    let base = format!("{stamp}-{safe}");
    let candidate = dir.join(format!("{base}.log"));
    if !candidate.exists() {
        return candidate;
    }
    for n in 2..1000 {
        let candidate = dir.join(format!("{base}-{n}.log"));
        if !candidate.exists() {
            return candidate;
        }
    }
    dir.join(format!("{base}-{}.log", uuid::Uuid::new_v4()))
}

fn filename_safe_segment(name: &str) -> String {
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

#[cfg(test)]
mod tests {
    use super::diff_committed_log_delta;

    #[test]
    fn diff_unchanged_is_empty() {
        assert_eq!(
            diff_committed_log_delta("a\nb", "a\nb"),
            (String::new(), "a\nb".into())
        );
    }

    #[test]
    fn diff_prefix_growth() {
        assert_eq!(
            diff_committed_log_delta("hello", "hello\nworld"),
            ("\nworld".into(), "hello\nworld".into())
        );
    }

    #[test]
    fn diff_empty_prev_writes_full() {
        assert_eq!(
            diff_committed_log_delta("", "line1\nline2"),
            ("line1\nline2".into(), "line1\nline2".into())
        );
    }

    #[test]
    fn diff_scrollback_trim_via_overlap() {
        assert_eq!(
            diff_committed_log_delta("A\nB\nC\nD\nE", "B\nC\nD\nE\nF"),
            ("\nF".into(), "B\nC\nD\nE\nF".into())
        );
    }

    #[test]
    fn diff_clear_redraw_separates_with_blank() {
        assert_eq!(
            diff_committed_log_delta("old\ncontent", "prompt>"),
            ("\n\nprompt>".into(), "prompt>".into())
        );
    }

    #[test]
    fn diff_empty_next_after_clear() {
        assert_eq!(
            diff_committed_log_delta("old", ""),
            (String::new(), String::new())
        );
    }
}
