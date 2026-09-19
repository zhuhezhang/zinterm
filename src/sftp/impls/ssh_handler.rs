use russh::client::Handler;
use russh::keys::PublicKey;
use tokio::sync::mpsc::UnboundedSender;

use anyhow::Result;

use crate::config::Session;
use crate::ssh::SessionEvent;

pub(super) struct SftpClientHandler {
    host: String,
    port: u16,
    events: UnboundedSender<SessionEvent>,
}

pub(super) fn sftp_handler(
    session: &Session,
    events: &UnboundedSender<SessionEvent>,
) -> SftpClientHandler {
    SftpClientHandler {
        host: session.host.clone(),
        port: session.port,
        events: events.clone(),
    }
}

impl Handler for SftpClientHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &PublicKey,
    ) -> Result<bool, Self::Error> {
        Ok(
            crate::ssh::verify_host_key(&self.host, self.port, server_public_key, &self.events)
                .await,
        )
    }
}
