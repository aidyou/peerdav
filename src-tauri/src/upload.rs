//! Resumable uploads are scoped to the authenticated device, destination and
//! content digest. Partial bytes stay outside shares until the full hash matches.

use crate::error::DomainError;
use crate::fs::ShareConfig;
use crate::types::{MAX_TRANSFER_BYTES, TRANSFER_PART_BYTES};
use futures_util::future::poll_fn;
use http::{Request, StatusCode};
use hyper::body::{Body, Incoming};
use sha2::{Digest, Sha256};
use std::path::Path;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn invalid(detail: impl Into<String>) -> DomainError {
    DomainError::PeerProtocol {
        detail: detail.into(),
    }
}
fn io(path: &Path, source: std::io::Error) -> DomainError {
    DomainError::Io {
        operation: "receive upload",
        path: Some(path.into()),
        source,
    }
}

pub(crate) async fn handle(
    mut request: Request<Incoming>,
    share: &ShareConfig,
    relative: &Path,
    identity: &str,
    root: &Path,
) -> Result<StatusCode, DomainError> {
    let get = |name| {
        request
            .headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
    };
    let digest = get("x-peerdav-transfer")
        .and_then(|v| crate::tls::normalize_fingerprint(Some(&v)))
        .ok_or_else(|| invalid("invalid upload digest"))?;
    let size = get("x-peerdav-size")
        .and_then(|v| v.parse::<u64>().ok())
        .ok_or_else(|| invalid("invalid upload size"))?;
    if size > MAX_TRANSFER_BYTES {
        return Err(DomainError::TransferTooLarge {
            size,
            limit: MAX_TRANSFER_BYTES,
        });
    }
    let count = size.div_ceil(TRANSFER_PART_BYTES).max(1);
    let identity_key = hex::encode(Sha256::digest(
        format!(
            "{identity}\n{}\n{}\n{digest}\n{size}",
            share.id,
            relative.display()
        )
        .as_bytes(),
    ));
    let directory = root.join(identity_key);
    tokio::fs::create_dir_all(&directory)
        .await
        .map_err(|e| io(&directory, e))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))
            .await
            .map_err(|e| io(&directory, e))?;
    }
    if request.method() == http::Method::PUT && get("x-peerdav-commit").as_deref() == Some("1") {
        let assembled = directory.join("assembled");
        let mut output = tokio::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&assembled)
            .await
            .map_err(|e| io(&assembled, e))?;
        let mut hash = Sha256::new();
        let mut total = 0;
        let mut buffer = vec![0u8; 64 * 1024];
        for index in 0..count {
            let path = directory.join(index.to_string());
            let expected = size
                .saturating_sub(index * TRANSFER_PART_BYTES)
                .min(TRANSFER_PART_BYTES);
            let metadata = tokio::fs::symlink_metadata(&path)
                .await
                .map_err(|e| io(&path, e))?;
            if !metadata.is_file()
                || metadata.file_type().is_symlink()
                || metadata.len() != expected
            {
                return Err(invalid("upload part is incomplete"));
            }
            let mut input = tokio::fs::File::open(&path)
                .await
                .map_err(|e| io(&path, e))?;
            loop {
                let len = input.read(&mut buffer).await.map_err(|e| io(&path, e))?;
                if len == 0 {
                    break;
                }
                total += len as u64;
                if total > size {
                    return Err(invalid("upload exceeds declared size"));
                }
                hash.update(&buffer[..len]);
                output
                    .write_all(&buffer[..len])
                    .await
                    .map_err(|e| io(&assembled, e))?;
            }
        }
        if total != size || hex::encode(hash.finalize()) != digest {
            return Err(invalid(
                "upload content changed; retry with the current file",
            ));
        }
        output.sync_all().await.map_err(|e| io(&assembled, e))?;
        drop(output);
        let share = share.clone();
        let destination = relative.to_path_buf();
        let source = assembled.clone();
        tokio::task::spawn_blocking(move || {
            crate::fs::commit_upload(&share, &destination, &source)
        })
        .await
        .map_err(|e| DomainError::Internal {
            detail: e.to_string(),
        })??;
        tokio::fs::remove_dir_all(&directory)
            .await
            .map_err(|e| io(&directory, e))?;
        return Ok(StatusCode::CREATED);
    }
    let index = get("x-peerdav-part")
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|i| *i < count)
        .ok_or_else(|| invalid("invalid upload part"))?;
    let path = directory.join(index.to_string());
    let expected = size
        .saturating_sub(index * TRANSFER_PART_BYTES)
        .min(TRANSFER_PART_BYTES);
    let expected_hash = get("x-peerdav-part-sha256");
    if request.method() == http::Method::HEAD {
        return Ok(match tokio::fs::symlink_metadata(&path).await {
            Ok(meta)
                if meta.is_file() && !meta.file_type().is_symlink() && meta.len() == expected =>
            {
                let bytes = tokio::fs::read(&path).await.map_err(|e| io(&path, e))?;
                if expected_hash
                    .as_ref()
                    .is_none_or(|hash| *hash == hex::encode(Sha256::digest(&bytes)))
                {
                    StatusCode::NO_CONTENT
                } else {
                    StatusCode::NOT_FOUND
                }
            }
            _ => StatusCode::NOT_FOUND,
        });
    }
    if request.method() != http::Method::PATCH {
        return Ok(StatusCode::METHOD_NOT_ALLOWED);
    }
    let temporary = directory.join(format!("{index}.writing"));
    let mut options = tokio::fs::OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        options
            .mode(0o600)
            .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32);
    }
    let mut output = options
        .open(&temporary)
        .await
        .map_err(|e| io(&temporary, e))?;
    let mut received = 0;
    let mut part_hash = Sha256::new();
    loop {
        let Some(frame) = poll_fn(|cx| std::pin::Pin::new(request.body_mut()).poll_frame(cx)).await
        else {
            break;
        };
        let frame = frame.map_err(|e| invalid(e.to_string()))?;
        if let Ok(bytes) = frame.into_data() {
            received += bytes.len() as u64;
            if received > expected {
                return Err(invalid("upload part is too large"));
            }
            part_hash.update(&bytes);
            output
                .write_all(&bytes)
                .await
                .map_err(|e| io(&temporary, e))?;
        }
    }
    if received != expected {
        return Err(invalid("upload part was interrupted"));
    }
    if expected_hash.is_some_and(|hash| hash != hex::encode(part_hash.finalize())) {
        return Err(invalid("upload part digest does not match"));
    }
    output.sync_all().await.map_err(|e| io(&temporary, e))?;
    drop(output);
    tokio::fs::rename(&temporary, &path)
        .await
        .map_err(|e| io(&path, e))?;
    Ok(StatusCode::NO_CONTENT)
}
