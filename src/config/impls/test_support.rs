use anyhow::Result;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chacha20poly1305::aead::{Aead, AeadCore, KeyInit, OsRng};
use chacha20poly1305::ChaCha20Poly1305;
use uuid::Uuid;

use super::super::structs::*;

pub(super) fn temp_store() -> ConfigStore {
    let dir = std::env::temp_dir().join(format!("ms-test-{}", Uuid::new_v4()));
    let _ = std::fs::create_dir_all(&dir);
    ConfigStore {
        path: dir.join("sessions.json"),
        cache: ConfigFile::default(),
    }
}

/// Build a legacy `enc:exp:v1:…` blob so import can still decrypt older exports.
pub(super) fn encrypt_export(plaintext: &str) -> Result<String> {
    let cipher = ChaCha20Poly1305::new((&ConfigStore::EXPORT_KEY).into());
    let nonce = ChaCha20Poly1305::generate_nonce(&mut OsRng);
    let ciphertext = cipher
        .encrypt(&nonce, plaintext.as_bytes())
        .map_err(|e| anyhow::anyhow!("export encrypt error: {e}"))?;
    let mut blob = nonce.to_vec();
    blob.extend_from_slice(&ciphertext);
    Ok(format!(
        "{}{}",
        ConfigStore::EXPORT_PREFIX,
        URL_SAFE_NO_PAD.encode(&blob)
    ))
}

pub(super) fn sample_session(name: &str) -> Session {
    Session {
        name: name.into(),
        host: "192.168.100.2".into(),
        port: 22,
        user: "root".into(),
        ..Session::default()
    }
}
