//! ICE candidates for a session, gathered before the offer or answer is made (not trickled): the host
//! address, the public address the STUN/TURN server sees (server-reflexive), and a relayed address on the
//! TURN server. All on the session's one UDP socket, so ICE checks leave from the address the candidates
//! describe. Gathering stops after 1.5 s at most; whatever did not answer by then is left out.
//!
//! `QUEQIAO_FORCE=network:relay` offers the relayed address only, so the session must go through TURN.
//!
//! With several relays (the server lists backups), the first is asked; when it refuses, or has not answered after
//! RELAY_FALLBACK, the next one is asked too, and the first in the server's order that gives an address is used (any
//! other that does is given back).
//!
//! What the socket's NAT does (`punch::classify`) comes from the public addresses that up to three servers at different
//! addresses see: the relay asked for an allocation, and a STUN Binding to each of the next relays (`nat_servers`), all
//! sent at once in that order. They wait for each other at most `NAT_GRACE` once the candidates are gathered.

use std::collections::VecDeque;
use std::io::ErrorKind;
use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use queqiao_core::stun::{self, Class, Message, attr, method};
use str0m::Candidate;

use crate::punch::{self, NatInfo};
use crate::turn::{IceServers, TurnClient, transaction};
use crate::{Error, err};

pub(crate) const GATHER_LIMIT: Duration = Duration::from_millis(1500);
const BINDING_RTO: Duration = Duration::from_millis(250);
/// How long the NAT's other answers may take once the candidates are there (they are sent at the same time).
const NAT_GRACE: Duration = Duration::from_millis(200);
/// Between the NAT's probes (the relay's allocation first): the order they leave in is the order the NAT numbers them.
const PROBE_GAP: Duration = Duration::from_millis(25);

/// The servers a socket asks to learn what its NAT does: the relays and the STUN server, each address once, three at
/// most, in the server's order.
pub(crate) fn nat_servers(servers: &IceServers) -> Vec<SocketAddr> {
    let mut out: Vec<SocketAddr> = Vec::new();
    let addresses = servers
        .turn
        .iter()
        .chain(&servers.backups)
        .map(|server| server.address)
        .chain(servers.stun);
    for address in addresses {
        if out.len() == 3 {
            break;
        }
        if address.is_ipv4() && !out.iter().any(|other| other.ip() == address.ip()) {
            out.push(address);
        }
    }
    out
}
/// How long a relay may take to give an address before the next one is asked as well: a relay answers within one
/// round trip (under 300 ms from anywhere it is offered), and this leaves the next one most of the gathering time.
const RELAY_FALLBACK: Duration = Duration::from_millis(600);

pub(crate) struct Gathered {
    pub candidates: Vec<Candidate>,
    pub turn: Option<TurnClient>,
    /// One line per candidate (and per failure), for the programs' reports.
    pub notes: Vec<String>,
    /// What the socket's NAT does (none without an answer, or when only the relay is offered).
    pub nat: Option<NatInfo>,
}

pub(crate) fn gather(
    socket: &UdpSocket,
    host: SocketAddr,
    servers: &IceServers,
    relay_only: bool,
) -> Result<Gathered, Error> {
    let started = Instant::now();
    let deadline = started + GATHER_LIMIT;
    let mut notes = Vec::new();
    // The relays in the server's order; each asked when the one before refused or was slow (RELAY_FALLBACK).
    let mut queue: VecDeque<_> = servers.turn.iter().chain(&servers.backups).collect();
    let mut turns: Vec<(TurnClient, Instant)> = Vec::new();
    if let Some(first) = queue.pop_front() {
        turns.push((TurnClient::new(first, started), started));
    }
    // A plain STUN binding only without TURN (an allocation reports the public address too).
    let mut binding = match (turns.is_empty(), servers.stun, relay_only) {
        (true, Some(server), false) => Some(Binding::new(server)),
        _ => None,
    };
    // A Binding to each of the other servers for the NAT (not the one already asked above).
    let asked: Vec<SocketAddr> = turns
        .iter()
        .map(|(client, _)| client.server)
        .chain(binding.as_ref().map(|binding| binding.server))
        .collect();
    let mut probes: Vec<Binding> = if relay_only || !host.is_ipv4() {
        Vec::new()
    } else {
        nat_servers(servers)
            .into_iter()
            .filter(|server| !asked.iter().any(|other| other.ip() == server.ip()))
            .map(Binding::new)
            .collect()
    };
    // One after the other, PROBE_GAP apart: sent in one burst, a NAT does not always number them in the order sent.
    for (index, probe) in probes.iter_mut().enumerate() {
        probe.next_send = started + PROBE_GAP * (index as u32 + 1);
    }
    let mut rest_done_at: Option<Instant> = None;
    let mut buffer = vec![0u8; 2048];
    loop {
        let now = Instant::now();
        // The latest relay asked refused, or is slow: the next one too.
        if let Some((latest, asked)) = turns.last()
            && latest.relayed.is_none()
            && (latest.failure.is_some() || now >= *asked + RELAY_FALLBACK)
            && let Some(next) = queue.pop_front()
        {
            notes.push(format!(
                "relay {} {}: asking {} too",
                latest.server,
                latest.failure.clone().unwrap_or_else(|| format!(
                    "gave no address in {} ms",
                    RELAY_FALLBACK.as_millis()
                )),
                next.address
            ));
            turns.push((TurnClient::new(next, now), now));
        }
        for (client, _) in &mut turns {
            client.poll(now);
            for datagram in client.take_outbox() {
                let _ = socket.send_to(&datagram, client.server);
            }
        }
        if let Some(binding) = binding.as_mut() {
            binding.send_if_due(socket, now);
        }
        for probe in &mut probes {
            probe.send_if_due(socket, now);
        }
        let turn_done = turns.iter().any(|(client, _)| client.relayed.is_some())
            || (queue.is_empty() && turns.iter().all(|(client, _)| client.failure.is_some()));
        let binding_done = binding
            .as_ref()
            .is_none_or(|binding| binding.mapped.is_some());
        if turn_done && binding_done && rest_done_at.is_none() {
            rest_done_at = Some(now);
        }
        let probes_done = probes.iter().all(|probe| probe.mapped.is_some())
            || rest_done_at.is_some_and(|at| now >= at + NAT_GRACE);
        if (turn_done && binding_done && probes_done) || now >= deadline {
            break;
        }
        let fallback = turns
            .last()
            .filter(|(latest, _)| latest.relayed.is_none() && !queue.is_empty())
            .map(|(_, asked)| *asked + RELAY_FALLBACK);
        let next = [
            Some(deadline),
            fallback,
            binding.as_ref().map(|binding| binding.next_send),
            rest_done_at.map(|at| at + NAT_GRACE),
        ]
        .into_iter()
        .chain(turns.iter().map(|(client, _)| client.next_deadline()))
        .chain(
            probes
                .iter()
                .filter(|probe| probe.mapped.is_none() && probe.tries < 5)
                .map(|probe| Some(probe.next_send)),
        )
        .flatten()
        .min()
        .unwrap_or(deadline);
        socket.set_read_timeout(Some(
            next.saturating_duration_since(now)
                .max(Duration::from_millis(1)),
        ))?;
        match socket.recv_from(&mut buffer) {
            Ok((length, from)) => {
                let packet = &buffer[..length];
                if let Some(binding) = binding.as_mut()
                    && binding.accept(from, packet)
                {
                    continue;
                }
                if probes.iter_mut().any(|probe| probe.accept(from, packet)) {
                    continue;
                }
                if let Some((client, _)) =
                    turns.iter_mut().find(|(client, _)| from == client.server)
                {
                    let _ = client.handle(packet, Instant::now());
                }
            }
            Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
            // An ICMP "port unreachable" from a server that is not running.
            Err(error) if error.kind() == ErrorKind::ConnectionReset => {}
            Err(error) => return Err(error.into()),
        }
    }

    // The public addresses seen, in the order the first packet to each server left (the order the NAT gave them out):
    // the relays asked for an allocation when they were asked, the Bindings with their first send.
    let mut seen: Vec<(Instant, usize, SocketAddr)> = turns
        .iter()
        .enumerate()
        .filter_map(|(index, (client, asked))| client.mapped.map(|mapped| (*asked, index, mapped)))
        .collect();
    seen.extend(
        binding
            .iter()
            .chain(&probes)
            .enumerate()
            .filter_map(|(index, binding)| {
                Some((binding.first_sent?, turns.len() + index, binding.mapped?))
            }),
    );
    seen.sort_by_key(|(at, index, _)| (*at, *index));
    let in_order: Vec<SocketAddr> = seen.into_iter().map(|(_, _, mapped)| mapped).collect();
    let nat = if relay_only || !host.is_ipv4() {
        None
    } else {
        punch::classify(host, &in_order)
    };
    if let Some(info) = nat {
        notes.push(format!(
            "NAT of {host}: {} ({} answer{}{})",
            info.nat.describe(),
            in_order.len(),
            if in_order.len() == 1 { "" } else { "s" },
            if in_order.len() > 1 {
                format!(
                    ": ports {}",
                    in_order
                        .iter()
                        .map(|address| address.port().to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            } else {
                String::new()
            }
        ));
    }

    // The first relay in the server's order that gave an address; any other that did gets it back now.
    let chosen = turns
        .iter()
        .position(|(client, _)| client.relayed.is_some());
    let mut turn = None;
    let mut failed = None;
    for (index, (mut client, _)) in turns.into_iter().enumerate() {
        if Some(index) == chosen {
            turn = Some(client);
        } else if client.relayed.is_some() {
            client.close(Instant::now());
            for datagram in client.take_outbox() {
                let _ = socket.send_to(&datagram, client.server);
            }
        } else if failed.is_none() {
            // The first relay's reason, for the report when none gave an address.
            failed = Some(client);
        }
    }
    let turn = turn.or(failed);
    let mut candidates = Vec::new();
    if !relay_only {
        candidates.push(Candidate::host(host, "udp").map_err(err)?);
        notes.push(format!("candidate host {host}"));
        let mapped = binding
            .as_ref()
            .and_then(|binding| binding.mapped)
            .or(turn.as_ref().and_then(|client| client.mapped));
        if let Some(mapped) = mapped
            && mapped != host
            && mapped.is_ipv4() == host.is_ipv4()
        {
            candidates.push(Candidate::server_reflexive(mapped, host, "udp").map_err(err)?);
            notes.push(format!("candidate server-reflexive {mapped}"));
        }
        if binding
            .as_ref()
            .is_some_and(|binding| binding.mapped.is_none())
        {
            notes.push("no answer from the STUN server".into());
        }
    }
    let turn = match turn {
        Some(client) => match client.relayed {
            Some(relayed) => {
                candidates.push(Candidate::relayed(relayed, host, "udp").map_err(err)?);
                notes.push(format!(
                    "candidate relayed {relayed} (TURN server {})",
                    client.server
                ));
                Some(client)
            }
            None => {
                let reason = client.failure.clone().unwrap_or_else(|| {
                    format!(
                        "no answer from the TURN server at {} in time",
                        client.server
                    )
                });
                notes.push(format!("no relay: {reason}"));
                if relay_only {
                    return Err(Error(format!(
                        "the relay was forced (QUEQIAO_FORCE=network:relay) but {reason}"
                    )));
                }
                None
            }
        },
        None if relay_only => {
            return Err(Error(
                "the relay was forced (QUEQIAO_FORCE=network:relay) but there is no TURN server"
                    .into(),
            ));
        }
        None => None,
    };
    notes.push(format!("gathered in {} ms", started.elapsed().as_millis()));
    Ok(Gathered {
        candidates,
        turn,
        notes,
        nat,
    })
}

/// When the extra sockets stop waiting: the gathering limit, brought forward to shortly after the main socket is
/// done (an uplink that cannot reach the STUN server must not hold the session up).
pub(crate) struct ExtraDeadline {
    started: Instant,
    /// Milliseconds after `started`.
    millis: AtomicU64,
}

/// How long the extra sockets may go on once the main socket's gathering is done.
const EXTRA_GRACE: Duration = Duration::from_millis(300);

impl ExtraDeadline {
    pub(crate) fn new(started: Instant) -> ExtraDeadline {
        ExtraDeadline {
            started,
            millis: AtomicU64::new(GATHER_LIMIT.as_millis() as u64),
        }
    }

    pub(crate) fn main_done(&self) {
        let soon = (self.started.elapsed() + EXTRA_GRACE).as_millis() as u64;
        self.millis.fetch_min(soon, Ordering::Relaxed);
    }

    fn at(&self) -> Instant {
        self.started + Duration::from_millis(self.millis.load(Ordering::Relaxed))
    }
}

/// The host candidate of another uplink's socket, and its server-reflexive one if the first of `stun` answers there in
/// time (the relay stays on the session's main socket); what its NAT does from all of `stun` (`nat_servers`, asked at
/// once). Runs alongside `gather`.
pub(crate) fn gather_extra(
    socket: &UdpSocket,
    host: SocketAddr,
    stun: &[SocketAddr],
    deadline: &ExtraDeadline,
) -> (Vec<Candidate>, Vec<String>, Option<NatInfo>) {
    let mut candidates = Vec::new();
    let mut notes = Vec::new();
    match Candidate::host(host, "udp") {
        Ok(candidate) => {
            candidates.push(candidate);
            notes.push(format!("candidate host {host}"));
        }
        Err(error) => {
            notes.push(format!("no host candidate on {host}: {error}"));
            return (candidates, notes, None);
        }
    }
    // A server on this PC's loopback cannot be reached from another card's address; nor one of the other family (an
    // IPv6 socket has no NAT to learn about, and the servers are IPv4).
    let mut bindings: Vec<Binding> = stun
        .iter()
        .filter(|server| !server.ip().is_loopback() && server.is_ipv4() == host.is_ipv4())
        .map(|server| Binding::new(*server))
        .collect();
    if bindings.is_empty() {
        return (candidates, notes, None);
    }
    let mut first_answer: Option<Instant> = None;
    let mut buffer = vec![0u8; 2048];
    while bindings.iter().any(|binding| binding.mapped.is_none()) {
        let now = Instant::now();
        let until = deadline.at();
        let given_up = bindings.iter().all(|binding| {
            binding.mapped.is_some() || (binding.tries >= 5 && now >= binding.next_send)
        });
        // The first server's answer makes the candidate; the others (for the NAT) get NAT_GRACE more.
        let others_late =
            first_answer.is_some_and(|at| now >= at + NAT_GRACE) && bindings[0].mapped.is_some();
        if now >= until || given_up || others_late {
            break;
        }
        for binding in &mut bindings {
            binding.send_if_due(socket, now);
        }
        // Short waits, so a deadline brought forward is seen soon.
        let next = bindings
            .iter()
            .filter(|binding| binding.mapped.is_none())
            .map(|binding| binding.next_send)
            .min()
            .unwrap_or(until);
        let wait = next
            .min(until)
            .saturating_duration_since(now)
            .min(Duration::from_millis(50));
        if socket
            .set_read_timeout(Some(wait.max(Duration::from_millis(1))))
            .is_err()
        {
            break;
        }
        match socket.recv_from(&mut buffer) {
            Ok((length, from)) => {
                for binding in &mut bindings {
                    if binding.accept(from, &buffer[..length]) {
                        first_answer.get_or_insert_with(Instant::now);
                        break;
                    }
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::ConnectionReset
                ) => {}
            // Unreachable from this card (no route to the server there): host candidate only.
            Err(_) => break,
        }
    }
    match bindings[0].mapped {
        Some(mapped) if mapped != host && mapped.is_ipv4() == host.is_ipv4() => {
            if let Ok(candidate) = Candidate::server_reflexive(mapped, host, "udp") {
                candidates.push(candidate);
                notes.push(format!("candidate server-reflexive {mapped} (from {host})"));
            }
        }
        Some(_) => {}
        None => notes.push(format!("no answer from the STUN server on {host}")),
    }
    // In the order the first packet to each server left (all in the first pass, in this order).
    let in_order: Vec<SocketAddr> = bindings
        .iter()
        .filter_map(|binding| binding.mapped)
        .collect();
    let nat = punch::classify(host, &in_order);
    if let Some(info) = nat {
        notes.push(format!(
            "NAT of {host}: {} ({} answer{})",
            info.nat.describe(),
            in_order.len(),
            if in_order.len() == 1 { "" } else { "s" }
        ));
    }
    (candidates, notes, nat)
}

/// One STUN Binding request, retransmitted until answered.
struct Binding {
    server: SocketAddr,
    transaction: [u8; 12],
    next_send: Instant,
    tries: u32,
    mapped: Option<SocketAddr>,
    /// When the first request left (the NAT made the mapping then).
    first_sent: Option<Instant>,
}

impl Binding {
    fn new(server: SocketAddr) -> Binding {
        Binding {
            server,
            transaction: transaction(),
            next_send: Instant::now(),
            tries: 0,
            mapped: None,
            first_sent: None,
        }
    }

    fn send_if_due(&mut self, socket: &UdpSocket, now: Instant) {
        if self.mapped.is_some() || now < self.next_send || self.tries >= 5 {
            return;
        }
        let request =
            Message::new(Class::Request, method::BINDING, self.transaction).encode(None, true);
        let _ = socket.send_to(&request, self.server);
        self.first_sent.get_or_insert(now);
        self.tries += 1;
        self.next_send = now + BINDING_RTO * 2u32.pow(self.tries - 1);
    }

    /// True if `packet` was the answer to this request.
    fn accept(&mut self, from: SocketAddr, packet: &[u8]) -> bool {
        if from != self.server || !stun::is_stun(packet) {
            return false;
        }
        match Message::decode(packet) {
            Some(message) if message.transaction == self.transaction => {
                if message.class == Class::Success {
                    self.mapped = message.address(attr::XOR_MAPPED_ADDRESS);
                }
                true
            }
            _ => false,
        }
    }
}
