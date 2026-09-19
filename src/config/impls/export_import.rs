use std::fs;
use std::path::Path;

use anyhow::{Context, Result};

use super::super::structs::*;
use super::normalize::*;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chacha20poly1305::{
    aead::{Aead, KeyInit},
    ChaCha20Poly1305,
};

impl ConfigStore {
    /// Marks a password encrypted with the **portable export key** (issue #46).
    /// Kept only so hand-edited / older import files can still decrypt.
    pub(super) const EXPORT_PREFIX: &'static str = "enc:exp:v1:";

    /// Fixed 32-byte key for portable exports. Baked into the binary so an
    /// exported file decrypts on any machine. Obfuscation only — see `ExportFile`.
    pub(super) const EXPORT_KEY: [u8; 32] = *b"zinterm.export.portable.key.v01!";

    // ── Portable export / import (issue #46) ──────────────────────────────

    /// `Date.toString()`-style local timestamp for the export file header.
    pub(super) fn format_export_timestamp() -> String {
        let now = chrono::Local::now();
        let offset_secs = now.offset().local_minus_utc();
        let sign = if offset_secs >= 0 { '+' } else { '-' };
        let abs = offset_secs.unsigned_abs();
        let oh = abs / 3600;
        let om = (abs % 3600) / 60;
        let zone = if offset_secs == 8 * 3600 {
            "中国标准时间"
        } else {
            "Local"
        };
        format!(
            "{} GMT{}{:02}{:02} ({})",
            now.format("%a %b %d %Y %H:%M:%S"),
            sign,
            oh,
            om,
            zone
        )
    }

    /// Decrypt a legacy export ciphertext (`enc:exp:v1:…`); `None` if it isn't one.
    pub(super) fn decrypt_export(s: &str) -> Option<String> {
        let b64 = s.strip_prefix(Self::EXPORT_PREFIX)?;
        let blob = URL_SAFE_NO_PAD.decode(b64).ok()?;
        if blob.len() < 12 {
            return None;
        }
        let (nonce_bytes, ciphertext) = blob.split_at(12);
        let cipher = ChaCha20Poly1305::new((&Self::EXPORT_KEY).into());
        let nonce = chacha20poly1305::Nonce::from_slice(nonce_bytes);
        let plain = cipher.decrypt(nonce, ciphertext).ok()?;
        String::from_utf8(plain).ok()
    }

    /// Export all sessions to a portable JSON file. Connection metadata stays
    /// plaintext and editable; password / private-key fields are cleared so the
    /// export never carries secrets. Returns the number of sessions.
    pub fn export_json(&self) -> Result<(String, usize)> {
        let empty_groups = self.collect_empty_groups();
        let mut sessions = self.cache.sessions.clone();
        for s in &mut sessions {
            s.sanitize_for_kind();
            // Never put secrets in the portable export; hand-fill on import.
            s.password = Secret::default();
            s.key_passphrase = Secret::default();
            s.private_key = Secret::default();
            // `last_used` is machine-local noise — don't carry it across.
            s.last_used = None;
        }
        let count = sessions.len();
        let out = ExportFile {
            zinterm_export: "sessions".into(),
            version: 1,
            exported_at: Self::format_export_timestamp(),
            empty_groups,
            sessions,
        };
        Ok((serde_json::to_string_pretty(&out)?, count))
    }

    /// Explicit `cache.empty_groups` entries that currently have no session in that
    /// folder or any descendant path.
    pub(super) fn collect_empty_groups(&self) -> Vec<String> {
        self.cache
            .empty_groups
            .iter()
            .filter(|g| {
                let g = g.trim();
                if g.is_empty() || is_reserved_session_group(g) {
                    return false;
                }
                let prefix = format!("{g}/");
                !self.cache.sessions.iter().any(|s| {
                    let sg = s.group.trim();
                    sg == g || sg.starts_with(&prefix)
                })
            })
            .cloned()
            .collect()
    }

    /// Export all sessions to a portable JSON file. Connection metadata stays
    /// plaintext and editable; password / private-key fields are cleared so the
    /// export never carries secrets. Returns the number of sessions.
    pub fn export_to(&self, path: &Path) -> Result<usize> {
        let (raw, count) = self.export_json()?;
        fs::write(path, raw).with_context(|| format!("failed to write {}", path.display()))?;
        Ok(count)
    }

    /// Import sessions from a string produced by [`Self::export_json`].
    ///
    /// Each session must include `kind`; SSH/Telnet also need a non-empty
    /// `host`, and Serial needs a non-empty `serial_port`. Other missing or
    /// malformed fields follow new-session dialog defaults. Optional plaintext
    /// `password` (login, password auth), `key_passphrase` / `private_key`
    /// (key auth; multi-line keys use `\n`) are kept only when Settings › Data ›
    /// save passwords is on; otherwise they are dropped before persist. A
    /// session is skipped only when the same group already has that `id` or the
    /// same `name`. Empty groups are restored first. Returns `(added, skipped)`.
    /// The store is saved if anything was added (sessions or empty groups).
    pub fn import_json(&mut self, raw: &str) -> Result<(usize, usize)> {
        let file: ExportFileImport =
            serde_json::from_str(raw).context("not a valid zinterm export file")?;
        if file.zinterm_export != "sessions" {
            anyhow::bail!(
                "invalid export: zinterm_export must be \"sessions\" (got {:?})",
                file.zinterm_export
            );
        }
        if file.version != 1 {
            anyhow::bail!("unsupported export version {} (expected 1)", file.version);
        }

        // Restore empty folders before sessions so the Quick Connect tree
        // already has those paths when connections land in sibling groups.
        let mut groups_added = false;
        for group in &file.empty_groups {
            let before = self.cache.empty_groups.len();
            self.add_group(group.clone());
            if self.cache.empty_groups.len() > before {
                groups_added = true;
            }
        }

        let save_passwords = self.save_passwords();
        let mut added = 0usize;
        let mut skipped = 0usize;
        for (i, raw_session) in file.sessions.iter().enumerate() {
            let mut s =
                Session::from_import_value(raw_session).with_context(|| format!("session[{i}]"))?;
            // Recover plaintext secrets for in-memory use / vault sync.
            // Accept an older export blob or hand-edited plaintext.
            if let Some(plain) = Self::decrypt_export(s.password.as_str()) {
                s.password = Secret::new(plain);
            }
            if let Some(plain) = Self::decrypt_export(s.key_passphrase.as_str()) {
                s.key_passphrase = Secret::new(plain);
            }
            if let Some(plain) = Self::decrypt_export(s.private_key.as_str()) {
                s.private_key = Secret::new(plain);
            }
            // Match Settings › Data › save passwords: ignore newly imported
            // secrets when the switch is off.
            if !save_passwords {
                s.password = Secret::default();
                s.key_passphrase = Secret::default();
                s.private_key = Secret::default();
            }
            s.sanitize_for_kind();
            if is_reserved_session_group(s.group.trim()) {
                s.group.clear();
            }

            let group = normalize_session_group(&s.group);
            let name = s.name.trim();
            let dup = self.cache.sessions.iter().any(|x| {
                normalize_session_group(&x.group) == group
                    && ((!s.id.is_empty() && x.id == s.id) || x.name.trim() == name)
            });
            if dup {
                skipped += 1;
                continue;
            }

            // Keep export id / saved_at; only fill blanks from broken files.
            if s.id.trim().is_empty() {
                s.id = Session::new_saved_id();
            }
            if s.saved_at == 0 {
                s.saved_at = Session::now_saved_at();
            }
            self.cache.sessions.push(s);
            added += 1;
        }
        if added > 0 || groups_added {
            self.save()?;
        }
        Ok((added, skipped))
    }

    /// Import sessions from a file produced by [`Self::export_to`].
    pub fn import_from(&mut self, path: &Path) -> Result<(usize, usize)> {
        let raw = fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        self.import_json(&raw)
    }
}

#[cfg(test)]
#[allow(unused_imports)]
mod tests {
    use super::super::super::structs::*;
    use super::super::defaults_migration::{
        fresh_config, migrate_defaults, normalize_macos_renderer_mode,
        normalize_reserved_session_groups,
    };
    use super::super::test_support::{encrypt_export, sample_session, temp_store};
    use uuid::Uuid;

    #[test]
    fn export_omits_password_and_key_fields() {
        let mut a = temp_store();
        let id = "saved-1700000000000-ab12".to_string();
        let saved_at = 1_700_000_000_000u64;
        a.cache.sessions.push(Session {
            id: id.clone(),
            saved_at,
            name: "pve".into(),
            host: "192.168.100.2".into(),
            port: 22,
            user: "root".into(),
            auth: AuthMethod::Key,
            password: Secret::new("s3cr3t"),
            key_passphrase: Secret::new("kp"),
            private_key: Secret::new(
                "-----BEGIN OPENSSH PRIVATE KEY-----\nAAAA\n-----END OPENSSH PRIVATE KEY-----\n",
            ),
            ..Session::default()
        });

        let export_path = std::env::temp_dir().join(format!("ms-exp-{}.json", Uuid::new_v4()));
        assert_eq!(a.export_to(&export_path).unwrap(), 1);

        let raw = std::fs::read_to_string(&export_path).unwrap();
        assert!(raw.contains("\"zinterm_export\": \"sessions\""));
        assert!(raw.contains("\"version\": 1"));
        assert!(raw.contains("\"empty_groups\""));
        assert!(raw.contains("\"sessions\""));
        assert!(raw.contains("192.168.100.2"));
        assert!(!raw.contains("s3cr3t"));
        assert!(!raw.contains("\"password\""));
        assert!(!raw.contains("key_passphrase"));
        assert!(!raw.contains("private_key"));
        assert!(!raw.contains(ConfigStore::EXPORT_PREFIX));

        // Round-trip restores connection metadata without secrets.
        let mut b = temp_store();
        assert_eq!(b.import_from(&export_path).unwrap(), (1, 0));
        assert_eq!(b.cache.sessions.len(), 1);
        assert!(b.cache.sessions[0].password.is_empty());
        assert!(b.cache.sessions[0].key_passphrase.is_empty());
        assert!(b.cache.sessions[0].private_key.is_empty());
        assert_eq!(b.cache.sessions[0].host, "192.168.100.2");
        assert_eq!(b.cache.sessions[0].id, id);
        assert_eq!(b.cache.sessions[0].saved_at, saved_at);

        assert_eq!(b.import_from(&export_path).unwrap(), (0, 1));

        let _ = std::fs::remove_file(&export_path);
        let _ = std::fs::remove_file(&a.path);
        let _ = std::fs::remove_file(&b.path);
    }

    #[test]
    fn import_plaintext_secrets_honor_save_passwords_switch() {
        let _guard = crate::config::vault::tests::with_test_master_key();
        let wrap = |sessions: &str| {
            format!(
                r#"{{"zinterm_export":"sessions","version":1,"exported_at":"t","empty_groups":[],"sessions":[{sessions}]}}"#
            )
        };
        let body = r#"{
            "kind":"ssh",
            "name":"box",
            "host":"10.0.0.9",
            "user":"root",
            "auth":"password",
            "password":"p@ss"
        }"#;

        let mut off = temp_store();
        assert!(!off.save_passwords());
        assert_eq!(off.import_json(&wrap(body)).unwrap(), (1, 0));
        assert!(off.cache.sessions[0].password.is_empty());

        let mut on = temp_store();
        on.set_save_passwords(true);
        assert_eq!(on.import_json(&wrap(body)).unwrap(), (1, 0));
        assert_eq!(on.cache.sessions[0].password.as_str(), "p@ss");

        let key_body = r#"{
            "kind":"ssh",
            "name":"keybox",
            "host":"10.0.0.10",
            "user":"root",
            "auth":"key",
            "key_passphrase":"key-pass",
            "private_key":"-----BEGIN OPENSSH PRIVATE KEY-----\\nAAAA\\n-----END OPENSSH PRIVATE KEY-----"
        }"#;
        let mut key_on = temp_store();
        key_on.set_save_passwords(true);
        assert_eq!(key_on.import_json(&wrap(key_body)).unwrap(), (1, 0));
        let s = &key_on.cache.sessions[0];
        assert!(s.password.is_empty());
        assert_eq!(s.key_passphrase.as_str(), "key-pass");
        assert_eq!(
            s.private_key.as_str(),
            "-----BEGIN OPENSSH PRIVATE KEY-----\nAAAA\n-----END OPENSSH PRIVATE KEY-----"
        );

        let mut key_off = temp_store();
        assert_eq!(key_off.import_json(&wrap(key_body)).unwrap(), (1, 0));
        assert!(key_off.cache.sessions[0].password.is_empty());
        assert!(key_off.cache.sessions[0].key_passphrase.is_empty());
        assert!(key_off.cache.sessions[0].private_key.is_empty());

        let _ = std::fs::remove_file(&off.path);
        let _ = std::fs::remove_file(&on.path);
        let _ = std::fs::remove_file(&key_on.path);
        let _ = std::fs::remove_file(&key_off.path);
    }

    #[test]
    fn import_accepts_legacy_export_encrypted_password_when_save_passwords_on() {
        let _guard = crate::config::vault::tests::with_test_master_key();
        let mut store = temp_store();
        store.set_save_passwords(true);
        let enc = encrypt_export("legacy-secret").unwrap();
        let raw = format!(
            r#"{{"zinterm_export":"sessions","version":1,"exported_at":"t","empty_groups":[],"sessions":[{{"kind":"ssh","name":"legacy","host":"1.2.3.4","password":"{enc}"}}]}}"#
        );
        assert_eq!(store.import_json(&raw).unwrap(), (1, 0));
        assert_eq!(store.cache.sessions[0].password.as_str(), "legacy-secret");
        let _ = std::fs::remove_file(&store.path);
    }

    #[test]
    fn import_skips_same_group_name_but_allows_other_group() {
        let mut store = temp_store();
        store.cache.sessions.push(Session {
            id: "saved-1700000000001-aaaa".into(),
            saved_at: 1,
            name: "box".into(),
            host: "1.1.1.1".into(),
            group: "prod".into(),
            ..Session::default()
        });

        let raw = serde_json::to_string_pretty(&ExportFile {
            zinterm_export: "sessions".into(),
            version: 1,
            exported_at: "test".into(),
            empty_groups: vec![],
            sessions: vec![
                Session {
                    id: "saved-1700000000002-bbbb".into(),
                    saved_at: 2,
                    name: "box".into(),
                    host: "2.2.2.2".into(),
                    group: "prod".into(),
                    ..Session::default()
                },
                Session {
                    id: "saved-1700000000003-cccc".into(),
                    saved_at: 3,
                    name: "box".into(),
                    host: "3.3.3.3".into(),
                    group: "lab".into(),
                    ..Session::default()
                },
            ],
        })
        .unwrap();

        assert_eq!(store.import_json(&raw).unwrap(), (1, 1));
        assert_eq!(store.sessions().len(), 2);
        assert!(store
            .sessions()
            .iter()
            .any(|s| s.group == "lab" && s.id == "saved-1700000000003-cccc"));
        assert!(!store
            .sessions()
            .iter()
            .any(|s| s.id == "saved-1700000000002-bbbb"));
    }

    #[test]
    fn import_requires_kind_and_kind_specific_endpoints() {
        let mut store = temp_store();
        let wrap = |sessions: &str| {
            format!(
                r#"{{"zinterm_export":"sessions","version":1,"exported_at":"t","empty_groups":[],"sessions":[{sessions}]}}"#
            )
        };

        let err = format!(
            "{:#}",
            store
                .import_json(&wrap(r#"{"name":"a","host":"1.1.1.1"}"#))
                .unwrap_err()
        );
        assert!(err.contains("kind is required"), "{err}");

        let err = format!(
            "{:#}",
            store
                .import_json(&wrap(r#"{"kind":"ssh","name":"a"}"#))
                .unwrap_err()
        );
        assert!(err.contains("host is required"), "{err}");

        let err = format!(
            "{:#}",
            store
                .import_json(&wrap(r#"{"kind":"telnet","name":"a","host":"  "}"#))
                .unwrap_err()
        );
        assert!(err.contains("host is required"), "{err}");

        let err = format!(
            "{:#}",
            store
                .import_json(&wrap(r#"{"kind":"serial","name":"a"}"#))
                .unwrap_err()
        );
        assert!(err.contains("serial_port is required"), "{err}");

        let err = format!(
            "{:#}",
            store
                .import_json(&wrap(r#"{"kind":"ftp","name":"a","host":"1.1.1.1"}"#))
                .unwrap_err()
        );
        assert!(err.contains("unsupported kind"), "{err}");
    }

    #[test]
    fn import_applies_new_session_defaults_for_optional_fields() {
        let mut store = temp_store();
        let raw = r#"{
            "zinterm_export": "sessions",
            "version": 1,
            "exported_at": "t",
            "empty_groups": [],
            "sessions": [
                {
                    "kind": "ssh",
                    "host": "10.0.0.1",
                    "port": "nope",
                    "parity": "weird",
                    "flow_control": "bogus",
                    "backspace_mode": "??",
                    "enable_sftp": "maybe"
                },
                {
                    "kind": "telnet",
                    "host": "10.0.0.2"
                },
                {
                    "kind": "serial",
                    "serial_port": "COM3",
                    "baud_rate": 0,
                    "data_bits": "x",
                    "stop_bits": -1
                },
                {
                    "kind": "local"
                }
            ]
        }"#;
        assert_eq!(store.import_json(raw).unwrap(), (4, 0));
        let sessions = store.sessions();

        let ssh = sessions.iter().find(|s| s.host == "10.0.0.1").unwrap();
        assert_eq!(ssh.kind, SessionKind::Ssh);
        assert_eq!(ssh.port, 22);
        assert_eq!(ssh.user, "");
        assert_eq!(ssh.name, "10.0.0.1");
        assert_eq!(ssh.auth, AuthMethod::Password);
        assert_eq!(ssh.parity, "none");
        assert_eq!(ssh.flow_control, "none");
        assert_eq!(ssh.backspace_mode, "auto");
        assert_eq!(ssh.encoding, "UTF-8");
        assert!(!ssh.enable_sftp);
        assert!(!ssh.enable_command_panel);

        let telnet = sessions.iter().find(|s| s.host == "10.0.0.2").unwrap();
        assert_eq!(telnet.kind, SessionKind::Telnet);
        assert_eq!(telnet.port, 23);
        assert_eq!(telnet.name, "10.0.0.2");

        let serial = sessions.iter().find(|s| s.serial_port == "COM3").unwrap();
        assert_eq!(serial.kind, SessionKind::Serial);
        assert_eq!(serial.baud_rate, 9_600);
        assert_eq!(serial.data_bits, 8);
        assert_eq!(serial.stop_bits, 1);
        assert_eq!(serial.name, "COM3 @9600");

        let local = sessions
            .iter()
            .find(|s| s.kind == SessionKind::Local)
            .unwrap();
        assert_eq!(local.name, "Local");
        assert!(local.host.is_empty());
    }

    #[test]
    fn export_import_restores_empty_groups_before_sessions() {
        let mut a = temp_store();
        a.add_group("empty-lab".into());
        a.add_group("has-session".into());
        a.cache.sessions.push(Session {
            name: "box".into(),
            host: "10.0.0.1".into(),
            group: "has-session".into(),
            ..Session::default()
        });

        let export_path =
            std::env::temp_dir().join(format!("ms-exp-groups-{}.json", Uuid::new_v4()));
        assert_eq!(a.export_to(&export_path).unwrap(), 1);
        let raw = std::fs::read_to_string(&export_path).unwrap();
        assert!(
            raw.contains("\"empty_groups\": [\n    \"empty-lab\"\n  ]")
                || raw.contains("\"empty-lab\"")
        );
        assert!(
            !raw.contains("\"has-session\"") || {
                // has-session may appear on the session's group field, but not in empty_groups.
                let file: ExportFile = serde_json::from_str(&raw).unwrap();
                !file.empty_groups.iter().any(|g| g == "has-session")
                    && file.empty_groups.iter().any(|g| g == "empty-lab")
            }
        );

        let mut b = temp_store();
        assert_eq!(b.import_from(&export_path).unwrap(), (1, 0));
        assert!(b.session_group_exists("empty-lab"));
        assert!(b.sessions().iter().any(|s| s.group == "has-session"));

        let _ = std::fs::remove_file(&export_path);
        let _ = std::fs::remove_file(&a.path);
        let _ = std::fs::remove_file(&b.path);
    }
}
