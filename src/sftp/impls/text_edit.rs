use anyhow::{Context, Result};
use russh_sftp::client::SftpSession;

use crate::i18n::t;
use crate::ssh::SessionEvent;

use super::paths::base_name;

pub(super) const MAX_BUILTIN_EDITOR_BYTES: usize = 512 * 1024;
pub(super) const MAX_BUILTIN_EDITOR_LINES: usize = 20_000;
pub(super) const MAX_BUILTIN_EDITOR_LINE_BYTES: usize = 64 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub(super) enum EditorTextRejection {
    TooLarge,
    TooManyLines,
    LineTooLong,
    Binary,
    InvalidUtf8,
}

pub(super) fn validate_editor_text(
    bytes: Vec<u8>,
) -> std::result::Result<String, EditorTextRejection> {
    if bytes.len() > MAX_BUILTIN_EDITOR_BYTES {
        return Err(EditorTextRejection::TooLarge);
    }
    if bytes.iter().any(|&byte| {
        (byte < 0x20 && byte != b'\t' && byte != b'\n' && byte != b'\r') || byte == 0x7f
    }) {
        return Err(EditorTextRejection::Binary);
    }

    let text = String::from_utf8(bytes).map_err(|_| EditorTextRejection::InvalidUtf8)?;
    let mut line_count = 1usize;
    let mut line_bytes = 0usize;
    for byte in text.bytes() {
        if byte == b'\n' {
            line_count += 1;
            line_bytes = 0;
            if line_count > MAX_BUILTIN_EDITOR_LINES {
                return Err(EditorTextRejection::TooManyLines);
            }
        } else {
            line_bytes += 1;
            if line_bytes > MAX_BUILTIN_EDITOR_LINE_BYTES {
                return Err(EditorTextRejection::LineTooLong);
            }
        }
    }
    Ok(text)
}

pub(super) fn editor_rejection_message(rejection: EditorTextRejection) -> String {
    match rejection {
        EditorTextRejection::TooLarge => t(
            "文件过大,无法在内置编辑器中打开(上限 512 KB),请使用外部打开/编辑或下载",
            "Too large for the built-in editor (512 KB limit); open/edit externally or download it instead",
        ),
        EditorTextRejection::TooManyLines => t(
            "文件行数过多,无法在内置编辑器中安全打开,请使用外部打开/编辑或下载",
            "Too many lines for the built-in editor; open/edit externally or download it instead",
        ),
        EditorTextRejection::LineTooLong => t(
            "文件包含过长的单行,无法在内置编辑器中安全打开,请使用外部打开/编辑或下载",
            "Contains a line too long for the built-in editor; open/edit externally or download it instead",
        ),
        EditorTextRejection::Binary => t(
            "包含控制字符(疑似二进制),无法以文本打开,请下载查看",
            "Contains control characters (likely binary); download it instead",
        ),
        EditorTextRejection::InvalidUtf8 => {
            t("非 UTF-8 文本,无法打开", "Not UTF-8 text; cannot open")
        }
    }
    .into()
}

/// Read a remote file as UTF-8 text for the built-in editor, rejecting content
/// that would make Slint eagerly lay out an unsafe amount of text (#70, #331).
/// Returns the text on success or a human-readable error message on failure.
pub(super) async fn read_text_guarded(
    sftp: &SftpSession,
    remote: &str,
) -> std::result::Result<String, String> {
    use tokio::io::AsyncReadExt;
    let size = sftp
        .metadata(remote)
        .await
        .ok()
        .and_then(|m| m.size)
        .unwrap_or(0);
    if size > MAX_BUILTIN_EDITOR_BYTES as u64 {
        return Err(editor_rejection_message(EditorTextRejection::TooLarge));
    }
    let f = sftp
        .open(remote)
        .await
        .map_err(|e| format!("{}: {e}", t("打开失败", "Open failed")))?;
    // Metadata may be missing or stale. Read at most one byte past the limit so
    // an untrusted remote file can never make this path allocate without bound.
    let mut bytes = Vec::with_capacity((size as usize).min(MAX_BUILTIN_EDITOR_BYTES));
    f.take(MAX_BUILTIN_EDITOR_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|e| format!("{}: {e}", t("读取失败", "Read failed")))?;
    validate_editor_text(bytes).map_err(editor_rejection_message)
}

/// Overwrite a remote file with the given text (CREATE | WRITE | TRUNCATE).
pub(super) async fn write_text_file(sftp: &SftpSession, remote: &str, content: &str) -> Result<()> {
    use tokio::io::AsyncWriteExt;
    let mut f = sftp
        .create(remote)
        .await
        .with_context(|| format!("create remote {remote}"))?;
    f.write_all(content.as_bytes())
        .await
        .context("write remote file")?;
    f.flush().await.context("flush remote file")?;
    let _ = f.shutdown().await;
    Ok(())
}

#[allow(unused_variables)]
pub(super) async fn handle_read_text(ctx: &mut super::ctx::WorkerCtx, remote: String, edit: bool) {
    let sftp = ctx.sftp.clone();
    let events = ctx.events.clone();
    let name = base_name(&remote);
    let _ = events.send(SessionEvent::SftpStatus(format!(
        "{} {}...",
        t("打开", "Opening"),
        name
    )));
    let (content, error) = match read_text_guarded(&sftp, &remote).await {
        Ok(text) => (text, String::new()),
        Err(msg) => (String::new(), msg),
    };
    let _ = events.send(SessionEvent::SftpFileText {
        path: remote,
        name,
        content,
        edit,
        error,
    });
}

#[allow(unused_variables)]
pub(super) async fn handle_write_text(
    ctx: &mut super::ctx::WorkerCtx,
    remote: String,
    content: String,
) {
    let sftp = ctx.sftp.clone();
    let events = ctx.events.clone();
    let name = base_name(&remote);
    match write_text_file(&sftp, &remote, &content).await {
        Ok(_) => {
            let _ = events.send(SessionEvent::SftpStatus(format!(
                "{}: {}",
                t("已保存", "Saved"),
                name
            )));
        }
        Err(e) => {
            let _ = events.send(SessionEvent::SftpStatus(format!(
                "{}: {e:#}",
                t("保存失败", "Save failed")
            )));
        }
    }
}

#[cfg(test)]
mod editor_tests {
    use super::{
        validate_editor_text, EditorTextRejection, MAX_BUILTIN_EDITOR_BYTES,
        MAX_BUILTIN_EDITOR_LINES, MAX_BUILTIN_EDITOR_LINE_BYTES,
    };

    #[test]
    fn editor_rejects_content_over_the_safe_byte_limit() {
        let bytes = vec![b'a'; MAX_BUILTIN_EDITOR_BYTES + 1];
        assert_eq!(
            validate_editor_text(bytes),
            Err(EditorTextRejection::TooLarge)
        );
    }

    #[test]
    fn editor_rejects_pathological_text_layouts() {
        let lines = vec![b'\n'; MAX_BUILTIN_EDITOR_LINES];
        assert_eq!(
            validate_editor_text(lines),
            Err(EditorTextRejection::TooManyLines)
        );
        let line = vec![b'a'; MAX_BUILTIN_EDITOR_LINE_BYTES + 1];
        assert_eq!(
            validate_editor_text(line),
            Err(EditorTextRejection::LineTooLong)
        );
    }

    #[test]
    fn editor_accepts_regular_utf8_text_within_limits() {
        assert_eq!(
            validate_editor_text("第一行\nsecond line\n".as_bytes().to_vec()),
            Ok("第一行\nsecond line\n".into())
        );
    }
}
