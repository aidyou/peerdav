//! U-3 loopback tests: self-connect approval, discovery de-duplication,
//! structured browsing, transfers with conflict control, backend previews,
//! external-edit round trip, revocation, policy denial, and transfer limits.
//!
//! The client and server share one process and one vault here. The approval
//! flow is identical to a real two-machine setup: the client has no admin
//! bypass, and the administrator must approve the pending request before the
//! code works.

use std::net::UdpSocket;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use peerdav_lib::{
    AccessRule, AppError, ConflictPolicy, DomainError, PeerEvents, PeerInfo, PeerLimits,
    PeerManager, PeerRevocationInfo, PendingRequestInfo, RuleKind, RuleMatcher, ServerConfig,
    ServerController, ServerEvents, SharePermissions, Vault,
};

mod common;

use common::{DevTestDir, dev_test_dir, key_dir};

const ADMIN_USERNAME: &str = "peerdav-admin";
const ADMIN_PASSWORD: &str = "admin password peer 2026";

#[derive(Default)]
struct Recording {
    errors: Mutex<Vec<AppError>>,
    pending: Mutex<Vec<PendingRequestInfo>>,
    revoked: Mutex<Vec<PeerRevocationInfo>>,
}

impl ServerEvents for Recording {
    fn error(&self, error: &AppError) {
        self.errors.lock().expect("errors").push(error.clone());
    }

    fn pending_request(&self, request: &PendingRequestInfo) {
        self.pending.lock().expect("pending").push(request.clone());
    }
}

impl PeerEvents for Recording {
    fn error(&self, error: &AppError) {
        self.errors.lock().expect("errors").push(error.clone());
    }

    fn peer_revoked(&self, peer: &PeerRevocationInfo) {
        self.revoked.lock().expect("revoked").push(peer.clone());
    }
}

struct Harness {
    directory: DevTestDir,
    controller: Arc<ServerController>,
    peers: Arc<PeerManager>,
    events: Arc<Recording>,
    share_id: String,
    url: String,
    discovery_port: u16,
    share_root: PathBuf,
    admin_username: String,
}

impl Harness {
    async fn new(rules: Vec<AccessRule>, with_discovery: bool) -> Self {
        Self::with_limits(rules, with_discovery, PeerLimits::default()).await
    }

    async fn with_limits(rules: Vec<AccessRule>, with_discovery: bool, limits: PeerLimits) -> Self {
        let directory = dev_test_dir("peer_flow");
        let vault =
            Arc::new(Vault::open(&directory.path().join("peerdav.pd")).expect("open vault"));
        let initialized = vault
            .initialize(&key_dir(directory.path()), ADMIN_USERNAME, ADMIN_PASSWORD)
            .expect("initialize");
        let admin_username = initialized.admin_username.clone();

        let share_root = directory.path().join("share");
        std::fs::create_dir(&share_root).expect("share root");
        std::fs::create_dir(share_root.join("sub")).expect("sub dir");
        std::fs::write(share_root.join("notes.txt"), b"hello peer\n").expect("notes");
        std::fs::write(share_root.join("sub").join("inner.txt"), b"inner\n").expect("inner");
        std::fs::write(share_root.join("sub").join("文 件.txt"), b"unicode\n").expect("unicode");
        std::fs::write(share_root.join("image.png"), b"\x89PNG\r\n\x1a\nfakepng").expect("image");

        let events = Arc::new(Recording::default());
        let controller = Arc::new(
            ServerController::new(
                Arc::clone(&vault),
                Arc::clone(&events) as Arc<dyn ServerEvents>,
            )
            .expect("controller"),
        );
        let share = controller
            .create_share(
                "Loopback share",
                share_root.to_str().expect("utf8 path"),
                SharePermissions {
                    can_read: true,
                    can_write: true,
                    can_delete: true,
                },
                &rules,
            )
            .expect("create share");

        let discovery_port = if with_discovery { free_udp_port() } else { 0 };
        let status = controller
            .start(ServerConfig {
                bind_address: "127.0.0.1".to_owned(),
                port: 0,
                discovery_port,
                server_name: "Self host".to_owned(),
            })
            .await
            .expect("start server");
        let port = status
            .bind_address
            .as_deref()
            .and_then(|address| address.rsplit(':').next())
            .and_then(|port| port.parse::<u16>().ok())
            .expect("bound port");

        let peers = Arc::new(
            PeerManager::with_limits_and_events(
                Arc::clone(&vault),
                directory.path().join("peer-temp"),
                limits,
                Arc::clone(&events) as Arc<dyn PeerEvents>,
            )
            .expect("peer manager"),
        );

        Self {
            directory,
            controller,
            peers,
            events,
            share_id: share.id,
            url: format!("http://127.0.0.1:{port}"),
            discovery_port,
            share_root,
            admin_username,
        }
    }

    /// Runs the real request/approve/connect sequence and returns the peer and
    /// its one-time code.
    async fn connect(&self) -> (PeerInfo, String) {
        let request = self
            .peers
            .request_access(&self.url)
            .await
            .expect("request access");
        assert!(request.requested, "the server must ask for approval");

        let pending = self.controller.list_pending();
        assert_eq!(pending.len(), 1, "one pending request");
        assert_eq!(pending[0].share_id, self.share_id);
        assert_eq!(pending[0].peer_ip, "127.0.0.1");

        let approved = self
            .controller
            .approve_pending(&pending[0].id)
            .expect("approve");
        assert_eq!(approved.authorization_code.len(), 64);

        let info = self
            .peers
            .connect(&self.url, &approved.authorization_code, Some("self"))
            .await
            .expect("connect with code");
        (info, approved.authorization_code)
    }
}

fn loopback_rule() -> AccessRule {
    AccessRule {
        kind: RuleKind::Allow,
        matcher: RuleMatcher::Ip("127.0.0.1".to_owned()),
    }
}

fn other_subnet_rule() -> AccessRule {
    AccessRule {
        kind: RuleKind::Allow,
        matcher: RuleMatcher::Network("10.99.0.0/16".to_owned()),
    }
}

fn free_udp_port() -> u16 {
    let socket = UdpSocket::bind("127.0.0.1:0").expect("free udp port");
    socket.local_addr().expect("local addr").port()
}

fn code_of(error: &DomainError) -> String {
    AppError::from_ref(error).code
}

#[tokio::test]
async fn approved_code_is_delivered_and_saved_automatically() {
    let harness = Harness::new(vec![loopback_rule()], false).await;
    let request = harness
        .peers
        .request_access(&harness.url)
        .await
        .expect("request access");
    assert!(request.requested);
    let request_id = request.request_id.expect("request id");

    let pending = harness.controller.list_pending();
    assert_eq!(pending.len(), 1);
    let approved = harness
        .controller
        .approve_pending(&pending[0].id)
        .expect("approve");
    assert!(approved.auto_delivery);

    let delivered = harness
        .peers
        .wait_for_access(&harness.url, &request_id)
        .await
        .expect("claim delivered code");
    assert_eq!(delivered, approved.authorization_code);

    let info = harness
        .peers
        .connect(&harness.url, &delivered, Some("automatic"))
        .await
        .expect("connect with delivered code");
    assert_eq!(info.label, "automatic");
    assert_eq!(info.shares.len(), 1);
    let saved = harness.peers.list_peers().expect("list saved peers");
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].url, harness.url);
}

#[tokio::test]
async fn access_wait_times_out_without_making_the_client_an_admin() {
    let limits = PeerLimits {
        access_wait: Duration::from_millis(150),
        ..PeerLimits::default()
    };
    let harness = Harness::with_limits(vec![loopback_rule()], false, limits).await;
    let request = harness
        .peers
        .request_access(&harness.url)
        .await
        .expect("request access");
    let request_id = request.request_id.expect("request id");

    let error = harness
        .peers
        .wait_for_access(&harness.url, &request_id)
        .await
        .expect_err("unapproved request must time out");
    assert!(matches!(error, DomainError::PeerAccessTimeout));
    assert_eq!(harness.controller.list_authorizations().len(), 0);
    assert!(harness.peers.list_peers().expect("saved peers").is_empty());

    let error = harness
        .peers
        .wait_for_access(&harness.url, &"f".repeat(32))
        .await
        .expect_err("unknown request id must not receive a code");
    assert!(matches!(error, DomainError::PeerAccessExpired));
}

#[tokio::test]
async fn self_connect_needs_approval_and_admin_credentials_are_not_a_code() {
    let harness = Harness::new(vec![loopback_rule()], false).await;

    // An unauthenticated catalog request records a pending approval.
    let request = harness
        .peers
        .request_access(&harness.url)
        .await
        .expect("request access");
    assert!(request.requested);
    assert_eq!(harness.controller.list_pending().len(), 1);
    assert_eq!(
        harness.controller.list_authorizations().len(),
        0,
        "nothing is approved automatically"
    );

    // A wrong code is rejected.
    let rejected = harness
        .peers
        .connect(&harness.url, "not-the-code", None)
        .await
        .expect_err("wrong code must fail");
    assert!(matches!(rejected, DomainError::PeerAuthorizationRejected));

    // The admin password is not a share token.
    let response = reqwest::Client::new()
        .get(format!("{}/__peerdav/catalog", harness.url))
        .basic_auth(&harness.admin_username, Some(ADMIN_PASSWORD))
        .send()
        .await
        .expect("admin request");
    assert_eq!(response.status().as_u16(), 401);

    let pending = harness.controller.list_pending();
    assert_eq!(pending.len(), 1);
    let approved = harness
        .controller
        .approve_pending(&pending[0].id)
        .expect("approve");
    let info = harness
        .peers
        .connect(&harness.url, &approved.authorization_code, Some("self"))
        .await
        .expect("connect");

    assert_eq!(info.shares.len(), 1);
    assert_eq!(info.shares[0].id, harness.share_id);
    assert!(info.shares[0].permissions.can_read);

    // The code is not exposed through the DTO or stored in plaintext.
    let serialized = serde_json::to_string(&info).expect("serialize peer");
    assert!(!serialized.contains(&approved.authorization_code));
    let database =
        std::fs::read(harness.directory.path().join("peerdav.pd")).expect("read database");
    assert!(
        !contains_bytes(&database, approved.authorization_code.as_bytes()),
        "the authorization code must not appear in SQLite"
    );

    let saved = harness.peers.list_peers().expect("list peers");
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].id, info.id);

    let health = harness.peers.check_health(&info.id).await.expect("health");
    assert!(health.online && health.authorized);
    assert!(health.latency_ms.is_some());

    // Revocation takes effect on the next request.
    harness
        .controller
        .revoke_authorization(&approved.authorization.id)
        .expect("revoke");
    let health = harness
        .peers
        .check_health(&info.id)
        .await
        .expect("health after revoke");
    assert!(health.online);
    assert!(!health.authorized);
    assert_eq!(health.code.as_deref(), Some("PEER_AUTHORIZATION_REJECTED"));

    let error = harness
        .peers
        .list_dir(&info.id, &harness.share_id, "/")
        .await
        .expect_err("revoked code must not list");
    assert_eq!(code_of(&error), "PEER_AUTHORIZATION_REJECTED");

    harness.peers.forget(&info.id).await.expect("forget peer");
    assert!(harness.peers.list_peers().expect("peers").is_empty());
    let missing = harness
        .peers
        .check_health(&info.id)
        .await
        .expect_err("forgotten peer");
    assert_eq!(code_of(&missing), "PEER_NOT_FOUND");

    harness.controller.stop().await;
}

#[tokio::test]
async fn pending_access_request_can_be_canceled_before_approval() {
    let harness = Harness::new(vec![loopback_rule()], false).await;
    let request = harness
        .peers
        .request_access(&harness.url)
        .await
        .expect("request access");
    let request_id = request.request_id.expect("request id");
    assert_eq!(harness.controller.list_pending().len(), 1);

    let waiter = {
        let peers = Arc::clone(&harness.peers);
        let url = harness.url.clone();
        let request_id = request_id.clone();
        tokio::spawn(async move { peers.wait_for_access(&url, &request_id).await })
    };
    tokio::time::sleep(Duration::from_millis(50)).await;
    harness
        .peers
        .cancel_access(&harness.url, &request_id)
        .await
        .expect("cancel access");

    let result = waiter.await.expect("waiter task");
    assert!(matches!(result, Err(DomainError::PeerAccessCanceled)));
    assert!(harness.controller.list_pending().is_empty());
    assert!(harness.controller.list_authorizations().is_empty());

    harness.controller.stop().await;
}

#[tokio::test]
async fn canceling_after_approval_discards_the_queued_code_and_authorization() {
    let harness = Harness::new(vec![loopback_rule()], false).await;
    let request = harness
        .peers
        .request_access(&harness.url)
        .await
        .expect("request access");
    let request_id = request.request_id.expect("request id");
    let pending = harness.controller.list_pending();
    let approved = harness
        .controller
        .approve_pending(&pending[0].id)
        .expect("approve");
    assert!(approved.auto_delivery);
    assert_eq!(harness.controller.list_authorizations().len(), 1);

    harness
        .peers
        .cancel_access(&harness.url, &request_id)
        .await
        .expect("cancel approved request");

    assert!(harness.controller.list_pending().is_empty());
    assert!(harness.controller.list_authorizations().is_empty());
    let result = harness
        .peers
        .wait_for_access(&harness.url, &request_id)
        .await;
    assert!(matches!(result, Err(DomainError::PeerAccessCanceled)));
    assert!(harness.peers.list_peers().expect("peers").is_empty());

    harness.controller.stop().await;
}

#[tokio::test]
async fn client_unbind_revokes_the_server_authorization() {
    let harness = Harness::new(vec![loopback_rule()], false).await;
    let (info, code) = harness.connect().await;
    assert_eq!(harness.controller.list_authorizations().len(), 1);

    harness.peers.forget(&info.id).await.expect("client unbind");

    assert!(harness.peers.list_peers().expect("peers").is_empty());
    assert!(harness.controller.list_authorizations().is_empty());
    let response = reqwest::Client::new()
        .get(format!("{}/__peerdav/catalog", harness.url))
        .basic_auth("peerdav", Some(&code))
        .send()
        .await
        .expect("catalog after client unbind");
    assert_eq!(response.status().as_u16(), 401);

    harness.controller.stop().await;
}

#[tokio::test]
async fn server_revocation_removes_the_client_binding() {
    let harness = Harness::new(vec![loopback_rule()], false).await;
    let (info, _code) = harness.connect().await;
    let authorization = harness
        .controller
        .list_authorizations()
        .into_iter()
        .next()
        .expect("connected authorization");

    harness
        .controller
        .revoke_authorization(&authorization.id)
        .expect("server revoke");
    let removed = harness
        .peers
        .sync_saved_peers()
        .await
        .expect("sync clients");

    assert_eq!(removed, 1);
    assert!(harness.peers.list_peers().expect("peers").is_empty());
    {
        let revocations = harness.events.revoked.lock().expect("revoked");
        assert_eq!(revocations.len(), 1);
        assert_eq!(revocations[0].id, info.id);
        assert_eq!(revocations[0].label, info.label);
        assert_eq!(revocations[0].reason, "revoked");
    }

    harness.controller.stop().await;
}

#[tokio::test]
async fn discovery_deduplicates_replies_from_the_same_server() {
    let harness = Harness::new(vec![loopback_rule()], true).await;
    let status = harness.controller.status();
    assert!(status.discovery_running);
    assert_eq!(status.discovery_port, Some(harness.discovery_port));

    let found = harness
        .peers
        .scan_lan(Some(harness.discovery_port), Some(1200))
        .await
        .expect("scan");

    assert_eq!(found.len(), 1, "duplicate replies must be merged");
    assert_eq!(found[0].name, "Self host");
    assert_eq!(found[0].url, harness.url);
    assert_eq!(found[0].share_count, 1);
    assert!(found[0].hosts.contains(&"127.0.0.1".to_owned()));

    // The discovered URL feeds the same request/approve/connect flow.
    let request = harness
        .peers
        .request_access(&found[0].url)
        .await
        .expect("request access from discovery");
    assert!(request.requested);
    let pending = harness.controller.list_pending();
    assert_eq!(pending.len(), 1);
    let approved = harness
        .controller
        .approve_pending(&pending[0].id)
        .expect("approve discovered request");
    let connected = harness
        .peers
        .connect(&found[0].url, &approved.authorization_code, None)
        .await
        .expect("connect through discovery");
    assert_eq!(connected.shares.len(), 1);

    harness.controller.stop().await;
}

#[tokio::test]
async fn browse_transfer_preview_and_external_edit_roundtrip() {
    let harness = Harness::new(vec![loopback_rule()], false).await;
    let (info, _code) = harness.connect().await;
    let peer_id = info.id;
    let share_id = harness.share_id.clone();

    // Structured browsing.
    let root = harness
        .peers
        .list_dir(&peer_id, &share_id, "/")
        .await
        .expect("list root");
    let names: Vec<&str> = root
        .entries
        .iter()
        .map(|entry| entry.name.as_str())
        .collect();
    assert_eq!(names, vec!["sub", "image.png", "notes.txt"]);
    assert!(root.entries[0].is_dir);
    let notes = root
        .entries
        .iter()
        .find(|entry| entry.name == "notes.txt")
        .expect("notes entry");
    assert_eq!(notes.path, "/notes.txt");
    assert!(!notes.is_dir);
    assert_eq!(notes.size, 11);
    assert!(notes.modified.is_some());

    let sub = harness
        .peers
        .list_dir(&peer_id, &share_id, "/sub")
        .await
        .expect("list sub");
    assert_eq!(sub.entries.len(), 2);
    let unicode = sub
        .entries
        .iter()
        .find(|entry| entry.name == "文 件.txt")
        .expect("unicode entry");
    assert_eq!(unicode.path, "/sub/文 件.txt");
    assert_eq!(unicode.size, 8);

    let unicode_dir = tempfile::tempdir().expect("unicode download");
    harness
        .peers
        .download(
            &peer_id,
            &share_id,
            "/sub/文 件.txt",
            unicode_dir.path().to_str().expect("utf8"),
            ConflictPolicy::Fail,
        )
        .await
        .expect("unicode download");
    assert_eq!(
        std::fs::read_to_string(unicode_dir.path().join("文 件.txt")).expect("unicode file"),
        "unicode\n"
    );

    // Download, conflict handling, and rename.
    let downloads = tempfile::tempdir().expect("downloads");
    let downloaded = harness
        .peers
        .download(
            &peer_id,
            &share_id,
            "/notes.txt",
            downloads.path().to_str().expect("utf8"),
            ConflictPolicy::Fail,
        )
        .await
        .expect("download");
    assert!(downloaded.transferred);
    let local_notes = downloads.path().join("notes.txt");
    assert_eq!(
        std::fs::read_to_string(&local_notes).expect("read notes"),
        "hello peer\n"
    );

    let conflict = harness
        .peers
        .download(
            &peer_id,
            &share_id,
            "/notes.txt",
            downloads.path().to_str().expect("utf8"),
            ConflictPolicy::Fail,
        )
        .await
        .expect_err("existing file");
    assert_eq!(code_of(&conflict), "TRANSFER_CONFLICT");

    let renamed = harness
        .peers
        .download(
            &peer_id,
            &share_id,
            "/notes.txt",
            downloads.path().to_str().expect("utf8"),
            ConflictPolicy::Rename,
        )
        .await
        .expect("rename download");
    assert_eq!(
        Path::new(renamed.local_path.as_deref().expect("path"))
            .file_name()
            .and_then(|name| name.to_str()),
        Some("notes (1).txt")
    );

    // Upload to the sub directory and rename on conflict.
    let upload_dir = tempfile::tempdir().expect("upload dir");
    let upload_path = upload_dir.path().join("upload.txt");
    std::fs::write(&upload_path, b"uploaded\n").expect("upload file");
    let uploaded = harness
        .peers
        .upload(
            &peer_id,
            &share_id,
            upload_path.to_str().expect("utf8"),
            "/sub",
            ConflictPolicy::Fail,
        )
        .await
        .expect("upload");
    assert_eq!(uploaded.remote_path, "/sub/upload.txt");
    assert_eq!(
        std::fs::read_to_string(harness.share_root.join("sub").join("upload.txt"))
            .expect("uploaded file"),
        "uploaded\n"
    );

    let upload_conflict = harness
        .peers
        .upload(
            &peer_id,
            &share_id,
            upload_path.to_str().expect("utf8"),
            "/sub",
            ConflictPolicy::Fail,
        )
        .await
        .expect_err("remote conflict");
    assert_eq!(code_of(&upload_conflict), "TRANSFER_CONFLICT");

    let upload_renamed = harness
        .peers
        .upload(
            &peer_id,
            &share_id,
            upload_path.to_str().expect("utf8"),
            "/sub",
            ConflictPolicy::Rename,
        )
        .await
        .expect("renamed upload");
    assert_eq!(upload_renamed.remote_path, "/sub/upload (1).txt");

    let nested_upload = harness
        .peers
        .upload(
            &peer_id,
            &share_id,
            upload_path.to_str().expect("utf8"),
            "/sub/new/deep",
            ConflictPolicy::Fail,
        )
        .await
        .expect("upload into a new folder tree");
    assert_eq!(nested_upload.remote_path, "/sub/new/deep/upload.txt");
    assert_eq!(
        std::fs::read_to_string(
            harness
                .share_root
                .join("sub")
                .join("new")
                .join("deep")
                .join("upload.txt"),
        )
        .expect("nested uploaded file"),
        "uploaded\n"
    );

    let unicode_upload = upload_dir.path().join("上 传.txt");
    std::fs::write(&unicode_upload, b"uploaded unicode\n").expect("unicode upload");
    let unicode_uploaded = harness
        .peers
        .upload(
            &peer_id,
            &share_id,
            unicode_upload.to_str().expect("utf8"),
            "/sub",
            ConflictPolicy::Fail,
        )
        .await
        .expect("unicode upload");
    assert_eq!(unicode_uploaded.remote_path, "/sub/上 传.txt");
    assert_eq!(
        std::fs::read_to_string(harness.share_root.join("sub").join("上 传.txt"))
            .expect("unicode uploaded file"),
        "uploaded unicode\n"
    );

    // Backend preview: text and binary.
    let preview = harness
        .peers
        .preview(&peer_id, &share_id, "/notes.txt")
        .await
        .expect("text preview");
    assert_eq!(preview.encoding, "utf8");
    assert_eq!(preview.data, "hello peer\n");
    assert_eq!(preview.mime, "text/plain");

    let image = harness
        .peers
        .preview(&peer_id, &share_id, "/image.png")
        .await
        .expect("image preview");
    assert_eq!(image.encoding, "base64");
    assert_eq!(image.mime, "image/png");
    assert!(!image.data.is_empty());

    // External edit: download, modify, upload back, then detect no change.
    let edit = harness
        .peers
        .prepare_external_edit(&peer_id, &share_id, "/notes.txt")
        .await
        .expect("prepare edit");
    assert!(Path::new(&edit.local_path).starts_with(harness.directory.path().join("peer-temp")));
    std::fs::write(&edit.local_path, b"edited outside\n").expect("edit file");
    let committed = harness
        .peers
        .commit_external_edit(&edit.edit_id, ConflictPolicy::Fail)
        .await
        .expect("commit edit");
    assert!(committed.transferred);
    assert_eq!(
        std::fs::read_to_string(harness.share_root.join("notes.txt")).expect("remote notes"),
        "edited outside\n"
    );
    assert!(!Path::new(&edit.local_path).exists(), "temp copy removed");

    let edit = harness
        .peers
        .prepare_external_edit(&peer_id, &share_id, "/notes.txt")
        .await
        .expect("prepare unchanged edit");
    let unchanged = harness
        .peers
        .commit_external_edit(&edit.edit_id, ConflictPolicy::Fail)
        .await
        .expect("commit unchanged");
    assert!(!unchanged.transferred);
    assert_eq!(unchanged.skipped_reason.as_deref(), Some("unchanged"));

    let edit = harness
        .peers
        .prepare_external_edit(&peer_id, &share_id, "/notes.txt")
        .await
        .expect("prepare conflict edit");
    std::fs::write(&edit.local_path, b"local edit\n").expect("local edit");
    std::fs::write(
        harness.share_root.join("notes.txt"),
        b"another writer changed the remote file\n",
    )
    .expect("remote change");
    let conflict = harness
        .peers
        .commit_external_edit(&edit.edit_id, ConflictPolicy::Fail)
        .await
        .expect_err("remote changed");
    assert_eq!(code_of(&conflict), "TRANSFER_CONFLICT");
    let resolved = harness
        .peers
        .commit_external_edit(&edit.edit_id, ConflictPolicy::Overwrite)
        .await
        .expect("overwrite remote change");
    assert!(resolved.transferred);
    assert_eq!(
        std::fs::read_to_string(harness.share_root.join("notes.txt")).expect("remote notes"),
        "local edit\n"
    );

    let discarded = harness
        .peers
        .prepare_external_edit(&peer_id, &share_id, "/image.png")
        .await
        .expect("prepare discard");
    harness
        .peers
        .discard_external_edit(&discarded.edit_id)
        .await
        .expect("discard");
    assert!(!Path::new(&discarded.local_path).exists());

    harness.controller.stop().await;
}

#[tokio::test]
async fn path_traversal_and_invalid_share_ids_are_rejected() {
    let harness = Harness::new(vec![loopback_rule()], false).await;
    let (info, _code) = harness.connect().await;
    let peer_id = info.id;

    for path in ["/../secret", "/sub/../../etc/passwd", "/sub\\..\\secret"] {
        let error = harness
            .peers
            .list_dir(&peer_id, &harness.share_id, path)
            .await
            .expect_err("invalid path");
        assert_eq!(code_of(&error), "PEER_INVALID_PATH", "path {path}");
    }

    let error = harness
        .peers
        .list_dir(&peer_id, "../escape", "/")
        .await
        .expect_err("invalid share");
    assert_eq!(code_of(&error), "PEER_INVALID_PATH");

    let downloads = tempfile::tempdir().expect("downloads");
    let error = harness
        .peers
        .download(
            &peer_id,
            &harness.share_id,
            "/../notes.txt",
            downloads.path().to_str().expect("utf8"),
            ConflictPolicy::Fail,
        )
        .await
        .expect_err("invalid download path");
    assert_eq!(code_of(&error), "PEER_INVALID_PATH");

    harness.controller.stop().await;
}

#[tokio::test]
async fn downloads_to_the_exact_save_dialog_path() {
    let harness = Harness::new(vec![loopback_rule()], false).await;
    let (info, _code) = harness.connect().await;
    let destination = tempfile::tempdir().expect("destination");
    let target = destination.path().join("renamed copy.txt");

    let result = harness
        .peers
        .download_to(
            &info.id,
            &harness.share_id,
            "/notes.txt",
            target.to_str().expect("utf8 target"),
        )
        .await
        .expect("download to save path");
    assert!(result.transferred);
    assert_eq!(
        std::fs::read_to_string(&target).expect("saved file"),
        "hello peer\n"
    );

    std::fs::write(&target, b"old contents").expect("existing target");
    harness
        .peers
        .download_to(
            &info.id,
            &harness.share_id,
            "/notes.txt",
            target.to_str().expect("utf8 target"),
        )
        .await
        .expect("overwrite save path");
    assert_eq!(
        std::fs::read_to_string(&target).expect("overwritten file"),
        "hello peer\n"
    );

    harness.controller.stop().await;
}

#[tokio::test]
async fn download_reports_streaming_progress() {
    let harness = Harness::new(vec![loopback_rule()], false).await;
    let (info, _code) = harness.connect().await;
    let destination = tempfile::tempdir().expect("destination");
    let target = destination.path().join("progress.txt");
    let updates = Arc::new(std::sync::Mutex::new(Vec::new()));
    let progress_updates = Arc::clone(&updates);
    let progress = Arc::new(move |transferred, total| {
        progress_updates
            .lock()
            .expect("progress updates")
            .push((transferred, total));
    });

    harness
        .peers
        .download_to_with_progress(
            &info.id,
            &harness.share_id,
            "/notes.txt",
            target.to_str().expect("utf8 target"),
            Some(progress),
        )
        .await
        .expect("download with progress");
    {
        let updates = updates.lock().expect("progress updates");
        assert!(!updates.is_empty());
        assert_eq!(updates.last().copied(), Some((11, 11)));
    }

    harness.controller.stop().await;
}

#[tokio::test]
async fn transfer_limits_and_symlink_destinations_are_enforced() {
    let limits = PeerLimits {
        max_transfer_bytes: 16,
        max_preview_bytes: 8,
        ..PeerLimits::default()
    };
    let harness = Harness::with_limits(vec![loopback_rule()], false, limits).await;
    std::fs::write(harness.share_root.join("large.txt"), vec![b'x'; 32]).expect("large");
    let (info, _code) = harness.connect().await;
    let peer_id = info.id;

    let local = tempfile::NamedTempFile::new().expect("temp file");
    std::fs::write(local.path(), vec![b'y'; 32]).expect("local file");
    let error = harness
        .peers
        .upload(
            &peer_id,
            &harness.share_id,
            local.path().to_str().expect("utf8"),
            "/",
            ConflictPolicy::Fail,
        )
        .await
        .expect_err("upload too large");
    assert_eq!(code_of(&error), "TRANSFER_TOO_LARGE");

    let downloads = tempfile::tempdir().expect("downloads");
    let error = harness
        .peers
        .download(
            &peer_id,
            &harness.share_id,
            "/large.txt",
            downloads.path().to_str().expect("utf8"),
            ConflictPolicy::Fail,
        )
        .await
        .expect_err("download too large");
    assert_eq!(code_of(&error), "TRANSFER_TOO_LARGE");
    assert!(
        std::fs::read_dir(downloads.path())
            .expect("read dir")
            .next()
            .is_none(),
        "no partial file is left behind"
    );

    let error = harness
        .peers
        .preview(&peer_id, &harness.share_id, "/large.txt")
        .await
        .expect_err("preview too large");
    assert_eq!(code_of(&error), "TRANSFER_TOO_LARGE");

    // A symlink destination is never replaced.
    #[cfg(unix)]
    {
        let symlink_dir = tempfile::tempdir().expect("symlink dir");
        let target = symlink_dir.path().join("target.txt");
        std::fs::write(&target, b"target").expect("target");
        std::os::unix::fs::symlink(&target, symlink_dir.path().join("notes.txt")).expect("symlink");
        let error = harness
            .peers
            .download(
                &peer_id,
                &harness.share_id,
                "/notes.txt",
                symlink_dir.path().to_str().expect("utf8"),
                ConflictPolicy::Overwrite,
            )
            .await
            .expect_err("symlink destination");
        assert_eq!(code_of(&error), "IO_FAILED");
        assert_eq!(std::fs::read_to_string(&target).expect("target"), "target");
    }

    harness.controller.stop().await;
}

#[tokio::test]
async fn network_policy_denial_does_not_create_pending_or_authorization() {
    let harness = Harness::new(vec![other_subnet_rule()], false).await;
    let error = harness
        .peers
        .request_access(&harness.url)
        .await
        .expect_err("policy denial");
    assert_eq!(code_of(&error), "PEER_REQUEST_FAILED");
    assert_eq!(harness.controller.list_pending().len(), 0);
    assert_eq!(harness.controller.list_authorizations().len(), 0);
    harness.controller.stop().await;
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}
