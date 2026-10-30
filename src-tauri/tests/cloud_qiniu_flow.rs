//! Explicit, opt-in Qiniu S3 conditional-write acceptance test.
//!
//! This test is intentionally ignored: it performs billable remote I/O.  It uses the
//! object_store S3 client with a counting HTTP connector, Direct routing and no
//! redirects or retries. Each attempt reserves 128 KiB of application body budget;
//! responses are limited to 64 KiB. These are not TLS/billing byte measurements.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use bytes::Bytes;
use futures_util::{StreamExt, TryStreamExt};
use http_body_util::{BodyExt, Limited};
use object_store::aws::AmazonS3Builder;
use object_store::client::{
    HttpClient, HttpConnector, HttpError, HttpErrorKind, HttpRequest, HttpResponse,
    HttpResponseBody, HttpService,
};
use object_store::path::Path;
use object_store::{
    GetOptions, GetRange, ObjectStore, PutMode, PutOptions, PutPayload, RetryConfig, UpdateVersion,
};
use rand::{Rng, distributions::Alphanumeric};
use reqwest::Client;
use zeroize::Zeroizing;

const ENDPOINT: &str = "https://s3.cn-east-1.qiniucs.com";
const BUCKET: &str = "peerdav";
const REGION: &str = "cn-east-1";
const MAX_REQUESTS: usize = 500;
const MAX_BYTES: usize = 100 * 1024 * 1024;
const MAX_TEST_TIME: Duration = Duration::from_secs(180);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const PROBE_SIZE: usize = 1024;
const MAX_WIRE_CHUNK: usize = 64 * 1024;

#[derive(Debug)]
struct BudgetState {
    requests: usize,
    bytes: usize,
}

#[derive(Clone, Debug)]
struct BudgetService {
    client: Client,
    state: Arc<Mutex<BudgetState>>,
}

impl BudgetService {
    fn reserve(&self) -> Result<(), HttpError> {
        let mut state = self.state.lock().expect("budget mutex poisoned");
        if state.requests >= MAX_REQUESTS {
            return Err(HttpError::new(
                HttpErrorKind::Unknown,
                std::io::Error::other("request budget exceeded"),
            ));
        }
        let bytes = state.bytes.checked_add(2 * MAX_WIRE_CHUNK).ok_or_else(|| {
            HttpError::new(
                HttpErrorKind::Unknown,
                std::io::Error::other("byte budget overflow"),
            )
        })?;
        if bytes > MAX_BYTES {
            return Err(HttpError::new(
                HttpErrorKind::Unknown,
                std::io::Error::other("byte budget exceeded"),
            ));
        }
        state.requests += 1;
        state.bytes = bytes;
        Ok(())
    }
}

#[async_trait]
impl HttpService for BudgetService {
    async fn call(&self, req: HttpRequest) -> Result<HttpResponse, HttpError> {
        if req.uri().scheme_str() != Some("https")
            || req.uri().host() != Some("s3.cn-east-1.qiniucs.com")
            || !matches!(req.method().as_str(), "PUT" | "GET" | "HEAD")
        {
            return Err(HttpError::new(
                HttpErrorKind::Unknown,
                std::io::Error::other("unexpected origin or method"),
            ));
        }
        if req.method() == http::Method::PUT
            && req.headers().contains_key(http::header::IF_NONE_MATCH)
        {
            assert_eq!(
                req.headers()
                    .get(http::header::IF_NONE_MATCH)
                    .and_then(|value| value.to_str().ok()),
                Some("*")
            );
            eprintln!("Qiniu request condition: If-None-Match: *");
        }
        self.reserve()?;
        let (parts, body) = req.into_parts();
        if parts
            .headers
            .get(http::header::CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<usize>().ok())
            .is_some_and(|size| size > MAX_WIRE_CHUNK)
        {
            return Err(HttpError::new(
                HttpErrorKind::Unknown,
                std::io::Error::other("request body exceeds per-request limit"),
            ));
        }
        let req = HttpRequest::from_parts(parts, body);
        let response = HttpService::call(&self.client, req).await?;
        eprintln!("Qiniu HTTP response status={}", response.status().as_u16());
        let (parts, body) = response.into_parts();
        let body = Limited::new(body, MAX_WIRE_CHUNK)
            .map_err(|error| HttpError::new_boxed(HttpErrorKind::Decode, error));
        Ok(HttpResponse::from_parts(parts, HttpResponseBody::new(body)))
    }
}

impl HttpConnector for BudgetService {
    fn connect(&self, _: &object_store::ClientOptions) -> object_store::Result<HttpClient> {
        Ok(HttpClient::new(self.clone()))
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "explicit remote Qiniu acceptance run only"]
async fn qiniu_random_prefix_conditional_acceptance() {
    let started = Instant::now();
    let access_key = Zeroizing::new(
        std::env::var("PEERDAV_QINIU_ACCESS_KEY")
            .expect("PEERDAV_QINIU_ACCESS_KEY must be injected by cargo"),
    );
    let secret_key = Zeroizing::new(
        std::env::var("PEERDAV_QINIU_SECRET_KEY")
            .expect("PEERDAV_QINIU_SECRET_KEY must be injected by cargo"),
    );
    let payload = vec![0x50_u8; PROBE_SIZE];
    let prefix: String = rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(24)
        .map(char::from)
        .collect();
    let key = Path::from(format!("peerdav-validation/{prefix}/probe.bin"));
    eprintln!("qiniu acceptance prefix=peerdav-validation/{prefix}/");

    let client = Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(REQUEST_TIMEOUT)
        .build()
        .expect("build Qiniu HTTP client");
    let state = Arc::new(Mutex::new(BudgetState {
        requests: 0,
        bytes: 0,
    }));
    let transport = BudgetService {
        client,
        state: Arc::clone(&state),
    };
    let store = AmazonS3Builder::new()
        .with_endpoint(ENDPOINT)
        .with_bucket_name(BUCKET)
        .with_region(REGION)
        .with_access_key_id(access_key.as_str())
        .with_secret_access_key(secret_key.as_str())
        .with_virtual_hosted_style_request(false)
        .with_retry(RetryConfig {
            max_retries: 0,
            retry_timeout: REQUEST_TIMEOUT,
            ..RetryConfig::default()
        })
        .with_http_connector(transport)
        .build()
        .expect("build Qiniu S3 client");

    // The transport wrapper enforces a hard per-attempt wire budget. The local counters
    // retain the operation-level accounting used in the acceptance assertions.
    let mut requests = 0usize;
    let mut accounted_bytes = 0usize;
    let mut account = |n: usize| {
        requests = requests.checked_add(1).expect("request counter overflow");
        accounted_bytes = accounted_bytes
            .checked_add(n)
            .expect("byte counter overflow");
        assert!(requests <= MAX_REQUESTS, "request budget exceeded");
        assert!(
            accounted_bytes <= MAX_BYTES,
            "application byte budget exceeded"
        );
        assert!(
            started.elapsed() <= MAX_TEST_TIME,
            "acceptance time budget exceeded"
        );
    };

    // Create-only is required: a pre-existing key is a hard failure, never reused.
    account(payload.len());
    let created = tokio::time::timeout(
        REQUEST_TIMEOUT,
        store.put_opts(
            &key,
            PutPayload::from(Bytes::from(payload.clone())),
            PutOptions {
                mode: PutMode::Create,
                ..PutOptions::default()
            },
        ),
    )
    .await
    .expect("Qiniu create-only request timed out")
    .expect("Qiniu create-only request failed");

    // A second create-only attempt must fail, proving the operation is not an overwrite.
    account(payload.len());
    let duplicate = tokio::time::timeout(
        REQUEST_TIMEOUT,
        store.put_opts(
            &key,
            PutPayload::from(Bytes::from(payload.clone())),
            PutOptions {
                mode: PutMode::Create,
                ..PutOptions::default()
            },
        ),
    )
    .await
    .expect("Qiniu duplicate create request timed out");
    assert!(
        matches!(
            duplicate,
            Err(object_store::Error::Precondition { .. }
                | object_store::Error::AlreadyExists { .. })
        ),
        "duplicate create must return a precondition error, not a transport failure"
    );

    let etag = created
        .e_tag
        .expect("create response did not include an ETag");
    let stale = format!("{etag}-stale");
    account(payload.len());
    let stale_result = tokio::time::timeout(
        REQUEST_TIMEOUT,
        store.put_opts(
            &key,
            PutPayload::from(Bytes::from(payload.clone())),
            PutOptions {
                mode: PutMode::Update(UpdateVersion {
                    e_tag: Some(stale),
                    version: None,
                }),
                ..PutOptions::default()
            },
        ),
    )
    .await
    .expect("Qiniu stale If-Match request timed out");
    assert!(
        matches!(stale_result, Err(object_store::Error::Precondition { .. })),
        "stale If-Match must return a precondition error"
    );
    let update = |value: &'static [u8]| {
        store.put_opts(
            &key,
            PutPayload::from(Bytes::from_static(value)),
            PutOptions {
                mode: PutMode::Update(UpdateVersion {
                    e_tag: Some(etag.clone()),
                    version: None,
                }),
                ..PutOptions::default()
            },
        )
    };
    let (left, right) = tokio::join!(update(b"left"), update(b"right"));
    assert!(
        matches!(
            (&left, &right),
            (Ok(_), Err(object_store::Error::Precondition { .. }))
                | (Err(object_store::Error::Precondition { .. }), Ok(_))
        ),
        "concurrent matching ETag writes must have exactly one winner"
    );
    let expected = if left.is_ok() {
        b"left".as_slice()
    } else {
        b"right".as_slice()
    };

    // A bounded range read validates the object without collecting an unbounded body.
    account(PROBE_SIZE);
    let object = tokio::time::timeout(
        REQUEST_TIMEOUT,
        store.get_opts(
            &key,
            GetOptions {
                range: Some(GetRange::Bounded(0..PROBE_SIZE as u64)),
                ..GetOptions::default()
            },
        ),
    )
    .await
    .expect("Qiniu range GET timed out")
    .expect("Qiniu range GET failed");
    let got = object
        .into_stream()
        .take(PROBE_SIZE)
        .try_fold(
            Vec::with_capacity(PROBE_SIZE),
            |mut out, chunk| async move {
                out.extend_from_slice(&chunk);
                if out.len() > PROBE_SIZE {
                    return Err(object_store::Error::Generic {
                        store: "qiniu test",
                        source: "range exceeded probe limit".into(),
                    });
                }
                Ok(out)
            },
        )
        .await
        .expect("Qiniu range response exceeded probe limit");
    assert_eq!(got, expected);

    // One bounded prefix-list item is enough to validate visibility; do not drain pages.
    account(0);
    let listed = store.list(Some(&Path::from(format!("peerdav-validation/{prefix}"))));
    let first = listed.take(1).try_next().await.expect("Qiniu list failed");
    assert_eq!(first.map(|meta| meta.location), Some(key));
    let budget = state.lock().expect("budget mutex");
    eprintln!(
        "Qiniu actual HTTP attempts={}, conservative application body reservation={} bytes; no DELETE",
        budget.requests, budget.bytes
    );
    assert!(started.elapsed() <= MAX_TEST_TIME);
}
