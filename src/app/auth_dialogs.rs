use super::*;

pub(super) fn hostkey_dialog_text(
    host: &str,
    port: u16,
    key_type: &str,
    fingerprint: &str,
    changed: bool,
) -> (String, String, String, String) {
    let detail = format!("{host}:{port}  ({key_type})\n{fingerprint}");
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
    let id = format!("{host}:{port}");
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
        let (title, message, detail, confirm_label) =
            hostkey_dialog_text(&host, port, &key_type, &fingerprint, changed);
        q.push_back(PendingHostKey {
            host,
            port,
            changed,
            title,
            message,
            detail,
            confirm_label,
            responders: vec![responder],
        });
        was_empty
    });
    if show_now {
        show_front_hostkey(win);
    }
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
            win.set_hostkey_prompt_open(true);
        }
    });
}

/// Apply the user's decision to the front prompt, then show the next one (or
/// close the dialog if the queue is now empty).
pub(super) fn resolve_front_hostkey(win: &AppWindow, accept: bool) {
    let has_next = HOSTKEY_QUEUE.with(|q| {
        let mut q = q.borrow_mut();
        if let Some(p) = q.pop_front() {
            // Only remember an *accept* for this run (so a slightly-later SFTP
            // prompt for the same host is answered without a second dialog). We
            // must NOT cache a reject: a single dismissal — e.g. an accidental
            // backdrop click instead of "Trust & connect" — used to poison the
            // host for the whole session, auto-rejecting every later connect with
            // "Unknown server key" until the app was restarted (#152). A reject now
            // only fails the current attempt; the next connect prompts again.
            if accept {
                HOSTKEY_DECIDED.with(|d| {
                    d.borrow_mut()
                        .insert(format!("{}:{}", p.host, p.port), true);
                });
            }
            for r in &p.responders {
                r.respond(accept);
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
/// If none is cached, clear the session password so we do **not** fall back to
/// whatever may be stored on disk — the UI will prompt again.
pub(super) fn apply_cached_credentials_for_reconnect(
    session: &mut crate::config::Session,
    tab_id: &str,
) {
    if let Some((user, password)) = CRED_DECIDED.with(|d| d.borrow().get(tab_id).cloned()) {
        if !user.trim().is_empty() {
            session.user = user;
        }
        session.password = crate::config::Secret::new(password);
    } else {
        session.password = crate::config::Secret::default();
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
    user: String,
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
            user,
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
            win.set_cred_need_user(p.need_user);
            win.set_cred_need_password(p.need_password);
            win.set_cred_user(p.user.clone().into());
            win.set_cred_password("".into());
            win.set_cred_prompt_open(true);
        }
    });
}

/// Apply the user's answer to the front credential prompt (or cancel). When
/// Settings › Data › save passwords is on — and this is not an ephemeral
/// "connect without saving" session — persist into the saved session, then
/// show the next prompt or close.
pub(super) fn resolve_front_cred(win: &AppWindow, accept: bool) {
    let reply: Option<crate::ssh::CredentialReply> = if accept {
        Some((
            win.get_cred_user().to_string(),
            win.get_cred_password().to_string(),
        ))
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
                if should_persist_credentials(&p.session_id) {
                    persist_credentials(
                        &p.session_id,
                        &accepted.0,
                        &accepted.1,
                        p.need_user,
                        p.need_password,
                    );
                }
            }
            for r in &p.responders {
                r.respond(reply.clone());
            }
        }
        !q.is_empty()
    });
    // Don't leave the typed password lingering in the UI property.
    win.set_cred_password("".into());
    if has_next {
        show_front_cred(win);
    } else {
        win.set_cred_prompt_open(false);
    }
}

fn should_persist_credentials(session_id: &str) -> bool {
    if is_ephemeral_session(session_id) {
        return false;
    }
    HISTORY_STORE.with(|s| {
        s.borrow()
            .as_ref()
            .map(|store| {
                let st = store.borrow();
                st.save_passwords() && st.get(session_id).is_some()
            })
            .unwrap_or(false)
    })
}

/// Persist newly-entered credentials onto the saved session (#110).
pub(super) fn persist_credentials(
    session_id: &str,
    user: &str,
    password: &str,
    set_user: bool,
    set_password: bool,
) {
    HISTORY_STORE.with(|s| {
        if let Some(store) = s.borrow().as_ref() {
            let mut st = store.borrow_mut();
            if let Some(mut sess) = st.get(session_id).cloned() {
                if set_user && !user.trim().is_empty() {
                    sess.user = user.trim().to_string();
                }
                if set_password {
                    sess.password = crate::config::Secret::new(password.to_string());
                }
                st.upsert(sess);
                let _ = st.save();
            }
        }
    });
}

// ---------------------------------------------------------------------------
// Split panes (v0.5)
// ---------------------------------------------------------------------------
