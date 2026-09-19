use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use futures::stream::{FuturesUnordered, StreamExt};
use russh::client;
use russh_sftp::client::error::Error as SftpError;
use russh_sftp::client::{RawSftpSession, SftpSession};
use russh_sftp::protocol::{FileAttributes, OpenFlags, StatusCode};
use tokio::sync::mpsc::UnboundedSender;
use uuid::Uuid;

use crate::i18n::t;
use crate::ssh::SessionEvent;

use super::super::transfer::DownloadConflict;
use super::listing::list_dir_impl;
use super::paths::{
    available_download_path, base_name, download_target_path, emit_transfer, exec_remote,
    sanitize_filename, sh_quote,
};
use super::ssh_handler::SftpClientHandler;

/// Download a remote file over a dedicated, *pipelined* raw SFTP channel.
///
/// The high-level reader issues one READ and waits for the reply before the
/// next, so throughput is capped by the round-trip time (slow on any latent
/// link). Here we keep many READ requests in flight at once, each tagged with
/// its absolute offset so out-of-order completion is fine — mirroring
/// `upload_pipelined`.
///
/// Returns `Ok(true)` when the whole file was written, or `Ok(false)` if the
/// transfer was cancelled. In both the cancel and error cases the partial
/// local file is removed so no half-downloaded junk is left behind.
pub(super) async fn download_impl(
    handle: &client::Handle<SftpClientHandler>,
    remote: &str,
    local: &str,
    name: &str,
    id: &str,
    events: &UnboundedSender<SessionEvent>,
    cancel: &Arc<AtomicBool>,
) -> Result<bool> {
    use tokio::io::{AsyncSeekExt, AsyncWriteExt};

    const CHUNK: usize = 32 * 1024;
    const MAX_INFLIGHT: usize = 32; // ~1 MB outstanding hides the RTT

    let channel = handle
        .channel_open_session()
        .await
        .context("open sftp download channel")?;
    channel
        .request_subsystem(true, "sftp")
        .await
        .context("request sftp subsystem")?;
    let raw = Arc::new(RawSftpSession::new(channel.into_stream()));
    raw.init().await.context("sftp download handshake")?;

    let total = raw
        .stat(remote)
        .await
        .ok()
        .and_then(|a| a.attrs.size)
        .unwrap_or(0);
    let fhandle = raw
        .open(remote, OpenFlags::READ, FileAttributes::default())
        .await
        .with_context(|| format!("open remote {remote}"))?
        .handle;
    let mut local_file = tokio::fs::File::create(local)
        .await
        .with_context(|| format!("create local {local}"))?;

    emit_transfer(events, id, name, false, 0, total, 0, "");

    let mut done: u64 = 0;
    let mut last = Instant::now();
    let mut err: Option<anyhow::Error> = None;
    let mut cancelled = false;

    if total > 0 {
        let mut next_off = 0u64;
        let mut inflight = FuturesUnordered::new();
        loop {
            if cancel.load(Ordering::Relaxed) {
                cancelled = true;
            }
            // Top up the pipeline with fresh READ requests.
            while !cancelled && err.is_none() && next_off < total && inflight.len() < MAX_INFLIGHT {
                let off = next_off;
                let want = ((total - off) as usize).min(CHUNK);
                next_off += want as u64;
                let raw2 = raw.clone();
                let h = fhandle.clone();
                inflight.push(async move {
                    // Fill the whole chunk, coping with short reads.
                    let mut data = Vec::with_capacity(want);
                    let mut o = off;
                    let end = off + want as u64;
                    while o < end {
                        match raw2.read(h.clone(), o, (end - o) as u32).await {
                            Ok(d) => {
                                if d.data.is_empty() {
                                    break;
                                }
                                o += d.data.len() as u64;
                                data.extend_from_slice(&d.data);
                            }
                            Err(SftpError::Status(s)) if s.status_code == StatusCode::Eof => break,
                            Err(e) => return Err(anyhow!("read remote: {e}")),
                        }
                    }
                    Ok::<(u64, Vec<u8>), anyhow::Error>((off, data))
                });
            }
            if inflight.is_empty() {
                break;
            }
            match inflight.next().await {
                Some(Ok((off, data))) => {
                    if !data.is_empty() {
                        if let Err(e) = local_file.seek(std::io::SeekFrom::Start(off)).await {
                            err = Some(anyhow!("seek local: {e}"));
                        } else if let Err(e) = local_file.write_all(&data).await {
                            err = Some(anyhow!("write local: {e}"));
                        } else {
                            done += data.len() as u64;
                        }
                    }
                    if last.elapsed() >= Duration::from_millis(150) {
                        last = Instant::now();
                        emit_transfer(events, id, name, false, done, total, 0, "");
                    }
                }
                Some(Err(e)) => err = Some(e),
                None => {}
            }
            if (cancelled || err.is_some()) && inflight.is_empty() {
                break;
            }
        }
    } else {
        // Unknown / zero size: serial drain until EOF (rare; keeps correctness).
        let mut off = 0u64;
        loop {
            if cancel.load(Ordering::Relaxed) {
                cancelled = true;
                break;
            }
            match raw.read(fhandle.clone(), off, CHUNK as u32).await {
                Ok(d) => {
                    if d.data.is_empty() {
                        break;
                    }
                    local_file
                        .write_all(&d.data)
                        .await
                        .context("write local file")?;
                    off += d.data.len() as u64;
                    done += d.data.len() as u64;
                    if last.elapsed() >= Duration::from_millis(150) {
                        last = Instant::now();
                        emit_transfer(events, id, name, false, done, done, 0, "");
                    }
                }
                Err(SftpError::Status(s)) if s.status_code == StatusCode::Eof => break,
                Err(e) => {
                    err = Some(anyhow!("read remote: {e}"));
                    break;
                }
            }
        }
    }

    let _ = raw.close(fhandle).await;

    if let Some(e) = err {
        drop(local_file);
        let _ = tokio::fs::remove_file(local).await;
        return Err(e);
    }
    if cancelled {
        drop(local_file);
        let _ = tokio::fs::remove_file(local).await;
        emit_transfer(
            events,
            id,
            name,
            false,
            done,
            total,
            4,
            t("已取消", "Cancelled"),
        );
        return Ok(false);
    }
    local_file.flush().await.context("flush local file")?;
    emit_transfer(events, id, name, false, done, total.max(done), 1, "");
    Ok(true)
}

/// Recursively download a remote directory tree under `local_parent` (#50).
///
/// Iterative (work-stack) rather than a boxed async recursion: each remote dir
/// is mirrored to a sanitized local name, then its files are downloaded with the
/// same per-file pipeline used for single downloads. Names are sanitized (#26)
/// so a hostile server can't escape the chosen folder.
pub(super) async fn download_dir(
    sftp: &SftpSession,
    handle: &client::Handle<SftpClientHandler>,
    remote_root: &str,
    local_parent: &str,
    events: &UnboundedSender<SessionEvent>,
) -> Result<()> {
    // Folder transfers aren't individually cancellable from the UI; a throwaway
    // never-set flag satisfies download_impl's signature.
    let no_cancel = Arc::new(AtomicBool::new(false));
    let root_name = sanitize_filename(&base_name(remote_root));
    let root_local = format!("{}/{}", local_parent.trim_end_matches('/'), root_name);
    // (remote_dir, local_dir) pairs still to mirror.
    let mut stack = vec![(remote_root.trim_end_matches('/').to_string(), root_local)];
    while let Some((rdir, ldir)) = stack.pop() {
        tokio::fs::create_dir_all(&ldir)
            .await
            .with_context(|| format!("create local dir {ldir}"))?;
        for entry in list_dir_impl(sftp, &rdir).await? {
            if entry.is_dir {
                let child_local = format!("{}/{}", ldir, sanitize_filename(&entry.name));
                stack.push((entry.full_path, child_local));
            } else {
                let fname = sanitize_filename(&entry.name);
                let lpath = format!("{}/{}", ldir, fname);
                let id = Uuid::new_v4().to_string();
                download_impl(
                    handle,
                    &entry.full_path,
                    &lpath,
                    &fname,
                    &id,
                    events,
                    &no_cancel,
                )
                .await?;
            }
        }
    }
    Ok(())
}

#[allow(unused_variables)]
pub(super) async fn handle_download(
    ctx: &mut super::ctx::WorkerCtx,
    remote: String,
    local_dir: String,
    conflict: DownloadConflict,
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
    // Register a cancel flag up-front under the file id, so a
    // CancelTransfer arriving mid-download can flip it (#100).
    let file_id = Uuid::new_v4().to_string();
    let cancel = Arc::new(AtomicBool::new(false));
    cancels
        .lock()
        .unwrap()
        .insert(file_id.clone(), cancel.clone());
    let cancels_done = cancels.clone();
    tokio::spawn(async move {
        // A directory target → recursively mirror the whole tree (#50).
        let is_dir = sftp
            .metadata(&remote)
            .await
            .ok()
            .map(|m| (m.permissions.unwrap_or(0) & 0o170_000) == 0o040_000)
            .unwrap_or(false);
        if is_dir {
            let dirname = base_name(&remote);
            // #100.3: an empty folder downloads nothing — just say so
            // rather than silently creating an empty local directory.
            let empty = list_dir_impl(&sftp, &remote)
                .await
                .map(|e| e.is_empty())
                .unwrap_or(false);
            if empty {
                let _ = events.send(SessionEvent::SftpStatus(format!(
                    "{}: {}",
                    t("空文件夹", "Empty folder"),
                    dirname
                )));
                return;
            }
            let _ = events.send(SessionEvent::SftpStatus(format!(
                "{} {}/...",
                t("下载文件夹", "Downloading folder"),
                dirname
            )));
            match download_dir(&sftp, &handle, &remote, &local_dir, &events).await {
                Ok(_) => {
                    let _ = events.send(SessionEvent::SftpStatus(format!(
                        "{}: {}",
                        t("下载完成", "Downloaded"),
                        dirname
                    )));
                }
                Err(e) => {
                    let _ = events.send(SessionEvent::SftpStatus(format!(
                        "{}: {e}",
                        t("下载失败", "Download failed")
                    )));
                }
            }
        } else {
            // Sanitize the server-supplied name before it touches the local
            // filesystem (#26): a malicious server could otherwise craft a
            // name with traversal, shell-special chars or a Windows reserved
            // device name to write outside the chosen dir or hit a device.
            let filename = sanitize_filename(&base_name(&remote));
            let requested = download_target_path(&remote, &local_dir);
            let local_path = match conflict {
                DownloadConflict::Replace => requested,
                DownloadConflict::KeepBoth => available_download_path(&requested),
            };
            let local_path_text = local_path.to_string_lossy().to_string();
            let id = file_id.clone();
            let _ = events.send(SessionEvent::SftpStatus(format!(
                "{} {}...",
                t("下载", "Downloading"),
                filename
            )));
            match download_impl(
                &handle,
                &remote,
                &local_path_text,
                &filename,
                &id,
                &events,
                &cancel,
            )
            .await
            {
                Ok(true) => {
                    let _ = events.send(SessionEvent::SftpStatus(format!(
                        "{}: {}",
                        t("下载完成", "Downloaded"),
                        filename
                    )));
                }
                Ok(false) => {
                    let _ = events.send(SessionEvent::SftpStatus(format!(
                        "{}: {}",
                        t("已取消", "Cancelled"),
                        filename
                    )));
                }
                Err(e) => {
                    emit_transfer(&events, &id, &filename, false, 0, 0, 2, &e.to_string());
                    let _ = events.send(SessionEvent::SftpStatus(format!(
                        "{}: {e}",
                        t("下载失败", "Download failed")
                    )));
                }
            }
        }
        cancels_done.lock().unwrap().remove(&file_id);
    });
}

#[allow(unused_variables)]
pub(super) async fn handle_download_archive(
    ctx: &mut super::ctx::WorkerCtx,
    remote_dir: String,
    names: Vec<String>,
    local_dir: String,
) {
    let sftp = ctx.sftp.clone();
    let events = ctx.events.clone();
    let handle = ctx.handle.clone();
    let cancels = ctx.cancels.clone();
    // #100: multi-select download. Instead of N concurrent transfers
    // (which raced and dropped files), tar everything into ONE archive
    // on the remote, pull that single file, then delete the temp.
    let sftp = sftp.clone();
    let handle = handle.clone();
    let events = events.clone();
    // Register a cancel flag up-front so CancelTransfer can flip it (#100).
    let id = Uuid::new_v4().to_string();
    let cancel = Arc::new(AtomicBool::new(false));
    cancels.lock().unwrap().insert(id.clone(), cancel.clone());
    let cancels_done = cancels.clone();
    tokio::spawn(async move {
        let n = names.len();
        let tmp = format!("/tmp/zinterm-{}.tar", Uuid::new_v4());
        // Name the archive after the first item's stem, per the user:
        // 11.txt → "11等文件.tar". Sanitize since names come from the server.
        let first = names.first().map(|s| s.as_str()).unwrap_or("download");
        let stem = first
            .rsplit_once('.')
            .map(|(a, _)| a)
            .filter(|a| !a.is_empty())
            .unwrap_or(first);
        let arc_name = sanitize_filename(&format!("{}{}.tar", stem, t("等文件", "-and-more")));
        let local_path = format!("{}/{}", local_dir.trim_end_matches('/'), arc_name);
        let _ = events.send(SessionEvent::SftpStatus(format!(
            "{} {} {}...",
            t("打包下载", "Archiving"),
            n,
            t("项", "items")
        )));
        // Show a "preparing" row in the transfer panel right away so a
        // big selection isn't a silent wait while tar runs (#100). The
        // download then reuses this same id, so the row turns into the
        // live progress bar once bytes start flowing.
        emit_transfer(&events, &id, &arc_name, false, 0, 0, 3, "");
        // Plain tar (no gzip): the user prefers speed over a smaller file.
        // Server-supplied names are untrusted → quote every argument.
        let mut cmd = format!("tar -cf {} -C {}", sh_quote(&tmp), sh_quote(&remote_dir));
        for nm in &names {
            cmd.push(' ');
            cmd.push_str(&sh_quote(nm));
        }
        let _ = &sftp; // listing session kept alive; transfer uses `handle`
        let res: Result<bool> = async {
            let st = exec_remote(&handle, &cmd).await.context("tar on remote")?;
            if st != 0 {
                return Err(anyhow!(t("远端 tar 打包失败", "remote tar failed")));
            }
            download_impl(&handle, &tmp, &local_path, &arc_name, &id, &events, &cancel).await
        }
        .await;
        // Best-effort cleanup of the remote temp tar — success, failure
        // or cancel all reach here, so no junk is left on the server (#100).
        let _ = exec_remote(&handle, &format!("rm -f {}", sh_quote(&tmp))).await;
        match res {
            Ok(true) => {
                let _ = events.send(SessionEvent::SftpStatus(format!(
                    "{}: {}",
                    t("下载完成", "Downloaded"),
                    arc_name
                )));
            }
            Ok(false) => {
                let _ = events.send(SessionEvent::SftpStatus(format!(
                    "{}: {}",
                    t("已取消", "Cancelled"),
                    arc_name
                )));
            }
            Err(e) => {
                emit_transfer(&events, &id, &arc_name, false, 0, 0, 2, &e.to_string());
                let _ = events.send(SessionEvent::SftpStatus(format!(
                    "{}: {e}",
                    t("下载失败", "Download failed")
                )));
            }
        }
        cancels_done.lock().unwrap().remove(&id);
    });
}
