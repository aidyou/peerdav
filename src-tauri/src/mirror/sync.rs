use std::collections::BTreeMap;

use super::types::{CloudFile, LocalFile, MirrorAction, MirrorBaseline};

/// 某个相对路径在本地是否相对基线发生了变化（不存在于基线视为新增）。
fn local_changed(
    path: &str,
    local: &BTreeMap<String, LocalFile>,
    baseline: &MirrorBaseline,
) -> bool {
    match (local.get(path), baseline.entry(path)) {
        (Some(file), Some(entry)) => {
            file.mtime_ns != entry.local_mtime_ns || file.size != entry.local_size
        }
        (Some(_), None) => true,
        (None, Some(_)) => true,
        (None, None) => false,
    }
}

/// 某个相对路径在云端是否相对基线发生了变化（不存在于基线视为新增）。
/// ETag 为服务商返回的不透明版本标识；ETag 相同且大小相同视为未变。
fn cloud_changed(
    path: &str,
    cloud: &BTreeMap<String, CloudFile>,
    baseline: &MirrorBaseline,
) -> bool {
    match (cloud.get(path), baseline.entry(path)) {
        (Some(file), Some(entry)) => file.etag != entry.cloud_etag || file.size != entry.cloud_size,
        (Some(_), None) => true,
        (None, Some(_)) => true,
        (None, None) => false,
    }
}

/// 由「本地扫描 + 云端列举 + 上次基线」三方比对生成本轮同步计划。
///
/// 判定矩阵（业界双向同步的共同基线法）：
/// - 仅本地变 → 上传；仅云端变 → 下载
/// - 本地删除且云端未变 → 删除云端；云端删除且本地未变 → 删除本地
/// - 两侧都改 → 冲突：保留双方，本地版本覆盖原路径、云端版本另存为本地冲突副本
/// - 本地删除但云端已改，或本地已改但云端已删 → 修改优先于删除：
///   前者下载恢复，后者上传恢复，避免并发修改被删除操作吞掉
/// - 两侧都删 → 无需动作（由调用方清理基线残留）
pub fn plan(
    local: &BTreeMap<String, LocalFile>,
    cloud: &BTreeMap<String, CloudFile>,
    baseline: &MirrorBaseline,
) -> Vec<MirrorAction> {
    let mut paths: Vec<&String> = local.keys().chain(cloud.keys()).collect();
    paths.sort_unstable();
    paths.dedup();
    let mut actions = Vec::new();
    for path in paths {
        let local_state = local.get(path);
        let cloud_state = cloud.get(path);
        let l_changed = local_changed(path, local, baseline);
        let c_changed = cloud_changed(path, cloud, baseline);
        match (local_state, cloud_state, l_changed, c_changed) {
            // 两侧都不存在：历史残留，交给基线清理，不出动作。
            (None, None, _, _) => {}
            // 两侧都在但都未变：跳过。
            (Some(_), Some(_), false, false) => {}
            // 仅本地变（含新增）：上传。
            (Some(_), Some(_), true, false) | (Some(_), None, true, false) => {
                actions.push(MirrorAction::Upload(path.clone()));
            }
            // 仅云端变（含新增）：下载。
            (Some(_), Some(_), false, true) | (None, Some(_), false, true) => {
                actions.push(MirrorAction::Download(path.clone()));
            }
            // 两侧都变：冲突（本地保留原路径，云端版本由调用方另存为本地副本）。
            (Some(_), Some(_), true, true) => {
                actions.push(MirrorAction::Conflict(path.clone()));
            }
            // 本地改 + 云端删：修改优先于删除，上传恢复原路径。
            (Some(_), None, true, true) => {
                actions.push(MirrorAction::Upload(path.clone()));
            }
            // 本地删 + 云端改：下载恢复，修改优先于删除。
            (None, Some(_), true, true) => {
                actions.push(MirrorAction::Restore(path.clone()));
            }
            // 本地删 + 云端未变：传播删除到云端。
            (None, Some(_), true, false) => {
                actions.push(MirrorAction::DeleteCloud(path.clone()));
            }
            // 云端删 + 本地未变：传播删除到本地。
            (Some(_), None, false, true) => {
                actions.push(MirrorAction::DeleteLocal(path.clone()));
            }
            // 剩余组合（本地未变且云端未变但有一侧缺失）在基线一致时不会出现：
            // 两侧均无变更意味着状态与基线相同，缺失侧在基线里也必然缺失。
            (None, Some(_), false, false) | (Some(_), None, false, false) => {}
        }
    }
    actions
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mirror::types::BaselineEntry;

    fn local(mtime: i128, size: u64) -> LocalFile {
        LocalFile {
            mtime_ns: mtime,
            size,
        }
    }

    fn cloud(etag: &str, size: u64) -> CloudFile {
        CloudFile {
            etag: Some(etag.into()),
            size,
        }
    }

    fn entry(mtime: i128, size: u64, etag: &str) -> BaselineEntry {
        BaselineEntry {
            local_mtime_ns: mtime,
            local_size: size,
            cloud_etag: Some(etag.into()),
            cloud_size: size,
        }
    }

    fn baseline_with(entries: &[(&str, BaselineEntry)]) -> MirrorBaseline {
        let mut baseline = MirrorBaseline::default();
        for (path, entry) in entries {
            baseline.entries.insert((*path).into(), entry.clone());
        }
        baseline
    }

    fn paths(actions: &[MirrorAction]) -> Vec<String> {
        actions
            .iter()
            .map(|a| match a {
                MirrorAction::Upload(p)
                | MirrorAction::Download(p)
                | MirrorAction::DeleteCloud(p)
                | MirrorAction::DeleteLocal(p)
                | MirrorAction::Conflict(p)
                | MirrorAction::Restore(p) => p.clone(),
            })
            .collect()
    }

    #[test]
    fn identical_state_produces_no_actions() {
        let local = BTreeMap::from([("a.txt".into(), local(100, 10))]);
        let cloud = BTreeMap::from([("a.txt".into(), cloud("e1", 10))]);
        let baseline = baseline_with(&[("a.txt", entry(100, 10, "e1"))]);
        assert!(plan(&local, &cloud, &baseline).is_empty());
    }

    #[test]
    fn local_only_changes_upload_and_cloud_only_changes_download() {
        let local = BTreeMap::from([
            ("changed.txt".into(), local(200, 10)),
            ("new.txt".into(), local(100, 5)),
        ]);
        let cloud = BTreeMap::from([
            ("changed.txt".into(), cloud("e1", 10)),
            ("remote-new.txt".into(), cloud("e9", 7)),
        ]);
        let baseline = baseline_with(&[("changed.txt", entry(100, 10, "e1"))]);
        let actions = plan(&local, &cloud, &baseline);
        assert_eq!(
            paths(&actions),
            vec!["changed.txt", "new.txt", "remote-new.txt"]
        );
        assert!(matches!(&actions[0], MirrorAction::Upload(p) if p == "changed.txt"));
        assert!(matches!(&actions[1], MirrorAction::Upload(p) if p == "new.txt"));
        assert!(matches!(&actions[2], MirrorAction::Download(p) if p == "remote-new.txt"));
    }

    #[test]
    fn both_changed_is_a_conflict() {
        let local = BTreeMap::from([("a.txt".into(), local(200, 12))]);
        let cloud = BTreeMap::from([("a.txt".into(), cloud("e2", 11))]);
        let baseline = baseline_with(&[("a.txt", entry(100, 10, "e1"))]);
        let actions = plan(&local, &cloud, &baseline);
        assert!(matches!(&actions[0], MirrorAction::Conflict(p) if p == "a.txt"));
    }

    #[test]
    fn deletions_propagate_only_when_the_other_side_is_unchanged() {
        // 本地删除、云端未变 → 删云端。
        let cloud = BTreeMap::from([("gone.txt".into(), cloud("e1", 10))]);
        let baseline = baseline_with(&[("gone.txt", entry(100, 10, "e1"))]);
        let actions = plan(&BTreeMap::new(), &cloud, &baseline);
        assert!(matches!(&actions[0], MirrorAction::DeleteCloud(p) if p == "gone.txt"));

        // 云端删除、本地未变 → 删本地。
        let local = BTreeMap::from([("gone.txt".into(), local(100, 10))]);
        let actions = plan(&local, &BTreeMap::new(), &baseline);
        assert!(matches!(&actions[0], MirrorAction::DeleteLocal(p) if p == "gone.txt"));

        // 两侧都删 → 无动作。
        assert!(plan(&BTreeMap::new(), &BTreeMap::new(), &baseline).is_empty());
    }

    #[test]
    fn local_delete_with_cloud_change_restores_instead_of_deleting() {
        let cloud = BTreeMap::from([("a.txt".into(), cloud("e2", 11))]);
        let baseline = baseline_with(&[("a.txt", entry(100, 10, "e1"))]);
        let actions = plan(&BTreeMap::new(), &cloud, &baseline);
        assert!(matches!(&actions[0], MirrorAction::Restore(p) if p == "a.txt"));
    }

    #[test]
    fn local_change_with_cloud_delete_restores_the_cloud_copy() {
        let local = BTreeMap::from([("a.txt".into(), local(200, 12))]);
        let baseline = baseline_with(&[("a.txt", entry(100, 10, "e1"))]);
        let actions = plan(&local, &BTreeMap::new(), &baseline);
        // 修改优先于删除：本地版本重新上传，而不是只留一个冲突副本。
        assert!(matches!(&actions[0], MirrorAction::Upload(p) if p == "a.txt"));
    }

    #[test]
    fn entries_missing_from_both_sides_are_ignored() {
        let baseline = baseline_with(&[("stale.txt", entry(100, 10, "e1"))]);
        assert!(plan(&BTreeMap::new(), &BTreeMap::new(), &baseline).is_empty());
    }
}
