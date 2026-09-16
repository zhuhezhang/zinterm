use std::fs::File;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// One monthly diagnostic log file (`error-YYYY-MM.log`).
pub struct MonthlyFile {
    pub(super) dir: PathBuf,
    /// Local calendar year/month currently open for append.
    pub(super) year: i32,
    pub(super) month: u32,
    pub(super) file: File,
}

/// Shared writer adapter used by the tracing formatter.
#[derive(Clone)]
pub struct MonthlyWriter(pub(super) Arc<Mutex<MonthlyFile>>);

pub struct Guard<'a>(pub(super) std::sync::MutexGuard<'a, MonthlyFile>);
