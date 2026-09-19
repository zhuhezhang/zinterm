use russh::client::Handle;
use russh::ChannelMsg;

use super::auth::ClientHandler;

const PROMPT_SETUP_PREFIX: &str = "test -z \"$FISH_VERSION\"";
const PROMPT_SETUP_SUFFIX: &str = "__zt7'";
#[cfg(test)]
const PROMPT_SETUP_HISTORY_MARKER: &str = "__ZINTERM_INTERNAL_SETUP_1";
const PROMPT_SETUP_DONE: &str = "\u{1b}]699;ready\u{07}";
// Multiline history: zsh's `fc -ln` prints real newlines as the two-char
// sequence `\n`. We keep this hook short (long lines leak under zsh/ZLE
// redraw) and expand those escapes in `repair_fc_newlines` on receive.
pub(super) const PROMPT_BODY: &str = "test -z \"$FISH_VERSION\" && eval '__ztc(){ __c=\"$(fc -ln -1 2>/dev/null)\"; [ -n \"$__c\" ] && [ \"$__c\" != \"$__cl\" ] && { __cl=\"$__c\"; printf \"\\033]697;%s\\007\" \"$__c\"; }; }; __zt7(){ printf \"\\033]7;file://%s%s\\007\" \"$HOSTNAME\" \"$PWD\"; __ztc; }; if [ -n \"$ZSH_VERSION\" ]; then autoload -Uz add-zsh-hook 2>/dev/null; add-zsh-hook precmd __zt7; else PROMPT_COMMAND=\"__zt7${PROMPT_COMMAND:+;$PROMPT_COMMAND}\"; fi; : __ZINTERM_INTERNAL_SETUP_1; if [ -n \"$BASH_VERSION\" ]; then __md=\"$(history 2>/dev/null | { __md=\"\"; while read -r __mn __mr; do case \"$__mr\" in *\"__zt7()\"*\"PROMPT_COMMAND=\"*) __mn=\"${__mn%\\*}\"; __md=\"$__mn $__md\";; esac; done; printf \"%s\" \"$__md\"; })\"; for __mn in $__md; do history -d \"$__mn\" 2>/dev/null; done; unset __md __mn __mr; fi; __cl=\"$(fc -ln -1 2>/dev/null)\"; printf \"\\033]699;ready\\007\"; __zt7'";
const PROMPT_SHELL_PROBE: &[u8] = b"if [ -n \"$BASH_VERSION\" ]; then printf '__ZINTERM_SHELL__:bash\\n'; elif [ -n \"$ZSH_VERSION\" ]; then printf '__ZINTERM_SHELL__:zsh\\n'; else printf '__ZINTERM_SHELL__:other\\n'; fi";

fn prompt_setup_supported(probe_output: &str) -> Option<bool> {
    if probe_output.contains("__ZINTERM_SHELL__:bash")
        || probe_output.contains("__ZINTERM_SHELL__:zsh")
    {
        Some(true)
    } else if probe_output.contains("__ZINTERM_SHELL__:other") {
        Some(false)
    } else {
        None
    }
}

/// Probe the login shell through a separate exec channel so unsupported shells
/// never see the long interactive prompt-integration command. In particular,
/// BusyBox ash (used by OpenWrt) ignores `PROMPT_COMMAND`; injecting into its
/// line editor only risks a visible partial command or continuation prompt.
pub(super) async fn remote_supports_prompt_setup(handle: &Handle<ClientHandler>) -> bool {
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
/// catch it (#266). Also catches mid-line fragments when zsh/ZLE redraws omit
/// the leading `test -z …` prefix.
fn strip_late_prompt_setup_echo(text: &mut String) -> bool {
    if let Some(prefix_pos) = text.find(PROMPT_SETUP_PREFIX) {
        if let Some(rel_end) = text[prefix_pos..].find(PROMPT_SETUP_SUFFIX) {
            let end = prefix_pos + rel_end + PROMPT_SETUP_SUFFIX.len();
            strip_prompt_setup_echo(text, prefix_pos, end);
            return true;
        }
    }
    // Partial redraw: no prefix, but the unique setup marker / tail is present.
    const MARKER: &str = "__ZINTERM_INTERNAL_SETUP";
    let Some(marker_pos) = text.find(MARKER) else {
        return false;
    };
    let start = line_start_before(text, marker_pos);
    let end = text[marker_pos..]
        .find(PROMPT_SETUP_SUFFIX)
        .map(|rel| marker_pos + rel + PROMPT_SETUP_SUFFIX.len())
        .or_else(|| {
            text[marker_pos..]
                .find(['\r', '\n'])
                .map(|i| marker_pos + i)
        })
        .unwrap_or(text.len());
    strip_prompt_setup_echo(text, start, end);
    true
}

pub(super) fn strip_pending_prompt_setup_echo(text: &mut String, pending: &mut bool) -> bool {
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
pub(super) fn take_after_prompt_setup_done(text: &mut String) -> Option<String> {
    let marker = text.find(PROMPT_SETUP_DONE)?;
    let tail = text.split_off(marker + PROMPT_SETUP_DONE.len());
    text.clear();
    Some(tail)
}

pub(super) fn bound_prompt_setup_echo(text: &mut String) {
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
            prompt_setup_supported("__ZINTERM_SHELL__:bash\n"),
            Some(true)
        );
        assert_eq!(
            prompt_setup_supported("__ZINTERM_SHELL__:zsh\n"),
            Some(true)
        );
        assert_eq!(
            prompt_setup_supported("__ZINTERM_SHELL__:other\n"),
            Some(false)
        );
        assert_eq!(prompt_setup_supported("ash: syntax error\n"), None);
    }

    #[test]
    fn bash_setup_removes_current_and_stale_history_entries() {
        assert!(PROMPT_BODY.contains(PROMPT_SETUP_HISTORY_MARKER));
        assert!(PROMPT_BODY.contains("history 2>/dev/null"));
        assert!(PROMPT_BODY.contains("__zt7()"));
        assert!(PROMPT_BODY.contains("history -d \"$__mn\""));
        // Multiline escapes are expanded in repair_fc_newlines on receive —
        // keep this hook short so zsh/ZLE redraws don't leak it to the screen.
        assert!(!PROMPT_BODY.contains("printf \"%b\""));
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
            "➜  ~  {} && eval 'body; __zt7'\rafter prompt",
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
            "banner\n➜  ~  {} && eval 'body; __zt7'\r\u{1b}]7;file://host/home/jeff\u{07}prompt",
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
            "prompt\r\n{} && eval 'body; __zt7'\r\nafter",
            PROMPT_SETUP_PREFIX
        );
        assert!(strip_late_prompt_setup_echo(&mut text));
        assert_eq!(text, "prompt\r\n\r\x1b[2Kafter");
    }

    #[test]
    fn late_setup_filter_disables_itself_after_one_match() {
        let echoed = format!(
            "prompt\r\n{} && eval 'body; __zt7'\r\nafter",
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
            "{prompt}{} && eval 'body; __zt7'\r\n\u{1b}]7;file://host/root\u{07}{prompt}",
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
