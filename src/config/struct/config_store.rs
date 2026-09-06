use std::path::PathBuf;

use super::ConfigFile;

pub struct ConfigStore {
    pub(crate) path: PathBuf,
    pub(crate) backup_dir: Option<PathBuf>,
    pub(crate) cache: ConfigFile,
}
