//! Bounded parallel transfers. Completed parts survive failed attempts and are
//! reused only for the same remote revision (download) or content digest (upload).

use crate::error::DomainError;
use crate::peer::TransferProgressCallback;
use crate::types::{
    ConflictPolicy, PEER_TRANSFER_REQUEST_TIMEOUT, TRANSFER_MAX_WORKERS, TRANSFER_PART_BYTES,
    UploadPhase, UploadProgressCallback,
};
use futures_util::{StreamExt, TryStreamExt, stream};
use reqwest_dav::Client;
use sha2::{Digest, Sha256};
use std::path::Path;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
#[cfg(test)]
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;

use core::{FileBackend, ReplayableWriteBackend};
use peerdav_protocol_peerdav::peerdav_file_core as core;
fn file_error(source: peerdav_protocol_peerdav::peerdav_file_core::FileError) -> DomainError {
    DomainError::PeerFile {
        operation: "transfer file",
        source,
    }
}

fn protocol(detail: impl Into<String>) -> DomainError {
    DomainError::PeerProtocol {
        detail: detail.into(),
    }
}
fn io(path: &Path, source: std::io::Error) -> DomainError {
    DomainError::Io {
        operation: "transfer file",
        path: Some(path.into()),
        source,
    }
}
fn transport(source: reqwest::Error) -> DomainError {
    DomainError::PeerRequest {
        operation: "transfer file",
        status: source.status().map(|s| s.as_u16()),
        detail: crate::error::error_chain(&source),
    }
}
fn dav(source: reqwest_dav::Error) -> DomainError {
    protocol(source.to_string())
}
pub(crate) fn workers(size: u64) -> usize {
    size.div_ceil(TRANSFER_PART_BYTES)
        .clamp(1, TRANSFER_MAX_WORKERS as u64) as usize
}

async fn private_file(path: &Path, fresh: bool) -> Result<tokio::fs::File, DomainError> {
    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    if fresh {
        options.create_new(true);
    }
    #[cfg(unix)]
    {
        options
            .mode(0o600)
            .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32);
    }
    options.open(path).await.map_err(|e| io(path, e))
}

pub(crate) async fn download(
    client: &Client,
    remote: &str,
    display: &str,
    destination: &Path,
    conflict: ConflictPolicy,
    limit: u64,
    progress: Option<TransferProgressCallback>,
) -> Result<u64, DomainError> {
    let metadata = client
        .start_request(reqwest::Method::HEAD, remote)
        .await
        .map_err(dav)?
        .send()
        .await
        .map_err(transport)?;
    if !metadata.status().is_success() {
        return Err(protocol(format!(
            "metadata request returned {}",
            metadata.status()
        )));
    }
    let size = metadata
        .headers()
        .get(reqwest::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        .ok_or_else(|| protocol("missing transfer size"))?;
    if size > limit {
        return Err(DomainError::TransferTooLarge { size, limit });
    }
    let etag = metadata
        .headers()
        .get(reqwest::header::ETAG)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let revision = etag
        .clone()
        .filter(|tag| !tag.starts_with("W/"))
        .ok_or_else(|| protocol("a strong ETag is required for resumable downloads"))?;
    let identity = format!(
        "{}\n{remote}\n{display}\n{}\n{revision}\n{size}",
        metadata.url(),
        destination.display()
    );
    let key = hex::encode(Sha256::digest(identity.as_bytes()));
    let parent = destination
        .parent()
        .ok_or_else(|| protocol("missing download parent"))?;
    let part_dir = parent.join(format!(".peerdav-{}", &key[..24]));
    match tokio::fs::symlink_metadata(&part_dir).await {
        Ok(meta) if !meta.is_dir() || meta.file_type().is_symlink() => {
            return Err(protocol("invalid download staging directory"));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            tokio::fs::create_dir(&part_dir)
                .await
                .map_err(|e| io(&part_dir, e))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                tokio::fs::set_permissions(&part_dir, std::fs::Permissions::from_mode(0o700))
                    .await
                    .map_err(|e| io(&part_dir, e))?;
            }
        }
        Err(error) => return Err(io(&part_dir, error)),
    }
    let completed = Arc::new(AtomicU64::new(0));
    let count = size.div_ceil(TRANSFER_PART_BYTES);
    let parts = stream::iter(0..count)
        .map(|index| {
            let client = client.clone();
            let part_dir = part_dir.clone();
            let completed = Arc::clone(&completed);
            let progress = progress.clone();
            let etag = etag.clone();
            async move {
                let start = index * TRANSFER_PART_BYTES;
                let expected = (size - start).min(TRANSFER_PART_BYTES);
                let path = part_dir.join(index.to_string());
                let marker = part_dir.join(format!("{index}.sha256"));
                let reusable = match (
                    tokio::fs::symlink_metadata(&path).await,
                    tokio::fs::read_to_string(&marker).await,
                ) {
                    (Ok(meta), Ok(hash))
                        if meta.is_file()
                            && !meta.file_type().is_symlink()
                            && meta.len() == expected =>
                    {
                        let bytes = tokio::fs::read(&path).await.map_err(|e| io(&path, e))?;
                        hex::encode(Sha256::digest(&bytes)) == hash
                    }
                    _ => false,
                };
                if !reusable {
                    let mut request = client
                        .start_request(reqwest::Method::GET, remote)
                        .await
                        .map_err(dav)?
                        .timeout(PEER_TRANSFER_REQUEST_TIMEOUT)
                        .header(
                            reqwest::header::RANGE,
                            format!("bytes={start}-{}", start + expected - 1),
                        );
                    if let Some(etag) = etag {
                        request = request.header(reqwest::header::IF_MATCH, etag);
                    }
                    let mut response = request.send().await.map_err(transport)?;
                    if response.status() != reqwest::StatusCode::PARTIAL_CONTENT {
                        return Err(protocol(format!(
                            "range request returned {}",
                            response.status()
                        )));
                    }
                    let expected_range = format!("bytes {start}-{}/{size}", start + expected - 1);
                    if response
                        .headers()
                        .get(reqwest::header::CONTENT_RANGE)
                        .and_then(|v| v.to_str().ok())
                        != Some(expected_range.as_str())
                    {
                        return Err(protocol("invalid range response"));
                    }
                    let mut file = private_file(&path, false).await?;
                    let mut received = 0u64;
                    let mut hash = Sha256::new();
                    while let Some(bytes) = response.chunk().await.map_err(transport)? {
                        received += bytes.len() as u64;
                        if received > expected {
                            return Err(protocol("part exceeds its declared range"));
                        }
                        hash.update(&bytes);
                        file.write_all(&bytes).await.map_err(|e| io(&path, e))?;
                    }
                    if received != expected {
                        return Err(protocol("incomplete download part"));
                    }
                    file.sync_all().await.map_err(|e| io(&path, e))?;
                    let digest = hex::encode(hash.finalize());
                    let mut marker_file = private_file(&marker, false).await?;
                    marker_file
                        .write_all(digest.as_bytes())
                        .await
                        .map_err(|e| io(&marker, e))?;
                    marker_file.sync_all().await.map_err(|e| io(&marker, e))?;
                }
                let transferred = completed.fetch_add(expected, Ordering::Relaxed) + expected;
                if let Some(progress) = progress {
                    progress(transferred, size);
                }
                Ok::<_, DomainError>(())
            }
        })
        .buffer_unordered(workers(size));
    parts.try_collect::<Vec<_>>().await?;
    let assembled = part_dir.join("assembled");
    let mut output = private_file(&assembled, false).await?;
    for index in 0..count {
        let path = part_dir.join(index.to_string());
        let mut input = tokio::fs::File::open(&path)
            .await
            .map_err(|e| io(&path, e))?;
        tokio::io::copy(&mut input, &mut output)
            .await
            .map_err(|e| io(&assembled, e))?;
    }
    output.sync_all().await.map_err(|e| io(&assembled, e))?;
    drop(output);
    if conflict == ConflictPolicy::Fail && destination.exists() {
        return Err(DomainError::TransferConflict {
            path: destination.display().to_string(),
        });
    }
    tokio::fs::rename(&assembled, destination)
        .await
        .map_err(|e| io(destination, e))?;
    tokio::fs::remove_dir_all(&part_dir)
        .await
        .map_err(|e| io(&part_dir, e))?;
    if let Some(progress) = progress {
        progress(size, size);
    }
    Ok(size)
}

pub(crate) async fn upload(
    client: &Client,
    remote: &str,
    local: &Path,
    size: u64,
    progress: Option<UploadProgressCallback>,
    cancellation: Option<(String, tokio::sync::watch::Receiver<bool>)>,
) -> Result<(), DomainError> {
    let file = std::fs::File::open(local).map_err(|e| io(local, e))?;
    upload_opened(client, remote, file, local, size, progress, cancellation).await
}

/// All reads and identity checks use the already authorized open file, never its ambient pathname.
pub(crate) async fn upload_opened(
    client: &Client,
    remote: &str,
    file: std::fs::File,
    local: &Path,
    size: u64,
    progress: Option<UploadProgressCallback>,
    cancellation: Option<(String, tokio::sync::watch::Receiver<bool>)>,
) -> Result<(), DomainError> {
    return upload_core_source(client, remote, file, local, size, progress, cancellation).await;
}

struct OpenedUploadSource {
    file: Arc<std::sync::Mutex<std::fs::File>>,
    metadata: std::fs::Metadata,
    key: String,
}
fn source_error(source: impl std::error::Error + Send + Sync + 'static) -> core::FileError {
    core::FileError::new(
        core::FileErrorKind::InvalidInput,
        core::Operation::Write,
        core::MutationOutcome::NotApplied,
    )
    .with_source(Box::new(source))
}
impl core::ReplayableSource for OpenedUploadSource {
    fn size(&self) -> u64 {
        self.metadata.len()
    }
    fn resume_key(&self) -> &str {
        &self.key
    }
    fn read_at(
        &self,
        offset: u64,
        length: usize,
    ) -> core::BoxFuture<'_, Result<bytes::Bytes, core::FileError>> {
        let file = Arc::clone(&self.file);
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                use std::io::{Read, Seek};
                let mut file = file
                    .lock()
                    .map_err(|_| source_error(std::io::Error::other("source lock poisoned")))?;
                file.seek(std::io::SeekFrom::Start(offset))
                    .map_err(source_error)?;
                let mut bytes = vec![0; length];
                file.read_exact(&mut bytes).map_err(source_error)?;
                Ok(bytes::Bytes::from(bytes))
            })
            .await
            .map_err(source_error)?
        })
    }
    fn validate(&self) -> core::BoxFuture<'_, Result<(), core::FileError>> {
        Box::pin(async move {
            let metadata = self
                .file
                .lock()
                .map_err(|_| source_error(std::io::Error::other("source lock poisoned")))?
                .metadata()
                .map_err(source_error)?;
            if metadata.len() != self.metadata.len()
                || metadata.modified().ok() != self.metadata.modified().ok()
            {
                return Err(source_error(std::io::Error::other(
                    "upload source changed; retry with the current file",
                )));
            }
            Ok(())
        })
    }
}

async fn upload_core_source(
    client: &Client,
    remote: &str,
    file: std::fs::File,
    local: &Path,
    size: u64,
    progress: Option<UploadProgressCallback>,
    cancellation: Option<(String, tokio::sync::watch::Receiver<bool>)>,
) -> Result<(), DomainError> {
    if crate::types::PEER_UPLOAD_COMMIT_TIMEOUT != peerdav_protocol_peerdav::session::COMMIT_TIMEOUT
        || crate::types::PEER_UPLOAD_CANCEL_TIMEOUT
            != peerdav_protocol_peerdav::session::CANCEL_TIMEOUT
    {
        return Err(protocol("upload timeout policy mismatch"));
    }
    let metadata = file.metadata().map_err(|e| io(local, e))?;
    if !metadata.is_file() || metadata.len() != size {
        return Err(protocol(
            "upload source changed; retry with the current file",
        ));
    }
    // Decode only the already validated share URI supplied by destination resolution.
    let suffix = remote
        .strip_prefix("/share/")
        .ok_or_else(|| protocol("invalid upload share path"))?;
    let (share, _) = suffix
        .split_once('/')
        .ok_or_else(|| protocol("invalid upload share path"))?;
    let path = peerdav_protocol_peerdav::href_to_relative(remote, share)
        .ok_or_else(|| protocol("invalid upload path"))?;
    let backend = peerdav_protocol_peerdav::CorePeerDavBackend::new(
        client.clone(),
        "authorized-client".into(),
        share.into(),
        0,
    )
    .map_err(file_error)?;
    let (keep_cancel, default_cancel) = tokio::sync::watch::channel(false);
    let (session_id, mut cancel) = cancellation.unwrap_or_else(|| {
        let mut id = [0u8; 16];
        rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut id);
        (hex::encode(id), default_cancel)
    });
    let _keep_cancel = keep_cancel;
    let token = tokio_util::sync::CancellationToken::new();
    if *cancel.borrow() {
        return Err(DomainError::TransferCanceled);
    }
    let request = core::SourceWriteRequest {
        entry: backend.scope().entry(path),
        condition: core::Condition::Any,
        mode: core::WriteMode::Streaming,
        source: Arc::new(OpenedUploadSource {
            file: Arc::new(std::sync::Mutex::new(file)),
            metadata,
            key: format!("{}\n{size}", local.display()),
        }),
        session_id,
        progress: progress.map(|p| {
            Arc::new(move |phase, bytes, total, sent| {
                let phase = match phase {
                    core::WritePhase::Hashing => UploadPhase::Hashing,
                    core::WritePhase::Uploading => UploadPhase::Uploading,
                    core::WritePhase::Committing => UploadPhase::Committing,
                    core::WritePhase::Completed => UploadPhase::Completed,
                };
                p(phase, bytes, total, sent);
            }) as core::WriteProgress
        }),
    };
    let operation = backend.write_source(
        core::OperationContext {
            cancellation: token.clone(),
            deadline: None,
            budget: core::OperationBudget {
                max_bytes: size,
                max_chunk_bytes: std::num::NonZeroUsize::new(64 * 1024)
                    .ok_or_else(|| protocol("invalid chunk budget"))?,
                max_entries: std::num::NonZeroUsize::MIN,
            },
        },
        request,
    );
    tokio::pin!(operation);
    let result = tokio::select! {
        biased;
        _ = cancel.changed() => { token.cancel(); operation.await },
        result = &mut operation => result,
    };
    match result {
        Ok(_) => Ok(()),
        Err(e)
            if e.kind == core::FileErrorKind::Cancelled
                && e.outcome == core::MutationOutcome::NotApplied
                && e.cleanup.is_empty() =>
        {
            Err(DomainError::TransferCanceled)
        }
        Err(e) => Err(file_error(e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn upload_requests_override_the_ordinary_client_timeout() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener");
        let address = listener.local_addr().expect("address");
        let server = tokio::spawn(async move {
            for (method, status) in [
                ("OPTIONS", 405),
                ("HEAD", 404),
                ("PATCH", 204),
                ("PUT", 201),
            ] {
                let (mut socket, _) = listener.accept().await.expect("connection");
                let mut headers = Vec::new();
                loop {
                    let byte = socket.read_u8().await.expect("request header");
                    headers.push(byte);
                    if headers.ends_with(b"\r\n\r\n") {
                        break;
                    }
                }
                assert!(headers.starts_with(method.as_bytes()));
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                socket
                    .write_all(
                        format!("HTTP/1.1 {status} Test\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                            .as_bytes(),
                    )
                    .await
                    .expect("response");
            }
        });
        let agent = reqwest::Client::builder()
            .no_proxy()
            .timeout(std::time::Duration::from_millis(1))
            .build()
            .expect("agent");
        let client = reqwest_dav::ClientBuilder::new()
            .set_agent(agent)
            .set_host(format!("http://{address}"))
            .build()
            .expect("DAV client");
        let source = tempfile::NamedTempFile::new().expect("empty source");
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            upload(
                &client,
                "/share/test/empty.bin/",
                source.path(),
                0,
                None,
                None,
            )
            .await?;
            server.await.expect("server task");
            Ok::<_, DomainError>(())
        })
        .await;
        assert!(matches!(result, Ok(Ok(()))), "{result:?}");
    }

    #[tokio::test]
    async fn cancellation_interrupts_a_stalled_part_request_and_confirms_remote_cleanup() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener");
        let address = listener.local_addr().expect("address");
        let (started, observed) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut probe, _) = listener.accept().await.expect("probe");
            let mut headers = Vec::new();
            loop {
                headers.push(probe.read_u8().await.expect("probe header"));
                if headers.ends_with(b"\r\n\r\n") {
                    break;
                }
            }
            assert!(headers.starts_with(b"OPTIONS"));
            probe.write_all(b"HTTP/1.1 204 No Content\r\nx-peerdav-upload-version: 2\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.expect("probe response");
            let (mut stalled, _) = listener.accept().await.expect("part HEAD");
            let mut headers = Vec::new();
            loop {
                headers.push(stalled.read_u8().await.expect("header"));
                if headers.ends_with(b"\r\n\r\n") {
                    break;
                }
            }
            assert!(headers.starts_with(b"HEAD"));
            started.send(()).expect("started");
            // Deliberately never respond to the HEAD. Cancellation must open
            // a separate authenticated request instead of waiting for it.
            let (mut cleanup, _) = listener.accept().await.expect("cleanup");
            let mut headers = Vec::new();
            loop {
                headers.push(cleanup.read_u8().await.expect("cancel header"));
                if headers.ends_with(b"\r\n\r\n") {
                    break;
                }
            }
            assert!(headers.starts_with(b"DELETE"));
            let headers = String::from_utf8(headers)
                .expect("headers")
                .to_ascii_lowercase();
            assert!(headers.contains("x-peerdav-cancel: 1"));
            assert!(headers.contains("x-peerdav-session: aa"));
            cleanup
                .write_all(
                    b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await
                .expect("cleanup response");
        });
        let agent = reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("agent");
        let client = reqwest_dav::ClientBuilder::new()
            .set_agent(agent)
            .set_host(format!("http://{address}"))
            .build()
            .expect("client");
        let source = tempfile::NamedTempFile::new().expect("empty file");
        let (cancel, receiver) = tokio::sync::watch::channel(false);
        let uploading = upload(
            &client,
            "/share/test/file/",
            source.path(),
            0,
            None,
            Some(("aa".into(), receiver)),
        );
        let canceling = async {
            observed.await.expect("request started");
            cancel.send_replace(true);
        };
        let result = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            let (result, ()) = tokio::join!(uploading, canceling);
            server.await.expect("server");
            result
        })
        .await
        .expect("cancel must not wait for request timeout");
        assert!(matches!(result, Err(DomainError::TransferCanceled)));
    }

    #[tokio::test]
    async fn changed_opened_source_cancels_remote_session_before_commit() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let source = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(source.path(), b"abc").unwrap();
        let changed = source.reopen().unwrap();
        let server = tokio::spawn(async move {
            for method in ["OPTIONS", "HEAD", "DELETE"] {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut headers = vec![];
                loop {
                    headers.push(socket.read_u8().await.unwrap());
                    if headers.ends_with(b"\r\n\r\n") {
                        break;
                    }
                }
                assert!(headers.starts_with(method.as_bytes()));
                if method == "HEAD" {
                    changed.set_len(4).unwrap();
                }
                socket.write_all(b"HTTP/1.1 204 No Content\r\nx-peerdav-upload-version: 2\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.unwrap();
            }
        });
        let client = reqwest_dav::ClientBuilder::new()
            .set_agent(reqwest::Client::builder().no_proxy().build().unwrap())
            .set_host(format!("http://{address}"))
            .build()
            .unwrap();
        let result = upload_opened(
            &client,
            "/share/test/source/",
            source.reopen().unwrap(),
            source.path(),
            3,
            None,
            None,
        )
        .await;
        match result {
            Err(DomainError::PeerFile { source, .. }) => {
                assert_eq!(source.outcome, core::MutationOutcome::NotApplied)
            }
            other => panic!("{other:?}"),
        }
        server.await.unwrap();
    }

    #[test]
    fn concurrency_tracks_ten_mebibyte_boundaries_and_is_capped() {
        for (bytes, expected) in [
            (0, 1),
            (1, 1),
            (TRANSFER_PART_BYTES, 1),
            (TRANSFER_PART_BYTES + 1, 2),
            (5 * TRANSFER_PART_BYTES, 5),
            (u64::MAX, 5),
        ] {
            assert_eq!(workers(bytes), expected);
        }
    }
}
