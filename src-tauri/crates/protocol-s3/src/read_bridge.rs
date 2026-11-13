//! Compatibility bridge: application ObjectService keeps its ledgers and guards,
//! while ordinary reads and mutations dispatch through the core backend contract.
use crate::*;
use futures_util::{StreamExt, TryStreamExt};
use peerdav_file_core::*;
use std::{
    num::NonZeroUsize,
    ops::Range,
    sync::{Arc, atomic::AtomicBool},
};

fn context() -> OperationContext {
    OperationContext {
        cancellation: Default::default(),
        deadline: None,
        budget: OperationBudget {
            max_bytes: u64::MAX,
            max_entries: NonZeroUsize::MAX,
            max_chunk_bytes: NonZeroUsize::MAX,
        },
    }
}
fn metadata(entry: Entry) -> ObjectMetadata {
    ObjectMetadata {
        key: entry.id.opaque().to_owned(),
        size: entry.size.unwrap_or(0),
        etag: entry.revision.map(|r| r.opaque().to_owned()),
    }
}
fn original(error: FileError) -> CloudError {
    if std::error::Error::source(&error).is_none() {
        if error.kind == FileErrorKind::BudgetExceeded && error.operation == Operation::Write {
            return CloudError::PayloadTooLarge;
        }
        return crate::core_backend::input_error(error);
    }
    let Some(source) = error.into_source() else {
        return CloudError::Unsupported;
    };
    match source.downcast::<CloudError>() {
        Ok(error) => match *error {
            // The bounded body crosses the cloud interface once more on its
            // way back. Remove only our own input wrapper, preserving provider
            // CloudError variants and their original sources unchanged.
            CloudError::Storage {
                source: object_store::Error::Generic { store: "file-core input", source },
            } => match source.downcast::<FileError>() {
                Ok(error) => original(*error),
                Err(source) => CloudError::Storage {
                    source: object_store::Error::Generic { store: "file-core input", source },
                },
            },
            error => error,
        },
        Err(source) => CloudError::Storage {
            source: object_store::Error::Generic {
                store: "file-core",
                source,
            },
        },
    }
}
pub struct CoreS3ReadBridge(pub CoreS3Backend);
impl ObjectStore for CoreS3ReadBridge {
    fn scope(&self) -> Option<ObjectStoreScope> {
        ObjectStore::scope(self.0.store.as_ref())
    }
    fn copy_move_stream<'a>(&'a self, key: &'a str, body: ObjectStream) -> BoxFuture<'a, Result<ObjectMetadata, CloudError>> {
        // Explicit move copies retain the provider's bounded create-only path.
        // The move planner checks MAX_PUT_BYTES before any user-object mutation.
        self.0.store.put(key, body, PutCondition::CreateOnly)
    }
    fn verify_move_capabilities(&self) -> BoxFuture<'_, Result<StorageCapabilities, CloudError>> {
        Box::pin(async {
            let _probe = self.0.store.probe_lock.lock().await;
            if let Some(supported) = self.0.store.cached_probe("move")? {
                return if supported { Ok(StorageCapabilities { create_only: true, conditional_replace: false, range_read: true, conditional_writes_verified: true }) } else { Err(CloudError::Unsupported) };
            }
            self.0.store.probe_progress("move", "checking")?;
            let key = format!("probes/{}/move-create", rand::random::<u128>());
            let result = async {
                let first = self.0.store.put(&key, futures_util::stream::once(async { Ok(bytes::Bytes::from_static(b"move probe")) }).boxed(), PutCondition::CreateOnly).await?;
                let duplicate = self.0.store.put(&key, futures_util::stream::once(async { Ok(bytes::Bytes::from_static(b"must not replace")) }).boxed(), PutCondition::CreateOnly).await;
                match duplicate {
                    Err(CloudError::Precondition) => {}
                    Err(error) => return Err(error),
                    Ok(_) => return Err(CloudError::Unsupported),
                }
                self.0.store.delete_if_match(&key, first.etag.as_deref().ok_or(CloudError::Unsupported)?).await?;
                Ok(StorageCapabilities { create_only: true, conditional_replace: false, range_read: true, conditional_writes_verified: true })
            }.await;
            self.0.store.probe_progress("move", "cleaning")?;
            let result = self.0.store.finish_probe(&[&key], result).await;
            self.0.store.remember_probe("move", &result)?;
            result
        })
    }
    fn capabilities(&self) -> StorageCapabilities {
        let snapshot = FileBackend::capabilities(&self.0);
        let supports = |condition| snapshot.writes.iter().any(|support| matches!(
            support,
            WriteSupport::BoundedAtomic { condition: kind, .. } if *kind == condition
        ));
        let create_only = supports(ConditionKind::CreateOnly);
        let conditional_replace = supports(ConditionKind::Match);
        StorageCapabilities {
            create_only,
            conditional_replace,
            range_read: snapshot.range_read,
            conditional_writes_verified: create_only && conditional_replace,
        }
    }
    fn list<'a>(&'a self, prefix: &'a str) -> ObjectListing<'a> {
        futures_util::stream::try_unfold((None, false), move |(token, done)| async move {
            if done {
                return Ok(None);
            }
            let page = ObjectStore::list_page(self, prefix, token.as_deref()).await?;
            let done = page.next_token.is_none();
            Ok(Some((
                futures_util::stream::iter(page.objects.into_iter().map(Ok)),
                (page.next_token, done),
            )))
        })
        .try_flatten()
        .boxed()
    }
    fn list_page<'a>(
        &'a self,
        prefix: &'a str,
        token: Option<&'a str>,
    ) -> BoxFuture<'a, Result<ObjectPage, CloudError>> {
        Box::pin(async move {
            let mut request = ListRequest {
                parent: FileBackend::scope(&self.0).entry(prefix),
                query: None,
                prefix: None,
                cursor: None,
                limit: NonZeroUsize::new(1000).ok_or(CloudError::Unsupported)?,
            };
            request.cursor = token
                .map(|t| ListCursor::new(&request, t.to_owned()))
                .transpose()
                .map_err(original)?;
            let page = FileBackend::list(&self.0, context(), request)
                .await
                .map_err(original)?;
            let next_token = page.next().map(|c| c.opaque().to_owned());
            Ok(ObjectPage {
                objects: page.into_parts().0.into_iter().map(metadata).collect(),
                next_token,
            })
        })
    }
    fn stat<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<ObjectMetadata, CloudError>> {
        Box::pin(async move {
            FileBackend::stat(&self.0, context(), FileBackend::scope(&self.0).entry(key))
                .await
                .map(metadata)
                .map_err(original)
        })
    }
    fn get<'a>(
        &'a self,
        key: &'a str,
        range: Option<Range<u64>>,
    ) -> BoxFuture<'a, Result<ObjectStream, CloudError>> {
        Box::pin(async move {
            let range = range
                .map(|r| ReadRange::new(r.start, r.end))
                .transpose()
                .map_err(original)?;
            let response = FileBackend::read(
                &self.0,
                context(),
                ReadRequest {
                    entry: FileBackend::scope(&self.0).entry(key),
                    range,
                },
            )
            .await
            .map_err(original)?;
            Ok(response.body.map_err(original).boxed())
        })
    }
    fn put<'a>(
        &'a self,
        key: &'a str,
        body: ObjectStream,
        condition: PutCondition,
    ) -> BoxFuture<'a, Result<ObjectMetadata, CloudError>> {
        Box::pin(async move {
            let condition = match condition {
                PutCondition::Any => Condition::Any,
                PutCondition::CreateOnly => Condition::CreateOnly,
                PutCondition::MatchEtag(etag) => Condition::Match(Revision::new(etag)),
            };
            let result = FileBackend::write(&self.0, context(), WriteRequest {
                entry: FileBackend::scope(&self.0).entry(key),
                condition,
                mode: WriteMode::BoundedAtomic { max_bytes: MAX_PUT_BYTES as u64 },
                body: body.map_err(|e| cloud_to_file(e, Operation::Write, MutationOutcome::NotApplied)).boxed(),
            }).await.map_err(original)?;
            result.entry.map(metadata).ok_or(CloudError::Unsupported)
        })
    }
    fn replace_edit<'a>(&'a self, key: &'a str, body: ObjectStream, etag: &'a str) -> BoxFuture<'a, Result<ObjectMetadata, CloudError>> {
        Box::pin(async move {
            let capabilities = self.0.store.verify_capabilities().await?;
            if !capabilities.conditional_writes_verified || !capabilities.conditional_replace { return Err(CloudError::Unsupported); }
            // Refresh the core capability snapshot only for this explicit write;
            // ordinary browsing still does not probe or grant write trust.
            let backend = CoreS3Backend::new(self.0.store.clone(), self.0.generation).map_err(original)?;
            ObjectStore::put(&CoreS3ReadBridge(backend), key, body, PutCondition::MatchEtag(etag.into())).await
        })
    }
    fn delete_if_match<'a>(
        &'a self,
        key: &'a str,
        etag: &'a str,
    ) -> BoxFuture<'a, Result<(), CloudError>> {
        // Conditional deletion includes its own provider probe. Do not route it
        // through the core adapter's unconditional-delete capability snapshot.
        self.0.store.delete_if_match(key, etag)
    }

    fn delete<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<(), CloudError>> {
        Box::pin(async move {
            FileBackend::delete(&self.0, context(), DeleteRequest {
                entry: FileBackend::scope(&self.0).entry(key),
                condition: Condition::Any,
                mode: DeleteMode::File,
            }).await.map(|_| ()).map_err(original)
        })
    }
    fn upload_stream<'a>(
        &'a self,
        key: &'a str,
        body: ObjectStream,
    ) -> BoxFuture<'a, Result<ObjectMetadata, CloudError>> {
        self.upload_stream_with_control(key, body, Arc::new(AtomicBool::new(false)))
    }
    fn upload_stream_with_control<'a>(
        &'a self,
        key: &'a str,
        body: ObjectStream,
        cancel: Arc<AtomicBool>,
    ) -> BoxFuture<'a, Result<ObjectMetadata, CloudError>> {
        Box::pin(async move {
            let result = self.0.write_with_control(context(), WriteRequest {
                entry: FileBackend::scope(&self.0).entry(key),
                condition: Condition::Any,
                mode: WriteMode::Streaming,
                body: body.map_err(|e| cloud_to_file(e, Operation::Write, MutationOutcome::Unknown)).boxed(),
            }, cancel).await.map_err(original)?;
            result.entry.map(metadata).ok_or(CloudError::Unsupported)
        })
    }
}
