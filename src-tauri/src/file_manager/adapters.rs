use super::types::PeerBrowserError;
use crate::{
    AppError, PeerListing, PeerManager, PreviewPayload,
    cloud::{object_runtime_types::ObjectPageDto, runtime::CloudRuntime},
};

pub async fn cloud_list(
    runtime: &CloudRuntime,
    connection: &str,
    prefix: &str,
    cursor: Option<&str>,
) -> Result<ObjectPageDto, AppError> {
    runtime.object_list(connection, prefix, cursor).await
}
pub async fn cloud_preview(
    runtime: &CloudRuntime,
    connection: &str,
    key: &str,
) -> Result<PreviewPayload, AppError> {
    runtime.object_preview(connection, key).await
}
pub async fn peer_list(
    peers: &PeerManager,
    peer: &str,
    share: &str,
    path: &str,
) -> Result<PeerListing, PeerBrowserError> {
    peers
        .list_dir(peer, share, path)
        .await
        .map_err(PeerBrowserError::from)
}
pub async fn peer_preview(
    peers: &PeerManager,
    peer: &str,
    share: &str,
    path: &str,
) -> Result<PreviewPayload, PeerBrowserError> {
    peers
        .preview(peer, share, path)
        .await
        .map_err(PeerBrowserError::from)
}

impl From<PeerBrowserError> for AppError {
    fn from(error: PeerBrowserError) -> Self {
        match error {
            PeerBrowserError::Peer(source) => source.into(),
        }
    }
}
