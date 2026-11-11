use std::ops::Range;
use std::sync::{Arc, atomic::AtomicBool};

use futures_util::future::BoxFuture;

use super::types::{
    CloudError, ObjectListing, ObjectMetadata, ObjectPage, ObjectStoreScope, ObjectStream,
    PutCondition, StorageCapabilities,
};

/// Provider-neutral primitives. Missing optional capabilities must fail closed.
pub trait ObjectStore: Send + Sync {
    /// All operation keys are relative to this physical namespace. Unknown
    /// stores remain usable by repository code, but cannot back ObjectService.
    fn scope(&self) -> Option<ObjectStoreScope> {
        None
    }
    fn capabilities(&self) -> StorageCapabilities;
    /// Pull-based listing preserves provider pagination and does not collect the
    /// full repository in memory. Every page error must reach the caller.
    fn list<'a>(&'a self, prefix: &'a str) -> ObjectListing<'a>;
    fn stat<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<ObjectMetadata, CloudError>>;
    fn get<'a>(
        &'a self,
        key: &'a str,
        range: Option<Range<u64>>,
    ) -> BoxFuture<'a, Result<ObjectStream, CloudError>>;
    fn put<'a>(
        &'a self,
        key: &'a str,
        body: ObjectStream,
        condition: PutCondition,
    ) -> BoxFuture<'a, Result<ObjectMetadata, CloudError>>;
    /// Explicit user-authorized editing may probe conditional-write support.
    fn replace_edit<'a>(&'a self, _key: &'a str, _body: ObjectStream, _etag: &'a str) -> BoxFuture<'a, Result<ObjectMetadata, CloudError>> {
        Box::pin(async { Err(CloudError::Unsupported) })
    }

    fn delete<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<(), CloudError>>;

    /// A HEAD followed by an unconditional DELETE is not a conditional delete.
    /// Providers without an atomic revision precondition must refuse the request.
    fn delete_if_match<'a>(
        &'a self,
        _key: &'a str,
        _etag: &'a str,
    ) -> BoxFuture<'a, Result<(), CloudError>> {
        Box::pin(async { Err(CloudError::Unsupported) })
    }

    fn list_page<'a>(
        &'a self,
        _prefix: &'a str,
        _token: Option<&'a str>,
    ) -> BoxFuture<'a, Result<ObjectPage, CloudError>> {
        Box::pin(async { Err(CloudError::Unsupported) })
    }

    fn upload_stream<'a>(
        &'a self,
        _key: &'a str,
        _body: ObjectStream,
    ) -> BoxFuture<'a, Result<ObjectMetadata, CloudError>> {
        Box::pin(async { Err(CloudError::Unsupported) })
    }

    fn upload_stream_with_control<'a>(
        &'a self,
        key: &'a str,
        body: ObjectStream,
        cancel: Arc<AtomicBool>,
    ) -> BoxFuture<'a, Result<ObjectMetadata, CloudError>> {
        let _ = cancel;
        self.upload_stream(key, body)
    }
}
