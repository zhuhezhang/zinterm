use rand::Rng;
use serde::ser::SerializeMap;
use serde::{Deserialize, Serialize, Serializer};

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

fn default_port() -> u16 {
    22
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
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum AuthMethod {
    #[serde(alias = "keyboard-interactive", alias = "keyboard", alias = "interactive")]
    #[default]
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

/// A single saved connection target (SSH / Serial / Telnet / Local).
///
/// Serialized with a kind-aware custom [`Serialize`] so SessionDialog fields for
/// the active kind are always written (including empty `group` and SSH `auth`),
/// while fields that belong to other kinds are omitted. Field order starts with
/// `id`, then `kind`.
#[derive(Debug, Clone, Deserialize)]
pub struct Session {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub host: String,
    /// TCP port for SSH/Telnet. `0` means unused (Serial/Local).
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default)]
    pub user: String,
    #[serde(default)]
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
    /// Empty = ungrouped. Always persisted (including `""`).
    #[serde(default)]
    pub group: String,

    /// SSH (default), Serial, Telnet, or Local. Absent in old config files → Ssh.
    #[serde(default)]
    pub kind: SessionKind,

    /// Last time this session was created/updated via save (Unix ms, 13 digits).
    #[serde(default)]
    pub saved_at: u64,

    // --- Serial-only fields -------------------------------------------------
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
    #[serde(default = "default_encoding")]
    pub encoding: String,

    /// Byte sent for the Backspace key: `"auto"` | `"del"` | `"bs"`.
    #[serde(default = "default_backspace_mode")]
    pub backspace_mode: String,

    // --- Local-only fields --------------------------------------------------
    #[serde(default)]
    pub shell: String,
    #[serde(default)]
    pub working_directory: String,

    /// Enable the SFTP side panel (SSH only).
    #[serde(default = "default_feature_enabled_compat")]
    pub enable_sftp: bool,

    /// Enable the bottom command panel.
    #[serde(
        default = "default_feature_enabled_compat",
        alias = "enable_quick_commands"
    )]
    pub enable_command_panel: bool,
}

impl Serialize for Session {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(None)?;
        // Leading order: id, saved_at, then kind.
        map.serialize_entry("id", &self.id)?;
        map.serialize_entry("saved_at", &self.saved_at)?;
        map.serialize_entry("kind", &self.kind)?;
        map.serialize_entry("name", &self.name)?;
        // Always persist group, including empty (ungrouped).
        map.serialize_entry("group", &self.group)?;

        match self.kind {
            SessionKind::Ssh => {
                map.serialize_entry("host", &self.host)?;
                map.serialize_entry("port", &self.port)?;
                map.serialize_entry("user", &self.user)?;
                map.serialize_entry("auth", &self.auth)?;
                if !self.password.is_empty() {
                    map.serialize_entry("password", &self.password)?;
                }
                if !self.private_key_path.is_empty() {
                    map.serialize_entry("private_key_path", &self.private_key_path)?;
                }
                if !self.private_key_inline.is_empty() {
                    map.serialize_entry("private_key_inline", &self.private_key_inline)?;
                }
            }
            SessionKind::Serial => {
                map.serialize_entry("serial_port", &self.serial_port)?;
                map.serialize_entry("baud_rate", &self.baud_rate)?;
                map.serialize_entry("data_bits", &self.data_bits)?;
                map.serialize_entry("stop_bits", &self.stop_bits)?;
                map.serialize_entry("parity", &self.parity)?;
                map.serialize_entry("flow_control", &self.flow_control)?;
            }
            SessionKind::Telnet => {
                map.serialize_entry("host", &self.host)?;
                map.serialize_entry("port", &self.port)?;
            }
            SessionKind::Local => {
                map.serialize_entry("shell", &self.shell)?;
                map.serialize_entry("working_directory", &self.working_directory)?;
            }
        }

        map.serialize_entry("backspace_mode", &self.backspace_mode)?;
        map.serialize_entry("encoding", &self.encoding)?;
        // SSH: enable_sftp is second-to-last (before enable_command_panel).
        if self.kind == SessionKind::Ssh {
            map.serialize_entry("enable_sftp", &self.enable_sftp)?;
        }
        map.serialize_entry("enable_command_panel", &self.enable_command_panel)?;
        if let Some(ref last_used) = self.last_used {
            map.serialize_entry("last_used", last_used)?;
        }
        map.end()
    }
}

impl Session {
    /// Unix time in milliseconds (typically 13 digits).
    pub fn now_saved_at() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }

    /// `saved-<13-digit-ms>-<4 alphanumerics>` — assigned on first persist.
    pub fn new_saved_id() -> String {
        let ms = Self::now_saved_at();
        const CHARSET: &[u8] =
            b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
        let mut rng = rand::thread_rng();
        let suffix: String = (0..4)
            .map(|_| CHARSET[rng.gen_range(0..CHARSET.len())] as char)
            .collect();
        format!("saved-{ms}-{suffix}")
    }

    /// Ephemeral id for connect-without-save (not written to disk).
    pub fn new_temp_id() -> String {
        let ms = Self::now_saved_at();
        const CHARSET: &[u8] =
            b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
        let mut rng = rand::thread_rng();
        let suffix: String = (0..4)
            .map(|_| CHARSET[rng.gen_range(0..CHARSET.len())] as char)
            .collect();
        format!("temp-{ms}-{suffix}")
    }

    pub fn new_empty() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            host: String::new(),
            port: default_port(),
            user: "root".into(),
            auth: AuthMethod::Password,
            password: Secret::default(),
            private_key_path: String::new(),
            private_key_inline: Secret::default(),
            last_used: None,
            group: String::new(),
            kind: SessionKind::Ssh,
            saved_at: 0,
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
            enable_sftp: false,
            enable_command_panel: false,
        }
    }

    /// Drop fields that do not apply to [`Self::kind`] so UI save / import /
    /// export never persist SSH auth on a serial session (or serial baud on SSH).
    /// Returns `true` when any field was cleared or reset.
    pub fn sanitize_for_kind(&mut self) -> bool {
        let before = serde_json::to_string(self).ok();
        match self.kind {
            SessionKind::Ssh => {
                self.clear_serial_fields();
                self.clear_local_fields();
            }
            SessionKind::Serial => {
                self.clear_network_fields();
                self.clear_auth_fields();
                self.clear_local_fields();
                self.enable_sftp = false;
            }
            SessionKind::Telnet => {
                self.clear_serial_fields();
                self.clear_local_fields();
                self.clear_auth_fields();
                self.user.clear();
                self.enable_sftp = false;
                if self.port == 0 {
                    self.port = 23;
                }
            }
            SessionKind::Local => {
                self.clear_network_fields();
                self.clear_auth_fields();
                self.clear_serial_fields();
                self.enable_sftp = false;
            }
        }
        before
            .map(|b| serde_json::to_string(self).ok().map(|a| a != b).unwrap_or(true))
            .unwrap_or(true)
    }

    fn clear_serial_fields(&mut self) {
        self.serial_port.clear();
        self.baud_rate = default_baud();
        self.data_bits = default_data_bits();
        self.stop_bits = default_stop_bits();
        self.parity = default_parity();
        self.flow_control = default_flow();
    }

    fn clear_local_fields(&mut self) {
        self.shell.clear();
        self.working_directory.clear();
    }

    fn clear_network_fields(&mut self) {
        self.host.clear();
        self.port = 0;
    }

    fn clear_auth_fields(&mut self) {
        self.user.clear();
        self.auth = AuthMethod::Password;
        self.password = Secret::default();
        self.private_key_path.clear();
        self.private_key_inline = Secret::default();
    }
}

#[cfg(test)]
mod sanitize_tests {
    use super::*;

    #[test]
    fn ssh_drops_serial_and_local_fields() {
        let mut s = Session::new_empty();
        s.kind = SessionKind::Ssh;
        s.host = "1.2.3.4".into();
        s.user = "root".into();
        s.auth = AuthMethod::Key;
        s.serial_port = "COM3".into();
        s.baud_rate = 115_200;
        s.parity = "even".into();
        s.shell = "/bin/zsh".into();
        s.working_directory = "/tmp".into();
        s.sanitize_for_kind();
        assert!(s.serial_port.is_empty());
        assert_eq!(s.baud_rate, 9_600);
        assert_eq!(s.parity, "none");
        assert!(s.shell.is_empty());
        assert!(s.working_directory.is_empty());
        assert_eq!(s.host, "1.2.3.4");
        assert_eq!(s.auth, AuthMethod::Key);
    }

    #[test]
    fn serial_drops_ssh_auth_and_network_fields() {
        let mut s = Session::new_empty();
        s.kind = SessionKind::Serial;
        s.host = "1.2.3.4".into();
        s.port = 22;
        s.user = "root".into();
        s.auth = AuthMethod::Key;
        s.password = Secret::new("secret");
        s.private_key_path = "/key".into();
        s.private_key_inline = Secret::new("PEM");
        s.enable_sftp = true;
        s.serial_port = "COM3".into();
        s.baud_rate = 115_200;
        s.shell = "/bin/zsh".into();
        s.sanitize_for_kind();
        assert!(s.host.is_empty());
        assert_eq!(s.port, 0);
        assert!(s.user.is_empty());
        assert_eq!(s.auth, AuthMethod::Password);
        assert!(s.password.is_empty());
        assert!(s.private_key_path.is_empty());
        assert!(s.private_key_inline.is_empty());
        assert!(!s.enable_sftp);
        assert!(s.shell.is_empty());
        assert_eq!(s.serial_port, "COM3");
        assert_eq!(s.baud_rate, 115_200);
    }

    #[test]
    fn json_keeps_dialog_fields_omits_other_kinds() {
        let mut serial = Session::new_empty();
        serial.kind = SessionKind::Serial;
        serial.name = "console".into();
        serial.serial_port = "COM3".into();
        serial.baud_rate = 9_600;
        serial.saved_at = 1_700_000_000_000;
        serial.sanitize_for_kind();
        let raw = serde_json::to_string(&serial).unwrap();
        assert!(!raw.contains("\"auth\""));
        assert!(!raw.contains("\"user\""));
        assert!(!raw.contains("\"host\""));
        assert!(!raw.contains("\"password\""));
        assert!(!raw.contains("\"private_key"));
        assert!(!raw.contains("\"shell\""));
        assert!(raw.contains("\"group\":\"\""));
        assert!(raw.contains("\"baud_rate\":9600"));
        assert!(raw.contains("\"parity\":\"none\""));
        assert!(raw.contains("\"serial_port\":\"COM3\""));
        assert!(raw.find("\"id\"").unwrap() < raw.find("\"saved_at\"").unwrap());
        assert!(raw.find("\"saved_at\"").unwrap() < raw.find("\"kind\"").unwrap());
        assert!(raw.find("\"backspace_mode\"").unwrap() < raw.find("\"encoding\"").unwrap());

        let mut ssh = Session::new_empty();
        ssh.kind = SessionKind::Ssh;
        ssh.name = "box".into();
        ssh.host = "10.0.0.1".into();
        ssh.user = "root".into();
        ssh.auth = AuthMethod::Password;
        ssh.serial_port = "COM3".into();
        ssh.baud_rate = 115_200;
        ssh.saved_at = 1_700_000_000_000;
        ssh.sanitize_for_kind();
        let raw = serde_json::to_string(&ssh).unwrap();
        assert!(!raw.contains("\"serial_port\""));
        assert!(!raw.contains("\"baud_rate\""));
        assert!(!raw.contains("\"parity\""));
        assert!(!raw.contains("\"shell\""));
        assert!(raw.contains("\"host\":\"10.0.0.1\""));
        assert!(raw.contains("\"auth\":\"password\""));
        assert!(raw.contains("\"group\":\"\""));
        assert!(raw.find("\"id\"").unwrap() < raw.find("\"saved_at\"").unwrap());
        assert!(raw.find("\"saved_at\"").unwrap() < raw.find("\"kind\"").unwrap());
        assert!(raw.find("\"backspace_mode\"").unwrap() < raw.find("\"encoding\"").unwrap());
        assert!(raw.find("\"encoding\"").unwrap() < raw.find("\"enable_sftp\"").unwrap());
        assert!(
            raw.find("\"enable_sftp\"").unwrap() < raw.find("\"enable_command_panel\"").unwrap()
        );
    }

    #[test]
    fn saved_id_format() {
        let id = Session::new_saved_id();
        let parts: Vec<_> = id.split('-').collect();
        assert_eq!(parts[0], "saved");
        assert_eq!(parts[1].len(), 13);
        assert!(parts[1].chars().all(|c| c.is_ascii_digit()));
        assert_eq!(parts[2].len(), 4);
        assert!(parts[2].chars().all(|c| c.is_ascii_alphanumeric()));
    }
}
