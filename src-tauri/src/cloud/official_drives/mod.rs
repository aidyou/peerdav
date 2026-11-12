//! Official drive protocol adapters: each provider owns its own wire protocol.
//!
//! Baidu Netdisk and Dropbox are compiled and wired into `traits::create`.
//! The Yandex Disk adapter stays on disk but is disabled until its developer
//! account is confirmed; re-enable `yandex` here and its branch in
//! `traits::create` together.
mod common;
mod baidu;
mod dropbox;
mod traits;
// mod yandex;

pub(crate) use traits::{create, OfficialDrive};