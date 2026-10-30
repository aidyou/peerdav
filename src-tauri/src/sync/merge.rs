pub use crate::sync::merge_types::*;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

fn body(
    entity: &EntityId,
    change: &ChangeId,
    parents: &[RevisionId],
    value: &RevisionValue,
) -> Result<Vec<u8>, MergeError> {
    let mut p = parents.to_vec();
    p.sort();
    let raw = serde_json::to_vec(&(entity, change, p, value)).map_err(MergeError::Serialization)?;
    let mut out = Vec::with_capacity(raw.len() + 32);
    out.extend_from_slice(b"peerdav-s3/revision/v1\0");
    out.extend((raw.len() as u64).to_be_bytes());
    out.extend(raw);
    Ok(out)
}
fn computed_id(
    entity: &EntityId,
    change: &ChangeId,
    parents: &[RevisionId],
    value: &RevisionValue,
) -> Result<RevisionId, MergeError> {
    Ok(RevisionId(hex::encode(Sha256::digest(body(
        entity, change, parents, value,
    )?))))
}
fn check_name(name: &str) -> Result<(), MergeError> {
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.contains('/')
        || name.contains('\\')
        || name.contains('\0')
    {
        return Err(MergeError::InvalidName(name.into()));
    }
    Ok(())
}
fn check_id(id: &str) -> Result<(), MergeError> {
    if id.is_empty() || id.len() > 512 || id.chars().any(|c| c.is_control()) {
        return Err(MergeError::InvalidId(id.into()));
    }
    Ok(())
}
fn check_value(v: &RevisionValue) -> Result<(), MergeError> {
    if let RevisionValue::Live {
        parent_dir,
        name,
        node,
    } = v
    {
        check_id(&parent_dir.0)?;
        check_name(name)?;
        if let Node::File(f) = node {
            if f.digest.len() != 64 || !f.digest.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(MergeError::InvalidId(f.digest.clone()));
            }
            if f.blocks.len() > 1_000_000 {
                return Err(MergeError::Limit("blocks".into()));
            }
            for block in &f.blocks {
                if block.id.len() != 64 || !block.id.bytes().all(|b| b.is_ascii_hexdigit()) {
                    return Err(MergeError::InvalidId(block.id.clone()));
                }
            }
            let total = f.blocks.iter().try_fold(0_u64, |total, block| {
                if block.len == 0 || block.len > 8 * 1024 * 1024 {
                    return Err(MergeError::Limit("block length".into()));
                }
                total
                    .checked_add(block.len)
                    .ok_or_else(|| MergeError::Limit("file length".into()))
            })?;
            if total != f.length {
                return Err(MergeError::Corrupt(RevisionId(f.digest.clone())));
            }
        }
    }
    Ok(())
}

fn validate_frontier(
    frontier: &[RevisionId],
    target: &EntityId,
    history: &RevisionMap,
) -> Result<(), MergeError> {
    if frontier.len() > MAX_FRONTIER || frontier.windows(2).any(|x| x[0] >= x[1]) {
        return Err(MergeError::Limit("frontier".into()));
    }
    for id in frontier {
        let r = history
            .get(id)
            .ok_or_else(|| MergeError::InvalidFrontier(id.clone()))?;
        if &r.entity != target {
            return Err(MergeError::ParentEntity(id.clone(), id.clone()));
        }
    }
    let mut seen = BTreeSet::new();
    let mut stack = frontier.to_vec();
    while let Some(id) = stack.pop() {
        if !seen.insert(id.clone()) {
            continue;
        }
        let r = history
            .get(&id)
            .ok_or_else(|| MergeError::InvalidFrontier(id.clone()))?;
        for p in &r.parents {
            stack.push(p.clone());
        }
    }
    for id in frontier {
        let current = history
            .get(id)
            .ok_or_else(|| MergeError::InvalidFrontier(id.clone()))?;
        let mut stack = current.parents.clone();
        let mut ancestors = BTreeSet::new();
        while let Some(x) = stack.pop() {
            if ancestors.insert(x.clone()) {
                if let Some(r) = history.get(&x) {
                    stack.extend(r.parents.clone());
                }
            }
        }
        if frontier.iter().any(|other| ancestors.contains(other)) {
            return Err(MergeError::InvalidFrontier(id.clone()));
        }
    }
    Ok(())
}

pub fn prepare_local_change(
    entity: EntityId,
    change_id: ChangeId,
    baseline: Baseline,
    value: RevisionValue,
    history: &RevisionMap,
    complete_scan: bool,
) -> Result<LocalChange, MergeError> {
    check_id(&entity.0)?;
    check_id(&change_id.0)?;
    check_value(&value)?;
    let parents = match baseline {
        Baseline::Unknown => {
            if matches!(value, RevisionValue::Tombstone) {
                return Err(MergeError::UnknownDelete);
            }
            Vec::new()
        }
        Baseline::Known { frontier } => {
            if matches!(value, RevisionValue::Tombstone) && !complete_scan {
                return Err(MergeError::IncompleteDelete);
            }
            validate_frontier(&frontier, &entity, history)?;
            frontier
        }
    };
    if parents.len() > MAX_PARENTS {
        return Err(MergeError::Limit("parents".into()));
    }
    Ok(LocalChange {
        entity,
        change_id,
        parents,
        value,
    })
}

pub fn revision(change: LocalChange) -> Result<Revision, MergeError> {
    let revision_id = computed_id(
        &change.entity,
        &change.change_id,
        &change.parents,
        &change.value,
    )?;
    Ok(Revision {
        entity: change.entity,
        change_id: change.change_id,
        parents: change.parents,
        value: change.value,
        revision_id,
    })
}

pub fn join(a: &MergeSet, b: &MergeSet) -> Result<MergeSet, MergeError> {
    let mut out = a.revisions.clone();
    for (id, r) in &b.revisions {
        if let Some(old) = out.get(id) {
            if old != r {
                return Err(MergeError::Corrupt(id.clone()));
            }
        } else {
            out.insert(id.clone(), r.clone());
        }
    }
    if out.len() > MAX_REVISIONS {
        return Err(MergeError::Limit("revisions".into()));
    }
    validate(&out)?;
    Ok(MergeSet { revisions: out })
}

pub fn validate(revisions: &RevisionMap) -> Result<(), MergeError> {
    if revisions.len() > MAX_REVISIONS {
        return Err(MergeError::Limit("revisions".into()));
    }
    let mut entities = BTreeSet::new();
    for (id, r) in revisions {
        check_id(&r.entity.0)?;
        check_id(&r.change_id.0)?;
        if &r.revision_id != id
            || computed_id(&r.entity, &r.change_id, &r.parents, &r.value)? != *id
        {
            return Err(MergeError::Corrupt(id.clone()));
        }
        if r.parents.len() > MAX_PARENTS || r.parents.windows(2).any(|x| x[0] >= x[1]) {
            return Err(MergeError::Corrupt(id.clone()));
        }
        check_value(&r.value)?;
        entities.insert(r.entity.clone());
        for p in &r.parents {
            let parent = revisions
                .get(p)
                .ok_or_else(|| MergeError::MissingParent(id.clone(), p.clone()))?;
            if parent.entity != r.entity {
                return Err(MergeError::ParentEntity(id.clone(), p.clone()));
            }
        }
    }
    if entities.len() > MAX_ENTITIES {
        return Err(MergeError::Limit("entities".into()));
    }
    let mut indegree: BTreeMap<RevisionId, usize> =
        revisions.keys().map(|id| (id.clone(), 0)).collect();
    let mut children: BTreeMap<RevisionId, Vec<RevisionId>> = BTreeMap::new();
    for (id, r) in revisions {
        for p in &r.parents {
            *indegree
                .get_mut(id)
                .ok_or_else(|| MergeError::Corrupt(id.clone()))? += 1;
            children.entry(p.clone()).or_default().push(id.clone());
        }
    }
    let mut ready: BTreeSet<_> = indegree
        .iter()
        .filter(|(_, n)| **n == 0)
        .map(|(id, _)| id.clone())
        .collect();
    let mut count = 0;
    while let Some(id) = ready.pop_first() {
        count += 1;
        if let Some(cs) = children.get(&id) {
            for c in cs {
                let n = indegree
                    .get_mut(c)
                    .ok_or_else(|| MergeError::Corrupt(c.clone()))?;
                *n -= 1;
                if *n == 0 {
                    ready.insert(c.clone());
                }
            }
        }
    }
    if count != revisions.len() {
        let id = indegree
            .into_iter()
            .find(|(_, n)| *n > 0)
            .map(|(id, _)| id)
            .unwrap_or_else(|| RevisionId("cycle".into()));
        return Err(MergeError::Cycle(id));
    }
    Ok(())
}

pub fn frontier(
    revisions: &RevisionMap,
) -> Result<BTreeMap<EntityId, Vec<RevisionId>>, MergeError> {
    validate(revisions)?;
    let mut result = BTreeMap::new();
    let mut maximal: BTreeMap<EntityId, BTreeSet<RevisionId>> = BTreeMap::new();
    for r in revisions.values() {
        maximal
            .entry(r.entity.clone())
            .or_default()
            .insert(r.revision_id.clone());
    }
    for r in revisions.values() {
        if let Some(set) = maximal.get_mut(&r.entity) {
            for p in &r.parents {
                set.remove(p);
            }
        }
    }
    for (entity, ids) in maximal {
        if ids.len() > MAX_FRONTIER {
            return Err(MergeError::Limit("frontier".into()));
        }
        result.insert(entity, ids.into_iter().collect());
    }
    Ok(result)
}

fn truncate_component(value: &str, max: usize) -> String {
    if value.len() <= max {
        return value.to_owned();
    }
    let mut end = max;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}

fn recovered(entity: &EntityId) -> String {
    format!(".recovered-{}", entity.0)
}

fn entity_path(
    entity: &EntityId,
    fs: &BTreeMap<EntityId, Vec<RevisionId>>,
    revisions: &RevisionMap,
) -> Result<Vec<String>, MergeError> {
    if entity.0 == "root" {
        return Ok(Vec::new());
    }
    let mut chain = Vec::new();
    let mut current = entity.clone();
    let mut seen = BTreeSet::new();
    for _ in 0..=128 {
        if current.0 == "root" {
            chain.reverse();
            return Ok(chain);
        }
        if !seen.insert(current.clone()) {
            return Err(MergeError::PathCycle(current));
        }
        // Resolve names from the current frontier, never from an obsolete
        // historical revision.  A deleted parent gets a stable recovery node
        // so children created concurrently with its deletion remain reachable.
        let ids = fs
            .get(&current)
            .ok_or_else(|| MergeError::UnknownParent(current.clone()))?;
        let current_revisions = ids
            .iter()
            .filter_map(|id| revisions.get(id))
            .collect::<Vec<_>>();
        if current_revisions.is_empty()
            || current_revisions
                .iter()
                .any(|r| matches!(r.value, RevisionValue::Tombstone))
        {
            chain.push(recovered(&current));
            // A tombstoned entity has no current parent, so its recovery node
            // is rooted at the nearest still-addressable ancestor.
            current = EntityId("root".into());
            continue;
        }
        let live = current_revisions
            .iter()
            .filter_map(|r| match &r.value {
                RevisionValue::Live {
                    parent_dir, name, ..
                } => Some((r.revision_id.clone(), parent_dir.clone(), name.clone())),
                RevisionValue::Tombstone => None,
            })
            .max_by(|a, b| a.0.cmp(&b.0))
            .ok_or_else(|| MergeError::UnknownParent(current.clone()))?;
        chain.push(live.2);
        current = live.1;
    }
    Err(MergeError::PathDepth)
}

fn source_path(
    entry: &Revision,
    fs: &BTreeMap<EntityId, Vec<RevisionId>>,
    revisions: &RevisionMap,
) -> Result<Vec<String>, MergeError> {
    match &entry.value {
        RevisionValue::Live {
            parent_dir, name, ..
        } => {
            let mut path = entity_path(parent_dir, fs, revisions)?;
            path.push(name.clone());
            Ok(path)
        }
        RevisionValue::Tombstone => {
            let source = revisions
                .values()
                .filter(|revision| {
                    revision.entity == entry.entity
                        && matches!(revision.value, RevisionValue::Live { .. })
                })
                .max_by_key(|revision| &revision.revision_id);
            if let Some(source) = source {
                source_path(source, fs, revisions)
            } else {
                Ok(vec![recovered(&entry.entity)])
            }
        }
    }
}

fn folded_component(component: &str) -> String {
    component.to_lowercase()
}

fn path_key(path: &[String]) -> Vec<String> {
    path.iter()
        .map(|component| folded_component(component))
        .collect()
}

fn collision_suffix(
    entry: &ProjectionEntry,
    base: &str,
    used: &BTreeSet<Vec<String>>,
) -> Result<Vec<String>, MergeError> {
    let domain = format!(
        "peerdav-s3/projection-alias/v1/{}/{}",
        entry.entity.0, entry.version.0
    );
    for attempt in 0_u32..=1024 {
        let digest = hex::encode(Sha256::digest(format!("{domain}/{attempt}").as_bytes()));
        let suffix = if attempt == 0 {
            format!("~{}", entry.version.0)
        } else {
            format!("~{}-{}", entry.version.0, &digest[..16])
        };
        let max = 255usize.saturating_sub(suffix.len());
        let mut candidate = truncate_component(base, max);
        candidate.push_str(&suffix);
        let mut path = entry.logical_path[..entry.logical_path.len().saturating_sub(1)].to_vec();
        path.push(candidate);
        if !used.contains(&path_key(&path)) {
            return Ok(path);
        }
    }
    Err(MergeError::Limit("projection aliases".into()))
}

pub fn project(revisions: &RevisionMap) -> Result<Projection, MergeError> {
    let fs = frontier(revisions)?;
    let mut entries = Vec::new();
    for (entity, ids) in &fs {
        let conflict = ids.len() > 1;
        for id in ids {
            let r = revisions
                .get(id)
                .ok_or_else(|| MergeError::Corrupt(id.clone()))?;
            entries.push(ProjectionEntry {
                entity: entity.clone(),
                version: id.clone(),
                logical_path: source_path(r, &fs, revisions)?,
                projection_path: Vec::new(),
                value: r.value.clone(),
                conflict,
            });
        }
    }
    // Stable ordering makes the projected view independent of join/device order.
    entries.sort_by(|a, b| {
        (a.logical_path.clone(), a.entity.clone(), a.version.clone()).cmp(&(
            b.logical_path.clone(),
            b.entity.clone(),
            b.version.clone(),
        ))
    });
    let mut used: BTreeSet<Vec<String>> = BTreeSet::new();
    for entry in &mut entries {
        // Tombstones remain in the projection for deletion targeting, but never
        // reserve a physical path or steal the live entry's ordinary name.
        if matches!(entry.value, RevisionValue::Tombstone) {
            entry.projection_path = entry.logical_path.clone();
            continue;
        }
        let mut path = entry
            .logical_path
            .iter()
            .map(|component| truncate_component(component, 255))
            .collect::<Vec<_>>();
        let key = path_key(&path);
        let deleted_concurrently = fs.get(&entry.entity).is_some_and(|ids| {
            ids.iter().any(|id| {
                revisions
                    .get(id)
                    .is_some_and(|revision| matches!(revision.value, RevisionValue::Tombstone))
            })
        });
        if deleted_concurrently || used.contains(&key) {
            let base = path.pop().unwrap_or_else(|| recovered(&entry.entity));
            path = collision_suffix(entry, &base, &used)?;
            entry.conflict = true;
        }
        used.insert(path_key(&path));
        entry.projection_path = path;
    }
    Ok(Projection { entries })
}
