// 镜像同步（路线 C）：本地目录与云端目录之间的明文双向镜像。
//
// 与加密同步盘（append 体系，内容寻址分块 + 历史版本）不同，镜像同步按整文件
// 存储在 `_peerdav_mirror/` 下并保持目录结构，方便用户用网盘自带界面直接浏览。
// 冲突采用「保留两者」策略：本地版本接管原路径，云端版本另存为本地
// `.conflict.{机器名}.{时间}` 副本。

mod baseline;
mod runtime;
mod scan;
mod sync;
mod types;

pub use baseline::remove as remove_baseline;
pub use runtime::{MirrorRunContext, run_sync};
pub(crate) use types::{MASS_DELETE_ABSOLUTE, MASS_DELETE_MINIMUM, MASS_DELETE_PERCENT};
pub use types::{
    MirrorError, MirrorMapping, MirrorSyncResult, SaveMirrorMappingInput, normalize_cloud_prefix,
};
