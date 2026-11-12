//! Yandex Disk official API adapter.
use futures_util::future::BoxFuture;
use serde_json::Value;
use zeroize::Zeroizing;

use super::common::{ensure_success, read_json};
use super::traits::OfficialDrive;
use crate::cloud::oauth_types::OfficialProvider;
use crate::cloud::official_api_types::{OfficialApiError, OfficialEntry, OfficialPage};

const FIELDS: &str = "_embedded.items.name,_embedded.items.path,_embedded.items.type,_embedded.items.size,_embedded.items.resource_id,_embedded.items.md5,_embedded.total";

pub(crate) struct YandexDrive {
    pub(crate) client: reqwest::Client,
    pub(crate) access_token: Zeroizing<String>,
}

impl OfficialDrive for YandexDrive {
    fn provider(&self) -> OfficialProvider {
        OfficialProvider::YandexDisk
    }

    fn list<'a>(
        &'a self,
        parent: &'a str,
        cursor: Option<&'a str>,
    ) -> BoxFuture<'a, Result<OfficialPage, OfficialApiError>> {
        Box::pin(async move {
            if !parent.starts_with("disk:/") || parent.contains('\0') || parent.len() > 4096
                || cursor.is_some_and(|value| value.len() > 16384) {
                return Err(OfficialApiError::InvalidInput);
            }
            let offset = cursor.map(|value| value.parse::<u64>().map_err(|_| OfficialApiError::InvalidInput))
                .transpose()?.unwrap_or(0);
            let response = self.client.get("https://cloud-api.yandex.net/v1/disk/resources")
                .query(&[("path", parent), ("limit", "200"), ("offset", &offset.to_string()), ("fields", FIELDS)])
                .bearer_auth(self.access_token.as_str())
                .send().await.map_err(|error| OfficialApiError::Transport(error.without_url()))?;
            parse_yandex_page(read_json(response, self.access_token.as_str(), self.provider()).await?, cursor)
        })
    }

    fn media<'a>(
        &'a self,
        id: &'a str,
    ) -> BoxFuture<'a, Result<reqwest::Response, OfficialApiError>> {
        Box::pin(async move {
            if !id.starts_with("disk:/") || id.contains('\0') || id.len() > 4096 {
                return Err(OfficialApiError::InvalidInput);
            }
            let response = self.client.get("https://cloud-api.yandex.net/v1/disk/resources/download")
                .query(&[("path", id)])
                .bearer_auth(self.access_token.as_str())
                .send().await.map_err(|error| OfficialApiError::Transport(error.without_url()))?;
            let payload = read_json(response, self.access_token.as_str(), self.provider()).await?;
            let href = payload.get("href").and_then(Value::as_str).ok_or(OfficialApiError::InvalidResponse)?;
            let url = download_url(href)?;
            // Signed storage URLs must never receive the API bearer token.
            let response = self.client.get(url).send().await
                .map_err(|error| OfficialApiError::Transport(error.without_url()))?;
            ensure_success(response, self.access_token.as_str(), self.provider()).await
        })
    }
}

fn parse_yandex_page(payload: Value, cursor: Option<&str>) -> Result<OfficialPage, OfficialApiError> {
    let embedded = payload.get("_embedded").ok_or(OfficialApiError::InvalidResponse)?;
    let items = embedded.get("items").and_then(Value::as_array).ok_or(OfficialApiError::InvalidResponse)?;
    if items.len() > 200 { return Err(OfficialApiError::InvalidResponse); }
    let mut seen = std::collections::HashSet::new();
    let mut entries = Vec::with_capacity(items.len());
    for item in items {
        let is_dir = match item.get("type").and_then(Value::as_str).ok_or(OfficialApiError::InvalidResponse)? {
            "dir" => true,
            "file" => false,
            _ => return Err(OfficialApiError::InvalidResponse),
        };
        let entry = OfficialEntry {
            id: string(item, "path")?, name: string(item, "name")?, is_dir,
            size: if is_dir { 0 } else { item.get("size").and_then(Value::as_u64).ok_or(OfficialApiError::InvalidResponse)? },
            content_type: None,
            online_preview: false,
            revision: item.get("md5").and_then(Value::as_str).map(str::to_owned),
        };
        if !seen.insert(entry.id.clone()) || entry.name.contains(['/', '\\', '\0']) || matches!(entry.name.as_str(), "." | "..") {
            return Err(OfficialApiError::InvalidResponse);
        }
        entries.push(entry);
    }
    let offset = cursor.map(|value| value.parse::<u64>().map_err(|_| OfficialApiError::InvalidResponse))
        .transpose()?.unwrap_or(0);
    let total = embedded.get("total").and_then(Value::as_u64).unwrap_or(0);
    let next_offset = offset.checked_add(entries.len() as u64).ok_or(OfficialApiError::InvalidResponse)?;
    let next = if !entries.is_empty() && next_offset < total { Some(next_offset.to_string()) } else { None };
    let complete = next.is_none();
    Ok(OfficialPage { entries, next, complete })
}

fn string(value: &Value, key: &str) -> Result<String, OfficialApiError> {
    value.get(key).and_then(Value::as_str).filter(|value| !value.is_empty())
        .map(str::to_owned).ok_or(OfficialApiError::InvalidResponse)
}

fn download_url(location: &str) -> Result<url::Url, OfficialApiError> {
    if location.len() > 16384 { return Err(OfficialApiError::InvalidResponse); }
    let url = url::Url::parse(location).map_err(|_| OfficialApiError::InvalidResponse)?;
    let host = url.host_str().ok_or(OfficialApiError::InvalidResponse)?;
    if url.scheme() != "https" || url.port_or_known_default() != Some(443) || url.fragment().is_some()
        || !url.username().is_empty() || url.password().is_some()
        || !(host == "disk.yandex.com" || host == "disk.yandex.ru"
            || host.ends_with(".yandex.net") || host.ends_with(".yandex.ru") || host.ends_with(".yandex.com")) {
        return Err(OfficialApiError::InvalidResponse);
    }
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_entries_and_offset_pagination() {
        let page = parse_yandex_page(json!({
            "_embedded": {
                "total": 3,
                "items": [
                    {"name": "docs", "path": "disk:/docs", "type": "dir"},
                    {"name": "a.txt", "path": "disk:/a.txt", "type": "file", "size": 7, "md5": "m"}
                ]
            }
        }), None).expect("page");
        assert_eq!(page.entries.len(), 2);
        assert!(page.entries[0].is_dir);
        assert_eq!(page.entries[1].size, 7);
        assert_eq!(page.entries[1].revision.as_deref(), Some("m"));
        assert_eq!(page.next.as_deref(), Some("2"));
        assert!(!page.complete);
    }

    #[test]
    fn last_page_has_no_cursor_and_rejects_unknown_types() {
        let page = parse_yandex_page(json!({
            "_embedded": {"total": 1, "items": [{"name": "a.txt", "path": "disk:/a.txt", "type": "file", "size": 1}]}
        }), Some("0")).expect("page");
        assert!(page.next.is_none() && page.complete);
        assert!(parse_yandex_page(json!({
            "_embedded": {"total": 1, "items": [{"name": "x", "path": "disk:/x", "type": "symlink"}]}
        }), None).is_err());
        assert!(parse_yandex_page(json!({
            "_embedded": {"total": 1, "items": [{"name": "a/b", "path": "disk:/a/b", "type": "file", "size": 1}]}
        }), None).is_err());
    }

    #[test]
    fn download_urls_reject_exfiltration_targets() {
        assert!(download_url("https://downloader.disk.yandex.ru/disk/x").is_ok());
        for location in [
            "http://downloader.disk.yandex.ru/disk/x",
            "https://evil.invalid/x",
            "https://yandex.ru.evil.invalid/x",
            "https://user:pass@downloader.disk.yandex.ru/x",
            "https://downloader.disk.yandex.ru:8443/x",
        ] {
            assert!(download_url(location).is_err(), "{location}");
        }
    }
}