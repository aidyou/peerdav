//! UDP LAN discovery.
//!
//! A server answers probe datagrams and periodically announces itself to every
//! directed broadcast address of the local interfaces. The client keeps a
//! passive listener on the discovery port: announcements join a live registry,
//! and a server that stops announcing expires after
//! [`DISCOVERY_PEER_TTL`](crate::types::DISCOVERY_PEER_TTL). The older active
//! scan remains available for one-shot diagnostics.
//!
//! Discovery intentionally exposes only the server name, port and share
//! count. Share ids are only returned by the authenticated catalog endpoint.

use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use futures_util::stream::{self, StreamExt};
use if_addrs::{IfAddr, get_if_addrs};
use serde::{Deserialize, Serialize};
use socket2::{Domain, Protocol, Socket, Type};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UdpSocket};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::{Instant, MissedTickBehavior, interval, timeout, timeout_at};
use tokio_rustls::TlsConnector;

use crate::error::DomainError;
use crate::fs::ShareMap;
use crate::traits::PeerEvents;
use crate::types::{
    CATALOG_PATH, DISCOVERY_ANNOUNCE_INTERVAL, DISCOVERY_ANNOUNCE_KIND, DISCOVERY_PEER_TTL,
    DISCOVERY_PROBE_KIND, DISCOVERY_PROTOCOL, DISCOVERY_PRUNE_INTERVAL, DISCOVERY_RECV_BUFFER,
    DISCOVERY_SCAN_TIMEOUT, DeviceIdentity, DiscoveredPeer, MAX_DISCOVERY_HOSTS,
};

const PROBE_INTERVAL: Duration = Duration::from_millis(250);
const DISCOVERY_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);
/// TCP fallback for networks where UDP broadcast is not forwarded (EasyTier,
/// WireGuard, most L3 overlays). Only private interface subnets are scanned.
pub(crate) const TCP_SCAN_TIMEOUT: Duration = Duration::from_millis(450);
const TCP_SCAN_CONCURRENCY: usize = 64;
const TCP_SCAN_TARGET_LIMIT: usize = 1024;
/// Scanned peers survive longer than UDP announcements because a scan is an
/// explicit user action and the overlay may never deliver an announcement.
const TCP_DISCOVERY_TTL: Duration = Duration::from_secs(120);

#[derive(Debug, Serialize, Deserialize)]
struct WireProbe {
    protocol: String,
    kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct WireAnnouncement {
    protocol: String,
    kind: String,
    server_id: String,
    name: String,
    port: u16,
    share_count: usize,
    hosts: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct WireEnvelope {
    protocol: String,
    kind: String,
}

/// A running UDP discovery responder.
pub struct DiscoveryServer {
    port: u16,
    shutdown: Option<oneshot::Sender<()>>,
    task: JoinHandle<()>,
}

impl DiscoveryServer {
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Stops the responder and waits briefly for its task to finish.
    pub async fn stop(mut self) {
        if let Some(sender) = self.shutdown.take() {
            let _ = sender.send(());
        }
        match timeout(DISCOVERY_SHUTDOWN_TIMEOUT, &mut self.task).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) if !error.is_cancelled() => {
                log::warn!("peerdav.discovery task ended with an error: {error}");
            }
            Ok(Err(_)) => {}
            Err(_) => {
                log::warn!("peerdav.discovery did not stop in time; aborting task");
                self.task.abort();
            }
        }
    }

    /// Best-effort synchronous shutdown for application exit.
    pub fn request_shutdown(&mut self) {
        if let Some(sender) = self.shutdown.take() {
            let _ = sender.send(());
        }
        self.task.abort();
    }
}

/// Identity a running WebDAV server publishes to the LAN. `shares` is shared
/// with the server so an announcement always carries the live share count.
pub(crate) struct LocalAdvert {
    pub(crate) server_id: String,
    pub(crate) name: String,
    pub(crate) port: u16,
    pub(crate) hosts: Vec<String>,
    pub(crate) shares: Arc<RwLock<ShareMap>>,
}

#[derive(Clone)]
struct PeerRecord {
    peer: DiscoveredPeer,
    last_seen: Instant,
    /// `None` uses the listener's UDP TTL. TCP-scanned entries use a longer
    /// TTL because they are not refreshed by announcements.
    ttl: Option<Duration>,
    /// A TCP-observed address must not be replaced by advertised NIC addresses.
    verified_url: Option<String>,
    udp: bool,
}

/// A long-running UDP listener that feeds the passive discovery registry.
///
/// The same socket also answers probe datagrams and broadcasts the attached
/// server advert, so the application never needs a second socket bound to the
/// discovery port. A server that stops announcing expires after `ttl` and is
/// removed from the registry.
pub(crate) struct DiscoveryListener {
    port: u16,
    peers: Arc<RwLock<HashMap<String, PeerRecord>>>,
    advert: Arc<RwLock<Option<LocalAdvert>>>,
    events: Arc<dyn PeerEvents>,
    shutdown: std::sync::Mutex<Option<oneshot::Sender<()>>>,
    task: JoinHandle<()>,
}

impl DiscoveryListener {
    pub(crate) fn start(port: u16, events: Arc<dyn PeerEvents>) -> Result<Self, DomainError> {
        Self::start_with_ttl(port, DISCOVERY_PEER_TTL, events)
    }

    fn start_with_ttl(
        port: u16,
        ttl: Duration,
        events: Arc<dyn PeerEvents>,
    ) -> Result<Self, DomainError> {
        if port == 0 {
            return Err(DomainError::Discovery {
                operation: "listen",
                detail: "discovery port must not be zero".to_owned(),
            });
        }

        // `UdpSocket::from_std` requires an entered Tokio runtime context; fail
        // with a structured error instead of panicking when called from a
        // synchronous setup path.
        let runtime =
            tokio::runtime::Handle::try_current().map_err(|error| DomainError::Discovery {
                operation: "listen",
                detail: format!("no async runtime for the discovery listener: {error}"),
            })?;
        let std_socket = bind_shared_socket(port)?;
        let socket = UdpSocket::from_std(std_socket).map_err(|source| DomainError::Discovery {
            operation: "listen",
            detail: format!("failed to attach UDP socket {port}: {source}"),
        })?;
        let peers = Arc::new(RwLock::new(HashMap::new()));
        let advert = Arc::new(RwLock::new(None));
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let task = runtime.spawn(run_listener(
            socket,
            port,
            Arc::clone(&peers),
            Arc::clone(&advert),
            ttl,
            Arc::clone(&events),
            shutdown_rx,
        ));
        // Clear a stale frontend list when the listener starts or rebinds.
        events.discovered_peers(&[]);

        Ok(Self {
            port,
            peers,
            advert,
            events,
            shutdown: std::sync::Mutex::new(Some(shutdown_tx)),
            task,
        })
    }

    pub(crate) fn port(&self) -> u16 {
        self.port
    }

    pub(crate) fn set_advert(&self, advert: Option<LocalAdvert>) {
        match self.advert.write() {
            Ok(mut slot) => *slot = advert,
            Err(_) => log::warn!("peerdav.discovery advert lock poisoned"),
        }
    }

    pub(crate) fn clear_advert(&self) {
        self.set_advert(None);
    }

    pub(crate) fn peers(&self) -> Vec<DiscoveredPeer> {
        match self.peers.read() {
            Ok(registry) => sorted_peers(registry.values().map(|record| record.peer.clone())),
            Err(_) => {
                log::warn!("peerdav.discovery registry lock poisoned");
                Vec::new()
            }
        }
    }

    /// Adds TCP-scanned peers to the same registry the passive UDP listener
    /// uses. A later UDP announcement upgrades the metadata and TTL in place.
    pub(crate) fn merge_peers(&self, peers: &[DiscoveredPeer]) {
        let now = Instant::now();
        let changed = match self.peers.write() {
            Ok(mut registry) => {
                let mut changed = false;
                for peer in peers {
                    if peer.server_id.is_empty() {
                        continue;
                    }
                    if let Some(record) = registry.get_mut(&peer.server_id) {
                        let previous = record.peer.clone();
                        record.verified_url = Some(peer.url.clone());
                        record.peer.url = peer.url.clone();
                        for host in &peer.hosts {
                            push_host(&mut record.peer.hosts, host.clone());
                        }
                        if !record.udp {
                            record.peer.name = peer.name.clone();
                        }
                        changed |= previous != record.peer;
                        record.last_seen = now;
                    } else {
                        registry.insert(
                            peer.server_id.clone(),
                            PeerRecord {
                                peer: peer.clone(),
                                last_seen: now,
                                ttl: Some(TCP_DISCOVERY_TTL),
                                verified_url: Some(peer.url.clone()),
                                udp: false,
                            },
                        );
                        changed = true;
                    }
                }
                changed
            }
            Err(_) => {
                log::warn!("peerdav.discovery registry lock poisoned");
                false
            }
        };
        if changed {
            emit_snapshot(&self.peers, self.events.as_ref());
        }
    }

    /// Best-effort synchronous shutdown used when rebinding or exiting.
    pub(crate) fn request_shutdown(&self) {
        if let Ok(mut sender) = self.shutdown.lock()
            && let Some(sender) = sender.take()
        {
            let _ = sender.send(());
        }
        self.task.abort();
    }
}

impl Drop for DiscoveryListener {
    fn drop(&mut self) {
        self.request_shutdown();
    }
}

async fn run_listener(
    socket: UdpSocket,
    discovery_port: u16,
    peers: Arc<RwLock<HashMap<String, PeerRecord>>>,
    advert: Arc<RwLock<Option<LocalAdvert>>>,
    ttl: Duration,
    events: Arc<dyn PeerEvents>,
    mut shutdown: oneshot::Receiver<()>,
) {
    let mut announce = interval(DISCOVERY_ANNOUNCE_INTERVAL);
    announce.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut prune = interval(DISCOVERY_PRUNE_INTERVAL);
    prune.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut buffer = vec![0_u8; DISCOVERY_RECV_BUFFER];

    loop {
        tokio::select! {
            _ = &mut shutdown => break,
            _ = announce.tick() => {
                if let Some(announcement) = current_announcement(&advert) {
                    send_announcement(&socket, &announcement, discovery_port).await;
                }
            }
            _ = prune.tick() => {
                let removed = match peers.write() {
                    Ok(mut registry) => prune_expired(&mut registry, Instant::now(), ttl),
                    Err(_) => {
                        log::warn!("peerdav.discovery registry lock poisoned");
                        false
                    }
                };
                if removed {
                    emit_snapshot(&peers, events.as_ref());
                }
            }
            received = socket.recv_from(&mut buffer) => {
                match received {
                    Ok((length, peer)) => {
                        handle_datagram(&socket, &peers, &advert, events.as_ref(), &buffer[..length], peer, ttl).await;
                    }
                    Err(error) => {
                        log::warn!("peerdav.discovery receive failed: {error}");
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                }
            }
        }
    }
}

async fn handle_datagram(
    socket: &UdpSocket,
    peers: &RwLock<HashMap<String, PeerRecord>>,
    advert: &RwLock<Option<LocalAdvert>>,
    events: &dyn PeerEvents,
    payload: &[u8],
    from: SocketAddr,
    ttl: Duration,
) {
    let Ok(envelope) = serde_json::from_slice::<WireEnvelope>(payload) else {
        return;
    };
    if envelope.protocol != DISCOVERY_PROTOCOL {
        return;
    }

    if envelope.kind == DISCOVERY_PROBE_KIND {
        if let Some(announcement) = current_announcement(advert) {
            let reply = match serde_json::to_vec(&announcement) {
                Ok(reply) => reply,
                Err(error) => {
                    log::error!("peerdav.discovery failed to encode announcement: {error}");
                    return;
                }
            };
            if let Err(error) = socket.send_to(&reply, from).await {
                log::debug!("peerdav.discovery failed to answer {from}: {error}");
            }
        }
        return;
    }

    if envelope.kind != DISCOVERY_ANNOUNCE_KIND {
        return;
    }
    let Some(announcement) = parse_announcement(payload) else {
        return;
    };
    let changed = match peers.write() {
        Ok(mut registry) => upsert_live(&mut registry, announcement, from, Instant::now(), ttl),
        Err(_) => {
            log::warn!("peerdav.discovery registry lock poisoned");
            false
        }
    };
    if changed {
        emit_snapshot(peers, events);
    }
}

fn emit_snapshot(peers: &RwLock<HashMap<String, PeerRecord>>, events: &dyn PeerEvents) {
    let snapshot = match peers.read() {
        Ok(registry) => sorted_peers(registry.values().map(|record| record.peer.clone())),
        Err(_) => {
            log::warn!("peerdav.discovery registry lock poisoned");
            return;
        }
    };
    events.discovered_peers(&snapshot);
}

fn current_announcement(advert: &RwLock<Option<LocalAdvert>>) -> Option<WireAnnouncement> {
    let advert = advert.read().ok()?;
    advert.as_ref().map(announcement_for_advert)
}

fn announcement_for_advert(advert: &LocalAdvert) -> WireAnnouncement {
    WireAnnouncement {
        protocol: DISCOVERY_PROTOCOL.to_owned(),
        kind: DISCOVERY_ANNOUNCE_KIND.to_owned(),
        server_id: advert.server_id.clone(),
        name: advert.name.clone(),
        port: advert.port,
        share_count: share_count(&advert.shares),
        hosts: advert.hosts.clone(),
    }
}

fn prune_expired(registry: &mut HashMap<String, PeerRecord>, now: Instant, ttl: Duration) -> bool {
    let before = registry.len();
    registry.retain(|_, record| now.duration_since(record.last_seen) <= record.ttl.unwrap_or(ttl));
    before != registry.len()
}

fn announcement_from_source(
    mut announcement: WireAnnouncement,
    from: SocketAddr,
) -> WireAnnouncement {
    // The datagram's source is the route that reached this machine. Advertised
    // NIC addresses may belong to unrelated physical/container networks.
    let source = from.ip();
    if !source.is_unspecified() && !source.is_loopback() {
        let host = source.to_string();
        announcement.hosts.retain(|entry| entry != &host);
        announcement.hosts.insert(0, host);
        announcement.hosts.truncate(MAX_DISCOVERY_HOSTS);
    }
    announcement
}

fn upsert_live(
    registry: &mut HashMap<String, PeerRecord>,
    announcement: WireAnnouncement,
    from: SocketAddr,
    now: Instant,
    ttl: Duration,
) -> bool {
    let announcement = announcement_from_source(announcement, from);
    let key = announcement_key(&announcement, from);
    if let Some(record) = registry.get_mut(&key) {
        let previous = record.peer.clone();
        update_discovered_peer(&mut record.peer, &announcement);
        if let Some(url) = &record.verified_url {
            if let Ok(parsed) = url::Url::parse(url) {
                if let Some(host) = parsed.host_str() {
                    push_host(&mut record.peer.hosts, host.to_owned());
                }
            }
            record.peer.url = url.clone();
        }
        record.udp = true;
        record.last_seen = now;
        record.ttl = None;
        return record.peer != previous;
    }
    registry.insert(
        key,
        PeerRecord {
            peer: new_discovered_peer(&announcement),
            last_seen: now,
            ttl: Some(ttl),
            verified_url: None,
            udp: true,
        },
    );
    true
}

fn bind_shared_socket(port: u16) -> Result<std::net::UdpSocket, DomainError> {
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP)).map_err(|source| {
        DomainError::Discovery {
            operation: "create socket",
            detail: source.to_string(),
        }
    })?;
    socket
        .set_reuse_address(true)
        .map_err(|source| DomainError::Discovery {
            operation: "reuse address",
            detail: source.to_string(),
        })?;
    #[cfg(unix)]
    socket
        .set_reuse_port(true)
        .map_err(|source| DomainError::Discovery {
            operation: "reuse port",
            detail: source.to_string(),
        })?;
    socket
        .set_broadcast(true)
        .map_err(|source| DomainError::Discovery {
            operation: "enable broadcast",
            detail: source.to_string(),
        })?;
    let address = SocketAddr::from((Ipv4Addr::UNSPECIFIED, port));
    socket
        .bind(&address.into())
        .map_err(|source| DomainError::Discovery {
            operation: "bind",
            detail: format!("failed to bind UDP port {port}: {source}"),
        })?;
    socket
        .set_nonblocking(true)
        .map_err(|source| DomainError::Discovery {
            operation: "set nonblocking",
            detail: source.to_string(),
        })?;
    Ok(socket.into())
}

#[derive(Debug, Clone)]
struct ServerIdentity {
    server_id: String,
    name: String,
    webdav_port: u16,
    hosts: Vec<String>,
}

/// Starts the responder on `0.0.0.0:<port>`.
pub async fn start(
    port: u16,
    server_id: String,
    name: String,
    webdav_port: u16,
    hosts: Vec<String>,
    shares: Arc<RwLock<ShareMap>>,
) -> Result<DiscoveryServer, DomainError> {
    if port == 0 {
        return Err(DomainError::Discovery {
            operation: "bind",
            detail: "discovery port must not be zero".to_owned(),
        });
    }

    let std_socket = bind_shared_socket(port)?;
    let local_port = std_socket
        .local_addr()
        .map_err(|source| DomainError::Discovery {
            operation: "inspect socket",
            detail: source.to_string(),
        })?
        .port();
    let socket = UdpSocket::from_std(std_socket).map_err(|source| DomainError::Discovery {
        operation: "bind",
        detail: format!("failed to attach UDP socket {port}: {source}"),
    })?;

    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let identity = Arc::new(ServerIdentity {
        server_id,
        name,
        webdav_port,
        hosts,
    });
    let task = tokio::spawn(async move {
        run_responder(socket, local_port, identity, shares, shutdown_rx).await;
    });

    Ok(DiscoveryServer {
        port,
        shutdown: Some(shutdown_tx),
        task,
    })
}

async fn run_responder(
    socket: UdpSocket,
    discovery_port: u16,
    identity: Arc<ServerIdentity>,
    shares: Arc<RwLock<ShareMap>>,
    mut shutdown: oneshot::Receiver<()>,
) {
    let mut ticker = interval(DISCOVERY_ANNOUNCE_INTERVAL);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut buffer = vec![0_u8; DISCOVERY_RECV_BUFFER];

    loop {
        tokio::select! {
            _ = &mut shutdown => break,
            _ = ticker.tick() => {
                let announcement = build_announcement(&identity, share_count(&shares));
                send_announcement(&socket, &announcement, discovery_port).await;
            }
            received = socket.recv_from(&mut buffer) => {
                match received {
                    Ok((length, peer)) => {
                        if let Ok(probe) = serde_json::from_slice::<WireProbe>(&buffer[..length])
                            && probe.protocol == DISCOVERY_PROTOCOL
                            && probe.kind == DISCOVERY_PROBE_KIND
                        {
                            let announcement = build_announcement(&identity, share_count(&shares));
                            let payload = match serde_json::to_vec(&announcement) {
                                Ok(payload) => payload,
                                Err(error) => {
                                    log::error!("peerdav.discovery failed to encode announcement: {error}");
                                    continue;
                                }
                            };
                            if let Err(error) = socket.send_to(&payload, peer).await {
                                log::debug!("peerdav.discovery failed to answer {peer}: {error}");
                            }
                        }
                    }
                    Err(error) => {
                        log::warn!("peerdav.discovery receive failed: {error}");
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                }
            }
        }
    }
}

async fn send_announcement(
    socket: &UdpSocket,
    announcement: &WireAnnouncement,
    discovery_port: u16,
) {
    let payload = match serde_json::to_vec(announcement) {
        Ok(payload) => payload,
        Err(error) => {
            log::error!("peerdav.discovery failed to encode announcement: {error}");
            return;
        }
    };

    for target in broadcast_targets(discovery_port) {
        if let Err(error) = socket.send_to(&payload, target).await {
            log::debug!("peerdav.discovery announcement to {target} failed: {error}");
        }
    }
}

fn build_announcement(identity: &ServerIdentity, share_count: usize) -> WireAnnouncement {
    WireAnnouncement {
        protocol: DISCOVERY_PROTOCOL.to_owned(),
        kind: DISCOVERY_ANNOUNCE_KIND.to_owned(),
        server_id: identity.server_id.clone(),
        name: identity.name.clone(),
        port: identity.webdav_port,
        share_count,
        hosts: identity.hosts.clone(),
    }
}

fn share_count(shares: &Arc<RwLock<ShareMap>>) -> usize {
    match shares.read() {
        Ok(shares) => shares.len(),
        Err(_) => {
            log::warn!("peerdav.discovery share catalog lock poisoned");
            0
        }
    }
}

/// Hosts advertised to other machines. The loopback address is always kept so
/// the same machine can connect to its own server; clients prefer a
/// non-loopback host when one exists.
pub fn advertised_hosts(bind_address: &str) -> Vec<String> {
    let bound = bind_address.trim().parse::<IpAddr>().ok();
    let mut hosts: Vec<String> = Vec::new();

    match bound {
        Some(address) if !address.is_unspecified() => {
            hosts.push(address.to_string());
        }
        _ => {
            if let Ok(interfaces) = get_if_addrs() {
                for interface in interfaces {
                    if let IfAddr::V4(address) = interface.addr
                        && !address.ip.is_loopback()
                        && !address.ip.is_unspecified()
                        && !address.is_link_local()
                    {
                        push_host(&mut hosts, address.ip.to_string());
                    }
                }
            }
        }
    }

    push_host(&mut hosts, Ipv4Addr::LOCALHOST.to_string());
    hosts
}

/// Private IPv4 subnets of the local non-loopback interfaces, in CIDR form.
///
/// These are the networks a PeerDav share should allow by default, so a user
/// does not have to look up their own LAN subnet manually.
pub fn local_ipv4_networks() -> Vec<String> {
    let mut networks = Vec::new();
    if let Ok(interfaces) = get_if_addrs() {
        for interface in interfaces {
            let IfAddr::V4(address) = interface.addr else {
                continue;
            };
            if !is_bindable_lan_address(address.ip) {
                continue;
            }
            if let Some(network) = ipv4_network(address.ip, address.netmask) {
                push_unique(&mut networks, network);
            }
        }
    }
    networks
}

/// Private IPv4 addresses of the local interfaces, with the address the system
/// would actually use to leave this host first.
///
/// The service pane lists the head as the LAN bind scope. Virtual bridges
/// (Docker, libvirt) also own private addresses, so interface order alone would
/// put a bridge in front of the real LAN card; the primary probe keeps the
/// option meaningful instead of asking the user to guess among a dozen IPs.
pub fn local_ipv4_addresses() -> Vec<String> {
    let mut addresses = Vec::new();
    if let Ok(interfaces) = get_if_addrs() {
        for interface in interfaces {
            let IfAddr::V4(address) = interface.addr else {
                continue;
            };
            if is_bindable_lan_address(address.ip) {
                push_unique(&mut addresses, address.ip.to_string());
            }
        }
    }
    prioritize_primary(addresses, primary_lan_address())
}

/// The source address the kernel would pick for outbound traffic. Connecting an
/// unconnected UDP socket only performs a route lookup; no packet is sent.
fn primary_lan_address() -> Option<Ipv4Addr> {
    let socket = std::net::UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    socket.connect((Ipv4Addr::new(8, 8, 8, 8), 53)).ok()?;
    match socket.local_addr().ok()?.ip() {
        IpAddr::V4(address) if is_bindable_lan_address(address) => Some(address),
        _ => None,
    }
}

/// Move `primary` to the head of `addresses`, leaving the rest in order. A
/// missing or unknown primary keeps the interface order untouched.
fn prioritize_primary(mut addresses: Vec<String>, primary: Option<Ipv4Addr>) -> Vec<String> {
    let Some(value) = primary.map(|address| address.to_string()) else {
        return addresses;
    };
    if let Some(index) = addresses.iter().position(|address| *address == value)
        && index > 0
    {
        let address = addresses.remove(index);
        addresses.insert(0, address);
    }
    addresses
}

/// Whether an interface address is a sensible LAN bind target: not the loopback,
/// not the wildcard, not an auto-assigned link-local address, and in a private
/// or shared-address range. Public addresses belong to the WAN scope, which
/// binds the wildcard.
fn is_bindable_lan_address(ip: Ipv4Addr) -> bool {
    if ip.is_loopback() || ip.is_unspecified() || ip.is_link_local() {
        return false;
    }
    // 100.64.0.0/10（RFC 6598）不属于 std 的 is_private，但 tun/VPN 网卡
    // （Tailscale、部分 WireGuard 配置）就在这段，漏掉它等于绑不了 VPN。
    ip.is_private() || is_shared_address_space(ip)
}

fn is_shared_address_space(ip: Ipv4Addr) -> bool {
    let [first, second, ..] = ip.octets();
    first == 100 && (64..=127).contains(&second)
}

fn ipv4_network(ip: Ipv4Addr, netmask: Ipv4Addr) -> Option<String> {
    let mask = u32::from(netmask);
    if mask == 0 {
        return None;
    }
    let network = Ipv4Addr::from(u32::from(ip) & mask);
    Some(format!("{network}/{}", mask.count_ones()))
}

fn push_unique(values: &mut Vec<String>, value: String) {
    if !values.contains(&value) {
        values.push(value);
    }
}

fn push_host(hosts: &mut Vec<String>, host: String) {
    if hosts.len() < MAX_DISCOVERY_HOSTS && !hosts.contains(&host) {
        hosts.push(host);
    }
}

/// Sends discovery probes and collects every server seen before the timeout.
pub async fn scan(port: u16, timeout_ms: Option<u64>) -> Result<Vec<DiscoveredPeer>, DomainError> {
    if port == 0 {
        return Err(DomainError::Discovery {
            operation: "scan",
            detail: "discovery port must not be zero".to_owned(),
        });
    }

    let budget = timeout_ms
        .map(|milliseconds| Duration::from_millis(milliseconds.clamp(100, 10_000)))
        .unwrap_or(DISCOVERY_SCAN_TIMEOUT);

    let socket = UdpSocket::bind(SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0)))
        .await
        .map_err(|source| DomainError::Discovery {
            operation: "bind",
            detail: format!("failed to bind a scan socket: {source}"),
        })?;
    socket
        .set_broadcast(true)
        .map_err(|source| DomainError::Discovery {
            operation: "enable broadcast",
            detail: source.to_string(),
        })?;

    let probe = serde_json::to_vec(&WireProbe {
        protocol: DISCOVERY_PROTOCOL.to_owned(),
        kind: DISCOVERY_PROBE_KIND.to_owned(),
    })
    .map_err(|error| DomainError::Discovery {
        operation: "encode probe",
        detail: error.to_string(),
    })?;

    let targets = broadcast_targets(port);
    let deadline = Instant::now() + budget;
    let mut ticker = interval(PROBE_INTERVAL);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut buffer = vec![0_u8; DISCOVERY_RECV_BUFFER];
    let mut found: HashMap<String, DiscoveredPeer> = HashMap::new();
    let mut sent_any = false;

    loop {
        tokio::select! {
            _ = ticker.tick() => {
                if Instant::now() >= deadline {
                    break;
                }
                for target in &targets {
                    match socket.send_to(&probe, target).await {
                        Ok(_) => sent_any = true,
                        Err(error) => {
                            log::debug!("peerdav.discovery probe to {target} failed: {error}");
                        }
                    }
                }
            }
            received = timeout_at(deadline, socket.recv_from(&mut buffer)) => {
                match received {
                    Err(_) => break,
                    Ok(Err(error)) => {
                        return Err(DomainError::Discovery {
                            operation: "receive",
                            detail: format!("failed to receive a discovery reply: {error}"),
                        });
                    }
                    Ok(Ok((length, from))) => {
                        let Some(announcement) = parse_announcement(&buffer[..length]) else {
                            continue;
                        };
                        merge_announcement(&mut found, announcement, from);
                    }
                }
            }
        }
    }

    if !sent_any {
        return Err(DomainError::Discovery {
            operation: "scan",
            detail: "no discovery probe could be sent on this host".to_owned(),
        });
    }

    Ok(sorted_peers(found.into_values()))
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ScanTarget {
    address: Ipv4Addr,
    /// 0 = tunnel/VPN interface, 1 = ordinary interface. The lower value wins
    /// when the same peer is reachable through more than one address.
    priority: u8,
}

/// TCP discovery used when UDP broadcast is not forwarded by the overlay
/// network. Each private interface contributes the hosts of its local /24;
/// peers are identified by the public-key fingerprint from the TLS handshake.
pub(crate) async fn scan_tcp(
    identity: &DeviceIdentity,
    port: u16,
    networks: &[String],
) -> Result<Vec<DiscoveredPeer>, DomainError> {
    let targets = selected_scan_targets(networks)?;
    Ok(scan_tcp_prioritized(identity, port, targets, TCP_SCAN_TIMEOUT).await)
}

pub(crate) fn selected_scan_targets(networks: &[String]) -> Result<Vec<ScanTarget>, DomainError> {
    let own: HashSet<Ipv4Addr> = local_ipv4_addresses()
        .iter()
        .filter_map(|ip| ip.parse().ok())
        .collect();
    let mut targets = Vec::new();
    let mut seen = HashSet::new();
    for value in networks {
        let network = value
            .parse::<ipnet::Ipv4Net>()
            .map_err(|_| DomainError::Discovery {
                operation: "select TCP network",
                detail: format!("invalid IPv4 network: {value}"),
            })?;
        // Reject oversized ranges instead of silently scanning an arbitrary subset.
        if network.prefix_len() < 22 || !is_bindable_lan_address(network.network()) {
            return Err(DomainError::Discovery {
                operation: "select TCP network",
                detail: "Select a private IPv4 network with prefix /22 or narrower".into(),
            });
        }
        for address in network.hosts() {
            if own.contains(&address) || !seen.insert(address) {
                continue;
            }
            if targets.len() >= TCP_SCAN_TARGET_LIMIT {
                return Err(DomainError::Discovery {
                    operation: "select TCP network",
                    detail: "TCP discovery allows at most 1024 target addresses".into(),
                });
            }
            targets.push(ScanTarget {
                address,
                priority: 0,
            });
        }
    }
    Ok(targets)
}

#[cfg(test)]
pub(crate) async fn scan_tcp_targets(
    identity: &DeviceIdentity,
    port: u16,
    targets: Vec<Ipv4Addr>,
    budget: Duration,
) -> Vec<DiscoveredPeer> {
    let targets = targets
        .into_iter()
        .map(|address| ScanTarget {
            address,
            priority: 0,
        })
        .collect();
    scan_tcp_prioritized(identity, port, targets, budget).await
}

async fn scan_tcp_prioritized(
    identity: &DeviceIdentity,
    port: u16,
    targets: Vec<ScanTarget>,
    budget: Duration,
) -> Vec<DiscoveredPeer> {
    if port == 0 || targets.is_empty() {
        return Vec::new();
    }
    let target_count = targets.len();
    let config = match crate::tls::probe_client(identity) {
        Ok(config) => Arc::new(config),
        Err(error) => {
            log::warn!("peerdav.discovery TCP probe client unavailable: {error}");
            return Vec::new();
        }
    };

    let found = stream::iter(targets.into_iter().map(|target| {
        let config = Arc::clone(&config);
        async move { probe_tcp(target, port, budget, config).await }
    }))
    .buffer_unordered(TCP_SCAN_CONCURRENCY)
    .filter_map(|peer| async move { peer })
    .collect::<Vec<_>>()
    .await;

    let peers = merge_scanned(found);
    log::info!(
        "peerdav.discovery TCP scan probed {target_count} host(s) on port {port} and found {} peer(s)",
        peers.len()
    );
    peers
}

/// Deduplicates scan hits by certificate fingerprint. All reachable hosts are
/// kept, and the URL comes from the highest-priority interface so a virtual
/// LAN address wins over a physical one for the same server.
fn merge_scanned(found: Vec<(u8, DiscoveredPeer)>) -> Vec<DiscoveredPeer> {
    let mut unique: HashMap<String, (u8, DiscoveredPeer)> = HashMap::new();
    for (priority, peer) in found {
        match unique.entry(peer.server_id.clone()) {
            std::collections::hash_map::Entry::Occupied(mut entry) => {
                let (best_priority, best) = entry.get_mut();
                for host in &peer.hosts {
                    push_host(&mut best.hosts, host.clone());
                }
                if priority < *best_priority {
                    *best_priority = priority;
                    best.name = peer.name;
                    best.url = peer.url;
                    best.port = peer.port;
                }
            }
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert((priority, peer));
            }
        }
    }
    sorted_peers(unique.into_values().map(|(_, peer)| peer))
}

/// Probe one user-selected HTTPS endpoint. The fingerprint is TOFU discovery,
/// not a replacement for a previously saved pin.
pub(crate) async fn probe_url(
    raw: &str,
    identity: &crate::types::DeviceIdentity,
) -> Result<DiscoveredPeer, DomainError> {
    let url = url::Url::parse(raw).map_err(|source| DomainError::PeerProtocol {
        detail: source.to_string(),
    })?;
    let host = url.host_str().ok_or_else(|| DomainError::PeerProtocol {
        detail: "server URL has no host".into(),
    })?;
    let port = url.port_or_known_default().unwrap_or(443);
    let config = Arc::new(crate::tls::probe_client(identity)?);
    let addresses = timeout(
        Duration::from_secs(5),
        tokio::net::lookup_host((host, port)),
    )
    .await
    .map_err(|_| DomainError::PeerProtocol {
        detail: "server address lookup timed out".into(),
    })?
    .map_err(|source| DomainError::Io {
        operation: "resolve server address",
        path: None,
        source,
    })?;
    for address in addresses.take(8) {
        if let Some((_, mut peer)) =
            probe_socket(address, 0, Duration::from_secs(3), Arc::clone(&config)).await
        {
            peer.url = raw.to_owned();
            return Ok(peer);
        }
    }
    Err(DomainError::PeerProtocol {
        detail: "No PeerDav server responded at this address.".into(),
    })
}

async fn probe_tcp(
    target: ScanTarget,
    port: u16,
    budget: Duration,
    config: Arc<rustls::ClientConfig>,
) -> Option<(u8, DiscoveredPeer)> {
    probe_socket(
        SocketAddr::from((target.address, port)),
        target.priority,
        budget,
        config,
    )
    .await
}

async fn probe_socket(
    socket: SocketAddr,
    priority: u8,
    budget: Duration,
    config: Arc<rustls::ClientConfig>,
) -> Option<(u8, DiscoveredPeer)> {
    let address = socket.ip();
    let port = socket.port();
    let stream = timeout(budget, TcpStream::connect(socket))
        .await
        .ok()?
        .ok()?;
    let connector = TlsConnector::from(config);
    let server_name = rustls::pki_types::ServerName::try_from("peerdav.local").ok()?;
    let mut stream = timeout(budget, connector.connect(server_name, stream))
        .await
        .ok()?
        .ok()?;
    let certificate = stream.get_ref().1.peer_certificates()?.first()?.clone();
    let server_id = crate::tls::fingerprint(&certificate).ok()?;

    // A bare TLS handshake is not enough: routers, NAS boxes and other HTTPS
    // services would otherwise be reported as PeerDav. HEAD is routed to the
    // catalog handler before authentication and never records a pending
    // request; the Server header identifies the protocol implementation.
    let host = socket.to_string();
    let request = format!(
        "HEAD {CATALOG_PATH} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: PeerDav-discovery\r\nConnection: close\r\n\r\n"
    );
    timeout(budget, stream.write_all(request.as_bytes()))
        .await
        .ok()?
        .ok()?;
    let mut head = Vec::new();
    let mut chunk = [0_u8; 512];
    loop {
        let count = match timeout(budget, stream.read(&mut chunk)).await.ok()? {
            Ok(0) => break,
            Ok(count) => count,
            Err(_) => return None,
        };
        head.extend_from_slice(&chunk[..count]);
        if head.windows(4).any(|window| window == b"\r\n\r\n") || head.len() >= 4096 {
            break;
        }
    }
    if !is_peerdav_response(&head) {
        return None;
    }

    let host = address.to_string();
    let hosts = vec![host.clone()];
    Some((
        priority,
        DiscoveredPeer {
            server_id,
            name: format!("PeerDav ({host})"),
            url: choose_url(&hosts, port).unwrap_or_default(),
            hosts,
            port,
            share_count: 0,
        },
    ))
}

fn is_peerdav_response(head: &[u8]) -> bool {
    String::from_utf8_lossy(head)
        .lines()
        .take(32)
        .filter_map(|line| line.split_once(':'))
        .any(|(name, value)| {
            name.eq_ignore_ascii_case("server") && value.trim().eq_ignore_ascii_case("peerdav")
        })
}

/// Virtual interfaces win over physical NICs when a peer is reachable through
/// both. The default-route interface is ranked last, so an address learned
/// through EasyTier/WireGuard is preferred even if the adapter name is not
/// recognised.
#[cfg(test)]
fn interface_priority(name: &str, primary: Option<Ipv4Addr>, ip: Ipv4Addr) -> u8 {
    let name = name.to_ascii_lowercase();
    const VIRTUAL_PREFIXES: &[&str] = &[
        "tun",
        "tap",
        "utun",
        "wg",
        "wireguard",
        "easytier",
        "tailscale",
        "zerotier",
        "zt",
        "vpn",
    ];
    if VIRTUAL_PREFIXES.iter().any(|prefix| name.contains(prefix)) {
        0
    } else if primary == Some(ip) {
        2
    } else {
        1
    }
}

#[cfg(test)]
fn scan_targets_for(ip: Ipv4Addr, netmask: Ipv4Addr) -> Vec<Ipv4Addr> {
    let prefix = u32::from(netmask).count_ones();
    let (base, hosts) = if (24..=30).contains(&prefix) {
        (
            u32::from(ip) & u32::from(netmask),
            (1_u32 << (32 - prefix)) - 2,
        )
    } else {
        // A /16 or /32 overlay still gets a bounded, useful probe of the /24
        // that contains this interface address instead of 65k connections.
        (u32::from(ip) & 0xFFFF_FF00, 254)
    };
    (1..=hosts)
        .map(|offset| Ipv4Addr::from(base + offset))
        .collect()
}

fn merge_announcement(
    found: &mut HashMap<String, DiscoveredPeer>,
    announcement: WireAnnouncement,
    from: SocketAddr,
) {
    let announcement = announcement_from_source(announcement, from);
    let key = announcement_key(&announcement, from);
    match found.entry(key) {
        std::collections::hash_map::Entry::Occupied(mut entry) => {
            update_discovered_peer(entry.get_mut(), &announcement);
        }
        std::collections::hash_map::Entry::Vacant(entry) => {
            entry.insert(new_discovered_peer(&announcement));
        }
    }
}

fn parse_announcement(bytes: &[u8]) -> Option<WireAnnouncement> {
    let announcement = serde_json::from_slice::<WireAnnouncement>(bytes).ok()?;
    if announcement.protocol != DISCOVERY_PROTOCOL
        || announcement.kind != DISCOVERY_ANNOUNCE_KIND
        || announcement.port == 0
    {
        return None;
    }
    Some(announcement)
}

fn announcement_key(announcement: &WireAnnouncement, from: SocketAddr) -> String {
    if announcement.server_id.is_empty() {
        format!("{}:{}", from.ip(), announcement.port)
    } else {
        announcement.server_id.clone()
    }
}

fn new_discovered_peer(announcement: &WireAnnouncement) -> DiscoveredPeer {
    let mut hosts = Vec::new();
    for host in &announcement.hosts {
        push_host(&mut hosts, host.clone());
    }
    DiscoveredPeer {
        server_id: announcement.server_id.clone(),
        name: announcement.name.clone(),
        url: choose_url(&hosts, announcement.port).unwrap_or_default(),
        hosts,
        port: announcement.port,
        share_count: announcement.share_count,
    }
}

fn update_discovered_peer(peer: &mut DiscoveredPeer, announcement: &WireAnnouncement) {
    peer.name = announcement.name.clone();
    peer.port = announcement.port;
    peer.share_count = announcement.share_count;
    let mut hosts = Vec::new();
    for host in &announcement.hosts {
        push_host(&mut hosts, host.clone());
    }
    peer.hosts = hosts;
    if let Some(url) = choose_url(&peer.hosts, peer.port) {
        peer.url = url;
    }
}

fn sorted_peers(peers: impl Iterator<Item = DiscoveredPeer>) -> Vec<DiscoveredPeer> {
    let mut peers: Vec<DiscoveredPeer> = peers.collect();
    peers.sort_by(|left, right| {
        left.name
            .to_lowercase()
            .cmp(&right.name.to_lowercase())
            .then_with(|| left.url.cmp(&right.url))
    });
    peers
}

fn choose_url(hosts: &[String], port: u16) -> Option<String> {
    let host = hosts
        .iter()
        .find(|host| !is_loopback_host(host))
        .or_else(|| hosts.first())?;
    let host = if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]")
    } else {
        host.clone()
    };
    Some(format!("https://{host}:{port}"))
}

fn is_loopback_host(host: &str) -> bool {
    host.parse::<IpAddr>()
        .is_ok_and(|address| address.is_loopback())
}

fn broadcast_targets(port: u16) -> Vec<SocketAddrV4> {
    let mut targets: Vec<SocketAddrV4> = Vec::new();

    if let Ok(interfaces) = get_if_addrs() {
        for interface in interfaces {
            if let IfAddr::V4(address) = interface.addr {
                if address.ip.is_loopback() || address.ip.is_unspecified() {
                    continue;
                }
                let directed = address.broadcast.or_else(|| {
                    let netmask = u32::from(address.netmask);
                    if netmask == 0 {
                        return None;
                    }
                    let broadcast = u32::from(address.ip) | !netmask;
                    Some(Ipv4Addr::from(broadcast))
                });
                if let Some(broadcast) = directed
                    && !targets.contains(&SocketAddrV4::new(broadcast, port))
                {
                    targets.push(SocketAddrV4::new(broadcast, port));
                }
            }
        }
    }

    for broadcast in [Ipv4Addr::BROADCAST, Ipv4Addr::LOCALHOST] {
        let target = SocketAddrV4::new(broadcast, port);
        if !targets.contains(&target) {
            targets.push(target);
        }
    }
    targets
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::Mutex;

    use crate::error::AppError;
    use crate::types::PeerRevocationInfo;

    #[test]
    fn lan_bind_filter_accepts_only_private_routable_addresses() {
        let accepted = [
            "10.0.0.7",
            "172.16.5.9",
            "192.168.1.23",
            // tun/VPN 网卡常用的 100.64.0.0/10 也算本机可达的网络内地址。
            "100.64.0.1",
            "100.127.255.10",
        ];
        for address in accepted {
            let ip = address
                .parse::<Ipv4Addr>()
                .unwrap_or_else(|_| panic!("{address} must parse"));
            assert!(is_bindable_lan_address(ip), "{address} should bind");
        }

        // 边界：回环、通配、链路本地、公网以及 /10 之外的地址都不能作为局域网绑定项。
        let rejected = [
            "127.0.0.1",
            "0.0.0.0",
            "169.254.9.9",
            "8.8.8.8",
            "100.63.255.1",
            "100.128.0.1",
        ];
        for address in rejected {
            let ip = address
                .parse::<Ipv4Addr>()
                .unwrap_or_else(|_| panic!("{address} must parse"));
            assert!(!is_bindable_lan_address(ip), "{address} must not bind");
        }
    }

    #[test]
    fn lan_bind_candidates_are_unique_private_addresses() {
        let addresses = local_ipv4_addresses();
        let mut deduped = addresses.clone();
        deduped.sort();
        deduped.dedup();
        assert_eq!(
            deduped.len(),
            addresses.len(),
            "candidates must stay unique"
        );

        for address in addresses {
            let ip = address
                .parse::<Ipv4Addr>()
                .unwrap_or_else(|_| panic!("{address} must be IPv4"));
            assert!(
                is_bindable_lan_address(ip),
                "{address} leaked a non-LAN address"
            );
        }
    }

    #[test]
    fn primary_address_moves_to_the_head() {
        let candidates: Vec<String> = ["172.17.0.1", "192.168.1.23", "10.0.0.7"]
            .iter()
            .map(|address| (*address).to_owned())
            .collect();

        let ordered = prioritize_primary(candidates.clone(), "192.168.1.23".parse().ok());
        assert_eq!(
            ordered,
            vec![
                "192.168.1.23".to_owned(),
                "172.17.0.1".to_owned(),
                "10.0.0.7".to_owned()
            ]
        );

        // 边界：没有默认路由或默认地址不在候选里时，保持网卡顺序，不能丢项。
        assert_eq!(prioritize_primary(candidates.clone(), None), candidates);
        assert_eq!(
            prioritize_primary(candidates.clone(), "198.51.100.9".parse().ok()),
            candidates
        );
        assert_eq!(
            prioritize_primary(candidates.clone(), "172.17.0.1".parse().ok()),
            candidates
        );
    }

    #[derive(Default)]
    struct RecordingEvents {
        snapshots: Mutex<Vec<Vec<DiscoveredPeer>>>,
    }

    impl PeerEvents for RecordingEvents {
        fn error(&self, _error: &AppError) {}

        fn peer_revoked(&self, _peer: &PeerRevocationInfo) {}

        fn discovered_peers(&self, peers: &[DiscoveredPeer]) {
            self.snapshots
                .lock()
                .expect("snapshots")
                .push(peers.to_vec());
        }
    }

    fn free_udp_port() -> u16 {
        std::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
            .expect("reserve port")
            .local_addr()
            .expect("local addr")
            .port()
    }

    async fn wait_until(mut predicate: impl FnMut() -> bool, message: &str) {
        for _ in 0..150 {
            if predicate() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("{message}");
    }

    fn discovered_peer(id: &str) -> DiscoveredPeer {
        DiscoveredPeer {
            server_id: id.to_owned(),
            name: id.to_owned(),
            url: format!("https://{id}:4918"),
            hosts: vec![id.to_owned()],
            port: 4918,
            share_count: 1,
        }
    }

    #[test]
    fn url_prefers_a_routable_host() {
        let hosts = vec![Ipv4Addr::LOCALHOST.to_string(), "192.168.1.9".to_owned()];
        assert_eq!(
            choose_url(&hosts, 4918),
            Some("https://192.168.1.9:4918".to_owned())
        );
        assert_eq!(
            choose_url(&[Ipv4Addr::LOCALHOST.to_string()], 4918),
            Some("https://127.0.0.1:4918".to_owned())
        );
    }

    #[test]
    fn ipv4_networks_are_derived_from_interface_masks() {
        assert_eq!(
            ipv4_network(
                "192.168.1.42".parse().unwrap(),
                "255.255.255.0".parse().unwrap()
            ),
            Some("192.168.1.0/24".to_owned())
        );
        assert_eq!(
            ipv4_network("10.4.7.9".parse().unwrap(), "255.255.0.0".parse().unwrap()),
            Some("10.4.0.0/16".to_owned())
        );
        assert_eq!(
            ipv4_network("192.168.1.42".parse().unwrap(), Ipv4Addr::UNSPECIFIED),
            None
        );
    }

    #[test]
    fn ipv6_urls_are_bracketed() {
        assert_eq!(
            choose_url(&["::1".to_owned()], 4918),
            Some("https://[::1]:4918".to_owned())
        );
    }

    #[test]
    fn listener_start_without_runtime_returns_an_error() {
        let events = Arc::new(RecordingEvents::default());
        let result = DiscoveryListener::start_with_ttl(4919, DISCOVERY_PEER_TTL, events);
        assert!(result.is_err());
    }

    #[test]
    fn announced_hosts_are_bounded() {
        let announcement = WireAnnouncement {
            protocol: DISCOVERY_PROTOCOL.to_owned(),
            kind: DISCOVERY_ANNOUNCE_KIND.to_owned(),
            server_id: "server-1".to_owned(),
            name: "Lab".to_owned(),
            port: 4918,
            share_count: 0,
            hosts: (0..MAX_DISCOVERY_HOSTS + 5)
                .map(|index| format!("10.0.0.{index}"))
                .collect(),
        };

        let peer = new_discovered_peer(&announcement);
        assert_eq!(peer.hosts.len(), MAX_DISCOVERY_HOSTS);
    }

    #[test]
    fn expired_servers_are_pruned_but_fresh_ones_survive() {
        let now = Instant::now();
        let mut registry = HashMap::new();
        registry.insert(
            "old".to_owned(),
            PeerRecord {
                peer: discovered_peer("old"),
                last_seen: now - Duration::from_secs(11),
                ttl: None,
                verified_url: None,
                udp: true,
            },
        );
        registry.insert(
            "fresh".to_owned(),
            PeerRecord {
                peer: discovered_peer("fresh"),
                last_seen: now,
                ttl: None,
                verified_url: None,
                udp: true,
            },
        );

        assert!(prune_expired(&mut registry, now, DISCOVERY_PEER_TTL));
        assert!(!registry.contains_key("old"));
        assert!(registry.contains_key("fresh"));
        assert!(!prune_expired(&mut registry, now, DISCOVERY_PEER_TTL));
    }

    #[tokio::test]
    async fn passive_listener_adds_and_expires_announcements() {
        let port = free_udp_port();
        let events = Arc::new(RecordingEvents::default());
        let listener = DiscoveryListener::start_with_ttl(
            port,
            Duration::from_millis(80),
            Arc::clone(&events) as Arc<dyn PeerEvents>,
        )
        .expect("listener");

        let sender = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("sender");
        let announcement = WireAnnouncement {
            protocol: DISCOVERY_PROTOCOL.to_owned(),
            kind: DISCOVERY_ANNOUNCE_KIND.to_owned(),
            server_id: "server-1".to_owned(),
            name: "Lab".to_owned(),
            port: 4918,
            share_count: 2,
            hosts: vec!["192.168.1.20".to_owned()],
        };
        sender
            .send_to(
                &serde_json::to_vec(&announcement).expect("encode"),
                SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
            )
            .await
            .expect("send announcement");

        wait_until(
            || !listener.peers().is_empty(),
            "announcement was not added",
        )
        .await;
        let peers = listener.peers();
        assert_eq!(peers.len(), 1);
        assert_eq!(peers[0].name, "Lab");
        assert_eq!(peers[0].url, "https://192.168.1.20:4918");
        assert_eq!(peers[0].share_count, 2);

        wait_until(|| listener.peers().is_empty(), "server did not expire").await;
        let snapshots = events.snapshots.lock().expect("snapshots");
        assert!(
            snapshots
                .iter()
                .any(|snapshot| snapshot.iter().any(|peer| peer.name == "Lab"))
        );
        assert_eq!(snapshots.last().map(Vec::len), Some(0));
    }

    #[tokio::test]
    async fn listener_answers_probes_when_a_server_advert_is_attached() {
        let port = free_udp_port();
        let events = Arc::new(RecordingEvents::default());
        let listener = DiscoveryListener::start_with_ttl(
            port,
            DISCOVERY_PEER_TTL,
            Arc::clone(&events) as Arc<dyn PeerEvents>,
        )
        .expect("listener");
        listener.set_advert(Some(LocalAdvert {
            server_id: "self".to_owned(),
            name: "Self host".to_owned(),
            port: 4918,
            hosts: vec![Ipv4Addr::LOCALHOST.to_string()],
            shares: Arc::new(RwLock::new(ShareMap::new())),
        }));

        let client = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("client");
        let probe = serde_json::to_vec(&WireProbe {
            protocol: DISCOVERY_PROTOCOL.to_owned(),
            kind: DISCOVERY_PROBE_KIND.to_owned(),
        })
        .expect("encode probe");
        client
            .send_to(&probe, SocketAddr::from((Ipv4Addr::LOCALHOST, port)))
            .await
            .expect("send probe");

        let mut buffer = vec![0_u8; DISCOVERY_RECV_BUFFER];
        let (length, _) = timeout(Duration::from_secs(2), client.recv_from(&mut buffer))
            .await
            .expect("probe reply timed out")
            .expect("receive probe reply");
        let reply = parse_announcement(&buffer[..length]).expect("announcement reply");
        assert_eq!(reply.server_id, "self");
        assert_eq!(reply.name, "Self host");
        assert_eq!(reply.port, 4918);
        assert_eq!(reply.share_count, 0);
    }

    #[tokio::test]
    async fn tcp_scan_results_are_visible_in_the_shared_registry() {
        let port = free_udp_port();
        let events = Arc::new(RecordingEvents::default());
        let listener = DiscoveryListener::start_with_ttl(
            port,
            DISCOVERY_PEER_TTL,
            Arc::clone(&events) as Arc<dyn PeerEvents>,
        )
        .expect("listener");
        let peer = discovered_peer("tcp-peer");

        listener.merge_peers(std::slice::from_ref(&peer));

        assert_eq!(listener.peers(), vec![peer.clone()]);
        let snapshots = events.snapshots.lock().expect("snapshots");
        assert!(
            snapshots
                .last()
                .is_some_and(|snapshot| snapshot.iter().any(|entry| entry.server_id == "tcp-peer"))
        );
    }

    #[test]
    fn selected_networks_are_explicit_bounded_and_deduplicated() {
        assert!(selected_scan_targets(&[]).expect("empty").is_empty());
        let targets = selected_scan_targets(&["10.231.242.0/30".into(), "10.231.242.0/30".into()])
            .expect("selected");
        assert_eq!(targets.len(), 2);
        assert!(
            targets
                .iter()
                .all(|target| target.address.octets()[..3] == [10, 231, 242])
        );
        for invalid in ["10.0.0.0/8", "8.8.8.0/24", "invalid"] {
            assert!(selected_scan_targets(&[invalid.into()]).is_err());
        }
        assert!(selected_scan_targets(&["10.231.0.0/22".into(), "10.231.4.0/22".into()]).is_err());
    }

    #[tokio::test]
    async fn udp_metadata_and_tcp_reachable_address_share_one_record() {
        let listener =
            DiscoveryListener::start(free_udp_port(), Arc::new(RecordingEvents::default()))
                .expect("listener");
        let announcement = WireAnnouncement {
            protocol: DISCOVERY_PROTOCOL.into(),
            kind: DISCOVERY_ANNOUNCE_KIND.into(),
            server_id: "same-device".into(),
            name: "Work Mac".into(),
            port: 4918,
            share_count: 3,
            hosts: vec!["192.168.90.8".into()],
        };
        let from = "10.2.2.8:4919".parse().expect("source");
        {
            let mut registry = listener.peers.write().expect("registry");
            upsert_live(
                &mut registry,
                announcement.clone(),
                from,
                Instant::now(),
                DISCOVERY_PEER_TTL,
            );
        }
        let mut scanned = discovered_peer("same-device");
        scanned.url = "https://10.2.2.8:4918".into();
        scanned.hosts = vec!["10.2.2.8".into()];
        listener.merge_peers(&[scanned]);
        {
            let mut registry = listener.peers.write().expect("registry");
            upsert_live(
                &mut registry,
                announcement,
                from,
                Instant::now(),
                DISCOVERY_PEER_TTL,
            );
        }
        let peers = listener.peers();
        assert_eq!(peers.len(), 1);
        assert_eq!(peers[0].name, "Work Mac");
        assert_eq!(peers[0].share_count, 3);
        assert_eq!(peers[0].url, "https://10.2.2.8:4918");
    }

    #[test]
    fn tcp_scan_targets_stay_inside_a_bounded_local_network() {
        let local = scan_targets_for(Ipv4Addr::new(10, 1, 2, 3), Ipv4Addr::new(255, 255, 255, 0));
        assert_eq!(local.len(), 254);
        assert!(local.contains(&Ipv4Addr::new(10, 1, 2, 1)));
        assert!(local.contains(&Ipv4Addr::new(10, 1, 2, 254)));
        assert!(!local.contains(&Ipv4Addr::new(10, 1, 2, 0)));
        assert!(!local.contains(&Ipv4Addr::new(10, 1, 2, 255)));

        // A wide or point-to-point interface is bounded to the local /24.
        assert_eq!(
            scan_targets_for(Ipv4Addr::new(10, 1, 2, 3), Ipv4Addr::new(255, 255, 0, 0)).len(),
            254
        );
        assert_eq!(
            scan_targets_for(
                Ipv4Addr::new(10, 1, 2, 3),
                Ipv4Addr::new(255, 255, 255, 255)
            )
            .len(),
            254
        );

        // A /30 only probes its two usable hosts.
        assert_eq!(
            scan_targets_for(
                Ipv4Addr::new(10, 1, 2, 6),
                Ipv4Addr::new(255, 255, 255, 252)
            ),
            vec![Ipv4Addr::new(10, 1, 2, 5), Ipv4Addr::new(10, 1, 2, 6)]
        );
    }

    #[tokio::test]
    async fn tcp_scan_reads_the_server_fingerprint_from_the_handshake() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let directory = tempfile::tempdir().expect("directory");
        let vault =
            crate::vault::Vault::open(&directory.path().join("vault.db")).expect("open vault");
        let identity = vault.current_identity().expect("identity");
        let acceptor = crate::tls::acceptor(&identity).expect("acceptor");
        let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("listener");
        let port = listener.local_addr().expect("local addr").port();
        let task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let mut stream = acceptor.accept(stream).await.expect("tls");
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request).await;
            stream
                .write_all(
                    b"HTTP/1.1 405 Method Not Allowed\r\nServer: PeerDav\r\nContent-Length: 0\r\n\r\n",
                )
                .await
                .expect("response");
        });

        let found = scan_tcp_targets(
            &identity,
            port,
            vec![Ipv4Addr::LOCALHOST],
            Duration::from_secs(2),
        )
        .await;
        task.abort();

        assert_eq!(found.len(), 1);
        assert_eq!(found[0].server_id, identity.fingerprint);
        assert_eq!(found[0].url, format!("https://127.0.0.1:{port}"));
        assert_eq!(found[0].port, port);
    }

    #[test]
    fn only_peerdav_server_headers_are_accepted() {
        assert!(is_peerdav_response(
            b"HTTP/1.1 405 Method Not Allowed\r\nServer: PeerDav\r\n\r\n"
        ));
        assert!(!is_peerdav_response(
            b"HTTP/1.1 200 OK\r\nServer: nginx\r\n\r\n"
        ));
        assert!(!is_peerdav_response(
            b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n"
        ));
    }

    #[test]
    fn virtual_interfaces_outrank_physical_ones() {
        let address = Ipv4Addr::new(10, 0, 0, 7);
        for name in ["tun0", "utun4", "wg0", "easytier0", "tailscale0"] {
            assert_eq!(interface_priority(name, None, address), 0, "{name}");
        }
        for name in ["en0", "eth0", "WLAN"] {
            assert_eq!(interface_priority(name, None, address), 1, "{name}");
        }
        // The default-route interface ranks below every other physical NIC.
        assert_eq!(interface_priority("eth0", Some(address), address), 2);
    }

    #[test]
    fn scanned_addresses_from_a_virtual_interface_win() {
        let physical = DiscoveredPeer {
            server_id: "same".to_owned(),
            name: "PeerDav (10.1.1.88)".to_owned(),
            url: "https://10.1.1.88:4918".to_owned(),
            hosts: vec!["10.1.1.88".to_owned()],
            port: 4918,
            share_count: 0,
        };
        let virtual_lan = DiscoveredPeer {
            server_id: "same".to_owned(),
            name: "PeerDav (10.2.2.8)".to_owned(),
            url: "https://10.2.2.8:4918".to_owned(),
            hosts: vec!["10.2.2.8".to_owned()],
            port: 4918,
            share_count: 0,
        };

        let merged = merge_scanned(vec![(1, physical), (0, virtual_lan)]);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].url, "https://10.2.2.8:4918");
        assert_eq!(merged[0].name, "PeerDav (10.2.2.8)");
        assert!(merged[0].hosts.contains(&"10.1.1.88".to_owned()));
        assert!(merged[0].hosts.contains(&"10.2.2.8".to_owned()));
    }
}
