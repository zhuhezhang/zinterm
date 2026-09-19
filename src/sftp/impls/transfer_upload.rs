use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use futures::stream::{FuturesUnordered, StreamExt};
use russh::client;
use russh_sftp::client::{RawSftpSession, SftpSession};
use russh_sftp::protocol::{FileAttributes, OpenFlags};
use tokio::sync::mpsc::UnboundedSender;
use uuid::Uuid;

use crate::i18n::t;
use crate::ssh::SessionEvent;

use super::super::transfer::SftpCommand;
use super::listing::list_dir_impl;
use super::paths::{
    base_name, cleanup_import_path, emit_transfer, local_file_name_utf8, parent_dir,
    sanitize_filename,
};
use super::ssh_handler::SftpClientHandler;
use super::transfer_download::{download_dir, download_impl};

pub(super) async fn stage_remote_for_copy(
    sftp: &SftpSession,
    handle: &client::Handle<SftpClientHandler>,
    remote: &str,
    events: &UnboundedSender<SessionEvent>,
) -> Result<(PathBuf, PathBuf)> {
    let cleanup_root = std::env::temp_dir().join(format!("zinterm-remote-copy-{}", Uuid::new_v4()));
    tokio::fs::create_dir_all(&cleanup_root)
        .await
        .with_context(|| format!("failed to create temp dir {}", cleanup_root.display()))?;

    let name = sanitize_filename(&base_name(remote));
    let local_path = cleanup_root.join(&name);
    let local_parent = cleanup_root.to_string_lossy().to_string();
    let is_dir = sftp
        .metadata(remote)
        .await
        .ok()
        .map(|m| (m.permissions.unwrap_or(0) & 0o170_000) == 0o040_000)
        .unwrap_or(false);
    let no_cancel = Arc::new(AtomicBool::new(false));
    let id = Uuid::new_v4().to_string();

    if is_dir {
        tokio::fs::create_dir_all(&local_path)
            .await
            .with_context(|| format!("failed to create temp dir {}", local_path.display()))?;
        let empty = list_dir_impl(sftp, remote)
            .await
            .map(|entries| entries.is_empty())
            .unwrap_or(false);
        if !empty {
            download_dir(sftp, handle, remote, &local_parent, events).await?;
        }
    } else {
        let local = local_path.to_string_lossy().to_string();
        download_impl(handle, remote, &local, &name, &id, events, &no_cancel).await?;
    }

    Ok((local_path, cleanup_root))
}

/// Recursively upload a local directory tree into `remote_parent` (#50).
///
/// Iterative work-stack: mirror each local dir to the remote (create_dir, whose
/// "already exists" error is ignored), then upload its files with the pipelined
/// path. Symlinks and other special files are skipped.
pub(super) async fn upload_dir(
    handle: &client::Handle<SftpClientHandler>,
    sftp: &SftpSession,
    local_root: &Path,
    remote_parent: &str,
    events: &UnboundedSender<SessionEvent>,
) -> Result<()> {
    // Folder uploads aren't individually cancellable from the UI; a throwaway
    // never-set flag satisfies upload_pipelined's signature.
    let no_cancel = Arc::new(AtomicBool::new(false));
    let root_name = local_file_name_utf8(local_root)?;
    let remote_root = format!("{}/{}", remote_parent.trim_end_matches('/'), root_name);
    let mut stack = vec![(local_root.to_path_buf(), remote_root)];
    while let Some((ldir, rdir)) = stack.pop() {
        // Best-effort mkdir; an error usually just means the dir already exists.
        let _ = sftp.create_dir(&rdir).await;
        let mut rd = tokio::fs::read_dir(&ldir)
            .await
            .with_context(|| format!("read local dir {}", ldir.display()))?;
        while let Some(entry) = rd.next_entry().await.context("read dir entry")? {
            let lpath = entry.path();
            let name = local_file_name_utf8(&lpath)?;
            let rchild = format!("{}/{}", rdir, name);
            let ft = entry.file_type().await.context("file type")?;
            if ft.is_dir() {
                stack.push((lpath, rchild));
            } else if ft.is_file() {
                let id = Uuid::new_v4().to_string();
                upload_pipelined(handle, &lpath, &rchild, &name, &id, events, &no_cancel).await?;
            }
        }
    }
    Ok(())
}

/// Pipelined SFTP upload (#16).
///
/// The high-level `SftpSession`/`File` writes one chunk and waits for the
/// server's ack before sending the next, so throughput is capped by the
/// round-trip time (~15x slower than scp on a latent link).  Here we open a
/// dedicated raw SFTP channel and keep many WRITE requests in flight at once
/// (each tagged with its absolute offset, so out-of-order completion is fine),
/// which hides the latency and brings us within a single order of magnitude of
/// native scp.
pub(super) async fn upload_pipelined(
    handle: &client::Handle<SftpClientHandler>,
    local: &Path,
    remote: &str,
    name: &str,
    id: &str,
    events: &UnboundedSender<SessionEvent>,
    cancel: &Arc<AtomicBool>,
) -> Result<bool> {
    use tokio::io::AsyncReadExt;

    const CHUNK: usize = 32 * 1024; // safe SFTP write size
    const MAX_INFLIGHT: usize = 32; // ~1 MB of outstanding writes hides the RTT

    let total = tokio::fs::metadata(local)
        .await
        .map(|m| m.len())
        .unwrap_or(0);
    let mut local_file = tokio::fs::File::open(local)
        .await
        .with_context(|| format!("open local {}", local.display()))?;

    // Dedicated raw SFTP channel for the transfer (keeps the browse session
    // responsive and lets us issue concurrent WRITE requests).
    let channel = handle
        .channel_open_session()
        .await
        .context("open sftp upload channel")?;
    channel
        .request_subsystem(true, "sftp")
        .await
        .context("request sftp subsystem")?;
    let raw = Arc::new(RawSftpSession::new(channel.into_stream()));
    raw.init().await.context("sftp upload handshake")?;

    let fhandle = raw
        .open(
            remote,
            OpenFlags::CREATE | OpenFlags::WRITE | OpenFlags::TRUNCATE,
            FileAttributes::default(),
        )
        .await
        .with_context(|| format!("create remote {remote}"))?
        .handle;

    emit_transfer(events, id, name, true, 0, total, 0, "");

    let mut offset: u64 = 0;
    let mut done: u64 = 0;
    let mut last = Instant::now();
    let mut eof = false;
    let mut err: Option<anyhow::Error> = None;
    let mut cancelled = false;
    let mut inflight = FuturesUnordered::new();

    while !eof || !inflight.is_empty() {
        if cancel.load(Ordering::Relaxed) {
            cancelled = true;
            eof = true; // stop reading more; drain what's in flight
        }
        // Top up the pipeline with fresh WRITE requests.
        while !eof && inflight.len() < MAX_INFLIGHT {
            let mut buf = vec![0u8; CHUNK];
            match local_file.read(&mut buf).await {
                Ok(0) => eof = true,
                Ok(n) => {
                    buf.truncate(n);
                    let off = offset;
                    offset += n as u64;
                    let raw2 = raw.clone();
                    let h = fhandle.clone();
                    inflight.push(async move { raw2.write(h, off, buf).await.map(|_| n as u64) });
                }
                Err(e) => {
                    err = Some(anyhow!("read local file: {e}"));
                    eof = true;
                }
            }
        }
        match inflight.next().await {
            Some(Ok(n)) => {
                done += n;
                if last.elapsed() >= Duration::from_millis(150) {
                    last = Instant::now();
                    emit_transfer(events, id, name, true, done, total, 0, "");
                }
            }
            Some(Err(e)) => {
                err = Some(anyhow!("write remote file: {e}"));
                eof = true; // stop reading more
            }
            None => {}
        }
        if err.is_some() {
            break;
        }
    }

    let _ = raw.close(fhandle).await;
    if let Some(e) = err {
        // Drop the partial remote file so a failed upload leaves no junk.
        let _ = raw.remove(remote).await;
        return Err(e);
    }
    if cancelled {
        // Remove the half-written remote file on cancel (#100).
        let _ = raw.remove(remote).await;
        emit_transfer(
            events,
            id,
            name,
            true,
            done,
            total,
            4,
            t("已取消", "Cancelled"),
        );
        return Ok(false);
    }
    emit_transfer(events, id, name, true, done, total.max(done), 1, "");
    Ok(true)
}

#[allow(unused_variables)]
pub(super) async fn handle_cancel(ctx: &mut super::ctx::WorkerCtx, id: String) {
    let flag = ctx.cancels.lock().unwrap().get(&id).cloned();
    if let Some(flag) = flag {
        flag.store(true, Ordering::Relaxed);
    }
}

#[allow(unused_variables)]
pub(super) async fn handle_upload(
    ctx: &mut super::ctx::WorkerCtx,
    local: PathBuf,
    remote_dir: String,
    cleanup_after: Option<PathBuf>,
) {
    let sftp = ctx.sftp.clone();
    let events = ctx.events.clone();
    let handle = ctx.handle.clone();
    let cancels = ctx.cancels.clone();
    // Run on its own task so the command loop stays free to list /
    // switch directories during the transfer (#116-2).
    let sftp = sftp.clone();
    let handle = handle.clone();
    let events = events.clone();
    // Register a cancel flag up-front under the file id so a
    // CancelTransfer arriving mid-upload can flip it (#100).
    let up_id = Uuid::new_v4().to_string();
    let cancel = Arc::new(AtomicBool::new(false));
    cancels
        .lock()
        .unwrap()
        .insert(up_id.clone(), cancel.clone());
    let cancels_done = cancels.clone();
    tokio::spawn(async move {
        // A directory source → recursively upload the whole tree (#50).
        let is_dir = tokio::fs::metadata(&local)
            .await
            .map(|m| m.is_dir())
            .unwrap_or(false);
        if is_dir {
            let dirname = match local_file_name_utf8(&local) {
                Ok(name) => name,
                Err(e) => {
                    let _ = events.send(SessionEvent::SftpStatus(format!(
                        "{}: {e}",
                        t("上传失败", "Upload failed")
                    )));
                    if let Some(path) = cleanup_after.as_deref() {
                        cleanup_import_path(path).await;
                    }
                    cancels_done.lock().unwrap().remove(&up_id);
                    return;
                }
            };
            let _ = events.send(SessionEvent::SftpStatus(format!(
                "{} {}/...",
                t("上传文件夹", "Uploading folder"),
                dirname
            )));
            let res = upload_dir(&handle, &sftp, &local, &remote_dir, &events).await;
            if let Ok(entries) = list_dir_impl(&sftp, &remote_dir).await {
                let _ = events.send(SessionEvent::SftpEntries {
                    path: remote_dir.clone(),
                    entries,
                });
            }
            match res {
                Ok(_) => {
                    let _ = events.send(SessionEvent::SftpStatus(format!(
                        "{}: {}",
                        t("上传完成", "Uploaded"),
                        dirname
                    )));
                }
                Err(e) => {
                    let _ = events.send(SessionEvent::SftpStatus(format!(
                        "{}: {e}",
                        t("上传失败", "Upload failed")
                    )));
                }
            }
        } else {
            let filename = match local_file_name_utf8(&local) {
                Ok(name) => name,
                Err(e) => {
                    let _ = events.send(SessionEvent::SftpStatus(format!(
                        "{}: {e}",
                        t("上传失败", "Upload failed")
                    )));
                    if let Some(path) = cleanup_after.as_deref() {
                        cleanup_import_path(path).await;
                    }
                    cancels_done.lock().unwrap().remove(&up_id);
                    return;
                }
            };
            let remote_path = format!("{}/{}", remote_dir.trim_end_matches('/'), filename);
            let id = up_id.clone();
            let _ = events.send(SessionEvent::SftpStatus(format!(
                "{} {}...",
                t("上传", "Uploading"),
                filename
            )));
            match upload_pipelined(
                &handle,
                &local,
                &remote_path,
                &filename,
                &id,
                &events,
                &cancel,
            )
            .await
            {
                Ok(true) => {
                    if let Ok(entries) = list_dir_impl(&sftp, &remote_dir).await {
                        let _ = events.send(SessionEvent::SftpEntries {
                            path: remote_dir.clone(),
                            entries,
                        });
                    }
                    let _ = events.send(SessionEvent::SftpStatus(format!(
                        "{}: {}",
                        t("上传完成", "Uploaded"),
                        filename
                    )));
                }
                Ok(false) => {
                    // Refresh the listing so the removed partial file disappears.
                    if let Ok(entries) = list_dir_impl(&sftp, &remote_dir).await {
                        let _ = events.send(SessionEvent::SftpEntries {
                            path: remote_dir.clone(),
                            entries,
                        });
                    }
                    let _ = events.send(SessionEvent::SftpStatus(format!(
                        "{}: {}",
                        t("已取消", "Cancelled"),
                        filename
                    )));
                }
                Err(e) => {
                    emit_transfer(&events, &id, &filename, true, 0, 0, 2, &e.to_string());
                    let _ = events.send(SessionEvent::SftpStatus(format!(
                        "{}: {e}",
                        t("上传失败", "Upload failed")
                    )));
                }
            }
        }
        if let Some(path) = cleanup_after.as_deref() {
            cleanup_import_path(path).await;
        }
        cancels_done.lock().unwrap().remove(&up_id);
    });
}

#[allow(unused_variables)]
pub(super) async fn handle_upload_edited(
    ctx: &mut super::ctx::WorkerCtx,
    local: PathBuf,
    remote: String,
) {
    let sftp = ctx.sftp.clone();
    let events = ctx.events.clone();
    let handle = ctx.handle.clone();
    let sftp = sftp.clone();
    let handle = handle.clone();
    let events = events.clone();
    tokio::spawn(async move {
        let filename = base_name(&remote);
        let remote_dir = parent_dir(&remote);
        let id = Uuid::new_v4().to_string();
        let no_cancel = Arc::new(AtomicBool::new(false));
        let result = upload_pipelined(
            &handle, &local, &remote, &filename, &id, &events, &no_cancel,
        )
        .await;
        match result {
            Ok(true) => {
                if let Ok(entries) = list_dir_impl(&sftp, &remote_dir).await {
                    let _ = events.send(SessionEvent::SftpEntries {
                        path: remote_dir,
                        entries,
                    });
                }
                let _ = events.send(SessionEvent::SftpStatus(format!(
                    "{}: {}",
                    t("已上传修改", "Re-uploaded changes"),
                    filename
                )));
            }
            Ok(false) => {}
            Err(e) => {
                let _ = events.send(SessionEvent::SftpStatus(format!(
                    "{}: {e}",
                    t("上传修改失败", "Failed to re-upload changes")
                )));
            }
        }
    });
}

#[allow(unused_variables)]
pub(super) async fn handle_copy_to(
    ctx: &mut super::ctx::WorkerCtx,
    remotes: Vec<String>,
    target: UnboundedSender<SftpCommand>,
    target_dir: String,
) {
    let sftp = ctx.sftp.clone();
    let events = ctx.events.clone();
    let handle = ctx.handle.clone();
    tokio::spawn(async move {
        let label = format!("{} {}", remotes.len(), t("项", "items"));
        let _ = events.send(SessionEvent::SftpStatus(format!(
            "{} {}...",
            t("复制到其他会话", "Copying to another session"),
            label
        )));
        for remote in remotes {
            match stage_remote_for_copy(&sftp, &handle, &remote, &events).await {
                Ok((local, cleanup_root)) => {
                    let _ = target.send(SftpCommand::Upload {
                        local,
                        remote_dir: target_dir.clone(),
                        cleanup_after: Some(cleanup_root),
                    });
                }
                Err(e) => {
                    let _ = events.send(SessionEvent::SftpStatus(format!(
                        "{}: {e}",
                        t("复制失败", "Copy failed")
                    )));
                }
            }
        }
    });
}
