use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ObjectEntryDto {
    pub key: String,
    pub size: u64,
    pub etag: Option<String>,
    pub protected: bool,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ObjectPageDto {
    pub prefix: String,
    pub objects: Vec<ObjectEntryDto>,
    pub next: Option<String>,
    pub complete: bool,
    pub prefix_protected: bool,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ObjectDeletePlanDto {
    pub plan_id: String,
    pub expires_at_unix: u64,
    pub object_count: usize,
    pub connection_revision: u64,
    /// Complete, exact confirmation snapshot; never a truncated preview.
    pub entries: Vec<ObjectEntryDto>,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ObjectDeleteResultDto {
    pub deleted: Vec<String>,
    pub failed: Vec<ObjectDeleteFailureDto>,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ObjectDeleteFailureDto {
    pub key: String,
    pub error: crate::AppError,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ObjectDiscoveryDto {
    pub complete: bool,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ObjectPreparedDto {
    pub physical_id: String,
    pub discovered: bool,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ObjectIdentityDto {
    pub physical_id: String,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum ObjectIdentityInput {
    New {},
    Existing {
        #[serde(rename = "physicalId")]
        physical_id: String,
    },
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ObjectLocalCapabilityDto {
    pub token: String,
    pub file_name: Option<String>,
}
/// One file inside a batch or folder download. The relative path is what the
/// capability root sees; it never carries the bucket prefix.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ObjectDownloadItemDto {
    pub key: String,
    pub relative_path: String,
}
/// Result of one batch download. `local_root` is the folder the user just
/// picked, so the UI can offer "reveal in folder" once the queue finishes.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ObjectDownloadBatchDto {
    pub transfers: Vec<ObjectTransferDto>,
    pub local_root: String,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ObjectTransferDto {
    pub transfer_id: String,
    pub connection_id: String,
    pub connection_revision: u64,
    pub direction: String,
    pub key: Option<String>,
    pub size_hint: Option<u64>,
    pub phase: String,
    pub error: Option<crate::AppError>,
    /// No provider upload id is available: never imply successful cleanup.
    pub recovery_required: bool,
}

#[derive(Default)]
pub(crate) struct ObjectBrowseOperation {
    pub ledger: super::object_service::BudgetLedger,
    pub last_token: Option<String>,
    pub next: Option<String>,
    pub complete: bool,
    pub seen: std::collections::HashSet<String>,
}

pub(crate) struct ObjectSession {
    pub browse: tokio::sync::Mutex<std::collections::HashMap<String, ObjectBrowseOperation>>,
    pub revision: u64,
    /// Bucket this session is bound to; switching buckets rebuilds the session.
    pub bucket: String,
    pub service: std::sync::Arc<super::ObjectService>,
    pub store: std::sync::Arc<dyn super::traits::ObjectStore>,
    pub identity: super::ProtectedIdentity,
}
pub(crate) enum LocalGrant {
    Upload(std::fs::File),
    Download(super::PhysicalRoot),
}
pub(crate) struct BoundGrant {
    pub connection_id: String,
    pub revision: u64,
    pub grant: LocalGrant,
    pub expires: std::time::Instant,
}
pub(crate) struct ObjectTask {
    pub created: std::time::Instant,
    pub status: ObjectTransferDto,
    pub cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    pub handle: Option<tokio::task::JoinHandle<()>>,
}
#[cfg(test)]
pub(crate) struct ObjectTestBackend {
    pub load: std::sync::Arc<
        dyn Fn() -> Result<super::config_types::CloudConnection, crate::AppError> + Send + Sync,
    >,
    pub store: std::sync::Arc<dyn super::traits::ObjectStore>,
}
pub(crate) const MAX_UPLOAD_DIRECTORY_DEPTH: usize = 64;
pub(crate) const MAX_UPLOAD_DIRECTORY_PATH_BYTES: usize = 4096;
pub(crate) const MAX_LOCAL_HANDLES: usize = 128;
pub(crate) const UPLOAD_DIRECTORY_TTL_SECS: u64 = 1800;

pub(crate) struct ObjectState {
    #[cfg(test)]
    pub backend: Option<ObjectTestBackend>,
    pub guard: Option<super::ObjectGuard>,
    pub sessions: std::collections::HashMap<String, std::sync::Arc<ObjectSession>>,
    /// Bucket the user explicitly selected for an open connection. Without it
    /// every later lookup would rebuild the saved default bucket session.
    pub active_buckets: std::collections::HashMap<String, String>,
    pub plans: std::collections::HashMap<String, (String, super::DeletePlan)>,
    pub grants: std::collections::HashMap<String, BoundGrant>,
    pub upload_directories:
        std::collections::HashMap<String, super::directory_types::DirectoryBatch>,
    pub tasks: std::collections::HashMap<String, ObjectTask>,
}
/// Untrusted, pre-save credentials used only to prove that the address can list
/// buckets. Nothing here is persisted; saving the connection re-validates it.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProbeBucketsInput {
    pub endpoint: String,
    #[serde(default)]
    pub region: String,
    pub access_key: String,
    pub secret_key: String,
    #[serde(default)]
    pub session_token: Option<String>,
    #[serde(default)]
    pub allow_insecure_http: bool,
    #[serde(default)]
    pub proxy: super::types::ProxyMode,
}

impl ObjectState {
    pub fn local_handle_count(&self) -> usize {
        self.grants.len() + self.upload_directories.len()
    }
    pub fn prune(&mut self) {
        let now = std::time::Instant::now();
        let unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        self.grants.retain(|_, grant| grant.expires > now);
        self.upload_directories.retain(|_, batch| {
            matches!(batch.status.phase.as_str(), "running" | "conflict")
                || batch.handle.as_ref().is_some_and(|h| !h.is_finished())
                || now.duration_since(batch.created).as_secs() < UPLOAD_DIRECTORY_TTL_SECS
        });
        self.plans
            .retain(|_, (_, plan)| plan.expires_at_unix > unix);
        self.tasks.retain(|_, task| {
            task.handle.is_some() || now.duration_since(task.created).as_secs() < 3600
        });
    }
    pub fn new() -> Self {
        Self {
            #[cfg(test)]
            backend: None,
            guard: None,
            sessions: std::collections::HashMap::new(),
            active_buckets: std::collections::HashMap::new(),
            plans: std::collections::HashMap::new(),
            grants: std::collections::HashMap::new(),
            upload_directories: std::collections::HashMap::new(),
            tasks: std::collections::HashMap::new(),
        }
    }
}
