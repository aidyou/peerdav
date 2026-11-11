//! Protocol primitives stay in file-core; application adapters add admission,
//! protected namespaces and DTO conversion without bypassing those primitives.
pub use peerdav_protocol_peerdav::peerdav_file_core::FileBackend;

use super::types::FilePageDto;
use crate::{AppError, PeerManager, PreviewPayload, cloud::runtime::CloudRuntime};
use futures_util::future::BoxFuture;

pub(crate) trait BrowserServiceFactory: Send + Sync {
    fn create<'a>(
        &'a self,
        peers: &'a PeerManager,
        cloud: &'a CloudRuntime,
        location: &'a super::types::FileLocation,
    ) -> BoxFuture<'a, Result<Option<(std::sync::Arc<dyn BrowserBackend>, String)>, AppError>>;
}

/// Mandatory application boundary: capabilities must reflect implemented provider
/// operations, not optimistic defaults. Upload selection accepts single, files and
/// folder modes through the same queue; downloads return the shared queue adapter.
/// Every operation rechecks session identity/revision and write admission. Root and
/// sync namespaces remain protected regardless of UI state. Unsupported operations
/// must return structured errors and must never return successful empty results.
/// All remote target arguments are opaque entry IDs (parent IDs for creation).
/// A path/key-backed protocol resolves its scoped ID inside the adapter only.
/// Client code must not parse IDs to derive ancestry or authorization.
/// Entry names are display labels, never identity: providers may allow same-name
/// file and directory siblings. List must preserve distinct provider IDs across
/// pagination/refresh; read, edit, delete and transfer targets bind those IDs to
/// the admitted session and parent. Name-based lookup may not pick or merge an
/// ambiguous sibling. Local download collision names do not change remote IDs.
pub(crate) trait BrowserBackend: Send + Sync {
    fn move_entry<'a>(&'a self, _peers: &'a PeerManager, _cloud: &'a CloudRuntime, _id: &'a str, _destination: &'a str) -> BoxFuture<'a, Result<(), AppError>> {
        Box::pin(async { Err(super::service::unavailable()) })
    }
    fn supports_move(&self) -> bool { false }
    fn prepare<'a>(&'a self, peers: &'a PeerManager, cloud: &'a CloudRuntime) -> BoxFuture<'a, Result<(), AppError>>;
    fn upload_page<'a>(&'a self, peers: &'a std::sync::Arc<PeerManager>, cloud: &'a std::sync::Arc<CloudRuntime>, id: &'a str, directory_token: Option<String>, cursor: Option<String>, limit: Option<usize>) -> BoxFuture<'a, Result<crate::local_upload_types::DirectoryPageDto, AppError>>;
    fn upload_resolve<'a>(&'a self, peers: &'a std::sync::Arc<PeerManager>, cloud: &'a std::sync::Arc<CloudRuntime>, id: &'a str, conflict_id: &'a str, decision: crate::cloud::directory_types::DirectoryConflictPolicy, apply_to_remaining: bool) -> BoxFuture<'a, Result<(), AppError>>;
    fn upload_action<'a>(&'a self, peers: &'a std::sync::Arc<PeerManager>, cloud: &'a std::sync::Arc<CloudRuntime>, id: &'a str, action: super::types::UploadAction) -> BoxFuture<'a, Result<super::types::FileUploadDto, AppError>>;
    fn create_dir<'a>(&'a self, peers: &'a PeerManager, cloud: &'a CloudRuntime, parent_id: &'a str, name: &'a str) -> BoxFuture<'a, Result<super::types::FileEntryDto, AppError>>;
    fn edit_read<'a>(&'a self, peers: &'a PeerManager, cloud: &'a CloudRuntime, id: &'a str) -> BoxFuture<'a, Result<super::types::EditContent, AppError>>;
    fn edit_write<'a>(&'a self, peers: &'a PeerManager, cloud: &'a CloudRuntime, id: &'a str, revision: &'a str, bytes: Vec<u8>) -> BoxFuture<'a, Result<String, AppError>>;
    fn online_preview<'a>(&'a self, peers: &'a PeerManager, cloud: &'a CloudRuntime, id: &'a str) -> BoxFuture<'a, Result<String, AppError>>;
    fn download<'a>(
        &'a self,
        peers: &'a std::sync::Arc<PeerManager>,
        cloud: &'a std::sync::Arc<CloudRuntime>,
    ) -> BoxFuture<
        'a,
        Result<
            (
                crate::download_types::DownloadBackend,
                std::sync::Arc<dyn crate::traits::TransferQueueAdapter>,
            ),
            AppError,
        >,
    >;
    fn select_upload<'a>(
        &'a self,
        peers: &'a PeerManager,
        cloud: &'a CloudRuntime,
        parent_id: String,
        paths: Vec<std::path::PathBuf>,
        mode: super::types::UploadMode,
        options: crate::types::IgnoreOptions,
    ) -> BoxFuture<'a, Result<super::types::FileUploadDto, AppError>>;
    fn release_upload<'a>(
        &'a self,
        peers: &'a PeerManager,
        cloud: &'a CloudRuntime,
        id: &'a str,
    ) -> BoxFuture<'a, Result<(), AppError>>;
    fn capabilities(&self) -> super::types::FileCapabilities;
    fn admit<'a>(
        &'a self,
        peers: &'a PeerManager,
        cloud: &'a CloudRuntime,
    ) -> BoxFuture<'a, Result<(), AppError>>;
    fn list<'a>(
        &'a self,
        peers: &'a PeerManager,
        cloud: &'a CloudRuntime,
        id: &'a str,
        next: Option<&'a str>,
    ) -> BoxFuture<'a, Result<FilePageDto, AppError>>;
    fn delete_plan<'a>(
        &'a self,
        peers: &'a PeerManager,
        cloud: &'a CloudRuntime,
        id: &'a str,
    ) -> BoxFuture<'a, Result<crate::cloud::object_runtime_types::ObjectDeletePlanDto, AppError>>;
    fn delete_execute<'a>(
        &'a self,
        peers: &'a PeerManager,
        cloud: &'a CloudRuntime,
        target: &'a str,
        plan: Option<&'a str>,
    ) -> BoxFuture<'a, Result<crate::cloud::object_runtime_types::ObjectDeleteResultDto, AppError>>;
    fn preview<'a>(
        &'a self,
        peers: &'a PeerManager,
        cloud: &'a CloudRuntime,
        id: &'a str,
    ) -> BoxFuture<'a, Result<PreviewPayload, AppError>>;
}
