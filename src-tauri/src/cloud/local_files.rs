//! Local sync-drive actions never use a remote object or transfer adapter.
use super::runtime::CloudRuntime;
use crate::{AppError, DomainError, PreviewPayload};
use std::io::Read;
#[cfg(target_os = "linux")]
use std::os::fd::AsFd;
use std::path::{Path, PathBuf};

fn io(operation: &'static str, source: std::io::Error) -> AppError {
    DomainError::Io { operation, path: None, source }.into()
}
fn invalid() -> AppError {
    AppError { code: "CLOUD_PATH_INVALID".into(), message: "本地文件路径无效 / Invalid local file path".into(), detail: None, context: None }
}
fn parent(root: &Path, relative: &str) -> Result<(cap_std::fs::Dir, String), AppError> {
    use cap_fs_ext::DirExt;
    let path = Path::new(relative);
    crate::sync::paths::validate_relative(path).map_err(|_| invalid())?;
    let mut parts = path.components();
    let name = parts.next_back().ok_or_else(invalid)?.as_os_str().to_str().ok_or_else(invalid)?.to_owned();
    let mut dir = crate::sync::materialize::open_root(root).map_err(super::runtime::append::append_error)?;
    for part in parts { dir = dir.open_dir_nofollow(part.as_os_str()).map_err(|e| io("open local folder", e))?; }
    Ok((dir, name))
}
impl CloudRuntime {
    pub async fn local_file_preview(&self, mapping_id: &str, path: &str) -> Result<PreviewPayload, AppError> {
        self.admit()?;
        let _network = self.network.read().await;
        self.admit()?;
        let mapping = self.mapping(mapping_id)?;
        let path = path.to_owned();
        tokio::task::spawn_blocking(move || {
            let file = crate::sync::materialize::open_nofollow(&mapping.local_root, Path::new(&path)).map_err(super::runtime::append::append_error)?;
            const LIMIT: u64 = 8 * 1024 * 1024;
            let mut bytes = Vec::new();
            file.take(LIMIT + 1).read_to_end(&mut bytes).map_err(|e| io("preview local file", e))?;
            if bytes.len() as u64 > LIMIT { return Err(DomainError::PreviewTooLarge { size: bytes.len() as u64, limit: LIMIT }.into()); }
            let name = Path::new(&path).file_name().and_then(|name| name.to_str()).ok_or_else(invalid)?.to_owned();
            crate::peer::preview_payload(name, bytes).map_err(AppError::from)
        }).await.map_err(super::runtime::append::append_error)?
    }

    pub async fn local_file_open(&self, app: &tauri::AppHandle, mapping_id: &str, path: &str) -> Result<(), AppError> {
        use tauri_plugin_opener::OpenerExt;
        self.admit()?;
        let _network = self.network.read().await;
        self.admit()?;
        let mapping = self.mapping(mapping_id)?;
        // Reject symlinks at every level before handing the local path to the OS.
        let file = crate::sync::materialize::open_nofollow(&mapping.local_root, Path::new(path)).map_err(super::runtime::append::append_error)?;
        drop(file);
        let target = mapping.local_root.join(path);
        app.opener().open_path(target.to_string_lossy(), None::<&str>).map_err(|source| AppError::from(DomainError::ExternalOpen { path: path.into(), detail: source.to_string() }))
    }

    /// Mutations hold the network write gate so a worker cannot start during a
    /// local operation. No remote action occurs here; sync observes the new tree.
    pub async fn local_file_mutate(&self, mapping_id: &str, path: &str, destination: Option<&str>) -> Result<(), AppError> {
        self.admit()?;
        let _network = self.network.write().await;
        self.admit()?;
        self.admit_sync_idle(mapping_id)?;
        let mapping = self.mapping(mapping_id)?;
        let root = mapping.local_root;
        let path = path.to_owned();
        let destination = destination.map(str::to_owned);
        tokio::task::spawn_blocking(move || mutate(&root, &path, destination.as_deref())).await.map_err(super::runtime::append::append_error)?
    }
}
fn mutate(root: &Path, path: &str, destination: Option<&str>) -> Result<(), AppError> {
    let (source, name) = parent(root, path)?;
    let metadata = source.symlink_metadata(&name).map_err(|e| io("inspect local file", e))?;
    if metadata.is_symlink() { return Err(invalid()); }
    if let Some(destination) = destination {
        let target = PathBuf::from(destination).join(&name);
        if target == Path::new(path) || Path::new(destination).starts_with(path) { return Err(invalid()); }
        let target = target.to_str().ok_or_else(invalid)?;
        let (folder, target_name) = parent(root, target)?;
        #[cfg(target_os = "linux")]
        {
            rustix::fs::renameat_with(source.as_fd(), &name, folder.as_fd(), &target_name, rustix::fs::RenameFlags::NOREPLACE)
                .map_err(|e| io("move local file", std::io::Error::from(e)))?;
        }
        #[cfg(not(target_os = "linux"))]
        { return Err(AppError { code: "CLOUD_UNSUPPORTED".into(), message: "当前平台不支持安全移动 / Safe move is unavailable on this platform".into(), detail: None, context: None }); }
    } else if metadata.is_dir() {
        source.remove_dir_all(&name).map_err(|e| io("delete local folder", e))?;
    } else if metadata.is_file() {
        source.remove_file(&name).map_err(|e| io("delete local file", e))?;
    } else { return Err(invalid()); }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn local_move_and_delete_reject_escape_collision_and_descendants() -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let root = temp.path();
        std::fs::create_dir(root.join("target"))?;
        std::fs::create_dir(root.join("folder"))?;
        std::fs::write(root.join("a.txt"), "original")?;
        std::fs::write(root.join("target/a.txt"), "collision")?;
        assert!(mutate(root, "a.txt", Some("target")).is_err());
        assert_eq!(std::fs::read_to_string(root.join("a.txt"))?, "original");
        assert!(mutate(root, "folder", Some("folder/child")).is_err());
        assert!(mutate(root, "../outside", None).is_err());
        assert!(mutate(root, "", None).is_err());
        std::fs::remove_file(root.join("target/a.txt"))?;
        assert!(mutate(root, "a.txt", Some("target")).is_ok());
        assert!(!root.join("a.txt").exists());
        assert_eq!(std::fs::read_to_string(root.join("target/a.txt"))?, "original");
        assert!(mutate(root, "target", None).is_ok());
        assert!(!root.join("target").exists());
        #[cfg(unix)] {
            std::os::unix::fs::symlink(root.join("folder"), root.join("link"))?;
            assert!(mutate(root, "link", None).is_err());
            assert!(mutate(root, "link/file", None).is_err());
            assert!(root.join("folder").is_dir());
        }
        Ok(())
    }
}
