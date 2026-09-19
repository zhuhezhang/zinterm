use anyhow::Result;

use super::super::structs::*;

impl ConfigStore {
    pub(super) fn persist_snapshot(
        &self,
        kind: SaveKind,
    ) -> Result<crate::config::persist::PersistSnapshot> {
        let data_dir = self.data_dir_path()?;
        Ok(crate::config::persist::build_snapshot(
            data_dir,
            self.path.clone(),
            &self.cache,
            kind,
        ))
    }

    /// Synchronously write the selected parts (and optional vault). Used by
    /// tests, first-run migration, and shutdown flushes.
    pub fn save_parts(&self, kind: SaveKind) -> Result<()> {
        let snap = self.persist_snapshot(kind)?;
        crate::config::persist::write_snapshot(&snap)
    }

    /// Queue a background persist so the UI thread is not blocked on disk I/O.
    pub fn save_later(&self, kind: SaveKind) {
        match self.persist_snapshot(kind) {
            Ok(snap) => {
                if let Err(e) = crate::config::persist::schedule(snap) {
                    tracing::warn!("failed to schedule config persist: {e:#}");
                }
            }
            Err(e) => tracing::warn!("failed to build config persist snapshot: {e:#}"),
        }
    }

    /// Block until the background persist worker has finished pending writes.
    pub fn flush_persist() -> Result<()> {
        crate::config::persist::flush()
    }

    pub fn save(&self) -> Result<()> {
        self.save_parts(SaveKind::ALL)
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

    #[test]
    fn split_files_roundtrip_keeps_sessions_and_settings() {
        let mut store = temp_store();
        store.cache.sessions.push(sample_session("alpha"));
        store.cache.empty_groups = vec!["prod".into()];
        store.set_theme_pref("dark".into());
        store.set_session_group_collapsed("prod", false);
        store.save().unwrap();

        let dir = store.path.parent().unwrap().to_path_buf();
        assert!(dir.join("settings.json").exists());
        assert!(dir.join("ui-state.json").exists());
        let sessions_raw = std::fs::read_to_string(&store.path).unwrap();
        assert!(sessions_raw.contains("alpha"));
        assert!(sessions_raw.contains("\"empty_groups\""));
        assert!(!sessions_raw.contains("\"groups\""));
        assert!(!sessions_raw.contains("\"theme_pref\""));
        let settings_raw = std::fs::read_to_string(dir.join("settings.json")).unwrap();
        assert!(settings_raw.contains("dark"));

        // Simulate split load into a fresh store path under the same dir.
        let reloaded = {
            let mut s = ConfigStore {
                path: store.path.clone(),
                cache: ConfigFile::default(),
            };
            // Re-read using the same helpers as load().
            let settings_path = crate::config::persist::settings_path(&dir);
            let ui_path = crate::config::persist::ui_state_path(&dir);
            let commands_path = crate::config::persist::commands_path(&dir);
            s.cache =
                ConfigStore::load_split(&store.path, &commands_path, &settings_path, &ui_path)
                    .unwrap();
            s
        };
        assert_eq!(reloaded.sessions().len(), 1);
        assert_eq!(reloaded.theme_pref(), "dark");
        let collapsed = reloaded.collapsed_session_groups().unwrap();
        assert!(!collapsed.iter().any(|g| g == "prod"));

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn saved_password_goes_to_vault_not_sessions_json() {
        let _guard = crate::config::vault::tests::with_test_master_key();
        let mut store = temp_store();
        store.set_save_passwords(true);
        assert!(store.save_passwords());
        let password = "p@ss word!^&*中文";
        let id = store.upsert(Session {
            name: "windows-password".into(),
            host: "192.168.100.2".into(),
            port: 22,
            user: "root".into(),
            password: Secret::new(password),
            ..Session::default()
        });

        store.save().unwrap();
        let raw = std::fs::read_to_string(&store.path).unwrap();
        assert!(!raw.contains(password));
        // `save_passwords` may appear; session secret fields must not.
        assert!(!raw.contains("\"password\":"));
        assert!(!raw.contains("\"private_key\""));
        assert!(!raw.contains("\"key_passphrase\""));

        let dir = store.path.parent().unwrap();
        let vault_raw =
            std::fs::read_to_string(dir.join(crate::config::vault::VAULT_FILE)).unwrap();
        assert!(!vault_raw.contains(password));
        let loaded = crate::config::vault::get_secrets(dir, &id)
            .unwrap()
            .expect("vault entry");
        assert_eq!(loaded.password, password);

        let _ = std::fs::remove_file(&store.path);
        let _ = std::fs::remove_file(dir.join(crate::config::vault::VAULT_FILE));
    }

    #[test]
    fn save_passwords_switch_gates_key_material_vault_write() {
        let _guard = crate::config::vault::tests::with_test_master_key();
        let dir_cleanup;
        {
            let mut store = temp_store();
            assert!(!store.save_passwords());
            let key_body =
                "-----BEGIN OPENSSH PRIVATE KEY-----\nAAAA\n-----END OPENSSH PRIVATE KEY-----\n";
            let id = store.upsert(Session {
                name: "key-prompt".into(),
                host: "10.0.0.8".into(),
                user: "ubuntu".into(),
                auth: AuthMethod::Key,
                private_key: Secret::new(key_body),
                key_passphrase: Secret::new("kp-secret"),
                ..Session::default()
            });
            // Switch off: in-memory secrets must not be pushed to the vault
            // (same rule as the welcome-page credential dialog).
            store.save().unwrap();
            let dir = store.path.parent().unwrap().to_path_buf();
            dir_cleanup = dir.clone();
            assert!(
                crate::config::vault::get_secrets(&dir, &id)
                    .unwrap()
                    .is_none(),
                "vault must stay empty while save-passwords is off"
            );

            store.set_save_passwords(true);
            store.save().unwrap();
            let loaded = crate::config::vault::get_secrets(&dir, &id)
                .unwrap()
                .expect("vault entry after enabling save-passwords");
            assert!(loaded.password.is_empty());
            assert_eq!(loaded.private_key, key_body);
            assert_eq!(loaded.passphrase, "kp-secret");

            let _ = std::fs::remove_file(&store.path);
        }
        let _ = std::fs::remove_file(dir_cleanup.join(crate::config::vault::VAULT_FILE));
    }
}
