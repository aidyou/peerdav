//! Opt-in integration coverage against a real Moto S3 HTTP server.
//!
//! Set `PEERDAV_S3_EXTERNAL_MOTO=1` to run.  The default invocation is a
//! deliberate no-op so normal test runs never require Python or credentials.
use bytes::Bytes;
use futures_util::{StreamExt, TryStreamExt, stream};
use peerdav_lib::cloud::{
    S3Store,
    traits::ObjectStore,
    types::{CloudError, ObjectStream, ProxyMode, PutCondition, S3Config},
};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::Arc;

const ACCESS: &str = "peerdav-test-access";
const SECRET: &str = "peerdav-test-secret";
const BUCKET: &str = "peerdav-external-test";

struct MotoServer {
    child: Child,
    keep_alive: Option<ChildStdin>,
    endpoint: String,
}

impl MotoServer {
    fn start() -> Result<Self, String> {
        let pythonpath = "/tmp/peerdav-s3-minimal-packages";
        let python = std::env::var("PEERDAV_PYTHON").unwrap_or_else(|_| "python3".to_owned());
        let script = r#"
import sys
sys.path.insert(0, "/tmp/peerdav-s3-minimal-packages")
import time
import boto3
from moto.server import ThreadedMotoServer
server = ThreadedMotoServer(ip_address="127.0.0.1", port=0, verbose=False)
server.start()
try:
    host, port = server.get_host_and_port()
    endpoint = f"http://{host}:{port}"
    client = boto3.client("s3", endpoint_url=endpoint, region_name="us-east-1",
                          aws_access_key_id="peerdav-test-access",
                          aws_secret_access_key="peerdav-test-secret")
    for attempt in range(30):
        try:
            client.create_bucket(Bucket="peerdav-external-test")
            break
        except Exception:
            if attempt == 29:
                raise
            time.sleep(0.1)
    print(f"READY {endpoint}", flush=True)
    sys.stdin.read()
finally:
    server.stop()
"#;
        let mut child = Command::new(python)
            .env("PYTHONPATH", pythonpath)
            .arg("-c")
            .arg(script)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| format!("start moto: {e}"))?;
        let stdout = child.stdout.take().ok_or("moto stdout unavailable")?;
        let mut lines = BufReader::new(stdout).lines();
        let line = lines
            .next()
            .ok_or_else(|| "moto exited before readiness".to_owned())
            .and_then(|x| x.map_err(|e| format!("read moto readiness: {e}")))?;
        let endpoint = line
            .strip_prefix("READY ")
            .ok_or_else(|| format!("unexpected moto readiness: {line}"))?
            .to_owned();
        if !endpoint.starts_with("http://127.0.0.1:") {
            return Err(format!("moto endpoint is not loopback HTTP: {endpoint}"));
        }
        let mut keep_alive = child.stdin.take().ok_or("moto stdin unavailable")?;
        keep_alive
            .flush()
            .map_err(|e| format!("keep moto alive: {e}"))?;
        // Keep the reader alive by transferring it to a thread.  Without this,
        // the child's stdout pipe can fill and stall a long pagination run.
        std::thread::spawn(move || for _ in lines {});
        Ok(Self {
            child,
            keep_alive: Some(keep_alive),
            endpoint,
        })
    }
}

impl Drop for MotoServer {
    fn drop(&mut self) {
        if let Some(mut keep_alive) = self.keep_alive.take() {
            let _ = keep_alive.flush();
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn body(data: impl Into<Bytes>) -> ObjectStream {
    let data = data.into();
    stream::once(async move { Ok(data) }).boxed()
}

fn store(endpoint: &str, prefix: &str) -> S3Store {
    S3Store::new(S3Config {
        endpoint: endpoint.to_owned(),
        bucket: BUCKET.to_owned(),
        region: "us-east-1".to_owned(),
        prefix: prefix.to_owned(),
        path_style: true,
        access_key: ACCESS.to_owned(),
        secret_key: SECRET.to_owned(),
        session_token: None,
        buckets: Vec::new(),
        allow_insecure_http: true,
        proxy: ProxyMode::Direct,
    })
    .expect("valid loopback S3 config")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cloud_external_moto_crud_cas_range_pagination_and_probe() {
    if std::env::var_os("PEERDAV_S3_EXTERNAL_MOTO").is_none() {
        eprintln!("external S3 test not configured; set PEERDAV_S3_EXTERNAL_MOTO=1");
        return;
    }
    let moto = MotoServer::start().expect("Moto 5.2.5 server");
    let isolated = store(&moto.endpoint, "external-isolated/");
    let second = store(&moto.endpoint, "external-other/");

    isolated
        .put(
            "folder/object",
            body("0123456789"),
            PutCondition::CreateOnly,
        )
        .await
        .expect("create object");
    assert_eq!(
        isolated
            .get("folder/object", Some(2..6))
            .await
            .expect("range GET")
            .try_collect::<Vec<_>>()
            .await
            .expect("range stream")
            .concat(),
        b"2345"
    );
    assert_eq!(isolated.stat("folder/object").await.expect("HEAD").size, 10);
    let first = isolated
        .stat("folder/object")
        .await
        .expect("metadata")
        .etag
        .expect("provider etag");
    assert!(matches!(
        isolated
            .put("folder/object", body("duplicate"), PutCondition::CreateOnly)
            .await,
        Err(CloudError::Precondition)
    ));
    isolated
        .put(
            "folder/object",
            body("replacement"),
            PutCondition::MatchEtag(first.clone()),
        )
        .await
        .expect("matching ETag replacement");
    assert!(matches!(
        isolated
            .put(
                "folder/object",
                body("stale"),
                PutCondition::MatchEtag(first)
            )
            .await,
        Err(CloudError::Precondition)
    ));

    for i in 0..1020 {
        isolated
            .put(&format!("bulk/{i:04}"), body("x"), PutCondition::Any)
            .await
            .expect("bulk object");
    }
    let listed = isolated.list("bulk/").collect::<Vec<_>>().await;
    assert_eq!(
        listed.len(),
        1020,
        "S3 pagination must include every object"
    );
    second
        .put(
            "folder/object",
            body("other-prefix"),
            PutCondition::CreateOnly,
        )
        .await
        .expect("other prefix object");
    assert_eq!(isolated.list("").collect::<Vec<_>>().await.len(), 1021);
    isolated
        .delete("folder/object")
        .await
        .expect("delete object");
    assert!(isolated.stat("folder/object").await.is_err());

    let current = isolated
        .stat("bulk/0000")
        .await
        .expect("CAS seed metadata")
        .etag
        .expect("CAS etag");
    let left = Arc::new(isolated);
    let right = Arc::clone(&left);
    let (a, b) = tokio::join!(
        left.put(
            "bulk/0000",
            body("writer-a"),
            PutCondition::MatchEtag(current.clone())
        ),
        right.put(
            "bulk/0000",
            body("writer-b"),
            PutCondition::MatchEtag(current)
        ),
    );
    assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
    assert!(
        left.verify_capabilities()
            .await
            .expect("provider capability probe")
            .conditional_writes_verified
    );
}
