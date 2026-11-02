//! Application-level browsing contracts; storage protocols remain independent.

pub mod adapters;
pub mod traits;
pub mod types;

pub use adapters::{cloud_list, cloud_preview, peer_list, peer_preview};
