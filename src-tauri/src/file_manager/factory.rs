use super::{traits::{BrowserBackend, BrowserServiceFactory}, types::*};
use crate::{AppError, PeerManager, cloud::runtime::CloudRuntime};
use futures_util::future::BoxFuture;
use std::sync::Arc;

pub(crate) fn factories() -> Vec<Box<dyn BrowserServiceFactory>> {
    vec![Box::new(OfficialServiceFactory), Box::new(S3ServiceFactory), Box::new(PeerServiceFactory)]
}

impl BrowserServiceFactory for PeerServiceFactory {
    fn create<'a>(&'a self, peers: &'a PeerManager, _: &'a CloudRuntime, location: &'a FileLocation)
        -> BoxFuture<'a, Result<Option<(Arc<dyn BrowserBackend>, String)>, AppError>> {
        Box::pin(async move {
            let FileLocation::Peerdav { peer_id, share_id } = location else { return Ok(None); };
            let authorization = peers.download_scope(peer_id)?;
            if !peers.shares(peer_id).await.map_err(AppError::from)?.iter().any(|share| &share.id == share_id) {
                return Err(super::service::invalid());
            }
            let backend: Arc<dyn BrowserBackend> = Arc::new(PeerBrowserBackend {
                peer_id: peer_id.clone(), share_id: share_id.clone(), authorization,
            });
            Ok(Some((backend, "/".into())))
        })
    }
}

impl BrowserServiceFactory for S3ServiceFactory {
    fn create<'a>(&'a self, _: &'a PeerManager, cloud: &'a CloudRuntime, location: &'a FileLocation)
        -> BoxFuture<'a, Result<Option<(Arc<dyn BrowserBackend>, String)>, AppError>> {
        Box::pin(async move {
            let (connection_id, bucket) = match location {
                FileLocation::Cloud { connection_id, root_id } => {
                    let connection = cloud.connection(connection_id)?;
                    (connection_id, root_id.clone().unwrap_or(connection.config.bucket))
                }
                FileLocation::S3 { connection_id, bucket } => (connection_id, bucket.clone()),
                _ => return Ok(None),
            };
            cloud.object_select_bucket(connection_id, &bucket).await?;
            let (revision, identity) = cloud.object_selection_scope(connection_id).await?;
            if identity.bucket != bucket { return Err(super::service::invalid()); }
            let backend: Arc<dyn BrowserBackend> = Arc::new(CloudBrowserBackend {
                connection_id: connection_id.clone(), revision, identity,
            });
            Ok(Some((backend, String::new())))
        })
    }
}
