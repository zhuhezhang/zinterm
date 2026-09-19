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

/// Expand zsh `fc -ln` newline escapes (`\n` / `\r` / `\\`) into real controls.
///
/// Bash already emits real LFs, so strings that contain them are left alone.
/// A lone trailing `\n` (e.g. `echo \n`) is preserved — only escapes with
/// non-whitespace on both sides, or heredoc markers, are expanded so we don't
/// corrupt intentional backslash-n.
pub fn repair_fc_newlines(cmd: &str) -> String {
    if cmd.contains('\n') || cmd.contains('\r') || !cmd.contains('\\') {
        return cmd.to_string();
    }
    let needs = cmd.contains("<<") || {
        let b = cmd.as_bytes();
        let mut i = 0;
        let mut found = false;
        while i + 1 < b.len() {
            if b[i] == b'\\' && (b[i + 1] == b'n' || b[i + 1] == b'r') {
                // Skip a doubled backslash (`\\n` → intentional `\n` after %b).
                let doubled = i > 0 && b[i - 1] == b'\\';
                if !doubled {
                    let left = cmd[..i].chars().any(|c| !c.is_whitespace());
                    let right = cmd[i + 2..].chars().any(|c| !c.is_whitespace());
                    if left && right {
                        found = true;
                        break;
                    }
                }
            }
            i += 1;
        }
        found
    };
    if !needs {
        return cmd.to_string();
    }
    let mut out = String::with_capacity(cmd.len());
    let mut chars = cmd.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some('\\') => out.push('\\'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Find a zinterm command-capture sequence (`ESC ] 697 ; <command> BEL|ST`)
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

#[cfg(test)]
mod osc_command_tests {
    use super::{extract_osc_command, repair_fc_newlines};

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

    #[test]
    fn repairs_zsh_fc_heredoc_escapes() {
        let raw = "cat <<EOF\\nline1\\nEOF";
        let fixed = repair_fc_newlines(raw);
        assert_eq!(fixed, "cat <<EOF\nline1\nEOF");
        assert!(fixed.contains('\n'));
    }

    #[test]
    fn repairs_two_line_command_without_heredoc() {
        assert_eq!(repair_fc_newlines("echo a\\necho b"), "echo a\necho b");
    }

    #[test]
    fn leaves_intentional_echo_backslash_n_alone() {
        assert_eq!(repair_fc_newlines("echo \\n"), "echo \\n");
    }

    #[test]
    fn leaves_real_newlines_alone() {
        let cmd = "cat <<EOF\nline1\nEOF";
        assert_eq!(repair_fc_newlines(cmd), cmd);
    }
}
