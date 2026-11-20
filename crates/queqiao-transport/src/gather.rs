//! ICE candidates for a session, gathered before the offer or answer is made (not trickled): the host
//! address, the public address the STUN/TURN server sees (server-reflexive), and a relayed address on the
//! TURN server. All on the session's one UDP socket, so ICE checks leave from the address the candidates
//! describe. Gathering stops after 1.5 s at most; whatever did not answer by then is left out.
//!
//! `QUEQIAO_FORCE=network:relay` offers the relayed address only, so the session must go through TURN.

use std::io::ErrorKind;
use std::net::{SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

use queqiao_core::stun::{self, Class, Message, attr, method};
use str0m::Candidate;

use crate::turn::{IceServers, TurnClient, transaction};
use crate::{Error, err};

const GATHER_LIMIT: Duration = Duration::from_millis(1500);
const BINDING_RTO: Duration = Duration::from_millis(250);

pub(crate) struct Gathered {
    pub candidates: Vec<Candidate>,
    pub turn: Option<TurnClient>,
    /// One line per candidate (and per failure), for the programs' reports.
    pub notes: Vec<String>,
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
    let mut turn = servers
        .turn
        .as_ref()
        .map(|server| TurnClient::new(server, started));
    // A plain STUN binding only without TURN (an allocation reports the public address too).
    let mut binding = match (&turn, servers.stun, relay_only) {
        (None, Some(server), false) => Some(Binding::new(server)),
        _ => None,
    };
    let mut buffer = vec![0u8; 2048];
    loop {
        let now = Instant::now();
        if let Some(client) = turn.as_mut() {
            client.poll(now);
            for datagram in client.take_outbox() {
                let _ = socket.send_to(&datagram, client.server);
            }
        }
        if let Some(binding) = binding.as_mut() {
            binding.send_if_due(socket, now);
        }
        let turn_done = turn
            .as_ref()
            .is_none_or(|client| client.relayed.is_some() || client.failure.is_some());
        let binding_done = binding
            .as_ref()
            .is_none_or(|binding| binding.mapped.is_some());
        if (turn_done && binding_done) || now >= deadline {
            break;
        }
        let next = [
            Some(deadline),
            turn.as_ref().and_then(TurnClient::next_deadline),
            binding.as_ref().map(|binding| binding.next_send),
        ]
        .into_iter()
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
                if let Some(client) = turn.as_mut()
                    && from == client.server
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
    })
}

/// One STUN Binding request, retransmitted until answered.
struct Binding {
    server: SocketAddr,
    transaction: [u8; 12],
    next_send: Instant,
    tries: u32,
    mapped: Option<SocketAddr>,
}

impl Binding {
    fn new(server: SocketAddr) -> Binding {
        Binding {
            server,
            transaction: transaction(),
            next_send: Instant::now(),
            tries: 0,
            mapped: None,
        }
    }

    fn send_if_due(&mut self, socket: &UdpSocket, now: Instant) {
        if self.mapped.is_some() || now < self.next_send || self.tries >= 5 {
            return;
        }
        let request =
            Message::new(Class::Request, method::BINDING, self.transaction).encode(None, true);
        let _ = socket.send_to(&request, self.server);
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
