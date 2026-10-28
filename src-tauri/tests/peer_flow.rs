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
    AccessRule, AppError, ConflictPolicy, DomainError, OutboundAccessStatus, PeerEvents, PeerInfo,
    PeerLimits, PeerManager, PeerRevocationInfo, PeerStatus, PendingRequestInfo, RuleKind,
    RuleMatcher, ServerConfig, ServerController, ServerEvents, SharePermissions, Vault,
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
    statuses: Mutex<Vec<PeerStatus>>,
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

    fn peer_status(&self, status: &PeerStatus) {
        self.statuses.lock().expect("statuses").push(status.clone());
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
    vault: Arc<Vault>,
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
            url: format!("https://127.0.0.1:{port}"),
            discovery_port,
            share_root,
            admin_username,
            vault,
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
    let response = harness
        .vault
        .encrypted_client(harness.vault.fingerprint())
        .expect("encrypted client")
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
async fn local_removal_preserves_authorization_and_same_identity_reconnects_without_approval() {
    let harness = Harness::new(vec![loopback_rule()], false).await;
    let (info, _) = harness.connect().await;
    harness
        .peers
        .forget_local(&info.id)
        .await
        .expect("local removal");
    assert!(harness.peers.list_peers().expect("peers").is_empty());
    assert_eq!(harness.controller.list_authorizations().len(), 1);
    let access = harness
        .peers
        .begin_access(&harness.url, None)
        .await
        .expect("reuse identity");
    assert_eq!(access.status, OutboundAccessStatus::Accessible);
    assert_eq!(harness.peers.list_peers().expect("restored peers").len(), 1);
    assert!(harness.controller.list_pending().is_empty());
    let auth = harness.controller.list_authorizations()[0].id.clone();
    harness
        .controller
        .revoke_authorization(&auth)
        .expect("revoke");
    let peer = harness.peers.list_peers().expect("peers")[0].id.clone();
    harness
        .peers
        .forget_local(&peer)
        .await
        .expect("remove again");
    let access = harness
        .peers
        .begin_access(&harness.url, None)
        .await
        .expect("request again");
    assert_eq!(access.status, OutboundAccessStatus::Waiting);
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
    let response = harness
        .vault
        .encrypted_client(harness.vault.fingerprint())
        .expect("encrypted client")
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
    // Without a caller-supplied label the server's own configured name wins over
    // the URL host, so the saved peer does not read like a bare address.
    assert_eq!(connected.label, "Self host");

    harness.controller.stop().await;
}

#[tokio::test]
async fn saved_peer_availability_follows_server_lifecycle() {
    let harness = Harness::new(vec![loopback_rule()], false).await;
    let (info, _code) = harness.connect().await;

    let peers = harness.peers.list_peers().expect("peers");
    assert_eq!(peers.len(), 1);
    assert!(peers[0].online);

    harness.controller.stop().await;
    harness
        .peers
        .sync_saved_peers()
        .await
        .expect("sync after shutdown");

    let peers = harness.peers.list_peers().expect("peers");
    assert_eq!(peers.len(), 1);
    assert!(!peers[0].online);
    let statuses = harness.events.statuses.lock().expect("statuses");
    assert!(
        statuses
            .iter()
            .any(|status| status.id == info.id && status.online)
    );
    assert!(
        statuses
            .iter()
            .any(|status| status.id == info.id && !status.online)
    );
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

    // Deleting files and non-empty directories requires delete permission.
    harness
        .peers
        .delete_entry(&peer_id, &share_id, "/notes.txt")
        .await
        .expect("delete file");
    assert!(!harness.share_root.join("notes.txt").exists());

    let missing = harness
        .peers
        .delete_entry(&peer_id, &share_id, "/notes.txt")
        .await
        .expect_err("already deleted");
    assert_eq!(code_of(&missing), "PEER_REMOTE_NOT_FOUND");

    let root = harness
        .peers
        .delete_entry(&peer_id, &share_id, "/")
        .await
        .expect_err("shared root");
    assert_eq!(code_of(&root), "PEER_INVALID_PATH");

    harness
        .peers
        .delete_entry(&peer_id, &share_id, "/sub")
        .await
        .expect("delete directory");
    assert!(!harness.share_root.join("sub").exists());

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
async fn prepares_directory_downloads_with_gitignore_and_nested_paths() {
    let harness = Harness::new(vec![loopback_rule()], false).await;
    let (info, _code) = harness.connect().await;
    let peer_id = info.id;
    let share_id = harness.share_id.clone();

    let sub = harness.share_root.join("sub");
    std::fs::create_dir(sub.join("nested")).expect("nested dir");
    std::fs::create_dir(sub.join("__pycache__")).expect("cache dir");
    std::fs::write(sub.join(".gitignore"), "ignored.txt\n*.log\n!keep.log\n").expect("gitignore");
    std::fs::write(sub.join("ignored.txt"), b"ignored").expect("ignored file");
    std::fs::write(sub.join("drop.log"), b"drop").expect("dropped log");
    std::fs::write(sub.join("keep.log"), b"keep").expect("kept log");
    std::fs::write(sub.join("module.pyc"), b"cache").expect("pyc file");
    std::fs::write(sub.join("__pycache__").join("x.pyc"), b"cache").expect("cache file");
    std::fs::write(sub.join("nested").join(".gitignore"), "drop.log\n").expect("nested gitignore");
    std::fs::write(sub.join("nested").join("drop.log"), b"drop").expect("nested dropped log");
    std::fs::write(sub.join("nested").join("keep.txt"), b"keep").expect("nested keep");

    let downloads = tempfile::tempdir().expect("downloads");
    let plan = harness
        .peers
        .prepare_directory_download(
            &peer_id,
            &share_id,
            "/sub",
            downloads.path().to_str().expect("utf8"),
            ConflictPolicy::Fail,
        )
        .await
        .expect("prepare directory download");

    assert_eq!(
        Path::new(&plan.local_root)
            .file_name()
            .and_then(|name| name.to_str()),
        Some("sub")
    );
    assert!(Path::new(&plan.local_root).join("nested").is_dir());
    assert!(!Path::new(&plan.local_root).join("__pycache__").exists());
    let relative: Vec<&str> = plan
        .files
        .iter()
        .map(|file| file.relative_path.as_str())
        .collect();
    for expected in [".gitignore", "inner.txt", "keep.log", "nested/keep.txt"] {
        assert!(
            relative.contains(&expected),
            "{expected} should be queued: {relative:?}"
        );
    }
    for ignored in [
        "ignored.txt",
        "drop.log",
        "module.pyc",
        "nested/drop.log",
        "__pycache__/x.pyc",
    ] {
        assert!(
            !relative.contains(&ignored),
            "{ignored} should be ignored: {relative:?}"
        );
    }

    let inner = plan
        .files
        .iter()
        .find(|file| file.relative_path == "inner.txt")
        .expect("inner candidate");
    harness
        .peers
        .download_to(&peer_id, &share_id, &inner.remote_path, &inner.local_path)
        .await
        .expect("download planned file");
    assert_eq!(
        std::fs::read_to_string(&inner.local_path).expect("downloaded inner"),
        "inner\n"
    );

    let conflict = harness
        .peers
        .prepare_directory_download(
            &peer_id,
            &share_id,
            "/sub",
            downloads.path().to_str().expect("utf8"),
            ConflictPolicy::Fail,
        )
        .await
        .expect_err("existing directory");
    assert_eq!(code_of(&conflict), "TRANSFER_CONFLICT");

    harness
        .peers
        .prepare_directory_download(
            &peer_id,
            &share_id,
            "/sub",
            downloads.path().to_str().expect("utf8"),
            ConflictPolicy::Overwrite,
        )
        .await
        .expect("overwrite directory");
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
async fn ten_gibibyte_upload_boundaries_are_enforced_without_large_allocations() {
    let limit = 10 * 1024 * 1024 * 1024u64;
    assert_eq!(PeerLimits::default().max_transfer_bytes, limit);
    let harness = Harness::new(vec![loopback_rule()], false).await;
    let (peer, code) = harness.connect().await;
    let client = harness
        .vault
        .encrypted_client(harness.vault.fingerprint())
        .expect("TLS client");
    let target = format!("{}/share/{}/boundary.bin", harness.url, harness.share_id);
    // HEAD validates the declared size and final part index without sending
    // or hashing a 10 GiB payload. A missing valid part returns 404; the
    // existing server error mapping returns 400 for rejected upload sizes.
    for (size, status) in [
        (256 * 1024 * 1024 + 1, reqwest::StatusCode::NOT_FOUND),
        (limit - 1, reqwest::StatusCode::NOT_FOUND),
        (limit, reqwest::StatusCode::NOT_FOUND),
        (limit + 1, reqwest::StatusCode::BAD_REQUEST),
    ] {
        let response = client
            .head(&target)
            .basic_auth("peerdav", Some(&code))
            .header("x-peerdav-transfer", "00".repeat(32))
            .header("x-peerdav-size", size)
            .header("x-peerdav-part", size.div_ceil(10 * 1024 * 1024) - 1)
            .send()
            .await
            .expect("size boundary response");
        assert_eq!(response.status(), status, "declared size {size}");
    }
    assert!(!harness.share_root.join("boundary.bin").exists());
    assert!(
        harness
            .events
            .errors
            .lock()
            .expect("errors")
            .iter()
            .any(|error| error.code == "TRANSFER_TOO_LARGE"),
        "the server reports the structured size-limit error"
    );

    // Sparse files exercise the real client checks without transferring data.
    let source = harness.share_root.join("oversized.bin");
    std::fs::File::create(&source)
        .expect("sparse file")
        .set_len(limit + 1)
        .expect("sparse length");
    let error = harness
        .peers
        .upload(
            &peer.id,
            &harness.share_id,
            source.to_str().expect("source path"),
            "/sub",
            ConflictPolicy::Fail,
        )
        .await
        .expect_err("client must reject oversized upload");
    assert!(
        matches!(error, DomainError::TransferTooLarge { size, limit: actual }
        if size == limit + 1 && actual == limit)
    );
    let destination = harness.directory.path().join("oversized-download.bin");
    let error = harness
        .peers
        .download_to(
            &peer.id,
            &harness.share_id,
            "/oversized.bin",
            destination.to_str().expect("destination path"),
        )
        .await
        .expect_err("client must reject oversized download");
    assert!(
        matches!(error, DomainError::TransferTooLarge { size, limit: actual }
        if size == limit + 1 && actual == limit)
    );
    assert!(!destination.exists());
    std::fs::remove_file(&source).expect("remove sparse file");
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
    assert_eq!(code_of(&error), "PEER_PREVIEW_TOO_LARGE");

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
async fn preview_rejects_text_over_the_line_limit() {
    let harness = Harness::new(vec![loopback_rule()], false).await;
    let (info, _code) = harness.connect().await;
    let peer_id = info.id;

    let over_limit = (0..=5000).map(|_| "line\n").collect::<String>();
    std::fs::write(harness.share_root.join("long.txt"), over_limit).expect("long text");
    let error = harness
        .peers
        .preview(&peer_id, &harness.share_id, "/long.txt")
        .await
        .expect_err("too many lines");
    assert_eq!(code_of(&error), "PEER_PREVIEW_TOO_MANY_LINES");

    let at_limit = (0..5000).map(|_| "line\n").collect::<String>();
    std::fs::write(harness.share_root.join("limit.txt"), at_limit).expect("limit text");
    let preview = harness
        .peers
        .preview(&peer_id, &harness.share_id, "/limit.txt")
        .await
        .expect("line limit boundary");
    assert_eq!(preview.encoding, "utf8");
    assert_eq!(preview.data.lines().count(), 5000);

    harness.controller.stop().await;
}

#[tokio::test]
async fn preview_uses_suffixes_for_text_and_media() {
    let harness = Harness::new(vec![loopback_rule()], false).await;
    let (info, _code) = harness.connect().await;
    let peer_id = info.id;

    for name in [
        "app.php",
        "App.vue",
        "config.yml",
        "config.toml",
        "app.conf",
        ".env",
        ".htaccess",
        "README",
    ] {
        std::fs::write(harness.share_root.join(name), "key = value\n").expect("text file");
        let preview = harness
            .peers
            .preview(&peer_id, &harness.share_id, &format!("/{name}"))
            .await
            .unwrap_or_else(|error| panic!("{name} should be text: {error:?}"));
        assert_eq!(preview.encoding, "utf8", "{name}");
        assert_eq!(preview.data, "key = value\n", "{name}");
    }

    std::fs::write(harness.share_root.join("track.mp3"), b"ID3fake-audio").expect("audio");
    let audio = harness
        .peers
        .preview(&peer_id, &harness.share_id, "/track.mp3")
        .await
        .expect("audio preview");
    assert_eq!(audio.encoding, "base64");
    assert_eq!(audio.mime, "audio/mpeg");

    std::fs::write(harness.share_root.join("clip.mp4"), b"fake-video").expect("video");
    let video = harness
        .peers
        .preview(&peer_id, &harness.share_id, "/clip.mp4")
        .await
        .expect("video preview");
    assert_eq!(video.encoding, "base64");
    assert_eq!(video.mime, "video/mp4");

    std::fs::write(
        harness.share_root.join("payload.bin"),
        [0_u8, 159, 146, 150],
    )
    .expect("binary");
    let binary = harness
        .peers
        .preview(&peer_id, &harness.share_id, "/payload.bin")
        .await
        .expect("binary preview");
    assert_eq!(binary.encoding, "base64");
    assert_eq!(binary.mime, "application/octet-stream");

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
    let app_error = AppError::from_ref(&error);
    assert_eq!(app_error.code, "PEER_ACCESS_DENIED");
    assert!(
        app_error.message.contains("127.0.0.1"),
        "the error must name the address the server saw: {}",
        app_error.message
    );
    assert!(app_error.message.contains("允许规则"));
    assert_eq!(harness.controller.list_pending().len(), 0);
    assert_eq!(harness.controller.list_authorizations().len(), 0);
    harness.controller.stop().await;
}

/// 出站申请状态由后端内存持有：页面重建（前端重新调用同样命令）不会重复登记，
/// 审批通过后连上对端并清掉这条申请。
#[tokio::test]
async fn access_request_state_lives_in_the_backend() {
    let harness = Harness::new(vec![loopback_rule()], false).await;

    let first = harness
        .peers
        .begin_access(&harness.url, Some("lab"))
        .await
        .expect("begin access");
    assert_eq!(first.status, OutboundAccessStatus::Waiting);
    assert_eq!(first.label.as_deref(), Some("lab"));
    let snapshot = harness.peers.access_requests();
    assert_eq!(snapshot.len(), 1);
    assert_eq!(snapshot[0].url, harness.url);

    // 页面重载后前端会再次调用 begin_access：复用既有申请，不再朝对方登记一条。
    let again = harness
        .peers
        .begin_access(&harness.url, Some("lab"))
        .await
        .expect("begin access again");
    assert_eq!(again.status, OutboundAccessStatus::Waiting);
    assert_eq!(harness.controller.list_pending().len(), 1);

    let pending = harness.controller.list_pending();
    harness
        .controller
        .approve_pending(&pending[0].id)
        .expect("approve");
    let peer = harness
        .peers
        .await_access(&harness.url, Some("lab"))
        .await
        .expect("connect after approval");
    assert_eq!(peer.label, "lab");
    assert!(harness.peers.access_requests().is_empty());
    assert_eq!(harness.peers.list_peers().expect("saved peers").len(), 1);

    harness.controller.stop().await;
}

#[tokio::test]
async fn canceling_an_access_request_keeps_the_canceled_state() {
    let harness = Harness::new(vec![loopback_rule()], false).await;
    harness
        .peers
        .begin_access(&harness.url, None)
        .await
        .expect("begin access");

    let canceled = harness
        .peers
        .cancel_access_request(&harness.url)
        .await
        .expect("cancel access");
    assert_eq!(canceled.status, OutboundAccessStatus::Canceled);
    let snapshot = harness.peers.access_requests();
    assert_eq!(snapshot.len(), 1);
    assert_eq!(snapshot[0].status, OutboundAccessStatus::Canceled);

    // 取消之后可以重新申请，状态回到等待批准。
    let again = harness
        .peers
        .begin_access(&harness.url, None)
        .await
        .expect("request again");
    assert_eq!(again.status, OutboundAccessStatus::Waiting);

    harness.controller.stop().await;
}

#[tokio::test]
async fn failed_access_requests_keep_their_reason() {
    let harness = Harness::new(vec![loopback_rule()], false).await;
    let entry = harness
        .peers
        .begin_access("https://127.0.0.1:1", None)
        .await
        .expect("begin access");
    assert_eq!(entry.status, OutboundAccessStatus::Failed);
    assert!(
        entry
            .message
            .as_deref()
            .is_some_and(|message| !message.is_empty()),
        "a failed request must carry a user-facing reason"
    );
    assert_eq!(harness.peers.access_requests().len(), 1);

    harness.controller.stop().await;
}

#[cfg(debug_assertions)]
#[tokio::test]
async fn upload_progress_is_incremental_and_cancellation_stops_the_current_file() {
    use peerdav_lib::UploadPhase;
    let harness = Harness::new(vec![loopback_rule()], false).await;
    let (peer, _) = harness.connect().await;
    let source = harness.directory.path().join("progress.bin");
    let size = 21 * 1024 * 1024 + 17;
    std::fs::write(&source, vec![73u8; size]).expect("source");
    let id = harness.peers.begin_upload().await;
    let updates = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&updates);
    harness
        .peers
        .upload_with_progress(
            &peer.id,
            &harness.share_id,
            source.to_str().expect("path"),
            "/",
            ConflictPolicy::Fail,
            &id,
            Arc::new(move |phase, bytes, total, sent| {
                recorded
                    .lock()
                    .expect("updates")
                    .push((phase, bytes, total, sent))
            }),
        )
        .await
        .expect("upload");
    {
        let updates = updates.lock().expect("updates");
        assert!(
            updates
                .iter()
                .all(|(phase, _, _, _)| *phase != UploadPhase::Hashing),
            "v2 must start uploading without a full-file pre-scan"
        );
        assert!(updates.iter().any(
            |(phase, bytes, total, sent)| *phase == UploadPhase::Uploading
                && *bytes > 0
                && bytes < total
                && *sent > 0
        ));
        assert_eq!(
            updates.last().map(|u| (u.0, u.1)),
            Some((UploadPhase::Completed, size as u64))
        );
    }
    assert_eq!(
        std::fs::metadata(harness.share_root.join("progress.bin"))
            .expect("uploaded")
            .len(),
        size as u64
    );

    // Both preparation and an active request body can be interrupted.
    for cancel_before_data in [true, false] {
        let id = harness.peers.begin_upload().await;
        let (started, mut observed) = tokio::sync::mpsc::unbounded_channel();
        let progress = Arc::new(move |phase, bytes, _, _| {
            if phase == UploadPhase::Uploading && (cancel_before_data || bytes > 0) {
                started.send(()).expect("cancellation observer");
            }
        });
        let uploading = harness.peers.upload_with_progress(
            &peer.id,
            &harness.share_id,
            source.to_str().expect("path"),
            "/sub",
            ConflictPolicy::Fail,
            &id,
            progress,
        );
        let canceling = async {
            observed.recv().await.expect("phase started");
            let start = std::time::Instant::now();
            harness.peers.cancel_upload(&id).await;
            start
        };
        let (result, start) = tokio::time::timeout(Duration::from_secs(15), async {
            tokio::join!(uploading, canceling)
        })
        .await
        .expect("cancel must not wait for the file");
        assert!(
            start.elapsed() < Duration::from_secs(1),
            "cancellation was not prompt"
        );
        assert!(matches!(result, Err(DomainError::TransferCanceled)));
        assert!(!harness.share_root.join("sub/progress.bin").exists());
        let transfers = harness.directory.path().join("peerdav.transfers");
        assert_eq!(
            std::fs::read_dir(&transfers)
                .expect("transfer root")
                .count(),
            0,
            "cancel must clean all temporary parts"
        );
    }

    // Cancel at the commit boundary as well: the server must discard staged
    // content rather than finishing a save after the UI reports cancellation.
    let id = harness.peers.begin_upload().await;
    let (started, mut observed) = tokio::sync::mpsc::unbounded_channel();
    let uploading = harness.peers.upload_with_progress(
        &peer.id,
        &harness.share_id,
        source.to_str().expect("path"),
        "/sub",
        ConflictPolicy::Fail,
        &id,
        Arc::new(move |phase, _, _, _| {
            if phase == UploadPhase::Committing {
                started.send(()).expect("commit observer");
            }
        }),
    );
    let canceling = async {
        observed.recv().await.expect("commit started");
        harness.peers.cancel_upload(&id).await;
    };
    let (result, ()) = tokio::join!(uploading, canceling);
    assert!(matches!(result, Err(DomainError::TransferCanceled)));
    assert!(!harness.share_root.join("sub/progress.bin").exists());
    assert_eq!(
        std::fs::read_dir(harness.directory.path().join("peerdav.transfers"))
            .expect("transfer root")
            .count(),
        0
    );

    // A cancel received before the upload command is polled is not lost.
    let id = harness.peers.begin_upload().await;
    harness.peers.cancel_upload(&id).await;
    let result = harness
        .peers
        .upload_with_progress(
            &peer.id,
            &harness.share_id,
            source.to_str().expect("path"),
            "/sub",
            ConflictPolicy::Fail,
            &id,
            Arc::new(|_, _, _, _| panic!("canceled before start")),
        )
        .await;
    assert!(matches!(result, Err(DomainError::TransferCanceled)));
    harness.controller.stop().await;
}

#[tokio::test]
async fn multipart_round_trip_handles_boundaries_and_empty_files() {
    let harness = Harness::new(vec![loopback_rule()], false).await;
    let (peer, _) = harness.connect().await;
    let source = harness.directory.path().join("large.bin");
    let payload: Vec<u8> = (0..(21 * 1024 * 1024 + 17))
        .map(|n| (n % 251) as u8)
        .collect();
    std::fs::write(&source, &payload).expect("source");
    harness
        .peers
        .upload(
            &peer.id,
            &harness.share_id,
            source.to_str().expect("path"),
            "/",
            ConflictPolicy::Overwrite,
        )
        .await
        .expect("multipart upload");
    assert_eq!(
        std::fs::read(harness.share_root.join("large.bin")).expect("uploaded"),
        payload
    );
    let destination = harness.directory.path().join("download.bin");
    harness
        .peers
        .download_to(
            &peer.id,
            &harness.share_id,
            "/large.bin",
            destination.to_str().expect("path"),
        )
        .await
        .expect("parallel download");
    assert_eq!(std::fs::read(&destination).expect("downloaded"), payload);
    std::fs::write(&source, b"").expect("empty source");
    harness
        .peers
        .upload(
            &peer.id,
            &harness.share_id,
            source.to_str().expect("path"),
            "/",
            ConflictPolicy::Overwrite,
        )
        .await
        .expect("empty upload");
    harness
        .peers
        .download_to(
            &peer.id,
            &harness.share_id,
            "/large.bin",
            destination.to_str().expect("path"),
        )
        .await
        .expect("empty download");
    assert!(std::fs::read(&destination).expect("empty").is_empty());
    harness.controller.stop().await;
}

#[tokio::test]
async fn remote_upload_cancel_cleans_parts_rejects_late_requests_and_preserves_completed_files() {
    use sha2::{Digest, Sha256};
    let harness = Harness::new(vec![loopback_rule()], false).await;
    let (_, code) = harness.connect().await;
    let client = harness
        .vault
        .encrypted_client(harness.vault.fingerprint())
        .expect("client");
    let payload = b"cancel remote upload";
    let digest = hex::encode(Sha256::digest(payload));
    let target = format!("{}/share/{}/cancel.txt", harness.url, harness.share_id);
    std::fs::write(harness.share_root.join("cancel.txt"), b"original").expect("original");
    let request = |method: reqwest::Method, session: &str| {
        client
            .request(method, &target)
            .basic_auth("peerdav", Some(&code))
            .header("x-peerdav-transfer", &digest)
            .header("x-peerdav-size", payload.len())
            .header("x-peerdav-session", session)
            .header("x-peerdav-part", "0")
    };
    assert_eq!(
        request(reqwest::Method::PATCH, "aa")
            .body(payload.to_vec())
            .send()
            .await
            .expect("part")
            .status(),
        reqwest::StatusCode::NO_CONTENT
    );
    // An unauthenticated request cannot cancel another device's upload.
    let foreign_vault =
        Vault::open(&harness.directory.path().join("foreign.pd")).expect("foreign device");
    let foreign_client = foreign_vault
        .encrypted_client(harness.vault.fingerprint())
        .expect("foreign TLS client");
    assert_eq!(
        foreign_client
            .delete(&target)
            .header("x-peerdav-transfer", &digest)
            .header("x-peerdav-size", payload.len())
            .header("x-peerdav-cancel", "1")
            .send()
            .await
            .expect("unauthorized cancel")
            .status(),
        reqwest::StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        request(reqwest::Method::HEAD, "aa")
            .send()
            .await
            .expect("still exists")
            .status(),
        reqwest::StatusCode::NO_CONTENT
    );
    assert_eq!(
        request(reqwest::Method::DELETE, "aa")
            .header("x-peerdav-cancel", "1")
            .send()
            .await
            .expect("cancel")
            .status(),
        reqwest::StatusCode::NO_CONTENT
    );
    assert_eq!(
        request(reqwest::Method::PATCH, "aa")
            .body(payload.to_vec())
            .send()
            .await
            .expect("late part")
            .status(),
        reqwest::StatusCode::CONFLICT
    );
    assert_eq!(
        std::fs::read_dir(harness.directory.path().join("peerdav.transfers"))
            .expect("transfer root")
            .count(),
        0
    );
    assert_eq!(
        std::fs::read(harness.share_root.join("cancel.txt")).expect("original"),
        b"original"
    );
    // A fresh attempt works, and cancellation after atomic replacement must
    // never delete the successfully saved destination.
    assert!(
        request(reqwest::Method::PATCH, "bb")
            .body(payload.to_vec())
            .send()
            .await
            .expect("retry")
            .status()
            .is_success()
    );
    assert_eq!(
        request(reqwest::Method::PUT, "bb")
            .header("x-peerdav-commit", "1")
            .send()
            .await
            .expect("commit")
            .status(),
        reqwest::StatusCode::CREATED
    );
    assert_eq!(
        request(reqwest::Method::DELETE, "bb")
            .header("x-peerdav-cancel", "1")
            .send()
            .await
            .expect("late cancel")
            .status(),
        reqwest::StatusCode::OK
    );
    assert_eq!(
        std::fs::read(harness.share_root.join("cancel.txt")).expect("saved"),
        payload
    );
    assert!(
        !std::fs::read_dir(&harness.share_root)
            .expect("share")
            .any(|entry| entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .starts_with(".peerdav-"))
    );
    harness.controller.stop().await;
}

#[tokio::test]
async fn upload_parts_survive_restart_and_reject_wrong_content() {
    use sha2::{Digest, Sha256};
    let harness = Harness::new(vec![loopback_rule()], false).await;
    let (_, code) = harness.connect().await;
    let client = harness
        .vault
        .encrypted_client(harness.vault.fingerprint())
        .expect("TLS client");
    let payload = b"resume verified upload";
    let digest = hex::encode(Sha256::digest(payload));
    let target = format!("{}/share/{}/resume.txt", harness.url, harness.share_id);
    let request = |method: reqwest::Method| {
        client
            .request(method, &target)
            .basic_auth("peerdav", Some(&code))
            .header("x-peerdav-transfer", &digest)
            .header("x-peerdav-size", payload.len())
            .header("x-peerdav-part", "0")
    };
    assert_eq!(
        request(reqwest::Method::PATCH)
            .body(payload.to_vec())
            .send()
            .await
            .expect("part")
            .status(),
        reqwest::StatusCode::NO_CONTENT
    );
    assert!(!harness.share_root.join("resume.txt").exists());
    harness.controller.stop().await;
    let port = url::Url::parse(&harness.url)
        .expect("url")
        .port()
        .expect("port");
    harness
        .controller
        .start(ServerConfig {
            bind_address: "127.0.0.1".into(),
            port,
            ..ServerConfig::default()
        })
        .await
        .expect("restart");
    assert_eq!(
        request(reqwest::Method::HEAD)
            .send()
            .await
            .expect("persisted part")
            .status(),
        reqwest::StatusCode::NO_CONTENT
    );
    assert!(
        request(reqwest::Method::PUT)
            .header("x-peerdav-commit", "1")
            .send()
            .await
            .expect("commit")
            .status()
            .is_success()
    );
    assert_eq!(
        std::fs::read(harness.share_root.join("resume.txt")).expect("committed"),
        payload
    );
    assert!(
        request(reqwest::Method::PATCH)
            .body(vec![b'x'; payload.len()])
            .send()
            .await
            .expect("wrong bytes")
            .status()
            .is_success()
    );
    assert_eq!(
        request(reqwest::Method::PUT)
            .header("x-peerdav-commit", "1")
            .send()
            .await
            .expect("reject wrong hash")
            .status(),
        reqwest::StatusCode::BAD_REQUEST
    );
    assert_eq!(
        std::fs::read(harness.share_root.join("resume.txt")).expect("original retained"),
        payload
    );
    harness.controller.stop().await;
}

#[tokio::test]
async fn streaming_upload_verifies_resumed_parts_and_final_digest() {
    use sha2::{Digest, Sha256};
    let harness = Harness::new(vec![loopback_rule()], false).await;
    let (_, code) = harness.connect().await;
    let client = harness
        .vault
        .encrypted_client(harness.vault.fingerprint())
        .expect("client");
    let payload = b"streamed content";
    let digest = hex::encode(Sha256::digest(payload));
    let target = format!("{}/share/{}/stream.txt", harness.url, harness.share_id);
    let request = |method: reqwest::Method| {
        client
            .request(method, &target)
            .basic_auth("peerdav", Some(&code))
            .header("x-peerdav-transfer", "ab".repeat(32))
            .header("x-peerdav-size", payload.len())
            .header("x-peerdav-part", "0")
            .header("x-peerdav-upload-version", "2")
    };
    std::fs::write(harness.share_root.join("stream.txt"), b"original").expect("original");
    assert_eq!(
        request(reqwest::Method::PATCH)
            .header("x-peerdav-part-sha256", "00".repeat(32))
            .body(payload.to_vec())
            .send()
            .await
            .expect("bad part")
            .status(),
        reqwest::StatusCode::BAD_REQUEST
    );
    assert_eq!(
        request(reqwest::Method::PATCH)
            .header("x-peerdav-part-sha256", &digest)
            .body(payload.to_vec())
            .send()
            .await
            .expect("part")
            .status(),
        reqwest::StatusCode::NO_CONTENT
    );
    assert_eq!(
        request(reqwest::Method::HEAD)
            .header("x-peerdav-part-sha256", "00".repeat(32))
            .send()
            .await
            .expect("mismatched resume")
            .status(),
        reqwest::StatusCode::NOT_FOUND
    );
    assert_eq!(
        request(reqwest::Method::HEAD)
            .header("x-peerdav-part-sha256", &digest)
            .send()
            .await
            .expect("verified resume")
            .status(),
        reqwest::StatusCode::NO_CONTENT
    );
    for expected in [None, Some("00".repeat(32))] {
        let mut commit = request(reqwest::Method::PUT).header("x-peerdav-commit", "1");
        if let Some(digest) = expected {
            commit = commit.header("x-peerdav-content-sha256", digest);
        }
        assert_eq!(
            commit.send().await.expect("invalid commit").status(),
            reqwest::StatusCode::BAD_REQUEST
        );
        assert_eq!(
            std::fs::read(harness.share_root.join("stream.txt")).expect("preserved"),
            b"original"
        );
        assert!(
            !std::fs::read_dir(&harness.share_root)
                .expect("share")
                .any(|entry| entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".peerdav-"))
        );
    }
    assert_eq!(
        request(reqwest::Method::PUT)
            .header("x-peerdav-commit", "1")
            .header("x-peerdav-content-sha256", &digest)
            .send()
            .await
            .expect("commit")
            .status(),
        reqwest::StatusCode::CREATED
    );
    assert_eq!(
        std::fs::read(harness.share_root.join("stream.txt")).expect("saved"),
        payload
    );
    harness.controller.stop().await;
}

#[tokio::test]
async fn download_reuses_verified_parts_and_repairs_corrupt_parts() {
    let harness = Harness::new(vec![loopback_rule()], false).await;
    let (peer, _) = harness.connect().await;
    let payload = vec![42u8; 11 * 1024 * 1024 + 9];
    std::fs::write(harness.share_root.join("resume.bin"), &payload).expect("remote");
    let output = harness.directory.path().join("downloads");
    std::fs::create_dir(&output).expect("output");
    std::fs::write(output.join("resume.bin"), b"original").expect("existing destination");
    let error = harness
        .peers
        .download(
            &peer.id,
            &harness.share_id,
            "/resume.bin",
            output.to_str().expect("path"),
            ConflictPolicy::Fail,
        )
        .await
        .expect_err("conflict preserves original");
    assert_eq!(code_of(&error), "TRANSFER_CONFLICT");
    assert_eq!(
        std::fs::read(output.join("resume.bin")).expect("original"),
        b"original"
    );
    // Pre-seed persisted parts through a failed commit: they must be verified
    // before reuse, not trusted merely because their size is right.
    let destination = output.join("blocked");
    let blocked = destination.clone();
    let progress = Arc::new(move |done, total| {
        if done == total && !blocked.exists() {
            std::fs::create_dir(&blocked).expect("block commit after download");
        }
    });
    harness
        .peers
        .download_to_with_progress(
            &peer.id,
            &harness.share_id,
            "/resume.bin",
            destination.to_str().expect("path"),
            Some(progress),
        )
        .await
        .expect_err("blocked destination");
    let staging = std::fs::read_dir(&output)
        .expect("staging entries")
        .filter_map(Result::ok)
        .find(|entry| entry.file_name().to_string_lossy().starts_with(".peerdav-"))
        .expect("persistent parts")
        .path();
    std::fs::write(staging.join("0"), vec![0u8; 10 * 1024 * 1024])
        .expect("corrupt same-length part");
    std::fs::remove_dir(&destination).expect("unblock commit");
    harness
        .peers
        .download_to(
            &peer.id,
            &harness.share_id,
            "/resume.bin",
            destination.to_str().expect("path"),
        )
        .await
        .expect("resume and repair");
    assert_eq!(std::fs::read(&destination).expect("download"), payload);
    assert!(
        !staging.exists(),
        "successful transfer removes staged parts"
    );
    harness.controller.stop().await;
}

#[tokio::test]
async fn connection_snapshots_keep_identity_across_approval_and_cancel() {
    let harness = Harness::new(vec![loopback_rule()], false).await;
    let fingerprint = harness.vault.fingerprint().to_owned();
    harness
        .peers
        .begin_access(&harness.url, Some("snapshot peer"))
        .await
        .expect("request");
    let outbound = harness
        .peers
        .outbound_connections()
        .expect("outbound snapshot");
    assert_eq!(outbound.len(), 1);
    assert_eq!(outbound[0].fingerprint, fingerprint);
    assert_eq!(
        serde_json::to_value(&outbound[0]).expect("serialize")["state"],
        "pending"
    );
    let inbound = harness
        .controller
        .inbound_connections()
        .expect("inbound snapshot");
    assert_eq!(inbound.len(), 1);
    assert_eq!(inbound[0].fingerprint, fingerprint);
    assert!(!inbound[0].authorized);
    harness
        .peers
        .cancel_access_request(&harness.url)
        .await
        .expect("cancel");
    let outbound = harness
        .peers
        .outbound_connections()
        .expect("canceled snapshot");
    assert_eq!(
        serde_json::to_value(&outbound[0]).expect("serialize")["state"],
        "canceled"
    );
    harness
        .peers
        .begin_access(&harness.url, None)
        .await
        .expect("request again");
    let pending = harness.controller.list_pending();
    harness
        .controller
        .approve_pending(&pending[0].id)
        .expect("approve");
    harness
        .peers
        .await_access(&harness.url, None)
        .await
        .expect("paired");
    let outbound = harness
        .peers
        .outbound_connections()
        .expect("paired snapshot");
    assert_eq!(outbound.len(), 1);
    assert!(outbound[0].authorized);
    assert_eq!(outbound[0].fingerprint, fingerprint);
    assert!(
        harness
            .controller
            .inbound_connections()
            .expect("approved snapshot")[0]
            .authorized
    );
    let authorization = harness.controller.list_authorizations()[0].id.clone();
    harness
        .controller
        .revoke_authorization(&authorization)
        .expect("revoke");
    assert!(
        !harness
            .controller
            .inbound_connections()
            .expect("revoked snapshot")[0]
            .authorized
    );
    harness.controller.stop().await;
}

#[tokio::test]
async fn manual_pairing_probes_fingerprint_and_never_replaces_an_existing_pin() {
    let harness = Harness::new(Vec::new(), false).await;
    let client_directory = dev_test_dir("manual_pairing");
    let client_vault =
        Arc::new(Vault::open(&client_directory.path().join("client.pd")).expect("client vault"));
    client_vault
        .initialize(&key_dir(client_directory.path()), "owner", ADMIN_PASSWORD)
        .expect("init");
    let peers = PeerManager::new(
        Arc::clone(&client_vault),
        client_directory.path().join("temp"),
    )
    .expect("peers");
    let url = harness.url.replace("127.0.0.1", "localhost");
    let request = peers
        .begin_access(&url, None)
        .await
        .expect("probe and request");
    assert_eq!(
        request.fingerprint.as_deref(),
        Some(harness.vault.fingerprint())
    );
    let pinned = PeerManager::new(client_vault, client_directory.path().join("pinned-temp"))
        .expect("pinned peers");
    pinned
        .set_pairing_fingerprint(&url, &"a".repeat(64))
        .expect("pin");
    let request = pinned
        .begin_access(&url, None)
        .await
        .expect("record failure");
    assert_ne!(
        request.fingerprint.as_deref(),
        Some(harness.vault.fingerprint())
    );
    harness.controller.stop().await;
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}
