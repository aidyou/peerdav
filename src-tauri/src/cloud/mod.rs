//! Provider-neutral cloud object storage interfaces.
pub mod cloud_commands;
pub mod object_commands;
pub mod object_guard;
pub mod object_runtime;
pub mod object_runtime_types;
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
pub mod config;
pub mod config_types;
mod proxy;
pub mod runtime;
pub mod runtime_recovery;
pub mod runtime_root;
pub mod runtime_schedule;
pub mod runtime_types;
mod s3;
pub mod traits;
pub mod types;
pub use s3::S3Store;
