use std::io;
use std::path::{Component, Path, PathBuf};

use cap_fs_ext::DirExt;
use cap_std::fs::{Dir, MetadataExt};

use super::index::{BaselineRecord, IndexError, SyncIndex};
pub use super::root_types::{RootBaseline, RootError};
use super::root_types::{
    RootBinding, RootBindingStatus, RootIdentity, RootPauseReason, RootStatus,
};

fn classify_io(error: io::Error) -> RootError {
    RootError::Io(error)
}

fn open_root_nofollow(path: &Path) -> Result<Dir, RootError> {
    if !path.is_absolute() {
        return Err(RootError::InvalidPath);
    }
    let mut current =
        Dir::open_ambient_dir("/", cap_std::ambient_authority()).map_err(classify_io)?;
    for component in path.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(name) => {
                current = current.open_dir_nofollow(name).map_err(classify_io)?;
            }
            _ => return Err(RootError::InvalidPath),
        }
    }
    Ok(current)
}

fn identity(path: &Path) -> Result<RootIdentity, RootError> {
    #[cfg(unix)]
    {
        let dir = open_root_nofollow(path)?;
        let metadata = dir.dir_metadata().map_err(classify_io)?;
        if !metadata.is_dir() {
            return Err(RootError::NotDirectory);
        }
        Ok(RootIdentity {
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Err(RootError::Unsupported)
    }
}

fn inspect_identity(path: &Path) -> Result<RootIdentity, RootError> {
    identity(path)
}

pub fn bind_initial(root: impl Into<PathBuf>) -> Result<RootBinding, RootError> {
    let root_path = root.into();
    let identity = inspect_identity(&root_path)?;
    Ok(RootBinding {
        root_path,
        identity,
        epoch: 0,
        status: RootBindingStatus::Ready,
        requires_preview: false,
    })
}

/// Revalidates a persisted binding. Any missing/replaced root is paused and
/// remains paused; it is never turned Ready merely because the path returns.
pub fn inspect_binding(binding: &mut RootBinding) -> Result<RootStatus, RootError> {
    let current = match inspect_identity(&binding.root_path) {
        Ok(identity) => identity,
        Err(RootError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
            binding.status = RootBindingStatus::Paused {
                reason: RootPauseReason::Missing,
            };
            binding.requires_preview = true;
            return Ok(binding.status());
        }
        Err(RootError::Io(error)) if error.kind() == io::ErrorKind::PermissionDenied => {
            binding.status = RootBindingStatus::Paused {
                reason: RootPauseReason::Unavailable,
            };
            binding.requires_preview = true;
            return Ok(binding.status());
        }
        Err(RootError::Unavailable | RootError::NotDirectory) => {
            binding.status = RootBindingStatus::Paused {
                reason: RootPauseReason::Unavailable,
            };
            binding.requires_preview = true;
            return Ok(binding.status());
        }
        Err(error) => return Err(error),
    };
    if current != binding.identity {
        binding.status = RootBindingStatus::Paused {
            reason: RootPauseReason::Replaced,
        };
        binding.requires_preview = true;
    }
    Ok(binding.status())
}

pub fn reselect(old: &RootBinding, new_root: impl Into<PathBuf>) -> Result<RootBinding, RootError> {
    let root_path = new_root.into();
    let identity = inspect_identity(&root_path)?;
    let epoch = old.epoch.checked_add(1).ok_or(RootError::EpochExhausted)?;
    Ok(RootBinding {
        root_path,
        identity,
        epoch,
        status: RootBindingStatus::Paused {
            reason: RootPauseReason::ReselectedAwaitingManual,
        },
        requires_preview: true,
    })
}

pub fn authorize_manual_after_preview(
    binding: &mut RootBinding,
    expected_epoch: u64,
    preview_root: &Path,
    explicit_confirm: bool,
) -> Result<(), RootError> {
    if !explicit_confirm || expected_epoch != binding.epoch || preview_root != binding.root_path {
        return Err(if !explicit_confirm {
            RootError::ConfirmationRequired
        } else {
            RootError::StalePreview
        });
    }
    let current = inspect_identity(preview_root).map_err(|_| RootError::StalePreview)?;
    if current != binding.identity {
        return Err(RootError::StalePreview);
    }
    binding.status = RootBindingStatus::Ready;
    binding.requires_preview = false;
    Ok(())
}

pub fn timer_allowed(binding: &RootBinding) -> bool {
    matches!(binding.status, RootBindingStatus::Ready) && !binding.requires_preview
}

impl SyncIndex {
    pub fn record_binding(&mut self, binding: &RootBinding) -> Result<(), IndexError> {
        self.save("root_state", "binding", binding)
    }

    pub fn load_binding(&self) -> Result<Option<RootBinding>, IndexError> {
        self.load("root_state", "binding")
    }

    pub fn record_root_baseline(
        &mut self,
        relative_path: &str,
        baseline: &RootBaseline,
    ) -> Result<(), IndexError> {
        self.save("root_baseline", relative_path, baseline)
    }

    pub fn root_baseline(
        &self,
        relative_path: &str,
        epoch: u64,
    ) -> Result<BaselineRecord, IndexError> {
        let Some(record) = self.load::<RootBaseline>("root_baseline", relative_path)? else {
            return Ok(BaselineRecord::Unknown);
        };
        if record.root_epoch != epoch {
            return Ok(BaselineRecord::Unknown);
        }
        if matches!(record.baseline, BaselineRecord::Unknown) {
            return Ok(BaselineRecord::Unknown);
        }
        Ok(record.baseline)
    }

    pub fn unknown_root_baseline(
        &mut self,
        relative_path: &str,
        epoch: u64,
    ) -> Result<(), IndexError> {
        self.record_root_baseline(
            relative_path,
            &RootBaseline {
                root_epoch: epoch,
                baseline: BaselineRecord::Unknown,
            },
        )
    }
}
