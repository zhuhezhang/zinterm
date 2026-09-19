use std::borrow::Cow;
use std::path::Path;

use anyhow::{anyhow, Context, Result};
use russh::keys::{decode_secret_key, load_secret_key, PrivateKey};

use crate::config::Session;
use crate::i18n::t;

/// Expand a leading `~/` (or bare `~`) to the user's home directory.
/// Paths without a tilde prefix are returned unchanged.
fn expand_user_path(path: &str) -> Cow<'_, Path> {
    if path == "~" {
        if let Some(home) = directories::UserDirs::new() {
            return Cow::Owned(home.home_dir().to_path_buf());
        }
    } else if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = directories::UserDirs::new() {
            return Cow::Owned(home.home_dir().join(rest));
        }
    }
    Cow::Borrowed(Path::new(path))
}

pub(crate) fn load_session_private_key(session: &Session, pass: &str) -> Result<PrivateKey> {
    let pass = if pass.is_empty() { None } else { Some(pass) };
    let raw = session.private_key.as_str().trim();
    if raw.is_empty() {
        return Err(anyhow!(t(
            "私钥路径或私钥内容为空",
            "private key path or private key content is empty"
        )));
    }

    if crate::config::looks_like_private_key_content(raw) {
        if crate::ssh::ppk::is_ppk(raw.as_bytes()) {
            return crate::ssh::ppk::decode_ppk(raw.as_bytes(), pass.unwrap_or_default())
                .context("failed to parse pasted PuTTY private key");
        }
        return decode_secret_key(raw, pass).context("failed to parse pasted private key");
    }

    let normalised = raw.replace('\\', "/");
    let key_path = normalised
        .strip_suffix(".pub")
        .map(str::to_string)
        .unwrap_or(normalised);
    let key_path = expand_user_path(&key_path);
    let key_display = key_path.display().to_string();
    if key_path
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("ppk"))
    {
        let raw = std::fs::read(key_path.as_ref())
            .with_context(|| format!("failed to read PuTTY key {key_display}"))?;
        return crate::ssh::ppk::decode_ppk(&raw, pass.unwrap_or_default())
            .with_context(|| format!("failed to load PuTTY key {key_display}"));
    }
    load_secret_key(key_path.as_ref(), pass)
        .with_context(|| format!("failed to load private key {key_display}"))
}

#[cfg(test)]
mod expand_user_path_tests {
    use super::expand_user_path;
    use std::path::Path;

    #[test]
    fn expands_tilde_slash_prefix() {
        let expanded = expand_user_path("~/.ssh/id_ed25519");
        assert!(!expanded.as_ref().starts_with("~"));
        assert!(expanded.as_ref().ends_with(Path::new(".ssh/id_ed25519")));
    }

    #[test]
    fn leaves_absolute_paths_unchanged() {
        let p = "/Users/me/.ssh/id_ed25519";
        assert_eq!(expand_user_path(p).as_ref(), Path::new(p));
    }
}
