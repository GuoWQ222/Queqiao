//! Signaling for a direct connection (M2: by IP address). One TCP connection carries a hello (protocol
//! version and role) and then the SDP offer and answer. Messages are a 4-byte big-endian length followed
//! by "kind\nbody". queqiao-server takes over this job in M3.

use std::io::{ErrorKind, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

use crate::turn::IceServers;
use crate::{Error, err};

const MAX_MESSAGE: usize = 1 << 20;

pub struct Signal {
    stream: TcpStream,
}

impl Signal {
    /// Waits up to `timeout` for the other side to connect to `address` (e.g. "0.0.0.0:47110").
    pub fn listen(address: &str, timeout: Duration) -> Result<Signal, Error> {
        let listener = TcpListener::bind(address)?;
        listener.set_nonblocking(true)?;
        let deadline = Instant::now() + timeout;
        loop {
            match listener.accept() {
                Ok((stream, _)) => {
                    stream.set_nonblocking(false)?;
                    return Signal::new(stream);
                }
                Err(error) if error.kind() == ErrorKind::WouldBlock => {
                    if Instant::now() > deadline {
                        return Err(Error(format!("nobody connected to {address} in time")));
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(error) => return Err(error.into()),
            }
        }
    }

    /// Connects to `address`, retrying until `timeout` (the other side may still be starting).
    pub fn connect(address: &str, timeout: Duration) -> Result<Signal, Error> {
        let target: SocketAddr = address
            .to_socket_addrs()?
            .next()
            .ok_or_else(|| Error(format!("no address for {address}")))?;
        let deadline = Instant::now() + timeout;
        loop {
            match TcpStream::connect_timeout(&target, Duration::from_secs(3)) {
                Ok(stream) => return Signal::new(stream),
                Err(error) if Instant::now() > deadline => {
                    return Err(Error(format!("cannot connect to {address}: {error}")));
                }
                Err(_) => std::thread::sleep(Duration::from_millis(300)),
            }
        }
    }

    fn new(stream: TcpStream) -> Result<Signal, Error> {
        stream.set_nodelay(true)?;
        stream.set_read_timeout(Some(Duration::from_secs(30)))?;
        Ok(Signal { stream })
    }

    pub fn send(&mut self, kind: &str, body: &str) -> Result<(), Error> {
        let payload = format!("{kind}\n{body}");
        self.stream
            .write_all(&(payload.len() as u32).to_be_bytes())?;
        self.stream.write_all(payload.as_bytes())?;
        Ok(())
    }

    pub fn recv(&mut self) -> Result<(String, String), Error> {
        let mut length = [0u8; 4];
        self.stream.read_exact(&mut length)?;
        let length = u32::from_be_bytes(length) as usize;
        if length > MAX_MESSAGE {
            return Err(Error(format!(
                "signaling message of {length} bytes refused"
            )));
        }
        let mut payload = vec![0u8; length];
        self.stream.read_exact(&mut payload)?;
        let text = String::from_utf8(payload).map_err(err)?;
        let (kind, body) = text.split_once('\n').unwrap_or((&text, ""));
        Ok((kind.to_string(), body.to_string()))
    }

    /// The next message, which must be of `kind`.
    pub fn expect(&mut self, kind: &str) -> Result<String, Error> {
        let (got, body) = self.recv()?;
        if got == kind {
            Ok(body)
        } else {
            Err(Error(format!("expected {kind}, got {got}")))
        }
    }

    /// Our address on the connection: the one the other side can reach us at.
    pub fn local_ip(&self) -> Result<IpAddr, Error> {
        Ok(self.stream.local_addr()?.ip())
    }

    pub fn peer_address(&self) -> Result<SocketAddr, Error> {
        Ok(self.stream.peer_addr()?)
    }
}

/// Carries the offer and answer between the two sides of a session: a direct connection ([`Signal`]) or
/// queqiao-server (in queqiao-account).
pub trait Signaling {
    fn send(&mut self, kind: &str, body: &str) -> Result<(), Error>;
    /// The next message from the other side, which must be of `kind`.
    fn expect(&mut self, kind: &str) -> Result<String, Error>;
    /// Our address as the other side can reach it (for the ICE host candidate).
    fn local_ip(&self) -> Result<IpAddr, Error>;

    /// The STUN and TURN servers for this session (none for a direct connection).
    fn ice_servers(&mut self) -> Result<IceServers, Error> {
        Ok(IceServers::default())
    }

    /// Whether the other side may go on (asked during a session, once a second; false ends it). An account's
    /// session says false once the other device is taken out of the account, or this device's login ended.
    fn peer_trusted(&mut self) -> bool {
        true
    }

    /// During a session: a message of `kind` from the other side that came meanwhile (an ICE restart's
    /// "restart-request", "restart-offer" or "restart-answer"), checked as at the start; none here (a direct test
    /// connection carries no restarts).
    fn take(&mut self, _kind: &str) -> Option<String> {
        None
    }

    /// Whether the connection to the server is up now (during a session it may be lost and made again: an ICE
    /// restart waits for it).
    fn online(&mut self) -> bool {
        true
    }

    /// The viewer: how the session's path went, anonymous, to the server when it asked for such reports.
    fn report_path(&mut self, _report: &queqiao_core::account::PathReport) {}

    /// Exchanges hellos: both sides must speak the same protocol version and have different roles.
    /// Returns the other side's.
    fn hello(&mut self, role: &str) -> Result<Hello, Error> {
        self.hello_with(role, false)
    }

    /// As `hello`; a viewer's with `take_over` ends the session another device has with that PC (the person
    /// confirmed it). An agent busy with another device answers a hello without it with "busy:<that device>"
    /// ([`busy_controller`]).
    fn hello_with(&mut self, role: &str, take_over: bool) -> Result<Hello, Error> {
        let mut body = hello_body(role);
        if take_over {
            body.push_str(" takeover");
        }
        self.send("hello", &body)?;
        check_hello(&self.expect("hello")?, role)
    }

    /// During a session: what else came for this program (another device asking for a session, or a device of the
    /// account ending this one), if anything. None on a direct connection.
    fn interruption(&mut self) -> Option<Interruption> {
        None
    }

    /// Answers `request` (another session's) with `kind` and `body` (an "error" refusing it).
    fn answer_request(&mut self, _request: &SessionRequest, _kind: &str, _body: &str) {}

    /// Puts `request` back for the program's own loop, which takes it up once this session ended.
    fn requeue(&mut self, _request: SessionRequest) {}

    /// An agent's word to the account on who controls this device now (None: nobody), shown in its device list.
    fn set_controller(&mut self, _controller: Option<&str>) {}
}

/// Another device's request for a session, come in during one (`Signaling::interruption`): its hello, signature
/// checked already.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionRequest {
    pub from: String,
    pub from_name: String,
    /// The account of a device of another account (joined by a help code); None for the account's own.
    pub helper: Option<String>,
    pub session: String,
    pub body: String,
    pub signature: String,
}

/// What came for a program during a session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Interruption {
    Request(SessionRequest),
    /// A device of the account ended this session (`by`: its name).
    Ended {
        by: String,
    },
}

/// An agent's answer to a hello while another device controls it: "busy:" and that device's name.
pub const BUSY: &str = "busy:";

/// The device controlling the PC, when `error` is an agent's "busy" answer (empty name: someone helping).
pub fn busy_controller(error: &str) -> Option<&str> {
    error.strip_prefix(BUSY)
}

/// Our hello: "queqiao <protocol version> <role> <what we can do>". What we can do: "fec", we rebuild lost video
/// packets from parity (crate::fec). Earlier programs read the first three words only, so they ignore it. A viewer
/// taking control from another device adds "takeover" (`Signaling::hello_with`).
pub fn hello_body(role: &str) -> String {
    format!("queqiao {} {role} fec", queqiao_core::PROTOCOL_VERSION)
}

/// The other side's hello.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hello {
    pub role: String,
    /// It rebuilds lost video packets from parity: send it parity.
    pub fec: bool,
    /// A viewer taking control: the session another device has with this PC ends (the person confirmed it).
    pub take_over: bool,
}

/// Checks the other side's hello (same protocol version, the other role) and returns it.
pub fn check_hello(body: &str, role: &str) -> Result<Hello, Error> {
    let version = queqiao_core::PROTOCOL_VERSION;
    let mut parts = body.split_whitespace();
    let (name, their_version, their_role) = (parts.next(), parts.next(), parts.next());
    if name != Some("queqiao") || their_version != Some(&version.to_string()) {
        return Err(Error(format!(
            "the other side speaks \"{body}\", we speak protocol {version}"
        )));
    }
    let their_role = their_role.unwrap_or("").to_string();
    if their_role == role {
        return Err(Error(format!("both sides are {role}s")));
    }
    let words: Vec<&str> = parts.collect();
    Ok(Hello {
        role: their_role,
        fec: words.contains(&"fec"),
        take_over: words.contains(&"takeover"),
    })
}

impl Signaling for Signal {
    fn send(&mut self, kind: &str, body: &str) -> Result<(), Error> {
        Signal::send(self, kind, body)
    }

    fn expect(&mut self, kind: &str) -> Result<String, Error> {
        Signal::expect(self, kind)
    }

    fn local_ip(&self) -> Result<IpAddr, Error> {
        Signal::local_ip(self)
    }
}

#[cfg(test)]
mod hello_tests {
    use super::*;

    #[test]
    fn hellos_say_what_the_other_side_can_do() {
        let ours = hello_body("viewer");
        let hello = check_hello(&ours, "agent").unwrap();
        assert_eq!(hello.role, "viewer");
        assert!(hello.fec);
        // An earlier program's hello: no parity for it.
        let earlier = format!("queqiao {} viewer", queqiao_core::PROTOCOL_VERSION);
        assert!(!check_hello(&earlier, "agent").unwrap().fec);
        // Earlier programs read the first three words: they are as before.
        assert!(ours.starts_with(&format!("{earlier} ")));
        assert!(check_hello(&ours, "viewer").is_err());
        // Taking control: said after what it can do, read only when said.
        assert!(!hello.take_over);
        let taking = check_hello(&format!("{ours} takeover"), "agent").unwrap();
        assert!(taking.take_over && taking.fec);
        assert_eq!(busy_controller("busy:Office PC"), Some("Office PC"));
        assert_eq!(busy_controller("the person at that PC declined"), None);
    }
}
