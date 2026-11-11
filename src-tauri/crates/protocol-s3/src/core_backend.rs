//! File-core adapter over the production S3 primitives. No second HTTP stack.
use crate::{CloudError, MAX_PUT_BYTES, ObjectMetadata, ObjectStore, PutCondition, S3Store};
use futures_util::{StreamExt, TryStreamExt};
use peerdav_file_core::*;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

/// A generation-bound view of one immutable S3 connection. Capabilities are a
/// construction-time snapshot: recreate the adapter after reconfiguration or
/// any capability verification attempt, using the caller's current generation.
/// Verification does not refresh or revoke an existing adapter, and this adapter
/// does not invent a capability epoch.
pub struct CoreS3Backend {
    pub(crate) store: Arc<S3Store>,
    pub(crate) generation: u64,
    scope: BackendScope,
    capabilities: Capabilities,
}

impl CoreS3Backend {
    pub fn new(store: Arc<S3Store>, generation: u64) -> Result<Self, FileError> {
        let physical = ObjectStore::scope(store.as_ref())
            .ok_or_else(|| FileError::unsupported(Operation::Scope))?;
        let scope = BackendScope::new(
            "s3".into(),
            physical.endpoint,
            serde_json::to_string(&(physical.bucket, physical.prefix)).map_err(|e| {
                FileError::new(
                    FileErrorKind::Internal,
                    Operation::Scope,
                    MutationOutcome::NotApplied,
                )
                .with_source(Box::new(e))
            })?,
            generation,
        )?;
        let provider = ObjectStore::capabilities(store.as_ref());
        let mut writes = vec![
            WriteSupport::BoundedAtomic {
                condition: ConditionKind::Any,
                max_bytes: MAX_PUT_BYTES as u64,
            },
            WriteSupport::Streaming {
                condition: ConditionKind::Any,
            },
        ];
        // Never advertise verified CAS until the production probe succeeded.
        if provider.conditional_writes_verified {
            if provider.create_only {
                writes.push(WriteSupport::BoundedAtomic {
                    condition: ConditionKind::CreateOnly,
                    max_bytes: MAX_PUT_BYTES as u64,
                });
            }
            if provider.conditional_replace {
                writes.push(WriteSupport::BoundedAtomic {
                    condition: ConditionKind::Match,
                    max_bytes: MAX_PUT_BYTES as u64,
                });
            }
        }
        Ok(Self {
            generation,
            store,
            scope,
            capabilities: Capabilities {
                list: true,
                list_prefix: true,
                list_cursor: true,
                stat: true,
                read: true,
                range_read: provider.range_read,
                writes,
                // S3 DeleteObject supports unconditional deletion of individual keys.
                deletes: vec![DeleteSupport {
                    mode: DeleteMode::File,
                    condition: ConditionKind::Any,
                }],
                ..Capabilities::default()
            },
        })
    }
    fn entry(&self, metadata: ObjectMetadata) -> Entry {
        Entry {
            id: self.scope.entry(metadata.key.clone()),
            name: metadata.key.rsplit('/').next().unwrap_or("").to_owned(),
            kind: EntryKind::File,
            size: Some(metadata.size),
            modified: None,
            content_type: None,
            revision: metadata.etag.map(Revision::new),
        }
    }
}

/// Stable cloud code for a later application error bridge. Diagnostics remain
/// in the FileError source chain, never flattened into a display string.
pub fn cloud_error_code(error: &FileError) -> &'static str {
    if error.outcome == MutationOutcome::Unknown || !error.cleanup.is_empty() {
        return "CLOUD_UPLOAD_PENDING_RECOVERY";
    }
    match error.kind {
        FileErrorKind::Unsupported => "CLOUD_UNSUPPORTED",
        FileErrorKind::NotFound => "CLOUD_NOT_FOUND",
        FileErrorKind::PermissionDenied => "CLOUD_PERMISSION_DENIED",
        FileErrorKind::Precondition => "CLOUD_PRECONDITION",
        FileErrorKind::Cancelled => "CLOUD_CANCELLED",
        FileErrorKind::Deadline => "CLOUD_DEADLINE",
        FileErrorKind::BudgetExceeded => "CLOUD_PRECONDITION",
        FileErrorKind::ScopeMismatch | FileErrorKind::InvalidInput => "CLOUD_CONFIG_INVALID",
        FileErrorKind::Transport => "CLOUD_TRANSPORT",
        _ => "CLOUD_STORAGE",
    }
}

pub fn cloud_to_file(
    error: CloudError,
    operation: Operation,
    outcome: MutationOutcome,
) -> FileError {
    use std::error::Error;
    // Keep the original CloudError intact: the compatibility bridge and recovery
    // ledger distinguish MultipartAbort/CompleteUnknown by their exact variants.
    let cleanup = match &error {
        CloudError::MultipartAbort { abort, .. } => Some(CleanupFailure {
            operation,
            status: None,
            provider_code: None,
            recovery_id: None,
            source: Box::new(std::io::Error::other(abort.to_string())),
        }),
        _ => None,
    };
    let mut primary = &error;
    while let CloudError::MultipartAbort { primary: nested, .. } = primary {
        primary = nested.as_ref();
    }
    let mut source: Option<&(dyn Error + 'static)> = Some(primary);
    let mut input = None;
    while let Some(current) = source {
        if let Some(original) = current.downcast_ref::<FileError>() {
            input = Some((original.kind, original.outcome));
            break;
        }
        source = current.source();
    }
    let kind = input.map(|i| i.0).unwrap_or(match primary {
        CloudError::Unsupported => FileErrorKind::Unsupported,
        CloudError::NotFound => FileErrorKind::NotFound,
        CloudError::Precondition => FileErrorKind::Precondition,
        CloudError::PrefixViolation => FileErrorKind::ScopeMismatch,
        CloudError::InvalidConfig(_) => FileErrorKind::InvalidInput,
        CloudError::PayloadTooLarge | CloudError::UploadBudgetExceeded => {
            FileErrorKind::BudgetExceeded
        }
        CloudError::UploadCancelled => FileErrorKind::Cancelled,
        CloudError::Transport { .. } => FileErrorKind::Transport,
        CloudError::Service {
            status: 401 | 403, ..
        } => FileErrorKind::PermissionDenied,
        CloudError::Storage {
            source:
                object_store::Error::PermissionDenied { .. }
                | object_store::Error::Unauthenticated { .. },
        } => FileErrorKind::PermissionDenied,
        CloudError::Storage {
            source: object_store::Error::NotSupported { .. },
        } => FileErrorKind::Unsupported,
        _ => FileErrorKind::Provider,
    });
    let outcome = if matches!(primary, CloudError::CompleteUnknown { .. }) {
        MutationOutcome::Unknown
    } else if matches!(
        kind,
        FileErrorKind::Precondition
            | FileErrorKind::NotFound
            | FileErrorKind::Unsupported
            | FileErrorKind::InvalidInput
            | FileErrorKind::ScopeMismatch
    ) {
        MutationOutcome::NotApplied
    } else {
        input.map(|i| i.1).unwrap_or(outcome)
    };
    let mut mapped = FileError::new(kind, operation, outcome);
    if let Some(cleanup) = cleanup {
        mapped.cleanup.push(cleanup);
    }
    if let CloudError::Service { status, .. } = &error {
        mapped.status = Some(*status);
    }
    if outcome == MutationOutcome::Unknown {
        mapped.retry = RetryClass::IfReplaySafe;
    }
    mapped.with_source(Box::new(error))
}

pub(crate) fn input_error(error: FileError) -> CloudError {
    CloudError::Storage {
        source: object_store::Error::Generic {
            store: "file-core input",
            source: Box::new(error),
        },
    }
}

impl FileBackend for CoreS3Backend {
    fn scope(&self) -> &BackendScope {
        &self.scope
    }
    fn capabilities(&self) -> &Capabilities {
        &self.capabilities
    }
    fn list<'a>(
        &'a self,
        context: OperationContext,
        request: ListRequest,
    ) -> BoxFuture<'a, Result<Page, FileError>> {
        Box::pin(async move {
            self.capabilities.check_list(&self.scope, &request)?;
            context.check_list_budget(&request)?;
            // Parent is a literal object prefix, not an invented directory tree.
            let prefix = format!(
                "{}{}",
                request.parent.opaque(),
                request.prefix.as_deref().unwrap_or("")
            );
            let (page, modified) = context
                .wait(Operation::List, MutationOutcome::NotApplied, async {
                    self.store
                        .core_list_page(
                            &prefix,
                            request.cursor.as_ref().map(ListCursor::opaque),
                            request.limit.get(),
                        )
                        .await
                        .map_err(|e| cloud_to_file(e, Operation::List, MutationOutcome::NotApplied))
                })
                .await?;
            let completeness = match page.next_token {
                Some(token) => Completeness::More(ListCursor::new(&request, token)?),
                None => Completeness::Complete,
            };
            Page::new(
                &request,
                page.objects
                    .into_iter()
                    .map(|m| {
                        let time = modified.get(&m.key).copied();
                        let mut entry = self.entry(m);
                        entry.modified = time;
                        entry
                    })
                    .collect(),
                completeness,
            )
        })
    }
    fn stat<'a>(
        &'a self,
        context: OperationContext,
        entry: EntryId,
    ) -> BoxFuture<'a, Result<Entry, FileError>> {
        Box::pin(async move {
            self.capabilities.check_stat(&self.scope, &entry)?;
            context
                .wait(Operation::Stat, MutationOutcome::NotApplied, async {
                    self.store
                        .core_stat(entry.opaque())
                        .await
                        .map(|(m, modified)| {
                            let mut entry = self.entry(m);
                            entry.modified = Some(modified);
                            entry
                        })
                        .map_err(|e| cloud_to_file(e, Operation::Stat, MutationOutcome::NotApplied))
                })
                .await
        })
    }
    fn read<'a>(
        &'a self,
        context: OperationContext,
        request: ReadRequest,
    ) -> BoxFuture<'a, Result<ReadResponse, FileError>> {
        Box::pin(async move {
            self.capabilities.check_read(&self.scope, &request)?;
            let (metadata, modified, actual, body) = context
                .wait(Operation::Read, MutationOutcome::NotApplied, async {
                    self.store
                        .core_read(
                            request.entry.opaque(),
                            request.range.map(|r| r.start()..r.end()),
                        )
                        .await
                        .map_err(|e| cloud_to_file(e, Operation::Read, MutationOutcome::NotApplied))
                })
                .await?;
            let range = if let Some(wanted) = request.range {
                // SDK result metadata is authoritative, including EOF clipping.
                if actual.start != wanted.start()
                    || actual.end != wanted.end().min(metadata.size)
                    || actual.end <= actual.start
                {
                    return Err(FileError::new(
                        FileErrorKind::Provider,
                        Operation::Read,
                        MutationOutcome::NotApplied,
                    ));
                }
                Some(ReadRange::new(actual.start, actual.end)?)
            } else {
                if actual != (0..metadata.size) {
                    return Err(FileError::new(
                        FileErrorKind::Provider,
                        Operation::Read,
                        MutationOutcome::NotApplied,
                    ));
                }
                None
            };
            let body = body
                .map_err(|e| cloud_to_file(e, Operation::Read, MutationOutcome::NotApplied))
                .boxed();
            let mut entry = self.entry(metadata);
            entry.modified = Some(modified);
            Ok(ReadResponse {
                entry,
                range,
                body: bounded_stream(body, context, Operation::Read, MutationOutcome::NotApplied),
            })
        })
    }
    fn write<'a>(
        &'a self,
        context: OperationContext,
        request: WriteRequest,
    ) -> BoxFuture<'a, Result<MutationResult, FileError>> {
        self.write_with_control(context, request, Arc::new(AtomicBool::new(false)))
    }
    fn delete<'a>(
        &'a self,
        context: OperationContext,
        request: DeleteRequest,
    ) -> BoxFuture<'a, Result<MutationResult, FileError>> {
        Box::pin(async move {
            self.capabilities.check_delete(&self.scope, &request)?;
            context.check(Operation::Delete, MutationOutcome::NotApplied)?;
            context
                .wait(Operation::Delete, MutationOutcome::Unknown, async {
                    self.store
                        .delete(request.entry.opaque())
                        .await
                        .map(|()| MutationResult { entry: None })
                        .map_err(|e| cloud_to_file(e, Operation::Delete, MutationOutcome::Unknown))
                })
                .await
        })
    }
}

impl CoreS3Backend {
    pub(crate) fn write_with_control<'a>(
        &'a self,
        mut context: OperationContext,
        request: WriteRequest,
        cancel: Arc<AtomicBool>,
    ) -> BoxFuture<'a, Result<MutationResult, FileError>> {
        Box::pin(async move {
            self.capabilities.check_write(&self.scope, &request)?;
            context.check(Operation::Write, MutationOutcome::NotApplied)?;
            if let WriteMode::BoundedAtomic { max_bytes } = request.mode {
                context.budget.max_bytes = context.budget.max_bytes.min(max_bytes);
            }
            let outcome = if matches!(request.mode, WriteMode::Streaming) {
                MutationOutcome::Unknown
            } else {
                MutationOutcome::NotApplied
            };
            let body = bounded_stream(request.body, context.clone(), Operation::Write, outcome)
                .map_err(input_error)
                .boxed();
            let condition = match request.condition {
                Condition::Any => PutCondition::Any,
                Condition::CreateOnly => PutCondition::CreateOnly,
                Condition::Match(r) => PutCondition::MatchEtag(r.opaque().to_owned()),
            };
            let metadata = match request.mode {
                WriteMode::BoundedAtomic { .. } => {
                    context
                        .wait(Operation::Write, MutationOutcome::Unknown, async {
                            self.store
                                .put(request.entry.opaque(), body, condition)
                                .await
                                .map_err(|e| {
                                    cloud_to_file(e, Operation::Write, MutationOutcome::Unknown)
                                })
                        })
                        .await?
                }
                WriteMode::Streaming => {
                    let upload = self.store.upload_stream_with_control(
                        request.entry.opaque(),
                        body,
                        cancel.clone(),
                    );
                    tokio::pin!(upload);
                    let deadline = async {
                        match context.deadline {
                            Some(d) => tokio::time::sleep_until(d.into()).await,
                            None => std::future::pending::<()>().await,
                        }
                    };
                    let interrupted = tokio::select! {
                        biased;
                        _ = context.cancellation.cancelled() => Some(FileErrorKind::Cancelled),
                        _ = deadline => Some(FileErrorKind::Deadline),
                        result = &mut upload => return result.map(|m| MutationResult { entry: Some(self.entry(m)) }).map_err(|e| cloud_to_file(e, Operation::Write, MutationOutcome::Unknown)),
                    };
                    // Do not drop multipart: original lifecycle owns abort/complete.
                    cancel.store(true, Ordering::Release);
                    match upload.await {
                        Ok(m) => {
                            return Ok(MutationResult {
                                entry: Some(self.entry(m)),
                            });
                        }
                        Err(e) => {
                            let mut mapped =
                                cloud_to_file(e, Operation::Write, MutationOutcome::Unknown);
                            if mapped.kind == FileErrorKind::Cancelled {
                                if let Some(kind) = interrupted {
                                    mapped.kind = kind;
                                }
                            }
                            return Err(mapped);
                        }
                    }
                }
            };
            Ok(MutationResult {
                entry: Some(self.entry(metadata)),
            })
        })
    }
}
