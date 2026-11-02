//! S3 object storage primitives and their isolated connection transport.
mod core_backend;
#[cfg(test)]
mod core_tests;
pub mod proxy;
pub use core_backend::{CoreS3Backend, cloud_error_code, cloud_to_file};
mod read_bridge;
pub use read_bridge::CoreS3ReadBridge;
mod s3;
pub mod traits;
pub mod types;

pub use s3::{S3Store, is_private};
pub use traits::ObjectStore;
pub use types::*;
