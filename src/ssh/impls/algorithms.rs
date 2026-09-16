//! SSH algorithm catalog, sanitize helpers, and russh Preferred conversion.

use std::borrow::Cow;
use std::collections::HashSet;
use std::str::FromStr;

use russh::keys::{Algorithm, HashAlg};
use russh::Preferred;

use crate::config::AlgorithmPreferences;

/// Category keys used by settings UI and IPC-style helpers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlgorithmCategory {
    Kex,
    ServerHostKey,
    Cipher,
    Hmac,
    Compress,
}

impl AlgorithmCategory {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Kex => "kex",
            Self::ServerHostKey => "serverHostKey",
            Self::Cipher => "cipher",
            Self::Hmac => "hmac",
            Self::Compress => "compress",
        }
    }

    pub fn from_str_key(s: &str) -> Option<Self> {
        match s {
            "kex" => Some(Self::Kex),
            "serverHostKey" | "server_host_key" => Some(Self::ServerHostKey),
            "cipher" => Some(Self::Cipher),
            "hmac" => Some(Self::Hmac),
            "compress" => Some(Self::Compress),
            _ => None,
        }
    }

    pub fn all() -> [Self; 5] {
        [
            Self::Kex,
            Self::ServerHostKey,
            Self::Cipher,
            Self::Hmac,
            Self::Compress,
        ]
    }
}

#[derive(Debug, Clone, Copy)]
struct AlgoDef {
    name: &'static str,
    /// Safe enough not to show the "weak" badge.
    safe: bool,
    /// Selected when resetting to defaults.
    is_default: bool,
}

const fn def(name: &'static str) -> AlgoDef {
    AlgoDef {
        name,
        safe: true,
        is_default: true,
    }
}
const fn weak_default(name: &'static str) -> AlgoDef {
    AlgoDef {
        name,
        safe: false,
        is_default: true,
    }
}
const fn legacy(name: &'static str) -> AlgoDef {
    AlgoDef {
        name,
        safe: false,
        is_default: false,
    }
}
const fn opt_in_safe(name: &'static str) -> AlgoDef {
    AlgoDef {
        name,
        safe: true,
        is_default: false,
    }
}

/// russh-supported option pool (UI whitelist). Order = display order for
/// unselected rows; selected rows keep the user's preference order.
const KEX_CATALOG: &[AlgoDef] = &[
    def("curve25519-sha256"),
    def("curve25519-sha256@libssh.org"),
    def("diffie-hellman-group16-sha512"),
    def("diffie-hellman-group14-sha256"),
    def("diffie-hellman-group-exchange-sha256"),
    def("ecdh-sha2-nistp256"),
    def("ecdh-sha2-nistp384"),
    def("ecdh-sha2-nistp521"),
    opt_in_safe("diffie-hellman-group18-sha512"),
    opt_in_safe("diffie-hellman-group15-sha512"),
    opt_in_safe("diffie-hellman-group17-sha512"),
    weak_default("diffie-hellman-group14-sha1"),
    weak_default("diffie-hellman-group1-sha1"),
    legacy("diffie-hellman-group-exchange-sha1"),
];

const HOST_KEY_CATALOG: &[AlgoDef] = &[
    def("ssh-ed25519"),
    def("ecdsa-sha2-nistp256"),
    def("ecdsa-sha2-nistp384"),
    def("ecdsa-sha2-nistp521"),
    def("rsa-sha2-512"),
    def("rsa-sha2-256"),
    weak_default("ssh-rsa"),
    legacy("ssh-dss"),
];

const CIPHER_CATALOG: &[AlgoDef] = &[
    def("chacha20-poly1305@openssh.com"),
    def("aes256-gcm@openssh.com"),
    def("aes256-ctr"),
    def("aes192-ctr"),
    def("aes128-ctr"),
    weak_default("aes256-cbc"),
    weak_default("aes192-cbc"),
    weak_default("aes128-cbc"),
    weak_default("3des-cbc"),
];

const HMAC_CATALOG: &[AlgoDef] = &[
    def("hmac-sha2-512-etm@openssh.com"),
    def("hmac-sha2-256-etm@openssh.com"),
    def("hmac-sha2-512"),
    def("hmac-sha2-256"),
    def("hmac-sha1-etm@openssh.com"),
    def("hmac-sha1"),
    weak_default("hmac-md5"),
];

const COMPRESS_CATALOG: &[AlgoDef] = &[
    def("none"),
    opt_in_safe("zlib@openssh.com"),
    opt_in_safe("zlib"),
];

fn catalog(category: AlgorithmCategory) -> &'static [AlgoDef] {
    match category {
        AlgorithmCategory::Kex => KEX_CATALOG,
        AlgorithmCategory::ServerHostKey => HOST_KEY_CATALOG,
        AlgorithmCategory::Cipher => CIPHER_CATALOG,
        AlgorithmCategory::Hmac => HMAC_CATALOG,
        AlgorithmCategory::Compress => COMPRESS_CATALOG,
    }
}

pub fn option_pool(category: AlgorithmCategory) -> Vec<&'static str> {
    catalog(category).iter().map(|a| a.name).collect()
}

pub fn is_weak_algorithm(category: AlgorithmCategory, name: &str) -> bool {
    catalog(category)
        .iter()
        .find(|a| a.name == name)
        .map(|a| !a.safe)
        .unwrap_or(false)
}

pub fn default_selection_for(category: AlgorithmCategory) -> Vec<String> {
    catalog(category)
        .iter()
        .filter(|a| a.is_default)
        .map(|a| a.name.to_string())
        .collect()
}

fn list_mut(prefs: &mut AlgorithmPreferences, category: AlgorithmCategory) -> &mut Vec<String> {
    match category {
        AlgorithmCategory::Kex => &mut prefs.kex,
        AlgorithmCategory::ServerHostKey => &mut prefs.server_host_key,
        AlgorithmCategory::Cipher => &mut prefs.cipher,
        AlgorithmCategory::Hmac => &mut prefs.hmac,
        AlgorithmCategory::Compress => &mut prefs.compress,
    }
}

fn list_ref(prefs: &AlgorithmPreferences, category: AlgorithmCategory) -> &[String] {
    match category {
        AlgorithmCategory::Kex => &prefs.kex,
        AlgorithmCategory::ServerHostKey => &prefs.server_host_key,
        AlgorithmCategory::Cipher => &prefs.cipher,
        AlgorithmCategory::Hmac => &prefs.hmac,
        AlgorithmCategory::Compress => &prefs.compress,
    }
}

/// Drop unknown names; if a non-empty list becomes empty after filtering, fall
/// back to that category's defaults. Explicit `[]` is preserved only for
/// compress (mapped to `none` later); other empty lists become defaults.
pub fn sanitize_algorithm_preferences(prefs: &AlgorithmPreferences) -> AlgorithmPreferences {
    let mut out = AlgorithmPreferences::builtin_default();
    for cat in AlgorithmCategory::all() {
        let raw = list_ref(prefs, cat);
        let pool: HashSet<&str> = option_pool(cat).into_iter().collect();
        let mut unique = Vec::new();
        let mut seen = HashSet::new();
        for name in raw {
            if pool.contains(name.as_str()) && seen.insert(name.clone()) {
                unique.push(name.clone());
            }
        }
        *list_mut(&mut out, cat) = if !unique.is_empty() {
            unique
        } else if raw.is_empty() && cat == AlgorithmCategory::Compress {
            Vec::new()
        } else {
            default_selection_for(cat)
        };
    }
    if out.compress.is_empty() {
        out.compress = vec!["none".to_string()];
    }
    out
}

pub fn toggle_algorithm(
    prefs: &mut AlgorithmPreferences,
    category: AlgorithmCategory,
    name: &str,
) {
    let pool = option_pool(category);
    if !pool.iter().any(|n| *n == name) {
        return;
    }
    let list = list_mut(prefs, category);
    if let Some(i) = list.iter().position(|n| n == name) {
        // Refuse clearing the last entry for required categories.
        if list.len() == 1 && category != AlgorithmCategory::Compress {
            return;
        }
        list.remove(i);
        if category == AlgorithmCategory::Compress && list.is_empty() {
            list.push("none".to_string());
        }
    } else {
        list.push(name.to_string());
    }
}

pub fn move_algorithm(
    prefs: &mut AlgorithmPreferences,
    category: AlgorithmCategory,
    name: &str,
    delta: i32,
) {
    let list = list_mut(prefs, category);
    let Some(i) = list.iter().position(|n| n == name) else {
        return;
    };
    let j = i as i32 + delta;
    if j < 0 || j as usize >= list.len() {
        return;
    }
    list.swap(i, j as usize);
}

pub fn reset_algorithm_section(prefs: &mut AlgorithmPreferences, category: AlgorithmCategory) {
    *list_mut(prefs, category) = default_selection_for(category);
}

pub fn reset_all_algorithms(prefs: &mut AlgorithmPreferences) {
    *prefs = AlgorithmPreferences::builtin_default();
}

fn parse_kex(name: &str) -> Option<russh::kex::Name> {
    russh::kex::Name::try_from(name).ok()
}

fn parse_cipher(name: &str) -> Option<russh::cipher::Name> {
    russh::cipher::Name::try_from(name).ok()
}

fn parse_mac(name: &str) -> Option<russh::mac::Name> {
    russh::mac::Name::try_from(name).ok()
}

fn parse_compress(name: &str) -> Option<russh::compression::Name> {
    russh::compression::Name::try_from(name).ok()
}

fn parse_host_key(name: &str) -> Option<Algorithm> {
    Algorithm::from_str(name).ok().or_else(|| match name {
        "ssh-rsa" => Some(Algorithm::Rsa { hash: None }),
        "rsa-sha2-256" => Some(Algorithm::Rsa {
            hash: Some(HashAlg::Sha256),
        }),
        "rsa-sha2-512" => Some(Algorithm::Rsa {
            hash: Some(HashAlg::Sha512),
        }),
        _ => None,
    })
}

/// Build a russh Preferred list. Appends client-only KEX extension markers.
pub fn to_preferred(prefs: &AlgorithmPreferences) -> Preferred {
    let prefs = sanitize_algorithm_preferences(prefs);

    let mut kex: Vec<russh::kex::Name> = prefs.kex.iter().filter_map(|n| parse_kex(n)).collect();
    // Client must advertise these; never expose in the settings UI.
    if !kex
        .iter()
        .any(|n| n.as_ref() == russh::kex::EXTENSION_SUPPORT_AS_CLIENT.as_ref())
    {
        kex.push(russh::kex::EXTENSION_SUPPORT_AS_CLIENT);
    }
    if !kex
        .iter()
        .any(|n| n.as_ref() == russh::kex::EXTENSION_OPENSSH_STRICT_KEX_AS_CLIENT.as_ref())
    {
        kex.push(russh::kex::EXTENSION_OPENSSH_STRICT_KEX_AS_CLIENT);
    }

    let key: Vec<Algorithm> = prefs
        .server_host_key
        .iter()
        .filter_map(|n| parse_host_key(n))
        .collect();
    let cipher: Vec<russh::cipher::Name> = prefs
        .cipher
        .iter()
        .filter_map(|n| parse_cipher(n))
        .collect();
    let mac: Vec<russh::mac::Name> = prefs.hmac.iter().filter_map(|n| parse_mac(n)).collect();
    let mut compress: Vec<russh::compression::Name> = prefs
        .compress
        .iter()
        .filter_map(|n| parse_compress(n))
        .collect();
    if compress.is_empty() {
        compress.push(russh::compression::NONE);
    }

    // Fall back per-category if filtering removed everything (should be rare).
    let kex = if kex
        .iter()
        .any(|n| {
            n.as_ref() != russh::kex::EXTENSION_SUPPORT_AS_CLIENT.as_ref()
                && n.as_ref() != russh::kex::EXTENSION_OPENSSH_STRICT_KEX_AS_CLIENT.as_ref()
        }) {
        kex
    } else {
        return to_preferred(&AlgorithmPreferences::builtin_default());
    };
    let key = if key.is_empty() {
        return to_preferred(&AlgorithmPreferences::builtin_default());
    } else {
        key
    };
    let cipher = if cipher.is_empty() {
        return to_preferred(&AlgorithmPreferences::builtin_default());
    } else {
        cipher
    };
    let mac = if mac.is_empty() {
        return to_preferred(&AlgorithmPreferences::builtin_default());
    } else {
        mac
    };

    Preferred {
        kex: Cow::Owned(kex),
        key: Cow::Owned(key),
        cipher: Cow::Owned(cipher),
        mac: Cow::Owned(mac),
        compression: Cow::Owned(compress),
    }
}

/// UI row: selected algorithms first (preference order), then the rest of the pool.
pub fn algorithm_rows(
    prefs: &AlgorithmPreferences,
    category: AlgorithmCategory,
) -> Vec<AlgorithmRow> {
    let prefs = sanitize_algorithm_preferences(prefs);
    let selected = list_ref(&prefs, category);
    let mut rows = Vec::new();
    for (i, name) in selected.iter().enumerate() {
        rows.push(AlgorithmRow {
            name: name.clone(),
            checked: true,
            weak: is_weak_algorithm(category, name),
            can_move_up: i > 0,
            can_move_down: i + 1 < selected.len(),
        });
    }
    for name in option_pool(category) {
        if selected.iter().any(|s| s == name) {
            continue;
        }
        rows.push(AlgorithmRow {
            name: name.to_string(),
            checked: false,
            weak: is_weak_algorithm(category, name),
            can_move_up: false,
            can_move_down: false,
        });
    }
    rows
}

#[derive(Debug, Clone)]
pub struct AlgorithmRow {
    pub name: String,
    pub checked: bool,
    pub weak: bool,
    pub can_move_up: bool,
    pub can_move_down: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ssh::{COMPAT_CIPHER, COMPAT_KEX};

    #[test]
    fn default_kex_matches_compat_without_extensions() {
        let preferred = to_preferred(&AlgorithmPreferences::builtin_default());
        let names: Vec<&str> = preferred
            .kex
            .iter()
            .map(|n| n.as_ref())
            .filter(|n| *n != "ext-info-c" && !n.starts_with("kex-strict-c"))
            .collect();
        let compat: Vec<&str> = COMPAT_KEX
            .iter()
            .map(|n| n.as_ref())
            .filter(|n| *n != "ext-info-c" && !n.starts_with("kex-strict-c"))
            .collect();
        assert_eq!(names, compat);
    }

    #[test]
    fn default_cipher_matches_compat() {
        let preferred = to_preferred(&AlgorithmPreferences::builtin_default());
        let names: Vec<&str> = preferred.cipher.iter().map(|n| n.as_ref()).collect();
        let compat: Vec<&str> = COMPAT_CIPHER.iter().map(|n| n.as_ref()).collect();
        assert_eq!(names, compat);
    }

    #[test]
    fn sanitize_drops_unknown_and_keeps_order() {
        let mut prefs = AlgorithmPreferences::builtin_default();
        prefs.cipher = vec![
            "nope".into(),
            "aes128-ctr".into(),
            "aes128-ctr".into(),
            "chacha20-poly1305@openssh.com".into(),
        ];
        let clean = sanitize_algorithm_preferences(&prefs);
        assert_eq!(
            clean.cipher,
            vec![
                "aes128-ctr".to_string(),
                "chacha20-poly1305@openssh.com".to_string()
            ]
        );
    }

    #[test]
    fn toggle_refuses_clearing_last_required() {
        let mut prefs = AlgorithmPreferences {
            kex: vec!["curve25519-sha256".into()],
            ..AlgorithmPreferences::builtin_default()
        };
        toggle_algorithm(&mut prefs, AlgorithmCategory::Kex, "curve25519-sha256");
        assert_eq!(prefs.kex, vec!["curve25519-sha256".to_string()]);
    }

    #[test]
    fn custom_prefs_are_not_builtin_default() {
        let mut prefs = AlgorithmPreferences::builtin_default();
        assert!(prefs.is_builtin_default());
        prefs.hmac.retain(|n| n != "hmac-sha1");
        assert!(!prefs.is_builtin_default());
    }

    #[test]
    fn hmac_md5_is_weak_default_and_negotiable() {
        assert!(is_weak_algorithm(AlgorithmCategory::Hmac, "hmac-md5"));
        assert!(default_selection_for(AlgorithmCategory::Hmac)
            .iter()
            .any(|n| n == "hmac-md5"));

        let mut prefs = AlgorithmPreferences::builtin_default();
        prefs.hmac = vec!["hmac-md5".into()];
        let preferred = to_preferred(&prefs);
        let names: Vec<&str> = preferred.mac.iter().map(|n| n.as_ref()).collect();
        assert_eq!(names, vec!["hmac-md5"]);
    }
}
