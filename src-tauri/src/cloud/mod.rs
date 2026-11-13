//! Provider-neutral cloud object storage interfaces.
pub mod cloud_commands;
pub mod object_commands;
pub mod object_guard;
pub mod object_runtime;
pub mod directory_types;
pub mod directory_upload;
pub mod object_runtime_types;
mod object_move;
pub mod object_service;
pub mod object_types;
pub use object_guard::{ObjectGuard, ObjectLease};
pub use object_service::{
    ConfigRevision, ObjectService, ObjectServiceError, PhysicalRoot, bytes_stream, download,
    put_bounded,
};
pub use object_types::{
    BrowsePage, DeletePlan, ObjectGuardError, ObjectRef, ProtectedIdentity, ProtectedMapping,
    ProtectionState, StorageIdentityAssociation,
};
mod capability_cache;
pub mod config;
pub mod config_types;
pub mod protocol_types;
pub mod protocol_registry;
pub mod oauth;
mod oauth_loopback;
mod network;
pub mod oauth_types;
pub mod official_types;
pub mod official_api;
pub mod official_api_types;
pub mod official_runtime;
// Yandex Disk stays disabled until its developer account is confirmed; its
// adapter file remains on disk and is excluded by `official_drives/mod.rs`.
mod official_drives;
mod google_store;
mod official_store;
mod proxy;
pub mod service;
mod local_files;
pub mod runtime;
pub mod runtime_recovery;
pub mod runtime_root;
pub mod runtime_schedule;
pub mod runtime_types;
mod s3;
pub mod traits;
pub mod types;
pub use s3::S3Store;
