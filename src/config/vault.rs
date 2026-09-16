//! OS-keyring–backed credentials vault (password / private key / passphrase).
//!
//! Secrets are stored in `zinterm-credentials-vault.json` as ChaCha20-Poly1305
//! ciphertext. The 32-byte master key lives in the OS credential store
//! (`keyring`), never beside the vault file.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{anyhow, Context, Result};
use base64::Engine as _;
use chacha20poly1305::{
    aead::{Aead, KeyInit},
    ChaCha20Poly1305, Nonce,
};
use keyring::Entry;
use rand::RngCore;
use serde::{Deserialize, Serialize};

use super::Secret;

const SERVICE: &str = "zinterm";
const MASTER_KEY_ACCOUNT: &str = "vault-master-key-v1";
pub(crate) const VAULT_FILE: &str = "zinterm-credentials-vault.json";

/// Optional test master-key override (avoids touching the real OS keyring).
static TEST_MASTER_KEY: Mutex<Option<[u8; 32]>> = Mutex::new(None);

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub(crate) struct VaultEntry {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    #[serde(rename = "privateKey", skip_serializing_if = "Option::is_none")]
    pub private_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub passphrase: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Vault {
    v: u32,
    entries: HashMap<String, VaultEntry>,
}

impl Default for Vault {
    fn default() -> Self {
        Self {
            v: 1,
            entries: HashMap::new(),
        }
    }
}

/// Plaintext secrets loaded from (or about to be written to) the vault.
#[derive(Debug, Clone, Default)]
pub(crate) struct PlainSecrets {
    pub password: String,
    pub private_key: String,
    pub passphrase: String,
}

impl PlainSecrets {
    pub fn from_session_fields(
        password: &Secret,
        private_key: &Secret,
        key_passphrase: &Secret,
    ) -> Self {
        Self {
            password: password.as_str().to_string(),
            private_key: private_key.as_str().to_string(),
            passphrase: key_passphrase.as_str().to_string(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.password.is_empty() && self.private_key.is_empty() && self.passphrase.is_empty()
    }
}

fn vault_path(app_data: &Path) -> PathBuf {
    app_data.join(VAULT_FILE)
}

fn get_or_create_master_key() -> Result<[u8; 32]> {
    {
        let guard = TEST_MASTER_KEY
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(k) = *guard {
            return Ok(k);
        }
    }

    let entry = Entry::new(SERVICE, MASTER_KEY_ACCOUNT).map_err(|e| anyhow!("{e}"))?;
    match entry.get_password() {
        Ok(pw) => {
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(pw.trim())
                .context("invalid vault master key encoding")?;
            if bytes.len() != 32 {
                anyhow::bail!("invalid vault master key length");
            }
            let mut key = [0u8; 32];
            key.copy_from_slice(&bytes);
            Ok(key)
        }
        Err(_) => {
            let mut key = [0u8; 32];
            rand::thread_rng().fill_bytes(&mut key);
            let b64 = base64::engine::general_purpose::STANDARD.encode(key);
            entry
                .set_password(&b64)
                .map_err(|e| anyhow!("failed to store vault master key: {e}"))?;
            Ok(key)
        }
    }
}

/// Whether the OS keyring can provide (or create) a vault master key.
pub(crate) fn is_encryption_available() -> bool {
    get_or_create_master_key().is_ok()
}

fn encrypt_field_with_key(key: &[u8; 32], plain: &str) -> Result<String> {
    let cipher = ChaCha20Poly1305::new_from_slice(key).map_err(|e| anyhow!("{e}"))?;
    let mut nonce_bytes = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);
    let ciphertext = cipher
        .encrypt(nonce, plain.as_bytes())
        .map_err(|e| anyhow!("vault encrypt error: {e}"))?;
    let mut out = Vec::with_capacity(12 + ciphertext.len());
    out.extend_from_slice(&nonce_bytes);
    out.extend_from_slice(&ciphertext);
    Ok(base64::engine::general_purpose::STANDARD.encode(out))
}

fn decrypt_field_with_key(key: &[u8; 32], b64: &str) -> Result<String> {
    let raw = base64::engine::general_purpose::STANDARD
        .decode(b64)
        .context("invalid vault ciphertext encoding")?;
    if raw.len() < 13 {
        anyhow::bail!("vault ciphertext too short");
    }
    let (nonce_bytes, ct) = raw.split_at(12);
    let cipher = ChaCha20Poly1305::new_from_slice(key).map_err(|e| anyhow!("{e}"))?;
    let nonce = Nonce::from_slice(nonce_bytes);
    let plain = cipher
        .decrypt(nonce, ct)
        .map_err(|e| anyhow!("vault decrypt error: {e}"))?;
    String::from_utf8(plain).context("vault plaintext is not UTF-8")
}

fn encrypt_field(plain: &str) -> Result<String> {
    let key = get_or_create_master_key()?;
    encrypt_field_with_key(&key, plain)
}

fn decrypt_field(b64: &str) -> Result<String> {
    let key = get_or_create_master_key()?;
    decrypt_field_with_key(&key, b64)
}

fn read_vault(app_data: &Path) -> Vault {
    let path = vault_path(app_data);
    match fs::read_to_string(&path) {
        Ok(raw) => serde_json::from_str(&raw).unwrap_or_default(),
        Err(_) => Vault::default(),
    }
}

fn write_vault(app_data: &Path, vault: &Vault) -> Result<()> {
    fs::create_dir_all(app_data)
        .with_context(|| format!("failed to create {}", app_data.display()))?;
    let path = vault_path(app_data);
    let tmp = path.with_extension("json.tmp");
    let data = serde_json::to_string_pretty(vault).context("failed to serialize vault")?;
    fs::write(&tmp, data).with_context(|| format!("failed to write {}", tmp.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600))
            .with_context(|| format!("failed to set permissions on {}", tmp.display()))?;
    }
    fs::rename(&tmp, &path).with_context(|| format!("failed to finalise {}", path.display()))?;
    Ok(())
}

fn try_decrypt_entry(enc: &VaultEntry) -> Result<VaultEntry> {
    Ok(VaultEntry {
        password: enc.password.as_ref().map(|s| decrypt_field(s)).transpose()?,
        private_key: enc
            .private_key
            .as_ref()
            .map(|s| decrypt_field(s))
            .transpose()?,
        passphrase: enc
            .passphrase
            .as_ref()
            .map(|s| decrypt_field(s))
            .transpose()?,
    })
}

/// Load plaintext secrets for `session_id`, if any.
pub(crate) fn get_secrets(app_data: &Path, session_id: &str) -> Result<Option<PlainSecrets>> {
    if session_id.is_empty() {
        return Ok(None);
    }
    if !is_encryption_available() {
        return Ok(None);
    }
    let vault = read_vault(app_data);
    let Some(enc) = vault.entries.get(session_id) else {
        return Ok(None);
    };
    let plain = try_decrypt_entry(enc)?;
    Ok(Some(PlainSecrets {
        password: plain.password.unwrap_or_default(),
        private_key: plain.private_key.unwrap_or_default(),
        passphrase: plain.passphrase.unwrap_or_default(),
    }))
}

/// Apply many session secret updates with a **single** vault read/write.
///
/// When `save_passwords` is false, non-empty secrets are left untouched in the
/// vault; empty secrets still remove that entry so clearing a field sticks.
pub(crate) fn sync_secrets_batch(
    app_data: &Path,
    save_passwords: bool,
    entries: &[(String, PlainSecrets)],
) -> Result<()> {
    if !is_encryption_available() {
        let any = entries.iter().any(|(_, s)| !s.is_empty());
        if any {
            anyhow::bail!("credentials vault encryption unavailable (OS keyring)");
        }
        return Ok(());
    }

    let mut vault = read_vault(app_data);
    let mut changed = false;

    for (session_id, secrets) in entries {
        if session_id.is_empty() {
            continue;
        }
        if !save_passwords && !secrets.is_empty() {
            continue;
        }

        if secrets.is_empty() {
            if vault.entries.remove(session_id).is_some() {
                changed = true;
            }
            continue;
        }

        let mut cur = VaultEntry::default();
        cur.password = if secrets.password.is_empty() {
            None
        } else {
            Some(encrypt_field(&secrets.password)?)
        };
        cur.private_key = if secrets.private_key.is_empty() {
            None
        } else {
            Some(encrypt_field(&secrets.private_key)?)
        };
        cur.passphrase = if secrets.passphrase.is_empty() {
            None
        } else {
            Some(encrypt_field(&secrets.passphrase)?)
        };

        if cur.password.is_none() && cur.private_key.is_none() && cur.passphrase.is_none() {
            if vault.entries.remove(session_id).is_some() {
                changed = true;
            }
        } else {
            vault.entries.insert(session_id.clone(), cur);
            changed = true;
        }
    }

    if changed {
        write_vault(app_data, &vault)?;
    }
    Ok(())
}

pub(crate) fn remove_secrets(app_data: &Path, session_id: &str) -> Result<()> {
    let mut vault = read_vault(app_data);
    vault.entries.remove(session_id);
    write_vault(app_data, &vault)
}

/// Copy an already-encrypted entry (no re-encrypt).
pub(crate) fn duplicate_secrets(app_data: &Path, from_id: &str, to_id: &str) -> Result<()> {
    if from_id.is_empty() || to_id.is_empty() {
        anyhow::bail!("invalid session id for vault duplicate");
    }
    let mut vault = read_vault(app_data);
    if let Some(e) = vault.entries.get(from_id).cloned() {
        vault.entries.insert(to_id.to_string(), e);
        write_vault(app_data, &vault)?;
    }
    Ok(())
}

pub(crate) fn clear_all(app_data: &Path) -> Result<()> {
    let path = vault_path(app_data);
    if path.exists() {
        fs::remove_file(&path)
            .with_context(|| format!("failed to remove {}", path.display()))?;
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::MutexGuard;
    use std::time::{SystemTime, UNIX_EPOCH};

    static TEST_KEY_LOCK: Mutex<()> = Mutex::new(());

    /// Holds the process-wide test master key until dropped.
    pub(crate) struct TestKeyGuard {
        _lock: MutexGuard<'static, ()>,
    }

    impl Drop for TestKeyGuard {
        fn drop(&mut self) {
            let mut g = TEST_MASTER_KEY
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            *g = None;
        }
    }

    /// Install a fixed master key for unit tests (avoids the real OS keyring).
    pub(crate) fn with_test_master_key() -> TestKeyGuard {
        let lock = TEST_KEY_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut key = [0u8; 32];
        key[0] = 7;
        key[31] = 9;
        *TEST_MASTER_KEY
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(key);
        TestKeyGuard { _lock: lock }
    }

    fn unique_temp_dir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "zinterm-vault-{}-{}-{}",
            label,
            std::process::id(),
            nanos
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn encrypt_decrypt_roundtrip() {
        let key = [3u8; 32];
        let ct = encrypt_field_with_key(&key, "secret-pass").unwrap();
        assert_ne!(ct, "secret-pass");
        assert_eq!(decrypt_field_with_key(&key, &ct).unwrap(), "secret-pass");
    }

    #[test]
    fn decrypt_rejects_tampered_ciphertext() {
        let key = [1u8; 32];
        let ct = encrypt_field_with_key(&key, "x").unwrap();
        let mut bytes = ct.into_bytes();
        if let Some(b) = bytes.last_mut() {
            *b ^= 0x01;
        }
        let tampered = String::from_utf8(bytes).unwrap();
        assert!(decrypt_field_with_key(&key, &tampered).is_err());
    }

    #[test]
    fn sync_get_duplicate_remove_clear() {
        let _guard = with_test_master_key();
        let dir = unique_temp_dir("round");

        sync_secrets_batch(
            &dir,
            true,
            &[(
                "sess-1".into(),
                PlainSecrets {
                    password: "p@ss".into(),
                    private_key: "-----BEGIN KEY-----\nA\n-----END KEY-----".into(),
                    passphrase: "ph".into(),
                },
            )],
        )
        .unwrap();

        let got = get_secrets(&dir, "sess-1").unwrap().expect("found");
        assert_eq!(got.password, "p@ss");
        assert_eq!(got.passphrase, "ph");
        assert!(got.private_key.contains("BEGIN KEY"));

        let raw = fs::read_to_string(vault_path(&dir)).unwrap();
        assert!(!raw.contains("p@ss"));
        assert_eq!(vault_path(&dir).file_name().unwrap(), VAULT_FILE);

        duplicate_secrets(&dir, "sess-1", "sess-2").unwrap();
        assert_eq!(
            get_secrets(&dir, "sess-2").unwrap().unwrap().password,
            "p@ss"
        );

        remove_secrets(&dir, "sess-1").unwrap();
        assert!(get_secrets(&dir, "sess-1").unwrap().is_none());

        clear_all(&dir).unwrap();
        assert!(!vault_path(&dir).exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn clearing_all_fields_removes_entry() {
        let _guard = with_test_master_key();
        let dir = unique_temp_dir("clear-fields");
        sync_secrets_batch(
            &dir,
            true,
            &[(
                "a".into(),
                PlainSecrets {
                    password: "x".into(),
                    ..PlainSecrets::default()
                },
            )],
        )
        .unwrap();
        sync_secrets_batch(&dir, true, &[("a".into(), PlainSecrets::default())]).unwrap();
        assert!(get_secrets(&dir, "a").unwrap().is_none());
        let _ = fs::remove_dir_all(&dir);
    }
}
