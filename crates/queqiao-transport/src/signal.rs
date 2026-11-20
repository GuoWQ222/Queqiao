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

    /// Exchanges hellos: both sides must speak the same protocol version and have different roles.
    /// Returns the other side's role.
    fn hello(&mut self, role: &str) -> Result<String, Error> {
        self.send("hello", &hello_body(role))?;
        check_hello(&self.expect("hello")?, role)
    }
}

/// Our hello: "queqiao <protocol version> <role>".
pub fn hello_body(role: &str) -> String {
    format!("queqiao {} {role}", queqiao_core::PROTOCOL_VERSION)
}

/// Checks the other side's hello (same protocol version, the other role) and returns its role.
pub fn check_hello(body: &str, role: &str) -> Result<String, Error> {
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
    Ok(their_role)
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
