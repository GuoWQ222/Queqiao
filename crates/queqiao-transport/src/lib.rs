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
pub use signal::{Hello, Signal, Signaling, check_hello, hello_body};
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
