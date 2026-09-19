use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use russh::client;
use russh_sftp::client::SftpSession;
use tokio::sync::mpsc::UnboundedSender;

use super::super::transfer::SftpCommand;
use super::ssh_handler::SftpClientHandler;

use crate::ssh::SessionEvent;

pub(super) struct WorkerCtx {
    pub(super) sftp: Arc<SftpSession>,
    pub(super) handle: Arc<client::Handle<SftpClientHandler>>,
    pub(super) events: UnboundedSender<SessionEvent>,
    pub(super) self_tx: UnboundedSender<SftpCommand>,
    pub(super) cancels: Arc<Mutex<HashMap<String, Arc<AtomicBool>>>>,
    pub(super) tree_dirs: HashMap<String, Vec<(String, String)>>,
    pub(super) tree_expanded: HashSet<String>,
    pub(super) external_edit_dir: PathBuf,
    pub(super) external_edit_prefix: String,
}
