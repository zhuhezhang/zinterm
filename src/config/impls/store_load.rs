use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::super::structs::*;
use super::defaults_migration::*;
use super::paths::data_dir;

impl ConfigStore {
    pub(super) fn data_dir_path(&self) -> Result<PathBuf> {
        self.path
            .parent()
            .map(|p| p.to_path_buf())
            .context("config path has no parent directory")
    }

    /// Hydrate in-memory secrets from the OS-keyring vault.
    pub(super) fn hydrate_secrets_from_vault(config_dir: &Path, sessions: &mut [Session]) {
        for session in sessions {
            // sessions.json must never carry secrets; drop anything leftover.
            session.password = Secret::default();
            session.key_passphrase = Secret::default();
            session.private_key = Secret::default();
            match crate::config::vault::get_secrets(config_dir, &session.id) {
                Ok(Some(secrets)) => {
                    session.password = Secret::new(secrets.password);
                    session.private_key = Secret::new(secrets.private_key);
                    session.key_passphrase = Secret::new(secrets.passphrase);
                }
                Ok(None) => {}
                Err(e) => tracing::warn!(
                    "failed to load vault secrets for session {}: {e:#}",
                    session.id
                ),
            }
        }
    }

    // ── Public API ────────────────────────────────────────────────────────

    /// Load (or initialise) the config files. On any parse error we back up the
    /// broken file and start fresh — losing saved sessions is better than
    /// crashing at launch.
    pub fn load() -> Result<Self> {
        let path = Self::config_path()?;
        let config_dir = path
            .parent()
            .context("config path has no parent directory")?
            .to_path_buf();

        fs::create_dir_all(&config_dir)
            .with_context(|| format!("failed to create config dir {}", config_dir.display()))?;

        let settings_path = crate::config::persist::settings_path(&config_dir);
        let ui_path = crate::config::persist::ui_state_path(&config_dir);
        let commands_path = crate::config::persist::commands_path(&config_dir);
        let split_layout = settings_path.exists() || ui_path.exists() || commands_path.exists();

        let mut migrated = false;
        let cache = if split_layout || path.exists() {
            let mut cfg = if split_layout {
                Self::load_split(&path, &commands_path, &settings_path, &ui_path)?
            } else {
                match Self::load_legacy_monolithic(&path)? {
                    Some(cfg) => {
                        migrated = true;
                        cfg
                    }
                    None => fresh_config(),
                }
            };

            if cfg.save_passwords && !crate::config::vault::is_encryption_available() {
                tracing::warn!(
                    "save_passwords was on but credentials vault is unavailable; disabling"
                );
                cfg.save_passwords = false;
            }
            Self::hydrate_secrets_from_vault(&config_dir, &mut cfg.sessions);
            for session in &mut cfg.sessions {
                if session.sanitize_for_kind() {
                    migrated = true;
                }
            }
            for cmd in &mut cfg.command_history {
                *cmd = repair_history_newlines(std::mem::take(cmd));
            }
            dedup_keep_last(&mut cfg.command_history);
            migrated |= normalize_reserved_session_groups(&mut cfg);
            migrated |= migrate_defaults(&mut cfg);
            migrated |= migrate_output_highlight_rules(&mut cfg);
            let sanitized = crate::ssh::sanitize_algorithm_preferences(&cfg.algorithm_preferences);
            if sanitized != cfg.algorithm_preferences {
                cfg.algorithm_preferences = sanitized;
                migrated = true;
            }
            cfg
        } else {
            fresh_config()
        };

        let store = Self { path, cache };
        if migrated {
            if let Err(e) = store.save_parts(SaveKind::ALL) {
                tracing::warn!("failed to persist config migration: {e:#}");
            }
        }
        Ok(store)
    }

    pub(super) fn load_split(
        sessions_path: &Path,
        commands_path: &Path,
        settings_path: &Path,
        ui_path: &Path,
    ) -> Result<ConfigFile> {
        let mut cfg = ConfigFile::default();

        if sessions_path.exists() {
            let raw = fs::read_to_string(sessions_path)
                .with_context(|| format!("failed to read {}", sessions_path.display()))?;
            match serde_json::from_str::<SessionsFile>(&raw) {
                Ok(sessions) => sessions.apply_to(&mut cfg),
                Err(_) => match serde_json::from_str::<ConfigFile>(&raw) {
                    Ok(full) => cfg = full,
                    Err(err) => {
                        let backup = sessions_path.with_extension("json.broken");
                        let _ = fs::rename(sessions_path, &backup);
                        tracing::warn!(
                            "sessions file was corrupt ({err}); backed up to {}",
                            backup.display()
                        );
                    }
                },
            }
        }

        match crate::config::persist::read_json_file::<CommandsFile>(commands_path) {
            Ok(Some(commands)) => commands.apply_to(&mut cfg),
            Ok(None) => {}
            Err(err) => {
                tracing::warn!("commands file unreadable ({err:#}); keeping defaults");
            }
        }

        match crate::config::persist::read_json_file::<SettingsFile>(settings_path) {
            Ok(Some(settings)) => settings.apply_to(&mut cfg),
            Ok(None) => {}
            Err(err) => {
                tracing::warn!("settings file unreadable ({err:#}); keeping defaults");
            }
        }

        match crate::config::persist::read_json_file::<UiStateFile>(ui_path) {
            Ok(Some(ui)) => ui.apply_to(&mut cfg),
            Ok(None) => {}
            Err(err) => {
                tracing::warn!("ui-state file unreadable ({err:#}); keeping defaults");
            }
        }

        Ok(cfg)
    }

    pub(super) fn load_legacy_monolithic(path: &Path) -> Result<Option<ConfigFile>> {
        let raw = fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        match serde_json::from_str::<ConfigFile>(&raw) {
            Ok(cfg) => Ok(Some(cfg)),
            Err(err) => {
                let backup = path.with_extension("json.broken");
                let _ = fs::rename(path, &backup);
                tracing::warn!(
                    "config file was corrupt ({err}); backed up to {}",
                    backup.display()
                );
                Ok(None)
            }
        }
    }

    pub(super) fn config_path() -> Result<PathBuf> {
        Ok(data_dir().join("sessions.json"))
    }
}
