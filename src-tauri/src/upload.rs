//! Resumable uploads are scoped to the authenticated device, destination and
//! content digest. Partial bytes stay outside shares until the full hash matches.

use crate::error::DomainError;
use crate::fs::ShareConfig;
use crate::types::{
    MAX_TRANSFER_BYTES, TRANSFER_PART_BYTES, UploadOutcome, UploadSession, UploadSessions,
};
use futures_util::future::poll_fn;
use http::{Request, StatusCode};
use hyper::body::{Body, Incoming};
use sha2::{Digest, Sha256};
use std::path::Path;
use std::sync::Arc;
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
    request: Request<Incoming>,
    share: &ShareConfig,
    relative: &Path,
    identity: &str,
    root: &Path,
    sessions: &UploadSessions,
) -> Result<StatusCode, DomainError> {
    let header = |name| {
        request
            .headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
    };
    let digest = crate::tls::normalize_fingerprint(Some(header("x-peerdav-transfer")))
        .ok_or_else(|| invalid("invalid upload digest"))?;
    let size = header("x-peerdav-size")
        .parse::<u64>()
        .map_err(|_| invalid("invalid upload size"))?;
    if size > MAX_TRANSFER_BYTES {
        return Err(DomainError::TransferTooLarge {
            size,
            limit: MAX_TRANSFER_BYTES,
        });
    }
    let session_id = header("x-peerdav-session");
    if session_id.len() > 128 || !session_id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(invalid("invalid upload session"));
    }
    let identity_key = hex::encode(Sha256::digest(
        format!(
            "{identity}\n{}\n{}\n{digest}\n{size}",
            share.id,
            relative.display()
        )
        .as_bytes(),
    ));
    let key = format!("{identity_key}:{session_id}");
    let directory = root.join(&identity_key);
    let cancel_request =
        request.method() == http::Method::DELETE && header("x-peerdav-cancel") == "1";
    let session = {
        let mut sessions = sessions.lock().await;
        // Keep terminal tombstones so late in-flight requests cannot recreate
        // canceled files. Bound memory; new attempts use a fresh session ID.
        if sessions.len() >= 10_000 && !sessions.contains_key(&key) {
            return Err(invalid("too many upload sessions; restart the server"));
        }
        let active = sessions
            .iter()
            .find(|(existing, _)| existing.starts_with(&format!("{identity_key}:")))
            .map(|(_, session)| Arc::clone(&session.active))
            .unwrap_or_else(|| Arc::new(tokio::sync::RwLock::new(())));
        Arc::clone(sessions.entry(key).or_insert_with(|| {
            let (cancel, _) = tokio::sync::watch::channel(false);
            Arc::new(UploadSession {
                cancel,
                active,
                outcome: std::sync::Mutex::new(UploadOutcome::Active),
            })
        }))
    };
    if cancel_request {
        {
            let mut outcome = session
                .outcome
                .lock()
                .map_err(|_| invalid("upload state lock poisoned"))?;
            if *outcome == UploadOutcome::Completed {
                return Ok(StatusCode::OK);
            }
            *outcome = UploadOutcome::Canceled;
            session.cancel.send_replace(true);
        }
        // A retry may reuse verified parts from an older failed attempt.
        // Signal every writer of that staging directory before deleting it.
        {
            let sessions = sessions.lock().await;
            for (key, other) in sessions
                .iter()
                .filter(|(key, _)| key.starts_with(&format!("{identity_key}:")))
            {
                let mut outcome = other
                    .outcome
                    .lock()
                    .map_err(|_| invalid("upload state lock poisoned"))?;
                if *outcome != UploadOutcome::Completed {
                    *outcome = UploadOutcome::Canceled;
                    other.cancel.send_replace(true);
                }
                log::debug!("canceled upload staging session {key}");
            }
        }
        // Wait until receivers and the blocking copy have closed their files.
        let _exclusive = session.active.write().await;
        match tokio::fs::remove_dir_all(&directory).await {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(io(&directory, error)),
        }
        return Ok(StatusCode::NO_CONTENT);
    }
    let active = Arc::clone(&session.active).read_owned().await;
    if !session_id.is_empty()
        && *session
            .outcome
            .lock()
            .map_err(|_| invalid("upload state lock poisoned"))?
            == UploadOutcome::Completed
    {
        return Ok(if request.method() == http::Method::PUT {
            StatusCode::CREATED
        } else {
            StatusCode::CONFLICT
        });
    }
    if *session.cancel.borrow() {
        return Err(DomainError::TransferCanceled);
    }
    // Keep the guard until pending filesystem operations close their handles;
    // dropping an in-flight create/write future could outlive cleanup.
    receive(
        request,
        share,
        relative,
        identity,
        root,
        Arc::clone(&session),
        active,
    )
    .await
}

async fn receive(
    mut request: Request<Incoming>,
    share: &ShareConfig,
    relative: &Path,
    identity: &str,
    root: &Path,
    session: Arc<UploadSession>,
    active: tokio::sync::OwnedRwLockReadGuard<()>,
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
                if *session.cancel.borrow() {
                    return Err(DomainError::TransferCanceled);
                }
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
            let _active = active;
            crate::fs::commit_upload(&share, &destination, &source, &session)
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
    let mut cancel = session.cancel.subscribe();
    let mut part_hash = Sha256::new();
    loop {
        if *cancel.borrow() {
            return Err(DomainError::TransferCanceled);
        }
        let frame = tokio::select! {
            biased;
            _ = cancel.changed() => return Err(DomainError::TransferCanceled),
            frame = poll_fn(|cx| std::pin::Pin::new(request.body_mut()).poll_frame(cx)) => frame,
        };
        let Some(frame) = frame else {
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
