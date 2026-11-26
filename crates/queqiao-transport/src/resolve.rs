//! The account server's addresses from its name ("host:port"). The system's resolver first. When it gives no address
//! on the internet for a name, the name is asked of public resolvers straight out of this PC's network cards, as the
//! server connection itself goes ([`crate::connect_direct`]): a proxy's "fake IP" DNS (Clash/Mihomo answer 198.18.x.x
//! and then carry the connection through their tunnel, abroad if their rules say so: the user, 2026-11-25: "each
//! computer connects to the server directly") would otherwise put the server connection through the proxy whenever
//! the server is given by name (a name built in: the security review of 2026-11-26).
//!
//! A forged answer can only make the connection fail: the server's certificate is pinned, so no other server can
//! take its place.

use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, ToSocketAddrs, UdpSocket};
use std::time::{Duration, Instant};

use crate::interfaces::{public_address, uplinks};

/// Public resolvers, asked in turn: AliDNS, DNSPod, 114DNS (reachable in China), Cloudflare, Google.
const RESOLVERS: [Ipv4Addr; 5] = [
    Ipv4Addr::new(223, 5, 5, 5),
    Ipv4Addr::new(119, 29, 29, 29),
    Ipv4Addr::new(114, 114, 114, 114),
    Ipv4Addr::new(1, 1, 1, 1),
    Ipv4Addr::new(8, 8, 8, 8),
];
/// Each resolver's time to answer, and all of them together.
const ASK_WAIT: Duration = Duration::from_millis(1500);
const ALL_WAIT: Duration = Duration::from_secs(6);

/// "host:port" (or "[v6]:port") taken apart.
fn split(server: &str) -> io::Result<(&str, u16)> {
    let invalid = || {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{server} is not host:port"),
        )
    };
    let (host, port) = server.rsplit_once(':').ok_or_else(invalid)?;
    let port = port.parse::<u16>().map_err(|_| invalid())?;
    let host = host.trim_start_matches('[').trim_end_matches(']');
    if host.is_empty() {
        return Err(invalid());
    }
    Ok((host, port))
}

/// The server's addresses, in the order to try them: an address on the internet first.
pub fn resolve_server(server: &str) -> io::Result<Vec<SocketAddr>> {
    let (host, port) = split(server)?;
    if let Ok(ip) = host.parse::<IpAddr>() {
        return Ok(vec![SocketAddr::new(ip, port)]);
    }
    let system: Vec<SocketAddr> = server
        .to_socket_addrs()
        .map(Iterator::collect)
        .unwrap_or_default();
    let (mut public, other): (Vec<SocketAddr>, Vec<SocketAddr>) = system
        .into_iter()
        .partition(|address| public_address(address.ip()));
    // Asked again only when the system gave nothing, or a proxy's made-up address: a name it gives this PC's own or
    // a LAN's address for (localhost, a server at home) is taken as it is, at once.
    if public.is_empty() && (other.is_empty() || other.iter().any(|a| made_up(a.ip()))) {
        public = ask_public_resolvers(host)
            .into_iter()
            .map(|ip| SocketAddr::new(IpAddr::V4(ip), port))
            .collect();
    }
    // A name only a LAN knows (a server there) keeps its own addresses, after any public one.
    public.extend(other);
    if public.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("no address for {server}"),
        ));
    }
    Ok(public)
}

/// A proxy's made-up address: 198.18.0.0/15 (Clash/Mihomo's fake IP range).
fn made_up(ip: IpAddr) -> bool {
    matches!(ip, IpAddr::V4(ip) if ip.octets()[0] == 198 && (18..20).contains(&ip.octets()[1]))
}

/// What a resolver said.
#[derive(Debug, PartialEq)]
enum Reply {
    Addresses(Vec<Ipv4Addr>),
    /// The name does not exist (NXDOMAIN): as final as an answer.
    NoSuchName,
    /// It could not answer (SERVFAIL, REFUSED, ...): the next one is asked.
    Failed,
}

/// The name's IPv4 addresses on the internet from the public resolvers, asked out of each uplink in turn: the first
/// one that answers decides; one that fails or keeps quiet passes the question on.
fn ask_public_resolvers(name: &str) -> Vec<Ipv4Addr> {
    let Some(query) = query(random_id(), name) else {
        return Vec::new();
    };
    let until = Instant::now() + ALL_WAIT;
    for source in uplinks() {
        for resolver in RESOLVERS {
            if Instant::now() >= until {
                return Vec::new();
            }
            match ask(source, resolver, &query) {
                Ok(Reply::Addresses(found)) => {
                    return found
                        .into_iter()
                        .filter(|ip| public_address(IpAddr::V4(*ip)))
                        .collect();
                }
                Ok(Reply::NoSuchName) => return Vec::new(),
                Ok(Reply::Failed) | Err(_) => {}
            }
        }
    }
    Vec::new()
}

fn random_id() -> u16 {
    let mut bytes = [0u8; 2];
    let _ = getrandom::fill(&mut bytes);
    u16::from_be_bytes(bytes)
}

/// One question to one resolver from `source` (a socket bound there leaves through that card); its reply.
fn ask(source: Ipv4Addr, resolver: Ipv4Addr, query: &[u8]) -> io::Result<Reply> {
    let socket = UdpSocket::bind((source, 0))?;
    // Connected: answers from anyone else are not taken.
    socket.connect((resolver, 53))?;
    socket.send(query)?;
    let id = u16::from_be_bytes([query[0], query[1]]);
    let until = Instant::now() + ASK_WAIT;
    let mut buffer = [0u8; 1500];
    loop {
        let left = until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "no answer"));
        }
        socket.set_read_timeout(Some(left))?;
        let length = socket.recv(&mut buffer)?;
        if let Some(found) = answer(&buffer[..length], id) {
            return Ok(found);
        }
    }
}

/// A DNS question for `name`'s A records (recursion asked for); None for a name that is not one.
fn query(id: u16, name: &str) -> Option<Vec<u8>> {
    let name = name.trim_end_matches('.');
    if name.is_empty() || name.len() > 253 {
        return None;
    }
    let mut out = Vec::with_capacity(18 + name.len());
    out.extend_from_slice(&id.to_be_bytes());
    out.extend_from_slice(&[0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0]);
    for label in name.split('.') {
        let ok = !label.is_empty()
            && label.len() <= 63
            && label
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
        if !ok {
            return None;
        }
        out.push(label.len() as u8);
        out.extend_from_slice(label.as_bytes());
    }
    out.extend_from_slice(&[0, 0, 1, 0, 1]);
    Some(out)
}

/// Past a name in a message at `at` (labels, ending in a zero or a pointer).
fn skip_name(message: &[u8], mut at: usize) -> Option<usize> {
    for _ in 0..128 {
        let length = *message.get(at)?;
        match length {
            0 => return Some(at + 1),
            l if l & 0xC0 == 0xC0 => {
                message.get(at + 1)?;
                return Some(at + 2);
            }
            l if l & 0xC0 == 0 => at += 1 + usize::from(l),
            _ => return None,
        }
    }
    None
}

/// The reply to question `id` (its A records, or that the name does not exist, or that it failed); None for anything
/// else (another question's, not an answer, malformed): waited past.
fn answer(message: &[u8], id: u16) -> Option<Reply> {
    let word = |at: usize| {
        Some(u16::from_be_bytes([
            *message.get(at)?,
            *message.get(at + 1)?,
        ]))
    };
    if word(0)? != id {
        return None;
    }
    let flags = word(2)?;
    // An answer (QR); its RCODE: 0 fine, 3 no such name, anything else a failure.
    if flags & 0x8000 == 0 {
        return None;
    }
    match flags & 0x000F {
        0 => {}
        3 => return Some(Reply::NoSuchName),
        _ => return Some(Reply::Failed),
    }
    let (questions, answers) = (word(4)?, word(6)?);
    let mut at = 12;
    for _ in 0..questions {
        at = skip_name(message, at)? + 4;
    }
    let mut found = Vec::new();
    for _ in 0..answers {
        at = skip_name(message, at)?;
        let (kind, class, length) = (word(at)?, word(at + 2)?, usize::from(word(at + 8)?));
        let data = message.get(at + 10..at + 10 + length)?;
        if kind == 1 && class == 1 && length == 4 {
            found.push(Ipv4Addr::new(data[0], data[1], data[2], data[3]));
        }
        at += 10 + length;
    }
    Some(Reply::Addresses(found))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_given_as_such_are_taken_as_they_are() {
        assert_eq!(
            resolve_server("101.201.221.56:47100").unwrap(),
            vec!["101.201.221.56:47100".parse().unwrap()]
        );
        assert_eq!(
            resolve_server("[2402:f000::1]:47100").unwrap(),
            vec!["[2402:f000::1]:47100".parse().unwrap()]
        );
        assert!(resolve_server("no-port").is_err());
        assert!(resolve_server(":47100").is_err());
    }

    #[test]
    fn a_question_is_written_and_its_answer_read() {
        let asked = query(0x1234, "server.queqiaoremote.com").unwrap();
        assert_eq!(&asked[..12], &[0x12, 0x34, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0]);
        assert_eq!(&asked[12..19], b"\x06server");
        assert_eq!(&asked[asked.len() - 5..], &[0, 0, 1, 0, 1]);
        assert!(query(1, "bad name.com").is_none());
        assert!(query(1, "").is_none());
        // The answer: the question again, then a CNAME and an A record (names by pointer to the question's).
        let mut reply = asked.clone();
        reply[2] = 0x81;
        reply[3] = 0x80;
        reply[7] = 2;
        reply.extend_from_slice(&[0xC0, 12, 0, 5, 0, 1, 0, 0, 0, 60, 0, 2, 0xC0, 12]);
        reply.extend_from_slice(&[0xC0, 12, 0, 1, 0, 1, 0, 0, 0, 60, 0, 4, 101, 201, 221, 56]);
        assert_eq!(
            answer(&reply, 0x1234),
            Some(Reply::Addresses(vec![Ipv4Addr::new(101, 201, 221, 56)]))
        );
        // No such name (final), a failure (the next resolver is asked).
        let mut missing = reply.clone();
        missing[3] = 0x83;
        assert_eq!(answer(&missing, 0x1234), Some(Reply::NoSuchName));
        let mut failed = reply.clone();
        failed[3] = 0x82;
        assert_eq!(answer(&failed, 0x1234), Some(Reply::Failed));
        // Another question's answer, a question (not an answer), a cut message: waited past.
        assert_eq!(answer(&reply, 0x4321), None);
        assert_eq!(answer(&asked, 0x1234), None);
        assert_eq!(answer(&reply[..reply.len() - 2], 0x1234), None);
        assert_eq!(answer(&[], 0x1234), None);
        // Clash/Mihomo's made-up range only.
        assert!(made_up("198.19.0.83".parse().unwrap()));
        assert!(made_up("198.18.0.1".parse().unwrap()));
        assert!(!made_up("198.20.0.1".parse().unwrap()));
        assert!(!made_up("192.168.1.10".parse().unwrap()));
        // A name running past the end of the message (pointers are never followed, so none can loop): not taken.
        let mut looped = vec![0x12, 0x34, 0x81, 0x80, 0, 1, 0, 0, 0, 0, 0, 0];
        looped.extend_from_slice(&[0x3F; 200]);
        assert_eq!(answer(&looped, 0x1234), None);
    }
}
