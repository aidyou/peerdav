use crate::*;

/// One authorized share and connection generation. The injected DAV client must
/// already carry the host's pinned TLS, authorization and redirect policy.
pub struct CorePeerDavBackend {
    client: Client,
    share: String,
    scope: BackendScope,
    capabilities: Capabilities,
}
impl CorePeerDavBackend {
    pub fn new(
        client: Client,
        endpoint_identity: String,
        share: String,
        generation: u64,
    ) -> Result<Self, FileError> {
        validate_share_id(&share)?;
        let scope = BackendScope::new(
            "peerdav".into(),
            endpoint_identity,
            share.clone(),
            generation,
        )?;
        Ok(Self {
            client,
            share,
            scope,
            capabilities: Capabilities {
                list: true,
                stat: true,
                read: true,
                range_read: true,
                // MKCOL succeeds only when a new collection was actually created.
                create_dir: vec![ConditionKind::CreateOnly],
                // DELETE is recursive by protocol. Never advertise atomic
                // file-type or empty-directory protection that DAV cannot supply.
                deletes: vec![DeleteSupport {
                    mode: DeleteMode::Recursive,
                    condition: ConditionKind::Any,
                }],
                ..Capabilities::default()
            },
        })
    }
    fn remote(&self, id: &EntryId) -> Result<String, FileError> {
        let normalized = normalize_display_path(id.opaque())?;
        if normalized != id.opaque() {
            return Err(invalid("entry identity must be a canonical raw path"));
        }
        encode_share_path(&self.share, &normalized)
    }
    fn entry(&self, entity: ListEntity) -> Option<Entry> {
        let (href, kind, size, modified, content_type, revision) = match entity {
            ListEntity::File(file) => (
                file.href,
                EntryKind::File,
                u64::try_from(file.content_length).ok(),
                Some(file.last_modified.timestamp()),
                Some(file.content_type),
                file.tag.map(Revision::new),
            ),
            ListEntity::Folder(folder) => (
                folder.href,
                EntryKind::Directory,
                None,
                Some(folder.last_modified.timestamp()),
                None,
                None,
            ),
        };
        let path = href_to_relative(&href, &self.share)?;
        Some(Entry {
            name: path.rsplit('/').next().unwrap_or("").into(),
            id: self.scope.entry(path),
            kind,
            size,
            modified,
            content_type,
            revision,
        })
    }
}
impl ReplayableWriteBackend for CorePeerDavBackend {
    fn write_source<'a>(
        &'a self,
        context: OperationContext,
        request: SourceWriteRequest,
    ) -> BoxFuture<'a, Result<MutationResult, FileError>> {
        Box::pin(async move {
            self.scope
                .validate_entry(&request.entry, Operation::Write)?;
            if request.condition != Condition::Any || request.mode != WriteMode::Streaming {
                return Err(FileError::unsupported(Operation::Write));
            }
            context.check(Operation::Write, MutationOutcome::NotApplied)?;
            if request.source.size() > context.budget.max_bytes {
                return Err(FileError::new(
                    FileErrorKind::BudgetExceeded,
                    Operation::Write,
                    MutationOutcome::NotApplied,
                ));
            }
            if request.session_id.is_empty()
                || request.session_id.len() > 128
                || !request.session_id.bytes().all(|b| b.is_ascii_hexdigit())
            {
                return Err(invalid("invalid upload session identity"));
            }
            let remote = self.remote(&request.entry)?;
            crate::session::execute(&self.client, &remote, context, request).await
        })
    }
}

impl FileBackend for CorePeerDavBackend {
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
            let remote = self.remote(&request.parent)?;
            let entities = context
                .wait(
                    Operation::List,
                    MutationOutcome::NotApplied,
                    list_entities(
                        &self.client,
                        &remote,
                        Depth::Number(1),
                        Operation::List,
                        context.budget.max_bytes,
                    ),
                )
                .await?;
            let parent = request.parent.opaque();
            let mut entries = Vec::new();
            let mut truncated = false;
            for entity in entities {
                let entry = self.entry(entity).ok_or_else(|| {
                    failure(
                        FileErrorKind::Provider,
                        Operation::List,
                        std::io::Error::other("invalid listing href"),
                    )
                })?;
                if entry.id.opaque() == parent {
                    continue;
                }
                let directory = entry
                    .id
                    .opaque()
                    .rsplit_once('/')
                    .map(|(dir, _)| if dir.is_empty() { "/" } else { dir });
                if directory != Some(parent) {
                    return Err(failure(
                        FileErrorKind::Provider,
                        Operation::List,
                        std::io::Error::other("listing contains a non-child entry"),
                    ));
                }
                if entries.len() == request.limit.get() {
                    truncated = true;
                    break;
                }
                entries.push(entry);
            }
            Page::new(
                &request,
                entries,
                if truncated {
                    Completeness::Truncated
                } else {
                    Completeness::Complete
                },
            )
        })
    }
    fn stat<'a>(
        &'a self,
        context: OperationContext,
        id: EntryId,
    ) -> BoxFuture<'a, Result<Entry, FileError>> {
        Box::pin(async move {
            self.capabilities.check_stat(&self.scope, &id)?;
            context.check(Operation::Stat, MutationOutcome::NotApplied)?;
            let remote = self.remote(&id)?;
            let entities = context
                .wait(
                    Operation::Stat,
                    MutationOutcome::NotApplied,
                    list_entities(
                        &self.client,
                        &remote,
                        Depth::Number(0),
                        Operation::Stat,
                        context.budget.max_bytes,
                    ),
                )
                .await?;
            entities
                .into_iter()
                .filter_map(|e| self.entry(e))
                .find(|e| e.id == id)
                .ok_or_else(|| {
                    FileError::new(
                        FileErrorKind::NotFound,
                        Operation::Stat,
                        MutationOutcome::NotApplied,
                    )
                })
        })
    }
    fn read<'a>(
        &'a self,
        context: OperationContext,
        request: ReadRequest,
    ) -> BoxFuture<'a, Result<ReadResponse, FileError>> {
        Box::pin(async move {
            self.capabilities.check_read(&self.scope, &request)?;
            context.check(Operation::Read, MutationOutcome::NotApplied)?;
            let remote = self.remote(&request.entry)?;
            let response = context
                .wait(
                    Operation::Read,
                    MutationOutcome::NotApplied,
                    get(&self.client, &remote, request.range),
                )
                .await?;
            let size = if request.range.is_some() {
                response
                    .headers()
                    .get(reqwest::header::CONTENT_RANGE)
                    .and_then(|value| value.to_str().ok())
                    .and_then(|value| value.rsplit_once('/'))
                    .and_then(|(_, total)| total.parse::<u64>().ok())
            } else {
                response.content_length()
            };
            let revision = response
                .headers()
                .get(reqwest::header::ETAG)
                .and_then(|v| v.to_str().ok())
                .map(Revision::new);
            let entry = Entry {
                name: request
                    .entry
                    .opaque()
                    .rsplit('/')
                    .next()
                    .unwrap_or("")
                    .into(),
                id: request.entry,
                kind: EntryKind::File,
                size,
                modified: None,
                content_type: response
                    .headers()
                    .get(reqwest::header::CONTENT_TYPE)
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_owned),
                revision,
            };
            let body = response_stream(response);
            // Verify the actual body length as well as the Content-Range header.
            let expected = request.range.map(ReadRange::len);
            let body: ByteStream = Box::pin(futures_util::stream::unfold(
                Some((body, 0u64)),
                move |state| async move {
                    let (mut body, used) = state?;
                    match body.next().await {
                        Some(Ok(chunk)) => {
                            let used = used.saturating_add(chunk.len() as u64);
                            if expected.is_some_and(|n| used > n) {
                                return Some((
                                    Err(failure(
                                        FileErrorKind::Provider,
                                        Operation::Read,
                                        std::io::Error::other(
                                            "range body exceeds requested length",
                                        ),
                                    )),
                                    None,
                                ));
                            }
                            Some((Ok(chunk), Some((body, used))))
                        }
                        Some(Err(e)) => Some((Err(e), None)),
                        None if expected.is_some_and(|n| used != n) => Some((
                            Err(failure(
                                FileErrorKind::Provider,
                                Operation::Read,
                                std::io::Error::other("short range body"),
                            )),
                            None,
                        )),
                        None => None,
                    }
                },
            ));
            Ok(ReadResponse {
                entry,
                range: request.range,
                body: bounded_stream(body, context, Operation::Read, MutationOutcome::NotApplied),
            })
        })
    }
    fn create_dir<'a>(
        &'a self,
        context: OperationContext,
        request: CreateDirRequest,
    ) -> BoxFuture<'a, Result<MutationResult, FileError>> {
        Box::pin(async move {
            self.capabilities.check_create_dir(&self.scope, &request)?;
            context.check(Operation::CreateDir, MutationOutcome::NotApplied)?;
            let remote = self.remote(&request.entry)?;
            if request.entry.opaque() == "/" {
                return Err(invalid("the shared root cannot be created"));
            }
            context
                .wait(
                    Operation::CreateDir,
                    MutationOutcome::Unknown,
                    create_directory(&self.client, &remote),
                )
                .await?;
            Ok(MutationResult {
                entry: Some(Entry {
                    name: request
                        .entry
                        .opaque()
                        .rsplit('/')
                        .next()
                        .unwrap_or("")
                        .into(),
                    id: request.entry,
                    kind: EntryKind::Directory,
                    size: None,
                    modified: None,
                    content_type: None,
                    revision: None,
                }),
            })
        })
    }
    fn write<'a>(
        &'a self,
        context: OperationContext,
        request: WriteRequest,
    ) -> BoxFuture<'a, Result<MutationResult, FileError>> {
        Box::pin(async move {
            self.capabilities.check_write(&self.scope, &request)?;
            context.check(Operation::Write, MutationOutcome::NotApplied)?;
            Err(FileError::unsupported(Operation::Write))
        })
    }
    fn delete<'a>(
        &'a self,
        context: OperationContext,
        request: DeleteRequest,
    ) -> BoxFuture<'a, Result<MutationResult, FileError>> {
        Box::pin(async move {
            self.capabilities.check_delete(&self.scope, &request)?;
            context.check(Operation::Delete, MutationOutcome::NotApplied)?;
            let remote = self.remote(&request.entry)?;
            if request.entry.opaque() == "/" {
                return Err(invalid("the shared root cannot be deleted"));
            }
            context
                .wait(
                    Operation::Delete,
                    MutationOutcome::Unknown,
                    delete_entry(&self.client, &remote),
                )
                .await?;
            Ok(MutationResult { entry: None })
        })
    }
}
