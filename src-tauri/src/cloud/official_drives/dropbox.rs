//! Dropbox official API adapter.
//!
//! # Limits
//! - Deletion is not atomic. Dropbox has no conditional `delete_v2`, so
//!   `delete_item` only checks that the caller confirmed a revision; a
//!   concurrent change between listing and deletion cannot be rejected.
//! - Overwrites are atomic: `replace_content` / `replace_file_stream` send
//!   `mode: update` with the item `rev` from the listing.
//! - Entry identity is `path_display`. Renaming or moving an item invalidates
//!   previously issued IDs, so a stale tab must list again.
//! - `export` is Google-only; Dropbox files are read through `media`.
//! - `online_preview` is unavailable: Dropbox list entries carry no web link.
use futures_util::future::BoxFuture;
use serde_json::Value;
use zeroize::Zeroizing;

use super::common::{ensure_success, read_json};
use super::traits::OfficialDrive;
use crate::cloud::oauth_types::OfficialProvider;
use crate::cloud::official_api_types::{OfficialApiError, OfficialEntry, OfficialPage};

pub(crate) struct DropboxDrive {
    pub(crate) client: reqwest::Client,
    pub(crate) access_token: Zeroizing<String>,
}

impl OfficialDrive for DropboxDrive {
    fn provider(&self) -> OfficialProvider {
        OfficialProvider::Dropbox
    }

    fn list<'a>(
        &'a self,
        parent: &'a str,
        cursor: Option<&'a str>,
    ) -> BoxFuture<'a, Result<OfficialPage, OfficialApiError>> {
        Box::pin(async move {
            if matches!(parent, "." | "..") || parent.contains('\0') || parent.len() > 4096
                || cursor.is_some_and(|value| value.len() > 16384) {
                return Err(OfficialApiError::InvalidInput);
            }
            let path = if parent == "/" { "" } else { parent };
            let request = match cursor {
                Some(cursor) => self.client.post("https://api.dropboxapi.com/2/files/list_folder/continue")
                    .bearer_auth(self.access_token.as_str())
                    .json(&serde_json::json!({ "cursor": cursor })),
                None => self.client.post("https://api.dropboxapi.com/2/files/list_folder")
                    .bearer_auth(self.access_token.as_str())
                    .json(&serde_json::json!({
                        "path": path, "recursive": false, "include_deleted": false, "limit": 200,
                    })),
            };
            let response = request.send().await.map_err(|error| OfficialApiError::Transport(error.without_url()))?;
            parse_dropbox_page(read_json(response, self.access_token.as_str(), self.provider()).await?)
        })
    }

    fn media<'a>(
        &'a self,
        id: &'a str,
    ) -> BoxFuture<'a, Result<reqwest::Response, OfficialApiError>> {
        Box::pin(async move {
            validate_path(id)?;
            let response = self.client.post("https://content.dropboxapi.com/2/files/download")
                .bearer_auth(self.access_token.as_str())
                .header("Dropbox-API-Arg", json_header(&serde_json::json!({ "path": id }))?)
                .send().await.map_err(|error| OfficialApiError::Transport(error.without_url()))?;
            ensure_success(response, self.access_token.as_str(), self.provider()).await
        })
    }

    fn metadata<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<Value, OfficialApiError>> {
        Box::pin(async move {
            validate_path(id)?;
            let response = self.client.post("https://api.dropboxapi.com/2/files/get_metadata")
                .bearer_auth(self.access_token.as_str())
                .json(&serde_json::json!({ "path": id }))
                .send().await.map_err(|error| OfficialApiError::Transport(error.without_url()))?;
            read_json(response, self.access_token.as_str(), self.provider()).await
        })
    }

    fn delete_item<'a>(
        &'a self,
        id: &'a str,
        revision: &'a str,
    ) -> BoxFuture<'a, Result<(), OfficialApiError>> {
        Box::pin(async move {
            validate_path(id)?;
            if revision.is_empty() { return Err(OfficialApiError::InvalidInput); }
            let response = self.client.post("https://api.dropboxapi.com/2/files/delete_v2")
                .bearer_auth(self.access_token.as_str())
                .json(&serde_json::json!({ "path": id }))
                .send().await.map_err(|error| OfficialApiError::Transport(error.without_url()))?;
            read_json(response, self.access_token.as_str(), self.provider()).await?;
            Ok(())
        })
    }

    fn move_item<'a>(
        &'a self,
        id: &'a str,
        _from: &'a str,
        destination: &'a str,
    ) -> BoxFuture<'a, Result<(), OfficialApiError>> {
        Box::pin(async move {
            validate_path(id)?;
            validate_path(destination)?;
            let name = basename(id).ok_or(OfficialApiError::InvalidInput)?;
            let response = self.client.post("https://api.dropboxapi.com/2/files/move_v2")
                .bearer_auth(self.access_token.as_str())
                .json(&serde_json::json!({
                    "from_path": id, "to_path": join(destination, name),
                    "autorename": false, "allow_ownership_transfer": false,
                }))
                .send().await.map_err(|error| OfficialApiError::Transport(error.without_url()))?;
            read_json(response, self.access_token.as_str(), self.provider()).await?;
            Ok(())
        })
    }

    fn create_folder<'a>(
        &'a self,
        parent: &'a str,
        name: &'a str,
    ) -> BoxFuture<'a, Result<String, OfficialApiError>> {
        Box::pin(async move {
            validate_path(parent)?;
            validate_name(name)?;
            let response = self.client.post("https://api.dropboxapi.com/2/files/create_folder_v2")
                .bearer_auth(self.access_token.as_str())
                .json(&serde_json::json!({ "path": join(parent, name), "autorename": false }))
                .send().await.map_err(|error| OfficialApiError::Transport(error.without_url()))?;
            let value = read_json(response, self.access_token.as_str(), self.provider()).await?;
            value.pointer("/metadata/path_display").or_else(|| value.pointer("/metadata/path_lower"))
                .and_then(Value::as_str).map(str::to_owned).ok_or(OfficialApiError::InvalidResponse)
        })
    }

    fn create_folder_id<'a>(
        &'a self,
        parent: &'a str,
        name: &'a str,
    ) -> BoxFuture<'a, Result<String, OfficialApiError>> {
        self.create_folder(parent, name)
    }

    fn replace_content<'a>(
        &'a self,
        id: &'a str,
        expected: &'a str,
        bytes: Vec<u8>,
    ) -> BoxFuture<'a, Result<String, OfficialApiError>> {
        Box::pin(async move {
            validate_path(id)?;
            let revision = expected.strip_prefix("dropbox-rev:").unwrap_or(expected);
            if revision.is_empty() { return Err(OfficialApiError::InvalidInput); }
            let arg = json_header(&serde_json::json!({
                "path": id, "mode": { ".tag": "update", "update": revision },
                "autorename": false, "mute": false,
            }))?;
            let response = self.client.post("https://content.dropboxapi.com/2/files/upload")
                .bearer_auth(self.access_token.as_str())
                .header("Dropbox-API-Arg", arg)
                .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
                .header(reqwest::header::CONTENT_LENGTH, bytes.len())
                .body(bytes)
                .send().await.map_err(|error| OfficialApiError::Transport(error.without_url()))?;
            let value = read_json(response, self.access_token.as_str(), self.provider()).await?;
            value.get("rev").and_then(Value::as_str).map(|rev| format!("dropbox-rev:{rev}"))
                .ok_or(OfficialApiError::InvalidResponse)
        })
    }

    fn upload_new<'a>(
        &'a self,
        parent: &'a str,
        name: &'a str,
        file: std::fs::File,
        size: u64,
        progress: Option<std::sync::Arc<std::sync::atomic::AtomicU64>>,
    ) -> BoxFuture<'a, Result<String, OfficialApiError>> {
        Box::pin(async move {
            validate_path(parent)?;
            validate_name(name)?;
            let arg = json_header(&serde_json::json!({
                "path": join(parent, name), "mode": "add", "autorename": false, "mute": false,
            }))?;
            let response = self.client.post("https://content.dropboxapi.com/2/files/upload")
                .bearer_auth(self.access_token.as_str())
                .header("Dropbox-API-Arg", arg)
                .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
                .header(reqwest::header::CONTENT_LENGTH, size)
                .body(stream_body(file, progress))
                .send().await.map_err(|error| OfficialApiError::Transport(error.without_url()))?;
            let value = read_json(response, self.access_token.as_str(), self.provider()).await?;
            value.get("path_display").or_else(|| value.get("id")).and_then(Value::as_str)
                .map(str::to_owned).ok_or(OfficialApiError::InvalidResponse)
        })
    }

    fn replace_file_stream<'a>(
        &'a self,
        id: &'a str,
        file: std::fs::File,
        size: u64,
        expected: Option<&'a str>,
        progress: Option<std::sync::Arc<std::sync::atomic::AtomicU64>>,
    ) -> BoxFuture<'a, Result<(), OfficialApiError>> {
        Box::pin(async move {
            validate_path(id)?;
            let revision = expected.map(|value| value.strip_prefix("dropbox-rev:").unwrap_or(value));
            let mode = match revision {
                Some(revision) if !revision.is_empty() => serde_json::json!({ ".tag": "update", "update": revision }),
                Some(_) => return Err(OfficialApiError::InvalidInput),
                None => serde_json::json!("overwrite"),
            };
            let arg = json_header(&serde_json::json!({
                "path": id, "mode": mode, "autorename": false, "mute": false,
            }))?;
            let response = self.client.post("https://content.dropboxapi.com/2/files/upload")
                .bearer_auth(self.access_token.as_str())
                .header("Dropbox-API-Arg", arg)
                .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
                .header(reqwest::header::CONTENT_LENGTH, size)
                .body(stream_body(file, progress))
                .send().await.map_err(|error| OfficialApiError::Transport(error.without_url()))?;
            read_json(response, self.access_token.as_str(), self.provider()).await?;
            Ok(())
        })
    }
}

fn validate_path(path: &str) -> Result<(), OfficialApiError> {
    if path.is_empty() || path.len() > 4096 || matches!(path, "." | "..")
        || !path.starts_with('/') || path.contains('\0') {
        return Err(OfficialApiError::InvalidInput);
    }
    Ok(())
}

fn validate_name(name: &str) -> Result<(), OfficialApiError> {
    if name.trim().is_empty() || name.len() > 255 || name.contains(['/', '\\', '\0'])
        || matches!(name, "." | "..") {
        return Err(OfficialApiError::InvalidInput);
    }
    Ok(())
}

fn join(parent: &str, name: &str) -> String {
    format!("{}/{name}", parent.trim_end_matches('/'))
}

fn basename(path: &str) -> Option<&str> {
    path.rsplit('/').find(|part| !part.is_empty())
}

fn stream_body(
    file: std::fs::File,
    progress: Option<std::sync::Arc<std::sync::atomic::AtomicU64>>,
) -> reqwest::Body {
    use futures_util::StreamExt;
    let stream = tokio_util::io::ReaderStream::new(tokio::fs::File::from_std(file))
        .inspect(move |chunk| {
            if let (Some(counter), Ok(bytes)) = (&progress, chunk) {
                counter.fetch_add(bytes.len() as u64, std::sync::atomic::Ordering::Relaxed);
            }
        });
    reqwest::Body::wrap_stream(stream)
}

fn parse_dropbox_page(payload: Value) -> Result<OfficialPage, OfficialApiError> {
    let items = payload.get("entries").and_then(Value::as_array).ok_or(OfficialApiError::InvalidResponse)?;
    if items.len() > 200 { return Err(OfficialApiError::InvalidResponse); }
    let mut seen = std::collections::HashSet::new();
    let mut entries = Vec::with_capacity(items.len());
    for item in items {
        let is_dir = match item.get(".tag").and_then(Value::as_str).ok_or(OfficialApiError::InvalidResponse)? {
            "folder" => true,
            "file" => false,
            _ => return Err(OfficialApiError::InvalidResponse),
        };
        let path = item.get("path_display").or_else(|| item.get("path_lower"))
            .and_then(Value::as_str).filter(|value| !value.is_empty()).ok_or(OfficialApiError::InvalidResponse)?;
        let entry = OfficialEntry {
            id: path.to_owned(),
            name: string(item, "name")?,
            is_dir,
            size: if is_dir { 0 } else { item.get("size").and_then(Value::as_u64).ok_or(OfficialApiError::InvalidResponse)? },
            content_type: None,
            online_preview: false,
            revision: item.get("rev").and_then(Value::as_str).map(str::to_owned),
        };
        if !seen.insert(entry.id.clone()) || entry.name.contains(['/', '\\', '\0']) || matches!(entry.name.as_str(), "." | "..") {
            return Err(OfficialApiError::InvalidResponse);
        }
        entries.push(entry);
    }
    let has_more = payload.get("has_more").and_then(Value::as_bool).ok_or(OfficialApiError::InvalidResponse)?;
    let next = if has_more {
        Some(payload.get("cursor").and_then(Value::as_str).filter(|value| !value.is_empty())
            .ok_or(OfficialApiError::InvalidResponse)?.to_owned())
    } else { None };
    Ok(OfficialPage { entries, next, complete: !has_more })
}

fn string(value: &Value, key: &str) -> Result<String, OfficialApiError> {
    value.get(key).and_then(Value::as_str).filter(|value| !value.is_empty())
        .map(str::to_owned).ok_or(OfficialApiError::InvalidResponse)
}

/// Header values must be ASCII. Dropbox accepts JSON escapes, so non-ASCII
/// names become `\uXXXX` instead of raw UTF-8 bytes.
fn json_header(value: &Value) -> Result<String, OfficialApiError> {
    let text = serde_json::to_string(value).map_err(|_| OfficialApiError::InvalidInput)?;
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        if character.is_ascii() {
            escaped.push(character);
        } else {
            let mut buffer = [0u16; 2];
            for unit in character.encode_utf16(&mut buffer) {
                escaped.push_str(&format!("\\u{unit:04x}"));
            }
        }
    }
    Ok(escaped)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_entries_and_pagination() {
        let page = parse_dropbox_page(json!({
            "entries": [
                {".tag": "folder", "name": "docs", "path_display": "/docs"},
                {".tag": "file", "name": "a.txt", "path_display": "/a.txt", "size": 7, "rev": "r1"}
            ],
            "cursor": "next",
            "has_more": true
        })).expect("page");
        assert_eq!(page.entries.len(), 2);
        assert!(page.entries[0].is_dir);
        assert_eq!(page.entries[1].size, 7);
        assert_eq!(page.entries[1].revision.as_deref(), Some("r1"));
        assert_eq!(page.next.as_deref(), Some("next"));
        assert!(!page.complete);
    }

    #[test]
    fn rejects_deleted_duplicate_and_unsafe_names() {
        assert!(parse_dropbox_page(json!({
            "entries": [{".tag": "deleted", "name": "x", "path_display": "/x"}],
            "cursor": "", "has_more": false
        })).is_err());
        let duplicate = json!({".tag": "file", "name": "x", "path_display": "/x", "size": 1});
        assert!(parse_dropbox_page(json!({
            "entries": [duplicate.clone(), duplicate], "cursor": "", "has_more": false
        })).is_err());
        assert!(parse_dropbox_page(json!({
            "entries": [{".tag": "file", "name": "a/b", "path_display": "/a/b", "size": 1}],
            "cursor": "", "has_more": false
        })).is_err());
    }

    #[test]
    fn escapes_non_ascii_header_values() {
        let header = json_header(&json!({ "path": "/文档/文件.txt" })).expect("header");
        assert!(header.is_ascii());
        assert!(header.contains("\\u"));
        assert!(!header.contains("文档"));
    }

    #[test]
    fn joins_and_validates_dropbox_paths() {
        assert_eq!(join("/", "a.txt"), "/a.txt");
        assert_eq!(join("/docs", "a.txt"), "/docs/a.txt");
        assert_eq!(basename("/docs/a.txt"), Some("a.txt"));
        assert_eq!(basename("/"), None);
        assert!(validate_path("/docs").is_ok());
        for path in ["", "docs", "/a\0b"] { assert!(validate_path(path).is_err(), "{path}"); }
        assert!(validate_name("a.txt").is_ok());
        for name in ["", " ", "a/b", "a\\b", ".", ".."] { assert!(validate_name(name).is_err(), "{name}"); }
    }
}