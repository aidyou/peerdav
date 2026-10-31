use super::append_index::{AppendIndex, KnownRecord};
pub use super::append_types::DiscoveryBudget;
use super::append_types::DiscoveryVisit as Visit;
use super::{append_repository::AppendRepository, append_types::*};
use crate::cloud::{traits::ObjectStore, types::ObjectMetadata};
use futures_util::StreamExt;
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::ops::{Deref, DerefMut};
use std::sync::Arc;

struct Meter<'a> {
    budget: &'a mut DiscoveryBudget,
    journal: Option<(&'a mut AppendIndex, &'a str)>,
}
impl Deref for Meter<'_> {
    type Target = DiscoveryBudget;
    fn deref(&self) -> &Self::Target {
        self.budget
    }
}
impl DerefMut for Meter<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.budget
    }
}
impl Meter<'_> {
    fn checkpoint(&mut self) -> Result<(), AppendError> {
        if let Some((index, operation)) = &mut self.journal {
            index.save_discovery_budget(operation, self.budget)?;
        }
        Ok(())
    }
}

/// Discover without publishing or applying. Every invocation reauthenticates;
/// local `Validated` markers alone are never accepted as closure evidence.
pub async fn discover<S: ObjectStore + ?Sized>(
    store: Arc<S>,
    binding: &TrustBinding,
    password: &[u8],
    index: &mut AppendIndex,
    operation: &str,
) -> Result<ValidatedClosure, AppendError> {
    if operation.is_empty() || index.repository_id() != binding.repo_id.as_bytes() {
        return Err(AppendError::Invalid("discovery binding"));
    }
    if !matches!(
        binding.source,
        TrustSource::TrustedFingerprint | TrustSource::AcceptedTofu
    ) {
        return Err(AppendError::TrustRejected);
    }
    let mut budget = index.discovery_budget(operation)?;
    let mut meter = Meter {
        budget: &mut budget,
        journal: Some((index, operation)),
    };
    let limits = DiscoveryLimits::default();
    let repo = open_metered(store.clone(), binding, password, &mut meter, &limits).await?;
    let prefix = format!("v2/{}/commits/", repo.repo_id());
    let mut token = None;
    let mut tokens = HashSet::new();
    loop {
        charge(&mut meter.requests, 1, MAX_DISCOVERY_REQUESTS, "requests")?;
        charge(&mut meter.pages, 1, MAX_DISCOVERY_PAGES, "list pages")?;
        meter.checkpoint()?;
        let page = store
            .list_page(&prefix, token.as_deref())
            .await
            .map_err(AppendError::IncompleteListing)?;
        charge(
            &mut meter.raw_entries,
            page.objects.len(),
            MAX_DISCOVERY_NODES,
            "candidates",
        )?;
        meter.checkpoint()?;
        for object in page.objects {
            allocation(&mut meter, expansion(object.key.len())?, &limits)?;
            commit_reference(&object.key, &prefix)?;
            if let Some((index, _)) = &mut meter.journal {
                index.save(&KnownRecord {
                    key: object.key,
                    state: DiscoveryState::Observed,
                    fingerprint: None,
                    error: None,
                })?;
            }
        }
        match page.next_token {
            None => break,
            Some(next) => {
                allocation(&mut meter, expansion(next.len())?, &limits)?;
                if !tokens.insert(next.clone()) {
                    return Err(AppendError::InvalidGraph("listing token cycle"));
                }
                token = Some(next);
            }
        }
    }
    // Known candidates are history, not frontier. Read and reserve one key at a
    // time without evicting entries or materializing full index records.
    let mut roots = Vec::new();
    let mut after = String::new();
    loop {
        allocation(&mut meter, 4096, &limits)?;
        let length = match &meter.journal {
            Some((index, _)) => index.next_known_key_len(&after)?,
            None => return Err(AppendError::Invalid("discovery journal")),
        };
        let Some(length) = length else { break };
        if roots.len() == MAX_DISCOVERY_NODES {
            return Err(AppendError::ResourceLimit("roots"));
        }
        allocation(&mut meter, expansion(length)?, &limits)?;
        let next = match &meter.journal {
            Some((index, _)) => index.next_known_key(&after)?,
            None => return Err(AppendError::Invalid("discovery journal")),
        };
        let Some(key) = next else { break };
        if roots.len() == MAX_DISCOVERY_NODES {
            return Err(AppendError::ResourceLimit("roots"));
        }
        allocation(&mut meter, expansion(key.len())?, &limits)?;
        roots.push(commit_reference(&key, &prefix)?);
        after = key;
    }
    let result = validate_metered(store, repo.repo_id(), &repo, &roots, &mut meter, &limits).await;
    if let Err(AppendError::ResourceLimit(_)) = &result {
        // The persisted meter already contains all completed reservations.
        return result;
    }
    meter.checkpoint()?;
    if let Some((index, _)) = &mut meter.journal {
        if let Ok(closure) = &result {
            for (digest, commit) in closure.commits() {
                let key = format!("{prefix}{}-{digest}", commit.operation_id);
                index.save(&KnownRecord {
                    key: key.clone(),
                    state: DiscoveryState::Validated,
                    fingerprint: Some(digest.clone()),
                    error: None,
                })?;
                index.save_verification(
                    &key,
                    &VerificationContext {
                        repo_id: repo.repo_id().into(),
                        anchor_fingerprint: binding.fingerprint.clone(),
                        object_digest: digest.clone(),
                        validator_version: 1,
                    },
                )?;
            }
        }
        for reference in roots {
            let key = format!("{prefix}{}-{}", reference.operation_id, reference.digest);
            let state = match &result {
                Ok(_) => DiscoveryState::Validated,
                Err(
                    AppendError::PendingDependency(_)
                    | AppendError::Storage(_)
                    | AppendError::ResourceLimit(_),
                ) => DiscoveryState::Pending,
                Err(_) => DiscoveryState::Invalid,
            };
            index.save(&KnownRecord {
                key: key.clone(),
                state,
                fingerprint: Some(reference.digest.clone()),
                error: result.as_ref().err().map(ToString::to_string),
            })?;
            if result.is_ok() {
                index.save_verification(
                    &key,
                    &VerificationContext {
                        repo_id: repo.repo_id().into(),
                        anchor_fingerprint: binding.fingerprint.clone(),
                        object_digest: reference.digest,
                        validator_version: 1,
                    },
                )?;
            }
        }
    }
    result
}

/// Open an engine anchor through exactly the same durable GET meter as discovery.
pub(crate) async fn open_budgeted<S: ObjectStore + ?Sized>(
    store: Arc<S>,
    binding: &TrustBinding,
    password: &[u8],
    index: &mut AppendIndex,
    operation: &str,
) -> Result<AppendRepository, AppendError> {
    let mut budget = index.discovery_budget(operation)?;
    let mut meter = Meter {
        budget: &mut budget,
        journal: Some((index, operation)),
    };
    open_metered(
        store,
        binding,
        password,
        &mut meter,
        &DiscoveryLimits::default(),
    )
    .await
}

async fn open_metered<S: ObjectStore + ?Sized>(
    store: Arc<S>,
    binding: &TrustBinding,
    password: &[u8],
    meter: &mut Meter<'_>,
    limits: &DiscoveryLimits,
) -> Result<AppendRepository, AppendError> {
    use sha2::{Digest, Sha256};
    if !matches!(
        binding.source,
        TrustSource::TrustedFingerprint | TrustSource::AcceptedTofu
    ) {
        return Err(AppendError::TrustRejected);
    }
    let bytes = get(store.as_ref(), &binding.key, meter, limits).await?;
    if hex::encode(Sha256::digest(&bytes)) != binding.fingerprint {
        return Err(AppendError::FingerprintMismatch);
    }
    let anchor: Anchor = serde_json::from_slice(&bytes)?;
    if serde_json::to_vec(&anchor)? != bytes
        || anchor.descriptor.repo_id != binding.repo_id
        || anchor.descriptor.mode != binding.mode
        || binding.key != format!("v2/anchors/{}-{}", binding.repo_id, binding.fingerprint)
    {
        return Err(AppendError::Invalid("anchor binding"));
    }
    AppendRepository::new(store, anchor, password)
}

fn commit_reference(key: &str, prefix: &str) -> Result<CommitRef, AppendError> {
    let name = key
        .strip_prefix(prefix)
        .ok_or(AppendError::Invalid("commit key"))?;
    let (operation_id, digest) = name
        .split_once('-')
        .ok_or(AppendError::Invalid("commit key"))?;
    if operation_id.len() != 32
        || digest.len() != 64
        || !operation_id
            .bytes()
            .chain(digest.bytes())
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(AppendError::Invalid("commit key"));
    }
    Ok(CommitRef {
        operation_id: operation_id.into(),
        digest: digest.into(),
    })
}

fn charge(
    counter: &mut usize,
    amount: usize,
    limit: usize,
    name: &'static str,
) -> Result<(), AppendError> {
    let next = counter
        .checked_add(amount)
        .ok_or(AppendError::ResourceLimit(name))?;
    if next > limit {
        return Err(AppendError::ResourceLimit(name));
    }
    *counter = next;
    Ok(())
}

// Deliberately conservative cumulative allocation units. Includes wire copies,
// decoding/JSON expansion and retained map/stack clones; never claims to be RSS.
fn allocation(
    budget: &mut Meter<'_>,
    size: usize,
    limits: &DiscoveryLimits,
) -> Result<(), AppendError> {
    charge(
        &mut budget.allocation_bytes,
        size,
        limits.max_allocation.min(MAX_DISCOVERY_ALLOCATION),
        "allocation bytes",
    )?;
    budget.checkpoint()
}

fn expansion(size: usize) -> Result<usize, AppendError> {
    size.checked_mul(16)
        .and_then(|n| n.checked_add(4096))
        .ok_or(AppendError::ResourceLimit("allocation bytes"))
}

fn metadata(
    budget: &mut Meter<'_>,
    bytes: &[u8],
    tree: bool,
    limits: &DiscoveryLimits,
) -> Result<(), AppendError> {
    let header: DiscoveryEnvelopeHeader = serde_json::from_slice(bytes)?;
    if header.plaintext_len > limits.append.max_metadata.min(MAX_METADATA_SIZE) {
        return Err(AppendError::ResourceLimit("metadata"));
    }
    if tree {
        charge(
            &mut budget.tree_plaintext,
            header.plaintext_len,
            limits.append.max_tree_total.min(MAX_TREE_SIZE),
            "tree plaintext",
        )?;
    }
    allocation(budget, expansion(bytes.len())?, limits)
}

fn snapshot_allocation(snapshot: &Snapshot) -> Result<usize, AppendError> {
    snapshot.values().try_fold(0usize, |sum, revision| {
        // Count serialized bytes without allocating a temporary serialization.
        let mut writer = CountingWriter(0);
        serde_json::to_writer(&mut writer, revision)?;
        sum.checked_add(expansion(writer.0)?)
            .ok_or(AppendError::ResourceLimit("allocation bytes"))
    })
}

impl std::io::Write for CountingWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0 = self
            .0
            .checked_add(bytes.len())
            .ok_or_else(|| std::io::Error::other("size overflow"))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub async fn list_candidates<S: ObjectStore + ?Sized>(
    store: &S,
    repo: &str,
    budget: &mut DiscoveryBudget,
) -> Result<Vec<ObjectMetadata>, AppendError> {
    let mut meter = Meter {
        budget,
        journal: None,
    };
    let budget = &mut meter;
    let prefix = format!("v2/{repo}/commits/");
    let mut token = None;
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    loop {
        if budget.pages >= MAX_DISCOVERY_PAGES {
            return Err(AppendError::ResourceLimit("list pages"));
        }
        charge(&mut budget.requests, 1, MAX_DISCOVERY_REQUESTS, "requests")?;
        budget.pages += 1;
        let page = store
            .list_page(&prefix, token.as_deref())
            .await
            .map_err(AppendError::Storage)?;
        charge(
            &mut budget.raw_entries,
            page.objects.len(),
            MAX_DISCOVERY_NODES,
            "candidates",
        )?;
        let size = page.objects.iter().try_fold(0usize, |n, object| {
            n.checked_add(std::mem::size_of::<ObjectMetadata>())
                .and_then(|n| n.checked_add(object.key.len()))
                .and_then(|n| n.checked_add(object.etag.as_ref().map_or(0, String::len)))
                .ok_or(AppendError::ResourceLimit("allocation bytes"))
        })?;
        allocation(budget, size, &DiscoveryLimits::default())?;
        out.extend(page.objects);
        match page.next_token {
            None => break,
            Some(next) => {
                if !seen.insert(next.clone()) {
                    return Err(AppendError::InvalidGraph("listing token cycle"));
                }
                token = Some(next);
            }
        }
    }
    Ok(out)
}

async fn get<S: ObjectStore + ?Sized>(
    store: &S,
    key: &str,
    budget: &mut Meter<'_>,
    limits: &DiscoveryLimits,
) -> Result<Vec<u8>, AppendError> {
    if let Some((index, _)) = &mut budget.journal {
        let prefix = format!(
            "v2/{}/commits/",
            String::from_utf8_lossy(index.repository_id())
        );
        if key.starts_with(&prefix) {
            let reference = commit_reference(key, &prefix)?;
            index.save(&KnownRecord {
                key: key.into(),
                state: DiscoveryState::Observed,
                fingerprint: Some(reference.digest),
                error: None,
            })?;
        }
    }
    read_bounded(store, key, |requests, bytes| {
        charge(
            &mut budget.requests,
            requests,
            MAX_DISCOVERY_REQUESTS,
            "requests",
        )?;
        charge(
            &mut budget.bytes_read,
            bytes,
            MAX_DISCOVERY_READ_BYTES,
            "read bytes",
        )?;
        if bytes != 0 {
            allocation(
                budget,
                bytes
                    .checked_mul(2)
                    .ok_or(AppendError::ResourceLimit("allocation bytes"))?,
                limits,
            )?;
        }
        budget.checkpoint()
    })
    .await
}

/// HEAD is a request too. Its charge commits before stat; GET's request and
/// declared wire bytes commit together before GET. No failure refunds work.
/// A changing object may shrink, but growing beyond HEAD fails closed.
pub(crate) async fn read_bounded<S: ObjectStore + ?Sized>(
    store: &S,
    key: &str,
    mut reserve: impl FnMut(usize, usize) -> Result<(), AppendError>,
) -> Result<Vec<u8>, AppendError> {
    let wire_limit = if key.starts_with("v2/anchors/") {
        64 * 1024
    } else if key.contains("/blocks/") {
        2 * (MAX_BLOCK_SIZE + 16) + 4096
    } else {
        2 * (MAX_METADATA_SIZE + 16) + 4096
    };
    reserve(1, 0)?;
    let metadata = store.stat(key).await.map_err(read_error)?;
    if metadata.key != key {
        return Err(AppendError::Invalid("stat key"));
    }
    let declared =
        usize::try_from(metadata.size).map_err(|_| AppendError::ResourceLimit("object wire"))?;
    if declared > wire_limit {
        return Err(AppendError::ResourceLimit("object wire"));
    }
    reserve(1, declared)?;
    let mut stream = store.get(key, None).await.map_err(read_error)?;
    let mut bytes = Vec::new();
    while let Some(part) = stream.next().await {
        let part = part.map_err(AppendError::Storage)?;
        if part.len() > declared.saturating_sub(bytes.len()) {
            return Err(AppendError::ResourceLimit("declared object length"));
        }
        bytes.extend_from_slice(&part);
    }
    Ok(bytes)
}

fn read_error(error: crate::cloud::types::CloudError) -> AppendError {
    match error {
        crate::cloud::types::CloudError::NotFound => AppendError::PendingDependency(error),
        _ => AppendError::Storage(error),
    }
}

type Snapshot = crate::sync::merge_types::RevisionMap;

pub async fn validate_closure<S: ObjectStore + ?Sized>(
    store: Arc<S>,
    repo_id: &str,
    repo: &AppendRepository,
    roots: Vec<CommitRef>,
    budget: &mut DiscoveryBudget,
) -> Result<ValidatedClosure, AppendError> {
    validate_closure_with_limits(
        store,
        repo_id,
        repo,
        roots,
        budget,
        &DiscoveryLimits::default(),
    )
    .await
}

pub async fn validate_closure_with_limits<S: ObjectStore + ?Sized>(
    store: Arc<S>,
    repo_id: &str,
    repo: &AppendRepository,
    roots: Vec<CommitRef>,
    budget: &mut DiscoveryBudget,
    limits: &DiscoveryLimits,
) -> Result<ValidatedClosure, AppendError> {
    let mut meter = Meter {
        budget,
        journal: None,
    };
    validate_metered(store, repo_id, repo, &roots, &mut meter, limits).await
}

async fn validate_metered<S: ObjectStore + ?Sized>(
    store: Arc<S>,
    repo_id: &str,
    repo: &AppendRepository,
    roots: &[CommitRef],
    budget: &mut Meter<'_>,
    limits: &DiscoveryLimits,
) -> Result<ValidatedClosure, AppendError> {
    if repo.repo_id() != repo_id {
        return Err(AppendError::Invalid("repository binding"));
    }
    if roots.len() > MAX_DISCOVERY_NODES {
        return Err(AppendError::ResourceLimit("roots"));
    }
    let mut commits: BTreeMap<String, Commit> = BTreeMap::new();
    let mut trees: BTreeMap<String, Tree> = BTreeMap::new();
    let mut snapshots: BTreeMap<String, Snapshot> = BTreeMap::new();
    let mut candidates = roots.iter();
    let mut stack: Vec<(CommitRef, usize, Visit)> = Vec::new();
    let mut active = BTreeSet::new();
    let mut completed = BTreeSet::new();
    loop {
        // Schedule one candidate at a time, not a full cloned roots stack.
        if stack.is_empty() {
            let Some(reference) = candidates.next() else {
                break;
            };
            allocation(budget, expansion(96)?, limits)?;
            stack.push((reference.clone(), 0, Visit::Enter));
        }
        let Some((reference, depth, phase)) = stack.pop() else {
            break;
        };
        if depth > limits.append.max_depth.min(MAX_TREE_DEPTH) {
            return Err(AppendError::InvalidGraph("depth"));
        }
        let digest = reference.digest.clone();
        match phase {
            Visit::Exit => {
                active.remove(&digest);
                let own = snapshots
                    .get(&digest)
                    .ok_or(AppendError::Invalid("commit snapshot"))?;
                let commit = commits
                    .get(&digest)
                    .ok_or(AppendError::Invalid("commit state"))?;
                for parent in &commit.parents {
                    let parent_snapshot = snapshots
                        .get(&parent.digest)
                        .ok_or(AppendError::InvalidGraph("parent ordering"))?;
                    for (id, value) in parent_snapshot {
                        if own.get(id) != Some(value) {
                            return Err(AppendError::Invalid("commit history"));
                        }
                    }
                }
                completed.insert(digest);
            }
            Visit::Enter => {
                if completed.contains(&digest) {
                    if commits
                        .get(&digest)
                        .is_none_or(|commit| commit.operation_id != reference.operation_id)
                    {
                        return Err(AppendError::Invalid("commit operation reference"));
                    }
                    continue;
                }
                if !active.insert(digest.clone()) {
                    return Err(AppendError::InvalidGraph("cycle"));
                }
                if commits.len() >= MAX_DISCOVERY_NODES {
                    return Err(AppendError::ResourceLimit("nodes"));
                }
                let key = format!("v2/{repo_id}/commits/{}-{}", reference.operation_id, digest);
                let bytes = get(store.as_ref(), &key, budget, limits).await?;
                let expected = ObjectRef {
                    kind: "commit".into(),
                    digest: digest.clone(),
                };
                metadata(budget, &bytes, false, limits)?;
                let commit = repo.open_commit(&bytes, &expected)?;
                if commit.operation_id != reference.operation_id {
                    return Err(AppendError::Invalid("commit operation reference"));
                }
                let tree_key = format!("v2/{repo_id}/trees/{}", commit.root.digest);
                let tree_bytes = get(store.as_ref(), &tree_key, budget, limits).await?;
                metadata(budget, &tree_bytes, true, limits)?;
                let tree = repo.open_tree(&tree_bytes, &commit.root)?;
                let snapshot = validate_tree_objects(
                    store.clone(),
                    repo,
                    repo_id,
                    tree,
                    &commit.root.digest,
                    &mut trees,
                    budget,
                    depth,
                    limits,
                )
                .await?;
                // Pending DFS work is not a maximal frontier. Bound its node
                // count and reserve clone/storage units before growing it.
                let pending = stack
                    .len()
                    .checked_add(commit.parents.len() + 1)
                    .ok_or(AppendError::ResourceLimit("nodes"))?;
                if pending > MAX_DISCOVERY_NODES {
                    return Err(AppendError::ResourceLimit("nodes"));
                }
                allocation(budget, expansion((commit.parents.len() + 1) * 96)?, limits)?;
                snapshots.insert(digest.clone(), snapshot);
                stack.push((reference, depth, Visit::Exit));
                for parent in commit.parents.iter().rev() {
                    stack.push((parent.clone(), depth + 1, Visit::Enter));
                }
                commits.insert(digest, commit);
            }
        }
    }
    // Only authenticated parent edges determine the final maximal frontier.
    // Borrow digests rather than cloning the entire validated DAG.
    allocation(
        budget,
        commits
            .len()
            .checked_mul(128)
            .ok_or(AppendError::ResourceLimit("allocation bytes"))?,
        limits,
    )?;
    let parents: BTreeSet<&str> = commits
        .values()
        .flat_map(|commit| commit.parents.iter().map(|parent| parent.digest.as_str()))
        .collect();
    if commits
        .keys()
        .filter(|digest| !parents.contains(digest.as_str()))
        .count()
        > 64
    {
        return Err(AppendError::ResourceLimit("frontier"));
    }
    Ok(ValidatedClosure::new(commits, trees))
}

async fn validate_tree_objects<S: ObjectStore + ?Sized>(
    store: Arc<S>,
    repo: &AppendRepository,
    repo_id: &str,
    root: Tree,
    root_digest: &str,
    trees: &mut BTreeMap<String, Tree>,
    budget: &mut Meter<'_>,
    depth: usize,
    limits: &DiscoveryLimits,
) -> Result<Snapshot, AppendError> {
    let mut stack = vec![(root, root_digest.to_owned(), depth, Visit::Enter)];
    let mut active = BTreeSet::new();
    let mut done = BTreeMap::<String, Snapshot>::new();
    while let Some((tree, digest, level, phase)) = stack.pop() {
        if level > limits.append.max_depth.min(MAX_TREE_DEPTH) {
            return Err(AppendError::InvalidGraph("depth"));
        }
        match phase {
            Visit::Exit => {
                active.remove(&digest);
                let mut snapshot = BTreeMap::new();
                for entry in &tree.entries {
                    let mut writer = CountingWriter(0);
                    serde_json::to_writer(&mut writer, entry)?;
                    allocation(budget, expansion(writer.0)?, limits)?;
                    if snapshot
                        .insert(entry.revision_id.clone(), entry.clone())
                        .is_some()
                    {
                        return Err(AppendError::Invalid("duplicate revision"));
                    }
                }
                for child in &tree.children {
                    let child_snapshot = done
                        .get(&child.object.digest)
                        .ok_or(AppendError::Invalid("tree child ordering"))?;
                    allocation(
                        budget,
                        snapshot_allocation(&snapshot)?
                            .checked_add(snapshot_allocation(child_snapshot)?)
                            .ok_or(AppendError::ResourceLimit("allocation bytes"))?,
                        limits,
                    )?;
                    let joined = crate::sync::merge::join(
                        &crate::sync::merge_types::MergeSet {
                            revisions: snapshot,
                        },
                        &crate::sync::merge_types::MergeSet {
                            revisions: child_snapshot.clone(),
                        },
                    )
                    .map_err(|_| AppendError::Invalid("tree history"))?;
                    snapshot = joined.revisions;
                }
                done.insert(digest.clone(), snapshot);
                trees.insert(digest, tree);
            }
            Visit::Enter => {
                if done.contains_key(&digest) {
                    continue;
                }
                if !active.insert(digest.clone()) {
                    return Err(AppendError::InvalidGraph("tree cycle"));
                }
                if trees.len() >= MAX_DISCOVERY_NODES {
                    return Err(AppendError::ResourceLimit("nodes"));
                }
                for binding in &tree.bindings {
                    let key = format!("v2/{repo_id}/blocks/{}", binding.object.digest);
                    let bytes = get(store.as_ref(), &key, budget, limits).await?;
                    allocation(budget, expansion(bytes.len())?, limits)?;
                    repo.open_block_with_ref(&bytes, Some(binding))?;
                }
                let mut writer = CountingWriter(0);
                serde_json::to_writer(&mut writer, &tree)?;
                allocation(budget, expansion(writer.0)?, limits)?;
                if stack
                    .len()
                    .saturating_add(tree.children.len())
                    .saturating_add(1)
                    > MAX_DISCOVERY_NODES
                {
                    return Err(AppendError::ResourceLimit("nodes"));
                }
                allocation(
                    budget,
                    expansion(
                        tree.children
                            .len()
                            .checked_mul(128)
                            .ok_or(AppendError::ResourceLimit("allocation bytes"))?,
                    )?,
                    limits,
                )?;
                stack.push((tree.clone(), digest.clone(), level, Visit::Exit));
                for child in tree.children.iter().rev() {
                    let key = format!("v2/{repo_id}/trees/{}", child.object.digest);
                    let bytes = get(store.as_ref(), &key, budget, limits).await?;
                    metadata(budget, &bytes, true, limits)?;
                    let child_tree = repo.open_tree(&bytes, &child.object)?;
                    if child_tree.directory != child.directory {
                        return Err(AppendError::Invalid("tree directory reference"));
                    }
                    stack.push((
                        child_tree,
                        child.object.digest.clone(),
                        level + 1,
                        Visit::Enter,
                    ));
                }
            }
        }
    }
    done.remove(root_digest)
        .ok_or(AppendError::Invalid("tree snapshot"))
}
