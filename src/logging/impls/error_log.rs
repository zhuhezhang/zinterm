//! Monthly diagnostic log files (#86).
//!
//! Writes go to `<log_dir>/error-YYYY-MM.log` — a `log/` subdir under the
//! per-user data dir (see [`crate::config::log_dir`]), kept separate from
//! sessions / vault files. Each calendar month uses one file; at most twelve
//! months are retained. Expired files are removed once on application startup.
//! If the process spans a month boundary, the next write opens the new month's
//! file so lines always land in the correct month.

use super::writer::{Guard, MonthlyFile, MonthlyWriter};
use chrono::{Datelike, Local};
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Filename prefix / suffix for monthly diagnostic logs.
const PREFIX: &str = "error-";
const SUFFIX: &str = ".log";

/// Keep the current month plus the previous eleven (= one year, ≤ 12 files).
const RETAIN_MONTHS: i32 = 12;

fn file_name(year: i32, month: u32) -> String {
    format!("{PREFIX}{year:04}-{month:02}{SUFFIX}")
}

fn parse_name(name: &str) -> Option<(i32, u32)> {
    let rest = name.strip_prefix(PREFIX)?.strip_suffix(SUFFIX)?;
    let (y, m) = rest.split_once('-')?;
    let year: i32 = y.parse().ok()?;
    let month: u32 = m.parse().ok()?;
    if (1..=12).contains(&month) {
        Some((year, month))
    } else {
        None
    }
}

fn month_index(year: i32, month: u32) -> i32 {
    year * 12 + month as i32 - 1
}

/// Delete monthly logs older than [`RETAIN_MONTHS`], plus any legacy single
/// `error.log` left from the previous size-capped scheme. Call once at startup.
pub fn cleanup_expired() {
    let now = Local::now();
    cleanup_dir(&crate::config::log_dir(), now.year(), now.month());
}

fn cleanup_dir(dir: &Path, year: i32, month: u32) {
    let cutoff = month_index(year, month) - (RETAIN_MONTHS - 1);
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if name == "error.log" {
            let _ = fs::remove_file(&path);
            continue;
        }
        if let Some((y, m)) = parse_name(name) {
            if month_index(y, m) < cutoff {
                let _ = fs::remove_file(&path);
            }
        }
    }
}

impl MonthlyFile {
    pub fn open() -> io::Result<Self> {
        let dir = crate::config::log_dir();
        let now = Local::now();
        Self::open_month(dir, now.year(), now.month())
    }

    fn open_month(dir: PathBuf, year: i32, month: u32) -> io::Result<Self> {
        let path = dir.join(file_name(year, month));
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        Ok(Self {
            dir,
            year,
            month,
            file,
        })
    }

    fn ensure_current_month(&mut self) -> io::Result<()> {
        let now = Local::now();
        let year = now.year();
        let month = now.month();
        if year == self.year && month == self.month {
            return Ok(());
        }
        *self = Self::open_month(self.dir.clone(), year, month)?;
        Ok(())
    }
}

impl Write for MonthlyFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.ensure_current_month()?;
        self.file.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

impl MonthlyWriter {
    pub fn new(file: MonthlyFile) -> Self {
        Self(Arc::new(Mutex::new(file)))
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for MonthlyWriter {
    type Writer = Guard<'a>;
    fn make_writer(&'a self) -> Self::Writer {
        Guard(self.0.lock().unwrap_or_else(|e| e.into_inner()))
    }
}

impl Write for Guard<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_roundtrip() {
        assert_eq!(parse_name("error-2026-09.log"), Some((2026, 9)));
        assert_eq!(parse_name("error-2026-9.log"), None);
        assert_eq!(parse_name("error.log"), None);
        assert_eq!(parse_name("other-2026-09.log"), None);
    }

    #[test]
    fn month_index_ordering() {
        assert!(month_index(2025, 12) < month_index(2026, 1));
        assert_eq!(month_index(2026, 9) - month_index(2025, 10), 11);
    }

    #[test]
    fn file_name_zero_pads() {
        assert_eq!(file_name(2026, 9), "error-2026-09.log");
        assert_eq!(file_name(2026, 12), "error-2026-12.log");
    }

    #[test]
    fn cleanup_keeps_twelve_months() {
        let dir = tempfile_dir();
        // Plant 14 months ending at "now" (fixed via files named relative to
        // Local::now so the test stays calendar-stable within the same month).
        let now = Local::now();
        let current = month_index(now.year(), now.month());
        for age in 0..14 {
            let idx = current - age;
            let year = idx / 12;
            let month = (idx % 12) as u32 + 1;
            let path = dir.join(file_name(year, month));
            fs::write(&path, b"x").unwrap();
        }
        fs::write(dir.join("error.log"), b"legacy").unwrap();

        cleanup_dir(&dir, now.year(), now.month());

        let mut left: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .filter_map(|e| e.file_name().into_string().ok())
            .collect();
        left.sort();
        assert!(!left.iter().any(|n| n == "error.log"));
        assert_eq!(left.len(), 12);
        // Oldest kept should be current - 11.
        let oldest = current - 11;
        let oy = oldest / 12;
        let om = (oldest % 12) as u32 + 1;
        assert!(left.contains(&file_name(oy, om)));
        // Age 12 and 13 must be gone.
        for age in 12..14 {
            let idx = current - age;
            let year = idx / 12;
            let month = (idx % 12) as u32 + 1;
            assert!(!left.contains(&file_name(year, month)));
        }
        let _ = fs::remove_dir_all(&dir);
    }

    fn tempfile_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "zinterm-log-test-{}-{}",
            std::process::id(),
            Local::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }
}
