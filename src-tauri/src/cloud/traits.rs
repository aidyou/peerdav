use std::ops::Range;

use futures_util::future::BoxFuture;

use super::types::{
    CloudError, ObjectListing, ObjectMetadata, ObjectStream, PutCondition, StorageCapabilities,
};

/// Provider-neutral primitives. Missing optional capabilities must fail closed.
pub trait ObjectStore: Send + Sync {
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
    fn delete<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<(), CloudError>>;
}
