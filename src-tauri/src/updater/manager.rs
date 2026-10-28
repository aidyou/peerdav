use std::sync::Mutex;
use std::time::Duration;

use tauri::{AppHandle, Emitter, State};
use tauri_plugin_updater::UpdaterExt;

use super::error::UpdateError;
use super::types::{ReadyUpdate, UpdatePhase, UpdateStatus};
use crate::AppError;

pub(crate) struct UpdateManager {
    status: Mutex<UpdateStatus>,
    operation: tokio::sync::Mutex<Option<ReadyUpdate>>,
}

impl UpdateManager {
    pub(crate) fn new(version: String, config: Option<&serde_json::Value>) -> Self {
        Self {
            status: Mutex::new(UpdateStatus {
                configured: is_configured(config),
                current_version: version,
                ..UpdateStatus::default()
            }),
            operation: tokio::sync::Mutex::new(None),
        }
    }

    fn snapshot(&self) -> UpdateStatus {
        self.status
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    fn change(&self, app: &AppHandle, change: impl FnOnce(&mut UpdateStatus)) {
        let snapshot = {
            let mut status = self
                .status
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            change(&mut status);
            status.clone()
        };
        if let Err(error) = app.emit("update://status", snapshot) {
            report(app, UpdateError::Event(error));
        }
    }

    async fn check(&self, app: &AppHandle) -> Result<UpdateStatus, UpdateError> {
        if !self.snapshot().configured {
            return Err(UpdateError::NotConfigured);
        }
        let Ok(mut ready) = self.operation.try_lock() else {
            return Ok(self.snapshot());
        };
        if ready.is_some() {
            return Ok(self.snapshot());
        }
        self.change(app, |status| {
            status.phase = UpdatePhase::Checking;
            status.version = None;
            status.notes = None;
            status.downloaded = 0;
            status.total = None;
        });
        let result = async {
            let updater = app
                .updater_builder()
                .timeout(Duration::from_secs(60))
                .build()?;
            let Some(update) = updater.check().await? else {
                self.change(app, |status| status.phase = UpdatePhase::UpToDate);
                return Ok::<(), UpdateError>(());
            };
            self.change(app, |status| {
                status.phase = UpdatePhase::Downloading;
                status.version = Some(update.version.clone());
                status.notes = update.body.clone();
            });
            // The plugin verifies the signature before returning these bytes.
            let bytes = update
                .download(
                    |length, total| {
                        self.change(app, |status| {
                            status.downloaded = status.downloaded.saturating_add(length as u64);
                            status.total = total;
                        });
                    },
                    || {},
                )
                .await?;
            *ready = Some(ReadyUpdate { update, bytes });
            self.change(app, |status| status.phase = UpdatePhase::Ready);
            Ok(())
        }
        .await;
        if result.is_err() {
            self.change(app, |status| status.phase = UpdatePhase::Failed);
        }
        result?;
        Ok(self.snapshot())
    }

    async fn install(&self, app: &AppHandle) -> Result<(), UpdateError> {
        let ready = self.operation.try_lock().map_err(|_| UpdateError::Busy)?;
        let update = ready.as_ref().ok_or(UpdateError::NotReady)?;
        self.change(app, |status| status.phase = UpdatePhase::Installing);
        if let Err(error) = update.update.install(&update.bytes) {
            self.change(app, |status| status.phase = UpdatePhase::Ready);
            return Err(error.into());
        }
        app.restart();
    }
}

fn is_configured(config: Option<&serde_json::Value>) -> bool {
    config
        .cloned()
        .and_then(|value| serde_json::from_value::<tauri_plugin_updater::Config>(value).ok())
        .is_some_and(|config| !config.pubkey.trim().is_empty() && !config.endpoints.is_empty())
}

fn report(app: &AppHandle, error: UpdateError) {
    let error = AppError::from(error);
    if let Err(source) = app.emit("app://error", error) {
        log::error!(
            "failed to report update error: {}",
            crate::error::error_chain(&source)
        );
    }
}

#[tauri::command]
pub(crate) fn update_status(manager: State<'_, UpdateManager>) -> Result<UpdateStatus, AppError> {
    Ok(manager.snapshot())
}

#[tauri::command]
pub(crate) async fn check_for_updates(
    app: AppHandle,
    manager: State<'_, UpdateManager>,
) -> Result<UpdateStatus, AppError> {
    manager.check(&app).await.map_err(AppError::from)
}

#[tauri::command]
pub(crate) async fn install_update(
    app: AppHandle,
    manager: State<'_, UpdateManager>,
) -> Result<(), AppError> {
    manager.install(&app).await.map_err(AppError::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initial_status_has_no_pending_install_or_progress() {
        let manager = UpdateManager::new("0.1.0".into(), None);
        let status = manager.snapshot();
        assert_eq!(status.phase, UpdatePhase::Idle);
        assert_eq!(status.current_version, "0.1.0");
        assert_eq!(status.downloaded, 0);
        assert!(status.total.is_none());
        assert!(status.version.is_none());
        assert!(manager.operation.try_lock().is_ok());
    }

    #[test]
    fn updater_plugin_configuration_deserializes_at_startup() -> Result<(), serde_json::Error> {
        let config: serde_json::Value =
            serde_json::from_str(include_str!("../../tauri.conf.json"))?;
        let updater_config: tauri_plugin_updater::Config =
            serde_json::from_value(config["plugins"]["updater"].clone())?;
        assert!(!updater_config.pubkey.trim().is_empty());
        assert_eq!(updater_config.endpoints.len(), 1);
        assert_eq!(
            updater_config.endpoints[0].as_str(),
            "https://github.com/aidyou/peerdav/releases/latest/download/latest.json"
        );
        let manager = UpdateManager::new("0.1.0".into(), Some(&config["plugins"]["updater"]));
        assert!(manager.snapshot().configured);
        Ok(())
    }

    #[test]
    fn missing_or_incomplete_configuration_disables_updates() {
        use serde_json::json;
        assert!(!is_configured(None));
        for config in [
            json!(null),
            json!({"pubkey": ""}),
            json!({"pubkey": "key", "endpoints": []}),
            json!({"pubkey": "  ", "endpoints": ["https://example.com/latest.json"]}),
            json!({"pubkey": "key", "endpoints": ["not a URL"]}),
        ] {
            assert!(!is_configured(Some(&config)));
        }
        assert!(is_configured(Some(&json!({
            "pubkey": "configured-key",
            "endpoints": ["https://example.com/latest.json"]
        }))));
    }

    #[test]
    fn serializes_frontend_contract() -> Result<(), serde_json::Error> {
        let manager = UpdateManager::new("0.1.0".into(), None);
        let value = serde_json::to_value(manager.snapshot())?;
        assert_eq!(value["phase"], "idle");
        assert_eq!(value["currentVersion"], "0.1.0");
        assert!(value["configured"].is_boolean());
        Ok(())
    }
}
