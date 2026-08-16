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
    /// Local shell process on this machine (PowerShell/CMD/WSL/$SHELL).
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
    115_200
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

/// How a session authenticates.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AuthMethod {
    Password,
    #[serde(rename = "keyboard-interactive")]
    KeyboardInteractive,
    Key,
}

impl AuthMethod {
    pub fn as_str(&self) -> &'static str {
        match self {
            AuthMethod::Password => "password",
            AuthMethod::KeyboardInteractive => "keyboard-interactive",
            AuthMethod::Key => "key",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s {
            "keyboard-interactive" | "keyboard" | "interactive" => AuthMethod::KeyboardInteractive,
            "key" => AuthMethod::Key,
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

    /// WSL distribution and startup directory for generated local sessions.
    /// The directory defaults to the selected distribution user's home (`~`).
    #[serde(default)]
    pub local_distribution: String,
    #[serde(default)]
    pub local_working_dir: String,

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
    /// "none" | "odd" | "even".
    #[serde(default = "default_parity")]
    pub parity: String,
    /// "none" | "hardware" | "software".
    #[serde(default = "default_flow")]
    pub flow_control: String,

    /// Character encoding used by the interactive terminal stream (#338).
    /// UTF-8 remains the default for existing and newly created sessions.
    #[serde(default = "default_encoding")]
    pub encoding: String,

    /// Skip the shell-integration setup (the cwd-follow PROMPT_COMMAND hook + the
    /// remote resource monitor). Those assume a POSIX shell; on a Windows server
    /// whose shell is pwsh/cmd the injected hook breaks the shell. Turn this on
    /// for such servers (#140).
    #[serde(default)]
    pub disable_shell_integration: bool,
    /// Free-form note for this session — somewhere to stash extra info
    /// (credentials hints, owner, etc.). Shown only in the edit dialog.
    /// (B站 suggestion)
    #[serde(default)]
    pub note: String,
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
            local_distribution: String::new(),
            local_working_dir: String::new(),
            serial_port: String::new(),
            baud_rate: default_baud(),
            data_bits: default_data_bits(),
            stop_bits: default_stop_bits(),
            parity: default_parity(),
            flow_control: default_flow(),
            encoding: default_encoding(),
            disable_shell_integration: false,
            note: String::new(),
        }
    }
}
