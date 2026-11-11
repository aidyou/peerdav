//! Application-level browsing contracts; storage protocols remain independent.

pub mod adapters;
mod factory;
mod editor;
mod official_adapter;
mod official_upload;
pub mod commands;
pub mod service;
pub mod traits;
pub mod types;

pub use adapters::{cloud_list, cloud_preview, peer_list, peer_preview};
