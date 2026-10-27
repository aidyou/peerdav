//! Domain errors and the single domain-to-IPC error mapping.

use std::fmt::Write as _;
use std::path::PathBuf;

use argon2::password_hash;
use serde::Serialize;
use serde_json::{Value, json};

/// Internal domain error. User-facing text is built exactly once in
/// [`AppError::from`]; every variant keeps the raw cause available for logs.
#[derive(Debug, thiserror::Error)]
pub enum DomainError {
    #[error("database operation `{operation}` failed")]
    Database {
        operation: &'static str,
        #[source]
        source: rusqlite::Error,
    },

    #[error("failed to open database at {path}")]
    DatabaseOpen {
        path: PathBuf,
        #[source]
        source: rusqlite::Error,
    },

    #[error("I/O operation `{operation}` failed")]
    Io {
        operation: &'static str,
        path: Option<PathBuf>,
        #[source]
        source: std::io::Error,
    },

    #[error("release builds cannot connect to this device")]
    SelfConnectionForbidden,

    #[error("vault is not initialized")]
    VaultNotInitialized,

    #[error("vault is already initialized")]
    VaultAlreadyInitialized,

    #[error("vault is locked")]
    VaultLocked,

    #[error("vault key file is not configured")]
    KeyFileNotConfigured,

    #[error("vault key file already exists at {path}")]
    KeyFileExists { path: String },

    #[error("vault key file was not found at {path}")]
    KeyFileMissing { path: String },

    #[error("vault key file is invalid at {path}: {detail}")]
    KeyFileInvalid { path: String, detail: String },

    #[error("the key file does not match this vault")]
    VaultKeyMismatch,

    #[error("invalid key storage directory: {detail}")]
    InvalidKeyDirectory { detail: String },

    #[error("the vault must be locked before a key-loss reset")]
    VaultMustBeLocked,

    #[error("this vault has no recovery code configured")]
    RecoveryCodeNotConfigured,

    #[error("the recovery code is invalid")]
    InvalidRecoveryCode,

    #[error("invalid admin credentials")]
    InvalidAdminCredentials,

    #[error("password does not meet requirements: {reason}")]
    WeakPassword { reason: &'static str },

    #[error("invalid admin username: {reason}")]
    InvalidUsername { reason: &'static str },

    #[error("new password equals current password")]
    PasswordUnchanged,

    #[error("password hashing failed")]
    PasswordHash {
        #[source]
        source: password_hash::Error,
    },

    #[error("cryptographic operation `{operation}` failed: {detail}")]
    Crypto {
        operation: &'static str,
        detail: String,
    },

    #[error("serialization operation `{operation}` failed")]
    Serialization {
        operation: &'static str,
        #[source]
        source: serde_json::Error,
    },

    #[error("vault data is corrupted: {detail}")]
    Corrupted { detail: String },

    #[error("secret name is reserved")]
    ReservedSecretName,

    #[error("secret name is invalid")]
    InvalidSecretName,

    #[error("secret `{name}` was not found")]
    SecretNotFound { name: String },

    #[error("admin must change the password before performing admin operations")]
    AdminPasswordChangeRequired,

    #[error("share `{id}` was not found")]
    ShareNotFound { id: String },

    #[error("invalid share configuration: {detail}")]
    InvalidShareConfig { detail: String },

    #[error("invalid policy rule: {detail}")]
    InvalidPolicyRule { detail: String },

    #[error("pending request `{id}` was not found")]
    PendingRequestNotFound { id: String },

    #[error("authorization `{id}` was not found")]
    AuthorizationNotFound { id: String },

    #[error("server is already running")]
    ServerAlreadyRunning,

    #[error("failed to bind server to {address}")]
    ServerBind {
        address: String,
        #[source]
        source: std::io::Error,
    },

    #[error("server task failed: {detail}")]
    ServerTask { detail: String },

    #[error("invalid peer URL: {detail}")]
    InvalidPeerUrl { detail: String },

    #[error("the peer requires administrator approval")]
    PeerAuthorizationRequired,

    #[error("the peer rejected the authorization code")]
    PeerAuthorizationRejected,

    #[error("the access approval request expired before the client claimed it")]
    PeerAccessExpired,

    #[error("timed out waiting for administrator approval")]
    PeerAccessTimeout,

    #[error("the client canceled the access request")]
    PeerAccessCanceled,

    #[error("the server denied this peer by its access rules (HTTP 403)")]
    PeerAccessDenied { peer_ip: Option<String> },

    #[error("peer request `{operation}` failed: {detail}")]
    PeerRequest {
        operation: &'static str,
        status: Option<u16>,
        detail: String,
    },

    #[error("remote file `{path}` was not found")]
    RemoteNotFound { path: String },

    #[error("peer `{id}` was not found")]
    PeerNotFound { id: String },

    #[error("peer protocol error: {detail}")]
    PeerProtocol { detail: String },

    #[error("invalid remote path: {detail}")]
    RemotePathInvalid { detail: String },

    #[error("the file transfer was canceled")]
    TransferCanceled,

    #[error("file size {size} exceeds the {limit} byte limit")]
    TransferTooLarge { size: u64, limit: u64 },

    #[error("preview size {size} exceeds the {limit} byte limit")]
    PreviewTooLarge { size: u64, limit: u64 },

    #[error("preview has {lines} lines, exceeding the limit of {limit}")]
    PreviewTooManyLines { lines: usize, limit: usize },

    #[error("destination already exists: {path}")]
    TransferConflict { path: String },

    #[error("external edit `{id}` was not found")]
    ExternalEditNotFound { id: String },

    #[error("failed to open `{path}` with the system application: {detail}")]
    ExternalOpen { path: String, detail: String },

    #[error("failed to reveal `{path}` in the file manager: {detail}")]
    ExternalReveal { path: String, detail: String },

    #[error("the upload queue contains {count} files, exceeding the limit of {limit}")]
    UploadQueueTooLarge { count: usize, limit: usize },

    #[error("the download queue contains {count} entries, exceeding the limit of {limit}")]
    DownloadQueueTooLarge { count: usize, limit: usize },

    #[error("discovery operation `{operation}` failed: {detail}")]
    Discovery {
        operation: &'static str,
        detail: String,
    },

    #[error("internal error: {detail}")]
    Internal { detail: String },
}

/// Structured error returned by every Tauri command.
#[derive(Debug, Clone, Serialize)]
pub struct AppError {
    /// Stable machine-readable error code.
    pub code: String,
    /// One layer of user-facing context, Chinese + English.
    pub message: String,
    /// Raw underlying error information for logs and the details panel.
    pub detail: Option<String>,
    /// Structured context such as a path or operation.
    pub context: Option<Value>,
}

impl From<DomainError> for AppError {
    fn from(error: DomainError) -> Self {
        Self::from_ref(&error)
    }
}

impl AppError {
    /// Maps a domain error reference to the IPC representation. Background
    /// tasks use this so they can emit the same structured payload without
    /// taking ownership of the error.
    pub fn from_ref(error: &DomainError) -> Self {
        let code = error_code(error).to_owned();
        let message = user_message(error);
        let detail = Some(error_chain(error));
        let context = error_context(error);

        log::error!(
            "peerdav.error code={code} message={message:?} detail={detail:?} context={context:?}"
        );

        Self {
            code,
            message,
            detail,
            context,
        }
    }
}

/// Renders an error and its full source chain, without losing raw causes.
pub(crate) fn error_chain(error: &(dyn std::error::Error + 'static)) -> String {
    let mut rendered = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        let _ = write!(rendered, ": {cause}");
        source = cause.source();
    }
    rendered
}

fn error_code(error: &DomainError) -> &'static str {
    match error {
        DomainError::Database { .. } => "DB_QUERY_FAILED",
        DomainError::DatabaseOpen { .. } => "DB_OPEN_FAILED",
        DomainError::Io { .. } => "IO_FAILED",
        DomainError::VaultNotInitialized => "VAULT_NOT_INITIALIZED",
        DomainError::VaultAlreadyInitialized => "VAULT_ALREADY_INITIALIZED",
        DomainError::VaultLocked => "VAULT_LOCKED",
        DomainError::KeyFileNotConfigured => "VAULT_KEY_FILE_NOT_CONFIGURED",
        DomainError::KeyFileExists { .. } => "VAULT_KEY_FILE_EXISTS",
        DomainError::KeyFileMissing { .. } => "VAULT_KEY_FILE_MISSING",
        DomainError::KeyFileInvalid { .. } => "VAULT_KEY_FILE_INVALID",
        DomainError::VaultKeyMismatch => "VAULT_KEY_MISMATCH",
        DomainError::InvalidKeyDirectory { .. } => "VAULT_INVALID_KEY_DIRECTORY",
        DomainError::VaultMustBeLocked => "VAULT_MUST_BE_LOCKED",
        DomainError::RecoveryCodeNotConfigured => "VAULT_RECOVERY_CODE_NOT_CONFIGURED",
        DomainError::InvalidRecoveryCode => "VAULT_INVALID_RECOVERY_CODE",
        DomainError::InvalidAdminCredentials => "AUTH_INVALID_CREDENTIALS",
        DomainError::WeakPassword { .. } => "VALIDATION_WEAK_PASSWORD",
        DomainError::InvalidUsername { .. } => "VALIDATION_INVALID_USERNAME",
        DomainError::PasswordUnchanged => "AUTH_PASSWORD_UNCHANGED",
        DomainError::Crypto { .. } => "CRYPTO_FAILED",
        DomainError::PasswordHash { .. } => "AUTH_HASH_FAILED",
        DomainError::Serialization { .. } => "SERIALIZATION_FAILED",
        DomainError::Corrupted { .. } => "VAULT_DATA_CORRUPTED",
        DomainError::ReservedSecretName => "VAULT_RESERVED_SECRET_NAME",
        DomainError::InvalidSecretName => "VAULT_INVALID_SECRET_NAME",
        DomainError::SecretNotFound { .. } => "VAULT_SECRET_NOT_FOUND",
        DomainError::AdminPasswordChangeRequired => "AUTH_PASSWORD_CHANGE_REQUIRED",
        DomainError::ShareNotFound { .. } => "SHARE_NOT_FOUND",
        DomainError::InvalidShareConfig { .. } => "SHARE_INVALID_CONFIG",
        DomainError::InvalidPolicyRule { .. } => "POLICY_INVALID_RULE",
        DomainError::PendingRequestNotFound { .. } => "SERVER_PENDING_REQUEST_NOT_FOUND",
        DomainError::AuthorizationNotFound { .. } => "SERVER_AUTHORIZATION_NOT_FOUND",
        DomainError::ServerAlreadyRunning => "SERVER_ALREADY_RUNNING",
        DomainError::ServerBind { .. } => "SERVER_BIND_FAILED",
        DomainError::ServerTask { .. } => "SERVER_TASK_FAILED",
        DomainError::SelfConnectionForbidden => "PEER_SELF_CONNECTION_FORBIDDEN",
        DomainError::InvalidPeerUrl { .. } => "PEER_INVALID_URL",
        DomainError::PeerAuthorizationRequired => "PEER_AUTHORIZATION_REQUIRED",
        DomainError::PeerAuthorizationRejected => "PEER_AUTHORIZATION_REJECTED",
        DomainError::PeerAccessExpired => "PEER_ACCESS_EXPIRED",
        DomainError::PeerAccessTimeout => "PEER_ACCESS_TIMEOUT",
        DomainError::PeerAccessCanceled => "PEER_ACCESS_CANCELED",
        DomainError::PeerAccessDenied { .. } => "PEER_ACCESS_DENIED",
        DomainError::PeerRequest { .. } => "PEER_REQUEST_FAILED",
        DomainError::RemoteNotFound { .. } => "PEER_REMOTE_NOT_FOUND",
        DomainError::PeerNotFound { .. } => "PEER_NOT_FOUND",
        DomainError::PeerProtocol { .. } => "PEER_PROTOCOL_ERROR",
        DomainError::RemotePathInvalid { .. } => "PEER_INVALID_PATH",
        DomainError::TransferCanceled => "TRANSFER_CANCELED",
        DomainError::TransferTooLarge { .. } => "TRANSFER_TOO_LARGE",
        DomainError::PreviewTooLarge { .. } => "PEER_PREVIEW_TOO_LARGE",
        DomainError::PreviewTooManyLines { .. } => "PEER_PREVIEW_TOO_MANY_LINES",
        DomainError::TransferConflict { .. } => "TRANSFER_CONFLICT",
        DomainError::ExternalEditNotFound { .. } => "PEER_EDIT_NOT_FOUND",
        DomainError::ExternalOpen { .. } => "PEER_OPEN_FAILED",
        DomainError::ExternalReveal { .. } => "PEER_REVEAL_FAILED",
        DomainError::UploadQueueTooLarge { .. } => "UPLOAD_QUEUE_TOO_LARGE",
        DomainError::DownloadQueueTooLarge { .. } => "DOWNLOAD_QUEUE_TOO_LARGE",
        DomainError::Discovery { .. } => "DISCOVERY_FAILED",
        DomainError::Internal { .. } => "INTERNAL_ERROR",
    }
}

fn user_message(error: &DomainError) -> String {
    match error {
        DomainError::Database { .. } => {
            "访问本地数据库失败 / Failed to access the local database.".to_owned()
        }
        DomainError::DatabaseOpen { .. } => {
            "打开本地数据库失败 / Failed to open the local database.".to_owned()
        }
        DomainError::SelfConnectionForbidden => "发布版本不允许连接本机 / Release builds cannot connect to this device.".to_owned(),
        DomainError::Io { .. } => "文件操作失败 / File operation failed.".to_owned(),
        DomainError::VaultNotInitialized => {
            "保险箱尚未初始化 / The vault has not been initialized.".to_owned()
        }
        DomainError::VaultAlreadyInitialized => {
            "保险箱已初始化 / The vault is already initialized.".to_owned()
        }
        DomainError::VaultLocked => {
            "保险箱已锁定，请先解锁 / The vault is locked. Unlock it first.".to_owned()
        }
        DomainError::KeyFileNotConfigured => {
            "尚未配置保险箱密钥文件 / No vault key file is configured.".to_owned()
        }
        DomainError::KeyFileExists { .. } => {
            "该目录已存在密钥文件，请选择其他目录 / A key file already exists in this directory; choose another directory."
                .to_owned()
        }
        DomainError::KeyFileMissing { .. } => {
            "找不到保险箱密钥文件，请重新选择备份密钥 / The vault key file is missing; select a backup key file."
                .to_owned()
        }
        DomainError::KeyFileInvalid { .. } => {
            "保险箱密钥文件无效 / The vault key file is invalid.".to_owned()
        }
        DomainError::VaultKeyMismatch => {
            "密钥文件与当前保险箱不匹配 / The key file does not match this vault.".to_owned()
        }
        DomainError::InvalidKeyDirectory { .. } => {
            "密钥保存目录无效 / The key storage directory is invalid.".to_owned()
        }
        DomainError::VaultMustBeLocked => {
            "请先锁定保险箱再执行重置 / Lock the vault before resetting it.".to_owned()
        }
        DomainError::RecoveryCodeNotConfigured => {
            "该保险箱未配置恢复码，无法执行重置 / This vault has no recovery code configured."
                .to_owned()
        }
        DomainError::InvalidRecoveryCode => {
            "恢复码无效 / The recovery code is invalid.".to_owned()
        }
        DomainError::InvalidAdminCredentials => {
            "管理员登录失败：用户名或密码错误 / Admin login failed: invalid username or password."
                .to_owned()
        }
        DomainError::WeakPassword { reason } => weak_password_message(reason).to_owned(),
        DomainError::InvalidUsername { reason } => invalid_username_message(reason).to_owned(),
        DomainError::PasswordUnchanged => {
            "新密码不能与当前密码相同 / The new password must differ from the current password."
                .to_owned()
        }
        DomainError::Crypto { .. } => {
            "保险箱加密操作失败 / Vault cryptographic operation failed.".to_owned()
        }
        DomainError::PasswordHash { .. } => {
            "密码处理失败 / Password processing failed.".to_owned()
        }
        DomainError::Serialization { .. } => "内部数据处理失败 / Internal data processing failed.".to_owned(),
        DomainError::Corrupted { .. } => {
            "保险箱数据已损坏 / The vault data is corrupted.".to_owned()
        }
        DomainError::ReservedSecretName => {
            "该秘密名称由系统保留 / This secret name is reserved by the system.".to_owned()
        }
        DomainError::InvalidSecretName => {
            "秘密名称无效 / The secret name is invalid.".to_owned()
        }
        DomainError::SecretNotFound { .. } => {
            "未找到该秘密记录 / The secret record was not found.".to_owned()
        }
        DomainError::AdminPasswordChangeRequired => {
            "首次登录请先修改管理员密码 / Change the admin password before continuing.".to_owned()
        }
        DomainError::ShareNotFound { .. } => {
            "未找到该共享目录 / The shared directory was not found.".to_owned()
        }
        DomainError::InvalidShareConfig { .. } => {
            "共享目录配置无效 / The shared directory configuration is invalid.".to_owned()
        }
        DomainError::InvalidPolicyRule { .. } => {
            "访问规则无效 / The access rule is invalid.".to_owned()
        }
        DomainError::PendingRequestNotFound { .. } => {
            "该待审批请求已不存在 / The pending request no longer exists.".to_owned()
        }
        DomainError::AuthorizationNotFound { .. } => {
            "未找到该授权记录 / The authorization was not found.".to_owned()
        }
        DomainError::ServerAlreadyRunning => {
            "服务已在运行 / The server is already running.".to_owned()
        }
        DomainError::ServerBind { .. } => {
            "启动服务失败：无法绑定地址 / Failed to start the server: cannot bind the address."
                .to_owned()
        }
        DomainError::ServerTask { .. } => {
            "服务任务异常退出 / The server task stopped unexpectedly.".to_owned()
        }
        DomainError::InvalidPeerUrl { .. } => {
            "服务器地址无效，仅支持 https:// 地址（例如 https://10.0.0.5:4918）/ The server address is invalid; only https:// addresses are supported (for example https://10.0.0.5:4918)."
                .to_owned()
        }
        DomainError::PeerAuthorizationRequired => {
            "尚未获得授权：请在服务器端批准该访问申请 / Not authorized yet: approve the access request on the server."
                .to_owned()
        }
        DomainError::PeerAuthorizationRejected => {
            "授权码无效或已被撤销 / The authorization code is invalid or has been revoked."
                .to_owned()
        }
        DomainError::PeerAccessExpired => {
            "授权申请已失效，请重新申请 / The access request expired. Request access again."
                .to_owned()
        }
        DomainError::PeerAccessTimeout => {
            "等待管理员批准超时 / Timed out waiting for administrator approval.".to_owned()
        }
        DomainError::PeerAccessCanceled => {
            "已取消访问申请 / The access request was canceled.".to_owned()
        }
        DomainError::PeerAccessDenied { peer_ip } => match peer_ip {
            Some(peer_ip) => format!(
                "服务器按共享访问规则拒绝了本机（服务器看到的来源地址：{peer_ip}）。请在服务器端把该地址或所在网段加入共享的允许规则 / The server denied this machine by its share access rules (source address seen by the server: {peer_ip}). Add that address or its subnet to the share's allow rules."
            ),
            None => "服务器按共享访问规则拒绝了本机。请在服务器端把本机地址或所在网段加入共享的允许规则 / The server denied this machine by its share access rules. Add this machine's address or subnet to the share's allow rules.".to_owned(),
        },
        DomainError::PeerRequest { status, detail, .. } => match status {
            Some(status) => format!(
                "请求服务器失败：HTTP {status}（{detail}）/ The request to the server failed: HTTP {status} ({detail})."
            ),
            None => format!(
                "请求服务器失败：{detail} / The request to the server failed: {detail}."
            ),
        },
        DomainError::RemoteNotFound { .. } => {
            "服务器上不存在该文件 / The file does not exist on the server.".to_owned()
        }
        DomainError::PeerNotFound { .. } => {
            "未找到已保存的服务器 / The saved server was not found.".to_owned()
        }
        DomainError::PeerProtocol { .. } => {
            "服务器响应无法识别 / The server response could not be understood.".to_owned()
        }
        DomainError::RemotePathInvalid { .. } => {
            "远程路径无效 / The remote path is invalid.".to_owned()
        }
        DomainError::TransferCanceled => {
            "已取消文件传输 / The file transfer was canceled.".to_owned()
        }
        DomainError::TransferTooLarge { .. } => {
            "文件超过大小限制 / The file exceeds the size limit.".to_owned()
        }
        DomainError::PreviewTooLarge { .. } => {
            "文件超过在线预览大小限制 / The file exceeds the online preview size limit."
                .to_owned()
        }
        DomainError::PreviewTooManyLines { .. } => {
            "文件超过在线预览行数限制 / The file exceeds the online preview line limit."
                .to_owned()
        }
        DomainError::TransferConflict { .. } => {
            "目标文件已存在 / The destination file already exists.".to_owned()
        }
        DomainError::ExternalEditNotFound { .. } => {
            "外部编辑记录已失效 / The external edit session no longer exists.".to_owned()
        }
        DomainError::ExternalOpen { .. } => {
            "无法使用外部应用打开该文件 / Could not open the file with an external application."
                .to_owned()
        }
        DomainError::ExternalReveal { .. } => {
            "无法在文件管理器中显示该文件 / Could not reveal the file in the file manager."
                .to_owned()
        }
        DomainError::UploadQueueTooLarge { limit, .. } => {
            format!("一次最多上传 {limit} 个文件 / At most {limit} files can be uploaded at once.")
        }
        DomainError::DownloadQueueTooLarge { limit, .. } => {
            format!(
                "一次最多下载 {limit} 个文件或目录 / At most {limit} files or directories can be downloaded at once."
            )
        }
        DomainError::Discovery { .. } => {
            "局域网发现失败 / LAN discovery failed.".to_owned()
        }
        DomainError::Internal { .. } => "内部错误 / Internal error.".to_owned(),
    }
}

fn weak_password_message(reason: &str) -> &'static str {
    match reason {
        "blank" => "密码不能为空白 / The password must not be blank.",
        "too_long" => {
            "密码长度不能超过 1024 个字符 / The password must not exceed 1024 characters."
        }
        "edge_whitespace" => {
            "密码首尾不能包含空格 / The password must not start or end with whitespace."
        }
        "missing_letter" => {
            "密码必须包含至少一个字母 / The password must contain at least one letter."
        }
        "missing_digit" => {
            "密码必须包含至少一个数字 / The password must contain at least one digit."
        }
        "invalid_char" => {
            "密码只能包含英文字母、数字、符号和中间空格 / The password may only contain ASCII letters, digits, symbols, and inner spaces."
        }
        _ => "密码长度至少为 6 个字符 / The password must be at least 6 characters.",
    }
}

fn invalid_username_message(reason: &str) -> &'static str {
    match reason {
        "blank" => "管理员用户名不能为空白 / The admin username must not be blank.",
        "too_long" => {
            "管理员用户名不能超过 64 个字符 / The admin username must not exceed 64 characters."
        }
        _ => "管理员用户名包含无效字符 / The admin username contains invalid characters.",
    }
}

fn error_context(error: &DomainError) -> Option<Value> {
    match error {
        DomainError::DatabaseOpen { path, .. } => Some(json!({ "path": path })),
        DomainError::Io {
            path: Some(path), ..
        } => Some(json!({ "path": path })),
        DomainError::SecretNotFound { name } => Some(json!({ "name": name })),
        DomainError::ShareNotFound { id }
        | DomainError::PendingRequestNotFound { id }
        | DomainError::AuthorizationNotFound { id }
        | DomainError::PeerNotFound { id }
        | DomainError::ExternalEditNotFound { id } => Some(json!({ "id": id })),
        DomainError::RemoteNotFound { path }
        | DomainError::TransferConflict { path }
        | DomainError::RemotePathInvalid { detail: path }
        | DomainError::ExternalOpen { path, .. }
        | DomainError::ExternalReveal { path, .. } => Some(json!({ "path": path })),
        DomainError::UploadQueueTooLarge { count, limit } => {
            Some(json!({ "count": count, "limit": limit }))
        }
        DomainError::DownloadQueueTooLarge { count, limit } => {
            Some(json!({ "count": count, "limit": limit }))
        }
        DomainError::TransferTooLarge { size, limit } => {
            Some(json!({ "size": size, "limit": limit }))
        }
        DomainError::PreviewTooLarge { size, limit } => {
            Some(json!({ "size": size, "limit": limit }))
        }
        DomainError::PreviewTooManyLines { lines, limit } => {
            Some(json!({ "lines": lines, "limit": limit }))
        }
        DomainError::PeerRequest {
            operation, status, ..
        } => Some(json!({ "operation": operation, "status": status })),
        DomainError::PeerAccessDenied { peer_ip } => Some(json!({ "peerIp": peer_ip })),
        DomainError::Discovery { operation, .. } => Some(json!({ "operation": operation })),
        DomainError::ServerBind { address, .. } => Some(json!({ "address": address })),
        DomainError::KeyFileExists { path }
        | DomainError::KeyFileMissing { path }
        | DomainError::KeyFileInvalid { path, .. } => Some(json!({ "path": path })),
        DomainError::InvalidKeyDirectory { detail } => Some(json!({ "detail": detail })),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn access_denial_names_the_address_the_server_saw() {
        let error = AppError::from(DomainError::PeerAccessDenied {
            peer_ip: Some("10.126.0.7".to_owned()),
        });

        assert_eq!(error.code, "PEER_ACCESS_DENIED");
        assert!(error.message.contains("10.126.0.7"));
        assert!(error.message.contains("允许规则"));
        assert!(
            error
                .detail
                .as_deref()
                .is_some_and(|detail| detail.contains("HTTP 403"))
        );
        assert_eq!(error.context, Some(json!({ "peerIp": "10.126.0.7" })));
    }

    #[test]
    fn generic_peer_failure_keeps_the_transport_reason_visible() {
        let error = AppError::from(DomainError::PeerRequest {
            operation: "request access",
            status: Some(403),
            detail: "Forbidden".to_owned(),
        });

        assert_eq!(error.code, "PEER_REQUEST_FAILED");
        assert!(error.message.contains("HTTP 403"));
        assert!(error.message.contains("Forbidden"));
        assert!(
            error
                .detail
                .as_deref()
                .is_some_and(|detail| detail.contains("Forbidden"))
        );
    }
}
