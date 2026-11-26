//! Network transport between viewer and agent: WebRTC through str0m (ICE, DTLS-SRTP, H.264 over RTP, an
//! SCTP data channel for input and control), driven on its own thread ([`Peer`]), and the signaling that
//! sets it up ([`Signaling`]: a direct TCP connection ([`Signal`]) or queqiao-server through queqiao-account).

mod fec;
mod flow;
mod gather;
mod interfaces;
mod peer;
mod quic;
mod resolve;
mod shape;
mod signal;
mod turn;

pub use interfaces::{connect_direct, connect_direct_noted, public_address};
pub use peer::{
    Addresses, AudioSender, Peer, PeerEvent, Role, START_BITRATE, STREAM_IN_FLIGHT, Stream, tune,
};
pub use resolve::resolve_server;
pub use signal::{
    BUSY, Hello, Interruption, SessionRequest, Signal, Signaling, busy_controller, check_hello,
    hello_body,
};
pub use turn::{IceServers, TurnServer};

/// A transport error, with a readable reason.
#[derive(Debug)]
pub struct Error(pub String);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Error(error.to_string())
    }
}

pub(crate) fn err(error: impl std::fmt::Display) -> Error {
    Error(error.to_string())
}

/// The relay's name in a path as the session reports it (`PeerEvent::Path`): "relayed through 198.51.100.1:3478
/// (shanghai) to ..." -> "shanghai"; none for a direct path or a relay without a name.
pub fn relay_name(path: &str) -> Option<&str> {
    let rest = path.strip_prefix("relayed through ")?;
    let (_, after) = rest.split_once(" (")?;
    let (name, tail) = after.split_once(')')?;
    (tail.starts_with(" to ") && !name.is_empty() && !name.contains(' ')).then_some(name)
}

#[cfg(test)]
mod path_tests {
    #[test]
    fn the_relays_name_is_read_back_from_the_path() {
        let named = "relayed through 198.51.100.1:3478 (shanghai) to 198.51.100.1:47203";
        assert_eq!(super::relay_name(named), Some("shanghai"));
        assert_eq!(
            super::relay_name("relayed through 198.51.100.1:3478 to 198.51.100.1:47203"),
            None
        );
        assert_eq!(
            super::relay_name(
                "relayed through that PC's relay 198.51.100.1:47203 (from 10.0.0.2:5000)"
            ),
            None
        );
        assert_eq!(
            super::relay_name("direct to 203.0.113.5:4000 (from 10.0.0.2:5000)"),
            None
        );
    }
}
