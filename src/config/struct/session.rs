use anyhow::{bail, Context, Result};
use rand::Rng;
use serde::ser::SerializeMap;
use serde::{Deserialize, Serialize, Serializer};
use serde_json::Value;

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
/// when the field is absent. New sessions still default those flags off in the UI.
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
    /// Login password for [`AuthMethod::Password`] only.
    #[serde(default)]
    pub password: Secret,
    /// Passphrase for an encrypted private key ([`AuthMethod::Key`] only).
    #[serde(default)]
    pub key_passphrase: Secret,
    /// Private key path **or** pasted PEM/OpenSSH/PPK body ([`AuthMethod::Key`]).
    /// Distinguished at use time by [`looks_like_private_key_content`].
    #[serde(default)]
    pub private_key: Secret,
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

    /// Probe bash/zsh and inject prompt hooks (SSH only). Opt-in; absent in
    /// older configs means off so network gear is not probed by default.
    #[serde(default)]
    pub enable_prompt_setup: bool,

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
                // password / key_passphrase / private_key live in the OS-keyring
                // vault (`zinterm-credentials-vault.json`), never in sessions.json.
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
        // SSH-only feature flags before the shared command-panel flag.
        if self.kind == SessionKind::Ssh {
            map.serialize_entry("enable_sftp", &self.enable_sftp)?;
            map.serialize_entry("enable_prompt_setup", &self.enable_prompt_setup)?;
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

    /// Parse one connection from a portable export file.
    ///
    /// Required: `kind` for every connection; non-empty `host` for SSH/Telnet;
    /// non-empty `serial_port` for Serial. Local has no host/port requirement.
    /// Other missing or malformed fields use the same defaults as the
    /// new-session dialog (empty user, port 22/23, baud 9600, SFTP/command
    /// panel off, …).
    pub fn from_import_value(value: &Value) -> Result<Self> {
        let obj = value
            .as_object()
            .context("session must be a JSON object")?;

        let kind = match obj.get("kind") {
            None => bail!("kind is required"),
            Some(v) => parse_import_kind(v)?,
        };

        let host = json_string(obj.get("host"));
        let serial_port = json_string(obj.get("serial_port"));
        match kind {
            SessionKind::Ssh | SessionKind::Telnet => {
                if host.trim().is_empty() {
                    bail!("host is required for {} connections", kind.as_str());
                }
            }
            SessionKind::Serial => {
                if serial_port.trim().is_empty() {
                    bail!("serial_port is required for serial connections");
                }
            }
            SessionKind::Local => {}
        }

        let default_port = if kind == SessionKind::Telnet { 23 } else { 22 };
        let auth = parse_import_auth(obj.get("auth"));
        let user = json_string(obj.get("user"));
        let shell = json_string(obj.get("shell"));
        let baud_rate = json_u32_positive(obj.get("baud_rate"), default_baud());
        let port = json_u16_positive(obj.get("port"), default_port);

        let name_raw = json_string(obj.get("name"));
        let name = if name_raw.trim().is_empty() {
            import_auto_name(kind, &host, &user, &serial_port, baud_rate, &shell)
        } else {
            name_raw
        };

        let (password, key_passphrase, private_key) = match auth {
            AuthMethod::Password => (
                Secret::new(json_string(obj.get("password"))),
                Secret::default(),
                Secret::default(),
            ),
            AuthMethod::Key => (
                Secret::default(),
                Secret::new(json_string(obj.get("key_passphrase"))),
                Secret::new(finalize_imported_private_key(&json_string(
                    obj.get("private_key"),
                ))),
            ),
        };

        let mut session = Self {
            id: json_string(obj.get("id")),
            name,
            host,
            port,
            user,
            auth,
            password,
            key_passphrase,
            private_key,
            last_used: json_optional_string(obj.get("last_used")),
            group: json_string(obj.get("group")),
            kind,
            saved_at: json_u64(obj.get("saved_at"), 0),
            serial_port,
            baud_rate,
            data_bits: json_u8_positive(obj.get("data_bits"), default_data_bits()),
            stop_bits: json_u8_positive(obj.get("stop_bits"), default_stop_bits()),
            parity: normalize_import_parity(&json_string(obj.get("parity"))),
            flow_control: normalize_import_flow(&json_string(obj.get("flow_control"))),
            encoding: {
                let enc = json_string(obj.get("encoding"));
                if enc.trim().is_empty() {
                    default_encoding()
                } else {
                    enc
                }
            },
            backspace_mode: normalize_import_backspace(&json_string(obj.get("backspace_mode")))
                .to_string(),
            shell,
            working_directory: json_string(obj.get("working_directory")),
            // Match new-session dialog defaults (off), not legacy config compat.
            enable_sftp: json_bool(obj.get("enable_sftp"), false),
            enable_prompt_setup: json_bool(obj.get("enable_prompt_setup"), false),
            enable_command_panel: json_bool(
                obj.get("enable_command_panel")
                    .or_else(|| obj.get("enable_quick_commands")),
                false,
            ),
        };
        session.sanitize_for_kind();
        Ok(session)
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
                self.sanitize_for_auth();
            }
            SessionKind::Serial => {
                self.clear_network_fields();
                self.clear_auth_fields();
                self.clear_local_fields();
                self.enable_sftp = false;
                self.enable_prompt_setup = false;
            }
            SessionKind::Telnet => {
                self.clear_serial_fields();
                self.clear_local_fields();
                self.clear_auth_fields();
                self.user.clear();
                self.enable_sftp = false;
                self.enable_prompt_setup = false;
                if self.port == 0 {
                    self.port = 23;
                }
            }
            SessionKind::Local => {
                self.clear_network_fields();
                self.clear_auth_fields();
                self.clear_serial_fields();
                self.enable_sftp = false;
                self.enable_prompt_setup = false;
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
        self.key_passphrase = Secret::default();
        self.private_key = Secret::default();
    }

    /// Drop credentials that do not apply to [`Self::auth`]:
    /// password auth ignores `key_passphrase` / `private_key`;
    /// key auth ignores `password`.
    fn sanitize_for_auth(&mut self) {
        match self.auth {
            AuthMethod::Password => {
                self.key_passphrase = Secret::default();
                self.private_key = Secret::default();
            }
            AuthMethod::Key => {
                self.password = Secret::default();
            }
        }
    }
}

/// True when `raw` looks like pasted key material (PEM / OpenSSH / PuTTY)
/// rather than a filesystem path.
pub fn looks_like_private_key_content(raw: &str) -> bool {
    let t = raw.trim();
    if t.is_empty() {
        return false;
    }
    // Avoid depending on the ssh module here (circular with Session consumers).
    if t.lines()
        .next()
        .is_some_and(|l| l.starts_with("PuTTY-User-Key-File"))
    {
        return true;
    }
    let upper = t.to_ascii_uppercase();
    if upper.contains("BEGIN") && upper.contains("PRIVATE KEY") {
        return true;
    }
    // Paths are almost never several non-empty lines; treat multi-line blobs as keys.
    t.lines().filter(|l| !l.trim().is_empty()).count() >= 3
}

/// Normalize a hand-edited `private_key` import value: `\n` → newlines for
/// key content, forward slashes for paths.
fn finalize_imported_private_key(raw: &str) -> String {
    let t = raw.trim();
    if t.is_empty() {
        return String::new();
    }
    if looks_like_private_key_content(t) {
        // JSON `\n` already becomes newlines during parse; also accept a
        // literal backslash-n sequence for editors that leave `\\n` in the value.
        t.replace("\\n", "\n")
    } else {
        t.replace('\\', "/")
    }
}

fn parse_import_kind(v: &Value) -> Result<SessionKind> {
    let Some(s) = v.as_str() else {
        bail!("kind must be a string");
    };
    match s {
        "ssh" => Ok(SessionKind::Ssh),
        "serial" => Ok(SessionKind::Serial),
        "telnet" => Ok(SessionKind::Telnet),
        "local" => Ok(SessionKind::Local),
        other => bail!("unsupported kind {other:?} (expected ssh|serial|telnet|local)"),
    }
}

fn parse_import_auth(v: Option<&Value>) -> AuthMethod {
    match v.and_then(Value::as_str) {
        Some("key") => AuthMethod::Key,
        // Missing / unknown / legacy keyboard-interactive → password (UI default).
        _ => AuthMethod::Password,
    }
}

fn import_auto_name(
    kind: SessionKind,
    host: &str,
    user: &str,
    serial_port: &str,
    baud_rate: u32,
    shell: &str,
) -> String {
    match kind {
        SessionKind::Serial => format!("{serial_port} @{baud_rate}"),
        SessionKind::Local => {
            let shell = shell.trim();
            if shell.is_empty() {
                "Local".to_string()
            } else {
                std::path::Path::new(shell)
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or(shell)
                    .to_string()
            }
        }
        _ if user.trim().is_empty() => host.to_string(),
        _ => format!("{user}@{host}"),
    }
}

fn json_string(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::Bool(b)) => b.to_string(),
        _ => String::new(),
    }
}

fn json_optional_string(v: Option<&Value>) -> Option<String> {
    match v {
        Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
        Some(Value::Null) | None => None,
        Some(other) => {
            let s = json_string(Some(other));
            if s.is_empty() {
                None
            } else {
                Some(s)
            }
        }
    }
}

fn json_bool(v: Option<&Value>, default: bool) -> bool {
    match v {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => match s.trim().to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" => true,
            "false" | "0" | "no" => false,
            _ => default,
        },
        Some(Value::Number(n)) => n.as_u64().map(|x| x != 0).unwrap_or(default),
        _ => default,
    }
}

fn json_u16_positive(v: Option<&Value>, default: u16) -> u16 {
    match v {
        Some(Value::Number(n)) => n
            .as_u64()
            .and_then(|x| u16::try_from(x).ok())
            .filter(|&p| p > 0)
            .unwrap_or(default),
        Some(Value::String(s)) => s
            .trim()
            .parse::<u16>()
            .ok()
            .filter(|&p| p > 0)
            .unwrap_or(default),
        _ => default,
    }
}

fn json_u32_positive(v: Option<&Value>, default: u32) -> u32 {
    match v {
        Some(Value::Number(n)) => n
            .as_u64()
            .and_then(|x| u32::try_from(x).ok())
            .filter(|&p| p > 0)
            .unwrap_or(default),
        Some(Value::String(s)) => s
            .trim()
            .parse::<u32>()
            .ok()
            .filter(|&p| p > 0)
            .unwrap_or(default),
        _ => default,
    }
}

fn json_u8_positive(v: Option<&Value>, default: u8) -> u8 {
    match v {
        Some(Value::Number(n)) => n
            .as_u64()
            .and_then(|x| u8::try_from(x).ok())
            .filter(|&p| p > 0)
            .unwrap_or(default),
        Some(Value::String(s)) => s
            .trim()
            .parse::<u8>()
            .ok()
            .filter(|&p| p > 0)
            .unwrap_or(default),
        _ => default,
    }
}

fn json_u64(v: Option<&Value>, default: u64) -> u64 {
    match v {
        Some(Value::Number(n)) => n.as_u64().unwrap_or(default),
        Some(Value::String(s)) => s.trim().parse().unwrap_or(default),
        _ => default,
    }
}

fn normalize_import_parity(raw: &str) -> String {
    match raw.trim().to_ascii_lowercase().as_str() {
        "odd" => "odd".into(),
        "even" => "even".into(),
        "mark" => "mark".into(),
        "space" => "space".into(),
        _ => default_parity(),
    }
}

fn normalize_import_flow(raw: &str) -> String {
    match raw.trim().to_ascii_lowercase().as_str() {
        "xonxoff" | "software" | "xon/xoff" => "xonxoff".into(),
        "rtscts" | "hardware" | "rts/cts" => "rtscts".into(),
        "dsrdtr" | "dsr/dtr" => "dsrdtr".into(),
        _ => default_flow(),
    }
}

fn normalize_import_backspace(raw: &str) -> &'static str {
    match raw.trim().to_ascii_lowercase().as_str() {
        "del" => "del",
        "bs" => "bs",
        _ => "auto",
    }
}

#[cfg(test)]
impl Default for Session {
    fn default() -> Self {
        sanitize_tests::new_empty()
    }
}

#[cfg(test)]
mod sanitize_tests {
    use super::*;

    /// Blank session for unit tests (defaults differ from serde import defaults).
    pub(super) fn new_empty() -> Session {
        Session {
            id: String::new(),
            name: String::new(),
            host: String::new(),
            port: default_port(),
            user: "root".into(),
            auth: AuthMethod::Password,
            password: Secret::default(),
            key_passphrase: Secret::default(),
            private_key: Secret::default(),
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
            enable_prompt_setup: false,
            enable_command_panel: false,
        }
    }

    #[test]
    fn password_auth_drops_private_key_fields() {
        let mut s = new_empty();
        s.kind = SessionKind::Ssh;
        s.auth = AuthMethod::Password;
        s.password = Secret::new("login");
        s.key_passphrase = Secret::new("should-drop");
        s.private_key = Secret::new("-----BEGIN OPENSSH PRIVATE KEY-----\n");
        s.sanitize_for_kind();
        assert_eq!(s.password.as_str(), "login");
        assert!(s.key_passphrase.is_empty());
        assert!(s.private_key.is_empty());
        let raw = serde_json::to_string(&s).unwrap();
        assert!(!raw.contains("\"password\":"));
        assert!(!raw.contains("\"private_key\""));
        assert!(!raw.contains("key_passphrase"));
    }

    #[test]
    fn key_auth_keeps_passphrase_and_key_fields() {
        let mut s = new_empty();
        s.kind = SessionKind::Ssh;
        s.auth = AuthMethod::Key;
        s.password = Secret::new("should-drop-login");
        s.key_passphrase = Secret::new("key-pass");
        s.private_key = Secret::new("/home/u/.ssh/id_ed25519");
        s.sanitize_for_kind();
        assert!(s.password.is_empty());
        assert_eq!(s.key_passphrase.as_str(), "key-pass");
        assert_eq!(s.private_key.as_str(), "/home/u/.ssh/id_ed25519");
        let raw = serde_json::to_string(&s).unwrap();
        assert!(!raw.contains("\"password\":"));
        assert!(!raw.contains("\"private_key\""));
        assert!(!raw.contains("key_passphrase"));
    }

    #[test]
    fn ssh_drops_serial_and_local_fields() {
        let mut s = new_empty();
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
        let mut s = new_empty();
        s.kind = SessionKind::Serial;
        s.host = "1.2.3.4".into();
        s.port = 22;
        s.user = "root".into();
        s.auth = AuthMethod::Key;
        s.password = Secret::new("secret");
        s.key_passphrase = Secret::new("kp");
        s.private_key = Secret::new("PEM");
        s.enable_sftp = true;
        s.enable_prompt_setup = true;
        s.serial_port = "COM3".into();
        s.baud_rate = 115_200;
        s.shell = "/bin/zsh".into();
        s.sanitize_for_kind();
        assert!(s.host.is_empty());
        assert_eq!(s.port, 0);
        assert!(s.user.is_empty());
        assert_eq!(s.auth, AuthMethod::Password);
        assert!(s.password.is_empty());
        assert!(s.key_passphrase.is_empty());
        assert!(s.private_key.is_empty());
        assert!(!s.enable_sftp);
        assert!(!s.enable_prompt_setup);
        assert!(s.shell.is_empty());
        assert_eq!(s.serial_port, "COM3");
        assert_eq!(s.baud_rate, 115_200);
    }

    #[test]
    fn json_keeps_dialog_fields_omits_other_kinds() {
        let mut serial = new_empty();
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

        let mut ssh = new_empty();
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
            raw.find("\"enable_sftp\"").unwrap() < raw.find("\"enable_prompt_setup\"").unwrap()
        );
        assert!(
            raw.find("\"enable_prompt_setup\"").unwrap()
                < raw.find("\"enable_command_panel\"").unwrap()
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
