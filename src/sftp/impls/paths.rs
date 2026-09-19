use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use russh::client;
use tokio::sync::mpsc::UnboundedSender;

use crate::ssh::SessionEvent;

use super::ssh_handler::SftpClientHandler;

/// File name component of a path.  Handles both remote (`/`) and local Windows
/// (`\`) separators, so uploading `C:\…\frp.tar.gz` yields `frp.tar.gz` rather
/// than the whole path (which previously became the remote file name).
pub(super) fn base_name(path: &str) -> String {
    let sep = |c: char| c == '/' || c == '\\';
    path.trim_end_matches(sep)
        .rsplit(sep)
        .next()
        .unwrap_or(path)
        .to_string()
}

pub(super) fn local_file_name_utf8(path: &Path) -> Result<String> {
    path.file_name()
        .and_then(|n| n.to_str())
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow!("local file name is not valid UTF-8: {}", path.display()))
}

/// Single-quote a string for safe interpolation into a remote `/bin/sh`
/// command. Remote names come from the *server's* listing and are therefore
/// untrusted — without quoting, a crafted name like `; rm -rf ~` would run.
pub(super) fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Run a one-shot command on the remote over its own exec channel and return
/// the exit status. Stdout/stderr are drained and discarded.
pub(super) async fn exec_remote(
    handle: &client::Handle<SftpClientHandler>,
    cmd: &str,
) -> Result<u32> {
    let mut ch = handle
        .channel_open_session()
        .await
        .context("open exec channel")?;
    ch.exec(true, cmd.as_bytes())
        .await
        .context("exec remote command")?;
    let mut status = 0u32;
    while let Some(msg) = ch.wait().await {
        match msg {
            russh::ChannelMsg::ExitStatus { exit_status } => status = exit_status,
            russh::ChannelMsg::Close => break,
            _ => {}
        }
    }
    Ok(status)
}

/// Parent directory of a remote path ("/a/b" → "/a", "/a" → "/").
pub(super) fn parent_dir(path: &str) -> String {
    let p = path.trim_end_matches('/');
    match p.rfind('/') {
        Some(0) | None => "/".to_string(),
        Some(i) => p[..i].to_string(),
    }
}

/// Make a remote-supplied file name safe to use as a *local* file name (for
/// both downloads and temp files): drops path separators (defence-in-depth
/// against traversal), replaces characters invalid on Windows or special to
/// shells with `_`, trims surrounding whitespace and Windows' trailing dots,
/// and neutralises reserved device names (CON, NUL, COM1…).  Normal names
/// (letters, digits, `.`, `-`, `_`, Unicode) pass through; Unix dotfiles keep
/// their leading dot.  Falls back to `file` when nothing usable remains.
pub(super) fn sanitize_filename(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '<' | '>' | '"' | '|' | '?' | '*' | '&' | '^' | '%' | '!' | '`'
            | '$' | '\'' => '_',
            c if (c as u32) < 0x20 => '_',
            c => c,
        })
        .collect();
    // Drop leading whitespace and trailing dots/spaces (Windows strips the
    // latter silently). A leading dot is preserved so `.bashrc` survives.
    let trimmed = cleaned.trim_start_matches(' ').trim_end_matches([' ', '.']);
    if trimmed.is_empty() {
        return "file".to_string();
    }
    // Windows reserved device names are reserved case-insensitively and even
    // with an extension ("CON.txt" still opens the console). A download named
    // after one could read/write a device instead of a file, so prefix `_`.
    let stem = trimmed.split('.').next().unwrap_or(trimmed);
    let reserved = matches!(
        stem.to_ascii_uppercase().as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    );
    if reserved {
        format!("_{trimmed}")
    } else {
        trimmed.to_string()
    }
}

pub(crate) fn download_target_path(remote: &str, local_dir: &str) -> PathBuf {
    Path::new(local_dir).join(sanitize_filename(&base_name(remote)))
}

pub(super) fn available_download_path(requested: &Path) -> PathBuf {
    if !requested.exists() {
        return requested.to_path_buf();
    }
    let parent = requested.parent().unwrap_or_else(|| Path::new(""));
    let stem = requested
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("download");
    let extension = requested.extension().and_then(|value| value.to_str());
    for suffix in 1usize.. {
        let name = match extension {
            Some(extension) if !extension.is_empty() => format!("{stem} ({suffix}).{extension}"),
            _ => format!("{stem} ({suffix})"),
        };
        let candidate = parent.join(name);
        if !candidate.exists() {
            return candidate;
        }
    }
    unreachable!("an unused download filename suffix must exist")
}

pub(super) async fn cleanup_import_path(path: &Path) {
    let res = match tokio::fs::metadata(path).await {
        Ok(meta) if meta.is_dir() => tokio::fs::remove_dir_all(path).await,
        Ok(_) => tokio::fs::remove_file(path).await,
        Err(_) => Ok(()),
    };
    if let Err(e) = res {
        tracing::debug!("failed to clean temporary SFTP copy {:?}: {e}", path);
    }
}

/// Emit a transfer-progress event.
#[allow(clippy::too_many_arguments)]
pub(super) fn emit_transfer(
    events: &UnboundedSender<SessionEvent>,
    id: &str,
    name: &str,
    is_upload: bool,
    transferred: u64,
    total: u64,
    state: u8,
    msg: &str,
) {
    let _ = events.send(SessionEvent::SftpTransfer {
        id: id.to_string(),
        name: name.to_string(),
        is_upload,
        transferred,
        total,
        state,
        msg: msg.to_string(),
    });
}

#[cfg(test)]
mod sanitize_tests {
    use super::{available_download_path, download_target_path, sanitize_filename};

    #[test]
    fn plain_names_pass_through() {
        assert_eq!(sanitize_filename("report.txt"), "report.txt");
        assert_eq!(sanitize_filename("my-file_v2.tar.gz"), "my-file_v2.tar.gz");
        assert_eq!(sanitize_filename("数据.csv"), "数据.csv");
        // Unix dotfiles keep their leading dot.
        assert_eq!(sanitize_filename(".bashrc"), ".bashrc");
    }

    #[test]
    fn strips_path_separators_and_traversal() {
        // base_name already strips dirs, but sanitize is defence-in-depth: the
        // result must never keep a separator that could escape the target dir.
        assert_eq!(sanitize_filename("a/b\\c"), "a_b_c");
        let traversal = sanitize_filename("../../etc/passwd");
        assert!(!traversal.contains('/') && !traversal.contains('\\'));
        let win = sanitize_filename("..\\..\\Windows\\System32");
        assert!(!win.contains('/') && !win.contains('\\'));
    }

    #[test]
    fn replaces_shell_and_windows_special_chars() {
        assert_eq!(sanitize_filename("foo&calc.exe"), "foo_calc.exe");
        assert_eq!(sanitize_filename("a|b>c<d:e?f*g"), "a_b_c_d_e_f_g");
        assert_eq!(sanitize_filename("$(whoami)"), "_(whoami)");
        assert_eq!(sanitize_filename("a`b'c"), "a_b_c");
    }

    #[test]
    fn trims_whitespace_and_trailing_dots() {
        assert_eq!(sanitize_filename("   spaced.txt  "), "spaced.txt");
        assert_eq!(sanitize_filename("name..."), "name");
        // control chars become underscores, not trimmed
        assert_eq!(sanitize_filename("a\tb"), "a_b");
    }

    #[test]
    fn neutralises_windows_reserved_device_names() {
        assert_eq!(sanitize_filename("CON"), "_CON");
        assert_eq!(sanitize_filename("nul"), "_nul");
        assert_eq!(sanitize_filename("COM1"), "_COM1");
        assert_eq!(sanitize_filename("LPT9.txt"), "_LPT9.txt"); // reserved even with ext
        assert_eq!(sanitize_filename("Aux.log"), "_Aux.log");
        // Not reserved: a name that merely starts with the same letters.
        assert_eq!(sanitize_filename("console.txt"), "console.txt");
        assert_eq!(sanitize_filename("COM10"), "COM10");
    }

    #[test]
    fn empty_or_all_bad_falls_back() {
        assert_eq!(sanitize_filename(""), "file");
        assert_eq!(sanitize_filename("   "), "file");
        assert_eq!(sanitize_filename("..."), "file");
    }

    #[test]
    fn keep_both_uses_the_first_available_numbered_name() {
        let dir =
            std::env::temp_dir().join(format!("zinterm-download-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let requested = download_target_path("/remote/report.txt", dir.to_str().unwrap());
        std::fs::write(&requested, b"old").unwrap();
        std::fs::write(dir.join("report (1).txt"), b"older").unwrap();

        assert_eq!(
            available_download_path(&requested),
            dir.join("report (2).txt")
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
