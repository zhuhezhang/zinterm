use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use russh::client::{Handle, Handler, NegotiatedAlgorithms};
use russh::keys::{HashAlg, PrivateKeyWithHashAlg, PublicKey};
use tokio::sync::mpsc::UnboundedSender;

use crate::config::{AuthMethod, Session};

use super::super::structs::*;
use super::keys::load_session_private_key;

/// Outcome of authenticating an SSH session, so callers can distinguish a user
/// cancel from a credential rejection and word the status line accordingly.
pub(crate) enum AuthResult {
    Success,
    Cancelled,
    Failed,
}

/// Authenticate an already-connected SSH handle using the session's method,
/// prompting for missing credentials (#110). Shared by the shell and SFTP paths.
pub(crate) async fn authenticate_session(
    handle: &mut Handle<ClientHandler>,
    session: &Session,
    events: &UnboundedSender<SessionEvent>,
) -> Result<AuthResult> {
    let creds = match resolve_credentials(session, events).await {
        Some(c) => c,
        None => return Ok(AuthResult::Cancelled),
    };

    let authed = match session.auth {
        AuthMethod::Password => handle
            .authenticate_password(&creds.user, creds.secret.as_str())
            .await
            .context("password auth failed")?
            .success(),
        AuthMethod::Key => {
            // Prefer dialog-supplied key / passphrase over the saved session
            // values so a connect-time prompt can fill blanks (#110 / key auth).
            let mut key_session = session.clone();
            if !creds.private_key.trim().is_empty() {
                key_session.private_key = crate::config::Secret::new(creds.private_key.clone());
            }
            let keypair = load_session_private_key(&key_session, creds.secret.as_str())?;
            // RSA keys must be signed with an explicit SHA-2 hash; every other
            // key type carries its own algorithm, so no override is needed.
            let hash = keypair.algorithm().is_rsa().then_some(HashAlg::Sha256);
            let key_with_hash = PrivateKeyWithHashAlg::new(Arc::new(keypair), hash);
            handle
                .authenticate_publickey(&creds.user, key_with_hash)
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

/// Client handler. Verifies the server host key against the known_hosts store,
/// prompting the user on first contact / on a changed key (#109-5).
pub(crate) struct ClientHandler {
    pub(crate) host: String,
    pub(crate) port: u16,
    pub(crate) events: UnboundedSender<SessionEvent>,
    /// Filled by [`Handler::algorithms_negotiated`] after the initial KEX.
    pub(crate) negotiated: Arc<Mutex<Option<NegotiatedAlgorithms>>>,
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
                Ok(decision) if decision.accepted() => {
                    if decision.remember() {
                        if let Err(e) = crate::ssh::known_hosts::remember(host, port, key) {
                            tracing::warn!("could not save host key for {host}:{port}: {e:#}");
                        }
                    }
                    true
                }
                _ => false,
            }
        }
    }
}

/// Resolve a session's username/secret/private key, prompting the UI for
/// whatever is missing (#110). Returns the effective credentials, or `None` if
/// the user cancelled. Both the shell and SFTP connections call this; the UI
/// de-duplicates by tab id so a single dialog serves both. A dropped reply
/// channel (no UI) falls through with the stored values so auth fails normally.
pub(crate) async fn resolve_credentials(
    session: &Session,
    events: &UnboundedSender<SessionEvent>,
) -> Option<CredentialReply> {
    let user = session.user.trim().to_string();
    let is_key = matches!(session.auth, AuthMethod::Key);
    let secret = if is_key {
        session.key_passphrase.as_str().to_string()
    } else {
        session.password.as_str().to_string()
    };
    let private_key = if is_key {
        session.private_key.as_str().to_string()
    } else {
        String::new()
    };
    let need_user = user.is_empty();
    let need_password = if is_key {
        // Missing key material always prompts. An empty passphrase only prompts
        // when the key looks encrypted (unencrypted keys connect without a
        // dialog). Password auth still prompts whenever the login password is
        // blank.
        if private_key.trim().is_empty() {
            true
        } else if secret.is_empty() {
            load_session_private_key(session, "").is_err()
        } else {
            false
        }
    } else {
        secret.is_empty()
    };
    if !(need_user || need_password) {
        return Some(CredentialReply {
            user,
            secret,
            private_key,
        });
    }
    let (tx, rx) = tokio::sync::oneshot::channel();
    let sent = events.send(SessionEvent::CredentialPrompt {
        session_id: session.id.clone(),
        host: session.host.clone(),
        auth: session.auth.as_str().to_string(),
        user: user.clone(),
        password: secret.clone(),
        private_key: private_key.clone(),
        need_user,
        need_password,
        responder: CredentialResponder::new(tx),
    });
    if sent.is_err() {
        return Some(CredentialReply {
            user,
            secret,
            private_key,
        });
    }
    match rx.await {
        Ok(Some(reply)) => Some(CredentialReply {
            user: reply.user.trim().to_string(),
            secret: reply.secret,
            private_key: reply.private_key,
        }),
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

    async fn algorithms_negotiated(
        &mut self,
        algorithms: &NegotiatedAlgorithms,
    ) -> Result<(), Self::Error> {
        if let Ok(mut slot) = self.negotiated.lock() {
            *slot = Some(algorithms.clone());
        }
        Ok(())
    }
}

// Marker trait impl so `Arc<Handle<Handler>>` is nameable in external code.
#[allow(dead_code)]
fn _assert_handle_send() {
    fn takes<T: Send>() {}
    takes::<Handle<ClientHandler>>();
}
