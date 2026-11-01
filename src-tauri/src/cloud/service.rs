//! Service-level S3 operations that are not scoped to one bucket.
//!
//! Bucket discovery is the only operation here: it signs a `GET /` request with
//! AWS Signature V4 and parses the provider's `ListAllMyBucketsResult` document.
//! Regions are inferred from the address because users only provide an endpoint.

use std::time::{SystemTime, UNIX_EPOCH};

use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use url::Url;

use super::types::{CloudError, ProxyMode};

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BucketEntry {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
}

/// Reject anything that is not a plain service URL, matching object I/O rules:
/// HTTPS, or HTTP only for a private host with explicit consent.
pub(crate) fn normalize_endpoint(
    raw: &str,
    allow_insecure_http: bool,
) -> Result<Url, CloudError> {
    let url = Url::parse(raw).map_err(|_| CloudError::InvalidConfig("invalid endpoint URL".into()))?;
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !matches!(url.path(), "" | "/")
        || url.host_str().is_none()
    {
        return Err(CloudError::InvalidConfig(
            "endpoint must contain only scheme, host and port".into(),
        ));
    }
    if url.scheme() != "https"
        && !(url.scheme() == "http"
            && allow_insecure_http
            && super::s3::is_private(url.host_str()))
    {
        return Err(CloudError::InvalidConfig(
            "HTTPS is required; private HTTP requires explicit consent".into(),
        ));
    }
    Ok(url)
}

/// Best-effort region inferred from a service address. Providers that ignore the
/// region still receive a valid value, and regional providers encode it in the host.
pub(crate) fn derive_region(host: &str) -> String {
    let host = host.trim_matches(['[', ']']).to_ascii_lowercase();
    let between = |prefix: &str, suffix: &str| {
        host.strip_prefix(prefix)
            .and_then(|rest| rest.strip_suffix(suffix))
            .filter(|region| !region.is_empty())
            .map(str::to_owned)
    };
    // Cloudflare R2 and other gateways that expect the literal "auto".
    if host.ends_with(".r2.cloudflarestorage.com") {
        return "auto".into();
    }
    if let Some(region) = between("s3.", ".amazonaws.com") {
        return region;
    }
    if let Some(region) = between("s3.", ".amazonaws.com.cn") {
        return region;
    }
    if host == "s3.amazonaws.com" {
        return "us-east-1".into();
    }
    if let Some(region) = between("oss-", ".aliyuncs.com") {
        return region;
    }
    // Tencent COS exposes a region-less service endpoint.
    if host == "service.cos.myqcloud.com" {
        return "ap-guangzhou".into();
    }
    if let Some(region) = between("cos.", ".myqcloud.com") {
        return region;
    }
    if let Some(region) = between("s3.", ".qiniucs.com") {
        return region;
    }
    // Baidu BOS.
    if let Some(region) = between("s3.", ".bcebos.com") {
        return region;
    }
    // Huawei OBS.
    if let Some(region) = between("obs.", ".myhuaweicloud.com") {
        return region;
    }
    if host == "storage.googleapis.com" {
        return "us-east-1".into();
    }
    // MinIO, Ceph and most self-hosted gateways accept us-east-1 by default.
    "us-east-1".into()
}

/// The `Host` header value reqwest sends for this URL. It is part of the
/// canonical request, so default ports must be omitted exactly like the client.
fn host_header(url: &Url) -> Result<String, CloudError> {
    let host = url
        .host_str()
        .ok_or_else(|| CloudError::InvalidConfig("endpoint has no host".into()))?;
    match url.port() {
        Some(port) if !is_default_port(url.scheme(), port) => Ok(format!("{host}:{port}")),
        _ => Ok(host.to_owned()),
    }
}

fn is_default_port(scheme: &str, port: u16) -> bool {
    matches!((scheme, port), ("https", 443) | ("http", 80))
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn hmac_sha256(key: &[u8], data: &[u8]) -> Result<Vec<u8>, CloudError> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key)
        .map_err(|_| CloudError::InvalidConfig("invalid signing key".into()))?;
    mac.update(data);
    Ok(mac.finalize().into_bytes().to_vec())
}

/// UTC date and timestamp components used by SigV4 (`YYYYMMDD` and
/// `YYYYMMDDTHHMMSSZ`) without depending on a formatting feature.
fn amz_timestamp() -> Result<(String, String), CloudError> {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| CloudError::InvalidConfig("system clock is before the Unix epoch".into()))?
        .as_secs();
    let now = time::OffsetDateTime::from_unix_timestamp(seconds as i64)
        .map_err(|_| CloudError::InvalidConfig("system clock is invalid".into()))?;
    let date = format!(
        "{:04}{:02}{:02}",
        now.year(),
        u8::from(now.month()),
        now.day()
    );
    let stamp = format!(
        "{date}T{:02}{:02}{:02}Z",
        now.hour(),
        now.minute(),
        now.second()
    );
    Ok((date, stamp))
}

fn signature(
    secret_key: &str,
    date_stamp: &str,
    region: &str,
    service: &str,
    string_to_sign: &str,
) -> Result<String, CloudError> {
    let k_date = hmac_sha256(format!("AWS4{secret_key}").as_bytes(), date_stamp.as_bytes())?;
    let k_region = hmac_sha256(&k_date, region.as_bytes())?;
    let k_service = hmac_sha256(&k_region, service.as_bytes())?;
    let k_signing = hmac_sha256(&k_service, b"aws4_request")?;
    Ok(hex::encode(hmac_sha256(&k_signing, string_to_sign.as_bytes())?))
}

fn authorization(
    host: &str,
    region: &str,
    payload_hash: &str,
    amz_date: &str,
    date_stamp: &str,
    access_key: &str,
    secret_key: &str,
    session_token: Option<&str>,
) -> Result<String, CloudError> {
    let mut canonical_headers = format!(
        "host:{host}\nx-amz-content-sha256:{payload_hash}\nx-amz-date:{amz_date}\n"
    );
    let mut signed_headers = String::from("host;x-amz-content-sha256;x-amz-date");
    if let Some(token) = session_token {
        canonical_headers.push_str(&format!("x-amz-security-token:{token}\n"));
        signed_headers.push_str(";x-amz-security-token");
    }
    let canonical_request = format!(
        "GET\n/\n\n{canonical_headers}\n{signed_headers}\n{payload_hash}"
    );
    let scope = format!("{date_stamp}/{region}/s3/aws4_request");
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
        sha256_hex(canonical_request.as_bytes())
    );
    let signature = signature(secret_key, date_stamp, region, "s3", &string_to_sign)?;
    Ok(format!(
        "AWS4-HMAC-SHA256 Credential={access_key}/{scope}, SignedHeaders={signed_headers}, Signature={signature}"
    ))
}

fn unescape(value: &str) -> String {
    value
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

fn tag(block: &str, name: &str) -> Option<String> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let start = block.find(&open)? + open.len();
    let end = start + block[start..].find(&close)?;
    let value = unescape(&block[start..end]);
    (!value.is_empty()).then_some(value)
}

pub(crate) fn parse_buckets(xml: &str) -> Vec<BucketEntry> {
    let mut buckets = Vec::new();
    let mut rest = xml;
    while let Some(start) = rest.find("<Bucket>") {
        let after = &rest[start + "<Bucket>".len()..];
        let Some(end) = after.find("</Bucket>") else {
            break;
        };
        let block = &after[..end];
        if let Some(name) = tag(block, "Name") {
            buckets.push(BucketEntry {
                name,
                created_at: tag(block, "CreationDate"),
            });
        }
        rest = &after[end + "</Bucket>".len()..];
    }
    buckets
}

/// Signs and issues `GET /` to discover the buckets the credentials can see.
pub(crate) async fn list_buckets(
    endpoint: &Url,
    region: &str,
    access_key: &str,
    secret_key: &str,
    session_token: Option<&str>,
    client: &reqwest::Client,
) -> Result<Vec<BucketEntry>, CloudError> {
    if access_key.is_empty() || secret_key.is_empty() {
        return Err(CloudError::InvalidConfig(
            "access key and secret key are required".into(),
        ));
    }
    let host = host_header(endpoint)?;
    let payload_hash = sha256_hex(b"");
    let (date_stamp, amz_date) = amz_timestamp()?;
    let authorization = authorization(
        &host,
        region,
        &payload_hash,
        &amz_date,
        &date_stamp,
        access_key,
        secret_key,
        session_token,
    )?;
    let mut url = endpoint.clone();
    url.set_path("/");
    url.set_query(None);
    url.set_fragment(None);
    let mut request = client
        .get(url)
        .header("x-amz-content-sha256", &payload_hash)
        .header("x-amz-date", &amz_date);
    if let Some(token) = session_token {
        request = request.header("x-amz-security-token", token);
    }
    let response = request
        .header("authorization", authorization)
        .send()
        .await
        .map_err(|source| CloudError::Transport { source })?;
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|source| CloudError::Transport { source })?;
    if !status.is_success() {
        return Err(CloudError::Service {
            status: status.as_u16(),
            detail: body.chars().take(512).collect(),
        });
    }
    Ok(parse_buckets(&body))
}

/// Convenience entry point used by the runtime command.
pub(crate) async fn probe(
    raw_endpoint: &str,
    region: &str,
    access_key: &str,
    secret_key: &str,
    session_token: Option<&str>,
    allow_insecure_http: bool,
    proxy: &ProxyMode,
) -> Result<Vec<BucketEntry>, CloudError> {
    let endpoint = normalize_endpoint(raw_endpoint, allow_insecure_http)?;
    let region = if region.trim().is_empty() {
        derive_region(endpoint.host_str().unwrap_or_default())
    } else {
        region.trim().to_owned()
    };
    let transport = super::proxy::ConnectionTransport::new(proxy)?;
    list_buckets(
        &endpoint,
        &region,
        access_key,
        secret_key,
        session_token,
        transport.client(),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_regions_from_common_service_hosts() {
        assert_eq!(derive_region("s3.us-west-2.amazonaws.com"), "us-west-2");
        assert_eq!(derive_region("s3.amazonaws.com"), "us-east-1");
        assert_eq!(derive_region("s3.cn-north-1.amazonaws.com.cn"), "cn-north-1");
        assert_eq!(derive_region("oss-cn-hangzhou.aliyuncs.com"), "cn-hangzhou");
        assert_eq!(derive_region("service.cos.myqcloud.com"), "ap-guangzhou");
        assert_eq!(derive_region("cos.ap-guangzhou.myqcloud.com"), "ap-guangzhou");
        assert_eq!(derive_region("s3.cn-east-1.qiniucs.com"), "cn-east-1");
        assert_eq!(derive_region("s3.bj.bcebos.com"), "bj");
        assert_eq!(derive_region("obs.cn-north-4.myhuaweicloud.com"), "cn-north-4");
        assert_eq!(derive_region("abc.r2.cloudflarestorage.com"), "auto");
        assert_eq!(derive_region("storage.googleapis.com"), "us-east-1");
        assert_eq!(derive_region("localhost:9000"), "us-east-1");
    }

    #[test]
    fn parses_list_buckets_xml_without_a_dom() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<ListAllMyBucketsResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/">
  <Owner><ID>owner</ID></Owner>
  <Buckets>
    <Bucket><Name>photos</Name><CreationDate>2024-01-02T03:04:05.000Z</CreationDate></Bucket>
    <Bucket><Name>backups</Name><CreationDate>2024-02-03T04:05:06.000Z</CreationDate></Bucket>
  </Buckets>
</ListAllMyBucketsResult>"#;
        let buckets = parse_buckets(xml);
        assert_eq!(buckets.len(), 2);
        assert_eq!(buckets[0].name, "photos");
        assert_eq!(
            buckets[0].created_at.as_deref(),
            Some("2024-01-02T03:04:05.000Z")
        );
        assert_eq!(buckets[1].name, "backups");
    }

    #[test]
    fn matches_the_aws_sigv4_get_vanilla_test_vector() {
        // AWS Signature V4 test suite: get-vanilla.
        let canonical = "GET\n/\n\nhost:example.amazonaws.com\nx-amz-date:20150830T123600Z\n\nhost;x-amz-date\ne3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        let string_to_sign = format!(
            "AWS4-HMAC-SHA256\n20150830T123600Z\n20150830/us-east-1/service/aws4_request\n{}",
            sha256_hex(canonical.as_bytes())
        );
        let signature = signature(
            "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY",
            "20150830",
            "us-east-1",
            "service",
            &string_to_sign,
        )
        .expect("signature");
        assert_eq!(
            signature,
            "5fa00fa31553b73ebf1942676e86291e8372ff2a2260956d9b8aae1d763fbf31"
        );
    }

    #[test]
    fn rejects_non_service_endpoints() {
        assert!(normalize_endpoint("https://s3.amazonaws.com/bucket", false).is_err());
        assert!(normalize_endpoint("https://user:pass@s3.amazonaws.com", false).is_err());
        assert!(normalize_endpoint("ftp://s3.amazonaws.com", false).is_err());
        assert!(normalize_endpoint("http://127.0.0.1:9000", false).is_err());
        assert!(normalize_endpoint("http://127.0.0.1:9000", true).is_ok());
        assert!(normalize_endpoint("https://s3.us-west-2.amazonaws.com", false).is_ok());
    }

    #[tokio::test]
    async fn lists_buckets_over_a_signed_get() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind");
        let address = listener.local_addr().expect("address");
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept");
            let mut request = Vec::new();
            loop {
                let mut byte = [0u8; 1];
                stream.read_exact(&mut byte).await.expect("read");
                request.push(byte[0]);
                if request.ends_with(b"\r\n\r\n") {
                    break;
                }
            }
            let text = String::from_utf8_lossy(&request).into_owned();
            let body = "<ListAllMyBucketsResult><Buckets><Bucket><Name>alpha</Name></Bucket></Buckets></ListAllMyBucketsResult>";
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await
                .expect("write");
            text
        });
        let endpoint = normalize_endpoint(&format!("http://{address}"), true).expect("endpoint");
        let transport = super::super::proxy::ConnectionTransport::new(&ProxyMode::Direct)
            .expect("client");
        let buckets = list_buckets(
            &endpoint,
            "us-east-1",
            "access",
            "secret",
            None,
            transport.client(),
        )
        .await
        .expect("buckets");
        assert_eq!(buckets.len(), 1);
        assert_eq!(buckets[0].name, "alpha");
        let request = server.await.expect("server");
        assert!(request.starts_with("GET / HTTP/1.1"));
        assert!(request.contains("authorization: AWS4-HMAC-SHA256 Credential=access/"));
        assert!(request.contains("x-amz-content-sha256:"));
    }
}