use super::*;

pub(super) fn hostkey_dialog_text(
    host: &str,
    port: u16,
    key_type: &str,
    fingerprint: &str,
    changed: bool,
) -> (String, String, String, String, String) {
    let detail = format!("{host}:{port}  ({key_type})\n{fingerprint}");
    let once_label = crate::i18n::t("仅信任一次", "Trust once").to_string();
    if changed {
        (
            crate::i18n::t("⚠ 主机密钥已改变", "⚠ Host key changed").to_string(),
            crate::i18n::t(
                "该主机的密钥与之前记录的不一致,可能存在中间人攻击。仅当你确知服务器密钥已更换时才继续。",
                "This host's key differs from the one stored earlier — this could be a man-in-the-middle attack. Only continue if you know the server's key really changed.",
            )
            .to_string(),
            detail,
            crate::i18n::t("仍然信任", "Trust anyway").to_string(),
            once_label,
        )
    } else {
        (
            crate::i18n::t("未知主机", "Unknown host").to_string(),
            crate::i18n::t(
                "首次连接该主机。请核对下面的密钥指纹,确认无误后再信任并连接。",
                "First time connecting to this host. Verify the key fingerprint below before you trust and connect.",
            )
            .to_string(),
            detail,
            crate::i18n::t("信任并连接", "Trust & connect").to_string(),
            once_label,
        )
    }
}

/// Queue a host-key prompt: answer immediately if already decided this run,
/// merge into an existing pending entry for the same host, otherwise enqueue
/// (and show it now if nothing else is up).
pub(super) fn enqueue_hostkey_prompt(
    win: &AppWindow,
    host: String,
    port: u16,
    key_type: String,
    fingerprint: String,
    changed: bool,
    responder: crate::ssh::HostKeyResponder,
) {
    let id = hostkey_cache_id(&host, port);
    if let Some(ans) = HOSTKEY_DECIDED.with(|d| d.borrow().get(&id).copied()) {
        responder.respond(ans);
        return;
    }
    let show_now = HOSTKEY_QUEUE.with(|q| {
        let mut q = q.borrow_mut();
        if let Some(p) = q.iter_mut().find(|p| p.host == host && p.port == port) {
            p.responders.push(responder);
            return false;
        }
        let was_empty = q.is_empty();
        let (title, message, detail, confirm_label, once_label) =
            hostkey_dialog_text(&host, port, &key_type, &fingerprint, changed);
        q.push_back(PendingHostKey {
            host,
            port,
            changed,
            title,
            message,
            detail,
            confirm_label,
            once_label,
            responders: vec![responder],
        });
        was_empty
    });
    if show_now {
        show_front_hostkey(win);
    }
}

/// Drop in-memory host-key accepts so the next connect re-prompts after a
/// settings "clear known hosts" (disk wipe alone is not enough — see #152-adjacent).
pub(super) fn clear_hostkey_decisions() {
    HOSTKEY_DECIDED.with(|d| d.borrow_mut().clear());
}

/// Drop a temporary `AcceptOnce` entry for `host:port` (after SFTP finishes, or
/// when no follow-up SFTP handshake will reuse it).
pub(super) fn clear_hostkey_once(host: &str, port: u16) {
    use crate::ssh::HostKeyDecision;
    let id = format!("{}:{}", host.trim().to_lowercase(), port);
    HOSTKEY_DECIDED.with(|d| {
        let mut d = d.borrow_mut();
        if matches!(d.get(&id), Some(HostKeyDecision::AcceptOnce)) {
            d.remove(&id);
        }
    });
}

fn hostkey_cache_id(host: &str, port: u16) -> String {
    format!("{}:{}", host.trim().to_lowercase(), port)
}

/// Push the front pending prompt's details into the window and open the dialog.
pub(super) fn show_front_hostkey(win: &AppWindow) {
    HOSTKEY_QUEUE.with(|q| {
        if let Some(p) = q.borrow().front() {
            win.set_hostkey_changed(p.changed);
            win.set_hostkey_title(p.title.clone().into());
            win.set_hostkey_message(p.message.clone().into());
            win.set_hostkey_detail(p.detail.clone().into());
            win.set_hostkey_confirm_label(p.confirm_label.clone().into());
            win.set_hostkey_once_label(p.once_label.clone().into());
            win.set_hostkey_prompt_open(true);
        }
    });
}

/// Apply the user's decision to the front prompt, then show the next one (or
/// close the dialog if the queue is now empty).
pub(super) fn resolve_front_hostkey(win: &AppWindow, decision: crate::ssh::HostKeyDecision) {
    let has_next = HOSTKEY_QUEUE.with(|q| {
        let mut q = q.borrow_mut();
        if let Some(p) = q.pop_front() {
            // Cache accepts for this run so a slightly-later SFTP handshake to
            // the same host (after shell Connected) is answered without a second
            // dialog. AcceptOnce is temporary — cleared once SFTP finishes (or
            // when no SFTP will follow). Reject is never cached (#152).
            if decision.accepted() {
                HOSTKEY_DECIDED.with(|d| {
                    d.borrow_mut()
                        .insert(hostkey_cache_id(&p.host, p.port), decision);
                });
            }
            for r in &p.responders {
                r.respond(decision);
            }
        }
        !q.is_empty()
    });
    if has_next {
        show_front_hostkey(win);
    } else {
        win.set_hostkey_prompt_open(false);
    }
}

// ---------------------------------------------------------------------------
// Connect-time credential prompt (#110)
// ---------------------------------------------------------------------------

thread_local! {
    static CRED_QUEUE: RefCell<VecDeque<PendingCred>> = RefCell::new(VecDeque::new());
    /// tab id → accepted credentials for that tab. Shared by shell + SFTP on the
    /// same tab; survives disconnect so R-reconnect can reuse it; copied (not
    /// shared) when duplicating a tab. Cleared only when the tab is closed.
    /// Cancels are intentionally *not* cached — same rationale as host-key
    /// reject (#152).
    static CRED_DECIDED: RefCell<HashMap<String, crate::ssh::CredentialReply>> =
        RefCell::new(HashMap::new());
    /// Session ids opened via "Connect without saving". Credential prompts for
    /// these must not write back to the saved session even when save-passwords
    /// is on (and even if the draft reused an existing session id while editing).
    static EPHEMERAL_SESSIONS: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
}

/// Mark a session as ephemeral for this run (connect without saving).
pub(super) fn mark_session_ephemeral(session_id: &str) {
    EPHEMERAL_SESSIONS.with(|s| {
        s.borrow_mut().insert(session_id.to_string());
    });
}

/// Drop the ephemeral mark (e.g. after the same id is saved/persisted).
pub(super) fn clear_session_ephemeral(session_id: &str) {
    EPHEMERAL_SESSIONS.with(|s| {
        s.borrow_mut().remove(session_id);
    });
}

fn is_ephemeral_session(session_id: &str) -> bool {
    EPHEMERAL_SESSIONS.with(|s| s.borrow().contains(session_id))
}

/// Drop any accepted credentials cached for this tab (tab close only).
pub(super) fn clear_tab_credentials(tab_id: &str) {
    CRED_DECIDED.with(|d| {
        d.borrow_mut().remove(tab_id);
    });
}

/// Copy one tab's accepted credentials onto another (e.g. Duplicate connection).
/// The destination keeps an independent entry — later clears do not affect the source.
pub(super) fn copy_tab_credentials(from_tab: &str, to_tab: &str) {
    let cred = CRED_DECIDED.with(|d| d.borrow().get(from_tab).cloned());
    if let Some(cred) = cred {
        CRED_DECIDED.with(|d| {
            d.borrow_mut().insert(to_tab.to_string(), cred);
        });
    }
}

/// For reconnect (R) / duplicate: prefer this tab's in-memory credential cache.
/// If none is cached, clear the session login password / key passphrase so we
/// do **not** fall back to whatever may be stored on disk — the UI will prompt
/// again (password auth, or key auth when the key is encrypted / missing).
pub(super) fn apply_cached_credentials_for_reconnect(
    session: &mut crate::config::Session,
    tab_id: &str,
) {
    use crate::config::AuthMethod;
    if let Some(cred) = CRED_DECIDED.with(|d| d.borrow().get(tab_id).cloned()) {
        if !cred.user.trim().is_empty() {
            session.user = cred.user;
        }
        match session.auth {
            AuthMethod::Key => {
                session.key_passphrase = crate::config::Secret::new(cred.secret);
                if !cred.private_key.trim().is_empty() {
                    session.private_key = crate::config::Secret::new(cred.private_key);
                }
            }
            _ => {
                session.password = crate::config::Secret::new(cred.secret);
            }
        }
    } else {
        match session.auth {
            AuthMethod::Key => {
                session.key_passphrase = crate::config::Secret::default();
            }
            _ => {
                session.password = crate::config::Secret::default();
            }
        }
    }
}

/// Queue a credential prompt: answer immediately if this tab already accepted
/// credentials, merge into an existing pending entry for the same tab
/// (shell + SFTP), otherwise enqueue (and show it now if nothing else is up).
pub(super) fn enqueue_cred_prompt(
    win: &AppWindow,
    tab_id: String,
    session_id: String,
    host: String,
    auth: String,
    user: String,
    password: String,
    private_key: String,
    need_user: bool,
    need_password: bool,
    responder: crate::ssh::CredentialResponder,
) {
    if let Some(reply) = CRED_DECIDED.with(|d| d.borrow().get(&tab_id).cloned()) {
        responder.respond(Some(reply));
        return;
    }
    let show_now = CRED_QUEUE.with(|q| {
        let mut q = q.borrow_mut();
        if let Some(p) = q.iter_mut().find(|p| p.tab_id == tab_id) {
            p.responders.push(responder);
            return false;
        }
        let was_empty = q.is_empty();
        q.push_back(PendingCred {
            tab_id,
            session_id,
            host,
            auth,
            user,
            password,
            private_key,
            need_user,
            need_password,
            responders: vec![responder],
        });
        was_empty
    });
    if show_now {
        show_front_cred(win);
    }
}

/// Populate the credential dialog from the front prompt and open it.
pub(super) fn show_front_cred(win: &AppWindow) {
    CRED_QUEUE.with(|q| {
        if let Some(p) = q.borrow().front() {
            win.set_cred_host(p.host.clone().into());
            win.set_cred_auth(p.auth.clone().into());
            win.set_cred_need_user(p.need_user);
            win.set_cred_need_password(p.need_password);
            win.set_cred_user(p.user.clone().into());
            // Prefill existing password / passphrase so the field stays usable.
            win.set_cred_password(p.password.clone().into());
            win.set_cred_key_inline(p.private_key.clone().into());
            win.set_cred_prompt_open(true);
        }
    });
}

/// Apply the user's answer to the front credential prompt (or cancel).
/// Non-ephemeral sessions always persist the username; the password / key
/// material is written only when Settings › Data › save passwords is on.
/// "Connect without saving" (ephemeral) never writes back.
pub(super) fn resolve_front_cred(win: &AppWindow, accept: bool) {
    let reply: Option<crate::ssh::CredentialReply> = if accept {
        Some(crate::ssh::CredentialReply {
            user: win.get_cred_user().to_string(),
            secret: win.get_cred_password().to_string(),
            private_key: win.get_cred_key_inline().to_string(),
        })
    } else {
        None
    };
    let has_next = CRED_QUEUE.with(|q| {
        let mut q = q.borrow_mut();
        if let Some(p) = q.pop_front() {
            // Only cache an *accept* for this tab (shell + SFTP share one dialog).
            // A cancel must not poison later reconnects with "login cancelled".
            if let Some(ref accepted) = reply {
                CRED_DECIDED.with(|d| {
                    d.borrow_mut()
                        .insert(p.tab_id.clone(), accepted.clone());
                });
                // Skip write-back for "Connect without saving".
                if !is_ephemeral_session(&p.session_id) {
                    let save_password = HISTORY_STORE.with(|s| {
                        s.borrow()
                            .as_ref()
                            .map(|store| store.borrow().save_passwords())
                            .unwrap_or(false)
                    });
                    persist_credentials(
                        &p.session_id,
                        &p.auth,
                        &accepted.user,
                        &accepted.secret,
                        &accepted.private_key,
                        true,
                        save_password,
                    );
                }
            }
            for r in &p.responders {
                r.respond(reply.clone());
            }
        }
        !q.is_empty()
    });
    // Don't leave typed secrets lingering in the UI properties.
    win.set_cred_password("".into());
    win.set_cred_key_inline("".into());
    if has_next {
        show_front_cred(win);
    } else {
        win.set_cred_prompt_open(false);
    }
}

/// Persist newly-entered credentials onto the saved session (#110).
///
/// Username is always written back for non-ephemeral sessions. Login password
/// / private key / key passphrase are written only when `set_password` is true
/// (Settings › Data › 保存密码/私钥). Same rule for password auth and key auth
/// so a welcome-page credential prompt honors the switch either way.
pub(super) fn persist_credentials(
    session_id: &str,
    auth: &str,
    user: &str,
    secret: &str,
    private_key: &str,
    set_user: bool,
    set_password: bool,
) {
    HISTORY_STORE.with(|s| {
        if let Some(store) = s.borrow().as_ref() {
            let mut st = store.borrow_mut();
            if let Some(mut sess) = st.get(session_id).cloned() {
                let mut changed = false;
                if set_user && !user.trim().is_empty() && sess.user != user.trim() {
                    sess.user = user.trim().to_string();
                    changed = true;
                }
                if set_password {
                    if auth == "key" || matches!(sess.auth, crate::config::AuthMethod::Key) {
                        sess.key_passphrase = crate::config::Secret::new(secret.to_string());
                        let key = private_key.trim();
                        if !key.is_empty() {
                            sess.private_key = if crate::config::looks_like_private_key_content(key)
                            {
                                crate::config::Secret::new(key.to_string())
                            } else {
                                crate::config::Secret::new(key.replace('\\', "/"))
                            };
                        }
                    } else {
                        sess.password = crate::config::Secret::new(secret.to_string());
                    }
                    changed = true;
                }
                if changed {
                    st.upsert(sess);
                    let _ = st.save();
                }
            }
        }
    });
}

// ---------------------------------------------------------------------------
// Split panes (v0.5)
// ---------------------------------------------------------------------------
