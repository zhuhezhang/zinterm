//! SSH session manager.
//!
//! Each open terminal tab maps to exactly one `SshSession`. The session runs
//! on the shared Tokio runtime; commands come in via an MPSC channel and
//! output lines are pushed back via an `UnboundedSender<SessionEvent>`.

// Re-exports preserve `crate::ssh::*`. Some names are only used from tests or
// sibling modules, which `cargo check` (without `--tests`) does not see.
#![allow(unused_imports)]

#[path = "auth.rs"]
mod auth;
#[path = "display.rs"]
mod display;
#[path = "keys.rs"]
mod keys;
#[path = "osc.rs"]
mod osc;
#[path = "prompt_setup.rs"]
mod prompt_setup;
#[path = "session.rs"]
mod session;
#[path = "transport.rs"]
mod transport;

pub(crate) use auth::{
    authenticate_session, resolve_credentials, verify_host_key, AuthResult, ClientHandler,
};
pub use display::{format_mtime, format_size};
pub(crate) use keys::load_session_private_key;
pub use osc::{extract_osc7_path, extract_osc_command, repair_fc_newlines};
pub use session::spawn_session;
pub(crate) use transport::{
    connect_transport, ssh_client_config_with_algorithms, ssh_legacy_client_config, LEGACY_CIPHER,
    LEGACY_COMPRESSION, LEGACY_KEX, LEGACY_KEY, LEGACY_MAC,
};

#[cfg(test)]
pub(crate) use transport::legacy_ssh_compat_tests;
