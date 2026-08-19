//! SSH session manager.
//!
//! Each open terminal tab maps to exactly one `SshSession`. The session runs
//! on the shared Tokio runtime; commands come in via an MPSC channel and
//! output lines are pushed back via an `UnboundedSender<SessionEvent>`.

use std::borrow::Cow;
use std::path::Path;
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use russh::client::{self, Handle, Handler};
use russh::keys::{
    decode_secret_key, load_secret_key, Algorithm, HashAlg, PrivateKey, PrivateKeyWithHashAlg,
    PublicKey,
};
use russh::{ChannelMsg, Disconnect};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

use crate::config::{AuthMethod, Session};
use crate::i18n::t;

use super::structs::*;

// ---------------------------------------------------------------------------
// SFTP-related shared types
// ---------------------------------------------------------------------------

pub(crate) fn load_session_private_key(session: &Session, pass: &str) -> Result<PrivateKey> {
    let pass = if pass.is_empty() { None } else { Some(pass) };
    let inline = session.private_key_inline.as_str().trim();
    if !inline.is_empty() {
        if crate::ssh::ppk::is_ppk(inline.as_bytes()) {
            return crate::ssh::ppk::decode_ppk(inline.as_bytes(), pass.unwrap_or_default())
                .context("failed to parse pasted PuTTY private key");
        }
        return decode_secret_key(inline, pass).context("failed to parse pasted private key");
    }

    let raw = session.private_key_path.trim();
    if raw.is_empty() {
        return Err(anyhow!(t(
            "私钥路径或私钥内容为空",
            "private key path or private key content is empty"
        )));
    }

    let normalised = raw.replace('\\', "/");
    let key_path = normalised
        .strip_suffix(".pub")
        .map(str::to_string)
        .unwrap_or(normalised);
    if Path::new(&key_path)
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("ppk"))
    {
        let raw = std::fs::read(&key_path)
            .with_context(|| format!("failed to read PuTTY key {key_path}"))?;
        return crate::ssh::ppk::decode_ppk(&raw, pass.unwrap_or_default())
            .with_context(|| format!("failed to load PuTTY key {key_path}"));
    }
    load_secret_key(Path::new(&key_path), pass)
        .with_context(|| format!("failed to load key {key_path}"))
}

/// Format a byte count as a human-readable string.
pub fn format_size(bytes: u64) -> String {
    if bytes < 1_024 {
        format!("{} B", bytes)
    } else if bytes < 1_024 * 1_024 {
        format!("{:.1} KB", bytes as f64 / 1_024.0)
    } else if bytes < 1_024 * 1_024 * 1_024 {
        format!("{:.1} MB", bytes as f64 / (1_024.0 * 1_024.0))
    } else {
        format!("{:.2} GB", bytes as f64 / (1_024.0 * 1_024.0 * 1_024.0))
    }
}

/// Format a Unix timestamp as `YYYY-MM-DD HH:MM`.
pub fn format_mtime(ts: u32) -> String {
    // SFTP mtime is a Unix timestamp (UTC seconds). Render it in the machine's
    // *local* timezone so the displayed time matches the user's wall clock
    // (e.g. UTC+8) instead of showing UTC — which read 8 h early (#168).
    use chrono::{Local, TimeZone};
    let dt = Local
        .timestamp_opt(ts as i64, 0)
        .single()
        .unwrap_or_else(Local::now);
    dt.format("%Y-%m-%d %H:%M").to_string()
}

/// The canonical ZMODEM abort sequence: eight CAN (0x18) then eight BS (0x08).
/// Sending this makes the remote `sz`/`rz` give up so the session recovers (#76).
const ZMODEM_CANCEL: [u8; 16] = [
    0x18, 0x18, 0x18, 0x18, 0x18, 0x18, 0x18, 0x18, 0x08, 0x08, 0x08, 0x08, 0x08, 0x08, 0x08, 0x08,
];

const PROMPT_SETUP_PREFIX: &str = "test -z \"$FISH_VERSION\"";
const PROMPT_SETUP_SUFFIX: &str = "__ms7'";
#[cfg(test)]
const PROMPT_SETUP_HISTORY_MARKER: &str = "__MEATSHELL_INTERNAL_SETUP_1";
const PROMPT_SETUP_DONE: &str = "\u{1b}]699;ready\u{07}";
const PROMPT_BODY: &str = "test -z \"$FISH_VERSION\" && eval '__msc(){ __c=\"$(fc -ln -1 2>/dev/null)\"; [ -n \"$__c\" ] && [ \"$__c\" != \"$__cl\" ] && { __cl=\"$__c\"; printf \"\\033]697;%s\\007\" \"$__c\"; }; }; __ms7(){ printf \"\\033]7;file://%s%s\\007\" \"$HOSTNAME\" \"$PWD\"; __msc; }; if [ -n \"$ZSH_VERSION\" ]; then autoload -Uz add-zsh-hook 2>/dev/null; add-zsh-hook precmd __ms7; else PROMPT_COMMAND=\"__ms7${PROMPT_COMMAND:+;$PROMPT_COMMAND}\"; fi; : __MEATSHELL_INTERNAL_SETUP_1; if [ -n \"$BASH_VERSION\" ]; then __md=\"$(history 2>/dev/null | { __md=\"\"; while read -r __mn __mr; do case \"$__mr\" in *\"__ms7()\"*\"PROMPT_COMMAND=\"*) __mn=\"${__mn%\\*}\"; __md=\"$__mn $__md\";; esac; done; printf \"%s\" \"$__md\"; })\"; for __mn in $__md; do history -d \"$__mn\" 2>/dev/null; done; unset __md __mn __mr; fi; __cl=\"$(fc -ln -1 2>/dev/null)\"; printf \"\\033]699;ready\\007\"; __ms7'";
const PROMPT_SHELL_PROBE: &[u8] = b"if [ -n \"$BASH_VERSION\" ]; then printf '__MEATSHELL_SHELL__:bash\\n'; elif [ -n \"$ZSH_VERSION\" ]; then printf '__MEATSHELL_SHELL__:zsh\\n'; else printf '__MEATSHELL_SHELL__:other\\n'; fi";

fn prompt_setup_supported(probe_output: &str) -> Option<bool> {
    if probe_output.contains("__MEATSHELL_SHELL__:bash")
        || probe_output.contains("__MEATSHELL_SHELL__:zsh")
    {
        Some(true)
    } else if probe_output.contains("__MEATSHELL_SHELL__:other") {
        Some(false)
    } else {
        None
    }
}

/// Probe the login shell through a separate exec channel so unsupported shells
/// never see the long interactive prompt-integration command. In particular,
/// BusyBox ash (used by OpenWrt) ignores `PROMPT_COMMAND`; injecting into its
/// line editor only risks a visible partial command or continuation prompt.
async fn remote_supports_prompt_setup(handle: &Handle<ClientHandler>) -> bool {
    let probe = async {
        let mut channel = handle.channel_open_session().await.ok()?;
        channel.exec(true, PROMPT_SHELL_PROBE).await.ok()?;
        let _ = channel.eof().await;

        let mut output = String::new();
        while let Some(message) = channel.wait().await {
            match message {
                ChannelMsg::Data { data } | ChannelMsg::ExtendedData { data, .. } => {
                    output.push_str(&String::from_utf8_lossy(&data));
                    if let Some(supported) = prompt_setup_supported(&output) {
                        return Some(supported);
                    }
                    if output.len() > 256 {
                        return Some(false);
                    }
                }
                ChannelMsg::Close => break,
                _ => {}
            }
        }
        Some(false)
    };

    tokio::time::timeout(std::time::Duration::from_millis(1000), probe)
        .await
        .ok()
        .flatten()
        .unwrap_or(false)
}

/// Detect the start of a ZMODEM transfer (sz/rz) in a raw channel chunk.
///
/// Every ZMODEM frame begins with ZDLE (0x18) followed by a type byte; the
/// `sz` handshake leads with a ZRQINIT hex header (`**\x18B00...`). Matching
/// ZDLE followed by `B` (hex frame) or `C` (binary frame) reliably catches the
/// handshake without false-positiving on a lone 0x18 (Ctrl-X) in normal output.
fn contains_zmodem_init(data: &[u8]) -> bool {
    data.windows(2)
        .any(|w| w[0] == 0x18 && (w[1] == b'B' || w[1] == b'C'))
}

fn line_start_before(text: &str, pos: usize) -> usize {
    text[..pos].rfind(['\r', '\n']).map(|i| i + 1).unwrap_or(0)
}

fn include_following_line_break(text: &str, mut pos: usize) -> usize {
    let bytes = text.as_bytes();
    if pos < bytes.len() && bytes[pos] == b'\r' {
        pos += 1;
        if pos < bytes.len() && bytes[pos] == b'\n' {
            pos += 1;
        }
    } else if pos < bytes.len() && bytes[pos] == b'\n' {
        pos += 1;
        if pos < bytes.len() && bytes[pos] == b'\r' {
            pos += 1;
        }
    }
    pos
}

#[cfg(test)]
fn prompt_setup_echo_end(text: &str, prefix_pos: usize) -> usize {
    if let Some(rel) = text[prefix_pos..].find(PROMPT_SETUP_SUFFIX) {
        return include_following_line_break(text, prefix_pos + rel + PROMPT_SETUP_SUFFIX.len());
    }
    let line_end = text[prefix_pos..]
        .find(['\r', '\n'])
        .map(|i| prefix_pos + i)
        .unwrap_or(text.len());
    include_following_line_break(text, line_end)
}

fn strip_prompt_setup_echo(text: &mut String, prefix_pos: usize, end_pos: usize) {
    let start = line_start_before(text, prefix_pos);
    let end = include_following_line_break(text, end_pos.min(text.len()));
    // The remote PTY has already echoed the hidden setup command and advanced
    // its cursor through that line. Removing the bytes outright leaves our
    // local vt100 parser at the old prompt column, so readline's later relative
    // backspaces repaint history commands beside one another (#289). Reset and
    // clear the current local row before feeding the final prompt that follows.
    text.replace_range(start..end, "\r\x1b[2K");
}

/// Remove a late-echoed prompt setup command when it arrives after the initial
/// suppression window. Some shells echo a long injected command only after the
/// first prompt has already been delivered, so the normal buffered path cannot
/// catch it (#266).
fn strip_late_prompt_setup_echo(text: &mut String) -> bool {
    let Some(prefix_pos) = text.find(PROMPT_SETUP_PREFIX) else {
        return false;
    };
    let Some(rel_end) = text[prefix_pos..].find(PROMPT_SETUP_SUFFIX) else {
        return false;
    };
    let end = prefix_pos + rel_end + PROMPT_SETUP_SUFFIX.len();
    strip_prompt_setup_echo(text, prefix_pos, end);
    true
}

fn strip_pending_prompt_setup_echo(text: &mut String, pending: &mut bool) -> bool {
    if !*pending || !strip_late_prompt_setup_echo(text) {
        return false;
    }
    *pending = false;
    true
}

/// Consume all buffered setup echo through the private completion marker.
/// The marker is emitted by the executed command, unlike its printable escaped
/// representation in the echoed input, so it remains reliable across zsh/ZLE
/// redraws, wrapping, and arbitrary chunk boundaries (#344).
fn take_after_prompt_setup_done(text: &mut String) -> Option<String> {
    let marker = text.find(PROMPT_SETUP_DONE)?;
    let tail = text.split_off(marker + PROMPT_SETUP_DONE.len());
    text.clear();
    Some(tail)
}

fn bound_prompt_setup_echo(text: &mut String) {
    const KEEP_CHARS: usize = 64;
    const MAX_BUFFER: usize = 64 * 1024;
    if text.len() <= MAX_BUFFER {
        return;
    }
    // Everything before the completion marker is private setup echo. Retain a
    // short suffix only so a marker split across channel chunks still matches.
    let mut tail: String = text.chars().rev().take(KEEP_CHARS).collect();
    tail = tail.chars().rev().collect();
    *text = tail;
}

/// Extract the remote path from an OSC 7 sequence embedded in `text`.
///
/// Format: `ESC ] 7 ; file://hostname/path BEL`
/// Returns the decoded absolute path component (without hostname).
pub fn extract_osc7_path(text: &str) -> Option<String> {
    extract_osc7_end(text).map(|(path, _)| path)
}

/// Like [`extract_osc7_path`] but also returns the byte index just past the OSC
/// sequence's terminator, so the caller can cut everything up to and including
/// it — used to discard the echoed setup line (which may wrap) at connect (#98).
fn extract_osc7_end(text: &str) -> Option<(String, usize)> {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] != 0x1b || bytes[i + 1] != b']' {
            i += 1;
            continue;
        }
        let osc_start = i + 2;
        i += 2;
        // Scan for BEL (0x07) or ST (ESC \)
        let mut end = i;
        let mut term_len = 0;
        while end < bytes.len() {
            if bytes[end] == 0x07 {
                term_len = 1;
                break;
            } else if bytes[end] == 0x1b && end + 1 < bytes.len() && bytes[end + 1] == b'\\' {
                term_len = 2;
                break;
            }
            end += 1;
        }
        if end >= bytes.len() {
            break;
        }
        if let Ok(content) = std::str::from_utf8(&bytes[osc_start..end]) {
            if let Some(rest) = content.strip_prefix("7;file://") {
                // rest = "hostname/path" or "/path" (empty hostname)
                let path = if rest.starts_with('/') {
                    rest.to_string()
                } else if let Some(slash) = rest.find('/') {
                    rest[slash..].to_string()
                } else {
                    "/".to_string()
                };
                return Some((url_decode(&path), end + term_len));
            }
        }
        i = end + term_len.max(1);
    }
    None
}

/// Find a meatshell command-capture sequence (`ESC ] 697 ; <command> BEL|ST`)
/// emitted by the shell hook (#113). Returns the command text and the byte
/// range of the whole escape sequence, so the caller can strip it before the
/// text is rendered. An incomplete sequence (terminator not yet received)
/// yields `None` — vt100 buffers it and the next chunk completes it.
pub fn extract_osc_command(text: &str) -> Option<(String, std::ops::Range<usize>)> {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] != 0x1b || bytes[i + 1] != b']' {
            i += 1;
            continue;
        }
        let seq_start = i;
        let osc_start = i + 2;
        i += 2;
        // Scan for BEL (0x07) or ST (ESC \).
        let mut end = i;
        let mut term_len = 0;
        while end < bytes.len() {
            if bytes[end] == 0x07 {
                term_len = 1;
                break;
            } else if bytes[end] == 0x1b && end + 1 < bytes.len() && bytes[end + 1] == b'\\' {
                term_len = 2;
                break;
            }
            end += 1;
        }
        if end >= bytes.len() {
            break; // incomplete — leave it for the next chunk
        }
        if let Ok(content) = std::str::from_utf8(&bytes[osc_start..end]) {
            if let Some(cmd) = content.strip_prefix("697;") {
                return Some((cmd.to_string(), seq_start..end + term_len));
            }
        }
        i = end + term_len;
    }
    None
}

/// Percent-decode a URL path segment (e.g. `%20` → space).
fn url_decode(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '%' {
            let h1 = chars.next();
            let h2 = chars.next();
            match (h1, h2) {
                (Some(a), Some(b)) => {
                    let hex = format!("{a}{b}");
                    if let Ok(byte) = u8::from_str_radix(&hex, 16) {
                        result.push(byte as char);
                    } else {
                        result.push('%');
                        result.push(a);
                        result.push(b);
                    }
                }
                (Some(a), None) => {
                    result.push('%');
                    result.push(a);
                }
                _ => result.push('%'),
            }
        } else {
            result.push(c);
        }
    }
    result
}

/// Entry point: spawn a session on the shared tokio runtime.
///
/// `initial_cols` / `initial_rows` are the PTY dimensions to request when
/// opening the channel. Slint fires a `terminal-resize` callback very shortly
/// after the tab becomes active; passing the best-known size here avoids the
/// remote shell starting at a stale 80×24 and sending an extra SIGWINCH.
///
/// Returns a [`SessionHandle`] for the UI + an [`UnboundedReceiver`] the UI
/// should drain on the Slint event loop.
pub fn spawn_session(
    runtime: &tokio::runtime::Handle,
    tab_id: String,
    session: Session,
    initial_cols: u32,
    initial_rows: u32,
) -> (SessionHandle, UnboundedReceiver<SessionEvent>) {
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<SessionCommand>();
    let (evt_tx, evt_rx) = mpsc::unbounded_channel::<SessionEvent>();

    let evt_tx_for_task = evt_tx.clone();
    let join = runtime.spawn(async move {
        if let Err(err) = run_session(
            session,
            cmd_rx,
            evt_tx_for_task.clone(),
            initial_cols,
            initial_rows,
        )
        .await
        {
            tracing::warn!("ssh session ended with error: {err:#}");
            let _ = evt_tx_for_task.send(SessionEvent::Closed(format!("{err:#}")));
        }
    });

    (
        SessionHandle {
            tab_id,
            commands: cmd_tx,
            join,
        },
        evt_rx,
    )
}

/// Open an SSH transport to the session's host and return the russh handle,
/// ready for authentication. Factored out so the keyboard-interactive fallback
/// can reconnect on a *fresh* handle — russh hangs if a second auth method is
/// attempted on a handle whose first attempt already failed (#86).
async fn connect_ssh(
    session: &Session,
    config: Arc<client::Config>,
    events: &UnboundedSender<SessionEvent>,
) -> Result<Handle<ClientHandler>> {
    let addr = format!("{}:{}", session.host, session.port);
    connect_with_config(&addr, config, client_handler(session, events)).await
}

fn client_handler(session: &Session, events: &UnboundedSender<SessionEvent>) -> ClientHandler {
    ClientHandler {
        host: session.host.clone(),
        port: session.port,
        events: events.clone(),
    }
}

async fn connect_ssh_handshake(
    session: &Session,
    events: &UnboundedSender<SessionEvent>,
) -> Result<(Handle<ClientHandler>, Arc<client::Config>)> {
    let addr = format!("{}:{}", session.host, session.port);
    connect_transport(&addr, || client_handler(session, events)).await
}

/// Connect with the modern algorithm set, then retry once with a compact
/// RFC-only profile. Old H3C/Huawei VRP SSH stacks (S3100 / VRP-3.3) drop the
/// TCP session when the client's KEXINIT lists `@openssh.com` names or is
/// simply too long — russh surfaces that as `Disconnected`.
pub(crate) async fn connect_transport<H, F>(
    addr: &str,
    make_handler: F,
) -> Result<(Handle<H>, Arc<client::Config>)>
where
    H: Handler<Error = russh::Error> + Send + 'static,
    F: Fn() -> H,
{
    let modern = ssh_client_config();
    match client::connect(modern.clone(), addr, make_handler()).await {
        Ok(handle) => Ok((handle, modern)),
        Err(err) if should_retry_legacy(&err) => {
            tracing::info!(
                "ssh handshake failed ({err}); retrying {addr} with compact legacy algorithms"
            );
            let legacy = ssh_legacy_client_config();
            match client::connect(legacy.clone(), addr, make_handler()).await {
                Ok(handle) => Ok((handle, legacy)),
                Err(err2) => Err(anyhow!(err2))
                    .with_context(|| format!("connect {addr} failed (legacy retry after: {err})")),
            }
        }
        Err(err) => Err(anyhow!(err)).with_context(|| format!("connect {addr} failed")),
    }
}

async fn connect_with_config<H: Handler<Error = russh::Error> + Send + 'static>(
    addr: &str,
    config: Arc<client::Config>,
    handler: H,
) -> Result<Handle<H>> {
    client::connect(config, addr, handler)
        .await
        .map_err(anyhow::Error::from)
        .with_context(|| format!("connect {addr} failed"))
}

fn should_retry_legacy(err: &russh::Error) -> bool {
    match err {
        russh::Error::Disconnect
        | russh::Error::KexInit
        | russh::Error::Kex
        | russh::Error::UnknownAlgo
        | russh::Error::NoCommonAlgo { .. }
        | russh::Error::Version
        | russh::Error::HUP => true,
        russh::Error::IO(io) => matches!(
            io.kind(),
            std::io::ErrorKind::ConnectionReset
                | std::io::ErrorKind::ConnectionAborted
                | std::io::ErrorKind::UnexpectedEof
        ),
        _ => false,
    }
}

/// Outcome of authenticating an SSH session, so callers can distinguish a user
/// cancel from a credential rejection and word the status line accordingly.
pub(crate) enum AuthResult {
    Success,
    Cancelled,
    Failed,
}

/// Authenticate an already-connected SSH handle using the session's method,
/// prompting for missing credentials and supporting explicit / fallback
/// `keyboard-interactive` auth (#86, #249). Shared by the shell and SFTP paths.
/// On the keyboard-interactive fallback it reconnects, updating `handle` in
/// place so the caller keeps the live connection.
pub(crate) async fn authenticate_session(
    handle: &mut Handle<ClientHandler>,
    session: &Session,
    config: Arc<client::Config>,
    events: &UnboundedSender<SessionEvent>,
) -> Result<AuthResult> {
    let (user, password) = match resolve_credentials(session, events).await {
        Some(c) => c,
        None => return Ok(AuthResult::Cancelled),
    };

    let authed = match session.auth {
        AuthMethod::Password => {
            let mut ok = handle
                .authenticate_password(&user, password.as_str())
                .await
                .context("password auth failed")?
                .success();
            if !ok {
                // russh can't switch auth methods on a handle whose first attempt
                // already failed (it hangs), so reconnect on a fresh handle before
                // trying keyboard-interactive (#86).
                let _ = handle.disconnect(Disconnect::ByApplication, "", "").await;
                *handle = Box::pin(connect_ssh(session, config.clone(), events)).await?;
                ok = keyboard_interactive_auth(
                    handle,
                    &user,
                    password.as_str(),
                    &session.id,
                    &session.host,
                    events,
                )
                .await
                .context("keyboard-interactive auth failed")?;
            }
            ok
        }
        AuthMethod::KeyboardInteractive => keyboard_interactive_auth(
            handle,
            &user,
            password.as_str(),
            &session.id,
            &session.host,
            events,
        )
        .await
        .context("keyboard-interactive auth failed")?,
        AuthMethod::Key => {
            // An encrypted private key needs its passphrase; we reuse the
            // session's password field for it (empty = unencrypted key) (#90).
            let pass = password.as_str();
            let keypair = load_session_private_key(session, pass)?;
            // RSA keys must be signed with an explicit SHA-2 hash; every other
            // key type carries its own algorithm, so no override is needed.
            let hash = keypair.algorithm().is_rsa().then_some(HashAlg::Sha256);
            let key_with_hash = PrivateKeyWithHashAlg::new(Arc::new(keypair), hash);
            handle
                .authenticate_publickey(&user, key_with_hash)
                .await
                .context("publickey auth failed")?
                .success()
        }
    };

    if authed {
        Ok(AuthResult::Success)
    } else {
        Ok(AuthResult::Failed)
    }
}

// Key-exchange algorithms offered to the server, strongest first. This is the
// russh default set PLUS the ecdh-sha2-nistp* curves and the legacy
// diffie-hellman-group{14,1}-sha1 exchanges appended as last-resort fallbacks, so
// we can still reach old servers / network gear that only speak SHA-1 KEX and
// otherwise fail with "No common algorithm" (#172). Modern servers still pick a
// strong algorithm because the client's order decides and SHA-1 is last.
//
// Only *client* extension markers are advertised. russh's default also lists the
// server-side `ext-info-s` / `kex-strict-s` names, which a client must not send
// (https://github.com/Eugeny/russh/issues/611) and which crash some old parsers.
pub(crate) const COMPAT_KEX: &[russh::kex::Name] = &[
    russh::kex::CURVE25519,
    russh::kex::CURVE25519_PRE_RFC_8731,
    russh::kex::DH_G16_SHA512,
    russh::kex::DH_G14_SHA256,
    russh::kex::ECDH_SHA2_NISTP256,
    russh::kex::ECDH_SHA2_NISTP384,
    russh::kex::ECDH_SHA2_NISTP521,
    russh::kex::DH_G14_SHA1, // legacy fallback
    russh::kex::DH_G1_SHA1,  // legacy fallback
    russh::kex::EXTENSION_SUPPORT_AS_CLIENT,
    russh::kex::EXTENSION_OPENSSH_STRICT_KEX_AS_CLIENT,
];

// Ciphers offered to the server, strongest first: russh's AEAD/CTR defaults plus
// the legacy CBC ciphers appended for old servers that only support CBC (#172).
pub(crate) const COMPAT_CIPHER: &[russh::cipher::Name] = &[
    russh::cipher::CHACHA20_POLY1305,
    russh::cipher::AES_256_GCM,
    russh::cipher::AES_256_CTR,
    russh::cipher::AES_192_CTR,
    russh::cipher::AES_128_CTR,
    russh::cipher::AES_256_CBC,    // legacy fallback
    russh::cipher::AES_192_CBC,    // legacy fallback
    russh::cipher::AES_128_CBC,    // legacy fallback
    russh::cipher::TRIPLE_DES_CBC, // legacy fallback
];

// Compact RFC-only set for ancient SSH2 stacks (H3C S3100 / Huawei VRP-3.3).
// Those daemons often abort when the KEXINIT lists `@openssh.com` names or is
// larger than their fixed parse buffer. Used only as a second-attempt fallback.
pub(crate) const LEGACY_KEX: &[russh::kex::Name] =
    &[russh::kex::DH_G14_SHA1, russh::kex::DH_G1_SHA1];

pub(crate) const LEGACY_CIPHER: &[russh::cipher::Name] = &[
    russh::cipher::AES_128_CBC,
    russh::cipher::AES_256_CBC,
    russh::cipher::TRIPLE_DES_CBC,
    russh::cipher::AES_128_CTR,
];

pub(crate) const LEGACY_MAC: &[russh::mac::Name] =
    &[russh::mac::HMAC_SHA1, russh::mac::HMAC_SHA256];

pub(crate) const LEGACY_KEY: &[Algorithm] = &[
    Algorithm::Rsa { hash: None },
    Algorithm::Rsa {
        hash: Some(HashAlg::Sha256),
    },
];

pub(crate) const LEGACY_COMPRESSION: &[russh::compression::Name] = &[russh::compression::NONE];

fn ssh_config_with_preferred(preferred: russh::Preferred, compact: bool) -> Arc<client::Config> {
    Arc::new(client::Config {
        // Keep idle connections alive (#160). Interactive shells can sit
        // idle long enough to be dropped by NAT / firewall / server timeouts,
        // especially when shell integration is disabled (#140).
        keepalive_interval: Some(std::time::Duration::from_secs(30)),
        // Short, RFC-shaped ident. russh's default (`SSH-2.0-russh_<ver>`) is
        // fine on OpenSSH but some VRP parsers are picky about the software tag.
        client_id: russh::SshId::Standard("SSH-2.0-meatshell".into()),
        preferred,
        // russh's 2 MiB initial window / 32 KiB packet is fine on OpenSSH, but
        // H3C/Huawei VRP 3.x (S3100) drops CHANNEL_OPEN when the advertised
        // window is that large. Keep the compact profile inside a 64 KiB window.
        window_size: if compact { 65_536 } else { 2 * 1024 * 1024 },
        maximum_packet_size: if compact { 16_384 } else { 32_768 },
        ..<_>::default()
    })
}

fn is_compact_legacy_config(config: &client::Config) -> bool {
    std::ptr::eq(config.preferred.kex.as_ref(), LEGACY_KEX)
}

pub(crate) fn ssh_client_config() -> Arc<client::Config> {
    ssh_config_with_preferred(
        russh::Preferred {
            kex: Cow::Borrowed(COMPAT_KEX),
            cipher: Cow::Borrowed(COMPAT_CIPHER),
            ..russh::Preferred::DEFAULT
        },
        false,
    )
}

pub(crate) fn ssh_legacy_client_config() -> Arc<client::Config> {
    ssh_config_with_preferred(
        russh::Preferred {
            kex: Cow::Borrowed(LEGACY_KEX),
            cipher: Cow::Borrowed(LEGACY_CIPHER),
            mac: Cow::Borrowed(LEGACY_MAC),
            key: Cow::Borrowed(LEGACY_KEY),
            compression: Cow::Borrowed(LEGACY_COMPRESSION),
        },
        true,
    )
}

async fn run_session(
    session: Session,
    mut commands: UnboundedReceiver<SessionCommand>,
    events: UnboundedSender<SessionEvent>,
    initial_cols: u32,
    initial_rows: u32,
) -> Result<()> {
    let session_started = std::time::Instant::now();
    let _ = events.send(SessionEvent::Status(format!(
        "{} {}@{}:{} ...",
        t("连接中", "Connecting"),
        session.user,
        session.host,
        session.port
    )));

    let (mut handle, mut config) = connect_ssh_handshake(&session, &events).await?;
    tracing::info!(
        "[SESSION_START] id={} stage=transport-ready elapsed_ms={}",
        session.id,
        session_started.elapsed().as_millis()
    );

    // --- Auth (shared with SFTP) --------------------------------------
    // Try plain `password` first, then `keyboard-interactive` on a fresh handle —
    // many bastions (JumpServer) disable `password` (#86). Missing credentials
    // are prompted for (#110).
    match authenticate_session(&mut handle, &session, config.clone(), &events).await? {
        AuthResult::Success => {}
        AuthResult::Cancelled => {
            let _ = events.send(SessionEvent::Closed(
                t("已取消登录", "login cancelled").into(),
            ));
            let _ = handle
                .disconnect(Disconnect::ByApplication, "cancelled", "")
                .await;
            return Ok(());
        }
        AuthResult::Failed => {
            tracing::warn!(
                "ssh authentication failed for {}@{}",
                session.user,
                session.host
            );
            let _ = events.send(SessionEvent::Closed(
                t("认证失败", "authentication failed").into(),
            ));
            let _ = handle
                .disconnect(Disconnect::ByApplication, "auth failed", "")
                .await;
            return Ok(());
        }
    };
    tracing::info!(
        "[SESSION_START] id={} stage=authenticated elapsed_ms={}",
        session.id,
        session_started.elapsed().as_millis()
    );

    // The integration body is Bash/Zsh-specific. Probe out-of-band before the
    // interactive channel exists, so ash/dash/fish/unknown shells never receive
    // (and therefore can never display or get stuck parsing) the setup command.
    // Skip that extra exec channel on compact/legacy transports: H3C/VRP SSH
    // only speaks a single CLI session and disconnects the whole TCP socket
    // when it sees CHANNEL_OPEN + exec (then the real shell open fails with
    // `Disconnected`).
    let skip_exec_probe = session.disable_shell_integration || is_compact_legacy_config(&config);
    let mut prompt_setup_supported =
        !skip_exec_probe && remote_supports_prompt_setup(&handle).await;

    // --- Shell channel --------------------------------------------------
    let mut channel = match handle.channel_open_session().await {
        Ok(ch) => ch,
        Err(err) if !skip_exec_probe && should_retry_legacy(&err) => {
            tracing::info!(
                "session channel failed after shell probe ({err}); reconnecting without exec probe"
            );
            let _ = handle.disconnect(Disconnect::ByApplication, "", "").await;
            let (new_handle, new_config) = connect_ssh_handshake(&session, &events).await?;
            handle = new_handle;
            config = new_config;
            match authenticate_session(&mut handle, &session, config.clone(), &events).await? {
                AuthResult::Success => {}
                AuthResult::Cancelled => {
                    let _ = events.send(SessionEvent::Closed(
                        t("已取消登录", "login cancelled").into(),
                    ));
                    return Ok(());
                }
                AuthResult::Failed => {
                    let _ = events.send(SessionEvent::Closed(
                        t("认证失败", "authentication failed").into(),
                    ));
                    return Ok(());
                }
            }
            prompt_setup_supported = false;
            handle
                .channel_open_session()
                .await
                .context("open session channel")?
        }
        Err(err) => {
            return Err(anyhow!(err)).context("open session channel");
        }
    };

    channel
        .request_pty(
            true,
            "xterm-256color",
            initial_cols,
            initial_rows,
            0,
            0,
            &[],
        )
        .await
        .context("request PTY")?;
    channel.request_shell(true).await.context("request shell")?;

    tracing::info!(
        "[SESSION_START] id={} stage=terminal-ready elapsed_ms={}",
        session.id,
        session_started.elapsed().as_millis()
    );

    let _ = events.send(SessionEvent::Connected);
    let _ = events.send(SessionEvent::Status(format!(
        "{} {}@{}",
        t("已连接", "Connected"),
        session.user,
        session.host
    )));

    // Whether we have already injected the PROMPT_COMMAND setup.
    // We wait for the first non-empty data chunk (the initial shell prompt)
    // before sending so the command doesn't interleave with banner text.
    let mut prompt_injected = false;
    // True from injecting PROMPT_SETUP until the echoed setup line has been
    // received and stripped; output is buffered (not shown) during that window.
    let mut suppress_echo = false;
    // Buffers output while `suppress_echo` so the (long) echoed setup line can be
    // stripped even when it splits across reads (#98).
    let mut echo_buf = String::new();
    // `strip_late_prompt_setup_echo` must only run while an initial setup echo
    // can genuinely still be in flight. Leaving it enabled for the whole SSH
    // session makes recalling an accidentally saved setup command clear normal
    // terminal rows (#289).
    let mut late_prompt_echo_pending = false;
    // After a ZMODEM transfer finishes we briefly ignore ZMODEM detection so the
    // sender's lingering close frames can't spawn a spurious second receive (#76).
    let mut zmodem_done_at: Option<std::time::Instant> = None;

    // Cwd-notification (OSC 7) setup, injected once after the first prompt so
    // the SFTP panel can follow `cd` (#91). It must work across shells:
    //   • bash/sh  → PROMPT_COMMAND runs `__ms7` before every prompt.
    //   • zsh      → bash's PROMPT_COMMAND is IGNORED by zsh, so we register a
    //                `precmd` hook via `add-zsh-hook` instead (non-destructive —
    //                it preserves oh-my-zsh / p10k hooks, unlike `precmd(){…}`).
    //   • fish     → guarded out (fish 3.1+ emits OSC 7 itself).
    // `__ms7` is called once at the end so the initial cwd arrives immediately.
    //
    // The whole shell-specific body lives inside `eval '…'`: fish can't parse
    // bash/zsh function & `if` syntax, but it CAN parse `eval '<opaque string>'`,
    // and the `test -z "$FISH_VERSION" &&` guard short-circuits before the eval
    // ever runs under fish (#71). The body uses only double quotes inside so the
    // outer single-quoted string needs no escaping; printf turns \033/\007 into
    // ESC/BEL at prompt time. No array syntax → safe to *parse* in dash/ash too.
    //
    // The leading space keeps the line out of shell history (HISTCONTROL=
    // ignorespace, the default on most distros); its echo is stripped locally
    // (the needle below) so the bookkeeping command never shows up.
    //
    // Besides OSC 7 (cwd), the hook also captures the command the user just ran
    // and reports it via a private `OSC 697 ; <cmd> BEL` so it can join the
    // command-box history (#113) — terminal-typed commands aren't otherwise
    // recorded. `__msc` reads the last history entry with `fc -ln -1`; this only
    // ever sees real executed commands, never password prompts (those use
    // `read -s` and aren't shell commands). `__cl` remembers the last reported
    // command so a redrawn prompt (e.g. Enter on an empty line) doesn't re-emit
    // it, and is primed once up front so the pre-session history isn't replayed.
    //
    // The echoed setup line is discarded through the private OSC 699 completion
    // marker emitted after installation (see the suppress block below), so zsh
    // redraws and soft wrapping cannot make the internal command visible.
    let prompt_setup = format!(" {}\r", PROMPT_BODY);
    let mut first_terminal_output = true;

    let mut terminal_decoder = crate::terminal::TerminalEncoding::new(&session.encoding);
    let mut extended_decoder = crate::terminal::TerminalEncoding::new(&session.encoding);
    let terminal_encoder = crate::terminal::TerminalEncoding::new(&session.encoding);

    // --- Main pump ------------------------------------------------------
    loop {
        tokio::select! {
            cmd = commands.recv() => {
                match cmd {
                    Some(SessionCommand::RawInput(bytes)) => {
                        // Only log the byte count — never the bytes themselves,
                        // which are raw keystrokes and may contain passwords (#15).
                        tracing::debug!("ssh channel.data len={} bytes", bytes.len());
                        let bytes = terminal_encoder.encode(&bytes);
                        if let Err(err) = channel.data(&bytes[..]).await {
                            let _ = events.send(SessionEvent::Closed(format!("{}: {err}", t("写入失败", "write failed"))));
                            break;
                        }
                    }
                    Some(SessionCommand::Resize(cols, rows)) => {
                        let _ = channel.window_change(cols, rows, 0, 0).await;
                    }
                    Some(SessionCommand::Close) | None => {
                        let _ = channel.eof().await;
                        break;
                    }
                }
            }
            msg = channel.wait() => {
                match msg {
                    Some(ChannelMsg::Data { data }) => {
                        // A `sz` in the terminal starts a ZMODEM send. Receive it
                        // straight to the Downloads dir (FinalShell style, #76).
                        // On any protocol error, cancel so the session recovers.
                        let zmodem_cooldown = zmodem_done_at
                            .is_some_and(|t| t.elapsed() < std::time::Duration::from_secs(2));
                        if !zmodem_cooldown && contains_zmodem_init(&data) {
                            let result =
                                crate::terminal::zmodem::receive(&mut channel, &data, &events).await;
                            zmodem_done_at = Some(std::time::Instant::now());
                            match result {
                                Ok(leftover) => {
                                    // Bytes after the transfer (the shell prompt):
                                    // run them through the normal output path so
                                    // the prompt shows and the cwd updates.
                                    if !leftover.is_empty() {
                                        let text = terminal_decoder.decode(&leftover);
                                        if let Some(cwd) = extract_osc7_path(&text) {
                                            let _ =
                                                events.send(SessionEvent::CwdChanged(cwd));
                                        }
                                        let _ = events.send(SessionEvent::Output(text));
                                    }
                                }
                                Err(e) => {
                                    tracing::warn!("zmodem receive failed: {e:#}");
                                    let _ = channel.data(&ZMODEM_CANCEL[..]).await;
                                    let _ = events.send(SessionEvent::Output(format!(
                                        "\r\n[meatshell] {}: {e}\r\n",
                                        t("ZMODEM 接收失败,已取消", "ZMODEM receive failed; cancelled")
                                    ).into()));
                                }
                            }
                            continue;
                        }

                        let chunk = terminal_decoder.decode(&data);

                        if first_terminal_output {
                            first_terminal_output = false;
                            tracing::info!(
                                "[SESSION_START] id={} stage=first-terminal-output elapsed_ms={}",
                                session.id,
                                session_started.elapsed().as_millis()
                            );
                        }

                        // Inject PROMPT_COMMAND after the first real shell output,
                        // unless shell integration is disabled for this session
                        // (e.g. a Windows pwsh/cmd server) (#140).
                        if !prompt_injected
                            && !chunk.trim().is_empty()
                            && prompt_setup_supported
                        {
                            prompt_injected = true;
                            suppress_echo = true;
                            // A separate exec probe already confirmed bash or zsh.
                            // Keep buffering until the hook's OSC 7 arrives: slow
                            // Linux/macOS PTYs may echo this command after several
                            // seconds, while unsupported Windows shells never enter
                            // this branch.
                            // Paint the banner/prompt immediately. Only later
                            // output containing our injected setup command is
                            // buffered and stripped; the first usable terminal
                            // frame no longer waits for shell integration.
                            let _ = events.send(SessionEvent::Output(chunk));
                            let _ = channel.data(prompt_setup.as_bytes()).await;
                            continue;
                        }

                        // While suppressing, wait for the private OSC 699 completion
                        // marker emitted by the executed setup command. Do not infer
                        // completion from echoed text size: zsh/ZLE may redraw the
                        // long input line often enough to exceed 16 KiB before it
                        // executes, which previously released the internal command
                        // onto the terminal (#344). Output before the marker is
                        // private setup echo and is safely discarded; the rolling
                        // buffer remains bounded while preserving split markers.
                        let mut text = if suppress_echo {
                            echo_buf.push_str(&chunk);
                            if let Some(tail) = take_after_prompt_setup_done(&mut echo_buf) {
                                suppress_echo = false;
                                late_prompt_echo_pending = false;
                                if let Some(cwd) = extract_osc7_path(&tail) {
                                    tracing::debug!("OSC7 cwd={:?}", cwd);
                                    let _ = events.send(SessionEvent::CwdChanged(cwd));
                                }
                                tail
                            } else {
                                bound_prompt_setup_echo(&mut echo_buf);
                                continue; // keep buffering; show nothing yet
                            }
                        } else {
                            // Scan for the OSC 7 CWD notification (cd-follow).
                            if let Some(cwd) = extract_osc7_path(&chunk) {
                                tracing::debug!("OSC7 cwd={:?}", cwd);
                                let _ = events.send(SessionEvent::CwdChanged(cwd));
                            }
                            let mut clean = chunk;
                            strip_pending_prompt_setup_echo(
                                &mut clean,
                                &mut late_prompt_echo_pending,
                            );
                            clean
                        };

                        // Capture commands run in the terminal via our OSC 697
                        // hook, and strip the sequence so it never reaches the
                        // renderer (#113). Skip our own injected setup line in the
                        // rare case HISTCONTROL=ignorespace isn't in effect.
                        while let Some((cmd, range)) = extract_osc_command(&text) {
                            text.replace_range(range, "");
                            let cmd = cmd.trim();
                            if !cmd.is_empty() && !cmd.contains("__ms7") {
                                let _ = events.send(SessionEvent::CommandRan(cmd.to_string()));
                            }
                        }

                        let _ = events.send(SessionEvent::Output(text));
                    }
                    Some(ChannelMsg::ExtendedData { data, ext: _ }) => {
                        let text = extended_decoder.decode(&data);
                        let _ = events.send(SessionEvent::Output(text));
                    }
                    Some(ChannelMsg::ExitStatus { exit_status }) => {
                        let _ = events.send(SessionEvent::Status(
                            format!("{} (code {exit_status})", t("远程进程退出", "remote process exited")),
                        ));
                    }
                    Some(ChannelMsg::Close) | None => {
                        break;
                    }
                    _ => {}
                }
            }
        }
    }

    let _ = handle
        .disconnect(Disconnect::ByApplication, "bye", "")
        .await;
    // The shell pump loop only exits when the channel closes / EOFs (incl. a
    // peer/bastion-initiated disconnect), so record it for #86 diagnostics.
    tracing::warn!("ssh connection closed ({}@{})", session.user, session.host);
    let _ = events.send(SessionEvent::Closed(
        t("连接已关闭", "connection closed").into(),
    ));
    Ok(())
}

/// True if a keyboard-interactive prompt is asking for a second factor (an MFA /
/// OTP / verification code) rather than the account password. We answer password
/// challenges automatically with the stored password but must ask the user for
/// these (#86-MFA). Heuristic over the common English/Chinese wordings used by
/// JumpServer, Google Authenticator (PAM), Duo, etc.
fn looks_like_mfa(prompt: &str) -> bool {
    let t = prompt.to_lowercase();
    t.contains("code")
        || t.contains("otp")
        || t.contains("mfa")
        || t.contains("2fa")
        || t.contains("factor") // two-factor / second factor
        || t.contains("duo")
        || t.contains("verification")
        || t.contains("verify")
        || t.contains("token")
        || t.contains("authenticator")
        || t.contains("passcode")
        || t.contains("one-time")
        || t.contains("one time")
        || t.contains("验证码")
        || t.contains("动态")
        || t.contains("令牌")
}

/// Authenticate via `keyboard-interactive`. The stored password answers the
/// first password challenge automatically (the JumpServer-style bastions that
/// disable the plain `password` method, #86); any *other* challenge — an MFA /
/// verification-code prompt — is shown to the user, whose typed answer is sent
/// back. This is what makes MFA-enabled bastions (JumpServer with MFA forced on)
/// work (#86-MFA).
pub(crate) async fn keyboard_interactive_auth<H>(
    handle: &mut Handle<H>,
    user: &str,
    password: &str,
    session_id: &str,
    host: &str,
    events: &UnboundedSender<SessionEvent>,
) -> Result<bool>
where
    H: Handler + 'static,
    H::Error: std::error::Error + Send + Sync + 'static,
{
    use russh::client::KeyboardInteractiveAuthResponse as Kb;
    let mut res = handle
        .authenticate_keyboard_interactive_start(user.to_string(), None)
        .await?;
    let mut password_used = false;
    // Bound the exchange so a misbehaving server can't loop us forever.
    for _ in 0..16 {
        match res {
            Kb::Success => return Ok(true),
            Kb::Failure { .. } => return Ok(false),
            Kb::InfoRequest { prompts, .. } => {
                let mut responses = Vec::with_capacity(prompts.len());
                for p in &prompts {
                    // Use the stored password for the first password-like
                    // challenge; ask the user for everything else (MFA codes).
                    if !password_used && !password.is_empty() && !looks_like_mfa(&p.prompt) {
                        responses.push(password.to_string());
                        password_used = true;
                    } else {
                        match ask_mfa_prompt(session_id, host, &p.prompt, p.echo, events).await {
                            Some(answer) => responses.push(answer),
                            None => return Ok(false), // user cancelled
                        }
                    }
                }
                res = handle
                    .authenticate_keyboard_interactive_respond(responses)
                    .await?;
            }
        }
    }
    Ok(false)
}

/// Ask the UI for a single keyboard-interactive answer (an MFA / verification
/// code), blocking until the user responds. `None` = cancelled or no UI (#86-MFA).
async fn ask_mfa_prompt(
    session_id: &str,
    host: &str,
    prompt: &str,
    echo: bool,
    events: &UnboundedSender<SessionEvent>,
) -> Option<String> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let sent = events.send(SessionEvent::MfaPrompt {
        session_id: session_id.to_string(),
        host: host.to_string(),
        prompt: prompt.to_string(),
        echo,
        responder: MfaResponder::new(tx),
    });
    if sent.is_err() {
        return None; // no UI to ask
    }
    rx.await.ok().flatten()
}

/// Client handler. Verifies the server host key against the known_hosts store,
/// prompting the user on first contact / on a changed key (#109-5).
pub(crate) struct ClientHandler {
    pub(crate) host: String,
    pub(crate) port: u16,
    pub(crate) events: UnboundedSender<SessionEvent>,
}

/// Shared host-key check used by both the shell and SFTP connections: trust a
/// matching stored key silently; otherwise ask the UI (via `events`) and, on
/// acceptance, remember the key. A dropped/closed reply channel (UI gone)
/// counts as a rejection so we never connect to an unverified host.
pub(crate) async fn verify_host_key(
    host: &str,
    port: u16,
    key: &PublicKey,
    events: &UnboundedSender<SessionEvent>,
) -> bool {
    use crate::ssh::HostKeyStatus;
    match crate::ssh::known_hosts::verify(host, port, key) {
        HostKeyStatus::Match => true,
        status => {
            let changed = status == HostKeyStatus::Changed;
            let (tx, rx) = tokio::sync::oneshot::channel();
            let sent = events.send(SessionEvent::HostKeyPrompt {
                host: host.to_string(),
                port,
                key_type: key.algorithm().to_string(),
                fingerprint: crate::ssh::known_hosts::fingerprint(key),
                changed,
                responder: HostKeyResponder::new(tx),
            });
            if sent.is_err() {
                return false; // no UI to ask
            }
            match rx.await {
                Ok(true) => {
                    if let Err(e) = crate::ssh::known_hosts::remember(host, port, key) {
                        tracing::warn!("could not save host key for {host}:{port}: {e:#}");
                    }
                    true
                }
                _ => false,
            }
        }
    }
}

/// Resolve a session's username/password, prompting the UI for whatever is
/// missing (#110). Returns the effective `(user, password)`, or `None` if the
/// user cancelled. Both the shell and SFTP connections call this; the UI
/// de-duplicates by session id so a single dialog serves both. A dropped reply
/// channel (no UI) falls through with the stored values so auth fails normally.
pub(crate) async fn resolve_credentials(
    session: &Session,
    events: &UnboundedSender<SessionEvent>,
) -> Option<(String, String)> {
    let mut user = session.user.trim().to_string();
    let mut password = session.password.as_str().to_string();
    let need_user = user.is_empty();
    let need_password = matches!(session.auth, AuthMethod::Password) && password.is_empty();
    if !(need_user || need_password) {
        return Some((user, password));
    }
    let (tx, rx) = tokio::sync::oneshot::channel();
    let sent = events.send(SessionEvent::CredentialPrompt {
        session_id: session.id.clone(),
        host: session.host.clone(),
        user: user.clone(),
        need_user,
        need_password,
        responder: CredentialResponder::new(tx),
    });
    if sent.is_err() {
        return Some((user, password));
    }
    match rx.await {
        Ok(Some((u, p, _remember))) => {
            if need_user {
                user = u.trim().to_string();
            }
            if need_password {
                password = p;
            }
            Some((user, password))
        }
        _ => None,
    }
}

impl Handler for ClientHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &PublicKey,
    ) -> Result<bool, Self::Error> {
        Ok(verify_host_key(&self.host, self.port, server_public_key, &self.events).await)
    }
}

// Marker trait impl so `Arc<Handle<Handler>>` is nameable in external code.
#[allow(dead_code)]
fn _assert_handle_send() {
    fn takes<T: Send>() {}
    takes::<Handle<ClientHandler>>();
}

#[cfg(test)]
mod prompt_setup_echo_tests {
    use super::{
        bound_prompt_setup_echo, prompt_setup_echo_end, prompt_setup_supported,
        strip_late_prompt_setup_echo, strip_pending_prompt_setup_echo, strip_prompt_setup_echo,
        take_after_prompt_setup_done, PROMPT_BODY, PROMPT_SETUP_DONE, PROMPT_SETUP_HISTORY_MARKER,
        PROMPT_SETUP_PREFIX,
    };

    #[test]
    fn only_bash_and_zsh_receive_prompt_setup() {
        assert_eq!(
            prompt_setup_supported("__MEATSHELL_SHELL__:bash\n"),
            Some(true)
        );
        assert_eq!(
            prompt_setup_supported("__MEATSHELL_SHELL__:zsh\n"),
            Some(true)
        );
        assert_eq!(
            prompt_setup_supported("__MEATSHELL_SHELL__:other\n"),
            Some(false)
        );
        assert_eq!(prompt_setup_supported("ash: syntax error\n"), None);
    }

    #[test]
    fn bash_setup_removes_current_and_stale_history_entries() {
        assert!(PROMPT_BODY.contains(PROMPT_SETUP_HISTORY_MARKER));
        assert!(PROMPT_BODY.contains("history 2>/dev/null"));
        assert!(PROMPT_BODY.contains("__ms7()"));
        assert!(PROMPT_BODY.contains("history -d \"$__mn\""));
        // Re-prime command capture only after deleting the setup entry, so the
        // previous real user command does not get reported as newly executed.
        assert!(PROMPT_BODY.find("history -d").unwrap() < PROMPT_BODY.rfind("__cl=").unwrap());
        assert!(PROMPT_BODY.contains("699;ready"));
    }

    #[test]
    fn completion_marker_hides_corrupted_large_zsh_redraws() {
        let mut buffered = "cst test -z redraw\r".repeat(5000);
        buffered.push_str(PROMPT_SETUP_DONE);
        buffered.push_str("\u{1b}]7;file://host/home/user\u{07}prompt");

        let tail = take_after_prompt_setup_done(&mut buffered).expect("completion marker");
        assert!(buffered.is_empty());
        assert_eq!(tail, "\u{1b}]7;file://host/home/user\u{07}prompt");
        assert!(!tail.contains("test -z"));
    }

    #[test]
    fn rolling_setup_buffer_preserves_a_split_completion_marker() {
        let split = 6;
        let mut buffered = "redraw".repeat(20_000);
        buffered.push_str(&PROMPT_SETUP_DONE[..split]);
        bound_prompt_setup_echo(&mut buffered);
        assert!(buffered.len() < 1024);

        buffered.push_str(&PROMPT_SETUP_DONE[split..]);
        buffered.push_str("prompt");
        assert_eq!(
            take_after_prompt_setup_done(&mut buffered).as_deref(),
            Some("prompt")
        );
    }

    #[test]
    fn strips_oh_my_zsh_echo_without_newline() {
        let mut text = format!(
            "➜  ~  {} && eval 'body; __ms7'\rafter prompt",
            PROMPT_SETUP_PREFIX
        );
        let p = text.find(PROMPT_SETUP_PREFIX).unwrap();
        let end = prompt_setup_echo_end(&text, p);
        strip_prompt_setup_echo(&mut text, p, end);
        assert_eq!(text, "\r\x1b[2Kafter prompt");
    }

    #[test]
    fn strips_echo_through_osc7() {
        let mut text = format!(
            "banner\n➜  ~  {} && eval 'body; __ms7'\r\u{1b}]7;file://host/home/jeff\u{07}prompt",
            PROMPT_SETUP_PREFIX
        );
        let p = text.find(PROMPT_SETUP_PREFIX).unwrap();
        let osc_end = text.find("prompt").unwrap();
        strip_prompt_setup_echo(&mut text, p, osc_end);
        assert_eq!(text, "banner\n\r\x1b[2Kprompt");
    }

    #[test]
    fn strips_late_echoed_setup_command() {
        let mut text = format!(
            "prompt\r\n{} && eval 'body; __ms7'\r\nafter",
            PROMPT_SETUP_PREFIX
        );
        assert!(strip_late_prompt_setup_echo(&mut text));
        assert_eq!(text, "prompt\r\n\r\x1b[2Kafter");
    }

    #[test]
    fn late_setup_filter_disables_itself_after_one_match() {
        let echoed = format!(
            "prompt\r\n{} && eval 'body; __ms7'\r\nafter",
            PROMPT_SETUP_PREFIX
        );
        let mut pending = true;
        let mut first = echoed.clone();
        assert!(strip_pending_prompt_setup_echo(&mut first, &mut pending));
        assert!(!pending);

        // A later readline recall can contain the same private setup text. It
        // must reach the terminal untouched instead of clearing visible rows.
        let mut recalled = echoed.clone();
        assert!(!strip_pending_prompt_setup_echo(
            &mut recalled,
            &mut pending
        ));
        assert_eq!(recalled, echoed);
    }

    #[test]
    fn hidden_setup_echo_resynchronizes_the_prompt_cursor() {
        let prompt = "root@host:~# ";
        let mut parser = vt100::Parser::new(4, 80, 0);
        // The initial prompt is painted immediately before shell integration is
        // injected. The buffered setup echo must replace, not append to, it.
        parser.process(prompt.as_bytes());
        let mut echoed = format!(
            "{prompt}{} && eval 'body; __ms7'\r\n\u{1b}]7;file://host/root\u{07}{prompt}",
            PROMPT_SETUP_PREFIX
        );
        let prefix = echoed.find(PROMPT_SETUP_PREFIX).unwrap();
        let osc_end = echoed.rfind(prompt).unwrap();
        strip_prompt_setup_echo(&mut echoed, prefix, osc_end);
        parser.process(echoed.as_bytes());

        assert_eq!(parser.screen().contents().lines().next(), Some(prompt));
        assert_eq!(parser.screen().cursor_position(), (0, prompt.len() as u16));
    }
}

#[cfg(test)]
mod osc_command_tests {
    use super::extract_osc_command;

    #[test]
    fn extracts_and_locates_bel_terminated() {
        let text = "before\u{1b}]697;ls -la\u{07}after";
        let (cmd, range) = extract_osc_command(text).expect("found");
        assert_eq!(cmd, "ls -la");
        // Stripping the range leaves the surrounding text intact.
        let mut s = text.to_string();
        s.replace_range(range, "");
        assert_eq!(s, "beforeafter");
    }

    #[test]
    fn extracts_st_terminated() {
        let text = "\u{1b}]697;echo hi\u{1b}\\";
        let (cmd, _) = extract_osc_command(text).expect("found");
        assert_eq!(cmd, "echo hi");
    }

    #[test]
    fn ignores_other_osc_and_incomplete() {
        // OSC 7 (cwd) is not a command sequence.
        assert!(extract_osc_command("\u{1b}]7;file:///home\u{07}").is_none());
        // No terminator yet → wait for more.
        assert!(extract_osc_command("\u{1b}]697;ls").is_none());
        assert!(extract_osc_command("plain text").is_none());
    }
}

#[cfg(test)]
mod mfa_tests {
    use super::looks_like_mfa;

    #[test]
    fn password_prompts_are_not_mfa() {
        // These should be answered automatically with the stored password.
        for p in [
            "Password: ",
            "password:",
            "jeff@host's password:",
            "请输入密码:",
            "Password for jeff:",
        ] {
            assert!(!looks_like_mfa(p), "wrongly flagged as MFA: {p:?}");
        }
    }

    #[test]
    fn verification_code_prompts_are_mfa() {
        // These must prompt the user (JumpServer / Google Authenticator / Duo …).
        for p in [
            "MFA code: ",
            "[MFA] Please enter 6 digit code: ",
            "Verification code: ",
            "Verification code (from your authenticator app): ",
            "One-time password (OATH-TOTP): ",
            "Enter passcode or select one of the following options:",
            "Duo two-factor login",
            "请输入验证码:",
            "动态口令:",
            "请输入令牌:",
        ] {
            assert!(looks_like_mfa(p), "missed an MFA prompt: {p:?}");
        }
    }
}

#[cfg(test)]
mod legacy_ssh_compat_tests {
    use super::{
        is_compact_legacy_config, should_retry_legacy, ssh_client_config, ssh_legacy_client_config,
        COMPAT_KEX, LEGACY_CIPHER, LEGACY_KEX, LEGACY_MAC,
    };

    fn has_at(name: impl AsRef<str>) -> bool {
        name.as_ref().contains('@')
    }

    #[test]
    fn legacy_algorithm_names_are_rfc_only() {
        assert!(LEGACY_KEX.iter().all(|n| !has_at(n)));
        assert!(LEGACY_CIPHER.iter().all(|n| !has_at(n)));
        assert!(LEGACY_MAC.iter().all(|n| !has_at(n)));
    }

    #[test]
    fn modern_kex_does_not_advertise_server_extension_markers() {
        let names: Vec<&str> = COMPAT_KEX.iter().map(|n| n.as_ref()).collect();
        assert!(!names.iter().any(|n| n.contains("ext-info-s")), "{names:?}");
        assert!(
            !names.iter().any(|n| n.contains("kex-strict-s")),
            "{names:?}"
        );
        assert!(names.iter().any(|n| *n == "ext-info-c"));
    }

    #[test]
    fn retry_legacy_on_handshake_disconnect() {
        assert!(should_retry_legacy(&russh::Error::Disconnect));
        assert!(should_retry_legacy(&russh::Error::NoCommonAlgo {
            kind: russh::AlgorithmKind::Kex,
            ours: Vec::new(),
            theirs: Vec::new(),
        }));
        assert!(should_retry_legacy(&russh::Error::IO(
            std::io::Error::from(std::io::ErrorKind::ConnectionReset)
        )));
        assert!(!should_retry_legacy(&russh::Error::NotAuthenticated));
        assert!(!should_retry_legacy(&russh::Error::ConnectionTimeout));
    }

    #[test]
    fn client_ident_is_short_rfc_string() {
        for config in [ssh_client_config(), ssh_legacy_client_config()] {
            match &config.client_id {
                russh::SshId::Standard(s) => assert_eq!(s, "SSH-2.0-meatshell"),
                russh::SshId::Raw(s) => panic!("expected Standard ident, got raw {s:?}"),
            }
        }
    }

    #[test]
    fn compact_legacy_profile_uses_small_windows() {
        let compact = ssh_legacy_client_config();
        assert!(is_compact_legacy_config(&compact));
        assert_eq!(compact.window_size, 65_536);
        assert_eq!(compact.maximum_packet_size, 16_384);

        let modern = ssh_client_config();
        assert!(!is_compact_legacy_config(&modern));
        assert_eq!(modern.window_size, 2 * 1024 * 1024);
    }
}
