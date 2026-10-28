use serde::Serialize;
use tauri_plugin_updater::Update;

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UpdateStatus {
    pub configured: bool,
    pub current_version: String,
    pub phase: UpdatePhase,
    pub version: Option<String>,
    pub notes: Option<String>,
    pub downloaded: u64,
    pub total: Option<u64>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum UpdatePhase {
    #[default]
    Idle,
    Checking,
    Downloading,
    Ready,
    Installing,
    UpToDate,
    Failed,
}

pub(super) struct ReadyUpdate {
    pub update: Update,
    pub bytes: Vec<u8>,
}
