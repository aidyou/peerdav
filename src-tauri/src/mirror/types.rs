use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::cloud::config_types::ScheduleConfig;
use crate::cloud::types::CloudError;

/// 镜像同步数据统一存放的云端前缀，与加密同步盘的 `_peerdav_encrypt` 区分：
/// 用户用网盘工具浏览存储桶时能直接看清两类数据的归属。
pub const MIRROR_ROOT: &str = "_peerdav_mirror";
/// 冲突副本文件名中的分隔标记，扫描与云端列举都会跳过含此标记的条目，
/// 避免冲突副本被当成普通文件反复同步。
pub const CONFLICT_MARKER: &str = ".conflict.";
/// 下载临时文件标记。正常流程会立即重命名，崩溃残留的临时文件由扫描与列举跳过，
/// 避免把半截内容当成普通文件同步。
pub const TEMP_MARKER: &str = ".peerdav-mirror-tmp";
/// 异常批量删除的数量及比例门槛；少量日常删除不要求重复确认。
pub const MASS_DELETE_ABSOLUTE: usize = 100;
pub const MASS_DELETE_MINIMUM: usize = 10;
pub const MASS_DELETE_PERCENT: usize = 25;

/// 镜像同步映射：本地目录与云端目录之间保持目录结构的明文双向镜像。
///
/// 与加密同步盘（内容寻址、分块、历史版本）不同，镜像同步按整文件存储：
/// 本地 `a/b/c.txt` 对应云端 `_peerdav_mirror/{cloud_prefix}/a/b/c.txt`，
/// 可直接用网盘自带界面浏览。代价是没有去重、断点续传与历史版本。
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MirrorMapping {
    pub id: String,
    pub connection_id: String,
    pub local_root: PathBuf,
    /// 云端子目录（相对 `MIRROR_ROOT`，无前后导斜杠）；空串表示直接镜像到 `_peerdav_mirror/` 下。
    #[serde(default)]
    pub cloud_prefix: String,
    #[serde(default)]
    pub schedule: ScheduleConfig,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SaveMirrorMappingInput {
    /// 编辑已有映射时传入其 id；留空表示新建。
    pub id: Option<String>,
    pub connection_id: String,
    pub local_root: PathBuf,
    #[serde(default)]
    pub cloud_prefix: String,
    #[serde(default)]
    pub schedule: ScheduleConfig,
}

/// 上次同步完成时单个文件的共同基线。双向同步以它判断「哪一侧相对基线发生了变化」：
/// 仅本地变 → 上传；仅云端变 → 下载；两侧都变 → 冲突。
///
/// `identity` 记录这份基线对应的连接、桶、云端目录与本地根。任何一项变化都意味着
/// 旧基线不再适用于当前目标，必须重置，否则会把「新目标里看不到旧文件」误判成删除。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MirrorBaseline {
    /// Earlier versions could adopt different equal-sized files as a common state.
    /// Such entries are never trusted for deletion or replacement after migration.
    #[serde(default)]
    pub safety_version: u32,
    /// 已完成当前目标的初次接入；文件全部删完后也不能重新当成首次接入。
    #[serde(default)]
    pub initialized: bool,
    #[serde(default)]
    pub identity: Option<MirrorIdentity>,
    /// 相对路径（POSIX 风格）→ 基线条目。
    pub entries: BTreeMap<String, BaselineEntry>,
}

/// 一份基线适用的同步目标。端点或桶变化同样视为目标变化（可能是另一台服务器）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MirrorIdentity {
    pub connection_id: String,
    pub endpoint: String,
    pub bucket: String,
    pub cloud_prefix: String,
    pub local_root: String,
    #[serde(default)]
    pub root_identity: Option<(u64, u64)>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BaselineEntry {
    pub local_mtime_ns: i128,
    pub local_size: u64,
    pub cloud_etag: Option<String>,
    pub cloud_size: u64,
}

/// 同步计划中的一个动作。相对路径均为 POSIX 风格。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MirrorAction {
    /// 本地新增或修改 → 上传。
    Upload(String),
    /// 云端新增或修改 → 下载。
    Download(String),
    /// 本地删除且云端未变 → 删除云端副本。
    DeleteCloud(String),
    /// 云端删除且本地未变 → 删除本地副本。
    DeleteLocal(String),
    /// 两侧都修改 → 本地内容上传为冲突副本，原文件保持不动，等用户人工合并。
    Conflict(String),
    /// 本地删除但云端已修改 → 下载云端版本恢复本地（修改优先于删除，避免丢数据）。
    Restore(String),
}

/// 单次镜像同步的汇总结果。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MirrorSyncResult {
    pub uploaded: u64,
    pub downloaded: u64,
    pub deleted_cloud: u64,
    pub deleted_local: u64,
    pub conflicts: Vec<String>,
    pub restored: u64,
    /// 逐条失败（相对路径 + 原因），同步不因单条失败而中断。
    pub failures: Vec<MirrorFailure>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MirrorFailure {
    pub path: String,
    pub reason: String,
}

#[derive(Debug, Error)]
pub enum MirrorError {
    #[error("本地目录无效或不存在: {0}")]
    InvalidRoot(String),
    #[error("云端目录无效: {0}")]
    InvalidPrefix(String),
    #[error("映射标识无效: {0}")]
    InvalidId(String),
    #[error("不安全的本地路径: {0}")]
    UnsafePath(String),
    #[error("同步被取消")]
    Canceled,
    #[error("云存储操作失败: {0}")]
    Cloud(#[from] CloudError),
    #[error("本地文件操作失败: {0}")]
    Local(#[from] std::io::Error),
    #[error("基线数据损坏: {0}")]
    Baseline(String),
    #[error("该映射正在同步中")]
    Busy,
    #[error("镜像目录对象数超过上限")]
    TooManyEntries,
    /// 异常批量删除或整侧变空必须绑定当前快照，由调用方显式确认。
    #[error(
        "本轮计划删除 {planned} 个文件（基线及当前快照最多 {total} 个），检测到异常删除；请核对计划并确认后重试"
    )]
    MassDelete {
        planned: usize,
        total: usize,
        plan_id: String,
        paths: Vec<String>,
    },
    #[error("首次接入非空目录需要确认合并 / Confirm merging existing folders before syncing")]
    ConfirmMerge { plan_id: String, paths: Vec<String> },
}

/// 云端对象在基线比较视角下的快照。ETag 是服务商返回的不透明版本标识，
/// 仅用于与基线比对判断「云端是否变化」，不作为内容哈希解释。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CloudFile {
    pub etag: Option<String>,
    pub size: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalFile {
    pub mtime_ns: i128,
    pub size: u64,
}

/// 判断相对路径是否为同步产物（冲突副本或下载临时文件），两侧均跳过。
pub fn is_artifact(path: &str) -> bool {
    path.contains(CONFLICT_MARKER) || path.contains(TEMP_MARKER)
}

/// 校验镜像相对路径：UTF-8、非绝对、无反斜杠/控制字符、折叠后无空段，
/// 且每段不是 `.`/`..`、不以 `.`/空格结尾（Windows 上会被静默截断）。
/// 本地扫描与云端列举共用同一套规则：云端对象键同样可能包含 `..`，不能直接拼接。
pub fn validate_relative_str(raw: &str) -> Result<(), String> {
    if raw.is_empty() {
        return Err("路径为空 / empty path".into());
    }
    if raw.contains('\0') || raw.contains('\\') || raw.starts_with('/') || raw.contains("//") {
        return Err(format!("路径包含不安全字符 / unsafe characters: {raw}"));
    }
    for segment in raw.split('/') {
        if segment.is_empty() || segment == "." || segment == ".." {
            return Err(format!("路径包含不安全片段 / unsafe segment: {segment:?}"));
        }
        if segment.ends_with(['.', ' ']) {
            return Err(format!(
                "路径片段以点或空格结尾 / segment ends with dot or space: {segment:?}"
            ));
        }
    }
    Ok(())
}

/// 把镜像映射的云端子目录规范化：去前后斜杠、折叠连续斜杠、拒绝 `.`/`..` 段与反斜杠。
/// 空串合法（表示直接放在 `_peerdav_mirror/` 下）。
pub fn normalize_cloud_prefix(prefix: &str) -> Result<String, MirrorError> {
    let trimmed = prefix.trim();
    if trimmed.is_empty() {
        return Ok(String::new());
    }
    if trimmed.contains('\\') || trimmed.contains('\0') {
        return Err(MirrorError::InvalidPrefix(prefix.to_owned()));
    }
    let mut parts = Vec::new();
    for segment in trimmed.split('/') {
        if segment.is_empty() {
            continue;
        }
        if segment == "." || segment == ".." {
            return Err(MirrorError::InvalidPrefix(prefix.to_owned()));
        }
        parts.push(segment);
    }
    Ok(parts.join("/"))
}

/// 镜像映射的完整云端键前缀：`_peerdav_mirror/{cloud_prefix}/`。
pub fn cloud_key_prefix(cloud_prefix: &str) -> String {
    if cloud_prefix.is_empty() {
        format!("{MIRROR_ROOT}/")
    } else {
        format!("{MIRROR_ROOT}/{cloud_prefix}/")
    }
}

/// 由相对路径生成云端对象键。
pub fn cloud_key(cloud_prefix: &str, relative: &str) -> String {
    format!("{}{relative}", cloud_key_prefix(cloud_prefix))
}

/// 冲突副本名称：`{stem}.conflict.{machine}.{timestamp}{ext}`。
pub fn conflict_name(relative: &str, machine: &str, timestamp: &str) -> String {
    let (stem, extension) = match relative.rsplit_once('.') {
        Some((stem, ext)) => (stem, format!(".{ext}")),
        None => (relative, String::new()),
    };
    format!("{stem}{CONFLICT_MARKER}{machine}.{timestamp}{extension}")
}

/// 把 Unix 秒格式化为可读的 UTC 时间戳（`YYYYMMDD-HHMMSSZ`），用于冲突文件名。
/// 不引入额外日期库，民用日期算法（Howard Hinnant 的 days_from_civil 逆运算）。
pub fn utc_stamp(unix_secs: u64) -> String {
    let days = (unix_secs / 86400) as i64;
    let secs = unix_secs % 86400;
    let era = if days + 719468 >= 0 {
        days + 719468
    } else {
        days + 719468 - 146096
    } / 146097;
    let doe = (days + 719468 - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { y + 1 } else { y };
    format!(
        "{year:04}{month:02}{day:02}-{:02}{:02}{:02}Z",
        secs / 3600,
        secs % 3600 / 60,
        secs % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_normalization_accepts_plain_and_empty() {
        assert_eq!(normalize_cloud_prefix("").unwrap(), "");
        assert_eq!(normalize_cloud_prefix("docs").unwrap(), "docs");
        assert_eq!(normalize_cloud_prefix("/a//b/").unwrap(), "a/b");
    }

    #[test]
    fn prefix_normalization_rejects_traversal_and_control() {
        for bad in ["../escape", "a/..", "a\\b", "a\0b"] {
            assert!(normalize_cloud_prefix(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn cloud_keys_are_prefixed() {
        assert_eq!(cloud_key("", "a/b.txt"), "_peerdav_mirror/a/b.txt");
        assert_eq!(cloud_key("docs", "a.txt"), "_peerdav_mirror/docs/a.txt");
    }

    #[test]
    fn conflict_names_keep_extension_and_mark_machine() {
        let name = conflict_name("report.txt", "pc1", "20261104-1530");
        assert_eq!(name, "report.conflict.pc1.20261104-1530.txt");
        let bare = conflict_name("README", "pc1", "20261104-1530");
        assert_eq!(bare, "README.conflict.pc1.20261104-1530");
    }

    #[test]
    fn artifacts_are_detected_by_marker() {
        assert!(is_artifact("a.conflict.pc1.2026.txt"));
        assert!(is_artifact("a.txt.peerdav-mirror-tmp"));
        assert!(!is_artifact("a.txt"));
    }

    #[test]
    fn relative_validation_rejects_escapes_and_admits_plain_paths() {
        assert!(validate_relative_str("a/b.txt").is_ok());
        assert!(validate_relative_str("a b/中文.txt").is_ok());
        for bad in [
            "",
            "../escape",
            "a/../b",
            "a/./b",
            "/abs",
            "//server/share",
            "a\\b",
            "a//b",
            "trailing.",
            "trailing ",
            "a\0b",
        ] {
            assert!(
                validate_relative_str(bad).is_err(),
                "expected rejection for {bad:?}"
            );
        }
    }

    #[test]
    fn utc_stamp_matches_known_epoch() {
        // 2026-11-04 15:30:00 UTC = 1793806200。
        assert_eq!(utc_stamp(1793806200), "20261104-153000Z");
        // Unix 纪元本身。
        assert_eq!(utc_stamp(0), "19700101-000000Z");
        // 跨年与闰年二月末。
        assert_eq!(utc_stamp(1709164800), "20240229-000000Z");
    }
}
