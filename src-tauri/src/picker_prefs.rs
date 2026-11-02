//! Backend-only native picker starting directories; never grants filesystem access.
use std::path::{Path, PathBuf};
use tauri::Manager;
use tauri_plugin_dialog::{DialogExt, FileDialogBuilder};

use crate::{Vault, types::{PickerDirection, PICKER_DOWNLOAD_DIRECTORY, PICKER_UPLOAD_DIRECTORY}};

impl PickerDirection {
    fn setting(self) -> &'static str {
        match self {
            Self::Upload => PICKER_UPLOAD_DIRECTORY,
            Self::Download => PICKER_DOWNLOAD_DIRECTORY,
        }
    }
}

fn existing_directory(path: PathBuf) -> Option<PathBuf> {
    (path.is_absolute() && path.is_dir()).then_some(path)
}

fn starting_directory(
    vault: &Vault,
    direction: PickerDirection,
    system_default: impl FnOnce() -> Option<PathBuf>,
    home: impl FnOnce() -> Option<PathBuf>,
) -> Option<PathBuf> {
    let remembered = match vault.get_setting(direction.setting()) {
        Ok(value) => value,
        Err(error) => {
            log::warn!("picker preference read failed: operation=read key={} error={error:?}", direction.setting());
            None
        }
    };
    remembered.and_then(|path| existing_directory(PathBuf::from(path)))
        .or_else(|| system_default().and_then(existing_directory))
        .or_else(|| home().and_then(existing_directory))
}

pub(crate) fn dialog(app: &tauri::AppHandle, direction: PickerDirection) -> FileDialogBuilder<tauri::Wry> {
    let Some(vault) = app.try_state::<crate::commands::SharedVault>() else {
        log::error!("picker preferences unavailable: reason=vault_state_missing");
        return app.dialog().file();
    };
    let directory = starting_directory(&vault, direction, || {
        let result = match direction {
            PickerDirection::Upload => app.path().document_dir(),
            PickerDirection::Download => app.path().download_dir(),
        };
        match result {
            Ok(path) => Some(path),
            Err(error) => {
                log::warn!("picker system directory unavailable: direction={direction:?} error={error:?}");
                None
            }
        }
    }, || match app.path().home_dir() {
        Ok(path) => Some(path),
        Err(error) => {
            log::warn!("picker home directory unavailable: error={error:?}");
            None
        }
    });
    let builder = app.dialog().file();
    match directory {
        Some(path) => builder.set_directory(path),
        None => builder,
    }
}

/// A canceled/empty selection is a no-op. File selections remember the first parent.
/// Preference failures must not discard an already selected capability or batch.
pub(crate) fn remember(vault: &Vault, direction: PickerDirection, selected: Option<&Path>, folder: bool) {
    let Some(path) = selected else { return; };
    let directory = if folder { Some(path) } else { path.parent() };
    let Some(directory) = directory else { return; };
    if !directory.is_absolute() { return; }
    let Some(value) = directory.to_str() else {
        log::warn!("picker preference not saved: operation=write key={} reason=non_utf8_path", direction.setting());
        return;
    };
    if let Err(error) = vault.set_setting(direction.setting(), value) {
        log::warn!("picker preference write failed: operation=write key={} error={error:?}", direction.setting());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn picker_prefs_persist_directions_and_selection_kinds() -> TestResult {
        let temp = tempfile::tempdir()?;
        let database = temp.path().join("vault.db");
        let files = temp.path().join("files");
        let folder = temp.path().join("folder");
        let downloads = temp.path().join("downloads");
        for path in [&files, &folder, &downloads] { std::fs::create_dir(path)?; }
        let vault = Vault::open(&database)?;
        remember(&vault, PickerDirection::Upload, Some(&files.join("one.txt")), false);
        assert_eq!(starting_directory(&vault, PickerDirection::Upload, || None, || None), Some(files));
        remember(&vault, PickerDirection::Upload, Some(&folder), true);
        remember(&vault, PickerDirection::Download, Some(&downloads), true);
        remember(&vault, PickerDirection::Upload, None, false);
        remember(&vault, PickerDirection::Download, None, true);
        drop(vault);
        let reopened = Vault::open(&database)?;
        assert_eq!(starting_directory(&reopened, PickerDirection::Upload, || None, || None), Some(folder));
        assert_eq!(starting_directory(&reopened, PickerDirection::Download, || None, || None), Some(downloads));
        Ok(())
    }

    #[test]
    fn picker_prefs_defaults_and_stale_path_fallback() -> TestResult {
        let temp = tempfile::tempdir()?;
        let vault = Vault::open(&temp.path().join("vault.db"))?;
        let documents = temp.path().join("documents");
        let downloads = temp.path().join("downloads");
        for path in [&documents, &downloads] { std::fs::create_dir(path)?; }
        assert_eq!(starting_directory(&vault, PickerDirection::Upload, || Some(documents.clone()), || None), Some(documents.clone()));
        assert_eq!(starting_directory(&vault, PickerDirection::Download, || Some(downloads.clone()), || None), Some(downloads));
        let stale = temp.path().join("removed");
        remember(&vault, PickerDirection::Upload, Some(&stale), true);
        assert_eq!(starting_directory(&vault, PickerDirection::Upload, || Some(documents.clone()), || None), Some(documents.clone()));
        let file = temp.path().join("not_directory");
        std::fs::write(&file, b"file")?;
        remember(&vault, PickerDirection::Upload, Some(&file), true);
        assert_eq!(starting_directory(&vault, PickerDirection::Upload, || Some(file), || Some(documents.clone())), Some(documents));
        assert_eq!(starting_directory(&vault, PickerDirection::Upload, || None, || None), None);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn picker_prefs_non_utf8_does_not_overwrite() -> TestResult {
        use std::os::unix::ffi::OsStringExt;
        let temp = tempfile::tempdir()?;
        let vault = Vault::open(&temp.path().join("vault.db"))?;
        remember(&vault, PickerDirection::Upload, Some(temp.path()), true);
        let non_utf8 = temp.path().join(std::ffi::OsString::from_vec(vec![0xff]));
        remember(&vault, PickerDirection::Upload, Some(&non_utf8), true);
        assert_eq!(starting_directory(&vault, PickerDirection::Upload, || None, || None), Some(temp.path().to_owned()));
        Ok(())
    }
}
