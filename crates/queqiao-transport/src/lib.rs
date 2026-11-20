//! Network transport between viewer and agent: WebRTC through str0m (ICE, DTLS-SRTP, H.264 over RTP, an
//! SCTP data channel for input and control), driven on its own thread ([`Peer`]), and the signaling that
//! sets it up ([`Signal`]; a direct TCP connection for now, queqiao-server later).

mod peer;
mod signal;

pub use peer::{Peer, PeerEvent, Role};
pub use signal::Signal;

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
