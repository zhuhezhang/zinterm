//! Tiny runtime internationalisation.
//!
//! Two cooperating mechanisms keep the whole UI translatable:
//!
//! * **Static `.slint` text** uses Slint's own `@tr("English")` plus bundled
//!   `.po` translations.  The source language (the msgids) is **English**; the
//!   Chinese strings live in `lang/zh/LC_MESSAGES/zinterm.po`.  Switching is
//!   done with `slint::select_bundled_translation` (`"zh"` → Chinese, `""`/`"en"`
//!   → the English source).
//!
//! * **Dynamic Rust text** (status lines, errors, transfer details that Rust
//!   builds with `format!`) can't use `@tr`, so it uses [`t`] which returns the
//!   Chinese or English variant based on the current language flag.
//!
//! [`set_language`] updates both at once so the two stay in sync.  Preference
//! codes are `"auto"` (follow the OS), `"zh"`, or `"en"`; anything else is
//! treated as auto.  Non-Chinese system locales resolve to English.

use std::sync::atomic::{AtomicU8, Ordering};

const ZH: u8 = 0;
const EN: u8 = 1;

static LANG: AtomicU8 = AtomicU8::new(ZH);

/// Normalize a UI language preference to `"auto"` / `"zh"` / `"en"`.
pub fn normalize_pref(code: &str) -> &'static str {
    match code.trim().to_ascii_lowercase().as_str() {
        "en" | "english" => "en",
        "zh" | "zh-cn" | "zh_cn" | "zh-hans" | "zh_hans" | "chinese" => "zh",
        _ => "auto",
    }
}

/// Resolve a preference (`"auto"` / `"zh"` / `"en"`) to an effective UI code.
/// Auto follows the OS: Chinese → `"zh"`, otherwise `"en"`.
pub fn resolve_language(pref: &str) -> &'static str {
    match normalize_pref(pref) {
        "en" => "en",
        "zh" => "zh",
        _ => system_language(),
    }
}

/// Apply a language preference (`"auto"` / `"zh"` / `"en"`).  Updates the
/// Rust-side flag and Slint's bundled-translation selection.  Safe to call
/// before the first component exists for the flag; the Slint selection is a
/// no-op error then and should be re-applied once the window is created.
pub fn set_language(code: &str) {
    let en = resolve_language(code) == "en";
    LANG.store(if en { EN } else { ZH }, Ordering::Relaxed);
    apply_to_slint();
}

/// Re-apply the current language to Slint's bundled translations.  Must run
/// after the first component is created (Slint requirement).  We bundle BOTH an
/// `en` (identity) and a `zh` translation and select explicitly, because the
/// empty/`"en"` shortcut selects bundle index 0 — which would be `zh` when only
/// the Chinese bundle exists.
pub fn apply_to_slint() {
    let lang = if is_en() { "en" } else { "zh" };
    let _ = slint::select_bundled_translation(lang);
}

/// Effective UI language is `"en"` when [`is_en`] is true, otherwise `"zh"`.
pub fn is_en() -> bool {
    LANG.load(Ordering::Relaxed) == EN
}

/// Pick the variant for the current language: `zh` is Chinese, `en` is English.
pub fn t(zh: &'static str, en: &'static str) -> &'static str {
    if is_en() {
        en
    } else {
        zh
    }
}

/// OS UI language mapped to zinterm's two locales.  Chinese variants → `"zh"`;
/// English and every other / undetectable locale → `"en"`.
pub fn system_language() -> &'static str {
    if system_locale_is_chinese() {
        "zh"
    } else {
        "en"
    }
}

fn system_locale_is_chinese() -> bool {
    for key in ["LC_ALL", "LC_MESSAGES", "LANG"] {
        if let Ok(val) = std::env::var(key) {
            if let Some(chinese) = locale_looks_chinese(&val) {
                return chinese;
            }
        }
    }
    // GNU LANGUAGE is a colon-separated priority list (e.g. "zh_CN:en_US").
    if let Ok(val) = std::env::var("LANGUAGE") {
        for part in val.split(':') {
            if let Some(chinese) = locale_looks_chinese(part) {
                return chinese;
            }
        }
    }
    #[cfg(windows)]
    {
        if let Some(name) = windows_locale_name() {
            return locale_looks_chinese(&name).unwrap_or(false);
        }
    }
    #[cfg(target_os = "macos")]
    {
        if let Some(name) = macos_apple_locale() {
            return locale_looks_chinese(&name).unwrap_or(false);
        }
    }
    false
}

/// `Some(true)` / `Some(false)` when `locale` names a real language; `None` for
/// empty / C / POSIX so callers can keep looking.
fn locale_looks_chinese(locale: &str) -> Option<bool> {
    let primary = locale
        .trim()
        .split(['.', '@'])
        .next()
        .unwrap_or("")
        .split(['_', '-'])
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    if primary.is_empty() || primary == "c" || primary == "posix" {
        return None;
    }
    Some(primary == "zh")
}

#[cfg(windows)]
fn windows_locale_name() -> Option<String> {
    #[link(name = "kernel32")]
    extern "system" {
        fn GetUserDefaultLocaleName(lp_locale_name: *mut u16, cch_locale_name: i32) -> i32;
    }
    let mut buf = [0u16; 85];
    let len = unsafe { GetUserDefaultLocaleName(buf.as_mut_ptr(), buf.len() as i32) };
    if len > 1 {
        String::from_utf16(&buf[..(len as usize - 1)]).ok()
    } else {
        None
    }
}

/// Finder-launched macOS apps often lack LANG; AppleLocale still reflects the
/// preferred language (e.g. "zh_CN", "en_US").
#[cfg(target_os = "macos")]
fn macos_apple_locale() -> Option<String> {
    let output = std::process::Command::new("defaults")
        .args(["read", "-g", "AppleLocale"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_pref_recognizes_aliases() {
        assert_eq!(normalize_pref("auto"), "auto");
        assert_eq!(normalize_pref(""), "auto");
        assert_eq!(normalize_pref("EN"), "en");
        assert_eq!(normalize_pref("zh_CN"), "zh");
        assert_eq!(normalize_pref("ja"), "auto");
    }

    #[test]
    fn resolve_explicit_ignores_system() {
        assert_eq!(resolve_language("en"), "en");
        assert_eq!(resolve_language("zh"), "zh");
    }

    #[test]
    fn locale_looks_chinese_samples() {
        assert_eq!(locale_looks_chinese("zh_CN.UTF-8"), Some(true));
        assert_eq!(locale_looks_chinese("zh-Hans"), Some(true));
        assert_eq!(locale_looks_chinese("en_US"), Some(false));
        assert_eq!(locale_looks_chinese("ja_JP"), Some(false));
        assert_eq!(locale_looks_chinese("C"), None);
        assert_eq!(locale_looks_chinese(""), None);
    }
}
