use std::collections::BTreeMap;
use std::path::{Component, Path};
use std::sync::atomic::{AtomicBool, Ordering};

use cap_fs_ext::DirExt;
use cap_std::fs::Dir;

use super::types::{is_conflict_artifact, LocalFile, MirrorError};

/// 单个映射最多同步的文件数，超过即视为异常目录，拒绝同步。
const MAX_ENTRIES: usize = 500_000;
const MAX_DEPTH: usize = 128;

fn canceled(cancel: &AtomicBool) -> Result<(), MirrorError> {
    if cancel.load(Ordering::Relaxed) {
        Err(MirrorError::Canceled)
    } else {
        Ok(())
    }
}

fn stamp(time: cap_std::time::SystemTime) -> i128 {
    time.into_std()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as i128
}

/// 相对路径安全检查：UTF-8、非绝对路径、无 `..`/`.` 段、无控制字符与反斜杠。
/// 与加密同步盘的 `validate_relative` 同样的安全意图，但镜像模块保持独立实现。
fn validate_relative(relative: &Path) -> Result<(), MirrorError> {
    let raw = relative
        .as_os_str()
        .to_str()
        .ok_or_else(|| MirrorError::UnsafePath("non-UTF-8 path".into()))?;
    if raw.is_empty()
        || raw.contains('\0')
        || raw.contains('\\')
        || raw.starts_with('/')
        || raw.contains("//")
    {
        return Err(MirrorError::UnsafePath(raw.into()));
    }
    for component in relative.components() {
        let Component::Normal(name) = component else {
            return Err(MirrorError::UnsafePath(raw.into()));
        };
        let text = name
            .to_str()
            .ok_or_else(|| MirrorError::UnsafePath("non-UTF-8 path".into()))?;
        if text == "." || text == ".." || text.ends_with(['.', ' ']) {
            return Err(MirrorError::UnsafePath(text.into()));
        }
    }
    Ok(())
}

fn open_root_nofollow(root: &Path) -> Result<Dir, MirrorError> {
    if !root.is_absolute() {
        return Err(MirrorError::InvalidRoot(root.display().to_string()));
    }
    let mut current =
        Dir::open_ambient_dir("/", cap_std::ambient_authority()).map_err(io_error)?;
    for component in root.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(name) => {
                current = current.open_dir_nofollow(name).map_err(io_error)?
            }
            _ => return Err(MirrorError::InvalidRoot(root.display().to_string())),
        }
    }
    Ok(current)
}

fn io_error(error: std::io::Error) -> MirrorError {
    MirrorError::from(error)
}

fn walk(
    dir: &Dir,
    relative: &Path,
    cancel: &AtomicBool,
    files: &mut BTreeMap<String, LocalFile>,
) -> Result<(), MirrorError> {
    canceled(cancel)?;
    if relative.components().count() > MAX_DEPTH || files.len() > MAX_ENTRIES {
        return Err(MirrorError::UnsafePath("too many entries or too deep".into()));
    }
    let mut names: Vec<_> = dir
        .read_dir(".")
        .map_err(io_error)?
        .filter_map(|item| item.ok())
        .filter_map(|item| item.file_name().into_string().ok())
        .collect();
    names.sort();
    for name in names {
        canceled(cancel)?;
        let child = relative.join(&name);
        validate_relative(&child)?;
        let metadata = dir.symlink_metadata(&name).map_err(io_error)?;
        // 符号链接一律跳过：镜像不追踪链接目标，也不应成为链接逃逸的通道。
        if metadata.file_type().is_symlink() {
            continue;
        }
        if metadata.is_dir() {
            let sub = dir.open_dir_nofollow(&name).map_err(io_error)?;
            walk(&sub, &child, cancel, files)?;
        } else {
            let path = child.to_string_lossy().replace('\\', "/");
            if is_conflict_artifact(&path) {
                continue;
            }
            files.insert(
                path,
                LocalFile {
                    mtime_ns: match metadata.modified() {
                        Ok(time) => stamp(time),
                        Err(_) => 0,
                    },
                    size: metadata.len(),
                },
            );
        }
    }
    Ok(())
}

/// 扫描本地目录，返回「相对路径（POSIX 风格）→ 本地文件元数据」。
/// 根目录必须是已存在的普通目录（不能是符号链接），否则拒绝同步。
pub fn scan(root: &Path, cancel: &AtomicBool) -> Result<BTreeMap<String, LocalFile>, MirrorError> {
    let link = std::fs::symlink_metadata(root).map_err(|_| {
        MirrorError::InvalidRoot(root.display().to_string())
    })?;
    if link.file_type().is_symlink() || !link.is_dir() {
        return Err(MirrorError::InvalidRoot(root.display().to_string()));
    }
    let root_dir = open_root_nofollow(root)?;
    let mut files = BTreeMap::new();
    walk(&root_dir, Path::new(""), cancel, &mut files)?;
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("peerdav-mirror-scan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("a.txt"), b"hello").unwrap();
        std::fs::write(dir.join("sub").join("b.txt"), b"world").unwrap();
        dir
    }

    #[test]
    fn scan_lists_files_with_metadata_and_skips_conflicts() {
        let root = dir();
        std::fs::write(root.join("keep.conflict.pc1.2026.txt"), b"x").unwrap();
        let cancel = AtomicBool::new(false);
        let files = scan(&root, &cancel).unwrap();
        assert_eq!(files.len(), 2);
        assert!(files.contains_key("a.txt"));
        assert!(files.contains_key("sub/b.txt"));
        assert!(!files.contains_key("keep.conflict.pc1.2026.txt"));
        assert_eq!(files.get("a.txt").unwrap().size, 5);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn scan_rejects_missing_and_symlink_roots() {
        let cancel = AtomicBool::new(false);
        assert!(scan(std::path::Path::new("/nonexistent/peerdav-mirror"), &cancel).is_err());
        let root = dir();
        let link = std::env::temp_dir().join(format!("peerdav-mirror-link-{}", std::process::id()));
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(&root, &link).unwrap();
        assert!(scan(&link, &cancel).is_err());
        let _ = std::fs::remove_file(&link);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn scan_is_cancelable() {
        let root = dir();
        let cancel = AtomicBool::new(true);
        assert!(matches!(scan(&root, &cancel), Err(MirrorError::Canceled)));
        let _ = std::fs::remove_dir_all(&root);
    }
}
