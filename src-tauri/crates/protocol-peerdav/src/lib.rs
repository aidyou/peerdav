//! PeerDAV wire operations. Clients are supplied by the host; this crate never
//! constructs TLS, proxy, redirect, authorization or local filesystem policy.
use std::time::Duration;

use futures_util::StreamExt;
use peerdav_file_core::*;
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, percent_decode_str, utf8_percent_encode};
use reqwest::StatusCode;
use reqwest_dav::{
    Client, Depth,
    types::list_cmd::{ListEntity, ListMultiStatus},
};

pub use peerdav_file_core;
pub mod backend;
pub mod session;
pub mod upload;
pub use backend::CorePeerDavBackend;

pub const LISTING_BODY_LIMIT: u64 = 8 * 1024 * 1024;
pub const TRANSFER_TIMEOUT: Duration = Duration::from_secs(300);
const PATH_SEGMENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

fn failure(
    kind: FileErrorKind,
    op: Operation,
    source: impl std::error::Error + Send + Sync + 'static,
) -> FileError {
    FileError::new(kind, op, MutationOutcome::NotApplied).with_source(Box::new(source))
}
fn invalid(reason: &'static str) -> FileError {
    failure(
        FileErrorKind::InvalidInput,
        Operation::Scope,
        std::io::Error::new(std::io::ErrorKind::InvalidInput, reason),
    )
}
pub fn normalize_display_path(path: &str) -> Result<String, FileError> {
    if path.len() > 4096 {
        return Err(invalid("the remote path is too long"));
    }
    if path.contains(['\\', '\0']) {
        return Err(invalid("the remote path contains a forbidden character"));
    }
    let mut segments = Vec::new();
    for segment in path.trim_matches('/').split('/') {
        if segment.is_empty() {
            continue;
        }
        if segment == "." || segment == ".." || segment.chars().any(char::is_control) {
            return Err(invalid("invalid path segment"));
        }
        segments.push(segment);
    }
    Ok(if segments.is_empty() {
        "/".into()
    } else {
        format!("/{}", segments.join("/"))
    })
}
pub fn validate_share_id(id: &str) -> Result<(), FileError> {
    if id.is_empty() || id.len() > 64 || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
        return Err(invalid("the share id is invalid"));
    }
    Ok(())
}
pub fn encode_segment(segment: &str) -> String {
    utf8_percent_encode(segment, PATH_SEGMENT).to_string()
}
pub fn encode_share_path(share: &str, path: &str) -> Result<String, FileError> {
    validate_share_id(share)?;
    let path = normalize_display_path(path)?;
    let mut encoded = format!("/share/{share}/");
    for segment in path.trim_matches('/').split('/').filter(|s| !s.is_empty()) {
        encoded.push_str(&encode_segment(segment));
        encoded.push('/');
    }
    Ok(encoded)
}
/// Decode exactly once, after parsing an absolute URL. Require the share boundary
/// and validate the result; never confuse share `a` with share `ab`.
pub fn href_to_relative(href: &str, share: &str) -> Option<String> {
    validate_share_id(share).ok()?;
    let raw = if href.starts_with("http://") || href.starts_with("https://") {
        url::Url::parse(href).ok()?.path().to_owned()
    } else {
        href.to_owned()
    };
    let prefix = format!("/share/{share}");
    let rest = raw.strip_prefix(&prefix)?;
    if !rest.is_empty() && !rest.starts_with('/') {
        return None;
    }
    // Encoded separators would change path identity and must not be interpreted.
    if rest.to_ascii_lowercase().contains("%2f") || rest.to_ascii_lowercase().contains("%5c") {
        return None;
    }
    let decoded = percent_decode_str(rest).decode_utf8().ok()?;
    normalize_display_path(&decoded).ok()
}

pub fn status_error(status: StatusCode, op: Operation) -> FileError {
    let kind = match status.as_u16() {
        401 | 403 => FileErrorKind::PermissionDenied,
        404 => FileErrorKind::NotFound,
        409 | 412 => FileErrorKind::Precondition,
        _ => FileErrorKind::Provider,
    };
    let mut error = failure(kind, op, std::io::Error::other(format!("HTTP {status}")));
    error.status = Some(status.as_u16());
    error
}
pub fn transport(error: reqwest::Error, op: Operation) -> FileError {
    let status = error.status().map(|s| s.as_u16());
    let mut result = failure(FileErrorKind::Transport, op, error);
    result.status = status;
    result
}
pub fn dav_error(error: reqwest_dav::Error, op: Operation) -> FileError {
    let status = match &error {
        reqwest_dav::Error::Decode(reqwest_dav::DecodeError::StatusMismatched(s)) => {
            Some(s.response_code)
        }
        reqwest_dav::Error::Decode(reqwest_dav::DecodeError::Server(s)) => Some(s.response_code),
        _ => None,
    };
    // reqwest_dav preserves its underlying request/decode source.
    let mut result = if let Some(code) = status.and_then(|s| StatusCode::from_u16(s).ok()) {
        status_error(code, op)
    } else {
        FileError::new(FileErrorKind::Transport, op, MutationOutcome::NotApplied)
    };
    result = result.with_source(Box::new(error));
    result
}
pub async fn read_response_limited(
    mut response: reqwest::Response,
    limit: u64,
    op: Operation,
) -> Result<Vec<u8>, FileError> {
    if response.content_length().is_some_and(|n| n > limit) {
        return Err(FileError::new(
            FileErrorKind::BudgetExceeded,
            op,
            MutationOutcome::NotApplied,
        ));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|e| transport(e, op))? {
        if (bytes.len() as u64).saturating_add(chunk.len() as u64) > limit {
            return Err(FileError::new(
                FileErrorKind::BudgetExceeded,
                op,
                MutationOutcome::NotApplied,
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}
/// Bound both header and body/decode time, as well as the XML allocation.
pub async fn list_entities(
    client: &Client,
    remote: &str,
    depth: Depth,
    op: Operation,
    limit: u64,
) -> Result<Vec<ListEntity>, FileError> {
    tokio::time::timeout(TRANSFER_TIMEOUT, async {
        let response = client
            .list_raw(remote, depth)
            .await
            .map_err(|e| dav_error(e, op))?;
        if !response.status().is_success() {
            return Err(status_error(response.status(), op));
        }
        let bytes = read_response_limited(response, limit.min(LISTING_BODY_LIMIT), op).await?;
        let text =
            std::str::from_utf8(&bytes).map_err(|e| failure(FileErrorKind::Provider, op, e))?;
        let parsed: ListMultiStatus = reqwest_dav::re_exports::serde_xml_rs::from_str(text)
            .map_err(|e| failure(FileErrorKind::Provider, op, e))?;
        parsed
            .responses
            .into_iter()
            .map(|r| ListEntity::try_from(r).map_err(|e| failure(FileErrorKind::Provider, op, e)))
            .collect()
    })
    .await
    .map_err(|e| failure(FileErrorKind::Deadline, op, e))?
}
pub async fn remote_exists(client: &Client, remote: &str) -> Result<bool, FileError> {
    let response =
        tokio::time::timeout(TRANSFER_TIMEOUT, client.list_raw(remote, Depth::Number(0)))
            .await
            .map_err(|e| failure(FileErrorKind::Deadline, Operation::Stat, e))?
            .map_err(|e| dav_error(e, Operation::Stat))?;
    match response.status() {
        s if s.is_success() => Ok(true),
        StatusCode::NOT_FOUND => Ok(false),
        s => Err(status_error(s, Operation::Stat)),
    }
}
/// Legacy recursive DELETE, deliberately separate from the core capability
/// matrix: WebDAV DELETE cannot enforce File/EmptyDirectory type conditions.
pub async fn delete_entry(client: &Client, remote: &str) -> Result<(), FileError> {
    client.delete(remote).await.map_err(|e| {
        let mut e = dav_error(e, Operation::Delete);
        if e.status.is_none() {
            e.outcome = MutationOutcome::Unknown;
        }
        e
    })
}
pub async fn create_directory(client: &Client, remote: &str) -> Result<(), FileError> {
    client.mkcol(remote).await.map_err(|e| {
        let mut e = dav_error(e, Operation::CreateDir);
        if e.status.is_none() {
            e.outcome = MutationOutcome::Unknown;
        }
        e
    })
}
pub async fn get(
    client: &Client,
    remote: &str,
    range: Option<ReadRange>,
) -> Result<reqwest::Response, FileError> {
    let mut request = client
        .start_request(reqwest::Method::GET, remote)
        .await
        .map_err(|e| dav_error(e, Operation::Read))?;
    if let Some(range) = range {
        request = request.header(
            reqwest::header::RANGE,
            format!("bytes={}-{}", range.start(), range.end() - 1),
        );
    }
    let response = request
        .send()
        .await
        .map_err(|e| transport(e, Operation::Read))?;
    if !response.status().is_success() {
        return Err(status_error(response.status(), Operation::Read));
    }
    if let Some(range) = range {
        let expected = format!("bytes {}-{}/", range.start(), range.end() - 1);
        let valid = response.status() == StatusCode::PARTIAL_CONTENT
            && response
                .headers()
                .get(reqwest::header::CONTENT_RANGE)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.strip_prefix(&expected))
                .and_then(|n| n.parse::<u64>().ok())
                .is_some_and(|n| n >= range.end())
            && response.content_length().is_none_or(|n| n == range.len());
        if !valid {
            return Err(failure(
                FileErrorKind::Provider,
                Operation::Read,
                std::io::Error::other("server did not honor the requested range"),
            ));
        }
    } else if response.status() == StatusCode::PARTIAL_CONTENT {
        return Err(failure(
            FileErrorKind::Provider,
            Operation::Read,
            std::io::Error::other("unsolicited partial response"),
        ));
    }
    Ok(response)
}
pub fn response_stream(response: reqwest::Response) -> ByteStream {
    Box::pin(
        response
            .bytes_stream()
            .map(|r| r.map_err(|e| transport(e, Operation::Read))),
    )
}

#[cfg(test)]
mod tests;
