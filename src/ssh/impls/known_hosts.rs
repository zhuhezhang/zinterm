//! Host-key verification store (#109-5 / #105).
//!
//! TOFU-style known-hosts JSON (aligned with zauterm):
//!   • unknown host  → prompt; on accept the fingerprint is remembered here
//!   • known + match  → connect silently
//!   • known + differ → flagged as *changed* (possible MITM); confirm to replace
//!
//! File: `zinterm-known-hosts.json` beside `sessions.json`:
//! ```json
//! {
//!   "v": 1,
//!   "hosts": {
//!     "example.com:22": {
//!       "fingerprint": "<sha256-base64>",
//!       "keyType": "ssh-ed25519",
//!       "updatedAt": 1710000000000
//!     }
//!   }
//! }
//! ```
//! Only SHA256 fingerprints are stored (not full public keys).

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use super::structs::HostKeyStatus;
use anyhow::{Context, Result};
use russh::keys::{HashAlg, PublicKey};
use serde::{Deserialize, Serialize};

/// On-disk filename (next to `sessions.json` in the per-user data dir).
pub(crate) const KNOWN_HOSTS_FILE: &str = "zinterm-known-hosts.json";

/// Test-only path override (ignored when `None`).
static PATH_OVERRIDE: Mutex<Option<PathBuf>> = Mutex::new(None);

#[derive(Debug, Clone, Serialize, Deserialize)]
struct KnownHostsFile {
    v: u32,
    hosts: HashMap<String, HostRecord>,
}

impl Default for KnownHostsFile {
    fn default() -> Self {
        Self {
            v: 1,
            hosts: HashMap::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct HostRecord {
    /// SHA256 fingerprint (base64, no `SHA256:` prefix).
    fingerprint: String,
    #[serde(rename = "keyType", default)]
    key_type: String,
    #[serde(rename = "updatedAt", default)]
    updated_at: u64,
}

/// `host:port` lookup key (trimmed, lowercased host — matches zauterm).
fn host_port_key(host: &str, port: u16) -> String {
    format!("{}:{}", host.trim().to_lowercase(), port)
}

fn path() -> PathBuf {
    if let Ok(guard) = PATH_OVERRIDE.lock() {
        if let Some(ref p) = *guard {
            return p.clone();
        }
    }
    crate::config::data_dir().join(KNOWN_HOSTS_FILE)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Normalize a fingerprint for comparison (strip optional `SHA256:` / padding).
fn fp_key(fp: &str) -> String {
    fp.trim()
        .trim_start_matches("SHA256:")
        .trim_start_matches("sha256:")
        .trim_end_matches('=')
        .to_string()
}

fn fp_eq(a: &str, b: &str) -> bool {
    fp_key(a) == fp_key(b)
}

/// Human-readable SHA256 fingerprint (`SHA256:…`) shown in the trust dialog.
pub fn fingerprint(key: &PublicKey) -> String {
    key.fingerprint(HashAlg::Sha256).to_string()
}

/// Fingerprint form stored on disk (no `SHA256:` prefix).
fn fingerprint_stored(key: &PublicKey) -> String {
    fp_key(&fingerprint(key))
}

fn key_type_of(key: &PublicKey) -> String {
    key.algorithm().to_string()
}

fn read_store() -> KnownHostsFile {
    let p = path();
    match fs::read_to_string(&p) {
        Ok(raw) => serde_json::from_str(&raw).unwrap_or_default(),
        Err(_) => KnownHostsFile::default(),
    }
}

fn write_store(store: &KnownHostsFile) -> Result<()> {
    let p = path();
    if let Some(parent) = p.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create config dir {}", parent.display()))?;
    }
    let tmp = p.with_extension("json.tmp");
    let data = serde_json::to_string_pretty(store).context("serialize known hosts")?;
    fs::write(&tmp, &data).with_context(|| format!("write {}", tmp.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600));
    }
    fs::rename(&tmp, &p).with_context(|| format!("finalise {}", p.display()))?;
    Ok(())
}

/// Check a presented server key against the fingerprint store.
pub fn verify(host: &str, port: u16, key: &PublicKey) -> HostKeyStatus {
    let id = host_port_key(host, port);
    let want = fingerprint_stored(key);
    let store = read_store();
    match store.hosts.get(&id) {
        Some(rec) if fp_eq(&rec.fingerprint, &want) => HostKeyStatus::Match,
        Some(_) => HostKeyStatus::Changed,
        None => HostKeyStatus::Unknown,
    }
}

/// Remember (or replace) the fingerprint for `host:port`.
pub fn remember(host: &str, port: u16, key: &PublicKey) -> Result<()> {
    let id = host_port_key(host, port);
    let mut store = read_store();
    store.v = 1;
    store.hosts.insert(
        id,
        HostRecord {
            fingerprint: fingerprint_stored(key),
            key_type: key_type_of(key),
            updated_at: now_ms(),
        },
    );
    write_store(&store)
}

/// Wipe the known-hosts file so every host is treated as unknown again.
pub fn clear() -> Result<()> {
    let p = path();
    match fs::remove_file(&p) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("remove {}", p.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use russh::keys::PrivateKey;
    use std::sync::MutexGuard;
    use std::time::{SystemTime, UNIX_EPOCH};

    static LOCK: Mutex<()> = Mutex::new(());

    struct TestPathGuard {
        _lock: MutexGuard<'static, ()>,
        dir: PathBuf,
    }

    impl Drop for TestPathGuard {
        fn drop(&mut self) {
            if let Ok(mut g) = PATH_OVERRIDE.lock() {
                *g = None;
            }
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    fn with_temp_store() -> TestPathGuard {
        let lock = LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "zinterm-kh-{}-{}",
            std::process::id(),
            nanos
        ));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join(KNOWN_HOSTS_FILE);
        *PATH_OVERRIDE.lock().unwrap_or_else(|p| p.into_inner()) = Some(file);
        TestPathGuard { _lock: lock, dir }
    }

    fn test_key() -> PublicKey {
        let sk = PrivateKey::random(&mut rand::thread_rng(), russh::keys::Algorithm::Ed25519)
            .expect("generate key");
        sk.public_key().clone()
    }

    #[test]
    fn host_port_key_lowercases_host() {
        assert_eq!(host_port_key(" Example.COM ", 22), "example.com:22");
    }

    #[test]
    fn fp_key_strips_prefix_and_padding() {
        assert_eq!(fp_key("SHA256:abc="), "abc");
        assert_eq!(fp_key("sha256:abc"), "abc");
    }

    #[test]
    fn verify_remember_clear_roundtrip() {
        let _g = with_temp_store();
        let key = test_key();

        assert!(matches!(verify("host.example", 22, &key), HostKeyStatus::Unknown));
        remember("Host.Example", 22, &key).unwrap();
        assert!(matches!(verify("host.example", 22, &key), HostKeyStatus::Match));

        let other = test_key();
        assert!(matches!(
            verify("host.example", 22, &other),
            HostKeyStatus::Changed
        ));

        let raw = fs::read_to_string(path()).unwrap();
        assert!(raw.contains("\"v\": 1"));
        assert!(raw.contains("\"fingerprint\""));
        assert!(raw.contains("\"keyType\""));
        assert!(!raw.contains("BEGIN"));
        assert_eq!(path().file_name().unwrap(), KNOWN_HOSTS_FILE);

        clear().unwrap();
        assert!(matches!(verify("host.example", 22, &key), HostKeyStatus::Unknown));
        assert!(!path().exists());
    }
}
