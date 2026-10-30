use sha2::{Digest, Sha256};
use std::sync::Arc;
use std::time::Duration;

use super::index::SyncIndex;
use super::index_types::{RunJournal, RunPhase};
use super::merge;
use super::merge_types::{MergeSet, Node, RevisionValue};
use super::publisher_types::{
    CommitDto, PubError, PublishAnchor, PublishBlock, PublishControl, PublishResult,
};
use super::repository::{Repository, RepositoryError};
use super::repository_types::{decode_metadata, hex_exact};

const MAX_ANCESTORS: usize = 128;

pub async fn publish(
    repository: Arc<Repository>,
    index: &mut SyncIndex,
    operation_id: &str,
    config_revision: u64,
    local_snapshot: MergeSet,
    blocks: &[PublishBlock],
    control: &PublishControl,
) -> Result<PublishResult, PubError> {
    if !valid_operation_id(operation_id) {
        return Err(PubError::MissingDependency);
    }
    let journal = RunJournal {
        operation_id: operation_id.to_owned(),
        config_revision,
        phase: RunPhase::Uploading,
        published_commit: None,
    };
    index.save("run", operation_id, &journal)?;
    for block in blocks {
        if !control.allowed() {
            return Err(PubError::Cancelled);
        }
        repository.put_block(&block.content).await?;
    }
    if !control.allowed() {
        return Err(PubError::Cancelled);
    }
    index.save(
        "run",
        operation_id,
        &RunJournal {
            phase: RunPhase::Publishing,
            ..journal.clone()
        },
    )?;
    let mut candidate = local_snapshot;
    let mut reuse = super::tree_types::TreeReuse::default();
    for attempt in 0..4_u32 {
        if !control.allowed() {
            return Err(PubError::Cancelled);
        }
        let (head, etag) = repository.read_head_revision().await?;
        check_anchor(index, &head)?;
        if let Some(commit) = head.commit.as_deref() {
            if let Some((_found_id, _)) = find_operation(&repository, commit, operation_id).await? {
                let loaded =
                    super::tree::load_commit_snapshot(&repository, commit, control).await?;
                let latest = loaded.snapshot;
                merge::project(&latest.revisions)?;
                verify_dependencies(&repository, &latest, control).await?;
                index.save(
                    "run",
                    operation_id,
                    &RunJournal {
                        phase: RunPhase::PublishedPendingApply,
                        published_commit: Some(commit.to_owned()),
                        ..journal.clone()
                    },
                )?;
                index.save(
                    "anchor",
                    "head",
                    &PublishAnchor {
                        generation: head.generation,
                        commit: head.commit.clone(),
                    },
                )?;
                return Ok(PublishResult {
                    commit: commit.to_owned(),
                    generation: head.generation,
                    recovered_operation_found: true,
                });
            }
            let loaded = super::tree::load_commit_snapshot(&repository, commit, control).await?;
            candidate = merge::join(&candidate, &loaded.snapshot)?;
            reuse.absorb(loaded.reuse);
        }
        merge::validate(&candidate.revisions)?;
        verify_dependencies(&repository, &candidate, control).await?;
        if !control.allowed() {
            return Err(PubError::Cancelled);
        }
        let tree_id = super::tree::write_tree(&repository, &candidate, &mut reuse, control).await?;
        let parents = head.commit.iter().cloned().collect::<Vec<_>>();
        validate_parents(&parents)?;
        let commit = serde_json::to_vec(&CommitDto {
            version: 1,
            operation_id: operation_id.to_owned(),
            parents,
            tree: tree_id,
        })
        .map_err(PubError::Serialization)?;
        let commit_id = repository.put_metadata("commit", &commit).await?;
        if !control.allowed() {
            return Err(PubError::Cancelled);
        }
        match repository
            .cas_head(
                &etag,
                head.generation.checked_add(1).ok_or(PubError::RetryLimit)?,
                &commit_id,
            )
            .await
        {
            Ok(new_head) => {
                index.save(
                    "run",
                    operation_id,
                    &RunJournal {
                        phase: RunPhase::PublishedPendingApply,
                        published_commit: Some(commit_id.clone()),
                        ..journal.clone()
                    },
                )?;
                index.save(
                    "anchor",
                    "head",
                    &PublishAnchor {
                        generation: new_head.generation,
                        commit: Some(commit_id.clone()),
                    },
                )?;
                return Ok(PublishResult {
                    commit: commit_id,
                    generation: new_head.generation,
                    recovered_operation_found: false,
                });
            }
            Err(RepositoryError::Cloud(crate::cloud::types::CloudError::Precondition))
                if attempt < 3 =>
            {
                tokio::time::sleep(Duration::from_millis(10 * (u64::from(attempt) + 1))).await;
            }
            Err(e) => {
                // A successful CAS can be reported as a transport/storage failure. Recover by operation id.
                if let Ok((latest, _)) = repository.read_head_revision().await {
                    check_anchor(index, &latest)?;
                    if let Some((found, _)) = find_operation(
                        &repository,
                        latest.commit.as_deref().unwrap_or(""),
                        operation_id,
                    )
                    .await?
                    {
                        index.save(
                            "run",
                            operation_id,
                            &RunJournal {
                                phase: RunPhase::PublishedPendingApply,
                                published_commit: Some(found.clone()),
                                ..journal.clone()
                            },
                        )?;
                        index.save(
                            "anchor",
                            "head",
                            &PublishAnchor {
                                generation: latest.generation,
                                commit: latest.commit.clone(),
                            },
                        )?;
                        return Ok(PublishResult {
                            commit: latest.commit.clone().ok_or(PubError::RetryLimit)?,
                            generation: latest.generation,
                            recovered_operation_found: true,
                        });
                    }
                }
                return Err(e.into());
            }
        }
    }
    Err(PubError::RetryLimit)
}

fn check_anchor(index: &SyncIndex, head: &super::repository_types::Head) -> Result<(), PubError> {
    let Some(anchor) = index.load::<PublishAnchor>("anchor", "head")? else {
        return Ok(());
    };
    if head.generation < anchor.generation
        || (head.generation == anchor.generation && head.commit != anchor.commit)
    {
        return Err(PubError::Rollback);
    }
    Ok(())
}

async fn verify_dependencies(
    repository: &Repository,
    set: &MergeSet,
    control: &PublishControl,
) -> Result<(), PubError> {
    for revision in set.revisions.values() {
        if !control.allowed() {
            return Err(PubError::Cancelled);
        }
        if let RevisionValue::Live {
            node: Node::File(file),
            ..
        } = &revision.value
        {
            let mut whole = Sha256::new();
            for block in &file.blocks {
                if !control.allowed() {
                    return Err(PubError::Cancelled);
                }
                let len = usize::try_from(block.len).map_err(|_| PubError::MissingDependency)?;
                let content = repository
                    .get_block(&block.id, len)
                    .await
                    .map_err(|error| match error {
                        RepositoryError::NotFound
                        | RepositoryError::Cloud(crate::cloud::types::CloudError::NotFound)
                        | RepositoryError::Corrupt("invalid block id") => {
                            PubError::MissingDependency
                        }
                        other => PubError::Dependency(other),
                    })?;
                whole.update(content.as_slice());
            }
            if hex::encode(whole.finalize()) != file.digest {
                return Err(PubError::MissingDependency);
            }
        }
    }
    Ok(())
}

fn valid_operation_id(id: &str) -> bool {
    (id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit()))
        || (id.len() == 36
            && id.as_bytes().iter().enumerate().all(|(i, b)| {
                [8, 13, 18, 23].contains(&i) && *b == b'-'
                    || ![8, 13, 18, 23].contains(&i) && b.is_ascii_hexdigit()
            }))
}
fn validate_parents(parents: &[String]) -> Result<(), PubError> {
    if parents.len() > 2
        || parents.windows(2).any(|p| p[0] == p[1])
        || parents.iter().any(|p| hex_exact::<32>(p).is_err())
    {
        Err(PubError::MissingDependency)
    } else {
        Ok(())
    }
}

async fn find_operation(
    repository: &Repository,
    start: &str,
    operation_id: &str,
) -> Result<Option<(String, CommitDto)>, PubError> {
    if start.is_empty() {
        return Ok(None);
    }
    let mut pending = vec![start.to_owned()];
    let mut visited = std::collections::BTreeSet::new();
    for _ in 0..MAX_ANCESTORS {
        let Some(id) = pending.pop() else {
            return Ok(None);
        };
        if !visited.insert(id.clone()) {
            continue;
        }
        let raw = repository.get_metadata("commit", &id).await?;
        let dto: CommitDto = decode_metadata(&raw).map_err(|_| PubError::MissingDependency)?;
        if dto.version != 1 || !valid_operation_id(&dto.operation_id) {
            return Err(PubError::MissingDependency);
        }
        validate_parents(&dto.parents)?;
        if dto.operation_id == operation_id {
            return Ok(Some((id, dto)));
        }
        pending.extend(dto.parents);
    }
    if pending.is_empty() {
        Err(PubError::ResourceLimit)
    } else {
        Err(PubError::ResourceLimit)
    }
}
