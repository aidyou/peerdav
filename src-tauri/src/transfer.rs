//! Bounded parallel transfers. Completed parts survive failed attempts and are
//! reused only for the same remote revision (download) or content digest (upload).

use crate::error::DomainError;
use crate::peer::TransferProgressCallback;
use crate::types::{
    ConflictPolicy, PEER_TRANSFER_REQUEST_TIMEOUT, PEER_UPLOAD_CANCEL_TIMEOUT,
    PEER_UPLOAD_COMMIT_TIMEOUT, TRANSFER_MAX_WORKERS, TRANSFER_PART_BYTES, UploadPhase,
    UploadProgressCallback,
};
use futures_util::{StreamExt, TryStreamExt, stream};
use reqwest_dav::Client;
use sha2::{Digest, Sha256};
use std::path::Path;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

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
    let (keep_cancel, default_cancel) = tokio::sync::watch::channel(false);
    let (session_id, mut cancel) = cancellation.unwrap_or_else(|| {
        let mut id = [0u8; 16];
        rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut id);
        (hex::encode(id), default_cancel)
    });
    let _keep_cancel = keep_cancel;
    let report = |phase, bytes, sent| {
        if let Some(callback) = &progress {
            callback(phase, bytes, size, sent);
        }
    };
    if *cancel.borrow() {
        return Err(DomainError::TransferCanceled);
    }
    let probe = async {
        client
            .start_request(reqwest::Method::OPTIONS, remote)
            .await
            .map_err(dav)?
            .timeout(PEER_TRANSFER_REQUEST_TIMEOUT)
            .header("x-peerdav-transfer", "00".repeat(32))
            .header("x-peerdav-size", size)
            .header("x-peerdav-upload-probe", "1")
            .send()
            .await
            .map_err(transport)
    };
    let probe = tokio::select! {
        biased;
        _ = cancel.changed() => return Err(DomainError::TransferCanceled),
        result = probe => result?,
    };
    let streaming = probe
        .headers()
        .get("x-peerdav-upload-version")
        .is_some_and(|value| value == "2");
    report(
        if streaming {
            UploadPhase::Uploading
        } else {
            UploadPhase::Hashing
        },
        0,
        0,
    );
    let mut hashed = 0;
    let mut input = tokio::fs::File::open(local)
        .await
        .map_err(|e| io(local, e))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1024 * 1024];
    while !streaming {
        if *cancel.borrow() {
            return Err(DomainError::TransferCanceled);
        }
        let len = tokio::select! {
            biased;
            _ = cancel.changed() => return Err(DomainError::TransferCanceled),
            result = input.read(&mut buffer) => result.map_err(|e| io(local, e))?,
        };
        if len == 0 {
            break;
        }
        hasher.update(&buffer[..len]);
        hashed += len as u64;
        report(UploadPhase::Hashing, hashed, 0);
    }
    let digest = if streaming {
        // A stable resume key is not a content guarantee: every reused part is
        // checked and the final content digest is supplied separately.
        hex::encode(Sha256::digest(
            format!("{}\n{size}", local.display()).as_bytes(),
        ))
    } else {
        hex::encode(hasher.finalize())
    };
    let content_hash = Arc::new(std::sync::Mutex::new(Sha256::new()));
    let original_metadata = tokio::fs::metadata(local).await.map_err(|e| io(local, e))?;
    report(UploadPhase::Uploading, 0, 0);
    let completed = Arc::new(AtomicU64::new(0));
    let sent = Arc::new(AtomicU64::new(0));
    let count = size.div_ceil(TRANSFER_PART_BYTES).max(1);
    let outcome = {
        let transferring = async {
            let content_hash_for_read = Arc::clone(&content_hash);
            stream::iter(0..count)
                .then(|index| {
                    let content_hash = Arc::clone(&content_hash_for_read);
                    async move {
                        let path = local.to_path_buf();
                        tokio::task::spawn_blocking(move || {
                            use std::io::{Read, Seek};
                            let length = size
                                .saturating_sub(index * TRANSFER_PART_BYTES)
                                .min(TRANSFER_PART_BYTES)
                                as usize;
                            let mut input = std::fs::File::open(&path).map_err(|e| io(&path, e))?;
                            input
                                .seek(std::io::SeekFrom::Start(index * TRANSFER_PART_BYTES))
                                .map_err(|e| io(&path, e))?;
                            let mut bytes = vec![0u8; length];
                            input.read_exact(&mut bytes).map_err(|e| io(&path, e))?;
                            if streaming {
                                content_hash
                                    .lock()
                                    .map_err(|_| protocol("upload hash lock poisoned"))?
                                    .update(&bytes);
                            }
                            let part_hash = hex::encode(Sha256::digest(&bytes));
                            Ok::<_, DomainError>((index, bytes, part_hash))
                        })
                        .await
                        .map_err(|e| DomainError::Internal {
                            detail: e.to_string(),
                        })?
                    }
                })
                .boxed()
                .map(|part| {
                    let client = client.clone();
                    let digest = digest.clone();
                    let session_id = session_id.clone();
                    let completed = Arc::clone(&completed);
                    let sent = Arc::clone(&sent);
                    let progress = progress.clone();
                    async move {
                        let (index, bytes, part_hash) = part?;
                        let length = bytes.len();
                        let request = |method| async {
                            client
                                .start_request(method, remote)
                                .await
                                .map_err(dav)
                                .map(|r| {
                                    r.timeout(PEER_TRANSFER_REQUEST_TIMEOUT)
                                        .header("x-peerdav-session", &session_id)
                                        .header("x-peerdav-transfer", &digest)
                                        .header("x-peerdav-size", size)
                                        .header("x-peerdav-part", index)
                                })
                        };

                        let existing = request(reqwest::Method::HEAD)
                            .await?
                            .header("x-peerdav-part-sha256", &part_hash)
                            .send()
                            .await
                            .map_err(transport)?;
                        if existing.status() == reqwest::StatusCode::NO_CONTENT {
                            let bytes = completed.fetch_add(length as u64, Ordering::Relaxed)
                                + length as u64;
                            if let Some(progress) = &progress {
                                progress(
                                    UploadPhase::Uploading,
                                    bytes,
                                    size,
                                    sent.load(Ordering::Relaxed),
                                );
                            }
                            return Ok::<_, DomainError>(());
                        }
                        let completed_for_body = Arc::clone(&completed);
                        let sent_for_body = Arc::clone(&sent);
                        let progress_for_body = progress.clone();
                        let body = stream::unfold(
                            (bytes::Bytes::from(bytes), 0usize),
                            move |(bytes, offset)| {
                                let completed = Arc::clone(&completed_for_body);
                                let sent = Arc::clone(&sent_for_body);
                                let progress = progress_for_body.clone();
                                async move {
                                    if offset == bytes.len() {
                                        return None;
                                    }
                                    let end = (offset + 64 * 1024).min(bytes.len());
                                    let chunk = bytes.slice(offset..end);
                                    let transferred = completed
                                        .fetch_add(chunk.len() as u64, Ordering::Relaxed)
                                        + chunk.len() as u64;
                                    let network_bytes = sent
                                        .fetch_add(chunk.len() as u64, Ordering::Relaxed)
                                        + chunk.len() as u64;
                                    if let Some(progress) = progress {
                                        progress(
                                            UploadPhase::Uploading,
                                            transferred,
                                            size,
                                            network_bytes,
                                        );
                                    }
                                    Some((Ok::<_, std::io::Error>(chunk), (bytes, end)))
                                }
                            },
                        );
                        let response = request(reqwest::Method::PATCH)
                            .await?
                            .header("x-peerdav-part-sha256", &part_hash)
                            .header(reqwest::header::CONTENT_LENGTH, length)
                            .body(reqwest::Body::wrap_stream(body))
                            .send()
                            .await
                            .map_err(transport)?;
                        if !response.status().is_success() {
                            return Err(protocol(format!(
                                "upload part returned {}",
                                response.status()
                            )));
                        }
                        Ok(())
                    }
                })
                .buffer_unordered(workers(size))
                .try_collect::<Vec<_>>()
                .await?;
            let metadata = tokio::fs::metadata(local).await.map_err(|e| io(local, e))?;
            if streaming
                && (metadata.len() != size
                    || metadata.modified().ok() != original_metadata.modified().ok())
            {
                return Err(protocol(
                    "upload source changed; retry with the current file",
                ));
            }
            let final_digest = if streaming {
                hex::encode(
                    content_hash
                        .lock()
                        .map_err(|_| protocol("upload hash lock poisoned"))?
                        .clone()
                        .finalize(),
                )
            } else {
                digest.clone()
            };
            report(UploadPhase::Committing, size, sent.load(Ordering::Relaxed));
            let response = client
                .start_request(reqwest::Method::PUT, remote)
                .await
                .map_err(dav)?
                .timeout(PEER_UPLOAD_COMMIT_TIMEOUT)
                .header("x-peerdav-session", &session_id)
                .header("x-peerdav-transfer", &digest)
                .header("x-peerdav-size", size)
                .header("x-peerdav-commit", "1")
                .header(
                    "x-peerdav-upload-version",
                    if streaming { "2" } else { "1" },
                )
                .header("x-peerdav-content-sha256", &final_digest)
                .body(Vec::new())
                .send()
                .await
                .map_err(transport)?;
            if !response.status().is_success() {
                return Err(protocol(format!(
                    "upload commit returned {}",
                    response.status()
                )));
            }
            Ok::<_, DomainError>(())
        };
        tokio::pin!(transferring);
        if *cancel.borrow() {
            Err(DomainError::TransferCanceled)
        } else {
            tokio::select! {
                biased;
                _ = cancel.changed() => Err(DomainError::TransferCanceled),
                result = &mut transferring => result,
            }
        }
    }; // Drop all active request bodies before asking the server to clean up.
    if matches!(outcome, Err(DomainError::TransferCanceled)) {
        let response = client
            .start_request(reqwest::Method::DELETE, remote)
            .await
            .map_err(dav)?
            .timeout(PEER_UPLOAD_CANCEL_TIMEOUT)
            .header("x-peerdav-transfer", &digest)
            .header("x-peerdav-session", &session_id)
            .header("x-peerdav-size", size)
            .header("x-peerdav-cancel", "1")
            .send()
            .await
            .map_err(transport)?;
        match response.status() {
            reqwest::StatusCode::OK => {} // Atomic replacement won the race.
            reqwest::StatusCode::NO_CONTENT => return Err(DomainError::TransferCanceled),
            status => return Err(protocol(format!("upload cancellation returned {status}"))),
        }
    } else {
        outcome?;
    }
    report(UploadPhase::Completed, size, sent.load(Ordering::Relaxed));
    Ok(())
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
            upload(&client, "/empty.bin", source.path(), 0, None, None).await?;
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
            "/file",
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
