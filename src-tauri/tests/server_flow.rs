//! U-2 integration tests: authentication/challenge, approval and revocation,
//! network policy, per-share permissions, scoped filesystem, WebDAV
//! read/write/delete, and server lifecycle.

use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use http::header::{AUTHORIZATION, HOST, WWW_AUTHENTICATE};
use http::{HeaderMap, Method, Request, StatusCode};
use http_body_util::{BodyExt, Full};
use hyper_util::rt::TokioIo;
use peerdav_lib::{
    AccessRule, AppError, DomainError, MAX_PENDING_ATTEMPTS, MAX_PENDING_PATH_CHARS,
    MAX_PENDING_REQUESTS, PendingRequestInfo, RuleKind, RuleMatcher, ServerConfig,
    ServerController, ServerEvents, SharePermissions, Vault,
};
use tokio::net::TcpStream;

mod common;

use common::{DevTestDir, dev_test_dir, key_dir};

const ADMIN_USERNAME: &str = "peerdav-admin";
const ADMIN_PASSWORD: &str = "admin password server 2026";
const CLIENT_USERNAME: &str = "peerdav";
const AUTH_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Default)]
struct Recording {
    errors: Mutex<Vec<AppError>>,
    pending: Mutex<Vec<PendingRequestInfo>>,
}

impl ServerEvents for Recording {
    fn error(&self, error: &AppError) {
        self.errors.lock().expect("errors").push(error.clone());
    }

    fn pending_request(&self, request: &PendingRequestInfo) {
        self.pending.lock().expect("pending").push(request.clone());
    }
}

struct Harness {
    directory: DevTestDir,
    vault: Arc<Vault>,
    controller: Arc<ServerController>,
    events: Arc<Recording>,
    share_id: String,
    address: SocketAddr,
}

impl Harness {
    async fn new(permissions: SharePermissions, rules: Vec<AccessRule>) -> Self {
        let directory = dev_test_dir("server_flow");
        let vault =
            Arc::new(Vault::open(&directory.path().join("peerdav.pd")).expect("open vault"));
        vault
            .initialize(&key_dir(directory.path()), ADMIN_USERNAME, ADMIN_PASSWORD)
            .expect("initialize");

        let events = Arc::new(Recording::default());
        let controller = Arc::new(
            ServerController::new(Arc::clone(&vault), events.clone()).expect("controller"),
        );

        let share_root = directory.path().join("share");
        std::fs::create_dir(&share_root).expect("share root");
        let share = controller
            .create_share(
                "Test share",
                share_root.to_str().expect("utf8 path"),
                permissions,
                &rules,
            )
            .expect("create share");

        let status = controller
            .start(ServerConfig {
                bind_address: "127.0.0.1".to_owned(),
                port: 0,
                ..ServerConfig::default()
            })
            .await
            .expect("start server");
        let address = status
            .bind_address
            .expect("bound address")
            .parse()
            .expect("socket addr");

        Self {
            directory,
            vault,
            controller,
            events,
            share_id: share.id,
            address,
        }
    }

    fn share_path(&self, relative: &str) -> String {
        if relative.is_empty() {
            format!("/share/{}/", self.share_id)
        } else {
            format!("/share/{}/{}", self.share_id, relative)
        }
    }

    fn share_root(&self) -> std::path::PathBuf {
        self.directory.path().join("share")
    }

    async fn request(
        &self,
        method: Method,
        path: &str,
        token: Option<&str>,
        body: &[u8],
    ) -> HttpResponse {
        send(self.address, method, path, token, body).await
    }

    async fn request_with_headers(
        &self,
        method: Method,
        path: &str,
        token: Option<&str>,
        body: &[u8],
        headers: &[(&'static str, String)],
    ) -> HttpResponse {
        send_with_headers(self.address, method, path, token, body, headers).await
    }

    /// Triggers a pending request and approves it, returning the one-time code.
    async fn issue_code(&self) -> String {
        let response = self
            .request(Method::GET, &self.share_path("missing.txt"), None, b"")
            .await;
        assert_eq!(response.status, StatusCode::UNAUTHORIZED);

        let pending = self.controller.list_pending();
        assert!(!pending.is_empty(), "pending request must be recorded");
        let request = pending
            .iter()
            .find(|request| request.share_id == self.share_id)
            .expect("pending request for the default share");
        let approved = self
            .controller
            .approve_pending(&request.id)
            .expect("approve pending");
        assert_eq!(approved.authorization_code.len(), 64);
        assert_eq!(self.controller.list_pending().len(), pending.len() - 1);
        approved.authorization_code
    }

    async fn shutdown(&self) {
        self.controller.stop().await;
    }
}

struct HttpResponse {
    status: StatusCode,
    headers: HeaderMap,
    body: Vec<u8>,
}

async fn send(
    address: SocketAddr,
    method: Method,
    path: &str,
    token: Option<&str>,
    body: &[u8],
) -> HttpResponse {
    send_with_headers(address, method, path, token, body, &[]).await
}

async fn send_with_headers(
    address: SocketAddr,
    method: Method,
    path: &str,
    token: Option<&str>,
    body: &[u8],
    extra_headers: &[(&'static str, String)],
) -> HttpResponse {
    let stream = tokio::time::timeout(AUTH_TIMEOUT, TcpStream::connect(address))
        .await
        .expect("connect timeout")
        .expect("connect");
    let io = TokioIo::new(stream);
    let (mut sender, connection) =
        tokio::time::timeout(AUTH_TIMEOUT, hyper::client::conn::http1::handshake(io))
            .await
            .expect("handshake timeout")
            .expect("handshake");
    let connection_task = tokio::spawn(async move {
        if let Err(error) = connection.await {
            eprintln!("client connection closed: {error}");
        }
    });

    let mut builder = Request::builder()
        .method(method)
        .uri(path)
        .header(HOST, address.to_string());
    if let Some(token) = token {
        builder = builder.header(AUTHORIZATION, basic_auth(token));
    }
    for (name, value) in extra_headers {
        builder = builder.header(*name, value);
    }
    let request = builder
        .body(Full::new(Bytes::from(body.to_vec())))
        .expect("build request");

    let response = tokio::time::timeout(AUTH_TIMEOUT, sender.send_request(request))
        .await
        .expect("response timeout")
        .expect("response");
    let status = response.status();
    let headers = response.headers().clone();
    let body = tokio::time::timeout(AUTH_TIMEOUT, response.into_body().collect())
        .await
        .expect("body timeout")
        .expect("collect body")
        .to_bytes()
        .to_vec();
    connection_task.abort();

    HttpResponse {
        status,
        headers,
        body,
    }
}

fn basic_auth(token: &str) -> http::HeaderValue {
    use base64::Engine as _;

    let encoded =
        base64::engine::general_purpose::STANDARD.encode(format!("{CLIENT_USERNAME}:{token}"));
    http::HeaderValue::from_str(&format!("Basic {encoded}")).expect("basic auth header")
}

fn loopback_rule() -> AccessRule {
    AccessRule {
        kind: RuleKind::Allow,
        matcher: RuleMatcher::Ip("127.0.0.1".to_owned()),
    }
}

fn full_permissions() -> SharePermissions {
    SharePermissions {
        can_read: true,
        can_write: true,
        can_delete: true,
    }
}

fn read_only() -> SharePermissions {
    SharePermissions {
        can_read: true,
        can_write: false,
        can_delete: false,
    }
}

fn write_only() -> SharePermissions {
    SharePermissions {
        can_read: false,
        can_write: true,
        can_delete: false,
    }
}

fn delete_only() -> SharePermissions {
    SharePermissions {
        can_read: false,
        can_write: false,
        can_delete: true,
    }
}

fn read_write_no_delete() -> SharePermissions {
    SharePermissions {
        can_read: true,
        can_write: true,
        can_delete: false,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn authentication_challenge_approval_and_webdav_crud() {
    let harness = Harness::new(full_permissions(), vec![loopback_rule()]).await;

    let unauthenticated = harness
        .request(Method::GET, &harness.share_path("hello.txt"), None, b"")
        .await;
    assert_eq!(unauthenticated.status, StatusCode::UNAUTHORIZED);
    let challenge = unauthenticated
        .headers
        .get(WWW_AUTHENTICATE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    assert!(challenge.starts_with("Basic "));

    let pending = harness.controller.list_pending();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].peer_ip, "127.0.0.1");
    assert_eq!(pending[0].share_id, harness.share_id);
    assert_eq!(harness.events.pending.lock().expect("pending").len(), 1);

    let approved = harness
        .controller
        .approve_pending(&pending[0].id)
        .expect("approve");
    assert_eq!(approved.authorization_code.len(), 64);
    assert!(harness.controller.list_pending().is_empty());
    assert!(matches!(
        harness.controller.approve_pending(&pending[0].id),
        Err(DomainError::PendingRequestNotFound { .. })
    ));
    let code = approved.authorization_code;

    let put = harness
        .request(
            Method::PUT,
            &harness.share_path("hello.txt"),
            Some(&code),
            b"hello dav",
        )
        .await;
    assert!(
        put.status.is_success(),
        "PUT should succeed, got {}",
        put.status
    );

    let get = harness
        .request(
            Method::GET,
            &harness.share_path("hello.txt"),
            Some(&code),
            b"",
        )
        .await;
    assert_eq!(get.status, StatusCode::OK);
    assert_eq!(get.body, b"hello dav");

    let propfind = harness
        .request(
            Method::from_bytes(b"PROPFIND").expect("PROPFIND"),
            &harness.share_path(""),
            Some(&code),
            b"",
        )
        .await;
    assert_eq!(propfind.status, StatusCode::MULTI_STATUS);

    let delete = harness
        .request(
            Method::DELETE,
            &harness.share_path("hello.txt"),
            Some(&code),
            b"",
        )
        .await;
    assert!(
        delete.status.is_success(),
        "DELETE should succeed, got {}",
        delete.status
    );
    assert!(!harness.share_root().join("hello.txt").exists());

    let missing = harness
        .request(
            Method::GET,
            &harness.share_path("hello.txt"),
            Some(&code),
            b"",
        )
        .await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);

    // Revocation invalidates the code and produces a fresh pending request.
    let authorization_id = harness.controller.list_authorizations()[0].id.clone();
    harness
        .controller
        .revoke_authorization(&authorization_id)
        .expect("revoke");
    let after_revoke = harness
        .request(
            Method::GET,
            &harness.share_path("hello.txt"),
            Some(&code),
            b"",
        )
        .await;
    assert_eq!(after_revoke.status, StatusCode::UNAUTHORIZED);
    assert_eq!(harness.controller.list_pending().len(), 1);

    harness.shutdown().await;
    drop(harness.directory);
}

#[tokio::test(flavor = "multi_thread")]
async fn webdav_collections_move_and_copy_are_scoped() {
    let harness = Harness::new(full_permissions(), vec![loopback_rule()]).await;
    let code = harness.issue_code().await;

    let mkcol = harness
        .request_with_headers(
            Method::from_bytes(b"MKCOL").expect("MKCOL"),
            &harness.share_path("newdir/"),
            Some(&code),
            b"",
            &[],
        )
        .await;
    assert!(
        mkcol.status.is_success(),
        "MKCOL failed with {}",
        mkcol.status
    );
    assert!(harness.share_root().join("newdir").is_dir());

    assert!(
        harness
            .request(
                Method::PUT,
                &harness.share_path("source.txt"),
                Some(&code),
                b"move me"
            )
            .await
            .status
            .is_success()
    );

    let moved_destination = format!(
        "http://{}/share/{}/newdir/moved.txt",
        harness.address, harness.share_id
    );
    let moved = harness
        .request_with_headers(
            Method::from_bytes(b"MOVE").expect("MOVE"),
            &harness.share_path("source.txt"),
            Some(&code),
            b"",
            &[("Destination", moved_destination)],
        )
        .await;
    assert!(
        moved.status.is_success(),
        "MOVE failed with {}",
        moved.status
    );
    assert!(!harness.share_root().join("source.txt").exists());
    assert_eq!(
        std::fs::read(harness.share_root().join("newdir/moved.txt")).expect("moved file"),
        b"move me"
    );

    let copied_destination = format!(
        "http://{}/share/{}/newdir/copied.txt",
        harness.address, harness.share_id
    );
    let copied = harness
        .request_with_headers(
            Method::from_bytes(b"COPY").expect("COPY"),
            &harness.share_path("newdir/moved.txt"),
            Some(&code),
            b"",
            &[("Destination", copied_destination)],
        )
        .await;
    assert!(
        copied.status.is_success(),
        "COPY failed with {}",
        copied.status
    );
    assert_eq!(
        harness
            .request(
                Method::GET,
                &harness.share_path("newdir/copied.txt"),
                Some(&code),
                b"",
            )
            .await
            .body,
        b"move me"
    );

    let deleted = harness
        .request(
            Method::DELETE,
            &harness.share_path("newdir/"),
            Some(&code),
            b"",
        )
        .await;
    assert!(
        deleted.status.is_success(),
        "recursive DELETE failed with {}",
        deleted.status
    );
    assert!(!harness.share_root().join("newdir").exists());

    harness.shutdown().await;
    drop(harness.directory);
}

#[tokio::test(flavor = "multi_thread")]
async fn network_policy_denies_empty_allow_and_deny_rules_win() {
    let harness = Harness::new(full_permissions(), vec![loopback_rule()]).await;
    let code = harness.issue_code().await;

    // Empty allow list => deny.
    let empty = harness
        .controller
        .create_share(
            "Empty rules",
            harness.share_root().to_str().expect("path"),
            full_permissions(),
            &[],
        )
        .expect("empty share");
    let response = harness
        .request(
            Method::GET,
            &format!("/share/{}/file.txt", empty.id),
            Some(&code),
            b"",
        )
        .await;
    assert_eq!(response.status, StatusCode::FORBIDDEN);
    assert!(
        harness
            .controller
            .list_pending()
            .iter()
            .all(|request| request.share_id != empty.id),
        "a network-denied peer must not become a pending approval"
    );

    // Allow rule for a different network => deny.
    let remote_allow = harness
        .controller
        .create_share(
            "Remote allow",
            harness.share_root().to_str().expect("path"),
            full_permissions(),
            &[AccessRule {
                kind: RuleKind::Allow,
                matcher: RuleMatcher::Network("10.0.0.0/8".to_owned()),
            }],
        )
        .expect("remote share");
    let denied = harness
        .request(
            Method::GET,
            &format!("/share/{}/file.txt", remote_allow.id),
            Some(&code),
            b"",
        )
        .await;
    assert_eq!(denied.status, StatusCode::FORBIDDEN);

    // Deny wins over a matching allow, even with a valid authorization code.
    let denied_share = harness
        .controller
        .create_share(
            "Deny wins",
            harness.share_root().to_str().expect("path"),
            full_permissions(),
            &[
                loopback_rule(),
                AccessRule {
                    kind: RuleKind::Deny,
                    matcher: RuleMatcher::Ip("127.0.0.1".to_owned()),
                },
            ],
        )
        .expect("deny share");
    let denied = harness
        .request(
            Method::GET,
            &format!("/share/{}/file.txt", denied_share.id),
            Some(&code),
            b"",
        )
        .await;
    assert_eq!(denied.status, StatusCode::FORBIDDEN);

    // The same code still works on the allowed share.
    let allowed = harness
        .request(
            Method::GET,
            &harness.share_path("missing.txt"),
            Some(&code),
            b"",
        )
        .await;
    assert_eq!(allowed.status, StatusCode::NOT_FOUND);

    harness.shutdown().await;
    drop(harness.directory);
}

#[tokio::test(flavor = "multi_thread")]
async fn permissions_are_enforced_per_share() {
    let harness = Harness::new(full_permissions(), vec![loopback_rule()]).await;
    let code = harness.issue_code().await;

    std::fs::write(harness.share_root().join("readable.txt"), b"read me").expect("seed file");

    let read_share = harness
        .controller
        .create_share(
            "Read only",
            harness.share_root().to_str().expect("path"),
            read_only(),
            &[loopback_rule()],
        )
        .expect("read share");
    let read_path = |name: &str| format!("/share/{}/{}", read_share.id, name);

    assert_eq!(
        harness
            .request(Method::GET, &read_path("readable.txt"), Some(&code), b"")
            .await
            .status,
        StatusCode::OK
    );
    assert_eq!(
        harness
            .request(
                Method::from_bytes(b"PROPFIND").expect("PROPFIND"),
                &read_path(""),
                Some(&code),
                b""
            )
            .await
            .status,
        StatusCode::MULTI_STATUS
    );
    assert_eq!(
        harness
            .request(Method::PUT, &read_path("write.txt"), Some(&code), b"nope")
            .await
            .status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        harness
            .request(Method::DELETE, &read_path("readable.txt"), Some(&code), b"")
            .await
            .status,
        StatusCode::FORBIDDEN
    );
    assert!(harness.share_root().join("readable.txt").exists());

    let write_share = harness
        .controller
        .create_share(
            "Write only",
            harness.share_root().to_str().expect("path"),
            write_only(),
            &[loopback_rule()],
        )
        .expect("write share");
    let write_path = |name: &str| format!("/share/{}/{}", write_share.id, name);
    assert_eq!(
        harness
            .request(
                Method::PUT,
                &write_path("upload.txt"),
                Some(&code),
                b"uploaded"
            )
            .await
            .status,
        StatusCode::CREATED
    );
    assert_eq!(
        std::fs::read(harness.share_root().join("upload.txt")).expect("uploaded file"),
        b"uploaded"
    );
    assert_eq!(
        harness
            .request(Method::GET, &write_path("upload.txt"), Some(&code), b"")
            .await
            .status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        harness
            .request(Method::DELETE, &write_path("upload.txt"), Some(&code), b"")
            .await
            .status,
        StatusCode::FORBIDDEN
    );

    // Overwriting an existing file must work with write permission only.
    let overwrite = harness
        .request(
            Method::PUT,
            &write_path("upload.txt"),
            Some(&code),
            b"replaced",
        )
        .await;
    assert!(
        overwrite.status.is_success(),
        "write-only overwrite failed with {}",
        overwrite.status
    );
    assert_eq!(
        std::fs::read(harness.share_root().join("upload.txt")).expect("overwritten file"),
        b"replaced"
    );

    // Delete permission is usable on its own; read methods stay blocked.
    std::fs::write(harness.share_root().join("deletable.txt"), b"bye").expect("seed delete file");
    let delete_share = harness
        .controller
        .create_share(
            "Delete only",
            harness.share_root().to_str().expect("path"),
            delete_only(),
            &[loopback_rule()],
        )
        .expect("delete share");
    let delete_path = |name: &str| format!("/share/{}/{}", delete_share.id, name);
    assert_eq!(
        harness
            .request(Method::GET, &delete_path("deletable.txt"), Some(&code), b"")
            .await
            .status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        harness
            .request(
                Method::from_bytes(b"PROPFIND").expect("PROPFIND"),
                &delete_path(""),
                Some(&code),
                b"",
            )
            .await
            .status,
        StatusCode::FORBIDDEN
    );
    let deleted = harness
        .request(
            Method::DELETE,
            &delete_path("deletable.txt"),
            Some(&code),
            b"",
        )
        .await;
    assert!(
        deleted.status.is_success(),
        "delete-only DELETE failed with {}",
        deleted.status
    );
    assert!(!harness.share_root().join("deletable.txt").exists());

    // Recursive delete of a non-empty directory with delete permission only.
    let deleted_dir = harness.share_root().join("deletedir");
    std::fs::create_dir(&deleted_dir).expect("delete dir");
    std::fs::write(deleted_dir.join("child.txt"), b"child").expect("child file");
    let recursive = harness
        .request(Method::DELETE, &delete_path("deletedir/"), Some(&code), b"")
        .await;
    assert!(
        recursive.status.is_success(),
        "delete-only recursive DELETE failed with {}",
        recursive.status
    );
    assert!(!deleted_dir.exists());

    let no_delete_share = harness
        .controller
        .create_share(
            "Read write",
            harness.share_root().to_str().expect("path"),
            read_write_no_delete(),
            &[loopback_rule()],
        )
        .expect("rw share");
    let rw_path = |name: &str| format!("/share/{}/{}", no_delete_share.id, name);
    assert!(
        harness
            .request(Method::PUT, &rw_path("rw.txt"), Some(&code), b"rw")
            .await
            .status
            .is_success()
    );
    assert_eq!(
        harness
            .request(Method::GET, &rw_path("rw.txt"), Some(&code), b"")
            .await
            .body,
        b"rw"
    );
    assert_eq!(
        harness
            .request(Method::DELETE, &rw_path("rw.txt"), Some(&code), b"")
            .await
            .status,
        StatusCode::FORBIDDEN
    );
    assert!(harness.share_root().join("rw.txt").exists());

    harness.shutdown().await;
    drop(harness.directory);
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn filesystem_scope_rejects_symlinks_and_traversal() {
    use std::os::unix::fs::symlink;

    let harness = Harness::new(full_permissions(), vec![loopback_rule()]).await;
    let code = harness.issue_code().await;

    let outside = harness.directory.path().join("secret.txt");
    std::fs::write(&outside, b"top secret").expect("outside secret");
    symlink(&outside, harness.share_root().join("link.txt")).expect("file symlink");
    symlink(
        harness.directory.path(),
        harness.share_root().join("linkdir"),
    )
    .expect("dir symlink");

    let file_link = harness
        .request(
            Method::GET,
            &harness.share_path("link.txt"),
            Some(&code),
            b"",
        )
        .await;
    assert_ne!(file_link.status, StatusCode::OK);
    assert_ne!(file_link.body, b"top secret");

    let dir_link = harness
        .request(
            Method::GET,
            &harness.share_path("linkdir/secret.txt"),
            Some(&code),
            b"",
        )
        .await;
    assert_ne!(dir_link.status, StatusCode::OK);
    assert_ne!(dir_link.body, b"top secret");

    let traversal = harness
        .request(
            Method::GET,
            &harness.share_path("../../secret.txt"),
            Some(&code),
            b"",
        )
        .await;
    assert!(!traversal.status.is_success());
    assert_ne!(traversal.body, b"top secret");

    let write_through_link = harness
        .request(
            Method::PUT,
            &harness.share_path("link.txt"),
            Some(&code),
            b"overwritten",
        )
        .await;
    assert!(!write_through_link.status.is_success());
    assert_eq!(
        std::fs::read(&outside).expect("outside secret"),
        b"top secret"
    );

    // A symlinked directory must not be traversable either.
    std::fs::write(harness.directory.path().join("other.txt"), b"other").expect("other");
    let other_link = harness
        .request(
            Method::GET,
            &harness.share_path("linkdir/other.txt"),
            Some(&code),
            b"",
        )
        .await;
    assert_ne!(other_link.status, StatusCode::OK);

    harness.shutdown().await;
    drop(harness.directory);
}

#[tokio::test(flavor = "multi_thread")]
async fn pending_requests_are_deduplicated_and_reported() {
    let harness = Harness::new(full_permissions(), vec![loopback_rule()]).await;

    for _ in 0..3 {
        let response = harness
            .request(Method::GET, &harness.share_path("a.txt"), None, b"")
            .await;
        assert_eq!(response.status, StatusCode::UNAUTHORIZED);
    }

    let pending = harness.controller.list_pending();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].attempts, 3);
    assert_eq!(harness.events.pending.lock().expect("pending").len(), 1);

    let other = harness
        .controller
        .create_share(
            "Other share",
            harness.share_root().to_str().expect("path"),
            full_permissions(),
            &[loopback_rule()],
        )
        .expect("other share");
    let response = harness
        .request(
            Method::GET,
            &format!("/share/{}/a.txt", other.id),
            None,
            b"",
        )
        .await;
    assert_eq!(response.status, StatusCode::UNAUTHORIZED);
    assert_eq!(harness.controller.list_pending().len(), 2);
    assert_eq!(harness.events.pending.lock().expect("pending").len(), 2);

    harness.shutdown().await;
    drop(harness.directory);
}

#[tokio::test(flavor = "multi_thread")]
async fn unauthenticated_pending_requires_discovery_method_and_share_target() {
    let harness = Harness::new(full_permissions(), vec![loopback_rule()]).await;

    // Mutating/unknown methods must never ask for approval.
    for method in [
        Method::POST,
        Method::DELETE,
        Method::from_bytes(b"SEARCH").expect("SEARCH"),
    ] {
        let response = harness
            .request(method, &harness.share_path("x.txt"), None, b"")
            .await;
        assert_eq!(response.status, StatusCode::UNAUTHORIZED);
    }
    let proppatch = harness
        .request(
            Method::from_bytes(b"PROPPATCH").expect("PROPPATCH"),
            &harness.share_path("x.txt"),
            None,
            b"",
        )
        .await;
    assert_eq!(proppatch.status, StatusCode::NOT_IMPLEMENTED);
    assert!(harness.controller.list_pending().is_empty());
    assert!(harness.events.pending.lock().expect("pending").is_empty());

    // Non-share paths, unknown shares, malformed escapes, and traversal
    // targets must not become pending approvals either.
    let outside = harness
        .request(Method::GET, "/not-a-share/file.txt", None, b"")
        .await;
    assert_eq!(outside.status, StatusCode::NOT_FOUND);
    let unknown_share = harness
        .request(Method::GET, "/share/unknown/file.txt", None, b"")
        .await;
    assert_eq!(unknown_share.status, StatusCode::NOT_FOUND);
    let invalid_percent = harness
        .request(
            Method::GET,
            &format!("/share/{}/%zz", harness.share_id),
            None,
            b"",
        )
        .await;
    assert_eq!(invalid_percent.status, StatusCode::BAD_REQUEST);
    let traversal = harness
        .request(
            Method::GET,
            &harness.share_path("../escaped.txt"),
            None,
            b"",
        )
        .await;
    assert_eq!(traversal.status, StatusCode::NOT_FOUND);
    let encoded_traversal = harness
        .request(
            Method::GET,
            &format!("/share/{}/%2e%2e/escaped.txt", harness.share_id),
            None,
            b"",
        )
        .await;
    assert_eq!(encoded_traversal.status, StatusCode::NOT_FOUND);
    let nul_byte = harness
        .request(
            Method::GET,
            &format!("/share/{}/%00", harness.share_id),
            None,
            b"",
        )
        .await;
    assert_eq!(nul_byte.status, StatusCode::BAD_REQUEST);
    assert!(harness.controller.list_pending().is_empty());
    assert!(harness.events.pending.lock().expect("pending").is_empty());

    // The share root and a missing resource are both valid WebDAV targets:
    // the first authorization must still be requested for them.
    let root = harness
        .request(Method::GET, &harness.share_path(""), None, b"")
        .await;
    assert_eq!(root.status, StatusCode::UNAUTHORIZED);
    let propfind = harness
        .request(
            Method::from_bytes(b"PROPFIND").expect("PROPFIND"),
            &harness.share_path("not-created-yet"),
            None,
            b"",
        )
        .await;
    assert_eq!(propfind.status, StatusCode::UNAUTHORIZED);

    let pending = harness.controller.list_pending();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].attempts, 2);
    assert_eq!(pending[0].requested_path, "not-created-yet");
    assert_eq!(harness.events.pending.lock().expect("pending").len(), 1);

    // The catalog only asks for approval on GET.
    let catalog_post = harness
        .request(Method::POST, "/__peerdav/catalog", None, b"")
        .await;
    assert_eq!(catalog_post.status, StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(harness.controller.list_pending().len(), 1);
    let catalog = harness
        .request(Method::GET, "/__peerdav/catalog", None, b"")
        .await;
    assert_eq!(catalog.status, StatusCode::UNAUTHORIZED);
    let pending = harness.controller.list_pending();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].attempts, 3);

    // Approving the first request still authorizes regular access.
    let approved = harness
        .controller
        .approve_pending(&pending[0].id)
        .expect("approve pending");
    let authorized = harness
        .request(
            Method::GET,
            &harness.share_path("missing.txt"),
            Some(&approved.authorization_code),
            b"",
        )
        .await;
    assert_eq!(authorized.status, StatusCode::NOT_FOUND);
    assert!(harness.controller.list_pending().is_empty());

    harness.shutdown().await;
    drop(harness.directory);
}

#[tokio::test(flavor = "multi_thread")]
async fn repeated_unauthenticated_attempts_are_bounded() {
    let harness = Harness::new(full_permissions(), vec![loopback_rule()]).await;

    for index in 0..(MAX_PENDING_ATTEMPTS + 10) {
        let path = harness.share_path(&format!("attack-{index}.txt"));
        let response = harness.request(Method::GET, &path, None, b"").await;
        assert_eq!(response.status, StatusCode::UNAUTHORIZED);
    }

    // One oversized target must not grow the stored path string.
    let long_name = "a".repeat(MAX_PENDING_PATH_CHARS * 4);
    let response = harness
        .request(Method::GET, &harness.share_path(&long_name), None, b"")
        .await;
    assert_eq!(response.status, StatusCode::UNAUTHORIZED);

    let pending = harness.controller.list_pending();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].attempts, MAX_PENDING_ATTEMPTS);
    assert_eq!(
        pending[0].requested_path.chars().count(),
        MAX_PENDING_PATH_CHARS
    );
    assert_eq!(harness.events.pending.lock().expect("pending").len(), 1);

    harness.shutdown().await;
    drop(harness.directory);
}

#[tokio::test(flavor = "multi_thread")]
async fn pending_queue_is_capacity_bounded() {
    let harness = Harness::new(full_permissions(), vec![loopback_rule()]).await;
    let root = harness.share_root();
    let mut paths = vec![harness.share_path("x.txt")];
    for index in 0..MAX_PENDING_REQUESTS {
        let share = harness
            .controller
            .create_share(
                &format!("Extra share {index}"),
                root.to_str().expect("utf8 path"),
                full_permissions(),
                &[loopback_rule()],
            )
            .expect("extra share");
        paths.push(format!("/share/{}/x.txt", share.id));
    }
    assert_eq!(paths.len(), MAX_PENDING_REQUESTS + 1);

    for path in &paths {
        let response = harness.request(Method::GET, path, None, b"").await;
        assert_eq!(response.status, StatusCode::UNAUTHORIZED);
    }
    assert_eq!(
        harness.controller.list_pending().len(),
        MAX_PENDING_REQUESTS
    );
    assert_eq!(
        harness.events.pending.lock().expect("pending").len(),
        MAX_PENDING_REQUESTS
    );

    // Repeating the flood neither adds entries nor emits more events.
    for path in &paths {
        let response = harness.request(Method::GET, path, None, b"").await;
        assert_eq!(response.status, StatusCode::UNAUTHORIZED);
    }
    assert_eq!(
        harness.controller.list_pending().len(),
        MAX_PENDING_REQUESTS
    );
    assert_eq!(
        harness.events.pending.lock().expect("pending").len(),
        MAX_PENDING_REQUESTS
    );

    harness.shutdown().await;
    drop(harness.directory);
}

#[tokio::test(flavor = "multi_thread")]
async fn rule_and_permission_updates_apply_immediately() {
    let harness = Harness::new(full_permissions(), vec![loopback_rule()]).await;
    let code = harness.issue_code().await;

    assert_eq!(
        harness
            .request(
                Method::GET,
                &harness.share_path("missing.txt"),
                Some(&code),
                b""
            )
            .await
            .status,
        StatusCode::NOT_FOUND
    );

    let updated = harness
        .controller
        .set_share_rules(
            &harness.share_id,
            &[AccessRule {
                kind: RuleKind::Deny,
                matcher: RuleMatcher::Ip("127.0.0.1".to_owned()),
            }],
        )
        .expect("deny rule");
    assert_eq!(updated.rules.len(), 1);
    assert_eq!(updated.rules[0].kind, RuleKind::Deny);
    assert_eq!(
        harness
            .request(
                Method::GET,
                &harness.share_path("missing.txt"),
                Some(&code),
                b""
            )
            .await
            .status,
        StatusCode::FORBIDDEN
    );

    harness
        .controller
        .set_share_rules(&harness.share_id, &[loopback_rule()])
        .expect("restore allow");
    assert_eq!(
        harness
            .request(
                Method::GET,
                &harness.share_path("missing.txt"),
                Some(&code),
                b""
            )
            .await
            .status,
        StatusCode::NOT_FOUND
    );

    harness
        .controller
        .update_share_permissions(&harness.share_id, read_only())
        .expect("read only");
    assert_eq!(
        harness
            .request(
                Method::PUT,
                &harness.share_path("blocked.txt"),
                Some(&code),
                b"x"
            )
            .await
            .status,
        StatusCode::FORBIDDEN
    );
    harness
        .controller
        .update_share_permissions(&harness.share_id, full_permissions())
        .expect("full");
    assert!(
        harness
            .request(
                Method::PUT,
                &harness.share_path("allowed.txt"),
                Some(&code),
                b"x"
            )
            .await
            .status
            .is_success()
    );

    harness.shutdown().await;
    drop(harness.directory);
}

#[tokio::test(flavor = "multi_thread")]
async fn shares_and_authorizations_survive_controller_restart() {
    let harness = Harness::new(full_permissions(), vec![loopback_rule()]).await;
    let code = harness.issue_code().await;

    let reloaded = Arc::new(
        ServerController::new(Arc::clone(&harness.vault), harness.events.clone())
            .expect("reload controller"),
    );
    let shares = reloaded.list_shares();
    assert_eq!(shares.len(), 1);
    assert_eq!(shares[0].id, harness.share_id);
    assert_eq!(shares[0].permissions, full_permissions());
    assert_eq!(shares[0].rules, vec![loopback_rule()]);
    assert_eq!(reloaded.list_authorizations().len(), 1);

    let status = reloaded
        .start(ServerConfig {
            bind_address: "127.0.0.1".to_owned(),
            port: 0,
            ..ServerConfig::default()
        })
        .await
        .expect("start reloaded server");
    let address: SocketAddr = status
        .bind_address
        .expect("address")
        .parse()
        .expect("socket addr");
    let response = send(
        address,
        Method::GET,
        &format!("/share/{}/missing.txt", harness.share_id),
        Some(&code),
        b"",
    )
    .await;
    assert_eq!(
        response.status,
        StatusCode::NOT_FOUND,
        "the persisted authorization must authenticate after restart"
    );
    reloaded.stop().await;

    harness.shutdown().await;
    drop(harness.directory);
}

#[tokio::test(flavor = "multi_thread")]
async fn locked_vault_can_still_serve_authorized_clients() {
    let harness = Harness::new(full_permissions(), vec![loopback_rule()]).await;
    let code = harness.issue_code().await;

    let put = harness
        .request(
            Method::PUT,
            &harness.share_path("before-lock.txt"),
            Some(&code),
            b"before lock",
        )
        .await;
    assert!(
        put.status.is_success(),
        "authorized write before lock failed: {}",
        put.status
    );

    harness.shutdown().await;
    harness.vault.lock().expect("lock vault");
    assert!(
        !harness.vault.status().expect("vault status").unlocked,
        "the vault must stay locked for this test"
    );

    let reloaded = Arc::new(
        ServerController::new(Arc::clone(&harness.vault), harness.events.clone())
            .expect("reload controller while locked"),
    );
    assert_eq!(reloaded.list_shares().len(), 1);
    assert_eq!(reloaded.list_authorizations().len(), 1);

    let status = reloaded
        .start(ServerConfig {
            bind_address: "127.0.0.1".to_owned(),
            port: 0,
            ..ServerConfig::default()
        })
        .await
        .expect("start server while locked");
    let address: SocketAddr = status
        .bind_address
        .expect("address")
        .parse()
        .expect("socket addr");

    let read = send(
        address,
        Method::GET,
        &format!("/share/{}/before-lock.txt", harness.share_id),
        Some(&code),
        b"",
    )
    .await;
    assert_eq!(read.status, StatusCode::OK);
    assert_eq!(read.body, b"before lock");

    let write = send(
        address,
        Method::PUT,
        &format!("/share/{}/after-lock.txt", harness.share_id),
        Some(&code),
        b"after lock",
    )
    .await;
    assert!(
        write.status.is_success(),
        "authorized write while locked failed: {}",
        write.status
    );

    reloaded.stop().await;
    drop(harness.directory);
}

#[tokio::test(flavor = "multi_thread")]
async fn server_lifecycle_and_structured_bind_failures() {
    let directory = dev_test_dir("server_flow");
    let vault = Arc::new(Vault::open(&directory.path().join("peerdav.pd")).expect("vault"));
    let events = Arc::new(Recording::default());
    let controller =
        Arc::new(ServerController::new(Arc::clone(&vault), events).expect("controller"));

    assert!(!controller.status().running);

    let config = ServerConfig {
        bind_address: "127.0.0.1".to_owned(),
        port: 0,
        ..ServerConfig::default()
    };
    let first = controller.start(config.clone()).await.expect("first start");
    assert!(first.running);
    assert!(first.bind_address.is_some());

    let second = controller.start(config.clone()).await;
    assert!(matches!(second, Err(DomainError::ServerAlreadyRunning)));
    let structured = AppError::from(second.expect_err("already running"));
    assert_eq!(structured.code, "SERVER_ALREADY_RUNNING");

    let stopped = controller.stop().await;
    assert!(!stopped.running);
    assert!(stopped.bind_address.is_none());
    let stopped_again = controller.stop().await;
    assert!(!stopped_again.running);

    let restarted = controller.start(config.clone()).await.expect("restart");
    let address = restarted.bind_address.expect("address");
    let options = send(
        address.parse().expect("addr"),
        Method::OPTIONS,
        "/share/anything/",
        None,
        b"",
    )
    .await;
    assert_eq!(options.status, StatusCode::OK);
    assert_eq!(
        options.headers.get("DAV").and_then(|v| v.to_str().ok()),
        Some("1, 2")
    );
    controller.stop().await;

    let invalid = controller
        .start(ServerConfig {
            bind_address: "300.300.300.300".to_owned(),
            port: 1,
            ..ServerConfig::default()
        })
        .await
        .expect_err("invalid bind address");
    let structured = AppError::from(invalid);
    assert_eq!(structured.code, "SERVER_BIND_FAILED");
    assert!(structured.message.contains("绑定地址"));

    let blocker = std::net::TcpListener::bind("127.0.0.1:0").expect("blocker");
    let occupied = blocker.local_addr().expect("occupied address").port();
    let error = controller
        .start(ServerConfig {
            bind_address: "127.0.0.1".to_owned(),
            port: occupied,
            ..ServerConfig::default()
        })
        .await
        .expect_err("occupied port");
    assert!(matches!(error, DomainError::ServerBind { .. }));
    drop(blocker);

    drop(directory);
}

#[tokio::test(flavor = "multi_thread")]
async fn proppatch_and_unknown_methods_are_rejected() {
    let harness = Harness::new(full_permissions(), vec![loopback_rule()]).await;
    let code = harness.issue_code().await;

    let proppatch = harness
        .request(
            Method::from_bytes(b"PROPPATCH").expect("PROPPATCH"),
            &harness.share_path("x.txt"),
            None,
            b"",
        )
        .await;
    assert_eq!(proppatch.status, StatusCode::NOT_IMPLEMENTED);

    let unknown = harness
        .request(
            Method::from_bytes(b"SEARCH").expect("method"),
            &harness.share_path("x.txt"),
            Some(&code),
            b"",
        )
        .await;
    assert_eq!(unknown.status, StatusCode::METHOD_NOT_ALLOWED);

    let unauthenticated_unknown = harness
        .request(
            Method::from_bytes(b"SEARCH").expect("method"),
            &harness.share_path("x.txt"),
            None,
            b"",
        )
        .await;
    assert_eq!(unauthenticated_unknown.status, StatusCode::UNAUTHORIZED);

    harness.shutdown().await;
    drop(harness.directory);
}

#[tokio::test(flavor = "multi_thread")]
async fn admin_reverification_requires_unlocked_vault_and_correct_password() {
    let directory = dev_test_dir("server_flow");
    let vault = Vault::open(&directory.path().join("peerdav.pd")).expect("vault");
    let initialized = vault
        .initialize(&key_dir(directory.path()), ADMIN_USERNAME, ADMIN_PASSWORD)
        .expect("initialize");

    vault
        .verify_admin(&initialized.admin_username, ADMIN_PASSWORD)
        .expect("verified admin");
    assert!(matches!(
        vault.verify_admin(&initialized.admin_username, "wrong password"),
        Err(DomainError::InvalidAdminCredentials)
    ));

    vault.lock().expect("lock");
    assert!(matches!(
        vault.verify_admin(&initialized.admin_username, ADMIN_PASSWORD),
        Err(DomainError::VaultLocked)
    ));
    vault.reload_key(None).expect("reload key");
    vault
        .verify_admin(&initialized.admin_username, ADMIN_PASSWORD)
        .expect("verified after unlock");

    drop(directory);
}

#[test]
fn peer_context_is_public_for_policy_consumers() {
    let peer = peerdav_lib::PeerContext {
        ip: "127.0.0.1".parse::<IpAddr>().expect("ip"),
        mac: None,
    };
    assert_eq!(
        peerdav_lib::evaluate_rules(&[], &peer).expect("evaluate"),
        peerdav_lib::PolicyDecision::Deny
    );
}
