//! How far each relay is from this device: a few STUN Binding requests to every relay the server listed in its
//! welcome (the user, 2026-11-26: the relayed traffic spread over the servers, each session on the relay nearest to
//! both of its ends). They leave from the address the account connection uses (the network card, past a proxy's TUN
//! adapter, which would add its own detour), in the background once after each login, and the server is told the
//! shortest round trip to each relay. A relay that does not answer is left out of the report.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use queqiao_core::account::{ClientMessage, RelayRtt};
use queqiao_core::stun::{self, Class, Message, method};

/// Requests to each relay, this far apart; the shortest round trip counts (one late answer does not).
const TRIES: u32 = 3;
const SPACING: Duration = Duration::from_millis(150);
/// How long the answers are waited for in all.
const WAIT: Duration = Duration::from_millis(2000);
/// At most this many relays are measured (a server's list is short; a longer one is not followed).
const MAX_RELAYS: usize = 16;

/// Measures `relays` from `local_ip` on a thread of its own and sends the result through `commands`.
pub(crate) fn spawn(local_ip: IpAddr, relays: Vec<String>, commands: Sender<ClientMessage>) {
    let _ = std::thread::Builder::new()
        .name("queqiao-relay-rtt".into())
        .spawn(move || {
            let rtts = measure(local_ip, &relays);
            if !rtts.is_empty() {
                let _ = commands.send(ClientMessage::RelayRtts { rtts });
            }
        });
}

/// The shortest round trip to each relay that answered, in the server's words for it.
pub(crate) fn measure(local_ip: IpAddr, relays: &[String]) -> Vec<RelayRtt> {
    let targets: Vec<(String, SocketAddr)> = relays
        .iter()
        .take(MAX_RELAYS)
        .filter_map(|relay| {
            let address = relay.parse::<SocketAddr>().ok().or_else(|| {
                queqiao_transport::resolve_server(relay)
                    .ok()?
                    .into_iter()
                    .find(|address| address.is_ipv4() == local_ip.is_ipv4())
            })?;
            (address.is_ipv4() == local_ip.is_ipv4()).then(|| (relay.clone(), address))
        })
        .collect();
    if targets.is_empty() {
        return Vec::new();
    }
    let Ok(socket) = UdpSocket::bind(SocketAddr::new(local_ip, 0)) else {
        return Vec::new();
    };
    let start = Instant::now();
    let mut sent: HashMap<[u8; 12], (usize, Instant)> = HashMap::new();
    let mut best: Vec<Option<Duration>> = vec![None; targets.len()];
    let mut tries = 0;
    let mut next_send = start;
    let mut buffer = [0u8; 1500];
    loop {
        let now = Instant::now();
        if tries < TRIES && now >= next_send {
            for (index, (_, address)) in targets.iter().enumerate() {
                let mut id = [0u8; 12];
                if getrandom::fill(&mut id).is_err() {
                    return Vec::new();
                }
                let request = Message::new(Class::Request, method::BINDING, id).encode(None, true);
                if socket.send_to(&request, address).is_ok() {
                    sent.insert(id, (index, Instant::now()));
                }
            }
            tries += 1;
            next_send = now + SPACING;
        }
        let end = start + WAIT;
        if now >= end || (tries == TRIES && sent.is_empty()) {
            break;
        }
        let until = if tries < TRIES {
            next_send.min(end)
        } else {
            end
        };
        let wait = until
            .saturating_duration_since(now)
            .max(Duration::from_millis(1));
        if socket.set_read_timeout(Some(wait)).is_err() {
            break;
        }
        let Ok((length, from)) = socket.recv_from(&mut buffer) else {
            continue;
        };
        let packet = &buffer[..length];
        if !stun::is_stun(packet) {
            continue;
        }
        let Some(message) = Message::decode(packet) else {
            continue;
        };
        if message.class != Class::Success {
            continue;
        }
        // Only the answer to a request of ours, from where it went.
        if let Some((index, at)) = sent.get(&message.transaction).copied()
            && targets[index].1 == from
        {
            sent.remove(&message.transaction);
            let rtt = at.elapsed();
            let slot = &mut best[index];
            if slot.is_none_or(|old| rtt < old) {
                *slot = Some(rtt);
            }
        }
    }
    targets
        .into_iter()
        .zip(best)
        .filter_map(|((relay, _), rtt)| {
            let ms = u32::try_from(rtt?.as_millis()).unwrap_or(u32::MAX).max(1);
            Some(RelayRtt { relay, ms })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_relay_that_answers_is_measured_and_one_that_does_not_is_left_out() {
        // A STUN server on this machine's loopback that answers every Binding request.
        let server = UdpSocket::bind("127.0.0.1:0").unwrap();
        let address = server.local_addr().unwrap();
        std::thread::spawn(move || {
            let mut buffer = [0u8; 1500];
            while let Ok((length, from)) = server.recv_from(&mut buffer) {
                if let Some(request) = Message::decode(&buffer[..length]) {
                    let reply = request.reply(Class::Success).encode(None, true);
                    let _ = server.send_to(&reply, from);
                }
            }
        });
        // A port nobody listens on.
        let silent = UdpSocket::bind("127.0.0.1:0").unwrap();
        let silent_address = silent.local_addr().unwrap();
        let rtts = measure(
            "127.0.0.1".parse().unwrap(),
            &[address.to_string(), silent_address.to_string()],
        );
        assert_eq!(rtts.len(), 1, "{rtts:?}");
        assert_eq!(rtts[0].relay, address.to_string());
        assert!(rtts[0].ms >= 1 && rtts[0].ms < 1000, "{rtts:?}");
    }
}
