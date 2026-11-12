use crate::file_watch_types::FileChange;
use notify::{Event, EventKind, RecursiveMode, Watcher, event::ModifyKind};
use std::path::Path;

pub(crate) fn content_event(kind: &EventKind) -> bool {
    matches!(kind, EventKind::Create(_) | EventKind::Remove(_) | EventKind::Modify(ModifyKind::Data(_) | ModifyKind::Name(_) | ModifyKind::Any) | EventKind::Any)
}

pub(crate) fn normalize(event: Event) -> Option<FileChange> {
    if event.need_rescan() { return Some(FileChange::Rescan); }
    if !content_event(&event.kind) { return None; }
    if event.paths.is_empty() { return Some(FileChange::Rescan); }
    let paths: Vec<_> = event.paths.into_iter().filter(|path| {
        !crate::ignore_rules::is_sync_temporary_path(&path.to_string_lossy().replace('\\', "/"))
    }).collect();
    if paths.is_empty() { None } else { Some(FileChange::Paths(paths)) }
}

/// Watch directories, not file inodes: atomic-save replacement must remain
/// visible. Rename endpoints are kept together; an unpaired endpoint simply
/// invalidates its old/new path and is reconciled as remove/create.
pub(crate) fn watch(
    root: &Path,
    recursive: bool,
    callback: impl Fn(Result<FileChange, notify::Error>) + Send + 'static,
) -> Result<notify::RecommendedWatcher, notify::Error> {
    let mut watcher = notify::recommended_watcher(move |event: Result<Event, notify::Error>| {
        match event {
            Ok(event) => { if let Some(change) = normalize(event) { callback(Ok(change)); } }
            Err(error) => callback(Err(error)),
        }
    })?;
    watcher.watch(root, if recursive { RecursiveMode::Recursive } else { RecursiveMode::NonRecursive })?;
    Ok(watcher)
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::event::{AccessKind, MetadataKind, RenameMode};

    #[test]
    fn ignores_times_access_and_temporary_files_but_keeps_atomic_save_target() {
        assert!(normalize(Event::new(EventKind::Access(AccessKind::Any)).add_path("/root/a".into())).is_none());
        assert!(normalize(Event::new(EventKind::Modify(ModifyKind::Metadata(MetadataKind::WriteTime))).add_path("/root/a".into())).is_none());
        let event = Event::new(EventKind::Modify(ModifyKind::Name(RenameMode::Both)))
            .add_path("/root/.a.swp".into()).add_path("/root/a".into());
        assert_eq!(normalize(event), Some(FileChange::Paths(vec!["/root/a".into()])));
    }

    #[test]
    fn preserves_move_endpoints_and_remove_events() {
        let event = Event::new(EventKind::Modify(ModifyKind::Name(RenameMode::Both)))
            .add_path("/root/a".into()).add_path("/root/b".into());
        assert_eq!(normalize(event), Some(FileChange::Paths(vec!["/root/a".into(), "/root/b".into()])));
        assert!(content_event(&EventKind::Remove(notify::event::RemoveKind::Folder)));
    }
}
