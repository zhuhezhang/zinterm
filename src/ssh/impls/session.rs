use anyhow::{anyhow, Context, Result};
use russh::{ChannelMsg, Disconnect};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

use crate::config::{AlgorithmPreferences, Session};
use crate::i18n::t;

use super::super::structs::*;
use super::auth::{authenticate_session, AuthResult};
use super::osc::{extract_osc7_path, extract_osc_command, repair_fc_newlines};
use super::prompt_setup::{
    bound_prompt_setup_echo, remote_supports_prompt_setup, strip_pending_prompt_setup_echo,
    take_after_prompt_setup_done, PROMPT_BODY,
};
use super::transport::{
    connect_ssh_handshake, is_compact_legacy_config, should_retry_shell_without_probe,
};

/// The canonical ZMODEM abort sequence: eight CAN (0x18) then eight BS (0x08).
/// Sending this makes the remote `sz`/`rz` give up so the session recovers (#76).
const ZMODEM_CANCEL: [u8; 16] = [
    0x18, 0x18, 0x18, 0x18, 0x18, 0x18, 0x18, 0x18, 0x08, 0x08, 0x08, 0x08, 0x08, 0x08, 0x08, 0x08,
];

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
    keepalive_secs: u32,
    algorithms: AlgorithmPreferences,
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
            keepalive_secs,
            algorithms,
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

async fn run_session(
    session: Session,
    mut commands: UnboundedReceiver<SessionCommand>,
    events: UnboundedSender<SessionEvent>,
    initial_cols: u32,
    initial_rows: u32,
    keepalive_secs: u32,
    algorithms: AlgorithmPreferences,
) -> Result<()> {
    let session_started = std::time::Instant::now();
    let _ = events.send(SessionEvent::Status(format!(
        "{} {}@{}:{} ...",
        t("正在SSH连接", "SSH connecting to"),
        session.user,
        session.host,
        session.port
    )));

    let (mut handle, config) =
        connect_ssh_handshake(&session, &events, keepalive_secs, &algorithms).await?;
    tracing::info!(
        "[SESSION_START] id={} stage=transport-ready elapsed_ms={}",
        session.id,
        session_started.elapsed().as_millis()
    );

    // --- Auth (shared with SFTP) --------------------------------------
    // Missing credentials are prompted for (#110).
    match authenticate_session(&mut handle, &session, &events).await? {
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
    // Probe failure already skips injection. Skip the extra exec channel when
    // the session opts out, or on compact/legacy transports: H3C/VRP SSH only
    // speaks a single CLI session and disconnects (or returns
    // AdministrativelyProhibited) when a second CHANNEL_OPEN arrives — then we
    // reconnect without probing.
    let skip_exec_probe = !session.enable_prompt_setup || is_compact_legacy_config(&config);
    let mut prompt_setup_supported =
        !skip_exec_probe && remote_supports_prompt_setup(&handle).await;

    // --- Shell channel --------------------------------------------------
    let mut channel = match handle.channel_open_session().await {
        Ok(ch) => ch,
        Err(err) if !skip_exec_probe && should_retry_shell_without_probe(&err) => {
            tracing::info!(
                "session channel failed after shell probe ({err}); reconnecting without exec probe"
            );
            let _ = handle.disconnect(Disconnect::ByApplication, "", "").await;
            let (new_handle, _) =
                connect_ssh_handshake(&session, &events, keepalive_secs, &algorithms).await?;
            handle = new_handle;
            match authenticate_session(&mut handle, &session, &events).await? {
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
    let _ = events.send(SessionEvent::Status(t("已连接！", "Connected!").into()));

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
    //   • bash/sh  → PROMPT_COMMAND runs `__zt7` before every prompt.
    //   • zsh      → bash's PROMPT_COMMAND is IGNORED by zsh, so we register a
    //                `precmd` hook via `add-zsh-hook` instead (non-destructive —
    //                it preserves oh-my-zsh / p10k hooks, unlike `precmd(){…}`).
    //   • fish     → guarded out (fish 3.1+ emits OSC 7 itself).
    // `__zt7` is called once at the end so the initial cwd arrives immediately.
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
    // recorded. `__ztc` reads the last history entry with `fc -ln -1`; this only
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
                                        "\r\n[zinterm] {}: {e}\r\n",
                                        t("ZMODEM 接收失败,已取消", "ZMODEM receive failed; cancelled")
                                    )));
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

                        // Inject PROMPT_COMMAND after the first real shell output
                        // when the out-of-band probe confirmed bash/zsh.
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
                                // zsh/ZLE may still redraw the long setup line
                                // after the completion marker; keep stripping
                                // identifiable leftovers for a short window.
                                late_prompt_echo_pending = true;
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
                            let cmd = repair_fc_newlines(cmd.trim());
                            if !cmd.is_empty() && !cmd.contains("__zt7") {
                                let _ = events.send(SessionEvent::CommandRan(cmd));
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
