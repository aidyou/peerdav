/// Connection-level browser orchestration, independent of provider names.
#[derive(Clone, Debug, Default, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionBrowserCapabilities {
    pub select_root: bool,
    pub sync_drives: bool,
}

use serde::Serialize;

/// Static configuration metadata contains no credentials or executable UI code.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProtocolField {
    pub key: &'static str,
    pub kind: &'static str,
    pub label_zh: &'static str,
    pub label_en: &'static str,
    pub required: bool,
    pub secret: bool,
    pub advanced: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProtocolDescriptor {
    pub id: &'static str,
    pub name: &'static str,
    pub config_version: u32,
    pub auth: &'static str,
    pub browse_only: bool,
    pub fields: Vec<ProtocolField>,
}
