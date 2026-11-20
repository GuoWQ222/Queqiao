//! A TURN client (RFC 8656) for the network thread, sans-IO like str0m: it never touches the socket, it
//! takes the packets that came from the TURN server and the packets str0m wants to send from the relayed
//! address, and produces the datagrams to send to the server (`take_outbox`) and the time it next needs to
//! run (`next_deadline`).
//!
//! It allocates a relayed address (long-term credentials from queqiao-server), binds a channel to each peer
//! the first time something is sent there (which also gives the peer permission to send back), uses Send
//! indications until the channel is bound and ChannelData after, and refreshes the allocation and the
//! channels before they expire.

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use queqiao_core::stun::{self, Class, Message, attr, method};

/// A TURN server and the credentials for it.
#[derive(Clone, Debug)]
pub struct TurnServer {
    pub address: SocketAddr,
    pub username: String,
    pub password: String,
}

/// The STUN and TURN servers for a session: from queqiao-server, or none for a direct connection.
#[derive(Clone, Debug, Default)]
pub struct IceServers {
    pub stun: Option<SocketAddr>,
    pub turn: Option<TurnServer>,
}

/// First retransmission after 250 ms, then doubling, five tries (about 7.75 s in all).
const FIRST_RTO: Duration = Duration::from_millis(250);
const MAX_TRIES: u32 = 5;
/// Permissions last 300 s and channel bindings 600 s on the server: renew both (one ChannelBind) every 240 s.
const CHANNEL_RENEW: Duration = Duration::from_secs(240);
/// The allocation's requested lifetime, and how long before its end it is refreshed.
const LIFETIME_SECONDS: u32 = 600;
const REFRESH_EARLY: Duration = Duration::from_secs(60);
const FIRST_CHANNEL: u16 = 0x4000;
const LAST_CHANNEL: u16 = 0x4FFF;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Request {
    Allocate,
    Refresh,
    Deallocate,
    ChannelBind(SocketAddr),
}

struct Pending {
    request: Request,
    bytes: Vec<u8>,
    sent: Instant,
    tries: u32,
}

impl Pending {
    fn due(&self) -> Instant {
        self.sent + FIRST_RTO * 2u32.pow(self.tries - 1)
    }
}

struct Channel {
    number: u16,
    bound: bool,
    renew_at: Instant,
}

/// What a packet from the TURN server was.
pub(crate) enum Incoming {
    /// Data a peer sent to our relayed address.
    Data { peer: SocketAddr, payload: Vec<u8> },
    /// A response or anything else, handled here.
    Handled,
}

pub(crate) struct TurnClient {
    pub server: SocketAddr,
    username: String,
    password: String,
    realm: String,
    nonce: Vec<u8>,
    key: Option<[u8; 16]>,
    /// Our address on the TURN server, once allocated.
    pub relayed: Option<SocketAddr>,
    /// Our public address as the TURN server saw it.
    pub mapped: Option<SocketAddr>,
    /// Why the relay does not work, if it does not.
    pub failure: Option<String>,
    refresh_at: Option<Instant>,
    pending: HashMap<[u8; 12], Pending>,
    channels: HashMap<SocketAddr, Channel>,
    peers: HashMap<u16, SocketAddr>,
    next_channel: u16,
    outbox: Vec<Vec<u8>>,
    /// What happened that a report should show (channels bound, errors, first data per peer).
    notes: Vec<String>,
    heard_from: Vec<SocketAddr>,
    /// Peers the server refused to relay to (403): never asked for again, nothing sent there.
    refused: HashSet<SocketAddr>,
}

impl TurnClient {
    /// A client that starts by asking for an allocation (the first datagram is in the outbox).
    pub fn new(server: &TurnServer, now: Instant) -> TurnClient {
        let mut client = TurnClient {
            server: server.address,
            username: server.username.clone(),
            password: server.password.clone(),
            realm: String::new(),
            nonce: Vec::new(),
            key: None,
            relayed: None,
            mapped: None,
            failure: None,
            refresh_at: None,
            pending: HashMap::new(),
            channels: HashMap::new(),
            peers: HashMap::new(),
            next_channel: FIRST_CHANNEL,
            outbox: Vec::new(),
            notes: Vec::new(),
            heard_from: Vec::new(),
            refused: HashSet::new(),
        };
        client.request(Request::Allocate, now);
        client
    }

    pub fn take_outbox(&mut self) -> Vec<Vec<u8>> {
        std::mem::take(&mut self.outbox)
    }

    pub fn take_notes(&mut self) -> Vec<String> {
        std::mem::take(&mut self.notes)
    }

    fn heard(&mut self, peer: SocketAddr) {
        if !self.heard_from.contains(&peer) {
            self.heard_from.push(peer);
            self.notes.push(format!("TURN: first data from {peer}"));
        }
    }

    /// Sends `payload` to `peer` through the relay.
    pub fn send(&mut self, peer: SocketAddr, payload: &[u8], now: Instant) {
        // ICE also tries pairs the server will not relay (for example to the other side's host address on
        // a private network): after the first refusal they are dropped here.
        if self.relayed.is_none() || self.refused.contains(&peer) {
            return;
        }
        if !self.channels.contains_key(&peer) && self.next_channel <= LAST_CHANNEL {
            let number = self.next_channel;
            self.next_channel += 1;
            self.channels.insert(
                peer,
                Channel {
                    number,
                    bound: false,
                    renew_at: now + CHANNEL_RENEW,
                },
            );
            self.peers.insert(number, peer);
            self.request(Request::ChannelBind(peer), now);
        }
        match self.channels.get(&peer) {
            Some(channel) if channel.bound => self
                .outbox
                .push(stun::channel_data(channel.number, payload)),
            _ => {
                let send = Message::new(Class::Indication, method::SEND, transaction())
                    .with_address(attr::XOR_PEER_ADDRESS, peer)
                    .with(attr::DATA, payload);
                self.outbox.push(send.encode(None, false));
            }
        }
    }

    /// A datagram that came from the TURN server.
    pub fn handle(&mut self, packet: &[u8], now: Instant) -> Incoming {
        if let Some((number, payload)) = stun::parse_channel_data(packet) {
            return match self.peers.get(&number).copied() {
                Some(peer) => {
                    self.heard(peer);
                    Incoming::Data {
                        peer,
                        payload: payload.to_vec(),
                    }
                }
                None => Incoming::Handled,
            };
        }
        let Some(message) = Message::decode(packet) else {
            return Incoming::Handled;
        };
        if message.class == Class::Indication && message.method == method::DATA {
            return match (
                message.address(attr::XOR_PEER_ADDRESS),
                message.get(attr::DATA),
            ) {
                (Some(peer), Some(data)) => {
                    self.heard(peer);
                    Incoming::Data {
                        peer,
                        payload: data.to_vec(),
                    }
                }
                _ => Incoming::Handled,
            };
        }
        let Some(pending) = self.pending.remove(&message.transaction) else {
            return Incoming::Handled;
        };
        // Once we have a key, a success must prove it knows it (anyone could send a forged one).
        if message.class == Class::Success
            && let Some(key) = self.key
            && !stun::check_integrity(packet, &key)
        {
            self.pending.insert(message.transaction, pending);
            return Incoming::Handled;
        }
        match (pending.request, message.class) {
            (request, Class::Error) => self.on_error(request, &message, now),
            (Request::Allocate, Class::Success) => {
                self.relayed = message.address(attr::XOR_RELAYED_ADDRESS);
                self.mapped = message.address(attr::XOR_MAPPED_ADDRESS);
                if self.relayed.is_none() {
                    self.failure = Some("the TURN server gave no relayed address".into());
                }
                self.refresh_at = Some(now + refresh_after(message.u32(attr::LIFETIME)));
            }
            (Request::Refresh, Class::Success) => {
                self.refresh_at = Some(now + refresh_after(message.u32(attr::LIFETIME)));
            }
            (Request::ChannelBind(peer), Class::Success) => {
                if let Some(channel) = self.channels.get_mut(&peer) {
                    if !channel.bound {
                        self.notes.push(format!(
                            "TURN: channel 0x{:04X} to {peer} bound",
                            channel.number
                        ));
                    }
                    channel.bound = true;
                    channel.renew_at = now + CHANNEL_RENEW;
                }
            }
            _ => {}
        }
        Incoming::Handled
    }

    /// Retransmissions, the allocation refresh and channel renewals that are due.
    pub fn poll(&mut self, now: Instant) {
        let due: Vec<[u8; 12]> = self
            .pending
            .iter()
            .filter(|(_, pending)| now >= pending.due())
            .map(|(transaction, _)| *transaction)
            .collect();
        for transaction in due {
            let Some(pending) = self.pending.get_mut(&transaction) else {
                continue;
            };
            if pending.tries < MAX_TRIES {
                pending.tries += 1;
                pending.sent = now;
                self.outbox.push(pending.bytes.clone());
                continue;
            }
            let request = pending.request;
            self.pending.remove(&transaction);
            match request {
                Request::Allocate | Request::Refresh => {
                    self.failure =
                        Some(format!("the TURN server at {} did not answer", self.server));
                }
                Request::ChannelBind(peer) => {
                    self.notes
                        .push(format!("TURN: no answer to the channel bind for {peer}"));
                    self.forget_channel(peer);
                }
                Request::Deallocate => {}
            }
        }
        if self.refresh_at.is_some_and(|at| now >= at) {
            self.refresh_at = None;
            self.request(Request::Refresh, now);
        }
        let renew: Vec<SocketAddr> = self
            .channels
            .iter()
            .filter(|(_, channel)| now >= channel.renew_at)
            .map(|(peer, _)| *peer)
            .collect();
        for peer in renew {
            if let Some(channel) = self.channels.get_mut(&peer) {
                channel.renew_at = now + CHANNEL_RENEW;
            }
            self.request(Request::ChannelBind(peer), now);
        }
    }

    pub fn next_deadline(&self) -> Option<Instant> {
        let pending = self.pending.values().map(Pending::due);
        let channels = self.channels.values().map(|channel| channel.renew_at);
        pending.chain(channels).chain(self.refresh_at).min()
    }

    /// Gives the allocation back (best effort: one datagram, no retries).
    pub fn close(&mut self, now: Instant) {
        if self.relayed.take().is_some() {
            self.request(Request::Deallocate, now);
        }
    }

    fn request(&mut self, request: Request, now: Instant) {
        let id = transaction();
        let message = match request {
            Request::Allocate => Message::new(Class::Request, method::ALLOCATE, id)
                .with(attr::REQUESTED_TRANSPORT, stun::TRANSPORT_UDP)
                .with(attr::LIFETIME, LIFETIME_SECONDS.to_be_bytes()),
            Request::Refresh => Message::new(Class::Request, method::REFRESH, id)
                .with(attr::LIFETIME, LIFETIME_SECONDS.to_be_bytes()),
            Request::Deallocate => Message::new(Class::Request, method::REFRESH, id)
                .with(attr::LIFETIME, 0u32.to_be_bytes()),
            Request::ChannelBind(peer) => {
                let Some(number) = self.channels.get(&peer).map(|channel| channel.number) else {
                    return;
                };
                let [high, low] = number.to_be_bytes();
                Message::new(Class::Request, method::CHANNEL_BIND, id)
                    .with(attr::CHANNEL_NUMBER, [high, low, 0, 0])
                    .with_address(attr::XOR_PEER_ADDRESS, peer)
            }
        };
        let bytes = match self.key {
            Some(key) => message
                .with(attr::USERNAME, self.username.as_bytes())
                .with(attr::REALM, self.realm.as_bytes())
                .with(attr::NONCE, self.nonce.clone())
                .encode(Some(&key), true),
            None => message.encode(None, true),
        };
        self.outbox.push(bytes.clone());
        if request != Request::Deallocate {
            self.pending.insert(
                id,
                Pending {
                    request,
                    bytes,
                    sent: now,
                    tries: 1,
                },
            );
        }
    }

    fn on_error(&mut self, request: Request, message: &Message, now: Instant) {
        let code = message.error_code().unwrap_or(0);
        // 401 the first time (credentials needed) or 438 at any time (a new nonce): take the server's realm
        // and nonce and send the request again.
        if (code == 401 && self.key.is_none()) || code == 438 {
            if let Some(realm) = message.text(attr::REALM) {
                self.realm = realm;
            }
            if let Some(nonce) = message.get(attr::NONCE) {
                self.nonce = nonce.to_vec();
            }
            self.key = Some(stun::long_term_key(
                &self.username,
                &self.realm,
                &self.password,
            ));
            self.request(request, now);
            return;
        }
        if let (Request::ChannelBind(peer), 403) = (request, code) {
            self.notes.push(format!(
                "TURN: the server does not relay to {peer} (403); that pair is left to direct ICE"
            ));
            self.refused.insert(peer);
            self.forget_channel(peer);
            return;
        }
        self.notes
            .push(format!("TURN: {request:?} answered with error {code}"));
        let reason = match code {
            401 => "the relay credentials were refused".to_string(),
            403 => "the TURN server does not relay to that address".to_string(),
            486 => "too many relay allocations for this device".to_string(),
            508 => "the TURN server has no relay ports left".to_string(),
            _ => format!("TURN error {code}"),
        };
        match request {
            Request::Allocate | Request::Refresh => self.failure = Some(reason),
            Request::ChannelBind(peer) => self.forget_channel(peer),
            Request::Deallocate => {}
        }
    }

    fn forget_channel(&mut self, peer: SocketAddr) {
        if let Some(channel) = self.channels.remove(&peer) {
            self.peers.remove(&channel.number);
        }
    }
}

fn refresh_after(lifetime: Option<u32>) -> Duration {
    let lifetime = Duration::from_secs(u64::from(lifetime.unwrap_or(LIFETIME_SECONDS)));
    lifetime
        .saturating_sub(REFRESH_EARLY)
        .max(Duration::from_secs(30))
}

/// A random STUN transaction id.
pub(crate) fn transaction() -> [u8; 12] {
    let mut id = [0u8; 12];
    getrandom::fill(&mut id).expect("the system random number generator");
    id
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Plays the TURN server's part for one allocation and one channel.
    #[test]
    fn allocates_with_credentials_binds_a_channel_and_relays() {
        let server = TurnServer {
            address: "198.51.100.1:3478".parse().unwrap(),
            username: "1999999999:device".into(),
            password: "secret".into(),
        };
        let now = Instant::now();
        let mut client = TurnClient::new(&server, now);
        // 1. The first Allocate has no credentials; the server answers 401 with realm and nonce.
        let first = Message::decode(&client.take_outbox()[0]).unwrap();
        assert_eq!(
            (first.method, first.has_integrity),
            (method::ALLOCATE, false)
        );
        let challenge = first
            .reply(Class::Error)
            .with_error(401, "Unauthorized")
            .with(attr::REALM, "queqiao")
            .with(attr::NONCE, "n1");
        client.handle(&challenge.encode(None, true), now);
        // 2. The second carries them; the server checks and allocates.
        let second_bytes = client.take_outbox().remove(0);
        let second = Message::decode(&second_bytes).unwrap();
        let key = stun::long_term_key("1999999999:device", "queqiao", "secret");
        assert!(stun::check_integrity(&second_bytes, &key));
        assert_eq!(
            second.text(attr::USERNAME).as_deref(),
            Some("1999999999:device")
        );
        let relayed: SocketAddr = "198.51.100.1:47200".parse().unwrap();
        let mapped: SocketAddr = "203.0.113.5:61000".parse().unwrap();
        let success = second
            .reply(Class::Success)
            .with_address(attr::XOR_RELAYED_ADDRESS, relayed)
            .with_address(attr::XOR_MAPPED_ADDRESS, mapped)
            .with(attr::LIFETIME, 600u32.to_be_bytes());
        // A success without the key is ignored; with it, the allocation is ours.
        client.handle(&success.encode(None, true), now);
        assert_eq!(client.relayed, None);
        client.handle(&success.encode(Some(&key), true), now);
        assert_eq!(
            (client.relayed, client.mapped),
            (Some(relayed), Some(mapped))
        );
        // 3. Sending to a peer: ChannelBind plus a Send indication; ChannelData once bound.
        let peer: SocketAddr = "192.0.2.7:5000".parse().unwrap();
        client.send(peer, b"hello", now);
        let out = client.take_outbox();
        let bind = Message::decode(&out[0]).unwrap();
        assert_eq!(bind.method, method::CHANNEL_BIND);
        assert_eq!(bind.address(attr::XOR_PEER_ADDRESS), Some(peer));
        assert_eq!(
            Message::decode(&out[1]).unwrap().get(attr::DATA),
            Some(&b"hello"[..])
        );
        client.handle(&bind.reply(Class::Success).encode(Some(&key), true), now);
        client.send(peer, b"again", now);
        assert_eq!(
            stun::parse_channel_data(&client.take_outbox()[0]),
            Some((FIRST_CHANNEL, &b"again"[..]))
        );
        // 4. What the peer sends comes back on the channel.
        match client.handle(&stun::channel_data(FIRST_CHANNEL, b"reply"), now) {
            Incoming::Data {
                peer: from,
                payload,
            } => assert_eq!((from, payload.as_slice()), (peer, &b"reply"[..])),
            Incoming::Handled => panic!("channel data not delivered"),
        }
        // 5. A peer the server refuses (403) is not asked for again, and nothing is sent to it.
        let private: SocketAddr = "10.1.2.3:6000".parse().unwrap();
        client.send(private, b"check", now);
        let out = client.take_outbox();
        let refused_bind = Message::decode(&out[0]).unwrap();
        assert_eq!(refused_bind.method, method::CHANNEL_BIND);
        client.handle(
            &refused_bind
                .reply(Class::Error)
                .with_error(403, "Forbidden")
                .encode(None, true),
            now,
        );
        client.send(private, b"check again", now);
        assert!(
            client.take_outbox().is_empty(),
            "nothing more for a refused peer"
        );
        // 6. Timers: the channel renewal comes first (240 s), then the allocation refresh (540 s).
        assert_eq!(client.next_deadline(), Some(now + CHANNEL_RENEW));
        client.poll(now + Duration::from_secs(541));
        let renewals: Vec<u16> = client
            .take_outbox()
            .iter()
            .map(|d| Message::decode(d).unwrap().method)
            .collect();
        assert!(renewals.contains(&method::REFRESH) && renewals.contains(&method::CHANNEL_BIND));
    }
}
