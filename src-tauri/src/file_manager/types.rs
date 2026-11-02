use crate::DomainError;

#[derive(Debug, thiserror::Error)]
pub enum PeerBrowserError {
    #[error(transparent)]
    Peer(#[from] DomainError),
}
