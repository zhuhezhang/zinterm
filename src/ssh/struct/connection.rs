/// Result of checking a server key against the known-hosts store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostKeyStatus {
    Unknown,
    Match,
    Changed,
}
