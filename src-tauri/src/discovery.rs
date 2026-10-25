//! UDP LAN discovery.
//!
//! The server answers probe datagrams and periodically announces itself to
//! every directed broadcast address of the local interfaces. The client sends
//! probes to those same addresses plus loopback (so a machine can discover a
//! server running on itself) and de-duplicates replies by `server_id`.
//!
//! Discovery intentionally exposes only the server name, port and share
//! count. Share ids are only returned by the authenticated catalog endpoint.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use if_addrs::{IfAddr, get_if_addrs};
use serde::{Deserialize, Serialize};
use tokio::net::UdpSocket;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::{Instant, MissedTickBehavior, interval, timeout, timeout_at};

use crate::error::DomainError;
use crate::fs::ShareMap;
use crate::types::{
    DISCOVERY_ANNOUNCE_INTERVAL, DISCOVERY_ANNOUNCE_KIND, DISCOVERY_PROBE_KIND, DISCOVERY_PROTOCOL,
    DISCOVERY_RECV_BUFFER, DISCOVERY_SCAN_TIMEOUT, DiscoveredPeer, MAX_DISCOVERY_HOSTS,
};

const PROBE_INTERVAL: Duration = Duration::from_millis(250);
const DISCOVERY_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Serialize, Deserialize)]
struct WireProbe {
    protocol: String,
    kind: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct WireAnnouncement {
    protocol: String,
    kind: String,
    server_id: String,
    name: String,
    port: u16,
    share_count: usize,
    hosts: Vec<String>,
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

    let socket = UdpSocket::bind(SocketAddr::from((Ipv4Addr::UNSPECIFIED, port)))
        .await
        .map_err(|source| DomainError::Discovery {
            operation: "bind",
            detail: format!("failed to bind UDP port {port}: {source}"),
        })?;
    socket
        .set_broadcast(true)
        .map_err(|source| DomainError::Discovery {
            operation: "enable broadcast",
            detail: source.to_string(),
        })?;

    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let identity = Arc::new(ServerIdentity {
        server_id,
        name,
        webdav_port,
        hosts,
    });
    let local_port = socket
        .local_addr()
        .map_err(|source| DomainError::Discovery {
            operation: "inspect socket",
            detail: source.to_string(),
        })?
        .port();
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
                        let Ok(announcement) =
                            serde_json::from_slice::<WireAnnouncement>(&buffer[..length])
                        else {
                            continue;
                        };
                        if announcement.protocol != DISCOVERY_PROTOCOL
                            || announcement.kind != DISCOVERY_ANNOUNCE_KIND
                            || announcement.port == 0
                        {
                            continue;
                        }
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

    let mut peers: Vec<DiscoveredPeer> = found.into_values().collect();
    peers.sort_by(|left, right| {
        left.name
            .to_lowercase()
            .cmp(&right.name.to_lowercase())
            .then_with(|| left.url.cmp(&right.url))
    });
    Ok(peers)
}

fn merge_announcement(
    found: &mut HashMap<String, DiscoveredPeer>,
    announcement: WireAnnouncement,
    from: SocketAddr,
) {
    let key = if announcement.server_id.is_empty() {
        format!("{}:{}", from.ip(), announcement.port)
    } else {
        announcement.server_id.clone()
    };

    let entry = found.entry(key).or_insert_with(|| DiscoveredPeer {
        server_id: announcement.server_id.clone(),
        name: announcement.name.clone(),
        url: choose_url(&announcement.hosts, announcement.port).unwrap_or_default(),
        hosts: announcement.hosts.clone(),
        port: announcement.port,
        share_count: announcement.share_count,
    });

    for host in announcement.hosts {
        if entry.hosts.len() < MAX_DISCOVERY_HOSTS && !entry.hosts.contains(&host) {
            entry.hosts.push(host);
        }
    }
    if entry.url.is_empty() {
        entry.url = choose_url(&entry.hosts, entry.port).unwrap_or_default();
    }
    entry.share_count = entry.share_count.max(announcement.share_count);
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
    Some(format!("http://{host}:{port}"))
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

    #[test]
    fn url_prefers_a_routable_host() {
        let hosts = vec![Ipv4Addr::LOCALHOST.to_string(), "192.168.1.9".to_owned()];
        assert_eq!(
            choose_url(&hosts, 4918),
            Some("http://192.168.1.9:4918".to_owned())
        );
        assert_eq!(
            choose_url(&[Ipv4Addr::LOCALHOST.to_string()], 4918),
            Some("http://127.0.0.1:4918".to_owned())
        );
    }

    #[test]
    fn ipv6_urls_are_bracketed() {
        assert_eq!(
            choose_url(&["::1".to_owned()], 4918),
            Some("http://[::1]:4918".to_owned())
        );
    }
}
