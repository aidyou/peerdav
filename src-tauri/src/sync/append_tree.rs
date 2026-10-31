use super::append_types::*;

pub fn canonicalize_tree(tree: &mut Tree) -> Result<(), AppendError> {
    if tree.version != APPEND_VERSION
        || tree.repo_id.len() != REPO_ID_LEN * 2
        || tree.directory.is_empty()
    {
        return Err(AppendError::Invalid("tree descriptor"));
    }
    tree.entries
        .sort_by(|a, b| a.revision_id.cmp(&b.revision_id));
    tree.bindings.sort_by(|a, b| {
        (&a.block_id, a.len, &a.object.digest).cmp(&(&b.block_id, b.len, &b.object.digest))
    });
    tree.bindings.dedup();
    tree.children
        .sort_by(|a, b| (&a.directory, &a.object.digest).cmp(&(&b.directory, &b.object.digest)));
    tree.children.dedup();
    let map = tree
        .entries
        .iter()
        .map(|r| (r.revision_id.clone(), r.clone()))
        .collect();
    crate::sync::merge::validate(&map).map_err(|_| AppendError::Invalid("tree history"))?;
    for r in &tree.entries {
        if let crate::sync::merge_types::RevisionValue::Live {
            node: crate::sync::merge_types::Node::File(f),
            ..
        } = &r.value
        {
            for b in &f.blocks {
                if !tree
                    .bindings
                    .iter()
                    .any(|x| x.block_id == b.id && x.len == b.len)
                {
                    return Err(AppendError::Invalid("missing block binding"));
                }
            }
        }
    }
    for b in &tree.bindings {
        validate_binding(b)?;
    }
    Ok(())
}

pub fn validate_binding(binding: &BlockBinding) -> Result<(), AppendError> {
    if binding.block_id.len() != BLOCK_ID_LEN * 2
        || binding.len > MAX_BLOCK_SIZE as u64
        || binding.object.kind != "block"
        || binding.object.digest.len() != BLOCK_ID_LEN * 2
        || !binding
            .block_id
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        || !binding
            .object
            .digest
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        return Err(AppendError::Invalid("invalid block binding"));
    }
    Ok(())
}
