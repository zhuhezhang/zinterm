/// Result of checking a server key against the known-hosts store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostKeyStatus {
    Unknown,
    Match,
    Changed,
}

/// User's answer to a host-key confirmation prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostKeyDecision {
    /// Abort the connection.
    Reject,
    /// Allow this attempt only — do not write known_hosts.
    AcceptOnce,
    /// Allow and persist the fingerprint to known_hosts.
    AcceptRemember,
}

impl HostKeyDecision {
    pub fn accepted(self) -> bool {
        matches!(
            self,
            HostKeyDecision::AcceptOnce | HostKeyDecision::AcceptRemember
        )
    }

    pub fn remember(self) -> bool {
        matches!(self, HostKeyDecision::AcceptRemember)
    }
}
