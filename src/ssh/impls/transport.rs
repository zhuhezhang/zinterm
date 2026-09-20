use std::borrow::Cow;
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Context, Result};
use russh::client::{self, Handle, Handler, NegotiatedAlgorithms};
use russh::keys::{Algorithm, EcdsaCurve, HashAlg};
use tokio::sync::mpsc::UnboundedSender;

use crate::config::{AlgorithmPreferences, Session};
use crate::i18n::t;

use super::super::algorithms::{sanitize_algorithm_preferences, to_preferred};
use super::super::structs::*;
use super::auth::ClientHandler;

/// Open an SSH transport handshake (modern algorithms, with legacy retry).
pub(super) async fn connect_ssh_handshake(
    session: &Session,
    events: &UnboundedSender<SessionEvent>,
    keepalive_secs: u32,
    algorithms: &AlgorithmPreferences,
) -> Result<(Handle<ClientHandler>, Arc<client::Config>)> {
    let addr = format!("{}:{}", session.host, session.port);
    let negotiated = Arc::new(Mutex::new(None));
    let negotiated_for_handler = negotiated.clone();
    let (handle, config) = connect_transport(
        &addr,
        keepalive_secs,
        algorithms,
        || client_handler(session, events, negotiated_for_handler.clone()),
        Some(events),
    )
    .await?;
    if is_compact_legacy_config(&config) {
        report_compact_compat_used(events, &negotiated);
    }
    Ok((handle, config))
}

fn client_handler(
    session: &Session,
    events: &UnboundedSender<SessionEvent>,
    negotiated: Arc<Mutex<Option<NegotiatedAlgorithms>>>,
) -> ClientHandler {
    ClientHandler {
        host: session.host.clone(),
        port: session.port,
        events: events.clone(),
        negotiated,
    }
}

fn report_compact_compat_used(
    events: &UnboundedSender<SessionEvent>,
    negotiated: &Mutex<Option<NegotiatedAlgorithms>>,
) {
    let _ = events.send(SessionEvent::Status(
        t(
            "已使用精简兼容配置（首次握手失败后自动降级）",
            "Using compact compatibility profile (automatic fallback after first handshake failed)",
        )
        .into(),
    ));
    if let Ok(guard) = negotiated.lock() {
        if let Some(algos) = guard.as_ref() {
            let _ = events.send(SessionEvent::Status(format!(
                "{} kex={}  host_key={}  cipher={}  mac={}  compression={}",
                t("协商算法:", "Negotiated algorithms:"),
                algos.kex,
                algos.host_key,
                algos.cipher,
                algos.mac,
                algos.compression,
            )));
        }
    }
}

/// Connect with the configured algorithm set, then optionally retry once with a
/// compact RFC-only profile. Legacy retry runs only when preferences are still
/// the built-in default — custom lists are applied strictly.
///
/// When `status` is provided, progress for the compact retry is written to the
/// terminal (shell path). SFTP passes `None` so the shell is not disturbed.
pub(crate) async fn connect_transport<H, F>(
    addr: &str,
    keepalive_secs: u32,
    algorithms: &AlgorithmPreferences,
    make_handler: F,
    status: Option<&UnboundedSender<SessionEvent>>,
) -> Result<(Handle<H>, Arc<client::Config>)>
where
    H: Handler<Error = russh::Error> + Send + 'static,
    F: Fn() -> H,
{
    let algorithms = sanitize_algorithm_preferences(algorithms);
    let allow_legacy = algorithms.is_builtin_default();
    let modern = ssh_client_config_with_algorithms(keepalive_secs, &algorithms);
    match client::connect(modern.clone(), addr, make_handler()).await {
        Ok(handle) => Ok((handle, modern)),
        Err(err) if allow_legacy && should_retry_legacy(&err) => {
            tracing::info!(
                "ssh handshake failed ({err}); retrying {addr} with compact legacy algorithms"
            );
            if let Some(events) = status {
                let _ = events.send(SessionEvent::Status(
                    t(
                        "首次握手失败，正在自动尝试精简兼容配置…",
                        "First handshake failed; automatically trying compact compatibility profile…",
                    )
                    .into(),
                ));
            }
            // Some network gear (Maipu SM3120 and similar) rate-limits or
            // briefly refuses a second TCP handshake if the first KEXINIT
            // made it abort — give the daemon a beat before the compact retry.
            tokio::time::sleep(std::time::Duration::from_millis(400)).await;
            let legacy = ssh_legacy_client_config(keepalive_secs);
            match client::connect(legacy.clone(), addr, make_handler()).await {
                Ok(handle) => Ok((handle, legacy)),
                Err(err2) => Err(anyhow!(err2))
                    .with_context(|| format!("connect {addr} failed (legacy retry after: {err})")),
            }
        }
        Err(err) => Err(anyhow!(err)).with_context(|| format!("connect {addr} failed")),
    }
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

/// After the out-of-band shell probe, some network gear (switches/routers) keep
/// the only allowed session slot busy or reject a second `CHANNEL_OPEN`. The
/// failure reason varies by vendor:
/// - Huawei VRP (S5720): often tears the writer down → `SendError`
/// - Maipu / some VRP: `ConnectFailed` / `AdministrativelyProhibited` /
///   `ResourceShortage`
///
/// Reconnect without the probe (and skip prompt injection) so the interactive
/// shell can use the single session channel.
pub(super) fn should_retry_shell_without_probe(err: &russh::Error) -> bool {
    if should_retry_legacy(err) {
        return true;
    }
    matches!(
        err,
        russh::Error::SendError
            | russh::Error::RequestDenied
            | russh::Error::ChannelOpenFailure(
                russh::ChannelOpenFailure::AdministrativelyProhibited
                    | russh::ChannelOpenFailure::ConnectFailed
                    | russh::ChannelOpenFailure::ResourceShortage
            )
    )
}

// Compact RFC-only set for ancient SSH2 stacks (H3C S3100 / Huawei VRP-3.3 /
// Maipu SM3120). Those daemons often abort when the KEXINIT lists
// `@openssh.com` names or is larger than their fixed parse buffer. Used only
// as a second-attempt fallback. Includes DH-GEX-SHA1 (common on Maipu; also
// a zauterm/libssh2 weak default) and ssh-dss host keys.
pub(crate) const LEGACY_KEX: &[russh::kex::Name] = &[
    russh::kex::DH_GEX_SHA1,
    russh::kex::DH_G14_SHA1,
    russh::kex::DH_G1_SHA1,
];

pub(crate) const LEGACY_CIPHER: &[russh::cipher::Name] = &[
    russh::cipher::AES_128_CBC,
    russh::cipher::AES_256_CBC,
    russh::cipher::TRIPLE_DES_CBC,
    russh::cipher::AES_128_CTR,
];

pub(crate) const LEGACY_MAC: &[russh::mac::Name] = &[
    russh::mac::HMAC_SHA1,
    russh::mac::HMAC_MD5,
    russh::mac::HMAC_SHA256,
];

pub(crate) const LEGACY_KEY: &[Algorithm] = &[
    Algorithm::Rsa { hash: None },
    Algorithm::Rsa {
        hash: Some(HashAlg::Sha256),
    },
    Algorithm::Dsa,
    Algorithm::Ecdsa {
        curve: EcdsaCurve::NistP256,
    },
];

pub(crate) const LEGACY_COMPRESSION: &[russh::compression::Name] = &[russh::compression::NONE];

fn keepalive_interval(secs: u32) -> Option<std::time::Duration> {
    match secs.min(crate::config::SSH_KEEPALIVE_SECS_MAX) {
        0 => None,
        n => Some(std::time::Duration::from_secs(n as u64)),
    }
}

fn legacy_gex_params() -> client::GexParams {
    // Match libssh2's historical request so Maipu / similar GEX-only daemons
    // that still offer 1024-bit moduli can complete KEX.
    client::GexParams::new_relaxed(1024, 2048, 8192).expect("legacy GexParams")
}

fn ssh_config_with_preferred(
    preferred: russh::Preferred,
    compact: bool,
    keepalive_secs: u32,
) -> Arc<client::Config> {
    Arc::new(client::Config {
        // 0 disables keepalive. A positive interval sends
        // `keepalive@openssh.com`; OpenSSH handles it, but some older
        // H3C/VRP stacks drop the TCP session.
        keepalive_interval: keepalive_interval(keepalive_secs),
        // Short, RFC-shaped ident. russh's default (`SSH-2.0-russh_<ver>`) is
        // fine on OpenSSH but some VRP parsers are picky about the software tag.
        client_id: russh::SshId::Standard("SSH-2.0-zinterm".into()),
        preferred,
        // russh's default 2 MiB window is fine on OpenSSH, but Huawei VRP
        // (S3100 / S5720 and similar) drops CHANNEL_OPEN — the session loop
        // then dies and the next open surfaces as `SendError` ("Channel send
        // error"). PuTTY / libssh2-style clients advertise ~64 KiB; use that
        // for every profile so a successful modern KEX still opens a channel.
        window_size: 65_536,
        maximum_packet_size: 16_384,
        gex: if compact {
            legacy_gex_params()
        } else {
            // Softer than upstream's old 3072/8192/8192 default.
            client::GexParams::default()
        },
        ..<_>::default()
    })
}

pub(super) fn is_compact_legacy_config(config: &client::Config) -> bool {
    std::ptr::eq(config.preferred.kex.as_ref(), LEGACY_KEX)
}

pub(crate) fn ssh_client_config_with_algorithms(
    keepalive_secs: u32,
    algorithms: &AlgorithmPreferences,
) -> Arc<client::Config> {
    ssh_config_with_preferred(to_preferred(algorithms), false, keepalive_secs)
}

pub(crate) fn ssh_legacy_client_config(keepalive_secs: u32) -> Arc<client::Config> {
    ssh_config_with_preferred(
        russh::Preferred {
            kex: Cow::Borrowed(LEGACY_KEX),
            cipher: Cow::Borrowed(LEGACY_CIPHER),
            mac: Cow::Borrowed(LEGACY_MAC),
            key: Cow::Borrowed(LEGACY_KEY),
            compression: Cow::Borrowed(LEGACY_COMPRESSION),
        },
        true,
        keepalive_secs,
    )
}

#[cfg(test)]
pub(crate) mod legacy_ssh_compat_tests {
    use std::sync::Arc;

    use russh::client;

    use crate::config::AlgorithmPreferences;

    use super::{
        is_compact_legacy_config, should_retry_legacy, should_retry_shell_without_probe,
        ssh_client_config_with_algorithms, ssh_legacy_client_config, LEGACY_CIPHER, LEGACY_KEX,
        LEGACY_MAC,
    };

    // Reference preferred sets (must stay in sync with
    // AlgorithmPreferences::builtin_default / algorithms catalog defaults).
    // Runtime code builds Preferred via to_preferred().
    //
    // Key-exchange: russh defaults PLUS ecdh-sha2-nistp*, group-exchange-sha256,
    // and legacy diffie-hellman-group{14,1}-sha1 last (#172). Only *client*
    // extension markers (https://github.com/Eugeny/russh/issues/611).
    pub(crate) const COMPAT_KEX: &[russh::kex::Name] = &[
        russh::kex::CURVE25519,
        russh::kex::CURVE25519_PRE_RFC_8731,
        russh::kex::DH_G16_SHA512,
        russh::kex::DH_G14_SHA256,
        russh::kex::DH_GEX_SHA256,
        russh::kex::ECDH_SHA2_NISTP256,
        russh::kex::ECDH_SHA2_NISTP384,
        russh::kex::ECDH_SHA2_NISTP521,
        russh::kex::DH_G14_SHA1, // legacy fallback
        russh::kex::DH_G1_SHA1,  // legacy fallback
        russh::kex::EXTENSION_SUPPORT_AS_CLIENT,
        russh::kex::EXTENSION_OPENSSH_STRICT_KEX_AS_CLIENT,
    ];

    // Ciphers: AEAD/CTR defaults plus legacy CBC for old servers (#172).
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

    fn ssh_client_config(keepalive_secs: u32) -> Arc<client::Config> {
        ssh_client_config_with_algorithms(keepalive_secs, &AlgorithmPreferences::builtin_default())
    }

    fn has_at(name: impl AsRef<str>) -> bool {
        name.as_ref().contains('@')
    }

    #[test]
    fn legacy_algorithm_names_are_rfc_only() {
        assert!(LEGACY_KEX.iter().all(|n| !has_at(n)));
        assert!(LEGACY_CIPHER.iter().all(|n| !has_at(n)));
        assert!(LEGACY_MAC.iter().all(|n| !has_at(n)));
        // Maipu SM3120 / libssh2 parity: DH-GEX-SHA1 must be offered on the
        // compact retry path (fixed group14/group1 alone is not enough).
        let kex: Vec<&str> = LEGACY_KEX.iter().map(|n| n.as_ref()).collect();
        assert!(
            kex.contains(&"diffie-hellman-group-exchange-sha1"),
            "{kex:?}"
        );
        assert!(kex.contains(&"diffie-hellman-group14-sha1"), "{kex:?}");
        assert!(kex.contains(&"diffie-hellman-group1-sha1"), "{kex:?}");
    }

    #[test]
    fn legacy_profile_accepts_small_dh_gex_groups() {
        let compact = ssh_legacy_client_config(0);
        assert_eq!(compact.gex.min_group_size(), 1024);
        assert_eq!(compact.gex.preferred_group_size(), 2048);
        assert_eq!(compact.gex.max_group_size(), 8192);

        let modern = ssh_client_config(0);
        // Modern stays on the OpenSSH-like floor (no 1024).
        assert_eq!(modern.gex.min_group_size(), 2048);
    }

    #[test]
    fn modern_kex_does_not_advertise_server_extension_markers() {
        let names: Vec<&str> = COMPAT_KEX.iter().map(|n| n.as_ref()).collect();
        assert!(!names.iter().any(|n| n.contains("ext-info-s")), "{names:?}");
        assert!(
            !names.iter().any(|n| n.contains("kex-strict-s")),
            "{names:?}"
        );
        assert!(names.contains(&"ext-info-c"));
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
        // Handshake retry must not treat channel-open policy rejects as algo failure.
        assert!(!should_retry_legacy(&russh::Error::ChannelOpenFailure(
            russh::ChannelOpenFailure::AdministrativelyProhibited
        )));
    }

    #[test]
    fn retry_shell_without_probe_on_switch_session_limit() {
        assert!(should_retry_shell_without_probe(
            &russh::Error::ChannelOpenFailure(
                russh::ChannelOpenFailure::AdministrativelyProhibited
            )
        ));
        assert!(should_retry_shell_without_probe(
            &russh::Error::ChannelOpenFailure(russh::ChannelOpenFailure::ResourceShortage)
        ));
        // Maipu S3120 (and similar) rejects the post-probe shell open with
        // SSH_OPEN_CONNECT_FAILED rather than AdministrativelyProhibited.
        assert!(should_retry_shell_without_probe(
            &russh::Error::ChannelOpenFailure(russh::ChannelOpenFailure::ConnectFailed)
        ));
        assert!(should_retry_shell_without_probe(&russh::Error::Disconnect));
        // Huawei VRP often tears the session down instead of sending
        // CHANNEL_OPEN_FAILURE — russh reports SendError ("Channel send error").
        assert!(should_retry_shell_without_probe(&russh::Error::SendError));
        assert!(should_retry_shell_without_probe(
            &russh::Error::RequestDenied
        ));
        assert!(!should_retry_shell_without_probe(
            &russh::Error::ChannelOpenFailure(russh::ChannelOpenFailure::UnknownChannelType)
        ));
        assert!(!should_retry_shell_without_probe(
            &russh::Error::NotAuthenticated
        ));
    }

    #[test]
    fn client_ident_is_short_rfc_string() {
        for config in [ssh_client_config(0), ssh_legacy_client_config(0)] {
            match &config.client_id {
                russh::SshId::Standard(s) => assert_eq!(s, "SSH-2.0-zinterm"),
                russh::SshId::Raw(s) => panic!("expected Standard ident, got raw {s:?}"),
            }
        }
    }

    #[test]
    fn client_profiles_use_vrp_safe_windows() {
        let compact = ssh_legacy_client_config(0);
        assert!(is_compact_legacy_config(&compact));
        assert_eq!(compact.window_size, 65_536);
        assert_eq!(compact.maximum_packet_size, 16_384);

        let modern = ssh_client_config(0);
        assert!(!is_compact_legacy_config(&modern));
        // Modern KEX must still use the VRP-safe window; S5720 connects with
        // current algorithms then fails CHANNEL_OPEN if the window is 2 MiB.
        assert_eq!(modern.window_size, 65_536);
        assert_eq!(modern.maximum_packet_size, 16_384);
    }

    #[test]
    fn keepalive_zero_disables_interval() {
        let off = ssh_client_config(0);
        assert!(off.keepalive_interval.is_none());
        let on = ssh_client_config(30);
        assert_eq!(
            on.keepalive_interval,
            Some(std::time::Duration::from_secs(30))
        );
        let clamped = ssh_legacy_client_config(u32::MAX);
        assert_eq!(
            clamped.keepalive_interval,
            Some(std::time::Duration::from_secs(
                crate::config::SSH_KEEPALIVE_SECS_MAX as u64
            ))
        );
    }
}
