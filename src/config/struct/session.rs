use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::Secret;

/// Which transport a session uses.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum SessionKind {
    /// SSH shell + SFTP (the original and default behaviour).
    #[default]
    Ssh,
    /// Local serial port (COM3 / /dev/ttyUSB0) for switches, routers, MCUs (#14).
    Serial,
    /// Plain Telnet over TCP, for legacy network gear (#17).
    Telnet,
    /// Local shell process on this machine (PowerShell/CMD/$SHELL).
    Local,
}

impl SessionKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            SessionKind::Ssh => "ssh",
            SessionKind::Serial => "serial",
            SessionKind::Telnet => "telnet",
            SessionKind::Local => "local",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s {
            "serial" => SessionKind::Serial,
            "telnet" => SessionKind::Telnet,
            "local" => SessionKind::Local,
            _ => SessionKind::Ssh,
        }
    }
}

fn default_baud() -> u32 {
    9_600
}
fn default_data_bits() -> u8 {
    8
}
fn default_stop_bits() -> u8 {
    1
}
fn default_parity() -> String {
    "none".to_string()
}

fn default_flow() -> String {
    "none".to_string()
}

fn default_encoding() -> String {
    "UTF-8".to_string()
}

fn default_backspace_mode() -> String {
    "auto".to_string()
}

/// Older configs always had SFTP (SSH) / the command panel available; keep that
/// when the field is absent. New sessions still default off via [`Session::new_empty`].
fn default_feature_enabled_compat() -> bool {
    true
}

/// How a session authenticates.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AuthMethod {
    #[serde(alias = "keyboard-interactive", alias = "keyboard", alias = "interactive")]
    Password,
    Key,
}

impl AuthMethod {
    pub fn as_str(&self) -> &'static str {
        match self {
            AuthMethod::Password => "password",
            AuthMethod::Key => "key",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s {
            "key" => AuthMethod::Key,
            // Legacy configs may still say keyboard-interactive; treat as password.
            _ => AuthMethod::Password,
        }
    }
}

/// A single saved SSH target.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub user: String,
    pub auth: AuthMethod,
    #[serde(default)]
    pub password: Secret,
    #[serde(default)]
    pub private_key_path: String,
    #[serde(default)]
    pub private_key_inline: Secret,
    #[serde(default)]
    pub last_used: Option<String>,
    /// Optional folder/group name to organize sessions in the list (#41).
    /// Empty = ungrouped. Sessions are grouped by this in Quick Connect.
    #[serde(default)]
    pub group: String,

    // --- Transport ----------------------------------------------------------
    /// SSH (default), Serial, or Telnet. Absent in old config files → Ssh.
    #[serde(default)]
    pub kind: SessionKind,

    // --- Serial-only fields (ignored unless kind == Serial) -----------------
    /// Serial device path, e.g. "COM3" (Windows) or "/dev/ttyUSB0" (Linux).
    #[serde(default)]
    pub serial_port: String,
    #[serde(default = "default_baud")]
    pub baud_rate: u32,
    #[serde(default = "default_data_bits")]
    pub data_bits: u8,
    #[serde(default = "default_stop_bits")]
    pub stop_bits: u8,
    /// "none" | "odd" | "even" | "mark" | "space".
    #[serde(default = "default_parity")]
    pub parity: String,
    /// "none" | "xonxoff" | "rtscts" | "dsrdtr" (legacy: "software" | "hardware").
    #[serde(default = "default_flow")]
    pub flow_control: String,

    /// Character encoding used by the interactive terminal stream (#338).
    /// UTF-8 remains the default for existing and newly created sessions.
    #[serde(default = "default_encoding")]
    pub encoding: String,

    /// Byte sent for the Backspace key: `"auto"` | `"del"` | `"bs"`.
    /// Auto keeps DEL for SSH/Local and maps DEL→BS for Telnet/Serial so more
    /// gear accepts erase; Del/Bs force 0x7F / 0x08 respectively.
    #[serde(default = "default_backspace_mode")]
    pub backspace_mode: String,

    // --- Local-only fields (ignored unless kind == Local) -------------------
    /// Shell program or path. Empty = platform default ($SHELL / PowerShell).
    #[serde(default)]
    pub shell: String,
    /// Working directory. Empty = the user's home directory.
    #[serde(default)]
    pub working_directory: String,

    /// Skip the shell-integration setup (the cwd-follow PROMPT_COMMAND hook).
    /// That assumes a POSIX shell; on a Windows server whose shell is pwsh/cmd
    /// the injected hook breaks the shell. Turn this on for such servers (#140).
    #[serde(default)]
    pub disable_shell_integration: bool,

    /// Enable the SFTP side panel (SSH only). New sessions default off; missing
    /// field in older configs deserializes as on so existing SSH sessions keep SFTP.
    #[serde(default = "default_feature_enabled_compat")]
    pub enable_sftp: bool,

    /// Enable the bottom command panel (quick commands + input + history).
    /// New sessions default off; missing field in older configs stays on.
    /// Accepts the previous `enable_quick_commands` key for configs saved
    /// before the rename.
    #[serde(
        default = "default_feature_enabled_compat",
        alias = "enable_quick_commands"
    )]
    pub enable_command_panel: bool,
}

impl Session {
    pub fn new_empty() -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            name: String::new(),
            host: String::new(),
            port: 22,
            user: "root".into(),
            auth: AuthMethod::Password,
            password: Secret::default(),
            private_key_path: String::new(),
            private_key_inline: Secret::default(),
            last_used: None,
            group: String::new(),
            kind: SessionKind::Ssh,
            serial_port: String::new(),
            baud_rate: default_baud(),
            data_bits: default_data_bits(),
            stop_bits: default_stop_bits(),
            parity: default_parity(),
            flow_control: default_flow(),
            encoding: default_encoding(),
            backspace_mode: default_backspace_mode(),
            shell: String::new(),
            working_directory: String::new(),
            disable_shell_integration: false,
            enable_sftp: false,
            enable_command_panel: false,
        }
    }
}
