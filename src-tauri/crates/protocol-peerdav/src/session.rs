//! Bounded resumable session execution. No local paths or filesystem authority.
use crate::{upload, *};
use futures_util::{TryStreamExt, stream};
use sha2::{Digest, Sha256};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

pub const PART_BYTES: u64 = 10 * 1024 * 1024;
pub const COMMIT_TIMEOUT: Duration = Duration::from_secs(1800);
pub const CANCEL_TIMEOUT: Duration = Duration::from_secs(10);

fn error(reason: &'static str) -> FileError {
    failure(
        FileErrorKind::InvalidInput,
        Operation::Write,
        std::io::Error::other(reason),
    )
}

pub(crate) async fn execute(
    client: &Client,
    remote: &str,
    context: OperationContext,
    request: SourceWriteRequest,
) -> Result<MutationResult, FileError> {
    let SourceWriteRequest {
        source,
        session_id,
        progress,
        ..
    } = request;
    let size = source.size();
    let report = |phase, bytes, sent| {
        if let Some(p) = &progress {
            p(phase, bytes, size, sent);
        }
    };
    context.check(Operation::Write, MutationOutcome::NotApplied)?;
    source.validate().await?;
    let probe = context
        .wait(Operation::Write, MutationOutcome::NotApplied, async {
            upload::probe(client, remote, size, TRANSFER_TIMEOUT)
                .await?
                .send()
                .await
                .map_err(|e| transport(e, Operation::Write))
        })
        .await?;
    let streaming = probe
        .headers()
        .get("x-peerdav-upload-version")
        .is_some_and(|v| v == "2");
    let mut hash = Sha256::new();
    if !streaming {
        report(WritePhase::Hashing, 0, 0);
        let mut offset = 0;
        while offset < size {
            let length = (size - offset)
                .min(context.budget.max_chunk_bytes.get() as u64)
                .min(1024 * 1024) as usize;
            let bytes = context
                .wait(
                    Operation::Write,
                    MutationOutcome::NotApplied,
                    source.read_at(offset, length),
                )
                .await?;
            if bytes.len() != length {
                return Err(error("source returned an incomplete chunk"));
            }
            hash.update(&bytes);
            offset += length as u64;
            report(WritePhase::Hashing, offset, 0);
        }
        source.validate().await?;
    }
    let digest = if streaming {
        hex::encode(Sha256::digest(source.resume_key().as_bytes()))
    } else {
        hex::encode(hash.finalize())
    };
    let sent = Arc::new(AtomicU64::new(0));
    let completed = Arc::new(AtomicU64::new(0));
    let content_hash = Arc::new(std::sync::Mutex::new(Sha256::new()));
    report(WritePhase::Uploading, 0, 0);
    let outcome = {
        let transferring = async {
            // Ordered source reads preserve the v2 cumulative digest. At most five
            // part buffers are in flight; body chunks remain independently bounded.
            stream::iter(0..size.div_ceil(PART_BYTES).max(1))
                .then(|index| {
                    let source = Arc::clone(&source);
                    let hash = Arc::clone(&content_hash);
                    async move {
                        let length =
                            size.saturating_sub(index * PART_BYTES).min(PART_BYTES) as usize;
                        let mut bytes = Vec::with_capacity(length);
                        while bytes.len() < length {
                            let n =
                                (length - bytes.len()).min(context.budget.max_chunk_bytes.get());
                            let chunk = source
                                .read_at(index * PART_BYTES + bytes.len() as u64, n)
                                .await?;
                            if chunk.len() != n {
                                return Err(error("source returned an incomplete chunk"));
                            }
                            bytes.extend_from_slice(&chunk);
                        }
                        if streaming {
                            hash.lock()
                                .map_err(|_| error("hash lock poisoned"))?
                                .update(&bytes);
                        }
                        Ok::<_, FileError>((index, bytes))
                    }
                })
                .boxed()
                .map(|part| {
                    let digest = &digest;
                    let session_id = &session_id;
                    let sent = Arc::clone(&sent);
                    let completed = Arc::clone(&completed);
                    let progress = progress.clone();
                    let chunk_limit = context.budget.max_chunk_bytes.get().min(64 * 1024);
                    async move {
                        let (index, bytes) = part?;
                        let length = bytes.len();
                        let part_hash = hex::encode(Sha256::digest(&bytes));
                        let existing = upload::part(
                            client,
                            remote,
                            reqwest::Method::HEAD,
                            session_id,
                            digest,
                            size,
                            index,
                            TRANSFER_TIMEOUT,
                        )
                        .await?
                        .header("x-peerdav-part-sha256", &part_hash)
                        .send()
                        .await
                        .map_err(|e| transport(e, Operation::Write))?;
                        if existing.status() == StatusCode::NO_CONTENT {
                            let bytes = completed.fetch_add(length as u64, Ordering::Relaxed)
                                + length as u64;
                            if let Some(p) = progress {
                                p(
                                    WritePhase::Uploading,
                                    bytes,
                                    size,
                                    sent.load(Ordering::Relaxed),
                                );
                            }
                            return Ok(());
                        }
                        if existing.status() != StatusCode::NOT_FOUND {
                            return Err(status_error(existing.status(), Operation::Write));
                        }
                        let body = stream::unfold(
                            (bytes::Bytes::from(bytes), 0),
                            move |(bytes, offset)| {
                                let sent = Arc::clone(&sent);
                                let completed = Arc::clone(&completed);
                                let progress = progress.clone();
                                async move {
                                    if offset == bytes.len() {
                                        return None;
                                    }
                                    let end = (offset + chunk_limit).min(bytes.len());
                                    let chunk = bytes.slice(offset..end);
                                    let done = completed
                                        .fetch_add(chunk.len() as u64, Ordering::Relaxed)
                                        + chunk.len() as u64;
                                    let network = sent
                                        .fetch_add(chunk.len() as u64, Ordering::Relaxed)
                                        + chunk.len() as u64;
                                    if let Some(p) = progress {
                                        p(WritePhase::Uploading, done, size, network);
                                    }
                                    Some((Ok::<_, std::io::Error>(chunk), (bytes, end)))
                                }
                            },
                        );
                        let response = upload::part(
                            client,
                            remote,
                            reqwest::Method::PATCH,
                            session_id,
                            digest,
                            size,
                            index,
                            TRANSFER_TIMEOUT,
                        )
                        .await?
                        .header("x-peerdav-part-sha256", part_hash)
                        .header(reqwest::header::CONTENT_LENGTH, length)
                        .body(reqwest::Body::wrap_stream(body))
                        .send()
                        .await
                        .map_err(|e| transport(e, Operation::Write))?;
                        if !response.status().is_success() {
                            return Err(status_error(response.status(), Operation::Write));
                        }
                        Ok(())
                    }
                })
                .buffer_unordered(size.div_ceil(PART_BYTES).clamp(1, 5) as usize)
                .try_for_each(|()| async { Ok(()) })
                .await?;
            source.validate().await?;
            let final_digest = if streaming {
                hex::encode(
                    content_hash
                        .lock()
                        .map_err(|_| error("hash lock poisoned"))?
                        .clone()
                        .finalize(),
                )
            } else {
                digest.clone()
            };
            report(WritePhase::Committing, size, sent.load(Ordering::Relaxed));
            let response = upload::commit(
                client,
                remote,
                &session_id,
                &digest,
                size,
                streaming,
                &final_digest,
                COMMIT_TIMEOUT,
            )
            .await?
            .send()
            .await
            .map_err(|e| {
                let mut e = transport(e, Operation::Write);
                e.outcome = MutationOutcome::Unknown;
                e
            })?;
            if !response.status().is_success() {
                let mut e = status_error(response.status(), Operation::Write);
                e.outcome = MutationOutcome::Unknown;
                return Err(e);
            }
            Ok(())
        };
        context
            .wait(Operation::Write, MutationOutcome::Unknown, transferring)
            .await
    }; // Active request bodies drop before cancellation/reconciliation.
    if let Err(mut primary) = outcome {
        let cleanup = async {
            upload::cancel(client, remote, &session_id, &digest, size, CANCEL_TIMEOUT)
                .await?
                .send()
                .await
                .map_err(|e| transport(e, Operation::Write))
        }
        .await;
        match cleanup {
            Ok(response) if response.status() == StatusCode::OK => {} // commit won
            Ok(response) if response.status() == StatusCode::NO_CONTENT => {
                primary.outcome = MutationOutcome::NotApplied;
                return Err(primary);
            }
            other => {
                let cleanup = match other {
                    Ok(response) => status_error(response.status(), Operation::Write),
                    Err(e) => e,
                };
                primary.outcome = MutationOutcome::Unknown;
                primary.cleanup.push(CleanupFailure {
                    operation: Operation::Write,
                    status: cleanup.status,
                    provider_code: cleanup.provider_code.clone(),
                    recovery_id: Some(session_id),
                    source: Box::new(cleanup),
                });
                return Err(primary);
            }
        }
    }
    report(WritePhase::Completed, size, sent.load(Ordering::Relaxed));
    Ok(MutationResult { entry: None })
}
