//! Global SSH/SFTP algorithm preference lists (persisted in settings.json).

use serde::{Deserialize, Serialize};

/// Ordered algorithm names the client offers during SSH negotiation.
///
/// Empty `kex` / `server_host_key` / `cipher` / `hmac` are rejected at apply
/// time (sanitizer falls back to defaults). Empty `compress` becomes `["none"]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlgorithmPreferences {
    #[serde(default = "default_kex")]
    pub kex: Vec<String>,
    #[serde(default = "default_server_host_key", rename = "serverHostKey", alias = "server_host_key")]
    pub server_host_key: Vec<String>,
    #[serde(default = "default_cipher")]
    pub cipher: Vec<String>,
    #[serde(default = "default_hmac")]
    pub hmac: Vec<String>,
    #[serde(default = "default_compress")]
    pub compress: Vec<String>,
}

impl Default for AlgorithmPreferences {
    fn default() -> Self {
        Self::builtin_default()
    }
}

impl AlgorithmPreferences {
    /// Selection that matches ZinTerm's historical COMPAT + russh DEFAULT key/MAC
    /// profile (compression stays `none` so enabling zlib is opt-in).
    pub fn builtin_default() -> Self {
        Self {
            kex: default_kex(),
            server_host_key: default_server_host_key(),
            cipher: default_cipher(),
            hmac: default_hmac(),
            compress: default_compress(),
        }
    }

    /// True when prefs match the built-in default (enables automatic legacy retry).
    pub fn is_builtin_default(&self) -> bool {
        self == &Self::builtin_default()
    }
}

fn default_kex() -> Vec<String> {
    [
        "curve25519-sha256",
        "curve25519-sha256@libssh.org",
        "diffie-hellman-group16-sha512",
        "diffie-hellman-group14-sha256",
        "ecdh-sha2-nistp256",
        "ecdh-sha2-nistp384",
        "ecdh-sha2-nistp521",
        "diffie-hellman-group14-sha1",
        "diffie-hellman-group1-sha1",
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

fn default_server_host_key() -> Vec<String> {
    [
        "ssh-ed25519",
        "ecdsa-sha2-nistp256",
        "ecdsa-sha2-nistp384",
        "ecdsa-sha2-nistp521",
        "rsa-sha2-512",
        "rsa-sha2-256",
        "ssh-rsa",
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

fn default_cipher() -> Vec<String> {
    [
        "chacha20-poly1305@openssh.com",
        "aes256-gcm@openssh.com",
        "aes256-ctr",
        "aes192-ctr",
        "aes128-ctr",
        "aes256-cbc",
        "aes192-cbc",
        "aes128-cbc",
        "3des-cbc",
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

fn default_hmac() -> Vec<String> {
    [
        "hmac-sha2-512-etm@openssh.com",
        "hmac-sha2-256-etm@openssh.com",
        "hmac-sha2-512",
        "hmac-sha2-256",
        "hmac-sha1-etm@openssh.com",
        "hmac-sha1",
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

fn default_compress() -> Vec<String> {
    vec!["none".to_string()]
}
