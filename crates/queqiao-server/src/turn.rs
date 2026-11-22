//! STUN and TURN over UDP (RFC 8489 / 8656), the relay of last resort when two devices cannot reach each
//! other directly.
//!
//! Security rules:
//! - Relaying needs credentials, and only logged-in devices get them (`credentials`, asked for over the
//!   account connection): username "<expiry>:<device id>", password HMAC-SHA1 of the username with a secret
//!   made at each start, valid for an hour for new allocations.
//! - Every request after the Allocate must carry MESSAGE-INTEGRITY made with that allocation's key.
//! - Data goes only to peers the client gave permission to, never to loopback, link-local, multicast or
//!   unspecified addresses, never to this server's own services (on its public address only the relay
//!   ports, for relay-to-relay sessions), and to private and shared (100.64/10) addresses only with
//!   `--relay-private` (tests and LAN-only servers).
//! - At most 4 allocations per device, a fixed port range, nonces that change every hour.
//!
//! STUN Binding requests (what is my public address?) are answered for anyone, as every STUN server does.

use std::collections::HashMap;
use std::io::ErrorKind;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use queqiao_core::stun::{self, Class, Message, attr, method};
use tokio::net::UdpSocket;

use crate::store::hex;

pub const REALM: &str = "queqiao";
/// Relay credentials allow new allocations for this long.
pub const CREDENTIAL_SECONDS: u64 = 3600;
const DEFAULT_LIFETIME: u32 = 600;
const MAX_LIFETIME: u32 = 3600;
const PERMISSION_LIFETIME: Duration = Duration::from_secs(300);
const CHANNEL_LIFETIME: Duration = Duration::from_secs(600);
const PER_DEVICE: usize = 4;
/// Allocations an account may hold at once (its devices together).
const PER_ACCOUNT: usize = 16;
/// Permissions and channels an allocation may hold (a session needs a few).
const MAX_PERMISSIONS: usize = 32;
const MAX_CHANNELS: usize = 32;
/// Bytes an allocation may relay, both ways together: RELAY_RATE a second (100 Mbit/s, well above a session's
/// picture), RELAY_BURST at once; more are dropped.
const RELAY_RATE: f64 = 12.5e6;
const RELAY_BURST: f64 = 4e6;

pub struct TurnConfig {
    /// UDP address for STUN and TURN (port 3478 by default).
    pub listen: SocketAddr,
    /// The address clients and peers reach this server at; relayed addresses are on it.
    pub public_ip: IpAddr,
    /// Inclusive range of UDP ports for relayed addresses.
    pub relay_ports: (u16, u16),
    pub allow_private: bool,
    pub secret: [u8; 32],
}

impl TurnConfig {
    /// Relay credentials for a logged-in device of an account: (username, password, expiry in Unix seconds). The
    /// username names both (signed by the password), for the quotas per device and per account.
    pub fn credentials(&self, account: i64, device: &str) -> (String, String, u64) {
        let expires = unix_now() + CREDENTIAL_SECONDS;
        let username = format!("{expires}:{account}:{device}");
        let password = hex(&stun::hmac_sha1(&self.secret, username.as_bytes()));
        (username, password, expires)
    }

    /// host:port of the STUN/TURN service for clients.
    pub fn address(&self) -> String {
        SocketAddr::new(self.public_ip, self.listen.port()).to_string()
    }

    fn nonce(&self, hour: u64) -> String {
        hex(&stun::hmac_sha1(
            &self.secret,
            format!("nonce:{hour}").as_bytes(),
        ))[..24]
            .to_string()
    }

    fn nonce_valid(&self, nonce: &[u8]) -> bool {
        let hour = unix_now() / 3600;
        [hour, hour.saturating_sub(1)]
            .iter()
            .any(|&h| self.nonce(h).as_bytes() == nonce)
    }

    fn peer_allowed(&self, peer: SocketAddr) -> bool {
        let ip = peer.ip();
        if ip == self.public_ip {
            return (self.relay_ports.0..=self.relay_ports.1).contains(&peer.port());
        }
        match ip {
            IpAddr::V4(v4) => {
                if v4.is_loopback()
                    || v4.is_unspecified()
                    || v4.is_broadcast()
                    || v4.is_multicast()
                    || v4.is_link_local()
                {
                    return false;
                }
                let [a, b, ..] = v4.octets();
                let shared = a == 100 && (b & 0xC0) == 64;
                !(v4.is_private() || shared) || self.allow_private
            }
            IpAddr::V6(v6) => {
                let first = v6.segments()[0];
                if v6.is_loopback()
                    || v6.is_unspecified()
                    || v6.is_multicast()
                    || (first & 0xFFC0) == 0xFE80
                {
                    return false;
                }
                (first & 0xFE00) != 0xFC00 || self.allow_private
            }
        }
    }
}

struct Allocation {
    username: String,
    key: [u8; 16],
    relay: Arc<UdpSocket>,
    expires: Instant,
    permissions: HashMap<IpAddr, Instant>,
    channels: HashMap<u16, (SocketAddr, Instant)>,
    by_peer: HashMap<SocketAddr, u16>,
    allocate_transaction: [u8; 12],
    allocate_response: Vec<u8>,
    task: tokio::task::JoinHandle<()>,
    budget: Budget,
}

/// What an allocation may still relay (the security review of 2026-11-22: there was no limit, so one allocation was
/// an anonymous UDP proxy at any rate).
struct Budget {
    bytes: f64,
    at: Instant,
}

impl Budget {
    fn new(now: Instant) -> Budget {
        Budget {
            bytes: RELAY_BURST,
            at: now,
        }
    }

    /// Whether `length` bytes may go now (they are taken off if so).
    fn take(&mut self, now: Instant, length: usize) -> bool {
        let elapsed = now.saturating_duration_since(self.at).as_secs_f64();
        self.bytes = (self.bytes + elapsed * RELAY_RATE).min(RELAY_BURST);
        self.at = now;
        if self.bytes < length as f64 {
            return false;
        }
        self.bytes -= length as f64;
        true
    }
}

/// The account and the device a relay username was made for ("expiry:account:device"; no account in the earlier
/// "expiry:device").
fn owner(username: &str) -> (&str, &str) {
    let mut parts = username.splitn(3, ':');
    let _expiry = parts.next();
    match (parts.next(), parts.next()) {
        (Some(account), Some(device)) => (account, device),
        (Some(device), None) => ("", device),
        _ => ("", ""),
    }
}

/// Whether another allocation for `username` would pass PER_DEVICE of its device or PER_ACCOUNT of its account (the
/// security review of 2026-11-22: new device ids got around the quota per device, and filled the relay's ports).
fn quota_reached<'a>(taken: impl Iterator<Item = &'a str>, username: &str) -> bool {
    let (account, device) = owner(username);
    let (mut by_device, mut by_account) = (0, 0);
    for other in taken {
        let (other_account, other_device) = owner(other);
        by_device += usize::from(other_device == device);
        by_account += usize::from(!account.is_empty() && other_account == account);
    }
    by_device >= PER_DEVICE || by_account >= PER_ACCOUNT
}

/// Permissions for `peers` until `until`, unless the allocation would then hold more than MAX_PERMISSIONS: all or
/// none (the security review of 2026-11-22: one CreatePermission could add thousands, until memory ran out).
fn permit(permissions: &mut HashMap<IpAddr, Instant>, peers: Vec<IpAddr>, until: Instant) -> bool {
    let mut new: Vec<IpAddr> = peers
        .iter()
        .copied()
        .filter(|ip| !permissions.contains_key(ip))
        .collect();
    new.sort();
    new.dedup();
    if permissions.len() + new.len() > MAX_PERMISSIONS {
        return false;
    }
    for ip in peers {
        permissions.insert(ip, until);
    }
    true
}

impl Allocation {
    fn permitted(&self, peer: SocketAddr, now: Instant) -> bool {
        self.permissions
            .get(&peer.ip())
            .is_some_and(|until| *until > now)
    }
}

type State = Arc<Mutex<HashMap<SocketAddr, Allocation>>>;

/// What the main loop does after handling a packet (the state lock is not held across sends).
enum Action {
    Reply(Vec<u8>),
    Relay(Arc<UdpSocket>, SocketAddr, Vec<u8>),
    Allocate {
        username: String,
        key: [u8; 16],
        request: Message,
        lifetime: u32,
    },
}

/// A UDP socket on `port`: on the public address itself when it is this machine's (and the listen address is
/// "any"). A socket on "any" sends each answer out of whichever interface the route picks; on a PC whose default
/// route is a VPN or a proxy's TUN adapter (Clash/Mihomo and the like) that is the tunnel, so answers left from
/// the tunnel's address and never matched what the client had asked (found 2026-11-21). Bound to the address,
/// Windows sends only out of the interface that has it.
async fn bind_udp(config: &TurnConfig, port: u16) -> std::io::Result<UdpSocket> {
    if config.listen.ip().is_unspecified()
        && !config.public_ip.is_unspecified()
        && let Ok(socket) = UdpSocket::bind(SocketAddr::new(config.public_ip, port)).await
    {
        return Ok(socket);
    }
    UdpSocket::bind(SocketAddr::new(config.listen.ip(), port)).await
}

pub async fn serve(config: Arc<TurnConfig>) -> std::io::Result<()> {
    let socket = Arc::new(bind_udp(&config, config.listen.port()).await?);
    let state: State = Arc::new(Mutex::new(HashMap::new()));
    let sweeper = state.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(10)).await;
            sweep(&sweeper);
        }
    });
    let mut next_port = config.relay_ports.0;
    let mut buffer = vec![0u8; 65536];
    loop {
        // An error from one datagram (a reset, an ICMP message) is not the end of the relay, nor of the server.
        let (length, client) = match socket.recv_from(&mut buffer).await {
            Ok(received) => received,
            Err(error) if error.kind() == ErrorKind::ConnectionReset => continue,
            Err(error) => {
                eprintln!("TURN: {error}");
                tokio::time::sleep(Duration::from_millis(50)).await;
                continue;
            }
        };
        let action = {
            let mut allocations = state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            handle(&config, &mut allocations, client, &buffer[..length])
        };
        match action {
            Some(Action::Reply(bytes)) => {
                let _ = socket.send_to(&bytes, client).await;
            }
            Some(Action::Relay(relay, peer, bytes)) => {
                let _ = relay.send_to(&bytes, peer).await;
            }
            Some(Action::Allocate {
                username,
                key,
                request,
                lifetime,
            }) => {
                let reply = allocate(
                    &config,
                    &state,
                    &socket,
                    &mut next_port,
                    client,
                    username,
                    key,
                    &request,
                    lifetime,
                )
                .await;
                let _ = socket.send_to(&reply, client).await;
            }
            None => {}
        }
    }
}

fn handle(
    config: &TurnConfig,
    allocations: &mut HashMap<SocketAddr, Allocation>,
    client: SocketAddr,
    packet: &[u8],
) -> Option<Action> {
    let now = Instant::now();
    if let Some((number, payload)) = stun::parse_channel_data(packet) {
        let allocation = allocations.get_mut(&client)?;
        let (peer, until) = *allocation.channels.get(&number)?;
        let allowed = until > now
            && allocation.permitted(peer, now)
            && allocation.budget.take(now, payload.len());
        return allowed.then(|| Action::Relay(allocation.relay.clone(), peer, payload.to_vec()));
    }
    let message = Message::decode(packet)?;
    match (message.class, message.method) {
        (Class::Request, method::BINDING) => {
            let reply = message
                .reply(Class::Success)
                .with_address(attr::XOR_MAPPED_ADDRESS, client);
            Some(Action::Reply(reply.encode(None, true)))
        }
        (Class::Indication, method::SEND) => {
            let allocation = allocations.get_mut(&client)?;
            let peer = message.address(attr::XOR_PEER_ADDRESS)?;
            let data = message.get(attr::DATA)?;
            (allocation.permitted(peer, now) && allocation.budget.take(now, data.len()))
                .then(|| Action::Relay(allocation.relay.clone(), peer, data.to_vec()))
        }
        (Class::Request, method::ALLOCATE) => Some(check_allocate(
            config,
            allocations,
            client,
            &message,
            packet,
        )),
        (Class::Request, method::REFRESH | method::CREATE_PERMISSION | method::CHANNEL_BIND) => {
            Some(Action::Reply(authenticated(
                config,
                allocations,
                client,
                &message,
                packet,
                now,
            )))
        }
        (Class::Request, _) => Some(Action::Reply(
            message
                .reply(Class::Error)
                .with_error(400, "Bad Request")
                .encode(None, true),
        )),
        _ => None,
    }
}

fn challenge(config: &TurnConfig, message: &Message, code: u16, reason: &str) -> Vec<u8> {
    message
        .reply(Class::Error)
        .with_error(code, reason)
        .with(attr::REALM, REALM)
        .with(attr::NONCE, config.nonce(unix_now() / 3600))
        .encode(None, true)
}

fn error(message: &Message, code: u16, reason: &str) -> Vec<u8> {
    message
        .reply(Class::Error)
        .with_error(code, reason)
        .encode(None, true)
}

/// Checks an Allocate's credentials; the allocation itself is made by `allocate` (it binds a socket).
fn check_allocate(
    config: &TurnConfig,
    allocations: &HashMap<SocketAddr, Allocation>,
    client: SocketAddr,
    message: &Message,
    packet: &[u8],
) -> Action {
    if let Some(existing) = allocations.get(&client) {
        // A retransmitted Allocate gets the same answer; any other one is a mismatch.
        return Action::Reply(if existing.allocate_transaction == message.transaction {
            existing.allocate_response.clone()
        } else {
            error(message, 437, "Allocation Mismatch")
        });
    }
    if !message.has_integrity {
        return Action::Reply(challenge(config, message, 401, "Unauthorized"));
    }
    let (Some(username), Some(nonce)) = (message.text(attr::USERNAME), message.get(attr::NONCE))
    else {
        return Action::Reply(error(message, 400, "Bad Request"));
    };
    if !config.nonce_valid(nonce) {
        return Action::Reply(challenge(config, message, 438, "Stale Nonce"));
    }
    let expiry = username
        .split_once(':')
        .and_then(|(expiry, _)| expiry.parse::<u64>().ok());
    if expiry.is_none_or(|expiry| expiry < unix_now()) {
        return Action::Reply(challenge(config, message, 401, "Unauthorized"));
    }
    let password = hex(&stun::hmac_sha1(&config.secret, username.as_bytes()));
    let key = stun::long_term_key(&username, REALM, &password);
    if !stun::check_integrity(packet, &key) {
        return Action::Reply(challenge(config, message, 401, "Unauthorized"));
    }
    if message.get(attr::REQUESTED_TRANSPORT) != Some(&stun::TRANSPORT_UDP[..]) {
        return Action::Reply(error(message, 442, "Unsupported Transport Protocol"));
    }
    if quota_reached(
        allocations
            .values()
            .map(|allocation| allocation.username.as_str()),
        &username,
    ) {
        return Action::Reply(error(message, 486, "Allocation Quota Reached"));
    }
    Action::Allocate {
        username,
        key,
        request: message.clone(),
        lifetime: lifetime(message),
    }
}

#[allow(clippy::too_many_arguments)]
async fn allocate(
    config: &TurnConfig,
    state: &State,
    main: &Arc<UdpSocket>,
    next_port: &mut u16,
    client: SocketAddr,
    username: String,
    key: [u8; 16],
    request: &Message,
    lifetime: u32,
) -> Vec<u8> {
    let (first, last) = config.relay_ports;
    let count = u32::from(last - first) + 1;
    let mut relay = None;
    for _ in 0..count {
        let port = *next_port;
        *next_port = if port >= last { first } else { port + 1 };
        if let Ok(socket) = bind_udp(config, port).await {
            relay = Some((Arc::new(socket), port));
            break;
        }
    }
    let Some((relay, port)) = relay else {
        return error(request, 508, "Insufficient Capacity");
    };
    let relayed = SocketAddr::new(config.public_ip, port);
    let response = request
        .reply(Class::Success)
        .with_address(attr::XOR_RELAYED_ADDRESS, relayed)
        .with_address(attr::XOR_MAPPED_ADDRESS, client)
        .with(attr::LIFETIME, lifetime.to_be_bytes())
        .encode(Some(&key), true);
    let task = tokio::spawn(relay_to_client(
        relay.clone(),
        main.clone(),
        state.clone(),
        client,
    ));
    if let Ok(mut allocations) = state.lock() {
        allocations.insert(
            client,
            Allocation {
                username,
                key,
                relay,
                expires: Instant::now() + Duration::from_secs(u64::from(lifetime)),
                permissions: HashMap::new(),
                channels: HashMap::new(),
                budget: Budget::new(Instant::now()),
                by_peer: HashMap::new(),
                allocate_transaction: request.transaction,
                allocate_response: response.clone(),
                task,
            },
        );
    }
    response
}

/// Refresh, CreatePermission and ChannelBind: only for the allocation's owner, with its key.
fn authenticated(
    config: &TurnConfig,
    allocations: &mut HashMap<SocketAddr, Allocation>,
    client: SocketAddr,
    message: &Message,
    packet: &[u8],
    now: Instant,
) -> Vec<u8> {
    let Some(allocation) = allocations.get_mut(&client) else {
        return error(message, 437, "Allocation Mismatch");
    };
    if !message.has_integrity {
        return challenge(config, message, 401, "Unauthorized");
    }
    if message.text(attr::USERNAME).as_deref() != Some(allocation.username.as_str())
        || !stun::check_integrity(packet, &allocation.key)
    {
        return challenge(config, message, 401, "Unauthorized");
    }
    if !message
        .get(attr::NONCE)
        .is_some_and(|nonce| config.nonce_valid(nonce))
    {
        return challenge(config, message, 438, "Stale Nonce");
    }
    let key = allocation.key;
    let success = |reply: Message| reply.encode(Some(&key), true);
    match message.method {
        method::REFRESH => {
            let requested = message.u32(attr::LIFETIME).unwrap_or(DEFAULT_LIFETIME);
            if requested == 0 {
                if let Some(gone) = allocations.remove(&client) {
                    gone.task.abort();
                }
                return success(
                    message
                        .reply(Class::Success)
                        .with(attr::LIFETIME, 0u32.to_be_bytes()),
                );
            }
            let lifetime = requested.min(MAX_LIFETIME);
            allocation.expires = now + Duration::from_secs(u64::from(lifetime));
            success(
                message
                    .reply(Class::Success)
                    .with(attr::LIFETIME, lifetime.to_be_bytes()),
            )
        }
        method::CREATE_PERMISSION => {
            let all = peer_addresses(message);
            if all.is_empty() {
                return error(message, 400, "Bad Request");
            }
            if all.iter().any(|peer| !config.peer_allowed(*peer)) {
                return error(message, 403, "Forbidden");
            }
            let peers = all.iter().map(|peer| peer.ip()).collect();
            if !permit(
                &mut allocation.permissions,
                peers,
                now + PERMISSION_LIFETIME,
            ) {
                return error(message, 508, "Insufficient Capacity");
            }
            success(message.reply(Class::Success))
        }
        method::CHANNEL_BIND => {
            let number = message
                .get(attr::CHANNEL_NUMBER)
                .and_then(|v| Some(u16::from_be_bytes([*v.first()?, *v.get(1)?])));
            let (Some(number), Some(peer)) = (number, message.address(attr::XOR_PEER_ADDRESS))
            else {
                return error(message, 400, "Bad Request");
            };
            if !(0x4000..=0x7FFE).contains(&number) {
                return error(message, 400, "Bad Request");
            }
            if !config.peer_allowed(peer) {
                return error(message, 403, "Forbidden");
            }
            let taken_by_other = allocation
                .channels
                .get(&number)
                .is_some_and(|(bound, _)| *bound != peer)
                || allocation
                    .by_peer
                    .get(&peer)
                    .is_some_and(|bound| *bound != number);
            if taken_by_other {
                return error(message, 400, "Bad Request");
            }
            let room = allocation.channels.contains_key(&number)
                || allocation.channels.len() < MAX_CHANNELS;
            if !room
                || !permit(
                    &mut allocation.permissions,
                    vec![peer.ip()],
                    now + PERMISSION_LIFETIME,
                )
            {
                return error(message, 508, "Insufficient Capacity");
            }
            allocation
                .channels
                .insert(number, (peer, now + CHANNEL_LIFETIME));
            allocation.by_peer.insert(peer, number);
            success(message.reply(Class::Success))
        }
        _ => error(message, 400, "Bad Request"),
    }
}

/// Every XOR-PEER-ADDRESS of a message (CreatePermission may carry several).
fn peer_addresses(message: &Message) -> Vec<SocketAddr> {
    message
        .attributes
        .iter()
        .filter(|(kind, _)| *kind == attr::XOR_PEER_ADDRESS)
        .filter_map(|(_, value)| {
            let single = Message::new(message.class, message.method, message.transaction)
                .with(attr::XOR_PEER_ADDRESS, value.clone());
            single.address(attr::XOR_PEER_ADDRESS)
        })
        .collect()
}

/// What peers send to a relayed address goes to its client, if the client gave them permission.
async fn relay_to_client(
    relay: Arc<UdpSocket>,
    main: Arc<UdpSocket>,
    state: State,
    client: SocketAddr,
) {
    let mut buffer = vec![0u8; 65536];
    loop {
        let (length, peer) = match relay.recv_from(&mut buffer).await {
            Ok(received) => received,
            Err(error) if error.kind() == ErrorKind::ConnectionReset => continue,
            Err(_) => return,
        };
        let framed = {
            let Ok(mut allocations) = state.lock() else {
                return;
            };
            let Some(allocation) = allocations.get_mut(&client) else {
                return;
            };
            let now = Instant::now();
            if !allocation.permitted(peer, now) || !allocation.budget.take(now, length) {
                continue;
            }
            match allocation.by_peer.get(&peer) {
                Some(&number) => stun::channel_data(number, &buffer[..length]),
                None => Message::new(
                    Class::Indication,
                    method::DATA,
                    crate::store::random_bytes(12)
                        .ok()
                        .and_then(|b| b.try_into().ok())
                        .unwrap_or([0; 12]),
                )
                .with_address(attr::XOR_PEER_ADDRESS, peer)
                .with(attr::DATA, &buffer[..length])
                .encode(None, false),
            }
        };
        let _ = main.send_to(&framed, client).await;
    }
}

fn sweep(state: &State) {
    let now = Instant::now();
    let Ok(mut allocations) = state.lock() else {
        return;
    };
    allocations.retain(|_, allocation| {
        let alive = allocation.expires > now;
        if !alive {
            allocation.task.abort();
        }
        alive
    });
    for allocation in allocations.values_mut() {
        allocation.permissions.retain(|_, until| *until > now);
        allocation.channels.retain(|_, (_, until)| *until > now);
        let channels = &allocation.channels;
        allocation
            .by_peer
            .retain(|_, number| channels.contains_key(number));
    }
}

fn lifetime(message: &Message) -> u32 {
    match message.u32(attr::LIFETIME) {
        Some(0) | None => DEFAULT_LIFETIME,
        Some(requested) => requested.min(MAX_LIFETIME),
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(allow_private: bool) -> TurnConfig {
        TurnConfig {
            listen: "0.0.0.0:3478".parse().unwrap(),
            public_ip: "198.51.100.1".parse().unwrap(),
            relay_ports: (47200, 47399),
            allow_private,
            secret: [9; 32],
        }
    }

    #[test]
    fn relays_only_where_it_may() {
        let strict = config(false);
        for (peer, allowed) in [
            ("203.0.113.9:5000", true),
            ("198.51.100.1:47210", true),
            ("198.51.100.1:47100", false),
            ("198.51.100.1:3478", false),
            ("127.0.0.1:5000", false),
            ("169.254.1.1:5000", false),
            ("224.0.0.1:5000", false),
            ("0.0.0.0:5000", false),
            ("192.168.1.5:5000", false),
            ("10.0.0.5:5000", false),
            ("100.99.120.92:5000", false),
            ("[::1]:5000", false),
            ("[fe80::1]:5000", false),
            ("[fd00::1]:5000", false),
            ("[2001:db8::1]:5000", true),
        ] {
            assert_eq!(
                strict.peer_allowed(peer.parse().unwrap()),
                allowed,
                "{peer}"
            );
        }
        let lan = config(true);
        assert!(lan.peer_allowed("192.168.1.5:5000".parse().unwrap()));
        assert!(lan.peer_allowed("100.99.120.92:5000".parse().unwrap()));
        assert!(!lan.peer_allowed("127.0.0.1:5000".parse().unwrap()));
    }

    #[test]
    fn credentials_and_nonces() {
        let config = config(false);
        let (username, password, expires) = config.credentials(7, "device1");
        assert!(username.ends_with(":7:device1") && expires > unix_now());
        assert_eq!(owner(&username), ("7", "device1"));
        assert_eq!(
            password,
            hex(&stun::hmac_sha1(&config.secret, username.as_bytes()))
        );
        let nonce = config.nonce(unix_now() / 3600);
        assert!(config.nonce_valid(nonce.as_bytes()));
        assert!(!config.nonce_valid(b"made up"));
    }

    #[test]
    fn quotas_permissions_and_rate_are_bounded() {
        // Per device, and per account however many device ids.
        let same: Vec<String> = (0..PER_DEVICE).map(|_| "1:7:dev".to_string()).collect();
        assert!(quota_reached(same.iter().map(String::as_str), "1:7:dev"));
        assert!(!quota_reached(same.iter().map(String::as_str), "1:7:other"));
        let many: Vec<String> = (0..PER_ACCOUNT).map(|i| format!("1:7:dev{i}")).collect();
        assert!(quota_reached(many.iter().map(String::as_str), "1:7:new"));
        assert!(!quota_reached(many.iter().map(String::as_str), "1:8:new"));
        assert_eq!(owner("1:dev"), ("", "dev"));
        // Permissions: all or none, MAX_PERMISSIONS at most; a known one is renewed.
        let now = Instant::now();
        let mut permissions = HashMap::new();
        let peers: Vec<IpAddr> = (0..MAX_PERMISSIONS as u32 + 1)
            .map(|i| IpAddr::from(i.to_be_bytes()))
            .collect();
        assert!(!permit(&mut permissions, peers.clone(), now));
        assert!(permissions.is_empty());
        assert!(permit(
            &mut permissions,
            peers[..MAX_PERMISSIONS].to_vec(),
            now
        ));
        assert!(permit(&mut permissions, vec![peers[0]], now));
        assert!(!permit(&mut permissions, vec![peers[MAX_PERMISSIONS]], now));
        // Relayed bytes: RELAY_BURST at once, then RELAY_RATE a second.
        let mut budget = Budget::new(now);
        assert!(budget.take(now, RELAY_BURST as usize));
        assert!(!budget.take(now, 1));
        assert!(budget.take(
            now + Duration::from_millis(100),
            (RELAY_RATE / 10.0) as usize - 1
        ));
        assert!(!budget.take(now + Duration::from_millis(100), 2));
    }
}
