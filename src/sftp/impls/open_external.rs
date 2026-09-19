use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use tokio::sync::mpsc::UnboundedSender;
use uuid::Uuid;

use crate::i18n::t;
use crate::ssh::SessionEvent;

use super::super::transfer::SftpCommand;
use super::paths::{base_name, sanitize_filename};
use super::transfer_download::download_impl;

/// Open a local file with the OS default application.
///
/// Security: we must NOT route the path through a shell.  The previous
/// `cmd /C start "" <path>` let cmd.exe re-parse the path, so a remote file name
/// containing shell metacharacters (`&` `|` `>` `<` `^` …) — e.g. `foo&calc.exe`
/// — could inject and run arbitrary commands when the user opened it.  We call
/// `ShellExecuteW` directly instead: it treats the path as one opaque string, so
/// no shell parsing happens.  (`xdg-open` on Unix already takes a single argv
/// argument and never invokes a shell.)
#[cfg(windows)]
pub(super) fn open_with_os(path: &str) {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "shell32")]
    extern "system" {
        fn ShellExecuteW(
            hwnd: isize,
            lp_operation: *const u16,
            lp_file: *const u16,
            lp_parameters: *const u16,
            lp_directory: *const u16,
            n_show_cmd: i32,
        ) -> isize;
    }
    let to_wide = |s: &str| -> Vec<u16> {
        OsStr::new(s)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    };
    let op = to_wide("open");
    let file = to_wide(path);
    unsafe {
        ShellExecuteW(
            0,
            op.as_ptr(),
            file.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            1, // SW_SHOWNORMAL
        );
    }
}

#[cfg(not(windows))]
pub(super) fn open_with_os(path: &str) {
    let _ = std::process::Command::new("xdg-open").arg(path).spawn();
}

pub(super) fn external_edit_local_name(host: &str, filename: &str, unique: &str) -> String {
    format!(
        "{}_{}_{}",
        sanitize_filename(host),
        sanitize_filename(unique),
        sanitize_filename(filename)
    )
}

/// Watch a downloaded temp file and re-upload it to the remote whenever it
/// changes on disk (the "edit" flow).  Re-upload is routed back through the
/// worker's own command channel.  Stops when the channel closes or after a
/// generous idle window.
pub(super) fn spawn_edit_watcher(
    self_tx: UnboundedSender<SftpCommand>,
    local: String,
    remote: String,
) {
    tokio::spawn(async move {
        let mtime = |p: &str| std::fs::metadata(p).ok().and_then(|m| m.modified().ok());
        let mut last = mtime(&local);
        // ~40 min of 2s polls; also exits early once the worker is gone.
        for _ in 0..1200 {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            if self_tx.is_closed() {
                break;
            }
            let cur = mtime(&local);
            if cur.is_some() && cur != last {
                last = cur;
                let _ = self_tx.send(SftpCommand::UploadEdited {
                    local: PathBuf::from(&local),
                    remote: remote.clone(),
                });
            }
        }
    });
}

#[allow(unused_variables)]
pub(super) async fn handle_open_temp(ctx: &mut super::ctx::WorkerCtx, remote: String, edit: bool) {
    let events = ctx.events.clone();
    let handle = ctx.handle.clone();
    let self_tx = ctx.self_tx.clone();
    let external_edit_dir = ctx.external_edit_dir.clone();
    let external_edit_prefix = ctx.external_edit_prefix.clone();
    // Sanitize the remote-controlled name before it becomes a local
    // file path that we later hand to the OS "open" call.
    let filename = sanitize_filename(&base_name(&remote));
    let tmp_dir = external_edit_dir.clone();
    let _ = tokio::fs::create_dir_all(&tmp_dir).await;
    let local = tmp_dir.join(external_edit_local_name(
        &external_edit_prefix,
        &filename,
        &Uuid::new_v4().to_string(),
    ));
    let local_str = local.to_string_lossy().to_string();
    let _ = events.send(SessionEvent::SftpStatus(format!(
        "{} {}...",
        t("打开", "Opening"),
        filename
    )));
    let xid = Uuid::new_v4().to_string();
    let no_cancel = Arc::new(AtomicBool::new(false));
    match download_impl(
        &handle, &remote, &local_str, &filename, &xid, &events, &no_cancel,
    )
    .await
    {
        Ok(_) => {
            open_with_os(&local_str);
            let _ = events.send(SessionEvent::SftpStatus(format!(
                "{}: {}",
                if edit {
                    t("已打开编辑", "Opened for editing")
                } else {
                    t("已打开", "Opened")
                },
                filename
            )));
            if edit {
                spawn_edit_watcher(self_tx.clone(), local_str, remote.clone());
            }
        }
        Err(e) => {
            let _ = events.send(SessionEvent::SftpStatus(format!(
                "{}: {e}",
                t("打开失败", "Open failed")
            )));
        }
    }
}

#[cfg(test)]
mod external_edit_tests {
    use super::external_edit_local_name;

    #[test]
    fn external_edits_include_the_host_in_the_local_name() {
        assert_eq!(
            external_edit_local_name("192.168.1.10", "nginx.conf", "edit-1"),
            "192.168.1.10_edit-1_nginx.conf"
        );
        assert_ne!(
            external_edit_local_name("server-a", "nginx.conf", "edit-1"),
            external_edit_local_name("server-b", "nginx.conf", "edit-1")
        );
        assert_ne!(
            external_edit_local_name("server-a", "nginx.conf", "edit-1"),
            external_edit_local_name("server-a", "nginx.conf", "edit-2")
        );
    }
}
