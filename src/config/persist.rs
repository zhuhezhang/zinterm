//! Background persist worker: coalesce saves and write split config files off
//! the UI thread.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use anyhow::{Context, Result};

use super::structs::{
    SaveKind, SessionsFile, SettingsFile, UiStateFile,
};
use super::vault::{self, PlainSecrets};

const SETTINGS_FILE: &str = "settings.json";
const UI_STATE_FILE: &str = "ui-state.json";

/// Snapshot of the pieces a background job may write.
pub(crate) struct PersistSnapshot {
    pub data_dir: PathBuf,
    pub sessions_path: PathBuf,
    pub sessions: Option<SessionsFile>,
    pub settings: Option<SettingsFile>,
    pub ui: Option<UiStateFile>,
    pub vault: Option<VaultSnapshot>,
}

pub(crate) struct VaultSnapshot {
    pub save_passwords: bool,
    pub entries: Vec<(String, PlainSecrets)>,
}

enum PersistMsg {
    Save(PersistSnapshot),
    /// Drain pending work then ack.
    Flush(SyncSender<Result<()>>),
}

fn channel() -> &'static Mutex<Sender<PersistMsg>> {
    static TX: OnceLock<Mutex<Sender<PersistMsg>>> = OnceLock::new();
    TX.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<PersistMsg>();
        std::thread::Builder::new()
            .name("zinterm-persist".into())
            .spawn(move || persist_loop(rx))
            .expect("failed to spawn zinterm-persist thread");
        Mutex::new(tx)
    })
}

fn persist_loop(rx: Receiver<PersistMsg>) {
    loop {
        let first = match rx.recv() {
            Ok(m) => m,
            Err(_) => return,
        };
        match first {
            PersistMsg::Flush(ack) => {
                let result = drain_and_write(&rx, None);
                let _ = ack.send(result);
            }
            PersistMsg::Save(snap) => {
                // Coalesce a short burst of saves into one write.
                match rx.recv_timeout(Duration::from_millis(20)) {
                    Ok(PersistMsg::Save(more)) => {
                        let mut merged = snap;
                        merge_snapshot(&mut merged, more);
                        if let Err(e) = drain_and_write(&rx, Some(merged)) {
                            tracing::warn!("background config persist failed: {e:#}");
                        }
                    }
                    Ok(PersistMsg::Flush(ack)) => {
                        let result = drain_and_write(&rx, Some(snap));
                        let _ = ack.send(result);
                    }
                    Err(RecvTimeoutError::Timeout) => {
                        if let Err(e) = write_snapshot(&snap) {
                            tracing::warn!("background config persist failed: {e:#}");
                        }
                    }
                    Err(RecvTimeoutError::Disconnected) => {
                        let _ = write_snapshot(&snap);
                        return;
                    }
                }
            }
        }
    }
}

/// Pull any queued saves (and nested flushes) then write the coalesced snapshot.
fn drain_and_write(rx: &Receiver<PersistMsg>, initial: Option<PersistSnapshot>) -> Result<()> {
    let mut snap = initial;
    let mut extra_acks: Vec<SyncSender<Result<()>>> = Vec::new();
    while let Ok(msg) = rx.try_recv() {
        match msg {
            PersistMsg::Save(more) => match &mut snap {
                Some(existing) => merge_snapshot(existing, more),
                None => snap = Some(more),
            },
            PersistMsg::Flush(ack) => extra_acks.push(ack),
        }
    }
    let result = match snap {
        Some(s) => write_snapshot(&s),
        None => Ok(()),
    };
    for ack in extra_acks {
        let _ = ack.send(match &result {
            Ok(()) => Ok(()),
            Err(e) => Err(anyhow::anyhow!("{e:#}")),
        });
    }
    result
}

fn merge_snapshot(dst: &mut PersistSnapshot, src: PersistSnapshot) {
    // Prefer the newer job's data_dir / sessions_path when present.
    dst.data_dir = src.data_dir;
    dst.sessions_path = src.sessions_path;
    if src.sessions.is_some() {
        dst.sessions = src.sessions;
    }
    if src.settings.is_some() {
        dst.settings = src.settings;
    }
    if src.ui.is_some() {
        dst.ui = src.ui;
    }
    if src.vault.is_some() {
        dst.vault = src.vault;
    }
}

/// Schedule a non-blocking persist. Errors only if the worker channel is gone.
pub(crate) fn schedule(snap: PersistSnapshot) -> Result<()> {
    let tx = channel()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone();
    tx.send(PersistMsg::Save(snap))
        .context("persist worker channel closed")?;
    Ok(())
}

/// Block until the worker has drained pending saves (and any coalesced writes).
pub(crate) fn flush() -> Result<()> {
    let (ack_tx, ack_rx) = mpsc::sync_channel(1);
    let tx = channel()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone();
    tx.send(PersistMsg::Flush(ack_tx))
        .context("persist worker channel closed")?;
    ack_rx
        .recv()
        .context("persist worker did not acknowledge flush")?
}

pub(crate) fn settings_path(data_dir: &Path) -> PathBuf {
    data_dir.join(SETTINGS_FILE)
}

pub(crate) fn ui_state_path(data_dir: &Path) -> PathBuf {
    data_dir.join(UI_STATE_FILE)
}

pub(crate) fn write_snapshot(snap: &PersistSnapshot) -> Result<()> {
    if let Some(sessions) = &snap.sessions {
        // Strip secrets before serialising sessions.json.
        let mut disk = sessions.clone();
        for session in &mut disk.sessions {
            session.sanitize_for_kind();
            session.password = crate::config::Secret::default();
            session.key_passphrase = crate::config::Secret::default();
            session.private_key = crate::config::Secret::default();
        }
        atomic_write_json(&snap.sessions_path, &disk)?;
    }
    if let Some(settings) = &snap.settings {
        atomic_write_json(&settings_path(&snap.data_dir), settings)?;
    }
    if let Some(ui) = &snap.ui {
        atomic_write_json(&ui_state_path(&snap.data_dir), ui)?;
    }
    if let Some(vault) = &snap.vault {
        if let Err(e) = vault::sync_secrets_batch(
            &snap.data_dir,
            vault.save_passwords,
            &vault.entries,
        ) {
            tracing::warn!("failed to sync credentials vault: {e:#}");
        }
    }
    Ok(())
}

pub(crate) fn build_snapshot(
    data_dir: PathBuf,
    sessions_path: PathBuf,
    cache: &crate::config::ConfigFile,
    kind: SaveKind,
) -> PersistSnapshot {
    PersistSnapshot {
        data_dir,
        sessions_path,
        sessions: kind
            .contains(SaveKind::SESSIONS)
            .then(|| SessionsFile::from_config(cache)),
        settings: kind
            .contains(SaveKind::SETTINGS)
            .then(|| SettingsFile::from_config(cache)),
        ui: kind
            .contains(SaveKind::UI)
            .then(|| UiStateFile::from_config(cache)),
        vault: kind.contains(SaveKind::VAULT).then(|| {
            let entries = cache
                .sessions
                .iter()
                .map(|s| {
                    (
                        s.id.clone(),
                        PlainSecrets::from_session_fields(
                            &s.password,
                            &s.private_key,
                            &s.key_passphrase,
                        ),
                    )
                })
                .collect();
            VaultSnapshot {
                save_passwords: cache.save_passwords,
                entries,
            }
        }),
    }
}

pub(crate) fn atomic_write_json<T: serde::Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs_create_dir_all(parent)?;
    }
    let raw = serde_json::to_string_pretty(value).context("failed to serialize config JSON")?;
    let tmp = path.with_extension(format!(
        "{}.tmp",
        path.extension().and_then(|e| e.to_str()).unwrap_or("json")
    ));
    // Prefer sibling `foo.json.tmp` naming used historically for sessions.json.
    let tmp = if path.extension().and_then(|e| e.to_str()) == Some("json") {
        path.with_extension("json.tmp")
    } else {
        tmp
    };
    std::fs::write(&tmp, &raw).with_context(|| format!("failed to write {}", tmp.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(&tmp, path)
        .with_context(|| format!("failed to finalise {}", path.display()))?;
    Ok(())
}

fn fs_create_dir_all(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir)
        .with_context(|| format!("failed to create {}", dir.display()))
}

pub(crate) fn read_json_file<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    if !path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let value = serde_json::from_str(&raw)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    Ok(Some(value))
}
