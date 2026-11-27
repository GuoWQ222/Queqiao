//! Direct paths past what ICE finds by itself ("punching"), Queqiao's own design (docs/DESIGN.md, "Direct paths").
//!
//! ICE connects two sides directly when each can reach an address the other offers. Two things stop it:
//! - a symmetric NAT (a phone carrier's, many CGNATs) gives every new destination a new public port, so the address a
//!   side learns from its STUN server is not the one its packets to the other side come from;
//! - a firewall in front of a public address (Windows' own) lets in only what answers its own sends: from an address it
//!   has not sent to, nothing.
//!
//! Both are beaten by sending first to where the other side's packets will come from. Each side asks up to three STUN
//! servers from each of its IPv4 sockets and tells the other, in its SDP (`a=x-queqiao-nat`), what its NAT does (no
//! NAT, one mapping for every destination, a new mapping per destination with ports a fixed step apart, or at random).
//! From the two answers each side plans (`plan`): its packets to the other's public address, to the ports the other's
//! NAT will hand out next (linear), to many random ports of its address (random), and, when its own NAT is random, from
//! many new sockets at once, so that one of their mappings meets the other side's random ports.
//!
//! The packets are this module's own, 28 bytes, authenticated with a key made from both sides' ICE passwords
//! (`Key`): one from anyone else is ignored. A packet that comes, or the answer to one, names a path that works: the
//! session hands it to ICE (str0m) as the other side's candidate, and a new socket that found it becomes one of the
//! session's sockets. ICE checks the pair and moves the session there (a direct pair outranks the relay); punching
//! waits while the session is direct. A round lasts `LIMIT`; while the session stays relayed, new rounds follow
//! (`gap_after`: every minute, after five minutes every five), the roles of the last resort swapped each time, and a
//! direct path that stops brings a new round at once.

use std::collections::HashSet;
use std::io::ErrorKind;
use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::sync::Arc;
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

/// The first byte of a punch packet. FEC parity is 0xFB and QUIC 0xFC; STUN (0-3), DTLS (20-63) and RTP (128-191)
/// never start with it.
pub(crate) const MAGIC: u8 = 0xFA;
pub(crate) const SDP_ATTRIBUTE: &str = "x-queqiao-nat";
const PACKET: usize = 28;
/// The largest port step taken as a linear NAT's.
const MAX_STEP: i32 = 16;
/// How often the lists send.
pub(crate) const TICK: Duration = Duration::from_millis(10);
/// How long a round of punching lasts at most (its plans' times count from its start).
pub(crate) const LIMIT: Duration = Duration::from_secs(30);
/// New sockets one session opens at most (all plans together).
const MAX_SOCKETS: usize = 300;
/// When the many-sockets and random-ports methods start where a prediction was tried first. Measured 2026-11-27 on
/// sunshine's carrier (its STUN answers count up by one, so it looks predictable): the ports it gave towards the PC came
/// from another block each session (33651-33786, 46407-46503), never the predicted ones; the many sockets met the
/// PC's random ports 0.3 to 1.6 s after they started, in each of three sessions.
const FALLBACK: Duration = Duration::from_millis(1500);
/// How long after the other side's many sockets start (all of them send at once) a sweep of its ports starts. Measured
/// 2026-11-27 on sunshine's carrier: a packet that reaches the NAT at a port before a mapping is made there leaves that
/// port broken for the mapping made after (its packets still come out, nothing gets in): with the sweep and the sockets
/// starting together, four sessions of four found only such a one-way path.
const SWEEP_AFTER: Duration = Duration::from_secs(1);

/// What a socket's NAT does, from the public addresses STUN servers at different addresses saw.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Nat {
    /// The socket's own address is its public one. A firewall may still let in only what answers its own sends.
    Open,
    /// One public address for every destination.
    Cone,
    /// A new public port for every new destination, this far from the one before.
    Linear(i32),
    /// A new public port for every new destination, not predictable.
    Random,
    /// One answer only: whether the NAT keeps one mapping is not known.
    Unknown,
}

impl Nat {
    /// The kind alone, as an anonymous path report names it (`PathReport::NATS`; no step, no address).
    pub(crate) fn kind(info: Option<&NatInfo>) -> &'static str {
        match info.map(|info| info.nat) {
            None => "none",
            Some(Nat::Open) => "open",
            Some(Nat::Cone) => "cone",
            Some(Nat::Linear(_)) => "linear",
            Some(Nat::Random) => "random",
            Some(Nat::Unknown) => "unknown",
        }
    }

    fn word(self) -> String {
        match self {
            Nat::Open => "open".into(),
            Nat::Cone => "cone".into(),
            Nat::Linear(step) => format!("linear:{step}"),
            Nat::Random => "random".into(),
            Nat::Unknown => "unknown".into(),
        }
    }

    fn parse(word: &str) -> Option<Nat> {
        Some(match word {
            "open" => Nat::Open,
            "cone" => Nat::Cone,
            "random" => Nat::Random,
            "unknown" => Nat::Unknown,
            _ => {
                let step: i32 = word.strip_prefix("linear:")?.parse().ok()?;
                if step == 0 || step.abs() > MAX_STEP {
                    return None;
                }
                Nat::Linear(step)
            }
        })
    }

    /// Whether the other side can be let in at this side's public address once this side has sent there: one
    /// mapping (or none) whatever the destination.
    fn exact(self) -> bool {
        matches!(self, Nat::Open | Nat::Cone | Nat::Unknown)
    }

    pub(crate) fn describe(self) -> String {
        match self {
            Nat::Open => "no NAT".into(),
            Nat::Cone => "a NAT with one public address for every destination".into(),
            Nat::Linear(step) => format!(
                "a NAT with a new public port for each destination, {step:+} each time (predictable)"
            ),
            Nat::Random => "a NAT with a new public port for each destination, at random".into(),
            Nat::Unknown => "not known (one STUN answer)".into(),
        }
    }
}

/// One socket's NAT: its address (`base`), what its NAT does, and its public address (for a linear NAT the latest one
/// seen: the next destination gets the port after it).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct NatInfo {
    pub base: SocketAddr,
    pub nat: Nat,
    pub public: SocketAddr,
}

/// What `base`'s NAT does, from the public addresses that STUN servers at different addresses saw, in the order they
/// were asked (the NAT hands out mappings in that order).
pub(crate) fn classify(base: SocketAddr, mapped: &[SocketAddr]) -> Option<NatInfo> {
    let last = *mapped.last()?;
    let info = |nat| {
        Some(NatInfo {
            base,
            nat,
            public: last,
        })
    };
    if mapped.iter().all(|address| *address == base) {
        return info(Nat::Open);
    }
    if mapped.iter().any(|address| address.ip() != last.ip()) {
        // A pool of public addresses, a different one per destination: nothing to predict.
        return info(Nat::Random);
    }
    if mapped.len() == 1 {
        return info(Nat::Unknown);
    }
    if mapped.iter().all(|address| address.port() == last.port()) {
        return info(Nat::Cone);
    }
    let steps: Vec<i32> = mapped
        .windows(2)
        .map(|pair| i32::from(pair[1].port()) - i32::from(pair[0].port()))
        .collect();
    let step = steps[0];
    if step != 0 && step.abs() <= MAX_STEP && steps.iter().all(|other| *other == step) {
        return info(Nat::Linear(step));
    }
    info(Nat::Random)
}

/// `sdp` with a line for each socket's NAT, before its first media section (session level).
pub(crate) fn add_to_sdp(sdp: &str, infos: &[NatInfo]) -> String {
    let lines: String = infos
        .iter()
        .map(|info| {
            format!(
                "a={SDP_ATTRIBUTE}:{} {} {}\r\n",
                info.base,
                info.nat.word(),
                info.public
            )
        })
        .collect();
    match sdp.find("\r\nm=") {
        Some(at) => format!("{}{lines}{}", &sdp[..at + 2], &sdp[at + 2..]),
        None => format!("{sdp}{lines}"),
    }
}

/// The other side's NATs from its SDP (none from a Queqiao of before 2026-11-27: then no punching).
pub(crate) fn from_sdp(sdp: &str) -> Vec<NatInfo> {
    let prefix = format!("a={SDP_ATTRIBUTE}:");
    sdp.lines()
        .filter_map(|line| line.trim().strip_prefix(&prefix))
        .filter_map(|value| {
            let mut parts = value.split_whitespace();
            let base: SocketAddr = parts.next()?.parse().ok()?;
            let nat = Nat::parse(parts.next()?)?;
            let public: SocketAddr = parts.next()?.parse().ok()?;
            (base.is_ipv4() && public.is_ipv4()).then_some(NatInfo { base, nat, public })
        })
        .take(4)
        .collect()
}

/// The key punch packets are signed with: from both sides' ICE passwords (each side has both SDPs), so only the two
/// ends of this session can make or check one.
#[derive(Clone)]
pub(crate) struct Key([u8; 32]);

/// A punch packet: a request (answered from the socket it came to) or an answer, with the request's nonce.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Packet {
    Request([u8; 8]),
    Answer([u8; 8]),
}

/// Whether `datagram` looks like a punch packet (the key decides whether it is one).
pub(crate) fn is_punch(datagram: &[u8]) -> bool {
    datagram.len() == PACKET && datagram[..3] == [MAGIC, b'Q', b'P']
}

impl Key {
    pub(crate) fn from_sdps(offer: &str, answer: &str) -> Option<Key> {
        let password = |sdp: &str| {
            sdp.lines()
                .find_map(|line| line.trim().strip_prefix("a=ice-pwd:"))
                .map(|password| password.trim().to_string())
                .filter(|password| !password.is_empty())
        };
        let (offer, answer) = (password(offer)?, password(answer)?);
        let mut hash = Sha256::new();
        hash.update(b"queqiao punch key\0");
        hash.update(offer.as_bytes());
        hash.update([0]);
        hash.update(answer.as_bytes());
        Some(Key(hash.finalize().into()))
    }

    pub(crate) fn encode(&self, packet: Packet) -> Vec<u8> {
        let (kind, nonce) = match packet {
            Packet::Request(nonce) => (1, nonce),
            Packet::Answer(nonce) => (2, nonce),
        };
        let mut out = vec![MAGIC, b'Q', b'P', kind];
        out.extend_from_slice(&nonce);
        let mac = hmac_sha256(&self.0, &out);
        out.extend_from_slice(&mac[..16]);
        out
    }

    pub(crate) fn decode(&self, datagram: &[u8]) -> Option<Packet> {
        if !is_punch(datagram) {
            return None;
        }
        let mac = hmac_sha256(&self.0, &datagram[..12]);
        // Compared in constant time.
        if mac[..16]
            .iter()
            .zip(&datagram[12..])
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            != 0
        {
            return None;
        }
        let nonce: [u8; 8] = datagram[4..12].try_into().ok()?;
        match datagram[3] {
            1 => Some(Packet::Request(nonce)),
            2 => Some(Packet::Answer(nonce)),
            _ => None,
        }
    }
}

fn hmac_sha256(key: &[u8; 32], data: &[u8]) -> [u8; 32] {
    let (mut inner, mut outer) = ([0x36u8; 64], [0x5cu8; 64]);
    for (index, byte) in key.iter().enumerate() {
        inner[index] ^= byte;
        outer[index] ^= byte;
    }
    let inner = Sha256::new()
        .chain_update(inner)
        .chain_update(data)
        .finalize();
    Sha256::new()
        .chain_update(outer)
        .chain_update(inner)
        .finalize()
        .into()
}

/// One part of the punching: packets from the session's socket `base` (`sockets` 0), or from `sockets` new sockets on
/// `base`'s address, to `targets` in turn, between `start` and `until` after the network started.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Plan {
    pub what: String,
    pub base: SocketAddr,
    pub sockets: usize,
    pub targets: Vec<SocketAddr>,
    /// Packets each tick (from new sockets: that many sockets send one each).
    pub per_tick: usize,
    pub start: Duration,
    pub until: Duration,
}

/// Ports on `ip` predicted for a linear NAT: the `count` after `public`, `step` apart.
fn predicted(public: SocketAddr, step: i32, count: i32) -> Vec<SocketAddr> {
    (1..=count)
        .filter_map(|index| {
            let port = i32::from(public.port()) + step * index;
            u16::try_from(port)
                .ok()
                .filter(|port| *port >= 1024)
                .map(|port| SocketAddr::new(public.ip(), port))
        })
        .collect()
}

/// Every `stride`th port of `ip` from 1024 up, starting at a random one of the first `stride`: a block of `stride` or more
/// ports in a row is met for certain (a NAT counting up per destination gives the other side's many sockets such a
/// block: sunshine's carrier, 2026-11-27), any scattered port with the same odds as random ones.
fn sweep(ip: IpAddr, stride: u16, random: &mut dyn FnMut() -> u16) -> Vec<SocketAddr> {
    let first = 1024 + random() % stride.max(1);
    (first..=u16::MAX)
        .step_by(usize::from(stride.max(1)))
        .map(|port| SocketAddr::new(ip, port))
        .collect()
}

/// `count` different random ports (1024 and up) on `ip`.
fn random_ports(ip: IpAddr, count: usize, random: &mut dyn FnMut() -> u16) -> Vec<SocketAddr> {
    let mut seen = HashSet::new();
    let mut out = Vec::with_capacity(count);
    let mut tries = 0;
    while out.len() < count && tries < count * 20 {
        tries += 1;
        let port = random();
        if port >= 1024 && seen.insert(port) {
            out.push(SocketAddr::new(ip, port));
        }
    }
    out
}

/// What this side does for the pair (`local` one of its sockets, `remote` one of the other side's), the strategy for
/// each kind of NAT on either side. `controlling`: this side's ICE role (the two sides take turns in the last resort).
pub(crate) fn plan(
    local: &NatInfo,
    remote: &NatInfo,
    controlling: bool,
    random: &mut dyn FnMut() -> u16,
) -> Vec<Plan> {
    let second = |seconds: u64| Duration::from_secs(seconds);
    let mut plans = Vec::new();
    let mut add = |what: &str, sockets: usize, targets: Vec<SocketAddr>, per_tick, start, until| {
        if !targets.is_empty() {
            plans.push(Plan {
                what: what.to_string(),
                base: local.base,
                sockets,
                targets,
                per_tick,
                start,
                until,
            });
        }
    };
    // First, to its public address: this side's NAT and firewall open towards it, and on a linear NAT this is the next
    // mapping, the one the other side predicts.
    add(
        "to its public address",
        0,
        vec![remote.public],
        1,
        Duration::ZERO,
        LIMIT,
    );
    let ip = remote.public.ip();
    match (local.nat, remote.nat) {
        // Theirs predictable, ours one mapping (or predictable too, below): the ports its NAT hands out next, at once;
        // then, if none answered, a sweep of every 128th port (its many sockets, 150, get a block in a row there).
        (ours, Nat::Linear(step)) if ours.exact() => {
            add(
                "to the 50 ports its NAT hands out next",
                0,
                predicted(remote.public, step, 50),
                1,
                Duration::ZERO,
                LIMIT,
            );
            add(
                "to every 128th port of its address",
                0,
                sweep(ip, 128, &mut *random),
                5,
                FALLBACK + SWEEP_AFTER,
                LIMIT,
            );
        }
        // Both predictable: each side predicts the port the other's NAT gives its 30th new socket, and opens 30 sockets
        // sending there; each side's 30th socket then has the port the other predicted.
        (Nat::Linear(_), Nat::Linear(step)) => add(
            "from 30 new sockets to the port its NAT gives its 30th",
            30,
            predicted(remote.public, step, 30)
                .last()
                .copied()
                .into_iter()
                .collect(),
            1,
            Duration::ZERO,
            LIMIT,
        ),
        // Theirs random, ours one mapping: every 32nd port of its address while it sends from many sockets (150 random
        // ports: one of them is met with 99 % odds).
        (ours, Nat::Random) if ours.exact() => add(
            "to every 32nd port of its address",
            0,
            sweep(ip, 32, &mut *random),
            5,
            second(1) + SWEEP_AFTER,
            LIMIT,
        ),
        // Ours random, theirs one mapping: many sockets towards its public address, one of them meets its random ports.
        (Nat::Random, theirs) if theirs.exact() => add(
            "from 150 new sockets to its public address",
            150,
            vec![remote.public],
            5,
            second(1),
            LIMIT,
        ),
        // Ours predictable, theirs one mapping: the other side predicts; if that found nothing in FALLBACK, many sockets.
        (Nat::Linear(_), theirs) if theirs.exact() => add(
            "from 150 new sockets to its public address",
            150,
            vec![remote.public],
            5,
            FALLBACK,
            LIMIT,
        ),
        // Both without one mapping and one of them random: random ports only (a long shot).
        (Nat::Linear(_) | Nat::Random, Nat::Random) | (Nat::Random, Nat::Linear(_)) => add(
            "to 100 random ports of its address",
            0,
            random_ports(ip, 100, &mut *random),
            1,
            second(1),
            LIMIT,
        ),
        // Both one mapping: ICE alone connects; if it has not after 10 s, the sides take turns, one sweeping its
        // ports, the other sending from many sockets, then the other way round.
        _ => {
            let (first_spray, first_sockets) = if controlling {
                (second(10), second(20))
            } else {
                (second(20), second(10))
            };
            add(
                "to every 128th port of its address",
                0,
                sweep(ip, 128, &mut *random),
                5,
                first_spray + SWEEP_AFTER,
                first_spray + second(10),
            );
            add(
                "from 150 new sockets to its public address",
                150,
                vec![remote.public],
                5,
                first_sockets,
                first_sockets + second(10),
            );
        }
    }
    plans
}

/// What punching found or received.
pub(crate) enum Event {
    /// A packet (`remote` to `local`) that shows the path works; `socket` when `local` is a new socket (it becomes one
    /// of the session's).
    Found {
        local: SocketAddr,
        remote: SocketAddr,
        socket: Option<Arc<UdpSocket>>,
    },
    /// Something other than a punch packet on a new socket (ICE once the other side uses the path): for str0m, and the
    /// socket becomes the session's.
    Deliver {
        local: SocketAddr,
        source: SocketAddr,
        contents: Vec<u8>,
        socket: Arc<UdpSocket>,
    },
}

struct Running {
    plan: Plan,
    next: usize,
    sockets: Vec<(Arc<UdpSocket>, SocketAddr)>,
    opened: bool,
    noted: bool,
    /// When the current round (every target once, from every socket) began.
    round_started: Option<Instant>,
}

/// Rounds begin no more often than this: a short list (one address) sends 10 packets a second, not 100.
const ROUND_GAP: Duration = Duration::from_millis(100);

/// Whether a list may send this tick: always within a round (`next` counts its packets); a new round once `ROUND_GAP`
/// has passed since the last began (`started`).
fn round_due(started: &mut Option<Instant>, next: usize, now: Instant, round: usize) -> bool {
    if round == 0 {
        return false;
    }
    if next.is_multiple_of(round) {
        if started.is_some_and(|at| now < at + ROUND_GAP) {
            return false;
        }
        *started = Some(now);
    }
    true
}

/// The punching of one session, driven by the session's network thread.
pub(crate) struct Puncher {
    key: Key,
    started: Instant,
    running: Vec<Running>,
    next_tick: Instant,
    stopped: bool,
    found: HashSet<(SocketAddr, SocketAddr)>,
    sockets_opened: usize,
    /// Punch packets noted so far (the first few each way are, with where they came from and went to).
    packets_noted: u32,
    notes: Vec<String>,
    /// Rounds (`rounds`): the pairs of sockets planned for again each round, this side's ICE role, the round under way
    /// and when it began, when the next begins (between rounds), and whether punching waits while the path is direct.
    pairs: Vec<(NatInfo, NatInfo)>,
    controlling: bool,
    round: u32,
    round_began: Instant,
    next_round: Option<Instant>,
    paused: bool,
    /// The most rounds (the server's `PunchSettings::rounds`; 0: no limit).
    max_rounds: u32,
}

/// After round `round` ended without a direct path, how long until the next: a new round every minute for the first
/// five minutes, then every five minutes, for as long as the session is relayed (as UU Remote punches again round
/// after round until it is direct).
fn gap_after(round: u32) -> Duration {
    if round < 4 {
        Duration::from_secs(30)
    } else {
        Duration::from_secs(270)
    }
}

impl Puncher {
    /// One round of `plans` only (tests).
    pub(crate) fn new(key: Key, plans: Vec<Plan>, started: Instant) -> Puncher {
        Puncher {
            key,
            started,
            running: running(plans, false),
            next_tick: started,
            stopped: false,
            found: HashSet::new(),
            sockets_opened: 0,
            packets_noted: 0,
            notes: Vec::new(),
            pairs: Vec::new(),
            controlling: false,
            round: 0,
            round_began: started,
            next_round: None,
            paused: false,
            max_rounds: 0,
        }
    }

    /// No more than `rounds` rounds (0: no limit), as the server set it.
    pub(crate) fn limit_rounds(mut self, rounds: u32) -> Puncher {
        self.max_rounds = rounds;
        self
    }

    /// Round after round for `pairs` (one of this side's sockets, one of the other's) until the session is direct;
    /// the roles of the last resort (`plan`, both sides one mapping) swap from one round to the next.
    pub(crate) fn rounds(
        key: Key,
        pairs: Vec<(NatInfo, NatInfo)>,
        controlling: bool,
        started: Instant,
    ) -> Puncher {
        let mut puncher = Puncher::new(key, Vec::new(), started);
        puncher.pairs = pairs;
        puncher.controlling = controlling;
        puncher.running = running(puncher.plans(), false);
        puncher
    }

    /// The plans of the round under way.
    fn plans(&self) -> Vec<Plan> {
        // Odd rounds swap the roles: the side that sprayed first sends from many sockets first.
        let controlling = self.controlling ^ (self.round % 2 == 1);
        let mut random = random_u16;
        self.pairs
            .iter()
            .flat_map(|(local, remote)| plan(local, remote, controlling, &mut random))
            .collect()
    }

    /// The next round begins: its plans afresh (new sockets, new sweep offsets).
    fn begin_round(&mut self, now: Instant) {
        self.round += 1;
        self.round_began = now;
        self.next_round = None;
        self.sockets_opened = 0;
        // Its lists are noted for the first rounds only.
        self.running = running(self.plans(), self.round >= 3);
        self.notes.push(format!(
            "punching: round {}{}",
            self.round + 1,
            if self.round % 2 == 1 {
                " (the roles swapped)"
            } else {
                ""
            }
        ));
    }

    /// The session went direct: punching waits (its sockets close) until the path is relayed again.
    pub(crate) fn pause(&mut self) {
        if self.paused || self.stopped {
            return;
        }
        self.paused = true;
        for running in &mut self.running {
            running.sockets.clear();
        }
        self.notes
            .push("punching waits: the session goes direct".into());
    }

    /// The session is relayed again after it was direct (the direct path stopped): a new round at once.
    pub(crate) fn resume(&mut self, now: Instant) {
        if !self.paused || self.stopped {
            return;
        }
        self.paused = false;
        self.notes
            .push("punching again: the session is relayed again".into());
        self.begin_round(now);
    }

    pub(crate) fn paused(&self) -> bool {
        self.paused
    }

    pub(crate) fn take_notes(&mut self) -> Vec<String> {
        std::mem::take(&mut self.notes)
    }

    /// The packets due from the session's own sockets (from, to, datagram) into `out`; new sockets send by themselves.
    /// When to call again (`None`: punching is over).
    pub(crate) fn poll(
        &mut self,
        now: Instant,
        out: &mut Vec<(SocketAddr, SocketAddr, Vec<u8>)>,
    ) -> Option<Instant> {
        if self.stopped || self.paused {
            return None;
        }
        if let Some(at) = self.next_round {
            if now < at {
                return Some(at);
            }
            self.begin_round(now);
        }
        let elapsed = now.saturating_duration_since(self.round_began);
        if elapsed >= LIMIT
            || self
                .running
                .iter()
                .all(|running| elapsed >= running.plan.until)
        {
            for running in &mut self.running {
                running.sockets.clear();
            }
            if self.pairs.is_empty() {
                self.notes.push(format!(
                    "punching over after {} s without a direct path",
                    elapsed.as_secs()
                ));
                self.stop();
                return None;
            }
            if self.max_rounds > 0 && self.round + 1 >= self.max_rounds {
                self.notes.push(format!(
                    "punching over after {} rounds without a direct path (the server's limit)",
                    self.round + 1
                ));
                self.stop();
                return None;
            }
            let gap = gap_after(self.round);
            self.notes.push(format!(
                "punching: round {} over without a direct path, the next in {} s",
                self.round + 1,
                gap.as_secs()
            ));
            let at = now + gap;
            self.next_round = Some(at);
            return Some(at);
        }
        if now < self.next_tick {
            return Some(self.next_tick);
        }
        self.next_tick = now + TICK;
        let key = self.key.clone();
        for running in &mut self.running {
            let plan = &running.plan;
            if elapsed < plan.start || elapsed >= plan.until {
                if elapsed >= plan.until && !running.sockets.is_empty() {
                    // Its time is over: its sockets close (any that found a path was taken before).
                    running.sockets.clear();
                }
                continue;
            }
            if !running.noted {
                running.noted = true;
                self.notes.push(format!(
                    "punching from {}: {}{}",
                    plan.base,
                    plan.what,
                    match plan.targets.as_slice() {
                        [one] => format!(" ({one})"),
                        many => format!(" ({} ...)", many[0]),
                    }
                ));
            }
            if plan.sockets == 0 {
                let round = plan.targets.len();
                if !round_due(&mut running.round_started, running.next, now, round) {
                    continue;
                }
                for _ in 0..plan.per_tick.min(round - running.next % round) {
                    let target = plan.targets[running.next % round];
                    running.next += 1;
                    out.push((plan.base, target, key.encode(Packet::Request(nonce()))));
                }
                continue;
            }
            if !running.opened {
                running.opened = true;
                let room = MAX_SOCKETS.saturating_sub(self.sockets_opened);
                for _ in 0..plan.sockets.min(room) {
                    if let Ok(socket) = UdpSocket::bind(SocketAddr::new(plan.base.ip(), 0))
                        && socket.set_nonblocking(true).is_ok()
                        && let Ok(local) = socket.local_addr()
                    {
                        running.sockets.push((Arc::new(socket), local));
                    }
                }
                self.sockets_opened += running.sockets.len();
            }
            if running.sockets.is_empty() {
                continue;
            }
            let round = running.sockets.len() * plan.targets.len();
            if !round_due(&mut running.round_started, running.next, now, round) {
                continue;
            }
            // The first round all at once: every socket's mapping exists before the other side's sweep reaches it
            // (`SWEEP_AFTER`); later rounds keep them alive at `per_tick`.
            let per_tick = if running.next < round {
                round
            } else {
                plan.per_tick
            };
            for _ in 0..per_tick.min(round - running.next % round) {
                let index = running.next % running.sockets.len();
                let target =
                    plan.targets[(running.next / running.sockets.len()) % plan.targets.len()];
                running.next += 1;
                let (socket, _) = &running.sockets[index];
                let _ = socket.send_to(&key.encode(Packet::Request(nonce())), target);
            }
        }
        Some(self.next_tick)
    }

    /// What the new sockets received (without blocking): punch requests answered from their socket, a path found on
    /// a valid request or answer, anything else handed on for str0m.
    pub(crate) fn receive(&mut self) -> Vec<Event> {
        let mut events = Vec::new();
        let mut buffer = [0u8; 2048];
        let key = self.key.clone();
        for running in &mut self.running {
            let mut taken = Vec::new();
            for (index, (socket, local)) in running.sockets.iter().enumerate() {
                loop {
                    match socket.recv_from(&mut buffer) {
                        Ok((length, source)) => {
                            let contents = &buffer[..length];
                            match key.decode(contents) {
                                Some(packet) => {
                                    if self.packets_noted < NOTED_PACKETS {
                                        self.packets_noted += 1;
                                        self.notes.push(packet_note(
                                            packet,
                                            source,
                                            *local,
                                            self.started,
                                        ));
                                    }
                                    if let Packet::Request(nonce) = packet {
                                        let _ = socket
                                            .send_to(&key.encode(Packet::Answer(nonce)), source);
                                    }
                                    if self.found.insert((*local, source)) {
                                        taken.push(index);
                                        events.push(Event::Found {
                                            local: *local,
                                            remote: source,
                                            socket: Some(socket.clone()),
                                        });
                                    }
                                }
                                None if !is_punch(contents) => {
                                    taken.push(index);
                                    events.push(Event::Deliver {
                                        local: *local,
                                        source,
                                        contents: contents.to_vec(),
                                        socket: socket.clone(),
                                    });
                                }
                                None => {}
                            }
                        }
                        Err(error) if error.kind() == ErrorKind::WouldBlock => break,
                        // An ICMP "port unreachable" for an earlier send (Windows): the next read goes on.
                        Err(error) if error.kind() == ErrorKind::ConnectionReset => {}
                        Err(_) => break,
                    }
                }
            }
            // A socket that found a path is the session's from now on: it leaves the punching (and stops sending).
            taken.sort_unstable();
            taken.dedup();
            for index in taken.into_iter().rev() {
                running.sockets.remove(index);
            }
        }
        events
    }

    /// A punch packet on one of the session's own sockets: the answer to send back (to a request), and the path when
    /// it is new.
    pub(crate) fn on_packet(
        &mut self,
        local: SocketAddr,
        source: SocketAddr,
        datagram: &[u8],
    ) -> (Option<Vec<u8>>, bool) {
        let Some(packet) = self.key.decode(datagram) else {
            return (None, false);
        };
        if self.packets_noted < NOTED_PACKETS {
            self.packets_noted += 1;
            self.notes
                .push(packet_note(packet, source, local, self.started));
        }
        let answer = match packet {
            Packet::Request(nonce) => Some(self.key.encode(Packet::Answer(nonce))),
            Packet::Answer(_) => None,
        };
        (answer, self.found.insert((local, source)))
    }

    /// Punching ends (the session went direct, or its time is over): the new sockets close.
    pub(crate) fn stop(&mut self) {
        self.stopped = true;
        for running in &mut self.running {
            running.sockets.clear();
        }
    }

    pub(crate) fn stopped(&self) -> bool {
        self.stopped
    }
}

/// Punch packets each side notes, the first ones (where they came from and to which socket: a path that works one
/// way only shows there).
const NOTED_PACKETS: u32 = 8;

fn packet_note(packet: Packet, source: SocketAddr, local: SocketAddr, started: Instant) -> String {
    format!(
        "punch {} from {source} to {local} after {} ms",
        match packet {
            Packet::Request(_) => "request",
            Packet::Answer(_) => "answer",
        },
        started.elapsed().as_millis()
    )
}

/// The state of each plan of a round (`noted`: its list not noted again).
fn running(plans: Vec<Plan>, noted: bool) -> Vec<Running> {
    plans
        .into_iter()
        .map(|plan| Running {
            plan,
            next: 0,
            sockets: Vec::new(),
            opened: false,
            noted,
            round_started: None,
        })
        .collect()
}

fn random_u16() -> u16 {
    let mut bytes = [0u8; 2];
    let _ = getrandom::fill(&mut bytes);
    u16::from_le_bytes(bytes)
}

fn nonce() -> [u8; 8] {
    let mut nonce = [0u8; 8];
    let _ = getrandom::fill(&mut nonce);
    nonce
}

#[cfg(test)]
mod tests {
    use super::*;

    fn address(text: &str) -> SocketAddr {
        text.parse().unwrap()
    }

    #[test]
    fn nats_are_told_apart() {
        let base = address("33.252.114.161:62664");
        let nat = |mapped: &[&str]| {
            classify(
                base,
                &mapped.iter().map(|text| address(text)).collect::<Vec<_>>(),
            )
            .unwrap()
            .nat
        };
        // sunshine's carrier (2026-11-27): +1 for every new destination.
        assert_eq!(
            nat(&[
                "122.13.163.201:57657",
                "122.13.163.201:57658",
                "122.13.163.201:57659"
            ]),
            Nat::Linear(1)
        );
        assert_eq!(
            nat(&["122.13.163.201:57657", "122.13.163.201:57655"]),
            Nat::Linear(-2)
        );
        assert_eq!(
            nat(&["1.2.3.4:5000", "1.2.3.4:5000", "1.2.3.4:5000"]),
            Nat::Cone
        );
        assert_eq!(
            nat(&["1.2.3.4:5000", "1.2.3.4:31000", "1.2.3.4:9000"]),
            Nat::Random
        );
        assert_eq!(
            nat(&["1.2.3.4:5000", "1.2.3.4:5001", "1.2.3.4:5003"]),
            Nat::Random
        );
        assert_eq!(nat(&["1.2.3.4:5000", "1.2.3.5:5000"]), Nat::Random);
        assert_eq!(nat(&["1.2.3.4:5000"]), Nat::Unknown);
        assert_eq!(
            nat(&["33.252.114.161:62664", "33.252.114.161:62664"]),
            Nat::Open
        );
        assert!(classify(base, &[]).is_none());
        let info = classify(
            base,
            &[
                address("122.13.163.201:57657"),
                address("122.13.163.201:57658"),
            ],
        )
        .unwrap();
        assert_eq!(info.public, address("122.13.163.201:57658"));
    }

    #[test]
    fn the_sdp_carries_each_sockets_nat() {
        let infos = [
            NatInfo {
                base: address("33.252.114.161:62664"),
                nat: Nat::Linear(1),
                public: address("122.13.163.201:57659"),
            },
            NatInfo {
                base: address("59.66.29.128:52960"),
                nat: Nat::Open,
                public: address("59.66.29.128:52960"),
            },
        ];
        let sdp = add_to_sdp("v=0\r\ns=-\r\nm=video 9 UDP/TLS/RTP/SAVPF 96\r\n", &infos);
        assert!(sdp.contains(
            "a=x-queqiao-nat:33.252.114.161:62664 linear:1 122.13.163.201:57659\r\n\
             a=x-queqiao-nat:59.66.29.128:52960 open 59.66.29.128:52960\r\nm=video"
        ));
        assert_eq!(from_sdp(&sdp), infos);
        // Nonsense is left out.
        assert!(
            from_sdp("a=x-queqiao-nat:1.2.3.4:5 linear:999 1.2.3.4:6\r\na=x-queqiao-nat:x y z\r\n")
                .is_empty()
        );
    }

    #[test]
    fn only_the_two_ends_make_and_check_packets() {
        let key = Key::from_sdps(
            "a=ice-pwd:offerpassword\r\n",
            "a=ice-pwd:answerpassword\r\n",
        )
        .unwrap();
        let other =
            Key::from_sdps("a=ice-pwd:offerpassword\r\n", "a=ice-pwd:somethingelse\r\n").unwrap();
        let request = key.encode(Packet::Request([7; 8]));
        assert_eq!(request.len(), PACKET);
        assert!(is_punch(&request));
        assert_eq!(key.decode(&request), Some(Packet::Request([7; 8])));
        assert_eq!(other.decode(&request), None);
        let mut changed = request.clone();
        changed[5] ^= 1;
        assert_eq!(key.decode(&changed), None);
        assert_eq!(
            key.decode(&key.encode(Packet::Answer([9; 8]))),
            Some(Packet::Answer([9; 8]))
        );
        assert!(Key::from_sdps("v=0\r\n", "a=ice-pwd:x\r\n").is_none());
    }

    #[test]
    fn each_pair_of_nats_gets_its_plan() {
        let mut counter = 1000u16;
        let mut random = move || {
            counter = counter.wrapping_add(7919);
            counter
        };
        let info = |base: &str, nat, public: &str| NatInfo {
            base: address(base),
            nat,
            public: address(public),
        };
        let pc = info("59.66.29.128:52960", Nat::Open, "59.66.29.128:52960");
        let sunshine = info(
            "33.252.114.161:62664",
            Nat::Linear(1),
            "122.13.163.201:57659",
        );
        // The PC (no NAT, Windows' firewall) for sunshine: its own address, then the 50 ports sunshine's carrier hands
        // out next, then a sweep of every 128th port: any block of 128 ports in a row is met.
        let plans = plan(&pc, &sunshine, true, &mut random);
        assert_eq!(plans[0].targets, vec![address("122.13.163.201:57659")]);
        assert_eq!(plans[1].targets.len(), 50);
        assert_eq!(plans[1].targets[0], address("122.13.163.201:57660"));
        assert_eq!(plans[1].targets[49], address("122.13.163.201:57709"));
        let swept: Vec<u16> = plans[2].targets.iter().map(SocketAddr::port).collect();
        assert!((503..=504).contains(&swept.len()));
        assert!(swept[0] >= 1024 && swept[0] < 1024 + 128);
        assert!(swept.windows(2).all(|pair| pair[1] - pair[0] == 128));
        assert!(*swept.last().unwrap() > u16::MAX - 128);
        let block = 36_800..36_928;
        assert_eq!(swept.iter().filter(|port| block.contains(*port)).count(), 1);
        assert_eq!(plans[2].start, FALLBACK + SWEEP_AFTER);
        // sunshine for the PC: to the PC first, many sockets only after FALLBACK.
        let plans = plan(&sunshine, &pc, false, &mut random);
        assert_eq!(plans[0].targets, vec![pc.public]);
        assert_eq!((plans[1].sockets, plans[1].start), (150, FALLBACK));
        // Both linear: 30 sockets each to the 30th port after the other's latest.
        let other = info("10.0.0.2:4000", Nat::Linear(2), "5.6.7.8:20000");
        let plans = plan(&sunshine, &other, true, &mut random);
        assert_eq!(plans[1].sockets, 30);
        assert_eq!(plans[1].targets, vec![address("5.6.7.8:20060")]);
        // Random behind one mapping: every 32nd port; the random side: 150 sockets.
        let random_nat = info("10.0.0.3:4000", Nat::Random, "9.9.9.9:30000");
        let plans = plan(&pc, &random_nat, true, &mut random);
        assert!((2015..=2016).contains(&plans[1].targets.len()));
        assert!(
            plans[1]
                .targets
                .iter()
                .all(|target| target.ip() == random_nat.public.ip())
        );
        let plans = plan(&random_nat, &pc, false, &mut random);
        assert_eq!(plans[1].sockets, 150);
        // Both one mapping: turns taken, the controlling side sweeping first, each sweep a second after the other side's sockets.
        let cone = info("192.168.1.2:4000", Nat::Cone, "8.8.4.4:4000");
        let plans = plan(&pc, &cone, true, &mut random);
        assert_eq!(plans[1].start, Duration::from_secs(10) + SWEEP_AFTER);
        assert_eq!(plans[2].start, Duration::from_secs(20));
        let plans = plan(&cone, &pc, false, &mut random);
        assert_eq!(plans[1].start, Duration::from_secs(20) + SWEEP_AFTER);
        assert_eq!(plans[2].start, Duration::from_secs(10));
    }

    #[test]
    fn the_servers_round_limit_ends_punching() {
        let key = Key::from_sdps("a=ice-pwd:a\r\n", "a=ice-pwd:b\r\n").unwrap();
        let local = NatInfo {
            base: address("192.0.2.1:4000"),
            nat: Nat::Open,
            public: address("192.0.2.1:4000"),
        };
        let remote = NatInfo {
            base: address("192.0.2.2:4000"),
            nat: Nat::Cone,
            public: address("198.51.100.2:5000"),
        };
        let started = Instant::now();
        let mut puncher =
            Puncher::rounds(key, vec![(local, remote)], true, started).limit_rounds(1);
        let mut out = Vec::new();
        assert!(puncher.poll(started, &mut out).is_some());
        // One round only: over after LIMIT, no next one.
        assert!(puncher.poll(started + LIMIT, &mut out).is_none());
        assert!(
            puncher.take_notes().join("\n").contains(
                "punching over after 1 rounds without a direct path (the server's limit)"
            )
        );
        assert_eq!(Nat::kind(Some(&remote)), "cone");
        assert_eq!(Nat::kind(None), "none");
    }

    #[test]
    fn rounds_go_on_until_the_session_is_direct() {
        let key = Key::from_sdps("a=ice-pwd:a\r\n", "a=ice-pwd:b\r\n").unwrap();
        // Addresses no socket here binds (TEST-NET): the many-sockets plans open none, the lists only fill `out`.
        let local = NatInfo {
            base: address("192.0.2.1:4000"),
            nat: Nat::Open,
            public: address("192.0.2.1:4000"),
        };
        let remote = NatInfo {
            base: address("192.0.2.2:4000"),
            nat: Nat::Cone,
            public: address("198.51.100.2:5000"),
        };
        let started = Instant::now();
        let mut puncher = Puncher::rounds(key, vec![(local, remote)], true, started);
        let mut out = Vec::new();
        assert!(puncher.poll(started, &mut out).is_some());
        assert!(!out.is_empty());
        // The first round ends after LIMIT; the next comes 30 s later, its roles swapped (the controlling side, which
        // swept first, now sends from many sockets first: its sweep from 20 s).
        let next = puncher.poll(started + LIMIT, &mut out).unwrap();
        assert_eq!(next, started + LIMIT + Duration::from_secs(30));
        assert!(puncher.poll(next - TICK, &mut out).is_some());
        puncher.poll(next, &mut out);
        let notes = puncher.take_notes().join("\n");
        assert!(notes.contains("round 1 over without a direct path, the next in 30 s"));
        assert!(notes.contains("round 2 (the roles swapped)"));
        let sweep = puncher
            .running
            .iter()
            .find(|running| running.plan.what.starts_with("to every 128th"))
            .unwrap();
        assert_eq!(sweep.plan.start, Duration::from_secs(20) + SWEEP_AFTER);
        // Direct: it waits; relayed again: a new round at once.
        puncher.pause();
        assert!(puncher.poll(next + TICK, &mut out).is_none());
        puncher.resume(next + Duration::from_secs(60));
        assert!(
            puncher
                .poll(next + Duration::from_secs(60), &mut out)
                .is_some()
        );
        let notes = puncher.take_notes().join("\n");
        assert!(notes.contains("punching waits") && notes.contains("round 3"));
        // After the first five rounds, every five minutes.
        assert_eq!(gap_after(3), Duration::from_secs(30));
        assert_eq!(gap_after(4), Duration::from_secs(270));
    }

    #[test]
    fn a_short_list_sends_ten_a_second() {
        let key = Key::from_sdps("a=ice-pwd:a\r\n", "a=ice-pwd:b\r\n").unwrap();
        let started = Instant::now();
        let list = |targets: Vec<SocketAddr>, per_tick| Plan {
            what: "test".into(),
            base: address("127.0.0.1:9"),
            sockets: 0,
            targets,
            per_tick,
            start: Duration::ZERO,
            until: LIMIT,
        };
        let many: Vec<SocketAddr> = (0..50)
            .map(|port| SocketAddr::new([127, 0, 0, 1].into(), 2000 + port))
            .collect();
        let mut puncher = Puncher::new(
            key,
            vec![list(vec![address("127.0.0.1:10")], 1), list(many, 1)],
            started,
        );
        let mut out = Vec::new();
        for tick in 0..100 {
            puncher.poll(started + TICK * tick, &mut out);
        }
        let to = |port: u16| {
            out.iter()
                .filter(|(_, target, _)| target.port() == port)
                .count()
        };
        // One address: a round every 100 ms. Fifty: one packet a tick, every address twice in the second.
        assert_eq!(to(10), 10);
        assert_eq!(out.len() - 10, 100);
        assert_eq!(to(2000), 2);
    }

    #[test]
    fn a_punch_on_a_new_socket_finds_the_path() {
        let key = Key::from_sdps("a=ice-pwd:a\r\n", "a=ice-pwd:b\r\n").unwrap();
        let peer = UdpSocket::bind("127.0.0.1:0").unwrap();
        let peer_address = peer.local_addr().unwrap();
        let plans = vec![Plan {
            what: "test".into(),
            base: address("127.0.0.1:9"),
            sockets: 3,
            targets: vec![peer_address],
            per_tick: 3,
            start: Duration::ZERO,
            until: LIMIT,
        }];
        let started = Instant::now();
        let mut puncher = Puncher::new(key.clone(), plans, started);
        let mut out = Vec::new();
        puncher.poll(started, &mut out);
        // The peer gets three requests (one per socket) and answers one of them.
        peer.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let mut buffer = [0u8; 64];
        let (length, from) = peer.recv_from(&mut buffer).unwrap();
        let Some(Packet::Request(nonce)) = key.decode(&buffer[..length]) else {
            panic!("not a request");
        };
        peer.send_to(&key.encode(Packet::Answer(nonce)), from)
            .unwrap();
        let mut found = None;
        for _ in 0..200 {
            for event in puncher.receive() {
                if let Event::Found {
                    local,
                    remote,
                    socket,
                } = event
                {
                    found = Some((local, remote, socket.is_some()));
                }
            }
            if found.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(found, Some((from, peer_address, true)));
        // On the session's own socket: a request is answered and names the path once.
        let request = key.encode(Packet::Request([1; 8]));
        let (answer, new) = puncher.on_packet(address("127.0.0.1:9"), peer_address, &request);
        assert!(answer.is_some() && new);
        let (_, again) = puncher.on_packet(address("127.0.0.1:9"), peer_address, &request);
        assert!(!again);
    }
}
