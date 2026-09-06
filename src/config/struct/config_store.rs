use std::path::PathBuf;

use super::ConfigFile;

pub struct ConfigStore {
    pub(crate) path: PathBuf,
    pub(crate) cache: ConfigFile,
}
