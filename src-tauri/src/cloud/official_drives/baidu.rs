//! Baidu Netdisk (xpan) official API adapter.
//!
//! # Limits
//! - Delete and move are not atomic. xpan has no conditional `filemanager`
//!   operation, so the revision only records the caller's confirmation.
//! - Overwrites use `rtype=3`; a concurrent change is overwritten.
//! - Entry identity is the absolute `path`. Renaming or moving an item
//!   invalidates previously issued IDs, so a stale tab must list again.
//! - Upload is three-step: `precreate`, per-4MiB `superfile2`, then `create`.
//!   `precreate.block_list` lists already stored parts; those are skipped.
//! - `export` is Google-only; `online_preview` is unavailable.
use std::io::{Read, Seek, SeekFrom, Write};

use futures_util::future::BoxFuture;
use md5::{Digest, Md5};
use serde_json::Value;
use zeroize::Zeroizing;

use super::common::{ensure_success, read_json};
use super::traits::OfficialDrive;
use crate::cloud::oauth_types::OfficialProvider;
use crate::cloud::official_api_types::{OfficialApiError, OfficialEntry, OfficialPage};

const API: &str = "https://pan.baidu.com/rest/2.0/xpan";
const UPLOAD: &str = "https://d.pcs.baidu.com/rest/2.0/pcs/superfile2";
const BLOCK: u64 = 4 * 1024 * 1024;
const PAGE: u64 = 200;

pub(crate) struct BaiduDrive {
    pub(crate) client: reqwest::Client,
    pub(crate) access_token: Zeroizing<String>,
}

impl OfficialDrive for BaiduDrive {
    fn provider(&self) -> OfficialProvider {
        OfficialProvider::BaiduNetdisk
    }

    fn list<'a>(
        &'a self,
        parent: &'a str,
        cursor: Option<&'a str>,
    ) -> BoxFuture<'a, Result<OfficialPage, OfficialApiError>> {
        Box::pin(async move {
            validate_path(parent)?;
            let start = cursor.map(|value| value.parse::<u64>().map_err(|_| OfficialApiError::InvalidInput))
                .transpose()?.unwrap_or(0);
            parse_page(self.list_page(parent, start).await?, start)
        })
    }

    fn media<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<reqwest::Response, OfficialApiError>> {
        Box::pin(async move {
            validate_path(id)?;
            let entry = self.entry(id).await?;
            let fs_id = entry.get("fs_id").and_then(Value::as_u64).ok_or(OfficialApiError::InvalidResponse)?;
            let response = self.client.get(format!("{API}/multimedia"))
                .query(&[("method", "filemetas"), ("access_token", self.access_token.as_str()),
                    ("fsids", &format!("[{fs_id}]")), ("dlink", "1")])
                .send().await.map_err(transport)?;
            let value = baidu_json(response, self.access_token.as_str()).await?;
            let dlink = value.pointer("/list/0/dlink").and_then(Value::as_str).ok_or(OfficialApiError::InvalidResponse)?;
            let mut url = download_url(dlink)?;
            url.query_pairs_mut().append_pair("access_token", self.access_token.as_str());
            let response = self.client.get(url).send().await.map_err(transport)?;
            ensure_success(response, self.access_token.as_str(), self.provider()).await
        })
    }

    fn metadata<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<Value, OfficialApiError>> {
        Box::pin(async move {
            validate_path(id)?;
            self.entry(id).await
        })
    }

    fn delete_item<'a>(&'a self, id: &'a str, revision: &'a str) -> BoxFuture<'a, Result<(), OfficialApiError>> {
        Box::pin(async move {
            validate_path(id)?;
            if revision.is_empty() { return Err(OfficialApiError::InvalidInput); }
            let filelist = serde_json::to_string(&[id]).map_err(|_| OfficialApiError::InvalidInput)?;
            let response = self.client.post(format!("{API}/file"))
                .query(&[("method", "filemanager"), ("access_token", self.access_token.as_str()), ("opera", "delete")])
                .form(&[("async", "0"), ("filelist", filelist.as_str())])
                .send().await.map_err(transport)?;
            baidu_json(response, self.access_token.as_str()).await?;
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
            let filelist = serde_json::to_string(&[id]).map_err(|_| OfficialApiError::InvalidInput)?;
            let response = self.client.post(format!("{API}/file"))
                .query(&[("method", "filemanager"), ("access_token", self.access_token.as_str()), ("opera", "move")])
                .form(&[("async", "0"), ("filelist", filelist.as_str()), ("destdir", destination)])
                .send().await.map_err(transport)?;
            baidu_json(response, self.access_token.as_str()).await?;
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
            let path = join(parent, name);
            let response = self.client.post(format!("{API}/file"))
                .query(&[("method", "create"), ("access_token", self.access_token.as_str())])
                .form(&[("path", path.as_str()), ("isdir", "1"), ("size", "0"), ("block_list", "[]")])
                .send().await.map_err(transport)?;
            let value = baidu_json(response, self.access_token.as_str()).await?;
            value.get("path").and_then(Value::as_str).filter(|value| !value.is_empty())
                .map(str::to_owned).ok_or(OfficialApiError::InvalidResponse)
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
            if expected.is_empty() { return Err(OfficialApiError::InvalidInput); }
            let mut file = tempfile::tempfile().map_err(|_| OfficialApiError::InvalidInput)?;
            file.write_all(&bytes).map_err(|_| OfficialApiError::InvalidInput)?;
            file.seek(SeekFrom::Start(0)).map_err(|_| OfficialApiError::InvalidInput)?;
            self.upload_file(id, file, bytes.len() as u64, None).await?;
            let mut hasher = Md5::new();
            hasher.update(&bytes);
            Ok(format!("baidu-md5:{}", hex::encode(hasher.finalize())))
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
            let path = join(parent, name);
            self.upload_file(&path, file, size, progress).await?;
            Ok(path)
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
            if expected.is_some_and(|value| value.is_empty()) { return Err(OfficialApiError::InvalidInput); }
            self.upload_file(id, file, size, progress).await?;
            Ok(())
        })
    }
}

impl BaiduDrive {
    async fn list_page(&self, parent: &str, start: u64) -> Result<Value, OfficialApiError> {
        let response = self.client.get(format!("{API}/file"))
            .query(&[("method", "list"), ("access_token", self.access_token.as_str()),
                ("dir", parent), ("start", &start.to_string()), ("limit", &PAGE.to_string())])
            .send().await.map_err(transport)?;
        baidu_json(response, self.access_token.as_str()).await
    }

    /// Find one entry by absolute path. `filemetas` needs `fs_id`, while list
    /// parents are paths, so resolution walks the parent directory.
    async fn entry(&self, path: &str) -> Result<Value, OfficialApiError> {
        let (parent, _) = split_parent(path)?;
        let mut start = 0u64;
        loop {
            let value = self.list_page(parent, start).await?;
            let items = value.get("list").and_then(Value::as_array).ok_or(OfficialApiError::InvalidResponse)?;
            if let Some(item) = items.iter().find(|item| item.get("path").and_then(Value::as_str) == Some(path)) {
                return Ok(item.clone());
            }
            if items.len() < PAGE as usize { return Err(OfficialApiError::InvalidInput); }
            start = start.checked_add(PAGE).ok_or(OfficialApiError::InvalidResponse)?;
        }
    }

    async fn upload_file(
        &self,
        path: &str,
        mut file: std::fs::File,
        size: u64,
        progress: Option<std::sync::Arc<std::sync::atomic::AtomicU64>>,
    ) -> Result<(), OfficialApiError> {
        let blocks = block_md5(&mut file, size)?;
        let block_list = serde_json::to_string(&blocks).map_err(|_| OfficialApiError::InvalidInput)?;
        let precreate = self.client.post(format!("{API}/file"))
            .query(&[("method", "precreate"), ("access_token", self.access_token.as_str())])
            .form(&[("path", path), ("size", &size.to_string()), ("isdir", "0"),
                ("autoinit", "1"), ("rtype", "3"), ("block_list", block_list.as_str())])
            .send().await.map_err(transport)?;
        let precreate = baidu_json(precreate, self.access_token.as_str()).await?;
        let uploadid = precreate.get("uploadid").and_then(Value::as_str)
            .filter(|value| !value.is_empty()).ok_or(OfficialApiError::InvalidResponse)?;
        let stored: std::collections::HashSet<u64> = precreate.get("block_list").and_then(Value::as_array)
            .map(|items| items.iter().filter_map(Value::as_u64).collect()).unwrap_or_default();
        for index in 0..blocks.len() as u64 {
            if stored.contains(&index) { continue; }
            let length = size.saturating_sub(index.saturating_mul(BLOCK)).min(BLOCK) as usize;
            file.seek(SeekFrom::Start(index.saturating_mul(BLOCK))).map_err(|_| OfficialApiError::InvalidInput)?;
            let mut bytes = vec![0u8; length];
            file.read_exact(&mut bytes).map_err(|_| OfficialApiError::InvalidInput)?;
            let part = reqwest::multipart::Part::bytes(bytes).file_name("file");
            let response = self.client.post(UPLOAD)
                .query(&[("method", "upload"), ("access_token", self.access_token.as_str()), ("type", "tmpfile"),
                    ("path", path), ("uploadid", uploadid), ("partseq", &index.to_string())])
                .multipart(reqwest::multipart::Form::new().part("file", part))
                .send().await.map_err(transport)?;
            baidu_json(response, self.access_token.as_str()).await?;
            if let Some(counter) = &progress {
                counter.fetch_add(length as u64, std::sync::atomic::Ordering::Relaxed);
            }
        }
        let create = self.client.post(format!("{API}/file"))
            .query(&[("method", "create"), ("access_token", self.access_token.as_str())])
            .form(&[("path", path), ("size", &size.to_string()), ("isdir", "0"),
                ("rtype", "3"), ("uploadid", uploadid), ("block_list", block_list.as_str())])
            .send().await.map_err(transport)?;
        baidu_json(create, self.access_token.as_str()).await?;
        Ok(())
    }
}

fn block_md5(file: &mut std::fs::File, size: u64) -> Result<Vec<String>, OfficialApiError> {
    file.seek(SeekFrom::Start(0)).map_err(|_| OfficialApiError::InvalidInput)?;
    let mut blocks = Vec::new();
    let mut remaining = size;
    let mut buffer = vec![0u8; BLOCK as usize];
    while remaining > 0 {
        let length = remaining.min(BLOCK) as usize;
        file.read_exact(&mut buffer[..length]).map_err(|_| OfficialApiError::InvalidInput)?;
        let mut hasher = Md5::new();
        hasher.update(&buffer[..length]);
        blocks.push(hex::encode(hasher.finalize()));
        remaining -= length as u64;
    }
    file.seek(SeekFrom::Start(0)).map_err(|_| OfficialApiError::InvalidInput)?;
    Ok(blocks)
}

async fn baidu_json(response: reqwest::Response, token: &str) -> Result<Value, OfficialApiError> {
    let value = read_json(response, token, OfficialProvider::BaiduNetdisk).await?;
    let errno = value.get("errno").and_then(Value::as_i64).unwrap_or(0);
    if errno != 0 {
        let message = value.get("errmsg").or_else(|| value.get("error_msg")).and_then(Value::as_str)
            .unwrap_or("百度网盘拒绝请求 / Baidu Netdisk rejected request");
        return Err(OfficialApiError::Provider {
            code: errno.to_string(),
            message: message.chars().take(2048).collect::<String>().replace(token, "[redacted]"),
        });
    }
    Ok(value)
}

fn parse_page(payload: Value, start: u64) -> Result<OfficialPage, OfficialApiError> {
    let items = payload.get("list").and_then(Value::as_array).ok_or(OfficialApiError::InvalidResponse)?;
    if items.len() > PAGE as usize { return Err(OfficialApiError::InvalidResponse); }
    let mut seen = std::collections::HashSet::new();
    let mut entries = Vec::with_capacity(items.len());
    for item in items {
        let entry = OfficialEntry {
            id: string(item, "path")?, name: string(item, "server_filename")?,
            is_dir: item.get("isdir").and_then(Value::as_u64).ok_or(OfficialApiError::InvalidResponse)? == 1,
            size: item.get("size").and_then(Value::as_u64).unwrap_or(0),
            content_type: None,
            online_preview: false,
            revision: item.get("md5").and_then(Value::as_str).filter(|value| !value.is_empty()).map(str::to_owned),
        };
        if !seen.insert(entry.id.clone()) || entry.name.contains(['/', '\\', '\0']) || matches!(entry.name.as_str(), "." | "..") {
            return Err(OfficialApiError::InvalidResponse);
        }
        entries.push(entry);
    }
    let next = if items.len() == PAGE as usize {
        Some(start.checked_add(PAGE).ok_or(OfficialApiError::InvalidResponse)?.to_string())
    } else { None };
    let complete = next.is_none();
    Ok(OfficialPage { entries, next, complete })
}

fn string(value: &Value, key: &str) -> Result<String, OfficialApiError> {
    value.get(key).and_then(Value::as_str).filter(|value| !value.is_empty())
        .map(str::to_owned).ok_or(OfficialApiError::InvalidResponse)
}

fn validate_path(path: &str) -> Result<(), OfficialApiError> {
    if path.is_empty() || path.len() > 4096 || !path.starts_with('/') || path.contains('\0')
        || path.split('/').any(|part| matches!(part, "." | "..")) {
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

fn split_parent(path: &str) -> Result<(&str, &str), OfficialApiError> {
    validate_path(path)?;
    let index = path.rfind('/').ok_or(OfficialApiError::InvalidInput)?;
    let parent = if index == 0 { "/" } else { &path[..index] };
    let name = &path[index + 1..];
    if name.is_empty() { return Err(OfficialApiError::InvalidInput); }
    Ok((parent, name))
}

fn download_url(location: &str) -> Result<url::Url, OfficialApiError> {
    if location.len() > 16384 { return Err(OfficialApiError::InvalidResponse); }
    let url = url::Url::parse(location).map_err(|_| OfficialApiError::InvalidResponse)?;
    let host = url.host_str().ok_or(OfficialApiError::InvalidResponse)?;
    if url.scheme() != "https" || url.port_or_known_default() != Some(443) || url.fragment().is_some()
        || !url.username().is_empty() || url.password().is_some()
        || !(host.ends_with(".baidu.com") || host.ends_with(".baidupcs.com")) {
        return Err(OfficialApiError::InvalidResponse);
    }
    Ok(url)
}

fn transport(error: reqwest::Error) -> OfficialApiError {
    OfficialApiError::Transport(error.without_url())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_pages_and_identity() {
        let page = parse_page(json!({
            "errno": 0,
            "list": [
                {"path": "/docs", "server_filename": "docs", "isdir": 1, "size": 0},
                {"path": "/docs/a.txt", "server_filename": "a.txt", "isdir": 0, "size": 7, "md5": "m"}
            ]
        }), 0).expect("page");
        assert_eq!(page.entries.len(), 2);
        assert!(page.entries[0].is_dir);
        assert_eq!(page.entries[1].revision.as_deref(), Some("m"));
        assert!(page.next.is_none() && page.complete);
    }

    #[test]
    fn rejects_duplicate_unsafe_names_and_boundary_pages() {
        let duplicate = json!({"path": "/x", "server_filename": "x", "isdir": 0, "size": 1});
        assert!(parse_page(json!({"list": [duplicate.clone(), duplicate]}), 0).is_err());
        assert!(parse_page(json!({"list": [{"path": "/a/b", "server_filename": "a/b", "isdir": 0, "size": 1}]}), 0).is_err());
        let items: Vec<_> = (0..200).map(|n| json!({"path": format!("/f{n}"), "server_filename": format!("f{n}"), "isdir": 0, "size": 0})).collect();
        let page = parse_page(json!({"list": items}), 200).expect("boundary");
        assert_eq!(page.next.as_deref(), Some("400"));
    }

    #[test]
    fn splits_paths_and_validates_names() {
        assert_eq!(split_parent("/a.txt").expect("root file"), ("/", "a.txt"));
        assert_eq!(split_parent("/docs/a.txt").expect("nested"), ("/docs", "a.txt"));
        assert!(split_parent("/").is_err());
        assert!(validate_path("/docs").is_ok());
        assert!(validate_path("docs").is_err());
        assert!(validate_name("a.txt").is_ok());
        for name in ["", "a/b", ".."] { assert!(validate_name(name).is_err(), "{name}"); }
    }

    #[test]
    fn block_hashes_cover_empty_single_and_multi_part() {
        let mut empty = tempfile::tempfile().expect("file");
        assert!(block_md5(&mut empty, 0).expect("empty").is_empty());
        let mut one = tempfile::tempfile().expect("file");
        one.write_all(b"abc").expect("write");
        let blocks = block_md5(&mut one, 3).expect("single");
        assert_eq!(blocks, vec!["900150983cd24fb0d6963f7d28e17f72".to_string()]);
        let mut multi = tempfile::tempfile().expect("file");
        multi.write_all(&vec![0u8; BLOCK as usize + 1]).expect("write");
        assert_eq!(block_md5(&mut multi, BLOCK + 1).expect("multi").len(), 2);
    }

    #[test]
    fn download_urls_reject_exfiltration_targets() {
        assert!(download_url("https://d.pcs.baidu.com/file/x").is_ok());
        for location in ["http://d.pcs.baidu.com/file/x", "https://evil.invalid/x", "https://baidu.com.evil.invalid/x"] {
            assert!(download_url(location).is_err(), "{location}");
        }
    }
}