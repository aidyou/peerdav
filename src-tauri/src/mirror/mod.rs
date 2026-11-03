// 镜像同步（路线 C）：本地目录与云端目录之间的明文双向镜像。
//
// 与加密同步盘（append 体系，内容寻址分块 + 历史版本）不同，镜像同步按整文件
// 存储在 `_peerdav_mirror/` 下并保持目录结构，方便用户用网盘自带界面直接浏览。
// 冲突采用业界通用的「保留两者」策略：远端写 `.conflict.{机器名}.{时间}` 副本。

mod baseline;
mod scan;
mod sync;
mod types;
mod runtime;

pub use runtime::run_sync;
pub use types::{
    MirrorError, MirrorMapping, MirrorSyncResult, SaveMirrorMappingInput, normalize_cloud_prefix,
};
