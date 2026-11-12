//! Provider-specific official drive operations.
//!
//! Each provider owns its own wire protocol here; `OfficialApi` only dispatches.
//! Optional capabilities must fail closed instead of returning empty success.
use futures_util::future::BoxFuture;
use serde_json::Value;
use zeroize::Zeroizing;

use super::baidu::BaiduDrive;
use super::dropbox::DropboxDrive;
use crate::cloud::oauth_types::OfficialProvider;
use crate::cloud::official_api_types::{OfficialApiError, OfficialPage};

/// Build the provider adapter. `None` means the provider still uses the legacy
/// dispatcher in `OfficialApi`.
pub(crate) fn create(
    provider: OfficialProvider,
    client: reqwest::Client,
    access_token: Zeroizing<String>,
) -> Option<Box<dyn OfficialDrive>> {
    match provider {
        OfficialProvider::BaiduNetdisk => Some(Box::new(BaiduDrive { client, access_token })),
        OfficialProvider::Dropbox => Some(Box::new(DropboxDrive { client, access_token })),
        _ => None,
    }
}

pub(crate) trait OfficialDrive: Send + Sync {
    fn provider(&self) -> OfficialProvider;

    /// List one provider page. `parent` is the provider's own opaque root/entry
    /// identity; callers must not parse it.
    fn list<'a>(
        &'a self,
        parent: &'a str,
        cursor: Option<&'a str>,
    ) -> BoxFuture<'a, Result<OfficialPage, OfficialApiError>>;

    /// Stream one file's content. The returned response must be a successful
    /// body response; providers that redirect to a signed URL fetch it without
    /// forwarding the bearer token.
    fn media<'a>(
        &'a self,
        id: &'a str,
    ) -> BoxFuture<'a, Result<reqwest::Response, OfficialApiError>>;

    fn metadata<'a>(
        &'a self,
        _id: &'a str,
    ) -> BoxFuture<'a, Result<Value, OfficialApiError>> {
        Box::pin(async { Err(OfficialApiError::Unsupported) })
    }

    fn delete_item<'a>(
        &'a self,
        _id: &'a str,
        _revision: &'a str,
    ) -> BoxFuture<'a, Result<(), OfficialApiError>> {
        Box::pin(async { Err(OfficialApiError::Unsupported) })
    }

    fn move_item<'a>(
        &'a self,
        _id: &'a str,
        _from: &'a str,
        _destination: &'a str,
    ) -> BoxFuture<'a, Result<(), OfficialApiError>> {
        Box::pin(async { Err(OfficialApiError::Unsupported) })
    }

    fn create_folder<'a>(
        &'a self,
        _parent: &'a str,
        _name: &'a str,
    ) -> BoxFuture<'a, Result<String, OfficialApiError>> {
        Box::pin(async { Err(OfficialApiError::Unsupported) })
    }

    fn create_folder_id<'a>(
        &'a self,
        _parent: &'a str,
        _name: &'a str,
    ) -> BoxFuture<'a, Result<String, OfficialApiError>> {
        Box::pin(async { Err(OfficialApiError::Unsupported) })
    }

    fn replace_content<'a>(
        &'a self,
        _id: &'a str,
        _expected: &'a str,
        _bytes: Vec<u8>,
    ) -> BoxFuture<'a, Result<String, OfficialApiError>> {
        Box::pin(async { Err(OfficialApiError::Unsupported) })
    }

    fn upload_new<'a>(
        &'a self,
        _parent: &'a str,
        _name: &'a str,
        _file: std::fs::File,
        _size: u64,
        _progress: Option<std::sync::Arc<std::sync::atomic::AtomicU64>>,
    ) -> BoxFuture<'a, Result<String, OfficialApiError>> {
        Box::pin(async { Err(OfficialApiError::Unsupported) })
    }

    fn replace_file_stream<'a>(
        &'a self,
        _id: &'a str,
        _file: std::fs::File,
        _size: u64,
        _expected: Option<&'a str>,
        _progress: Option<std::sync::Arc<std::sync::atomic::AtomicU64>>,
    ) -> BoxFuture<'a, Result<(), OfficialApiError>> {
        Box::pin(async { Err(OfficialApiError::Unsupported) })
    }

    fn export<'a>(
        &'a self,
        _id: &'a str,
        _mime: &'a str,
    ) -> BoxFuture<'a, Result<reqwest::Response, OfficialApiError>> {
        Box::pin(async { Err(OfficialApiError::Unsupported) })
    }
}