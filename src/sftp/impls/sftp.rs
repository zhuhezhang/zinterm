//! SFTP subsystem worker.
//!
//! Each terminal tab that spawns an SSH shell also spawns a *separate* SSH
//! connection for SFTP. This keeps the shell PTY completely unblocked: large
//! file transfers cannot stall readline or vim.
//!
//! The public API is a simple command channel (`SftpHandle::commands`) that
//! accepts `SftpCommand` messages. Results and status updates are pushed back
//! via the shared `UnboundedSender<SessionEvent>` that already exists for the
//! terminal tab.

#[path = "ctx.rs"]
mod ctx;
#[path = "fs_ops.rs"]
mod fs_ops;
#[path = "handle.rs"]
mod handle;
#[path = "listing.rs"]
mod listing;
#[path = "open_external.rs"]
mod open_external;
#[path = "paths.rs"]
mod paths;
#[path = "ssh_handler.rs"]
mod ssh_handler;
#[path = "text_edit.rs"]
mod text_edit;
#[path = "transfer_download.rs"]
mod transfer_download;
#[path = "transfer_upload.rs"]
mod transfer_upload;
#[path = "tree.rs"]
mod tree;

pub(crate) use paths::download_target_path;

use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use uuid::Uuid;

use anyhow::{anyhow, Context, Result};
use russh::keys::{HashAlg, PrivateKeyWithHashAlg};
use russh::Disconnect;
use russh_sftp::client::SftpSession;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

use crate::config::{AuthMethod, Session};
use crate::i18n::t;
use crate::ssh::SessionEvent;

use super::transfer::{SftpCommand, SftpHandle};

/// Turn an SFTP-worker failure into a status-bar message.
///
/// SFTP runs on its own SSH connection, fully separate from the shell PTY, so
/// when it can't connect the terminal keeps working — we just surface why in the
/// SFTP panel. The common bastion/jump-host case is "shell is allowed but the
/// `sftp` subsystem is not", which shows up as a failed subsystem request /
/// channel / handshake (or an explicit "permission denied"). For that family we
/// give a plain-language hint instead of the raw russh error (#190).
fn friendly_sftp_error(err: &anyhow::Error) -> String {
    let chain = err
        .chain()
        .map(|e| e.to_string().to_lowercase())
        .collect::<Vec<_>>()
        .join(" | ");
    let permission_like = [
        "subsystem",      // server refused the `sftp` subsystem request
        "sftp channel",   // channel_open_session refused
        "sftp handshake", // subsystem opened but no SFTP server behind it
        "permission",
        "denied",
        "prohibited", // "administratively prohibited"
        "not allowed",
    ]
    .iter()
    .any(|k| chain.contains(k));
    if permission_like {
        t(
            "SFTP 不可用,请检查是否有访问权限(服务器可能未开放 SFTP)",
            "SFTP unavailable — check whether you have permission (server may not allow SFTP)",
        )
        .to_string()
    } else {
        format!("{}: {err:#}", t("SFTP 错误", "SFTP error"))
    }
}

pub fn spawn_sftp(
    runtime: &tokio::runtime::Handle,
    session: Session,
    events: UnboundedSender<SessionEvent>,
    keepalive_secs: u32,
    algorithms: crate::config::AlgorithmPreferences,
) -> SftpHandle {
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let self_tx = cmd_tx.clone();
    let events_err = events.clone();
    let join = runtime.spawn(async move {
        if let Err(err) =
            run_sftp(session, cmd_rx, self_tx, events, keepalive_secs, algorithms).await
        {
            let _ = events_err.send(SessionEvent::SftpFailed(friendly_sftp_error(&err)));
        }
    });
    SftpHandle {
        commands: cmd_tx,
        join,
    }
}

async fn run_sftp(
    session: Session,
    mut commands: UnboundedReceiver<SftpCommand>,
    self_tx: UnboundedSender<SftpCommand>,
    events: UnboundedSender<SessionEvent>,
    keepalive_secs: u32,
    algorithms: crate::config::AlgorithmPreferences,
) -> Result<()> {
    let _ = events.send(SessionEvent::SftpStatus(
        t("SFTP 连接中...", "SFTP connecting...").into(),
    ));

    // Open a dedicated SSH connection for SFTP. Shares the shell path so H3C/VRP
    // gear that needs SSH-1.99- and a compact algorithm retry still works.
    let addr = format!("{}:{}", session.host, session.port);
    // Isolate external-editor files by connection and keep the server in the
    // visible local filename. Different hosts (or concurrent edit sessions)
    // can therefore open the same remote basename without sharing one temp
    // file or editor document (#318).
    let external_edit_prefix = paths::sanitize_filename(&session.host);
    let external_edit_dir = std::env::temp_dir().join("zinterm").join(format!(
        "{}-{}-{}",
        external_edit_prefix,
        session.port,
        Uuid::new_v4()
    ));
    let (mut handle, _) = crate::ssh::connect_transport(
        &addr,
        keepalive_secs,
        &algorithms,
        || ssh_handler::sftp_handler(&session, &events),
        None,
    )
    .await
    .with_context(|| format!("sftp connect {} failed", addr))?;

    // Resolve missing username/secret/key (shares the shell's prompt; the UI
    // de-dupes by tab id so SFTP on the same tab doesn't prompt a second time) (#110).
    let creds = match crate::ssh::resolve_credentials(&session, &events).await {
        Some(c) => c,
        None => return Err(anyhow!(t("已取消登录", "login cancelled"))),
    };

    // --- Authenticate (same method as the shell session) -------------------
    let authed = match session.auth {
        AuthMethod::Password => handle
            .authenticate_password(&creds.user, creds.secret.as_str())
            .await
            .context("sftp password auth failed")?
            .success(),
        AuthMethod::Key => {
            // Same dialog-supplied key / passphrase as the shell path (#133).
            let mut key_session = session.clone();
            if !creds.private_key.trim().is_empty() {
                key_session.private_key = crate::config::Secret::new(creds.private_key.clone());
            }
            let keypair =
                crate::ssh::load_session_private_key(&key_session, creds.secret.as_str())?;
            // RSA keys need an explicit SHA-2 hash; other key types don't.
            let hash = keypair.algorithm().is_rsa().then_some(HashAlg::Sha256);
            let key_with_hash = PrivateKeyWithHashAlg::new(Arc::new(keypair), hash);
            handle
                .authenticate_publickey(&creds.user, key_with_hash)
                .await
                .context("sftp publickey auth failed")?
                .success()
        }
    };

    if !authed {
        return Err(anyhow!(t("SFTP 认证失败", "SFTP authentication failed")));
    }

    // --- Open the sftp subsystem channel -----------------------------------
    let channel = handle
        .channel_open_session()
        .await
        .context("open sftp channel")?;
    channel
        .request_subsystem(true, "sftp")
        .await
        .context("request sftp subsystem")?;
    let sftp = SftpSession::new(channel.into_stream())
        .await
        .context("sftp handshake")?;
    // Share the session + connection so transfers can run on their own task,
    // leaving the command loop free to list/switch directories meanwhile (#116-2).
    let sftp = std::sync::Arc::new(sftp);
    let handle = std::sync::Arc::new(handle);

    // Per-transfer cancel flags, keyed by transfer id. A download task registers
    // its flag here; a CancelTransfer command flips it; the task removes it on
    // exit (#100 cancel download).
    let cancels: Arc<Mutex<HashMap<String, Arc<AtomicBool>>>> =
        Arc::new(Mutex::new(HashMap::new()));

    // Resolve the home directory and do an initial listing.
    let home = sftp
        .canonicalize(".")
        .await
        .unwrap_or_else(|_| "/".to_string());
    let _ = events.send(SessionEvent::SftpStatus(format!(
        "{} {}...",
        t("SFTP 加载", "SFTP loading"),
        home
    )));
    match listing::list_dir_impl(&sftp, &home).await {
        Ok(entries) => {
            let _ = events.send(SessionEvent::SftpEntries {
                path: home.clone(),
                entries,
            });
            let _ = events.send(SessionEvent::SftpStatus(home.clone()));
        }
        Err(e) => {
            let _ = events.send(SessionEvent::SftpError(listing::list_error_msg(&home, &e)));
        }
    }

    // --- Directory tree initialization -------------------------------------
    // tree_dirs: path -> [(child_name, child_full_path)] for directories only
    // tree_expanded: set of paths currently shown as expanded
    let mut tree_dirs: std::collections::HashMap<String, Vec<(String, String)>> =
        std::collections::HashMap::new();
    let mut tree_expanded: std::collections::HashSet<String> = std::collections::HashSet::new();

    // Fetch root "/" subdirs, then expand path down to home.
    let root_dirs = listing::list_dirs_only_impl(&sftp, "/")
        .await
        .unwrap_or_default();
    tree_dirs.insert("/".to_string(), root_dirs);
    tree_expanded.insert("/".to_string());

    // Walk each path segment from "/" toward home, expanding as we go.
    if home != "/" {
        let mut current = "/".to_string();
        for segment in home.trim_start_matches('/').split('/') {
            if segment.is_empty() {
                continue;
            }
            let child = format!("{}/{}", current.trim_end_matches('/'), segment);
            // Only expand if this child appeared in the parent listing.
            let found = tree_dirs
                .get(&current)
                .map(|c| c.iter().any(|(_, p)| p == &child))
                .unwrap_or(false);
            if !found {
                break;
            }
            let dirs = listing::list_dirs_only_impl(&sftp, &child)
                .await
                .unwrap_or_default();
            tree_dirs.insert(child.clone(), dirs);
            tree_expanded.insert(child.clone());
            current = child;
        }
    }
    {
        let mut nodes = Vec::new();
        tree::build_tree_nodes("/", 0, &tree_expanded, &tree_dirs, &mut nodes);
        let _ = events.send(SessionEvent::SftpTreeUpdate(nodes));
    }

    let mut ctx = ctx::WorkerCtx {
        sftp,
        handle,
        events,
        self_tx,
        cancels,
        tree_dirs,
        tree_expanded,
        external_edit_dir,
        external_edit_prefix,
    };

    while let Some(cmd) = commands.recv().await {
        match cmd {
            SftpCommand::Close => break,
            SftpCommand::ListDir(path) => listing::handle_list_dir(&mut ctx, path).await,
            SftpCommand::RefreshDir(path) => listing::handle_refresh_dir(&mut ctx, path).await,
            SftpCommand::ToggleTreeNode(path) => {
                listing::handle_toggle_tree_node(&mut ctx, path).await
            }
            SftpCommand::Download {
                remote,
                local_dir,
                conflict,
            } => transfer_download::handle_download(&mut ctx, remote, local_dir, conflict).await,
            SftpCommand::DownloadArchive {
                remote_dir,
                names,
                local_dir,
            } => {
                transfer_download::handle_download_archive(&mut ctx, remote_dir, names, local_dir)
                    .await
            }
            SftpCommand::CancelTransfer(id) => transfer_upload::handle_cancel(&mut ctx, id).await,
            SftpCommand::Upload {
                local,
                remote_dir,
                cleanup_after,
            } => transfer_upload::handle_upload(&mut ctx, local, remote_dir, cleanup_after).await,
            SftpCommand::UploadEdited { local, remote } => {
                transfer_upload::handle_upload_edited(&mut ctx, local, remote).await
            }
            SftpCommand::CopyTo {
                remotes,
                target,
                target_dir,
            } => transfer_upload::handle_copy_to(&mut ctx, remotes, target, target_dir).await,
            SftpCommand::Delete(path) => fs_ops::handle_delete(&mut ctx, path).await,
            SftpCommand::Rename { from, to } => fs_ops::handle_rename(&mut ctx, from, to).await,
            SftpCommand::Chmod { path, mode } => fs_ops::handle_chmod(&mut ctx, path, mode).await,
            SftpCommand::MkDir(path) => fs_ops::handle_mkdir(&mut ctx, path).await,
            SftpCommand::TouchFile(path) => fs_ops::handle_touch(&mut ctx, path).await,
            SftpCommand::OpenTemp { remote, edit } => {
                open_external::handle_open_temp(&mut ctx, remote, edit).await
            }
            SftpCommand::ReadText { remote, edit } => {
                text_edit::handle_read_text(&mut ctx, remote, edit).await
            }
            SftpCommand::WriteText { remote, content } => {
                text_edit::handle_write_text(&mut ctx, remote, content).await
            }
        }
    }

    let _ = ctx
        .handle
        .disconnect(Disconnect::ByApplication, "bye", "")
        .await;
    Ok(())
}
