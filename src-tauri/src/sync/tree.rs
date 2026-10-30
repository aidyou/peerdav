use std::collections::{BTreeMap, BTreeSet};

use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use super::merge;
use super::merge_types::{EntityId, MAX_ENTITIES, MAX_REVISIONS, MergeSet, Node, RevisionValue};
use super::publisher_types::{CommitDto, PublishControl};
use super::repository::Repository;
use super::repository_types::{decode_metadata, hex_exact};
use super::tree_types::{
    BorrowedTreeDto, BorrowedTreeKind, DirectoryRefDto, EntityHistoryDto, LoadedTree,
    PlannedDirectory, TREE_NODE_LIMIT, TREE_OBJECT_LIMIT, TREE_SCHEMA_VERSION, TREE_TOTAL_LIMIT,
    TreeDto, TreeError, TreeKind, TreeOwner, TreeReuse,
};

fn root_owner() -> TreeOwner {
    TreeOwner::Directory(EntityId("root".into()))
}
fn allowed(control: &PublishControl) -> Result<(), TreeError> {
    if control.allowed() {
        Ok(())
    } else {
        Err(TreeError::Cancelled)
    }
}
fn valid_id(id: &str) -> Result<(), TreeError> {
    hex_exact::<32>(id).map(|_| ()).map_err(TreeError::Wire)
}

/// Physical ownership is canonical, not a causal winner. Each entity's complete
/// history stays together, including tombstones and revisions of former names.
fn plan(
    snapshot: &MergeSet,
    control: &PublishControl,
) -> Result<BTreeMap<TreeOwner, PlannedDirectory>, TreeError> {
    allowed(control)?;
    merge::validate(&snapshot.revisions)?;
    let root = EntityId("root".into());
    let mut histories = BTreeMap::<EntityId, Vec<_>>::new();
    let mut directories = BTreeSet::from([root.clone()]);
    for revision in snapshot.revisions.values() {
        allowed(control)?;
        if matches!(
            revision.value,
            RevisionValue::Live {
                node: Node::Dir,
                ..
            }
        ) {
            directories.insert(revision.entity.clone());
        }
        histories
            .entry(revision.entity.clone())
            .or_default()
            .push(revision.clone());
    }
    let mut owners = BTreeMap::<EntityId, TreeOwner>::new();
    for (entity, history) in &histories {
        let selected = history
            .iter()
            .rev()
            .find_map(|revision| match &revision.value {
                RevisionValue::Live { parent_dir, .. } => Some(parent_dir),
                RevisionValue::Tombstone => None,
            });
        let owner = match selected {
            Some(parent) if parent != entity && directories.contains(parent) => {
                TreeOwner::Directory(parent.clone())
            }
            _ => TreeOwner::Recovery,
        };
        owners.insert(
            entity.clone(),
            if entity == &root { root_owner() } else { owner },
        );
    }
    // Functional directory ownership may contain historical move cycles. Cut
    // every cycle member to the typed recovery container, without changing DAGs.
    let mut finished = BTreeSet::from([root.clone()]);
    for directory in &directories {
        allowed(control)?;
        let mut positions = BTreeMap::new();
        let mut path = Vec::<EntityId>::new();
        let mut current = directory.clone();
        loop {
            if finished.contains(&current) {
                break;
            }
            if let Some(start) = positions.get(&current).copied() {
                for cyclic in &path[start..] {
                    owners.insert(cyclic.clone(), TreeOwner::Recovery);
                }
                break;
            }
            positions.insert(current.clone(), path.len());
            path.push(current.clone());
            match owners.get(&current) {
                Some(TreeOwner::Directory(parent)) => current = parent.clone(),
                _ => break,
            }
        }
        finished.extend(path);
    }
    let mut nodes = BTreeMap::<TreeOwner, PlannedDirectory>::new();
    nodes.insert(root_owner(), PlannedDirectory::default());
    for directory in &directories {
        nodes
            .entry(TreeOwner::Directory(directory.clone()))
            .or_default();
    }
    for (entity, revisions) in histories {
        let owner = owners
            .get(&entity)
            .ok_or(TreeError::Invalid("missing entity owner"))?
            .clone();
        let node = nodes.entry(owner).or_default();
        if directories.contains(&entity) && entity != root {
            node.children.push(entity.clone());
        }
        node.entries.push(EntityHistoryDto { entity, revisions });
    }
    let recovery = nodes.contains_key(&TreeOwner::Recovery);
    nodes
        .get_mut(&root_owner())
        .ok_or(TreeError::Invalid("missing root"))?
        .recovery = recovery;
    if nodes.len() > TREE_NODE_LIMIT {
        return Err(TreeError::ResourceLimit("objects"));
    }
    Ok(nodes)
}

fn dto_for<'a>(
    owner: &'a TreeOwner,
    node: &'a PlannedDirectory,
    ids: &BTreeMap<TreeOwner, String>,
) -> Result<BorrowedTreeDto<'a>, TreeError> {
    let children = node
        .children
        .iter()
        .map(|directory| {
            let object = ids
                .get(&TreeOwner::Directory(directory.clone()))
                .ok_or(TreeError::Invalid("missing child object"))?
                .clone();
            Ok(DirectoryRefDto {
                dir: directory.clone(),
                object,
            })
        })
        .collect::<Result<Vec<_>, TreeError>>()?;
    let tree = match owner {
        TreeOwner::Directory(directory) => BorrowedTreeKind::Directory {
            directory,
            entries: &node.entries,
            children,
            recovery: if node.recovery {
                Some(
                    ids.get(&TreeOwner::Recovery)
                        .ok_or(TreeError::Invalid("missing recovery object"))?
                        .clone(),
                )
            } else {
                None
            },
        },
        TreeOwner::Recovery => BorrowedTreeKind::Recovery {
            entries: &node.entries,
            children,
        },
    };
    Ok(BorrowedTreeDto {
        version: TREE_SCHEMA_VERSION,
        tree,
    })
}

pub async fn write_tree(
    repo: &Repository,
    snapshot: &MergeSet,
    reuse: &mut TreeReuse,
    control: &PublishControl,
) -> Result<String, TreeError> {
    let nodes = plan(snapshot, control)?;
    // Preflight every node using fixed-width placeholder IDs, before any PUT.
    let placeholders = nodes
        .keys()
        .cloned()
        .map(|owner| (owner, "0".repeat(64)))
        .collect::<BTreeMap<_, _>>();
    let mut total = 0usize;
    for (owner, node) in &nodes {
        allowed(control)?;
        let plaintext = Zeroizing::new(serde_json::to_vec(&dto_for(owner, node, &placeholders)?)?);
        let len = plaintext.len();
        if len > TREE_OBJECT_LIMIT {
            return Err(TreeError::ResourceLimit("single directory manifest"));
        }
        total = total
            .checked_add(len)
            .ok_or(TreeError::ResourceLimit("plaintext"))?;
        if total > TREE_TOTAL_LIMIT {
            return Err(TreeError::ResourceLimit("plaintext"));
        }
    }
    let mut cache_bytes: usize = reuse.0.values().map(|(_, bytes)| bytes.len()).sum();
    let mut stack = vec![(root_owner(), false)];
    let mut ids = BTreeMap::new();
    while let Some((owner, ready)) = stack.pop() {
        allowed(control)?;
        let node = nodes
            .get(&owner)
            .ok_or(TreeError::Invalid("missing directory"))?;
        if !ready {
            stack.push((owner.clone(), true));
            if node.recovery {
                stack.push((TreeOwner::Recovery, false));
            }
            for child in node.children.iter().rev() {
                stack.push((TreeOwner::Directory(child.clone()), false));
            }
            if stack.len() > TREE_NODE_LIMIT * 2 {
                return Err(TreeError::ResourceLimit("traversal"));
            }
            continue;
        }
        let body = Zeroizing::new(serde_json::to_vec(&dto_for(&owner, node, &ids)?)?);
        let digest = hex::encode(Sha256::digest(body.as_slice()));
        let id = match reuse.0.get(&digest) {
            Some((id, old)) if old.as_slice() == body.as_slice() => id.clone(),
            _ => {
                allowed(control)?;
                let id = repo.put_metadata("tree", &body).await?;
                if cache_bytes.saturating_add(body.len()) > TREE_TOTAL_LIMIT {
                    reuse.0.clear();
                    cache_bytes = 0;
                }
                cache_bytes += body.len();
                reuse.0.insert(digest, (id.clone(), body));
                id
            }
        };
        ids.insert(owner, id);
    }
    ids.remove(&root_owner())
        .ok_or(TreeError::Invalid("missing root object"))
}

pub async fn load_tree(
    repo: &Repository,
    root: &str,
    control: &PublishControl,
) -> Result<LoadedTree, TreeError> {
    valid_id(root)?;
    let mut stack = vec![(root.to_owned(), root_owner(), 0usize)];
    let mut visited = BTreeSet::from([root.to_owned()]);
    let mut owners = BTreeSet::from([root_owner()]);
    let mut actual = BTreeMap::new();
    let mut revisions = BTreeMap::new();
    let mut entities = BTreeSet::new();
    let mut total = 0usize;
    let mut reuse = TreeReuse::default();
    while let Some((id, owner, depth)) = stack.pop() {
        allowed(control)?;
        if depth > TREE_NODE_LIMIT {
            return Err(TreeError::ResourceLimit("depth"));
        }
        let bytes = repo.get_metadata("tree", &id).await?;
        total = total
            .checked_add(bytes.len())
            .ok_or(TreeError::ResourceLimit("plaintext"))?;
        if bytes.len() > TREE_OBJECT_LIMIT || total > TREE_TOTAL_LIMIT {
            return Err(TreeError::ResourceLimit("plaintext"));
        }
        let dto: TreeDto = decode_metadata(&bytes)?;
        if dto.version != TREE_SCHEMA_VERSION {
            return Err(TreeError::Unsupported);
        }
        let (entries, children, recovery) = match dto.tree {
            TreeKind::Directory {
                directory,
                entries,
                children,
                recovery,
            } if owner == TreeOwner::Directory(directory.clone()) => (entries, children, recovery),
            TreeKind::Recovery { entries, children } if owner == TreeOwner::Recovery => {
                (entries, children, None)
            }
            _ => {
                return Err(TreeError::Invalid(
                    "directory identity or object kind mismatch",
                ));
            }
        };
        if recovery.is_some() && owner != root_owner() {
            return Err(TreeError::Invalid("non-root recovery reference"));
        }
        if entries
            .windows(2)
            .any(|pair| pair[0].entity >= pair[1].entity)
            || children.windows(2).any(|pair| pair[0].dir >= pair[1].dir)
        {
            return Err(TreeError::Invalid(
                "unsorted or duplicate directory entries",
            ));
        }
        for history in &entries {
            allowed(control)?;
            if history.revisions.is_empty()
                || history
                    .revisions
                    .windows(2)
                    .any(|pair| pair[0].revision_id >= pair[1].revision_id)
                || !entities.insert(history.entity.clone())
            {
                return Err(TreeError::Invalid(
                    "empty, duplicate or unsorted entity history",
                ));
            }
            if entities.len() > MAX_ENTITIES {
                return Err(TreeError::ResourceLimit("entities"));
            }
            for revision in &history.revisions {
                if revision.entity != history.entity
                    || revisions
                        .insert(revision.revision_id.clone(), revision.clone())
                        .is_some()
                {
                    return Err(TreeError::Invalid(
                        "revision ownership or duplicate revision",
                    ));
                }
                if revisions.len() > MAX_REVISIONS {
                    return Err(TreeError::ResourceLimit("revisions"));
                }
            }
        }
        let mut refs = children
            .iter()
            .map(|child| {
                (
                    child.object.clone(),
                    TreeOwner::Directory(child.dir.clone()),
                )
            })
            .collect::<Vec<_>>();
        if let Some(ref recovery_id) = recovery {
            refs.push((recovery_id.clone(), TreeOwner::Recovery));
        }
        for (object, child_owner) in refs {
            valid_id(&object)?;
            if !visited.insert(object.clone()) || !owners.insert(child_owner.clone()) {
                return Err(TreeError::Invalid(
                    "duplicate or cyclic directory reference",
                ));
            }
            if visited.len() > TREE_NODE_LIMIT {
                return Err(TreeError::ResourceLimit("objects"));
            }
            stack.push((object, child_owner, depth + 1));
        }
        actual.insert(
            owner,
            PlannedDirectory {
                entries,
                children: children.into_iter().map(|child| child.dir).collect(),
                recovery: recovery.is_some(),
            },
        );
        reuse
            .0
            .insert(hex::encode(Sha256::digest(bytes.as_slice())), (id, bytes));
    }
    let snapshot = MergeSet { revisions };
    if plan(&snapshot, control)? != actual {
        return Err(TreeError::Invalid("non-canonical directory ownership"));
    }
    Ok(LoadedTree { snapshot, reuse })
}

pub async fn load_commit_snapshot(
    repo: &Repository,
    commit_id: &str,
    control: &PublishControl,
) -> Result<LoadedTree, TreeError> {
    valid_id(commit_id)?;
    allowed(control)?;
    let raw = repo.get_metadata("commit", commit_id).await?;
    let commit: CommitDto = decode_metadata(&raw)?;
    if commit.version != 1 {
        return Err(TreeError::Unsupported);
    }
    let operation = &commit.operation_id;
    let valid_operation = (operation.len() == 32
        && operation.bytes().all(|byte| byte.is_ascii_hexdigit()))
        || (operation.len() == 36
            && operation.bytes().enumerate().all(|(index, byte)| {
                if [8, 13, 18, 23].contains(&index) {
                    byte == b'-'
                } else {
                    byte.is_ascii_hexdigit()
                }
            }));
    if !valid_operation
        || commit.parents.len() > 2
        || commit.parents.windows(2).any(|pair| pair[0] >= pair[1])
    {
        return Err(TreeError::Invalid("commit operation or parents"));
    }
    for parent in &commit.parents {
        valid_id(parent)?;
    }
    load_tree(repo, &commit.tree, control).await
}
