use super::{append_index::AppendIndex, append_repository::AppendRepository, append_types::*};
use crate::cloud::{
    traits::ObjectStore,
    types::{CloudError, PutCondition},
};
use bytes::Bytes;
use futures_util::{StreamExt, stream};
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicBool, Ordering};

/// Independent append-only publisher. No scanning, materialization or baseline writes.
pub struct AppendPublisher;
impl AppendPublisher {
    /// Input is already sealed exactly once by the caller. An empty closure is a
    /// pull-only/no-change operation and deliberately creates no publish record.
    pub fn prepare(
        index: &mut AppendIndex,
        repository: &AppendRepository,
        operation_id: &str,
        parents_frontier: &[String],
        root: ObjectRef,
        objects: &[PreparedObject],
    ) -> Result<(), PublishError> {
        Self::prepare_with_limits(
            index,
            repository,
            operation_id,
            parents_frontier,
            root,
            objects,
            &PublishLimits::default(),
        )
    }

    pub fn prepare_with_limits(
        index: &mut AppendIndex,
        repository: &AppendRepository,
        operation_id: &str,
        parents_frontier: &[String],
        root: ObjectRef,
        objects: &[PreparedObject],
        limits: &PublishLimits,
    ) -> Result<(), PublishError> {
        Self::prepare_source(
            index,
            repository,
            operation_id,
            parents_frontier,
            root,
            objects.len(),
            |_, i| Ok(objects[i].clone()),
            limits,
        )
    }

    pub(crate) fn prepare_cached(
        index: &mut AppendIndex,
        repository: &AppendRepository,
        operation_id: &str,
        parents: &[String],
        root: ObjectRef,
        count: usize,
    ) -> Result<(), PublishError> {
        Self::prepare_source(
            index,
            repository,
            operation_id,
            parents,
            root,
            count,
            |index, i| {
                index
                    .engine_object(operation_id, i)
                    .map_err(PublishError::from)
            },
            &PublishLimits::default(),
        )
    }

    fn prepare_source<F>(
        index: &mut AppendIndex,
        repository: &AppendRepository,
        operation_id: &str,
        parents_frontier: &[String],
        root: ObjectRef,
        count: usize,
        mut object_at: F,
        limits: &PublishLimits,
    ) -> Result<(), PublishError>
    where
        F: FnMut(&AppendIndex, usize) -> Result<PreparedObject, PublishError>,
    {
        if count == 0 {
            if index.frozen_operation(operation_id)?.is_some() {
                return Err(PublishError::Invalid(
                    "existing operation cannot become empty",
                ));
            }
            return Ok(());
        }
        if count > limits.max_objects.min(MAX_PUBLISH_OBJECTS)
            || operation_id.len() != 32
            || index.repository_id() != repository.repo_id().as_bytes()
        {
            return Err(PublishError::Invalid("operation identity or object count"));
        }
        // Enforce aggregate and individual wire limits before parsing payloads.
        let mut staged = 0u64;
        for ordinal in 0..count {
            let object = object_at(index, ordinal)?;
            let block = object.key().contains("/blocks/");
            let wire_limit = 2
                * (if block {
                    MAX_BLOCK_SIZE
                } else {
                    MAX_METADATA_SIZE
                } + 16)
                + 4096;
            staged = staged
                .checked_add(object.bytes().len() as u64)
                .ok_or(PublishError::Invalid("staging overflow"))?;
            if object.bytes().len() > wire_limit
                || staged > limits.max_staging.min(MAX_PUBLISH_STAGING)
            {
                return Err(PublishError::Invalid("wire or staging limit"));
            }
        }
        let mut total = 0u64;
        let mut previous = 0;
        let mut keys = std::collections::BTreeSet::new();
        let mut trees = std::collections::BTreeMap::new();
        let mut blocks = std::collections::BTreeMap::new();
        let mut frozen = Vec::with_capacity(count);
        for ordinal in 0..count {
            let object = object_at(index, ordinal)?;
            total = total
                .checked_add(object.bytes().len() as u64)
                .ok_or(PublishError::Invalid("staging overflow"))?;
            if total > MAX_PUBLISH_STAGING {
                return Err(PublishError::Invalid("staging limit"));
            }
            let envelope: Envelope =
                serde_json::from_slice(object.bytes()).map_err(AppendError::from)?;
            let rank = match envelope.kind.as_str() {
                "block" => 0,
                "tree" => 1,
                "commit" => 2,
                _ => return Err(PublishError::Invalid("object kind")),
            };
            if rank < previous
                || (rank == 2 && ordinal + 1 != count)
                || !keys.insert(object.key().to_owned())
            {
                return Err(PublishError::Invalid("object ordering or duplicate key"));
            }
            previous = rank;
            let digest = hex::encode(Sha256::digest(object.bytes()));
            let expected = if rank == 2 {
                format!(
                    "{APPEND_ROOT}/{}/commits/{operation_id}-{digest}",
                    repository.repo_id()
                )
            } else {
                format!("{APPEND_ROOT}/{}/{}s/{digest}", repository.repo_id(), envelope.kind)
            };
            if object.key() != expected {
                return Err(PublishError::Invalid("object key"));
            }
            let reference = ObjectRef {
                kind: envelope.kind.clone(),
                digest: digest.clone(),
            };
            match rank {
                0 => {
                    repository.open_block(object.bytes())?;
                    blocks.insert(digest.clone(), ordinal);
                }
                1 => {
                    trees.insert(
                        digest.clone(),
                        repository.open_tree(object.bytes(), &reference)?,
                    );
                }
                _ => {
                    let commit = repository.open_commit(object.bytes(), &reference)?;
                    let parents: Vec<_> = commit
                        .parents
                        .iter()
                        .map(|p| {
                            format!(
                                "{APPEND_ROOT}/{}/commits/{}-{}",
                                repository.repo_id(),
                                p.operation_id,
                                p.digest
                            )
                        })
                        .collect();
                    if commit.operation_id != operation_id
                        || commit.root != root
                        || parents != parents_frontier
                    {
                        return Err(PublishError::Invalid("commit intent"));
                    }
                }
            }
            frozen.push(FrozenObject {
                key: object.key().into(),
                digest,
                len: object.bytes().len(),
                chunks: object.bytes().len().div_ceil(1024 * 1024),
            });
        }
        if previous != 2 {
            return Err(PublishError::Invalid("missing commit"));
        }
        // No remote reuse proof API exists yet: every reachable dependency must
        // be locally frozen and authenticated. Opaque cache markers are not proof.
        let mut reached_trees = std::collections::BTreeSet::new();
        let mut reached_blocks = std::collections::BTreeSet::new();
        let mut stack = vec![(root.digest.as_str(), 0usize)];
        let mut tree_total = 0usize;
        while let Some((digest, depth)) = stack.pop() {
            if depth > MAX_TREE_DEPTH {
                return Err(PublishError::Invalid("tree depth"));
            }
            if !reached_trees.insert(digest) {
                continue;
            }
            let tree = trees
                .get(digest)
                .ok_or(PublishError::Invalid("missing frozen tree"))?;
            tree_total = tree_total
                .checked_add(serde_json::to_vec(tree).map_err(AppendError::from)?.len())
                .ok_or(PublishError::Invalid("tree size"))?;
            if tree_total > MAX_TREE_SIZE {
                return Err(PublishError::Invalid("tree size"));
            }
            for child in &tree.children {
                let target = trees
                    .get(&child.object.digest)
                    .ok_or(PublishError::Invalid("missing frozen child"))?;
                if target.directory != child.directory {
                    return Err(PublishError::Invalid("child directory identity"));
                }
                stack.push((&child.object.digest, depth + 1));
            }
            for binding in &tree.bindings {
                let bytes = blocks
                    .get(&binding.object.digest)
                    .ok_or(PublishError::Invalid("missing frozen block"))?;
                let object = object_at(index, *bytes)?;
                repository.open_block_with_ref(object.bytes(), Some(binding))?;
                reached_blocks.insert(binding.object.digest.as_str());
            }
        }
        if reached_trees.len() != trees.len() || reached_blocks.len() != blocks.len() {
            return Err(PublishError::Invalid("unreachable frozen object"));
        }
        let op = FrozenOperation {
            closure_validation_version: 1,
            operation_id: operation_id.into(),
            parents_frontier: parents_frontier.to_vec(),
            root,
            objects: frozen,
            state: OperationState::Prepared,
            next_object: 0,
            requests: 0,
            bytes_written: 0,
        };
        index.freeze_publication_source(&op, |index, i| {
            object_at(index, i).map_err(|_| super::index_types::IndexError::InvalidRecord)
        })?;
        Ok(())
    }

    fn charge(
        index: &mut AppendIndex,
        op: &mut FrozenOperation,
        write: u64,
        read: usize,
    ) -> Result<(), PublishError> {
        let requests = op.requests.checked_add(1).ok_or(PublishError::Budget)?;
        let bytes = op
            .bytes_written
            .checked_add(write)
            .ok_or(PublishError::Budget)?;
        // Every request has one attempt per resume call (strictly below the
        // approved three-attempt ceiling); retries across resumes are metered.
        // Discovery and publisher debit the same persisted operation ledger.
        // op.requests is the publisher-only diagnostic subset, not added twice.
        let mut shared = index.discovery_budget(&op.operation_id)?;
        shared.requests = shared.requests.checked_add(1).ok_or(PublishError::Budget)?;
        shared.bytes_read = shared
            .bytes_read
            .checked_add(read)
            .ok_or(PublishError::Budget)?;
        if requests > MAX_PUBLISH_REQUESTS
            || shared.requests > MAX_DISCOVERY_REQUESTS
            || shared.bytes_read > MAX_DISCOVERY_READ_BYTES
            || bytes > MAX_PUBLISH_WRITE
        {
            return Err(PublishError::Budget);
        }
        index.save_discovery_budget(&op.operation_id, &shared)?;
        op.requests = requests;
        op.bytes_written = bytes;
        index.save_publication(op)?;
        Ok(())
    }

    pub async fn resume<S: ObjectStore + ?Sized>(
        index: &mut AppendIndex,
        store: &S,
        id: &str,
        cancel: &AtomicBool,
    ) -> Result<Option<OperationState>, PublishError> {
        let Some(mut op) = index.frozen_operation(id)? else {
            return Ok(None);
        };
        if op.closure_validation_version != 1 {
            return Err(PublishError::Invalid("unvalidated legacy closure"));
        }
        if matches!(
            op.state,
            OperationState::Published | OperationState::ApplyPending | OperationState::Applied
        ) {
            return Ok(Some(op.state));
        }
        if op.state == OperationState::Publishing {
            op.state = OperationState::PublishedUnknown;
            index.save_publication(&op)?;
        }
        // Validate the entire staging closure before any request. Missing earlier
        // chunks cannot be silently replaced even if a later object is pending.
        for ordinal in 0..op.objects.len() {
            index.frozen_bytes(&op, ordinal)?;
        }
        while op.next_object < op.objects.len() {
            if cancel.load(Ordering::Relaxed) {
                return Err(PublishError::Cancelled);
            }
            let ordinal = op.next_object;
            let object = op.objects[ordinal].clone();
            let bytes = index.frozen_bytes(&op, ordinal)?;
            // Frozen bytes bound the only acceptable response. Commit request and
            // full read reservation together before GET, including missing/error
            // responses and dropped futures; never refund or charge chunks again.
            Self::charge(index, &mut op, 0, bytes.len())?;
            let exists = match store.get(&object.key, None).await {
                Ok(mut body) => {
                    let mut offset = 0usize;
                    let mut digest = Sha256::new();
                    while let Some(chunk) = body.next().await {
                        let chunk = chunk?;
                        let end = offset
                            .checked_add(chunk.len())
                            .ok_or(PublishError::Invalid("remote bytes"))?;
                        if bytes.get(offset..end) != Some(chunk.as_ref()) {
                            return Err(PublishError::Invalid(
                                "remote bytes differ from frozen object",
                            ));
                        }
                        digest.update(&chunk);
                        offset = end;
                    }
                    if offset != bytes.len() || hex::encode(digest.finalize()) != object.digest {
                        return Err(PublishError::Invalid("remote object digest"));
                    }
                    true
                }
                Err(CloudError::NotFound) => false,
                Err(error) => return Err(error.into()),
            };
            if !exists {
                if cancel.load(Ordering::Relaxed) {
                    return Err(PublishError::Cancelled);
                }
                if ordinal + 1 == op.objects.len() {
                    op.state = OperationState::Publishing;
                    index.save_publication(&op)?;
                    // Persist uncertainty before dispatch: dropped futures and process
                    // death have exactly the same recovery path as lost responses.
                    op.state = OperationState::PublishedUnknown;
                    index.save_publication(&op)?;
                }
                Self::charge(index, &mut op, bytes.len() as u64, 0)?;
                let body = Box::pin(stream::once(async move { Ok(Bytes::from(bytes)) }));
                store.put(&object.key, body, PutCondition::Any).await?;
            }
            op.next_object += 1;
            if op.next_object == op.objects.len() {
                op.state = OperationState::Published;
            }
            index.save_publication(&op)?;
        }
        Ok(Some(op.state))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn insufficient_read_budget_rejects_before_get() -> Result<(), Box<dyn std::error::Error>>
    {
        struct NoNetwork;
        impl ObjectStore for NoNetwork {
            fn capabilities(&self) -> crate::cloud::types::StorageCapabilities {
                panic!("network")
            }
            fn list<'a>(&'a self, _: &'a str) -> crate::cloud::types::ObjectListing<'a> {
                panic!("network")
            }
            fn stat<'a>(
                &'a self,
                _: &'a str,
            ) -> futures_util::future::BoxFuture<
                'a,
                Result<crate::cloud::types::ObjectMetadata, CloudError>,
            > {
                panic!("network")
            }
            fn get<'a>(
                &'a self,
                _: &'a str,
                _: Option<std::ops::Range<u64>>,
            ) -> futures_util::future::BoxFuture<
                'a,
                Result<crate::cloud::types::ObjectStream, CloudError>,
            > {
                panic!("GET before reservation")
            }
            fn put<'a>(
                &'a self,
                _: &'a str,
                _: crate::cloud::types::ObjectStream,
                _: PutCondition,
            ) -> futures_util::future::BoxFuture<
                'a,
                Result<crate::cloud::types::ObjectMetadata, CloudError>,
            > {
                panic!("network")
            }
            fn delete<'a>(
                &'a self,
                _: &'a str,
            ) -> futures_util::future::BoxFuture<'a, Result<(), CloudError>> {
                panic!("network")
            }
        }
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("index");
        let mut index = AppendIndex::open(&path, zeroize::Zeroizing::new([9; 32]), "repo".into())?;
        let wire = b"frozen".to_vec();
        let op = FrozenOperation {
            closure_validation_version: 1,
            operation_id: "read-limit".into(),
            parents_frontier: vec![],
            root: ObjectRef {
                kind: "tree".into(),
                digest: "0".repeat(64),
            },
            objects: vec![FrozenObject {
                key: "object".into(),
                digest: hex::encode(Sha256::digest(&wire)),
                len: wire.len(),
                chunks: 1,
            }],
            state: OperationState::Prepared,
            next_object: 0,
            requests: 0,
            bytes_written: 0,
        };
        index.freeze_publication(&op, &[PreparedObject::new("object".into(), wire.clone())])?;
        for remaining in [wire.len() - 1, 0] {
            let budget = DiscoveryBudget {
                bytes_read: MAX_DISCOVERY_READ_BYTES - remaining,
                ..DiscoveryBudget::default()
            };
            index.save_discovery_budget(&op.operation_id, &budget)?;
            for _ in 0..2 {
                assert!(matches!(
                    AppendPublisher::resume(
                        &mut index,
                        &NoNetwork,
                        &op.operation_id,
                        &AtomicBool::new(false)
                    )
                    .await,
                    Err(PublishError::Budget)
                ));
                assert_eq!(
                    serde_json::to_value(index.discovery_budget(&op.operation_id)?)?,
                    serde_json::to_value(&budget)?
                );
                assert_eq!(
                    index
                        .frozen_operation(&op.operation_id)?
                        .ok_or("missing operation")?,
                    op
                );
                drop(index);
                index = AppendIndex::open(&path, zeroize::Zeroizing::new([9; 32]), "repo".into())?;
            }
        }
        Ok(())
    }

    #[test]
    fn cumulative_budget_exact_boundaries_and_plus_one() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let mut index = AppendIndex::open(
            &dir.path().join("index"),
            zeroize::Zeroizing::new([9; 32]),
            "repo".into(),
        )?;
        let mut op = FrozenOperation {
            closure_validation_version: 1,
            operation_id: "op".into(),
            parents_frontier: vec![],
            root: ObjectRef {
                kind: "tree".into(),
                digest: "0".repeat(64),
            },
            objects: vec![],
            state: OperationState::Prepared,
            next_object: 0,
            requests: MAX_PUBLISH_REQUESTS - 1,
            bytes_written: MAX_PUBLISH_WRITE - 1,
        };
        index.freeze_publication(&op, &[])?;
        index.save_discovery_budget(
            "op",
            &DiscoveryBudget {
                requests: MAX_DISCOVERY_REQUESTS - 1,
                bytes_read: MAX_DISCOVERY_READ_BYTES - 1,
                ..DiscoveryBudget::default()
            },
        )?;
        AppendPublisher::charge(&mut index, &mut op, 1, 1)?;
        assert_eq!(
            index.discovery_budget("op")?.bytes_read,
            MAX_DISCOVERY_READ_BYTES
        );
        assert!(matches!(
            AppendPublisher::charge(&mut index, &mut op, 0, 1),
            Err(PublishError::Budget)
        ));
        drop(index);
        let mut index = AppendIndex::open(
            &dir.path().join("index"),
            zeroize::Zeroizing::new([9; 32]),
            "repo".into(),
        )?;
        assert!(matches!(
            AppendPublisher::charge(&mut index, &mut op, 0, 1),
            Err(PublishError::Budget)
        ));
        assert_eq!(op.requests, MAX_PUBLISH_REQUESTS);
        assert_eq!(op.bytes_written, MAX_PUBLISH_WRITE);
        assert!(matches!(
            AppendPublisher::charge(&mut index, &mut op, 0, 0),
            Err(PublishError::Budget)
        ));
        assert_eq!(
            index.frozen_operation("op")?.ok_or("missing operation")?,
            op
        );
        let mut bytes_op = op.clone();
        bytes_op.operation_id = "bytes".into();
        bytes_op.requests = 0;
        index.freeze_publication(&bytes_op, &[])?;
        assert!(matches!(
            AppendPublisher::charge(&mut index, &mut bytes_op, 1, 0),
            Err(PublishError::Budget)
        ));
        assert_eq!(
            index
                .frozen_operation("bytes")?
                .ok_or("missing operation")?,
            bytes_op
        );
        Ok(())
    }
}
