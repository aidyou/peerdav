use std::io::Write;
use std::path::{Path, PathBuf};

use super::types::{BaselineEntry, MirrorBaseline, MirrorError};

/// 基线文件目录名，位于应用数据目录下。
const BASELINE_DIR: &str = "mirror-baselines";

fn baseline_path(data_dir: &Path, mapping_id: &str) -> Result<PathBuf, MirrorError> {
    // 映射标识由后端生成（十六进制随机串）；只允许文件系统安全的字符，
    // 任何外部传入的标识都不能逃出基线目录。
    if !mapping_id
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        || mapping_id.is_empty()
    {
        return Err(MirrorError::InvalidId(mapping_id.to_owned()));
    }
    Ok(data_dir
        .join(BASELINE_DIR)
        .join(format!("{mapping_id}.json")))
}

/// 加载某个映射的基线；文件不存在视为空基线（首次同步）。
pub fn load(data_dir: &Path, mapping_id: &str) -> Result<MirrorBaseline, MirrorError> {
    let path = baseline_path(data_dir, mapping_id)?;
    match std::fs::read(&path) {
        Ok(bytes) => {
            let baseline: MirrorBaseline =
                serde_json::from_slice(&bytes).map_err(|e| MirrorError::Baseline(e.to_string()))?;
            Ok(baseline)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(MirrorBaseline::default()),
        Err(error) => Err(MirrorError::from(error)),
    }
}

/// 原子写入基线：先写临时文件再重命名，避免同步中断留下半截 JSON。
pub fn save(
    data_dir: &Path,
    mapping_id: &str,
    baseline: &MirrorBaseline,
) -> Result<(), MirrorError> {
    let path = baseline_path(data_dir, mapping_id)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(MirrorError::from)?;
    }
    let bytes = serde_json::to_vec(baseline).map_err(|e| MirrorError::Baseline(e.to_string()))?;
    let mut tmp = path.clone();
    tmp.set_extension("json.tmp");
    {
        let mut file = std::fs::File::create(&tmp).map_err(MirrorError::from)?;
        file.write_all(&bytes).map_err(MirrorError::from)?;
        file.sync_all().map_err(MirrorError::from)?;
    }
    std::fs::rename(&tmp, &path).map_err(MirrorError::from)?;
    Ok(())
}

/// 删除某个映射的基线文件；文件不存在视为成功（幂等清理）。
pub fn remove(data_dir: &Path, mapping_id: &str) -> Result<(), MirrorError> {
    let path = baseline_path(data_dir, mapping_id)?;
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(MirrorError::from(error)),
    }
}

impl MirrorBaseline {
    /// 记录一次成功上传后的共同状态。
    pub fn record_upload(
        &mut self,
        path: &str,
        mtime_ns: i128,
        size: u64,
        etag: Option<String>,
        cloud_size: u64,
    ) {
        self.entries.insert(
            path.to_owned(),
            BaselineEntry {
                local_mtime_ns: mtime_ns,
                local_size: size,
                cloud_etag: etag,
                cloud_size,
            },
        );
    }

    /// 记录一次成功下载后的共同状态。本地修改时间以落盘后实测为准，
    /// 避免把下载时间误判为「本地又改过」而在下一轮触发重复上传。
    pub fn record_download(
        &mut self,
        path: &str,
        mtime_ns: i128,
        etag: Option<String>,
        cloud_size: u64,
    ) {
        self.entries.insert(
            path.to_owned(),
            BaselineEntry {
                local_mtime_ns: mtime_ns,
                local_size: cloud_size,
                cloud_etag: etag,
                cloud_size,
            },
        );
    }

    /// 删除已不存在于两侧的条目（删除传播成功后调用）。
    pub fn remove(&mut self, path: &str) {
        self.entries.remove(path);
    }

    /// 仅保留指定路径集合内的条目，用于同步结束后清理陈旧记录。
    pub fn retain(&mut self, keep: impl Fn(&String) -> bool) {
        self.entries.retain(|path, _| keep(path));
    }

    pub fn entry(&self, path: &str) -> Option<&BaselineEntry> {
        self.entries.get(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("peerdav-mirror-baseline-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn missing_baseline_is_empty_and_round_trips() {
        let dir = dir();
        let mut baseline = load(&dir, "abc123").unwrap();
        assert!(baseline.entries.is_empty());
        baseline.record_upload("a.txt", 1, 10, Some("etag-1".into()), 10);
        save(&dir, "abc123", &baseline).unwrap();
        let again = load(&dir, "abc123").unwrap();
        assert_eq!(
            again.entry("a.txt").unwrap().cloud_etag.as_deref(),
            Some("etag-1")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unsafe_mapping_ids_are_rejected() {
        let dir = dir();
        for bad in ["", "..", "a/b", "a\\b", "a\0b"] {
            assert!(load(&dir, bad).is_err(), "{bad:?}");
            assert!(remove(&dir, bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn removing_a_baseline_is_idempotent() {
        let dir = dir();
        let mut baseline = load(&dir, "abc123").unwrap();
        baseline.record_upload("a.txt", 1, 10, Some("etag-1".into()), 10);
        save(&dir, "abc123", &baseline).unwrap();
        remove(&dir, "abc123").unwrap();
        assert!(load(&dir, "abc123").unwrap().entries.is_empty());
        remove(&dir, "abc123").unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
