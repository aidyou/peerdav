use std::collections::BTreeMap;
use std::path::{Component, Path};
use std::sync::atomic::{AtomicBool, Ordering};

use cap_fs_ext::DirExt;
use cap_std::fs::Dir;

use super::types::{self, LocalFile, MirrorError, is_artifact};

/// 单个映射最多同步的文件数，超过即视为异常目录，拒绝同步。
const MAX_ENTRIES: usize = 500_000;
const MAX_DEPTH: usize = 128;

/// 一次本地扫描的结果。`skipped` 非空表示有文件/目录未能参与本轮比对，
/// 调用方必须关闭删除传播，避免把「没看到」误判成「已删除」。
pub struct ScanReport {
    pub files: BTreeMap<String, LocalFile>,
    pub skipped: Vec<(String, String)>,
}

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
/// 与云端键校验共用 `validate_relative_str`，保证两侧判定一致。
fn validate_relative(relative: &Path) -> Result<(), String> {
    let raw = relative
        .as_os_str()
        .to_str()
        .ok_or_else(|| "路径不是 UTF-8 / non-UTF-8 path".to_string())?;
    types::validate_relative_str(raw)
}

fn skipped_path(relative: &Path) -> String {
    relative.to_string_lossy().replace('\\', "/")
}

pub(crate) fn open_root_nofollow(root: &Path) -> Result<Dir, MirrorError> {
    if !root.is_absolute() {
        return Err(MirrorError::InvalidRoot(root.display().to_string()));
    }
    let mut current = Dir::open_ambient_dir("/", cap_std::ambient_authority()).map_err(io_error)?;
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
    report: &mut ScanReport,
) -> Result<(), MirrorError> {
    canceled(cancel)?;
    if relative.components().count() > MAX_DEPTH {
        return Err(MirrorError::UnsafePath(
            "目录层级超过上限 / directory depth exceeds the limit".into(),
        ));
    }
    let mut names = Vec::new();
    for item in dir.read_dir(".").map_err(io_error)? {
        match item {
            Ok(item) => match item.file_name().into_string() {
                Ok(name) => names.push(name),
                Err(name) => report.skipped.push((
                    name.to_string_lossy().into_owned(),
                    "文件名不是 UTF-8 / non-UTF-8 file name".into(),
                )),
            },
            Err(error) => report.skipped.push((
                skipped_path(relative),
                format!("读取目录项失败 / failed to read a directory entry: {error}"),
            )),
        }
    }
    names.sort();
    for name in names {
        canceled(cancel)?;
        let child = relative.join(&name);
        if let Err(reason) = validate_relative(&child) {
            report.skipped.push((
                skipped_path(&child),
                format!("路径不适用于镜像 / unsafe path: {reason}"),
            ));
            continue;
        }
        let metadata = match dir.symlink_metadata(&name) {
            Ok(metadata) => metadata,
            Err(error) => {
                report.skipped.push((
                    skipped_path(&child),
                    format!("读取文件元数据失败 / failed to read metadata: {error}"),
                ));
                continue;
            }
        };
        // 符号链接一律跳过：镜像不追踪链接目标，也不应成为链接逃逸的通道。
        if metadata.file_type().is_symlink() || (!metadata.is_dir() && !metadata.is_file()) {
            report.skipped.push((
                skipped_path(&child),
                "符号链接或特殊文件未扫描，本轮禁止删除 / Symlink or special file skipped; deletions disabled".into(),
            ));
            continue;
        }
        if metadata.is_dir() {
            let sub = match dir.open_dir_nofollow(&name) {
                Ok(sub) => sub,
                Err(error) => {
                    report.skipped.push((
                        skipped_path(&child),
                        format!("打开子目录失败 / failed to open a subdirectory: {error}"),
                    ));
                    continue;
                }
            };
            walk(&sub, &child, cancel, report)?;
        } else {
            let path = skipped_path(&child);
            if is_artifact(&path) {
                continue;
            }
            if report.files.len() >= MAX_ENTRIES {
                return Err(MirrorError::TooManyEntries);
            }
            report.files.insert(
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

/// 扫描本地目录，返回文件元数据与「未能参与比对」的条目。
/// 根目录必须是已存在的普通目录（不能是符号链接），否则拒绝同步。
pub fn scan(root: &Path, cancel: &AtomicBool) -> Result<ScanReport, MirrorError> {
    let link = std::fs::symlink_metadata(root)
        .map_err(|_| MirrorError::InvalidRoot(root.display().to_string()))?;
    if link.file_type().is_symlink() || !link.is_dir() {
        return Err(MirrorError::InvalidRoot(root.display().to_string()));
    }
    let root_dir = open_root_nofollow(root)?;
    let mut report = ScanReport {
        files: BTreeMap::new(),
        skipped: Vec::new(),
    };
    walk(&root_dir, Path::new(""), cancel, &mut report)?;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> std::path::PathBuf {
        use std::sync::atomic::AtomicUsize;
        static SEQ: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "peerdav-mirror-scan-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("a.txt"), b"hello").unwrap();
        std::fs::write(dir.join("sub").join("b.txt"), b"world").unwrap();
        dir
    }

    #[test]
    fn scan_lists_files_with_metadata_and_skips_artifacts() {
        let root = dir();
        std::fs::write(root.join("keep.conflict.pc1.2026.txt"), b"x").unwrap();
        std::fs::write(root.join("half.peerdav-mirror-tmp"), b"x").unwrap();
        let cancel = AtomicBool::new(false);
        let report = scan(&root, &cancel).unwrap();
        assert_eq!(report.files.len(), 2);
        assert!(report.files.contains_key("a.txt"));
        assert!(report.files.contains_key("sub/b.txt"));
        assert!(!report.files.contains_key("keep.conflict.pc1.2026.txt"));
        assert!(!report.files.contains_key("half.peerdav-mirror-tmp"));
        assert_eq!(report.files.get("a.txt").unwrap().size, 5);
        assert!(report.skipped.is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn unsupported_names_are_reported_instead_of_aborting_the_scan() {
        let root = dir();
        // Linux 允许以点结尾，但镜像跨平台不支持：跳过后必须让调用方知道扫描不完整。
        std::fs::write(root.join("weird."), b"x").unwrap();
        let cancel = AtomicBool::new(false);
        let report = scan(&root, &cancel).unwrap();
        assert_eq!(report.files.len(), 2);
        assert_eq!(report.skipped.len(), 1);
        assert_eq!(report.skipped[0].0, "weird.");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_names_are_reported_instead_of_aborting_the_scan() {
        use std::os::unix::ffi::OsStrExt;
        let root = dir();
        let name = std::ffi::OsStr::from_bytes(b"bad-\xff.txt");
        std::fs::write(root.join(name), b"x").unwrap();
        let cancel = AtomicBool::new(false);
        let report = scan(&root, &cancel).unwrap();
        assert_eq!(report.files.len(), 2);
        assert_eq!(report.skipped.len(), 1);
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
