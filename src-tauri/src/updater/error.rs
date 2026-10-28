use crate::error::{AppError, error_chain, visible_reason};
use serde_json::json;

#[derive(Debug, thiserror::Error)]
pub(super) enum UpdateError {
    #[error("更新未配置 / Updates are not configured")]
    NotConfigured,
    #[error("没有已下载的更新 / No downloaded update is ready")]
    NotReady,
    #[error("更新操作正在进行 / An update operation is in progress")]
    Busy,
    #[error("更新操作失败 / Update operation failed")]
    Plugin(#[from] tauri_plugin_updater::Error),
    #[error("发送更新事件失败 / Failed to send update event")]
    Event(#[from] tauri::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_keep_stable_codes_and_diagnostics() {
        for (error, code) in [
            (UpdateError::NotConfigured, "UPDATE_NOT_CONFIGURED"),
            (UpdateError::NotReady, "UPDATE_NOT_READY"),
            (UpdateError::Busy, "UPDATE_BUSY"),
        ] {
            let mapped = AppError::from(error);
            assert_eq!(mapped.code, code);
            assert!(mapped.detail.is_some());
            assert_eq!(
                mapped.context.as_ref().map(|value| &value["operation"]),
                Some(&json!("update"))
            );
        }
    }
}

impl From<UpdateError> for AppError {
    fn from(error: UpdateError) -> Self {
        let code = match &error {
            UpdateError::NotConfigured => "UPDATE_NOT_CONFIGURED",
            UpdateError::NotReady => "UPDATE_NOT_READY",
            UpdateError::Busy => "UPDATE_BUSY",
            UpdateError::Plugin(_) => "UPDATE_FAILED",
            UpdateError::Event(_) => "UPDATE_EVENT_FAILED",
        };
        let detail = error_chain(&error);
        let reason = match &error {
            UpdateError::Plugin(source) => visible_reason(&source.to_string()),
            _ => visible_reason(&error.to_string()),
        };
        log::error!("peerdav.error code={code} detail={detail} operation=update");
        Self {
            code: code.to_owned(),
            message: format!("更新失败：{reason} / Update failed: {reason}"),
            detail: Some(detail),
            context: Some(json!({ "operation": "update", "reason": reason })),
        }
    }
}
