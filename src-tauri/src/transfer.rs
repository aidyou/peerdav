//! Bounded parallel transfers. Completed parts survive failed attempts and are
//! reused only for the same remote revision (download) or content digest (upload).

use crate::error::DomainError;
use crate::peer::TransferProgressCallback;
use crate::types::{ConflictPolicy, TRANSFER_MAX_WORKERS, TRANSFER_PART_BYTES};
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
        detail: source.to_string(),
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
) -> Result<(), DomainError> {
    let mut input = tokio::fs::File::open(local)
        .await
        .map_err(|e| io(local, e))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let len = input.read(&mut buffer).await.map_err(|e| io(local, e))?;
        if len == 0 {
            break;
        }
        hasher.update(&buffer[..len]);
    }
    let digest = hex::encode(hasher.finalize());
    let count = size.div_ceil(TRANSFER_PART_BYTES).max(1);
    stream::iter(0..count)
        .map(|index| {
            let client = client.clone();
            let digest = digest.clone();
            async move {
                let request = |method| async {
                    client
                        .start_request(method, remote)
                        .await
                        .map_err(dav)
                        .map(|r| {
                            r.header("x-peerdav-transfer", &digest)
                                .header("x-peerdav-size", size)
                                .header("x-peerdav-part", index)
                        })
                };
                let mut input = tokio::fs::File::open(local)
                    .await
                    .map_err(|e| io(local, e))?;
                use tokio::io::AsyncSeekExt;
                input
                    .seek(std::io::SeekFrom::Start(index * TRANSFER_PART_BYTES))
                    .await
                    .map_err(|e| io(local, e))?;
                let length = size
                    .saturating_sub(index * TRANSFER_PART_BYTES)
                    .min(TRANSFER_PART_BYTES) as usize;
                let mut bytes = vec![0u8; length];
                input
                    .read_exact(&mut bytes)
                    .await
                    .map_err(|e| io(local, e))?;
                let part_hash = hex::encode(Sha256::digest(&bytes));
                let existing = request(reqwest::Method::HEAD)
                    .await?
                    .header("x-peerdav-part-sha256", &part_hash)
                    .send()
                    .await
                    .map_err(transport)?;
                if existing.status() == reqwest::StatusCode::NO_CONTENT {
                    return Ok::<_, DomainError>(());
                }
                let response = request(reqwest::Method::PATCH)
                    .await?
                    .header("x-peerdav-part-sha256", &part_hash)
                    .body(bytes)
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
    let response = client
        .start_request(reqwest::Method::PUT, remote)
        .await
        .map_err(dav)?
        .header("x-peerdav-transfer", digest)
        .header("x-peerdav-size", size)
        .header("x-peerdav-commit", "1")
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
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
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
