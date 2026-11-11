//! Official listing adapters. Tokens are never placed in user-visible errors.
use super::{oauth_types::OfficialProvider, official_api_types::*};
use serde_json::Value;
use zeroize::Zeroizing;

pub use super::official_api_types::OfficialApi;

impl OfficialApi {
    pub fn new(provider: OfficialProvider, access_token: Zeroizing<String>) -> Result<Self, OfficialApiError> {
        if access_token.is_empty() { return Err(OfficialApiError::InvalidInput); }
        let client = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(30)).build().map_err(OfficialApiError::Transport)?;
        Ok(Self { #[cfg(test)] fixture: None, client, provider, access_token })
    }

    pub(crate) fn item_url(&self, id: &str) -> Result<url::Url, OfficialApiError> {
        if id.is_empty() || id.len() > 4096 || matches!(id, "." | "..") || id.contains(['/', '\\', '\0']) { return Err(OfficialApiError::InvalidInput); }
        let base = match self.provider {
            OfficialProvider::GoogleDrive => "https://www.googleapis.com/drive/v3/files/",
            OfficialProvider::OneDrive => "https://graph.microsoft.com/v1.0/me/drive/items/",
            OfficialProvider::BaiduNetdisk => return Err(OfficialApiError::InvalidInput),
        };
        let mut url = url::Url::parse(base).map_err(|_| OfficialApiError::InvalidInput)?;
        url.path_segments_mut().map_err(|_| OfficialApiError::InvalidInput)?.pop_if_empty().push(id);
        Ok(url)
    }

    async fn send(&self, request: reqwest::RequestBuilder) -> Result<reqwest::Response, OfficialApiError> {
        #[cfg(test)]
        if let Some(endpoint) = &self.fixture {
            let mut request = request.build().map_err(OfficialApiError::Transport)?;
            let mut url = url::Url::parse(endpoint).map_err(|_| OfficialApiError::InvalidInput)?;
            url.set_path(request.url().path()); url.set_query(request.url().query());
            *request.url_mut() = url;
            return self.client.execute(request).await.map_err(|e| OfficialApiError::Transport(e.without_url()));
        }
        request.send().await.map_err(|e| OfficialApiError::Transport(e.without_url()))
    }

    async fn mutation_response(&self, request: reqwest::RequestBuilder) -> Result<serde_json::Value, OfficialApiError> {
        let mut response = self.send(request).await?;
        let status = response.status();
        let etag = response.headers().get(reqwest::header::ETAG).and_then(|value| value.to_str().ok()).map(str::to_owned);
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|e| OfficialApiError::Transport(e.without_url()))? {
            if bytes.len().saturating_add(chunk.len()) > 1024 * 1024 { return Err(OfficialApiError::InvalidResponse); }
            bytes.extend_from_slice(&chunk);
        }
        let mut value: Value = if bytes.is_empty() { Value::Null } else { serde_json::from_slice(&bytes).map_err(|_| OfficialApiError::InvalidResponse)? };
        if !status.is_success() {
            let reason = value.pointer("/error/message").and_then(Value::as_str).unwrap_or("Drive request rejected");
            return Err(OfficialApiError::Http { status: status.as_u16(), message: reason.chars().take(2048).collect::<String>().replace(self.access_token.as_str(), "[redacted]") });
        }
        if let (Some(etag), Some(object)) = (etag, value.as_object_mut()) { object.insert("eTag".into(), etag.into()); }
        Ok(value)
    }

    pub(crate) async fn metadata(&self, id: &str) -> Result<Value, OfficialApiError> {
        let mut request = self.client.get(self.item_url(id)?).bearer_auth(self.access_token.as_str());
        request = match self.provider {
            OfficialProvider::GoogleDrive => request.query(&[("fields", "id,name,version,trashed,webViewLink"), ("supportsAllDrives", "true")]),
            _ => request.query(&[("$select", "id,name,eTag,webUrl")]),
        };
        self.mutation_response(request).await
    }

    pub(crate) async fn replace_content(&self, id: &str, expected: &str, bytes: Vec<u8>) -> Result<String, OfficialApiError> {
        if expected.is_empty() { return Err(OfficialApiError::InvalidInput); }
        let mut url = self.item_url(id)?;
        let request = match self.provider {
            OfficialProvider::GoogleDrive => {
                url.set_path(&url.path().replacen("/drive/v3/", "/upload/drive/v3/", 1));
                url.query_pairs_mut().append_pair("uploadType", "media").append_pair("supportsAllDrives", "true");
                self.client.patch(url).query(&[("fields", "id,version")])
            }
            OfficialProvider::OneDrive => {
                url.path_segments_mut().map_err(|_| OfficialApiError::InvalidInput)?.push("content");
                self.client.put(url)
            }
            _ => return Err(OfficialApiError::InvalidInput),
        };
        let request = if let Some(version) = expected.strip_prefix("google-version:") {
            let current = self.metadata(id).await?;
            if self.provider != OfficialProvider::GoogleDrive || current.get("version").and_then(Value::as_str) != Some(version) { return Err(OfficialApiError::Http { status: 412, message: "File changed; confirm again".into() }); }
            request
        } else { request.header(reqwest::header::IF_MATCH, expected) };
        let result = self.mutation_response(request.bearer_auth(self.access_token.as_str()).header(reqwest::header::CONTENT_TYPE, "application/octet-stream").body(bytes)).await?;
        if self.provider == OfficialProvider::GoogleDrive {
            return result.get("version").and_then(Value::as_str).map(|version| format!("google-version:{version}")).ok_or(OfficialApiError::InvalidResponse);
        }
        Ok(result.get("eTag").and_then(Value::as_str).unwrap_or_default().into())
    }

    pub(crate) async fn move_item(&self, id: &str, from: &str, destination: &str) -> Result<(), OfficialApiError> {
        let url = self.item_url(id)?;
        self.item_url(destination)?;
        let request = match self.provider {
            OfficialProvider::GoogleDrive => self.client.patch(url).query(&[("addParents", destination), ("removeParents", from), ("supportsAllDrives", "true")]).json(&serde_json::json!({})),
            OfficialProvider::OneDrive => self.client.patch(url).json(&serde_json::json!({"parentReference": {"id": destination}, "@microsoft.graph.conflictBehavior": "fail"})),
            _ => return Err(OfficialApiError::InvalidInput),
        };
        self.mutation_response(request.bearer_auth(self.access_token.as_str())).await?;
        Ok(())
    }

    pub(crate) async fn delete_item(&self, id: &str, revision: &str) -> Result<(), OfficialApiError> {
        let url = self.item_url(id)?;
        // Google trash is recoverable and includes folder descendants. Graph DELETE
        // likewise moves personal drive items to the recycle bin.
        let request = if self.provider == OfficialProvider::GoogleDrive {
            self.client.patch(url).query(&[("supportsAllDrives", "true")]).header(reqwest::header::CONTENT_TYPE, "application/json").body(r#"{"trashed":true}"#)
        } else { self.client.delete(url) };
        if revision.is_empty() { return Err(OfficialApiError::InvalidInput); }
        let request = if let Some(version) = revision.strip_prefix("google-version:") {
            if self.provider != OfficialProvider::GoogleDrive { return Err(OfficialApiError::InvalidInput); }
            let current = self.metadata(id).await?;
            if current.get("version").and_then(Value::as_str) != Some(version) {
                return Err(OfficialApiError::Http { status: 412, message: "文件版本已变化，请重新确认 / File changed; confirm again".into() });
            }
            request
        } else { request.header(reqwest::header::IF_MATCH, revision) };
        self.mutation_response(request.bearer_auth(self.access_token.as_str())).await?;
        Ok(())
    }

    fn upload_request(&self, parent: &str, name: &str, file: std::fs::File, size: u64, progress: Option<std::sync::Arc<std::sync::atomic::AtomicU64>>) -> Result<reqwest::RequestBuilder, OfficialApiError> {
        if parent.is_empty() || parent.contains(['/', '\\', '\0']) || name.trim().is_empty() || name.contains(['/', '\\', '\0']) || matches!(name, "." | "..") { return Err(OfficialApiError::InvalidInput); }
        if self.provider != OfficialProvider::GoogleDrive { return Err(OfficialApiError::InvalidInput); }
        let metadata = serde_json::to_vec(&serde_json::json!({"name":name,"parents":[parent]})).map_err(|_| OfficialApiError::InvalidInput)?;
        use futures_util::StreamExt;
        let boundary = crate::local_upload::token().map_err(|_| OfficialApiError::InvalidInput)?;
        let header = format!("--{boundary}\r\nContent-Type: application/json; charset=UTF-8\r\n\r\n{}\r\n--{boundary}\r\nContent-Type: application/octet-stream\r\n\r\n", String::from_utf8_lossy(&metadata));
        let tail = format!("\r\n--{boundary}--\r\n");
        let length = (header.len() as u64).checked_add(size).and_then(|n| n.checked_add(tail.len() as u64)).ok_or(OfficialApiError::InvalidInput)?;
        let stream = futures_util::stream::once(async move { Ok::<_, std::io::Error>(bytes::Bytes::from(header)) })
            .chain(tokio_util::io::ReaderStream::new(tokio::fs::File::from_std(file)).inspect(move |chunk| {
                if let (Some(counter), Ok(bytes)) = (&progress, chunk) { counter.fetch_add(bytes.len() as u64, std::sync::atomic::Ordering::Relaxed); }
            }))
            .chain(futures_util::stream::once(async move { Ok::<_, std::io::Error>(bytes::Bytes::from(tail)) }));
        Ok(self.client.post("https://www.googleapis.com/upload/drive/v3/files").query(&[("uploadType", "multipart"), ("supportsAllDrives", "true")]).bearer_auth(self.access_token.as_str()).header(reqwest::header::CONTENT_TYPE, format!("multipart/related; boundary={boundary}")).header(reqwest::header::CONTENT_LENGTH, length).body(reqwest::Body::wrap_stream(stream)))
    }

    #[cfg(test)]
    pub(crate) async fn upload_new(&self, parent: &str, name: &str, file: std::fs::File, size: u64) -> Result<(), OfficialApiError> {
        self.mutation_response(self.upload_request(parent, name, file, size, None)?).await?;
        Ok(())
    }

    pub(crate) async fn upload_new_progress(&self, parent: &str, name: &str, file: std::fs::File, size: u64, progress: std::sync::Arc<std::sync::atomic::AtomicU64>) -> Result<(), OfficialApiError> {
        self.mutation_response(self.upload_request(parent, name, file, size, Some(progress))?).await?;
        Ok(())
    }

    pub(crate) async fn replace_file_stream(&self, id: &str, file: std::fs::File, size: u64, expected: Option<&str>) -> Result<(), OfficialApiError> {
        self.replace_file_stream_progress(id, file, size, expected, None).await
    }

    pub(crate) async fn replace_file_stream_progress(&self, id: &str, file: std::fs::File, size: u64, expected: Option<&str>, progress: Option<std::sync::Arc<std::sync::atomic::AtomicU64>>) -> Result<(), OfficialApiError> {
        use futures_util::StreamExt;
        if self.provider != OfficialProvider::GoogleDrive { return Err(OfficialApiError::InvalidInput); }
        if let Some(expected) = expected {
            if self.metadata(id).await?.get("version").and_then(Value::as_str) != Some(expected) { return Err(OfficialApiError::Http { status: 412, message: "File changed; confirm again".into() }); }
        }
        let mut url = self.item_url(id)?;
        url.set_path(&url.path().replacen("/drive/v3/", "/upload/drive/v3/", 1));
        let request = self.client.patch(url).query(&[("uploadType", "media"), ("supportsAllDrives", "true")])
            .bearer_auth(self.access_token.as_str()).header(reqwest::header::CONTENT_TYPE, "application/octet-stream").header(reqwest::header::CONTENT_LENGTH, size)
            .body(reqwest::Body::wrap_stream(tokio_util::io::ReaderStream::new(tokio::fs::File::from_std(file)).inspect(move |chunk| {
                if let (Some(counter), Ok(bytes)) = (&progress, chunk) { counter.fetch_add(bytes.len() as u64, std::sync::atomic::Ordering::Relaxed); }
            })));
        self.mutation_response(request).await?;
        Ok(())
    }

    pub(crate) async fn upload_object(&self, parent: &str, name: &str, file: std::fs::File, size: u64) -> Result<String, OfficialApiError> {
        let value = self.mutation_response(self.upload_request(parent, name, file, size, None)?).await?;
        value.get("id").and_then(Value::as_str).map(str::to_owned).ok_or(OfficialApiError::InvalidResponse)
    }

    pub(crate) async fn create_folder_id(&self, parent: &str, name: &str) -> Result<String, OfficialApiError> {
        if self.provider != OfficialProvider::GoogleDrive { return Err(OfficialApiError::InvalidInput); }
        let value = self.mutation_response(self.client.post("https://www.googleapis.com/drive/v3/files").query(&[("supportsAllDrives", "true")]).bearer_auth(self.access_token.as_str()).json(&serde_json::json!({"name":name,"mimeType":"application/vnd.google-apps.folder","parents":[parent]}))).await?;
        value.get("id").and_then(Value::as_str).map(str::to_owned).ok_or(OfficialApiError::InvalidResponse)
    }

    pub(crate) async fn create_folder(&self, parent: &str, name: &str) -> Result<String, OfficialApiError> {
        if name.trim().is_empty() || name.contains(['/', '\\', '\0']) || matches!(name, "." | "..") { return Err(OfficialApiError::InvalidInput); }
        let request = match self.provider {
            OfficialProvider::GoogleDrive => self.client.post("https://www.googleapis.com/drive/v3/files")
                .query(&[("supportsAllDrives", "true")]).json(&serde_json::json!({"name":name,"mimeType":"application/vnd.google-apps.folder","parents":[parent]})),
            OfficialProvider::OneDrive => {
                let mut url = self.item_url(parent)?;
                url.path_segments_mut().map_err(|_| OfficialApiError::InvalidInput)?.push("children");
                self.client.post(url).json(&serde_json::json!({"name":name,"folder":{},"@microsoft.graph.conflictBehavior":"fail"}))
            }
            _ => return Err(OfficialApiError::InvalidInput),
        };
        let value = self.mutation_response(request.bearer_auth(self.access_token.as_str())).await?;
        value.get("id").and_then(Value::as_str).filter(|id| !id.is_empty()).map(str::to_owned).ok_or(OfficialApiError::InvalidResponse)
    }

    pub(crate) fn export_format(content_type: Option<&str>) -> Result<Option<(&'static str, &'static str)>, OfficialApiError> {
        Ok(match content_type {
            Some("application/vnd.google-apps.document") => Some(("application/vnd.openxmlformats-officedocument.wordprocessingml.document", ".docx")),
            Some("application/vnd.google-apps.spreadsheet") => Some(("application/vnd.openxmlformats-officedocument.spreadsheetml.sheet", ".xlsx")),
            Some("application/vnd.google-apps.presentation") => Some(("application/vnd.openxmlformats-officedocument.presentationml.presentation", ".pptx")),
            Some("application/vnd.google-apps.drawing") => Some(("application/pdf", ".pdf")),
            Some(kind) if kind.starts_with("application/vnd.google-apps.") && kind != "application/vnd.google-apps.folder" => return Err(OfficialApiError::Provider { code: "EXPORT_UNAVAILABLE".into(), message: "此 Google 原生文件类型暂不支持导出 / Export unavailable for this Google file type".into() }),
            _ => None,
        })
    }

    fn export_request(&self, id: &str, mime: &str) -> Result<reqwest::RequestBuilder, OfficialApiError> {
        if self.provider != OfficialProvider::GoogleDrive || !matches!(mime, "application/vnd.openxmlformats-officedocument.wordprocessingml.document" | "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet" | "application/vnd.openxmlformats-officedocument.presentationml.presentation" | "application/pdf") { return Err(OfficialApiError::InvalidInput); }
        let mut url = self.item_url(id)?;
        url.path_segments_mut().map_err(|_| OfficialApiError::InvalidInput)?.push("export");
        Ok(self.client.get(url).bearer_auth(self.access_token.as_str()).query(&[("mimeType", mime)]))
    }

    pub(crate) async fn export(&self, id: &str, mime: &str) -> Result<reqwest::Response, OfficialApiError> {
        self.media_request(self.export_request(id, mime)?).await
    }

    pub async fn media(&self, id: &str) -> Result<reqwest::Response, OfficialApiError> {
        if id.is_empty() || id.len() > 4096 || matches!(id, "." | "..") || id.contains(['/', '\\', '\0']) {
            return Err(OfficialApiError::InvalidInput);
        }
        let (base, tail) = match self.provider {
            OfficialProvider::GoogleDrive => ("https://www.googleapis.com/drive/v3/files/", None),
            OfficialProvider::OneDrive => ("https://graph.microsoft.com/v1.0/me/drive/items/", Some("content")),
            OfficialProvider::BaiduNetdisk => return Err(OfficialApiError::InvalidInput),
        };
        let mut url = url::Url::parse(base).map_err(|_| OfficialApiError::InvalidInput)?;
        {
            let mut segments = url.path_segments_mut().map_err(|_| OfficialApiError::InvalidInput)?;
            segments.pop_if_empty().push(id);
            if let Some(tail) = tail { segments.push(tail); }
        }
        if self.provider == OfficialProvider::GoogleDrive { url.query_pairs_mut().append_pair("alt", "media"); }
        self.media_request(self.client.get(url).bearer_auth(self.access_token.as_str())).await
    }

    async fn media_request(&self, request: reqwest::RequestBuilder) -> Result<reqwest::Response, OfficialApiError> {
        let mut response = self.send(request).await?;
        if self.provider == OfficialProvider::OneDrive && response.status() == reqwest::StatusCode::FOUND {
            let location = response.headers().get(reqwest::header::LOCATION).and_then(|value| value.to_str().ok()).ok_or(OfficialApiError::InvalidResponse)?;
            let url = content_redirect(location)?;
            // Signed content URLs must not receive the Graph bearer token.
            response = self.client.get(url).send().await.map_err(|error| OfficialApiError::Transport(error.without_url()))?;
        }
        if response.status() != reqwest::StatusCode::OK {
            let status = response.status().as_u16();
            let mut bytes = Zeroizing::new(Vec::new());
            const LIMIT: usize = 64 * 1024;
            while bytes.len() < LIMIT {
                let chunk = response.chunk().await.map_err(|error| OfficialApiError::Transport(error.without_url()))?;
                let Some(chunk) = chunk else { break; };
                let remaining = LIMIT - bytes.len();
                bytes.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
            }
            let value = serde_json::from_slice::<Value>(&bytes).ok();
            let raw = String::from_utf8_lossy(&bytes);
            let reason = value.as_ref().and_then(|v| v.pointer("/error/message")).and_then(Value::as_str)
                .unwrap_or(raw.as_ref());
            let reason_code = value.as_ref().and_then(|v| v.pointer("/error/errors/0/reason").or_else(|| v.pointer("/error/code"))).and_then(Value::as_str);
            // Redact before truncating so a token cannot leak at the cut boundary.
            let message = match reason_code {
                Some(code) => format!("{reason} [{code}]"),
                None if reason.trim().is_empty() => "File content request rejected (empty response)".into(),
                None => reason.into(),
            }.replace(self.access_token.as_str(), "[redacted]").chars().take(2048).collect();
            return Err(OfficialApiError::Http { status, message });
        }
        Ok(response)
    }

    pub async fn list(&self, parent: &str, cursor: Option<&str>) -> Result<OfficialPage, OfficialApiError> {
        if matches!(parent, "." | "..") || parent.contains('\0') {
            return Err(OfficialApiError::InvalidInput);
        }
        if parent.is_empty() || parent.len() > 4096 || cursor.is_some_and(|value| value.len() > 16384) {
            return Err(OfficialApiError::InvalidInput);
        }
        let request = self.list_request(parent, cursor)?;
        self.execute_list(request, cursor).await
    }

    fn list_request(&self, parent: &str, cursor: Option<&str>) -> Result<reqwest::RequestBuilder, OfficialApiError> {
        let request = match self.provider {
            OfficialProvider::GoogleDrive => {
                let parent = parent.replace('\\', "\\\\").replace('\'', "\\'");
                let mut request = self.client.get("https://www.googleapis.com/drive/v3/files")
                    .bearer_auth(self.access_token.as_str()).query(&[
                        ("q", format!("'{parent}' in parents and trashed = false")),
                        ("fields", "files(id,name,mimeType,size,version,webViewLink),nextPageToken,incompleteSearch".into()),
                        ("pageSize", "200".into()), ("supportsAllDrives", "true".into()),
                        ("includeItemsFromAllDrives", "true".into()),
                    ]);
                if let Some(cursor) = cursor { request = request.query(&[("pageToken", cursor)]); }
                request
            }
            OfficialProvider::OneDrive => {
                let url = if parent == "root" {
                    url::Url::parse("https://graph.microsoft.com/v1.0/me/drive/root/children").map_err(|_| OfficialApiError::InvalidInput)?
                } else {
                    let mut url = url::Url::parse("https://graph.microsoft.com/v1.0/me/drive/items/").map_err(|_| OfficialApiError::InvalidInput)?;
                    url.path_segments_mut().map_err(|_| OfficialApiError::InvalidInput)?.pop_if_empty().push(parent).push("children");
                    url
                };
                let mut request = self.client.get(url).bearer_auth(self.access_token.as_str())
                    .query(&[("$top", "200"), ("$select", "id,name,size,folder,eTag,webUrl")]);
                if let Some(cursor) = cursor { request = request.query(&[("$skiptoken", cursor)]); }
                request
            }
            OfficialProvider::BaiduNetdisk => {
                if !parent.starts_with('/') { return Err(OfficialApiError::InvalidInput); }
                let start = cursor.unwrap_or("0").parse::<u64>().map_err(|_| OfficialApiError::InvalidInput)?;
                self.client.get("https://pan.baidu.com/rest/2.0/xpan/file")
                    .query(&[("method", "list"), ("access_token", self.access_token.as_str()),
                        ("dir", parent), ("start", &start.to_string()), ("limit", "200")])
            }
        };
        Ok(request)
    }

    #[cfg(test)]
    pub(crate) async fn list_fixture(&self, endpoint: &str, cursor: Option<&str>) -> Result<OfficialPage, OfficialApiError> {
        self.execute_list(self.client.get(endpoint), cursor).await
    }

    async fn execute_list(&self, request: reqwest::RequestBuilder, cursor: Option<&str>) -> Result<OfficialPage, OfficialApiError> {
        let mut response = self.send(request).await?;
        let status = response.status();
        let mut bytes = Zeroizing::new(Vec::new());
        while let Some(chunk) = response.chunk().await.map_err(|error| OfficialApiError::Transport(error.without_url()))? {
            if bytes.len().saturating_add(chunk.len()) > 4 * 1024 * 1024 { return Err(OfficialApiError::InvalidResponse); }
            bytes.extend_from_slice(&chunk);
        }
        if !status.is_success() {
            let payload: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
            let reason = payload.pointer("/error/message").or_else(|| payload.get("errmsg"))
                .and_then(Value::as_str).unwrap_or("Drive request rejected");
            let message = reason.chars().take(2048).collect::<String>().replace(self.access_token.as_str(), "[redacted]");
            return Err(OfficialApiError::Http { status: status.as_u16(), message });
        }
        let mut payload: Value = serde_json::from_slice(&bytes).map_err(|_| OfficialApiError::InvalidResponse)?;
        for key in ["errmsg", "error_msg"] {
            if let Some(value) = payload.get(key).and_then(Value::as_str).map(str::to_owned) {
                payload[key] = value.replace(self.access_token.as_str(), "[redacted]").into();
            }
        }
        parse_page(self.provider, payload, cursor)
    }
}

fn content_redirect(location: &str) -> Result<url::Url, OfficialApiError> {
    if location.len() > 16384 { return Err(OfficialApiError::InvalidResponse); }
    let url = url::Url::parse(location).map_err(|_| OfficialApiError::InvalidResponse)?;
    let host = url.host_str().ok_or(OfficialApiError::InvalidResponse)?;
    if url.scheme() != "https" || url.port_or_known_default() != Some(443) || url.fragment().is_some()
        || !url.username().is_empty() || url.password().is_some()
        || !(host.ends_with(".sharepoint.com") || host.ends_with(".files.1drv.com")
            || host.ends_with(".storage.live.com") || host.ends_with(".onedrive.com")) {
        return Err(OfficialApiError::InvalidResponse);
    }
    Ok(url)
}

fn string(value: &Value, key: &str) -> Result<String, OfficialApiError> {
    value.get(key).and_then(Value::as_str).filter(|value| !value.is_empty())
        .map(str::to_owned).ok_or(OfficialApiError::InvalidResponse)
}

fn parse_page(provider: OfficialProvider, payload: Value, cursor: Option<&str>) -> Result<OfficialPage, OfficialApiError> {
    let key = match provider { OfficialProvider::GoogleDrive => "files", OfficialProvider::OneDrive => "value", OfficialProvider::BaiduNetdisk => "list" };
    if provider == OfficialProvider::BaiduNetdisk && payload.get("errno").and_then(Value::as_i64) != Some(0) {
        return Err(OfficialApiError::Provider {
            code: payload.get("errno").map(Value::to_string).unwrap_or_default(),
            message: payload.get("errmsg").or_else(|| payload.get("error_msg")).and_then(Value::as_str)
                .filter(|value| value.len() <= 2048).unwrap_or("百度网盘拒绝请求 / Baidu Netdisk rejected request").to_owned(),
        });
    }
    let items = payload.get(key).and_then(Value::as_array).ok_or(OfficialApiError::InvalidResponse)?;
    if items.len() > 200 { return Err(OfficialApiError::InvalidResponse); }
    let mut seen = std::collections::HashSet::new();
    let mut entries = Vec::with_capacity(items.len());
    for item in items {
        let entry = match provider {
            OfficialProvider::GoogleDrive => OfficialEntry {
                id: string(item, "id")?, name: string(item, "name")?,
                content_type: item.get("mimeType").and_then(Value::as_str).map(str::to_owned),
                is_dir: string(item, "mimeType")? == "application/vnd.google-apps.folder",
                size: item.get("size").and_then(Value::as_str).map(str::parse).transpose().map_err(|_| OfficialApiError::InvalidResponse)?.unwrap_or(0),
                online_preview: item.get("webViewLink").and_then(Value::as_str).is_some_and(|s| s.starts_with("https://")),
                revision: item.get("version").and_then(Value::as_str).map(str::to_owned),
            },
            OfficialProvider::OneDrive => OfficialEntry {
                id: string(item, "id")?, name: string(item, "name")?,
                is_dir: item.get("folder").is_some(), size: item.get("size").and_then(Value::as_u64).ok_or(OfficialApiError::InvalidResponse)?,
                content_type: None,
                online_preview: item.get("webUrl").and_then(Value::as_str).is_some_and(|s| s.starts_with("https://")),
                revision: item.get("eTag").and_then(Value::as_str).map(str::to_owned),
            },
            OfficialProvider::BaiduNetdisk => OfficialEntry {
                id: string(item, "path")?, name: string(item, "server_filename")?,
                is_dir: item.get("isdir").and_then(Value::as_u64).ok_or(OfficialApiError::InvalidResponse)? == 1,
                size: item.get("size").and_then(Value::as_u64).ok_or(OfficialApiError::InvalidResponse)?,
                content_type: None,
                online_preview: false,
                revision: item.get("md5").and_then(Value::as_str).map(str::to_owned),
            },
        };
        if !seen.insert(entry.id.clone()) || entry.name.contains(['/', '\\', '\0']) || matches!(entry.name.as_str(), "." | "..") {
            return Err(OfficialApiError::InvalidResponse);
        }
        entries.push(entry);
    }
    let next = match provider {
        OfficialProvider::GoogleDrive => payload.get("nextPageToken").and_then(Value::as_str).map(str::to_owned),
        OfficialProvider::OneDrive => match payload.get("@odata.nextLink") {
            None => None,
            Some(value) => {
                let url = url::Url::parse(value.as_str().ok_or(OfficialApiError::InvalidResponse)?).map_err(|_| OfficialApiError::InvalidResponse)?;
                if url.scheme() != "https" || url.host_str() != Some("graph.microsoft.com") || url.port_or_known_default() != Some(443)
                    || !url.path().starts_with("/v1.0/") || url.fragment().is_some() || !url.username().is_empty() || url.password().is_some() {
                    return Err(OfficialApiError::InvalidResponse);
                }
                Some(url.query_pairs().find(|(key, _)| key == "$skiptoken").ok_or(OfficialApiError::InvalidResponse)?.1.into_owned())
            }
        },
        OfficialProvider::BaiduNetdisk => if entries.len() == 200 {
            Some(cursor.unwrap_or("0").parse::<u64>().map_err(|_| OfficialApiError::InvalidInput)?.checked_add(200).ok_or(OfficialApiError::InvalidResponse)?.to_string())
        } else { None },
    };
    if next.as_deref().is_some_and(|value| value.is_empty() || Some(value) == cursor) { return Err(OfficialApiError::InvalidResponse); }
    let complete = next.is_none() && payload.get("incompleteSearch").and_then(Value::as_bool) != Some(true);
    Ok(OfficialPage { entries, next, complete })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[tokio::test]
    async fn stream_replacement_rejects_changed_version_before_payload_dispatch() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let address = listener.local_addr().expect("address");
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept");
            let mut request = [0; 4096];
            let size = socket.read(&mut request).await.expect("read");
            assert!(String::from_utf8_lossy(&request[..size]).starts_with("GET /drive/v3/files/item?"));
            let body = br#"{"version":"2"}"#;
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).as_bytes()).await.expect("header");
            socket.write_all(body).await.expect("body");
            assert!(tokio::time::timeout(std::time::Duration::from_millis(100), listener.accept()).await.is_err(), "no PATCH after changed version");
        });
        let mut api = OfficialApi::new(OfficialProvider::GoogleDrive, Zeroizing::new("test-token".into())).expect("api");
        api.fixture = Some(format!("http://{address}"));
        let result = api.replace_file_stream("item", tempfile::tempfile().expect("file"), 0, Some("1")).await;
        assert!(matches!(result, Err(OfficialApiError::Http { status: 412, .. })));
        server.await.expect("server");
    }

    #[tokio::test]
    async fn mutation_errors_preserve_status_redact_token_and_keep_write_revision() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        for (status, body, expected) in [
            ("200 OK", r#"{"id":"a"}"#, None),
            ("412 Precondition Failed", r#"{"error":{"message":"revision changed secret-token"}}"#, Some(412)),
            ("403 Forbidden", r#"{"error":{"message":"permission denied"}}"#, Some(403)),
        ] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
            let address = listener.local_addr().expect("address");
            let server = tokio::spawn(async move {
                let (mut socket, _) = listener.accept().await.expect("accept");
                let mut request = [0; 4096];
                socket.read(&mut request).await.expect("read");
                socket.write_all(format!("HTTP/1.1 {status}\r\nETag: \"write-revision\"\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.expect("response");
            });
            let api = OfficialApi::new(OfficialProvider::GoogleDrive, Zeroizing::new("secret-token".into())).expect("api");
            let result = api.mutation_response(api.client.patch(format!("http://{address}/item"))).await;
            if let Some(expected) = expected {
                let error = result.expect_err("rejected");
                assert!(matches!(&error, OfficialApiError::Http { status, .. } if *status == expected));
                assert!(!error.to_string().contains("secret-token"));
            } else { assert_eq!(result.expect("success")["eTag"], "\"write-revision\""); }
            server.await.expect("server");
        }
    }

    #[tokio::test]
    async fn media_reads_preserve_bytes_and_reject_hostile_redirects() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        for (provider, status, location, accepted) in [
            (OfficialProvider::GoogleDrive, "200 OK", "", true),
            (OfficialProvider::OneDrive, "302 Found", "Location: https://evil.invalid/\r\n", false),
            (OfficialProvider::GoogleDrive, "403 Forbidden", "", false),
        ] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("listener");
            let address = listener.local_addr().expect("address");
            let task = tokio::spawn(async move {
                let (mut socket, _) = listener.accept().await.expect("accept");
                let mut buffer = [0; 4096]; socket.read(&mut buffer).await.expect("read");
                socket.write_all(format!("HTTP/1.1 {status}\r\n{location}Content-Length: 7\r\nConnection: close\r\n\r\npayload").as_bytes()).await.expect("write");
            });
            let api = OfficialApi::new(provider, Zeroizing::new("token".into())).expect("api");
            let result = api.media_request(api.client.get(format!("http://{address}/content"))).await;
            assert_eq!(result.is_ok(), accepted);
            if let Ok(response) = result { assert_eq!(&response.bytes().await.expect("body")[..], b"payload"); }
            task.await.expect("server");
        }
    }

    #[tokio::test]
    async fn media_errors_preserve_provider_reason_and_redact_credentials() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        for body in [
            r#"{"error":{"message":"Download blocked secret-token","errors":[{"reason":"downloadRestricted"}]}}"#,
            "Download denied secret-token",
        ] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("listener");
            let address = listener.local_addr().expect("address");
            let task = tokio::spawn(async move {
                let (mut socket, _) = listener.accept().await.expect("accept");
                let mut buffer = [0; 4096]; socket.read(&mut buffer).await.expect("read");
                socket.write_all(format!("HTTP/1.1 403 Forbidden\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.expect("write");
            });
            let api = OfficialApi::new(OfficialProvider::GoogleDrive, Zeroizing::new("secret-token".into())).expect("api");
            let error = api.media_request(api.client.get(format!("http://{address}/content"))).await.expect_err("403");
            let OfficialApiError::Http { status, message } = error else { panic!("HTTP error required") };
            assert_eq!(status, 403);
            assert!(message.contains("Download"), "{message}");
            assert!(!message.contains("secret-token"));
            if body.starts_with('{') { assert!(message.contains("downloadRestricted")); }
            task.await.expect("server");
        }
    }

    #[tokio::test]
    async fn upload_multipart_uses_related_content_and_exact_parent_and_payload() {
        let api = OfficialApi::new(OfficialProvider::GoogleDrive, Zeroizing::new("token".into())).expect("api");
        let mut file = tempfile::tempfile().expect("file");
        use std::io::{Write, Seek};
        file.write_all(b"payload").expect("write"); file.rewind().expect("rewind");
        let progress = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let request = api.upload_request("root", "quote\".txt", file, 7, Some(progress.clone())).expect("upload").build().expect("request");
        assert_eq!(progress.load(std::sync::atomic::Ordering::Relaxed), 0);
        assert_eq!(request.url().path(), "/upload/drive/v3/files");
        assert!(request.headers()[reqwest::header::CONTENT_TYPE].to_str().expect("type").starts_with("multipart/related;"));
        let expected = request.headers()[reqwest::header::CONTENT_LENGTH].to_str().expect("length").parse::<usize>().expect("size");
        // Consume the streamed request through a local HTTP substitute, not Google.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("listener");
        let address = listener.local_addr().expect("address");
        let task = tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let (mut socket, _) = listener.accept().await.expect("accept");
            let mut bytes = Vec::new();
            loop {
                let mut chunk = [0; 4096]; let n = socket.read(&mut chunk).await.expect("read"); if n == 0 { break; } bytes.extend_from_slice(&chunk[..n]);
                if let Some(start) = bytes.windows(4).position(|s| s == b"\r\n\r\n") { if bytes.len() >= start + 4 + expected { break; } }
            }
            socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}").await.expect("response");
            bytes
        });
        let mut request = request;
        *request.url_mut() = url::Url::parse(&format!("http://{address}/upload")).expect("url");
        api.client.execute(request).await.expect("send");
        assert_eq!(progress.load(std::sync::atomic::Ordering::Relaxed), 7, "count file bytes, not multipart metadata or framing");
        let bytes = task.await.expect("task");
        let body = String::from_utf8_lossy(&bytes);
        assert!(body.contains("\"parents\":[\"root\"]"));
        assert!(body.contains("payload"));
        assert!(body.contains("quote\\\".txt"));
    }

    #[test]
    fn docs_export_preserves_type_and_uses_official_export_endpoint() {
        let api = OfficialApi::new(OfficialProvider::GoogleDrive, Zeroizing::new("token".into())).expect("api");
        for (kind, extension) in [("document", ".docx"), ("spreadsheet", ".xlsx"), ("presentation", ".pptx"), ("drawing", ".pdf")] {
            let content_type = format!("application/vnd.google-apps.{kind}");
            let (mime, ext) = OfficialApi::export_format(Some(&content_type)).expect("supported").expect("format");
            assert_eq!(ext, extension);
            let page = parse_page(OfficialProvider::GoogleDrive, serde_json::json!({"files":[{"id":"doc", "name":"test", "mimeType":content_type}]}), None).expect("listing");
            assert_eq!(page.entries[0].content_type.as_deref(), Some(content_type.as_str()));
            let request = api.export_request("doc", mime).expect("export").build().expect("request");
            assert_eq!(request.url().host_str(), Some("www.googleapis.com"));
            assert_eq!(request.url().path(), "/drive/v3/files/doc/export");
            assert!(request.url().query_pairs().any(|(key, value)| key == "mimeType" && value == mime));
        }
        assert!(OfficialApi::export_format(Some("application/vnd.google-apps.form")).is_err());
        assert!(OfficialApi::export_format(Some("text/plain")).expect("binary").is_none());
        assert!(api.export_request("../escape", "application/pdf").is_err());
    }

    #[test]
    fn content_redirects_reject_credential_exfiltration_targets() {
        assert!(content_redirect("https://tenant.sharepoint.com/download?token=signed").is_ok());
        for location in ["http://tenant.sharepoint.com/download", "https://evil.invalid/download", "https://tenant.sharepoint.com.evil.invalid/", "https://user:password@tenant.sharepoint.com/", "https://127.0.0.1/", "https://tenant.sharepoint.com:8443/", "https://tenant.sharepoint.com/#fragment"] {
            assert!(content_redirect(location).is_err(), "{location}");
        }
    }

    #[test]
    fn requests_use_only_official_endpoints_and_encode_parent_ids() {
        for provider in [OfficialProvider::GoogleDrive, OfficialProvider::OneDrive, OfficialProvider::BaiduNetdisk] {
            let api = OfficialApi::new(provider, Zeroizing::new("secret".into())).expect("api");
            let parent = if provider == OfficialProvider::BaiduNetdisk { "/docs" } else { "root" };
            let request = api.list_request(parent, None).expect("request").build().expect("build");
            assert_eq!(request.url().scheme(), "https");
            match provider {
                OfficialProvider::GoogleDrive => assert_eq!(request.url().host_str(), Some("www.googleapis.com")),
                OfficialProvider::OneDrive => {
                    assert_eq!(request.url().host_str(), Some("graph.microsoft.com"));
                    assert_eq!(request.url().path(), "/v1.0/me/drive/root/children");
                }
                OfficialProvider::BaiduNetdisk => assert_eq!(request.url().host_str(), Some("pan.baidu.com")),
            }
        }
    }

    #[tokio::test]
    async fn http_listing_adapter_uses_bounded_json_and_rejects_errors() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        for (provider, body) in [
            (OfficialProvider::GoogleDrive, r#"{"files":[]}"#),
            (OfficialProvider::OneDrive, r#"{"value":[]}"#),
            (OfficialProvider::BaiduNetdisk, r#"{"errno":0,"list":[]}"#),
        ] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("listener");
            let address = listener.local_addr().expect("address");
            let task = tokio::spawn(async move {
                let (mut socket, _) = listener.accept().await.expect("accept");
                let mut bytes = [0; 8192];
                socket.read(&mut bytes).await.expect("read");
                socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.expect("write");
            });
            let api = OfficialApi::new(provider, Zeroizing::new("token".into())).expect("api");
            let page = api.execute_list(api.client.get(format!("http://{address}/list")), None).await.expect("page");
            assert!(page.entries.is_empty() && page.next.is_none());
            task.await.expect("server");
        }
    }

    #[test]
    fn official_listings_keep_identity_and_pagination() {
        let google = parse_page(OfficialProvider::GoogleDrive, json!({"files":[{"id":"f1","name":"notes","mimeType":"text/plain","size":"9","version":"2"}],"nextPageToken":"next"}), None).expect("google");
        assert_eq!(google.entries[0].size, 9);
        assert_eq!(google.next.as_deref(), Some("next"));
        let ms = parse_page(OfficialProvider::OneDrive, json!({"value":[{"id":"d1","name":"docs","size":0,"folder":{}}],"@odata.nextLink":"https://graph.microsoft.com/v1.0/me/drive/root/children?$skiptoken=next"}), None).expect("microsoft");
        assert!(ms.entries[0].is_dir);
        assert_eq!(ms.next.as_deref(), Some("next"));
        let baidu = parse_page(OfficialProvider::BaiduNetdisk, json!({"errno":0,"list":[{"path":"/docs","server_filename":"docs","size":0,"isdir":1}]}), None).expect("baidu");
        assert_eq!(baidu.entries[0].id, "/docs");
        assert!(baidu.next.is_none());
    }
    #[test]
    fn listing_boundaries_duplicates_and_provider_errors_are_preserved() {
        let items: Vec<_> = (0..200).map(|n| json!({"path":format!("/file-{n}"),"server_filename":format!("file-{n}"),"size":0,"isdir":0})).collect();
        let page = parse_page(OfficialProvider::BaiduNetdisk, json!({"errno":0,"list":items}), Some("200")).expect("exact page boundary");
        assert_eq!(page.next.as_deref(), Some("400"));
        let over: Vec<_> = (0..201).map(|n| json!({"id":format!("f{n}"),"name":format!("n{n}"),"mimeType":"text/plain"})).collect();
        assert!(parse_page(OfficialProvider::GoogleDrive, json!({"files":over}), None).is_err());
        let duplicate = json!({"id":"same","name":"file","mimeType":"text/plain"});
        assert!(parse_page(OfficialProvider::GoogleDrive, json!({"files":[duplicate.clone(),duplicate]}), None).is_err());
        let error = parse_page(OfficialProvider::BaiduNetdisk, json!({"errno":-6,"errmsg":"expired token"}), None).expect_err("provider error");
        assert!(matches!(error, OfficialApiError::Provider { code, message } if code == "-6" && message == "expired token"));
    }

    #[test]
    fn incomplete_malformed_and_hostile_pages_fail_closed() {
        assert!(parse_page(OfficialProvider::GoogleDrive, json!({}), None).is_err());
        let incomplete = parse_page(OfficialProvider::GoogleDrive, json!({"files":[],"incompleteSearch":true}), None).expect("incomplete search");
        assert!(!incomplete.complete);
        assert!(parse_page(OfficialProvider::GoogleDrive, json!({"files":[],"nextPageToken":"same"}), Some("same")).is_err());
        assert!(parse_page(OfficialProvider::OneDrive, json!({"value":[],"@odata.nextLink":"https://evil.invalid/?$skiptoken=x"}), None).is_err());
        assert!(parse_page(OfficialProvider::BaiduNetdisk, json!({"errno":-6,"list":[]}), None).is_err());
        let file = json!({"id":"f","name":"a/b","mimeType":"text/plain"});
        assert!(parse_page(OfficialProvider::GoogleDrive, json!({"files":[file]}), None).is_err());
    }
}
