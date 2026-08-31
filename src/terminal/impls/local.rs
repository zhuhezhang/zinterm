//! Local terminal session worker.
//!
//! Local shells need a real PTY/ConPTY. Plain stdin/stdout pipes break normal
//! console editing (Backspace/Delete/IME composition) and make Windows shells
//! disagree about encodings. `portable-pty` gives us ConPTY on Windows and a
//! Unix PTY on Linux/macOS while reusing the same UI event path as SSH.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use portable_pty::{CommandBuilder, PtySize};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

use crate::config::Session;
use crate::i18n::t;
use crate::ssh::{SessionCommand, SessionEvent, SessionHandle};
use crate::terminal::TerminalEncoding;

pub fn spawn_local_session(
    runtime: &tokio::runtime::Handle,
    tab_id: String,
    session: Session,
    initial_cols: u32,
    initial_rows: u32,
) -> (SessionHandle, UnboundedReceiver<SessionEvent>) {
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<SessionCommand>();
    let (evt_tx, evt_rx) = mpsc::unbounded_channel::<SessionEvent>();

    let evt_for_task = evt_tx.clone();
    let join = runtime.spawn(async move {
        if let Err(err) = run_local(
            session,
            cmd_rx,
            evt_for_task.clone(),
            initial_cols,
            initial_rows,
        )
        .await
        {
            let _ = evt_for_task.send(SessionEvent::Closed(format!("{err:#}")));
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

async fn run_local(
    session: Session,
    mut commands: UnboundedReceiver<SessionCommand>,
    events: UnboundedSender<SessionEvent>,
    initial_cols: u32,
    initial_rows: u32,
) -> Result<()> {
    let (program, args) = local_program(&session);
    let label = if session.name.trim().is_empty() {
        program.clone()
    } else {
        session.name.clone()
    };
    let _ = events.send(SessionEvent::Status(format!(
        "{} {}",
        t("启动本地终端", "Starting local terminal"),
        label
    )));

    let pty_system = portable_pty::native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows: initial_rows.clamp(1, u16::MAX as u32) as u16,
            cols: initial_cols.clamp(1, u16::MAX as u32) as u16,
            pixel_width: 0,
            pixel_height: 0,
        })
        .context("failed to open local pty")?;

    let mut cmd = CommandBuilder::new(&program);
    for arg in &args {
        cmd.arg(arg);
    }
    cmd.env("TERM", "xterm-256color");
    if let Some(cwd) = local_working_directory(&session) {
        cmd.cwd(cwd);
    }

    let child = pair
        .slave
        .spawn_command(cmd)
        .with_context(|| format!("failed to start local terminal: {program}"))?;
    drop(pair.slave);

    let mut reader = pair.master.try_clone_reader().context("local pty reader")?;
    let writer = pair.master.take_writer().context("local pty writer")?;
    let writer = Arc::new(Mutex::new(writer));
    let child = Arc::new(Mutex::new(child));
    let encoder = Arc::new(Mutex::new(TerminalEncoding::new(&session.encoding)));

    let _ = events.send(SessionEvent::Connected);
    let _ = events.send(SessionEvent::Status(format!(
        "{} {}",
        t("已启动", "Started"),
        label
    )));

    {
        let reader_events = events.clone();
        let mut decoder = TerminalEncoding::new(&session.encoding);
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => {
                        let _ = reader_events.send(SessionEvent::Closed(
                            t("本地终端已退出", "local terminal exited").into(),
                        ));
                        break;
                    }
                    Ok(n) => {
                        let text = decoder.decode(&buf[..n]);
                        if reader_events.send(SessionEvent::Output(text)).is_err() {
                            break;
                        }
                    }
                    Err(e) => {
                        let _ = reader_events.send(SessionEvent::Closed(format!(
                            "{}: {e}",
                            t("本地终端读取失败", "local terminal read failed")
                        )));
                        break;
                    }
                }
            }
        });
    }

    while let Some(cmd) = commands.recv().await {
        match cmd {
            SessionCommand::RawInput(bytes) => {
                tracing::debug!("local pty write len={} bytes", bytes.len());
                // UI sends Unicode text as UTF-8 bytes; re-encode for the session charset.
                let encoded = encoder.lock().unwrap().encode(&bytes);
                let mut guard = writer.lock().unwrap();
                if guard
                    .write_all(&encoded)
                    .and_then(|_| guard.flush())
                    .is_err()
                {
                    let _ = events.send(SessionEvent::Closed(t("写入失败", "write failed").into()));
                    break;
                }
            }
            SessionCommand::Resize(cols, rows) => {
                let _ = pair.master.resize(PtySize {
                    rows: rows.clamp(1, u16::MAX as u32) as u16,
                    cols: cols.clamp(1, u16::MAX as u32) as u16,
                    pixel_width: 0,
                    pixel_height: 0,
                });
            }
            SessionCommand::Close => {
                let _ = child.lock().unwrap().kill();
                break;
            }
        }
    }
    Ok(())
}

fn local_working_directory(session: &Session) -> Option<PathBuf> {
    let cwd = session.working_directory.trim();
    if !cwd.is_empty() {
        return Some(PathBuf::from(cwd));
    }
    directories::UserDirs::new().map(|u| u.home_dir().to_path_buf())
}

fn local_program(session: &Session) -> (String, Vec<String>) {
    let shell = session.shell.trim();
    if !shell.is_empty() {
        return resolve_shell(shell, &session.encoding);
    }
    default_shell(&session.encoding)
}

fn default_shell(encoding: &str) -> (String, Vec<String>) {
    #[cfg(windows)]
    {
        powershell_program(encoding)
    }
    #[cfg(not(windows))]
    {
        let _ = encoding;
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
        (shell, Vec::new())
    }
}

fn resolve_shell(shell: &str, encoding: &str) -> (String, Vec<String>) {
    let base = Path::new(shell)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(shell)
        .to_ascii_lowercase();

    #[cfg(windows)]
    {
        let lower = shell.to_ascii_lowercase();
        if lower == "cmd" || base == "cmd.exe" || base == "cmd" {
            return cmd_program(shell, encoding);
        }
        if lower == "powershell"
            || lower == "pwsh"
            || base == "powershell.exe"
            || base == "pwsh.exe"
            || base == "powershell"
            || base == "pwsh"
        {
            let program = if lower == "pwsh" || base.starts_with("pwsh") {
                if Path::new(shell).components().count() > 1 {
                    shell.to_string()
                } else {
                    "pwsh.exe".to_string()
                }
            } else if Path::new(shell).components().count() > 1 {
                shell.to_string()
            } else {
                "powershell.exe".to_string()
            };
            return (
                program,
                vec![
                    "-NoLogo".to_string(),
                    "-NoExit".to_string(),
                    "-Command".to_string(),
                    powershell_encoding_command(encoding),
                ],
            );
        }
        (shell.to_string(), Vec::new())
    }
    #[cfg(not(windows))]
    {
        let _ = encoding;
        let _ = base;
        (shell.to_string(), Vec::new())
    }
}

#[cfg(windows)]
fn powershell_program(encoding: &str) -> (String, Vec<String>) {
    (
        "powershell.exe".to_string(),
        vec![
            "-NoLogo".to_string(),
            "-NoExit".to_string(),
            "-Command".to_string(),
            powershell_encoding_command(encoding),
        ],
    )
}

#[cfg(windows)]
fn cmd_program(shell: &str, encoding: &str) -> (String, Vec<String>) {
    let program = if Path::new(shell).components().count() > 1 {
        shell.to_string()
    } else {
        "cmd.exe".to_string()
    };
    (
        program,
        vec![
            "/Q".to_string(),
            "/K".to_string(),
            format!("chcp {}>nul", windows_code_page(encoding)),
        ],
    )
}

#[cfg(windows)]
fn powershell_encoding_command(encoding: &str) -> String {
    let cp = windows_code_page(encoding);
    if cp == 65001 {
        "$utf8 = New-Object System.Text.UTF8Encoding $false; [Console]::InputEncoding = $utf8; [Console]::OutputEncoding = $utf8; $OutputEncoding = $utf8".to_string()
    } else {
        format!(
            "chcp {cp}>$null; $enc = [System.Text.Encoding]::GetEncoding({cp}); [Console]::InputEncoding = $enc; [Console]::OutputEncoding = $enc; $OutputEncoding = $enc"
        )
    }
}

#[cfg(windows)]
fn windows_code_page(encoding: &str) -> u32 {
    match encoding.trim().to_ascii_lowercase().as_str() {
        "gbk" | "gb2312" | "cp936" => 936,
        "big5" | "cp950" => 950,
        "shift_jis" | "shift-jis" | "sjis" | "cp932" => 932,
        "euc-kr" | "cp949" => 949,
        "windows-1252" | "cp1252" => 1252,
        _ => 65001, // UTF-8
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn windows_shells_start_in_utf8_mode() {
        let mut session = Session::new_empty();
        session.kind = crate::config::SessionKind::Local;
        session.shell = "powershell".to_string();
        let (_, ps_args) = local_program(&session);
        assert!(ps_args.iter().any(|arg| arg.contains("OutputEncoding")));
        assert!(ps_args.iter().any(|arg| arg.contains("InputEncoding")));

        session.shell = "cmd".to_string();
        let (_, cmd_args) = local_program(&session);
        assert!(cmd_args.iter().any(|arg| arg.contains("chcp 65001")));
    }

    #[test]
    fn empty_shell_uses_powershell_default() {
        let mut session = Session::new_empty();
        session.kind = crate::config::SessionKind::Local;
        let (program, _) = local_program(&session);
        assert_eq!(program, "powershell.exe");
    }
}
