use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU32};
use std::sync::{Arc, Mutex};

use tokio::runtime::Runtime;

use crate::sftp::{SftpHandles, SftpLastCwd};
use crate::ssh::{CredentialResponder, HostKeyResponder, MfaResponder, SessionHandle};
use crate::terminal::{RenderGates, TermBuffers};
use crate::ui::AppWindow;

/// Per-tab connection state used for reconnect (R) and tab duplicate.
#[derive(Clone, Default)]
pub(crate) struct TabStatus {
    pub(crate) session_id: String,
    /// 0 connecting / 1 connected / 2 disconnected
    pub(crate) state: u8,
}

pub(crate) type TabStatuses = Arc<Mutex<HashMap<String, TabStatus>>>;

/// Shared dependencies for starting or reconnecting a session tab.
pub(crate) struct ConnectCtx {
    pub(crate) weak: slint::Weak<AppWindow>,
    pub(crate) runtime: Arc<Runtime>,
    pub(crate) handles: Rc<RefCell<HashMap<String, SessionHandle>>>,
    pub(crate) sftp_handles: SftpHandles,
    pub(crate) sftp_last_cwd: SftpLastCwd,
    pub(crate) bufs: TermBuffers,
    pub(crate) render_gates: RenderGates,
    pub(crate) tab_statuses: TabStatuses,
    pub(crate) last_term_size: Arc<Mutex<(u32, u32)>>,
    pub(crate) sftp_follow_cd: Arc<AtomicBool>,
    /// SSH keepalive interval in seconds. 0 = off. Read when a session starts.
    pub(crate) ssh_keepalive_secs: Arc<AtomicU32>,
}

pub(crate) struct PendingHostKey {
    pub(crate) host: String,
    pub(crate) port: u16,
    pub(crate) changed: bool,
    pub(crate) title: String,
    pub(crate) message: String,
    pub(crate) detail: String,
    pub(crate) confirm_label: String,
    pub(crate) responders: Vec<HostKeyResponder>,
}

pub(crate) struct PendingCred {
    pub(crate) session_id: String,
    pub(crate) host: String,
    pub(crate) user: String,
    pub(crate) need_user: bool,
    pub(crate) need_password: bool,
    pub(crate) responders: Vec<CredentialResponder>,
}

pub(crate) struct PendingMfa {
    pub(crate) session_id: String,
    pub(crate) host: String,
    pub(crate) prompt: String,
    pub(crate) echo: bool,
    pub(crate) responders: Vec<MfaResponder>,
}
