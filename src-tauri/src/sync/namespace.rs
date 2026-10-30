use sha2::{Digest, Sha256};

use super::merge_types::{EntityId, MergeError, Projection};

/// Logical namespace prefix used in the shared merge graph.
pub fn components(mapping: &str) -> Result<Vec<String>, MergeError> {
    let parts = mapping.split('/').map(str::to_owned).collect::<Vec<_>>();
    if parts.is_empty() || parts.iter().any(|part| part.is_empty()) {
        return Err(MergeError::InvalidName(mapping.to_owned()));
    }
    for part in &parts {
        if part == "." || part == ".." || part.contains('\\') {
            return Err(MergeError::InvalidName(part.clone()));
        }
    }
    Ok(parts)
}

pub fn entity(logical_path: &[String]) -> EntityId {
    let mut hasher = Sha256::new();
    hasher.update(b"peerdav-s3/domainrepo_namespace_v1");
    for component in logical_path {
        hasher.update((component.len() as u64).to_be_bytes());
        hasher.update(component.as_bytes());
    }
    EntityId(hex::encode(hasher.finalize()))
}

pub fn project_scoped(full: &Projection, mapping: &str) -> Result<Projection, MergeError> {
    let prefix = components(mapping)?;
    let folded = prefix.iter().map(|x| x.to_lowercase()).collect::<Vec<_>>();
    let mut entries = Vec::new();
    for entry in &full.entries {
        let matches = entry.logical_path.len() >= prefix.len()
            && entry.logical_path[..prefix.len()] == prefix[..];
        let folded_matches = entry.logical_path.len() >= prefix.len()
            && entry.logical_path[..prefix.len()]
                .iter()
                .map(|x| x.to_lowercase())
                .eq(folded.iter().cloned());
        if folded_matches && !matches {
            return Err(MergeError::InvalidName(
                "namespace prefix case collision".into(),
            ));
        }
        if !matches {
            continue;
        }
        // The namespace directory itself is a graph anchor, not a local directory.
        if entry.logical_path.len() == prefix.len() {
            continue;
        }
        let mut projected = entry.clone();
        projected.logical_path = entry.logical_path[prefix.len()..].to_vec();
        projected.projection_path = entry.projection_path[prefix.len()..].to_vec();
        entries.push(projected);
    }
    Ok(Projection { entries })
}
