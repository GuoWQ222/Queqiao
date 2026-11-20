//! TLS with a pinned server certificate, the WebSocket on top, and the connection thread's loop.

use std::net::TcpStream;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, TryRecvError};
use std::time::{Duration, Instant};

use queqiao_core::account::{ClientMessage, ServerMessage};
use queqiao_transport::Error;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, verify_tls12_signature, verify_tls13_signature};
use rustls::{ClientConfig, ClientConnection, DigitallySignedStruct, SignatureScheme, StreamOwned};
use rustls_pki_types::{CertificateDer, ServerName, UnixTime};
use sha2::{Digest, Sha256};
use tungstenite::{Message, WebSocket};

use crate::AccountEvent;

pub(crate) type Socket = WebSocket<StreamOwned<ClientConnection, TcpStream>>;

pub(crate) fn err(error: impl std::fmt::Display) -> Error {
    Error(error.to_string())
}

/// Accepts exactly the server certificate with the pinned SHA-256 fingerprint; the handshake signatures
/// are still checked against that certificate.
#[derive(Debug)]
struct PinnedCertificate {
    fingerprint: String,
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for PinnedCertificate {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let actual: String = Sha256::digest(end_entity.as_ref())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        if actual == self.fingerprint {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::General(format!(
                "the server's certificate ({actual}) is not the pinned one"
            )))
        }
    }

    fn verify_tls12_signature(
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

    fn verify_tls13_signature(
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

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// A fingerprint as lower-case hex, whether it was written with colons, spaces or capitals.
pub(crate) fn normalize_pin(pin: &str) -> String {
    pin.chars()
        .filter(char::is_ascii_hexdigit)
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// TLS (pinned) and the WebSocket handshake on a connected TCP stream.
pub(crate) fn open(tcp: TcpStream, server: &str, pin: &str) -> Result<Socket, Error> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let verifier = PinnedCertificate {
        fingerprint: normalize_pin(pin),
        provider: provider.clone(),
    };
    let config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(err)?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_no_client_auth();
    // The certificate is pinned, so the name only goes into the TLS hello.
    let name = ServerName::try_from("queqiao-server").map_err(err)?;
    let connection = ClientConnection::new(Arc::new(config), name).map_err(err)?;
    tcp.set_read_timeout(Some(Duration::from_secs(15)))?;
    let stream = StreamOwned::new(connection, tcp);
    let (socket, _) = tungstenite::client(format!("ws://{server}/"), stream)
        .map_err(|e| Error(format!("connecting to {server}: {e}")))?;
    Ok(socket)
}

/// The connection thread: sends the program's messages, delivers the server's, pings every 20 s and
/// gives up when the server has been silent for 60 s (it answers every ping).
pub(crate) fn run(
    mut socket: Socket,
    commands: Receiver<ClientMessage>,
    events: &Sender<AccountEvent>,
) -> String {
    let _ = socket
        .get_ref()
        .sock
        .set_read_timeout(Some(Duration::from_millis(20)));
    let mut next_ping = Instant::now() + Duration::from_secs(20);
    let mut last_heard = Instant::now();
    loop {
        if last_heard.elapsed() > Duration::from_secs(60) {
            return "the server stopped answering".into();
        }
        loop {
            match commands.try_recv() {
                Ok(message) => {
                    let Ok(text) = serde_json::to_string(&message) else {
                        continue;
                    };
                    if let Err(error) = socket.send(Message::text(text)) {
                        return format!("sending: {error}");
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    let _ = socket.close(None);
                    let _ = socket.flush();
                    return "closed".into();
                }
            }
        }
        if Instant::now() >= next_ping {
            next_ping = Instant::now() + Duration::from_secs(20);
            if let Err(error) = socket.send(Message::Ping(Vec::new().into())) {
                return format!("ping: {error}");
            }
        }
        let read = socket.read();
        if read.is_ok() {
            last_heard = Instant::now();
        }
        match read {
            Ok(Message::Text(text)) => {
                let event = match serde_json::from_str::<ServerMessage>(&text) {
                    Ok(ServerMessage::Devices { devices }) => AccountEvent::Devices(devices),
                    Ok(ServerMessage::Signal {
                        from,
                        from_name,
                        from_role,
                        session,
                        kind,
                        body,
                        signature,
                    }) => AccountEvent::Signal {
                        from,
                        from_name,
                        from_role,
                        session,
                        kind,
                        body,
                        signature,
                    },
                    Ok(ServerMessage::Error { message }) => AccountEvent::Error(message),
                    Ok(ServerMessage::Notice { message }) => AccountEvent::Notice(message),
                    Ok(ServerMessage::PasswordChanged { refresh_token }) => {
                        AccountEvent::PasswordChanged(refresh_token)
                    }
                    Ok(ServerMessage::IceServers { stun, turn }) => {
                        AccountEvent::IceServers { stun, turn }
                    }
                    Ok(ServerMessage::Welcome { .. }) => continue,
                    Err(error) => {
                        AccountEvent::Error(format!("bad message from the server: {error}"))
                    }
                };
                if events.send(event).is_err() {
                    return "the program stopped listening".into();
                }
            }
            Ok(Message::Close(_)) => return "the server closed the connection".into(),
            Ok(_) => {}
            Err(tungstenite::Error::Io(error))
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(error) => return format!("connection to the server lost: {error}"),
        }
    }
}
