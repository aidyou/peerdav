//! PeerDAV resumable upload wire headers used by the session executor.
//! Local selection, opened-file validation and recovery policy belong to the host.
use crate::{dav_error, peerdav_file_core::FileError};
use peerdav_file_core::Operation;
use reqwest::{Method, RequestBuilder};
use reqwest_dav::Client;
use std::time::Duration;

pub async fn probe(
    client: &Client,
    remote: &str,
    size: u64,
    timeout: Duration,
) -> Result<RequestBuilder, FileError> {
    Ok(client
        .start_request(Method::OPTIONS, remote)
        .await
        .map_err(|e| dav_error(e, Operation::Write))?
        .timeout(timeout)
        .header("x-peerdav-transfer", "00".repeat(32))
        .header("x-peerdav-size", size)
        .header("x-peerdav-upload-probe", "1"))
}
pub async fn part(
    client: &Client,
    remote: &str,
    method: Method,
    session: &str,
    digest: &str,
    size: u64,
    index: u64,
    timeout: Duration,
) -> Result<RequestBuilder, FileError> {
    Ok(client
        .start_request(method, remote)
        .await
        .map_err(|e| dav_error(e, Operation::Write))?
        .timeout(timeout)
        .header("x-peerdav-session", session)
        .header("x-peerdav-transfer", digest)
        .header("x-peerdav-size", size)
        .header("x-peerdav-part", index))
}
pub async fn commit(
    client: &Client,
    remote: &str,
    session: &str,
    digest: &str,
    size: u64,
    streaming: bool,
    final_digest: &str,
    timeout: Duration,
) -> Result<RequestBuilder, FileError> {
    Ok(client
        .start_request(Method::PUT, remote)
        .await
        .map_err(|e| dav_error(e, Operation::Write))?
        .timeout(timeout)
        .header("x-peerdav-session", session)
        .header("x-peerdav-transfer", digest)
        .header("x-peerdav-size", size)
        .header("x-peerdav-commit", "1")
        .header(
            "x-peerdav-upload-version",
            if streaming { "2" } else { "1" },
        )
        .header("x-peerdav-content-sha256", final_digest)
        .body(Vec::new()))
}
pub async fn cancel(
    client: &Client,
    remote: &str,
    session: &str,
    digest: &str,
    size: u64,
    timeout: Duration,
) -> Result<RequestBuilder, FileError> {
    Ok(client
        .start_request(Method::DELETE, remote)
        .await
        .map_err(|e| dav_error(e, Operation::Write))?
        .timeout(timeout)
        .header("x-peerdav-session", session)
        .header("x-peerdav-transfer", digest)
        .header("x-peerdav-size", size)
        .header("x-peerdav-cancel", "1"))
}
