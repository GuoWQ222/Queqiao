//! The file channel over QUIC, as UU Remote moves files over a QUIC channel beside its WebRTC media. SCTP (the data
//! channels) sends without pacing and recovers a lost burst tail only by its retransmission timer, which left file
//! transfers idle for a third of their time (2026-11-23: 4.9 MB/s where the link carries 11.3). QUIC (quinn-proto,
//! RFC 9000/9002) paces, detects losses within a round trip and probes instead of waiting.
//!
//! On the session's own path: QUIC's datagrams go over the pair ICE selected (whatever it is now: direct, through an
//! uplink's socket or through TURN), each after the byte [`MAGIC`], and come back the same way; QUIC is told of one
//! fixed peer address ([`PEER`]), so ICE changing the pair changes nothing for it.
//!
//! Security: each side makes a self-signed certificate for the session and puts its SHA-256 fingerprint into the
//! SDP it sends (offer or answer), which goes over the same authenticated signalling as the DTLS fingerprints. TLS
//! 1.3 both ways: the viewer (the client) accepts only the agent's certificate and the agent only the viewer's, with
//! the handshake signatures checked against them. The agent takes one connection at most.
//!
//! One bidirectional stream (the viewer opens it) carries the file messages, each after its 4-byte length.

use std::collections::VecDeque;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::BytesMut;
use quinn_proto::crypto::rustls::{QuicClientConfig, QuicServerConfig};
use quinn_proto::{
    ClientConfig, Connection, ConnectionHandle, DatagramEvent, Dir, Endpoint, EndpointConfig,
    Event, ReadError, ServerConfig, StreamEvent, StreamId, TransportConfig, VarInt, WriteError,
};
use rustls::DigitallySignedStruct;
use rustls::SignatureScheme;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, verify_tls12_signature, verify_tls13_signature};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls_pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use sha2::{Digest, Sha256};

use crate::Error;

/// First byte of the QUIC datagrams on the session's path. STUN starts with 0-3, DTLS 20-63, RTP and RTCP 128-191,
/// the video parity with `fec::MAGIC` (0xFB).
pub const MAGIC: u8 = 0xFC;

/// The SDP attribute that carries a side's certificate fingerprint (a session attribute; str0m and earlier Queqiao
/// programs ignore attributes they do not know).
pub const SDP_ATTRIBUTE: &str = "x-queqiao-quic";

/// The one peer QUIC knows (TEST-NET-1: never a real address); the datagrams go over ICE's pair.
const PEER: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1)), 443);

const ALPN: &[u8] = b"queqiao-files/1";
const SERVER_NAME: &str = "queqiao";
/// A message larger than this ends the connection (the file messages are 16 kB chunks and small control messages).
const MAX_MESSAGE: usize = 16 << 20;
/// Without a connection this long after starting, QUIC is given up and the files go over SCTP.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(6);
/// Datagrams at most per `poll` (the network thread goes back to str0m between them; QUIC's pacer asks again).
const DATAGRAMS_PER_POLL: usize = 64;

/// This side's certificate for one session.
pub struct Identity {
    certificate: CertificateDer<'static>,
    key: Vec<u8>,
    /// SHA-256 of the certificate, lowercase hex.
    pub fingerprint: String,
}

impl Identity {
    pub fn new() -> Result<Identity, Error> {
        let made = rcgen::generate_simple_self_signed(vec![SERVER_NAME.to_string()])
            .map_err(|e| Error(format!("QUIC certificate: {e}")))?;
        let certificate = made.cert.der().clone();
        Ok(Identity {
            fingerprint: fingerprint(&certificate),
            certificate,
            key: made.signing_key.serialize_der(),
        })
    }

    fn key(&self) -> PrivateKeyDer<'static> {
        PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(self.key.clone()))
    }
}

fn fingerprint(certificate: &CertificateDer<'_>) -> String {
    Sha256::digest(certificate.as_ref())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// `sdp` with our fingerprint as a session attribute (before the first media section).
pub fn add_to_sdp(sdp: &str, identity: &Identity) -> String {
    let line = format!("a={SDP_ATTRIBUTE}:{}\r\n", identity.fingerprint);
    match sdp.find("\r\nm=") {
        Some(at) => format!("{}{line}{}", &sdp[..at + 2], &sdp[at + 2..]),
        None => format!("{sdp}{line}"),
    }
}

/// The other side's fingerprint from its SDP, if it offers QUIC (64 hex digits).
pub fn from_sdp(sdp: &str) -> Option<String> {
    let prefix = format!("a={SDP_ATTRIBUTE}:");
    sdp.lines()
        .find_map(|line| line.trim().strip_prefix(&prefix))
        .map(str::to_ascii_lowercase)
        .filter(|hex| hex.len() == 64 && hex.chars().all(|c| c.is_ascii_hexdigit()))
}

/// Accepts exactly the certificate with the pinned fingerprint, as server (the viewer's check) or as client (the
/// agent's); the TLS 1.3 handshake signatures are checked against it.
#[derive(Debug)]
struct Pinned {
    fingerprint: String,
    provider: Arc<CryptoProvider>,
}

impl Pinned {
    fn check(&self, certificate: &CertificateDer<'_>) -> Result<(), rustls::Error> {
        if fingerprint(certificate) == self.fingerprint {
            Ok(())
        } else {
            Err(rustls::Error::General(
                "the other side's QUIC certificate is not the one its SDP named".into(),
            ))
        }
    }

    fn tls12(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn tls13(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

impl ServerCertVerifier for Pinned {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        self.check(end_entity)
            .map(|()| ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.tls12(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.tls13(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.schemes()
    }
}

impl ClientCertVerifier for Pinned {
    fn root_hint_subjects(&self) -> &[rustls::DistinguishedName] {
        &[]
    }

    fn verify_client_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> Result<ClientCertVerified, rustls::Error> {
        self.check(end_entity)
            .map(|()| ClientCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.tls12(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.tls13(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.schemes()
    }
}

fn err(error: impl std::fmt::Display) -> Error {
    Error(format!("QUIC: {error}"))
}

/// One stream, files only: room for a few MB in each direction (the path's bandwidth-delay product is far less), a
/// keep-alive while files wait, and the connection given up after 30 s without anything from the other side.
fn transport() -> Arc<TransportConfig> {
    let mut transport = TransportConfig::default();
    transport
        .max_concurrent_bidi_streams(VarInt::from_u32(1))
        .max_concurrent_uni_streams(VarInt::from_u32(0))
        .stream_receive_window(VarInt::from_u32(8 << 20))
        .receive_window(VarInt::from_u32(16 << 20))
        .send_window(8 << 20)
        .keep_alive_interval(Some(Duration::from_secs(5)))
        .max_idle_timeout(Some(VarInt::from_u32(30_000).into()))
        // One datagram per transmit: each goes out with its own MAGIC byte.
        .enable_segmentation_offload(false);
    Arc::new(transport)
}

fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// What the QUIC channel tells the network thread.
pub enum QuicEvent {
    /// The file stream is ready (both sides).
    Open,
    /// One file message.
    Message(Vec<u8>),
    /// QUIC did not come up or ended; the reason, for the report.
    Failed(String),
}

pub struct Quic {
    endpoint: Endpoint,
    /// The viewer's settings for connecting (None on the agent).
    client: Option<ClientConfig>,
    connection: Option<(ConnectionHandle, Connection)>,
    stream: Option<StreamId>,
    /// Messages to send, each already after its length; how much of the first is written.
    outgoing: VecDeque<Vec<u8>>,
    written: usize,
    queued: usize,
    /// Bytes received and not yet a whole message.
    incoming: Vec<u8>,
    events: Vec<QuicEvent>,
    started: Option<Instant>,
    /// The viewer's side (it connects; it never takes a connection).
    client_side: bool,
    open: bool,
    failed: bool,
    buffer: Vec<u8>,
}

impl Quic {
    /// The viewer's side: it connects once [`Quic::start`] is called.
    pub fn client(identity: &Identity, peer_fingerprint: &str) -> Result<Quic, Error> {
        let provider = provider();
        let pinned = Arc::new(Pinned {
            fingerprint: peer_fingerprint.to_string(),
            provider: provider.clone(),
        });
        let mut tls = rustls::ClientConfig::builder_with_provider(provider)
            .with_protocol_versions(&[&rustls::version::TLS13])
            .map_err(err)?
            .dangerous()
            .with_custom_certificate_verifier(pinned)
            .with_client_auth_cert(vec![identity.certificate.clone()], identity.key())
            .map_err(err)?;
        tls.alpn_protocols = vec![ALPN.to_vec()];
        let crypto = QuicClientConfig::try_from(tls).map_err(err)?;
        let mut client = ClientConfig::new(Arc::new(crypto));
        client.transport_config(transport());
        Ok(Quic::with(
            Endpoint::new(Arc::new(EndpointConfig::default()), None, true, None),
            Some(client),
        ))
    }

    /// The agent's side: it takes the viewer's connection.
    pub fn server(identity: &Identity, peer_fingerprint: &str) -> Result<Quic, Error> {
        let provider = provider();
        let pinned = Arc::new(Pinned {
            fingerprint: peer_fingerprint.to_string(),
            provider: provider.clone(),
        });
        let mut tls = rustls::ServerConfig::builder_with_provider(provider)
            .with_protocol_versions(&[&rustls::version::TLS13])
            .map_err(err)?
            .with_client_cert_verifier(pinned)
            .with_single_cert(vec![identity.certificate.clone()], identity.key())
            .map_err(err)?;
        tls.alpn_protocols = vec![ALPN.to_vec()];
        let crypto = QuicServerConfig::try_from(tls).map_err(err)?;
        let mut server = ServerConfig::with_crypto(Arc::new(crypto));
        server.transport_config(transport());
        let endpoint = Endpoint::new(
            Arc::new(EndpointConfig::default()),
            Some(Arc::new(server)),
            true,
            None,
        );
        Ok(Quic::with(endpoint, None))
    }

    fn with(endpoint: Endpoint, client: Option<ClientConfig>) -> Quic {
        let client_side = client.is_some();
        Quic {
            endpoint,
            client,
            connection: None,
            stream: None,
            outgoing: VecDeque::new(),
            written: 0,
            queued: 0,
            incoming: Vec::new(),
            events: Vec::new(),
            started: None,
            client_side,
            open: false,
            failed: false,
            buffer: Vec::with_capacity(1500),
        }
    }

    /// The session's path is up: the viewer connects, both start the clock for [`CONNECT_TIMEOUT`].
    pub fn start(&mut self, now: Instant) {
        if self.started.is_some() {
            return;
        }
        self.started = Some(now);
        if let Some(client) = self.client.take() {
            match self.endpoint.connect(now, client, PEER, SERVER_NAME) {
                Ok(connection) => self.connection = Some(connection),
                Err(error) => self.fail(format!("connecting: {error}")),
            }
        }
    }

    /// Bytes of file messages not yet taken by QUIC.
    pub fn queued(&self) -> usize {
        self.queued
    }

    /// A file message to send (in order with the others).
    pub fn send(&mut self, message: &[u8]) {
        let mut framed = Vec::with_capacity(4 + message.len());
        framed.extend_from_slice(&(message.len() as u32).to_le_bytes());
        framed.extend_from_slice(message);
        self.queued += framed.len();
        self.outgoing.push_back(framed);
    }

    /// A datagram that came with [`MAGIC`] in front (`datagram` without it).
    pub fn receive(&mut self, now: Instant, datagram: &[u8]) {
        if self.failed {
            return;
        }
        self.buffer.clear();
        let event = self.endpoint.handle(
            now,
            PEER,
            None,
            None,
            BytesMut::from(datagram),
            &mut self.buffer,
        );
        match event {
            Some(DatagramEvent::ConnectionEvent(handle, event)) => {
                if let Some((ours, connection)) = self.connection.as_mut()
                    && *ours == handle
                {
                    connection.handle_event(event);
                }
            }
            Some(DatagramEvent::NewConnection(incoming)) => {
                // The agent takes the first connection only (one per session); the certificates decide who it is.
                if self.connection.is_some() || self.client_side {
                    self.buffer.clear();
                    let _ = self.endpoint.refuse(incoming, &mut self.buffer);
                    return;
                }
                self.buffer.clear();
                match self.endpoint.accept(incoming, now, &mut self.buffer, None) {
                    Ok(connection) => self.connection = Some(connection),
                    Err(error) => self.fail(format!("accepting: {}", error.cause)),
                }
            }
            Some(DatagramEvent::Response(_)) | None => {}
        }
    }

    /// Runs QUIC up to `now`: the datagrams to send now (each with [`MAGIC`] in front) and what happened.
    pub fn poll(&mut self, now: Instant) -> (Vec<Vec<u8>>, Vec<QuicEvent>) {
        let mut datagrams = Vec::new();
        if let Some(started) = self.started
            && !self.open
            && !self.failed
            && now.duration_since(started) > CONNECT_TIMEOUT
        {
            self.fail(format!(
                "no connection within {} s",
                CONNECT_TIMEOUT.as_secs()
            ));
        }
        if !self.failed && self.connection.is_some() {
            self.drive(now, &mut datagrams);
        }
        (datagrams, std::mem::take(&mut self.events))
    }

    /// When QUIC needs [`Quic::poll`] next, at the latest.
    pub fn next_timeout(&mut self) -> Option<Instant> {
        if self.failed {
            return None;
        }
        let connection = self.connection.as_mut().and_then(|(_, c)| c.poll_timeout());
        let connect = self
            .started
            .filter(|_| !self.open)
            .map(|started| started + CONNECT_TIMEOUT);
        match (connection, connect) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    fn fail(&mut self, reason: String) {
        if !self.failed {
            self.failed = true;
            self.open = false;
            self.events.push(QuicEvent::Failed(reason));
        }
    }

    fn drive(&mut self, now: Instant, datagrams: &mut Vec<Vec<u8>>) {
        let mut lost = None;
        if let Some((handle, connection)) = self.connection.as_mut() {
            let handle = *handle;
            if connection.poll_timeout().is_some_and(|at| at <= now) {
                connection.handle_timeout(now);
            }
            while let Some(event) = connection.poll() {
                match event {
                    // The viewer opens the stream and writes an empty message first, so the agent sees it at once.
                    Event::Connected if self.stream.is_none() && connection.side().is_client() => {
                        if let Some(id) = connection.streams().open(Dir::Bi) {
                            self.stream = Some(id);
                            self.outgoing.push_front(vec![0; 4]);
                            self.queued += 4;
                            self.open = true;
                            self.events.push(QuicEvent::Open);
                        }
                    }
                    Event::Stream(StreamEvent::Opened { dir: Dir::Bi })
                        if self.stream.is_none() =>
                    {
                        if let Some(id) = connection.streams().accept(Dir::Bi) {
                            self.stream = Some(id);
                            self.open = true;
                            self.events.push(QuicEvent::Open);
                        }
                    }
                    Event::ConnectionLost { reason } => lost = Some(reason.to_string()),
                    _ => {}
                }
            }
            // The connection's requests to the endpoint (new connection ids and the like), and the answers.
            while let Some(event) = connection.poll_endpoint_events() {
                if let Some(answer) = self.endpoint.handle_event(handle, event) {
                    connection.handle_event(answer);
                }
            }
            if let Some(stream) = self.stream {
                // Writing: as much as QUIC takes (its send window), in order.
                while let Some(front) = self.outgoing.front() {
                    match connection.send_stream(stream).write(&front[self.written..]) {
                        Ok(0) | Err(WriteError::Blocked) => break,
                        Ok(n) => {
                            self.written += n;
                            self.queued -= n;
                            if self.written == front.len() {
                                self.outgoing.pop_front();
                                self.written = 0;
                            }
                        }
                        Err(error) => {
                            lost = Some(format!("writing: {error}"));
                            break;
                        }
                    }
                }
                // Reading: whatever arrived.
                let mut receive = connection.recv_stream(stream);
                if let Ok(mut chunks) = receive.read(true) {
                    loop {
                        match chunks.next(usize::MAX) {
                            Ok(Some(chunk)) => self.incoming.extend_from_slice(&chunk.bytes),
                            Ok(None) | Err(ReadError::Blocked) => break,
                            Err(error) => {
                                lost = Some(format!("reading: {error}"));
                                break;
                            }
                        }
                    }
                    let _ = chunks.finalize();
                }
            }
        }
        if let Err(reason) = self.take_messages() {
            lost.get_or_insert(reason);
        }
        for _ in 0..DATAGRAMS_PER_POLL {
            self.buffer.clear();
            let Some((_, connection)) = self.connection.as_mut() else {
                break;
            };
            let Some(transmit) = connection.poll_transmit(now, 1, &mut self.buffer) else {
                break;
            };
            let mut datagram = Vec::with_capacity(transmit.size + 1);
            datagram.push(MAGIC);
            datagram.extend_from_slice(&self.buffer[..transmit.size]);
            datagrams.push(datagram);
        }
        if let Some(reason) = lost {
            if self.open || self.client_side {
                self.fail(reason);
            } else {
                // A handshake that failed (someone else's certificate): the agent waits on for the viewer's, until
                // CONNECT_TIMEOUT.
                self.connection = None;
            }
        }
    }

    /// Whole messages out of `incoming`; an oversized one is an error.
    fn take_messages(&mut self) -> Result<(), String> {
        let mut at = 0;
        while self.incoming.len() - at >= 4 {
            let length =
                u32::from_le_bytes(self.incoming[at..at + 4].try_into().unwrap_or_default())
                    as usize;
            if length > MAX_MESSAGE {
                return Err(format!("a message of {length} bytes"));
            }
            if self.incoming.len() - at - 4 < length {
                break;
            }
            if length > 0 {
                self.events.push(QuicEvent::Message(
                    self.incoming[at + 4..at + 4 + length].to_vec(),
                ));
            }
            at += 4 + length;
        }
        self.incoming.drain(..at);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two sides wired back to back: every datagram one sends, the other receives (no loss), until quiet.
    fn exchange(
        viewer: &mut Quic,
        agent: &mut Quic,
        now: &mut Instant,
        rounds: usize,
    ) -> (Vec<QuicEvent>, Vec<QuicEvent>) {
        let (mut seen_viewer, mut seen_agent) = (Vec::new(), Vec::new());
        for _ in 0..rounds {
            let (to_agent, events) = viewer.poll(*now);
            seen_viewer.extend(events);
            for datagram in &to_agent {
                assert_eq!(datagram[0], MAGIC);
                agent.receive(*now, &datagram[1..]);
            }
            let (to_viewer, events) = agent.poll(*now);
            seen_agent.extend(events);
            for datagram in &to_viewer {
                viewer.receive(*now, &datagram[1..]);
            }
            *now += Duration::from_millis(1);
        }
        (seen_viewer, seen_agent)
    }

    #[test]
    fn files_go_both_ways_between_pinned_sides() {
        let (viewer_id, agent_id) = (Identity::new().unwrap(), Identity::new().unwrap());
        let mut viewer = Quic::client(&viewer_id, &agent_id.fingerprint).unwrap();
        let mut agent = Quic::server(&agent_id, &viewer_id.fingerprint).unwrap();
        let mut now = Instant::now();
        agent.start(now);
        viewer.start(now);
        let message: Vec<u8> = (0..100_000u32).map(|i| i as u8).collect();
        viewer.send(&message);
        let (viewer_events, agent_events) = exchange(&mut viewer, &mut agent, &mut now, 200);
        assert!(viewer_events.iter().any(|e| matches!(e, QuicEvent::Open)));
        assert!(agent_events.iter().any(|e| matches!(e, QuicEvent::Open)));
        let received: Vec<&Vec<u8>> = agent_events
            .iter()
            .filter_map(|e| match e {
                QuicEvent::Message(m) => Some(m),
                _ => None,
            })
            .collect();
        assert_eq!(received, vec![&message]);
        agent.send(b"accepted");
        let (viewer_events, _) = exchange(&mut viewer, &mut agent, &mut now, 50);
        assert!(
            viewer_events
                .iter()
                .any(|e| matches!(e, QuicEvent::Message(m) if m == b"accepted"))
        );
        assert_eq!(viewer.queued(), 0);
    }

    #[test]
    fn a_certificate_other_than_the_named_one_is_refused() {
        let (viewer_id, agent_id, stranger) = (
            Identity::new().unwrap(),
            Identity::new().unwrap(),
            Identity::new().unwrap(),
        );
        // The agent expects the viewer, but the stranger connects with its own certificate.
        let mut stranger_side = Quic::client(&stranger, &agent_id.fingerprint).unwrap();
        let mut agent = Quic::server(&agent_id, &viewer_id.fingerprint).unwrap();
        let mut now = Instant::now();
        agent.start(now);
        stranger_side.start(now);
        let (stranger_events, agent_events) =
            exchange(&mut stranger_side, &mut agent, &mut now, 200);
        assert!(!agent_events.iter().any(|e| matches!(e, QuicEvent::Open)));
        // TLS 1.3: the client's handshake ends before the server has checked the client's certificate, so the
        // stranger may think itself connected for a moment; the agent then refuses it and the connection ends.
        assert!(
            stranger_events
                .iter()
                .any(|e| matches!(e, QuicEvent::Failed(_)))
        );
        // The real viewer still gets through afterwards.
        let mut viewer = Quic::client(&viewer_id, &agent_id.fingerprint).unwrap();
        viewer.start(now);
        let (viewer_events, agent_events) = exchange(&mut viewer, &mut agent, &mut now, 200);
        assert!(viewer_events.iter().any(|e| matches!(e, QuicEvent::Open)));
        assert!(agent_events.iter().any(|e| matches!(e, QuicEvent::Open)));
        // And a viewer that expects another agent refuses the real one.
        let mut viewer = Quic::client(&viewer_id, &stranger.fingerprint).unwrap();
        let mut agent = Quic::server(&agent_id, &viewer_id.fingerprint).unwrap();
        agent.start(now);
        viewer.start(now);
        let (viewer_events, agent_events) = exchange(&mut viewer, &mut agent, &mut now, 200);
        assert!(!viewer_events.iter().any(|e| matches!(e, QuicEvent::Open)));
        assert!(!agent_events.iter().any(|e| matches!(e, QuicEvent::Open)));
    }

    #[test]
    fn the_fingerprint_travels_in_the_sdp() {
        let identity = Identity::new().unwrap();
        let sdp = "v=0\r\no=- 1 2 IN IP4 0.0.0.0\r\ns=-\r\nt=0 0\r\nm=application 9 UDP/DTLS/SCTP webrtc-datachannel\r\n";
        let with = add_to_sdp(sdp, &identity);
        assert!(with.find("a=x-queqiao-quic").unwrap() < with.find("m=").unwrap());
        assert_eq!(from_sdp(&with), Some(identity.fingerprint.clone()));
        assert_eq!(from_sdp(sdp), None);
        assert_eq!(from_sdp("a=x-queqiao-quic:not-hex\r\n"), None);
    }
}
