//! The link between a relay server (`queqiao-server relay`) and the account server it serves: the relay connects
//! out to the account server's usual TLS WebSocket (its certificate pinned, as the programs do), and proves it holds
//! the key `add-relay` made for its name, by an HMAC-SHA256 over both sides' fresh nonces. Then it reports where it is
//! and its load every REPORT_EVERY, and the account server sends it every revocation of relay credentials. The relay
//! checks credentials itself (they are made with its key), so sessions do not wait on the link; one that is down
//! for a while only stops the relay from being handed out.
//!
//! A standby (`relay --standby`) also keeps a copy of the accounts over its link (replica.rs). A relay given several
//! account servers (`--central` more than once: the account server, then its standbys) links to the first that
//! takes it, so after a standby was made the account server, the relays follow it.

use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use ring::hmac;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, verify_tls12_signature, verify_tls13_signature};
use rustls::{ClientConfig, DigitallySignedStruct, SignatureScheme};
use rustls_pki_types::{CertificateDer, ServerName, UnixTime};
use serde::{Deserialize, Serialize};
use tokio::net::TcpStream;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;

use crate::replica::Change;
use crate::store::{Store, hex, random_hex, sha256_hex};
use crate::turn::TurnConfig;

/// A relay reports this often.
pub const REPORT_EVERY: Duration = Duration::from_secs(2);
/// The relay pings this often and gives the link up after LINK_SILENCE without a word from the account server.
const PING_EVERY: Duration = Duration::from_secs(20);
const LINK_SILENCE: Duration = Duration::from_secs(60);
/// After a failed or lost link the relay tries again after this, doubling up to RETRY_MAX.
const RETRY_FIRST: Duration = Duration::from_secs(2);
const RETRY_MAX: Duration = Duration::from_secs(60);
const STEP_TIMEOUT: Duration = Duration::from_secs(15);
/// The account server's messages (a batch of a standby's changes is the largest, a few hundred kB).
const MAX_MESSAGE: usize = 4 << 20;

/// From a relay server (the tags start with "relay_", which no program's message does).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum FromRelay {
    /// The first message: which relay, and its nonce (hex) for the proof.
    #[serde(rename = "relay_hello")]
    Hello { name: String, nonce: String },
    /// HMAC-SHA256 (`proof`) with its key over both nonces, hex.
    #[serde(rename = "relay_proof")]
    Proof { proof: String },
    /// Where clients reach it (host:port, UDP), its capacity and what it relays now, in Mbit/s.
    #[serde(rename = "relay_load")]
    Load {
        address: String,
        capacity_mbps: f64,
        rate_mbps: f64,
        allocations: u32,
    },
    /// A standby (`relay --standby`): the accounts' changes after `after` (0: a full copy), and where it would serve
    /// the accounts once made the account server (host:port, TCP) with its certificate's fingerprint.
    #[serde(rename = "relay_replicate")]
    Replicate {
        after: i64,
        accounts: String,
        pin: String,
    },
}

/// To a relay server.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToRelay {
    #[serde(rename = "relay_challenge")]
    Challenge {
        nonce: String,
    },
    #[serde(rename = "relay_welcome")]
    Welcome,
    /// Credentials of `device`'s `programs` made until `at` (Unix seconds) stop working.
    Revoke {
        device: String,
        programs: Vec<String>,
        at: u64,
    },
    Error {
        message: String,
    },
    /// To a standby: changes to apply (`reset` first: the start of a full copy, the copied tables emptied), after
    /// which it stands at change `upto` (0: in the middle of a full copy).
    #[serde(rename = "relay_changes")]
    Changes {
        reset: bool,
        upto: i64,
        changes: Vec<Change>,
    },
}

fn signed(name: &str, relay_nonce: &str, server_nonce: &str) -> String {
    format!("queqiao relay link 1\n{name}\n{relay_nonce}\n{server_nonce}")
}

/// The proof a relay sends: HMAC-SHA256 with its key over its name and both nonces, hex.
pub fn proof(key: &[u8; 32], name: &str, relay_nonce: &str, server_nonce: &str) -> String {
    let key = hmac::Key::new(hmac::HMAC_SHA256, key);
    hex(hmac::sign(&key, signed(name, relay_nonce, server_nonce).as_bytes()).as_ref())
}

/// Whether `proof` is right (compared in constant time).
pub fn proof_valid(
    key: &[u8; 32],
    name: &str,
    relay_nonce: &str,
    server_nonce: &str,
    proof: &str,
) -> bool {
    let Some(tag) = crate::store::unhex(proof) else {
        return false;
    };
    let key = hmac::Key::new(hmac::HMAC_SHA256, key);
    hmac::verify(
        &key,
        signed(name, relay_nonce, server_nonce).as_bytes(),
        &tag,
    )
    .is_ok()
}

/// A nonce is 32 random bytes as hex; a relay's must look like one.
pub fn nonce_valid(nonce: &str) -> bool {
    nonce.len() == 64 && nonce.bytes().all(|b| b.is_ascii_hexdigit())
}

/// A relay's key file (`relay-key` writes it): "name:key in hex", as `add-relay` printed it.
pub fn parse_key_line(line: &str) -> Result<(String, [u8; 32]), String> {
    let (name, key) = line
        .trim()
        .split_once(':')
        .ok_or("the relay key is name:key, as add-relay printed it")?;
    if !crate::store::valid_relay_name(name) {
        return Err("the relay key's name is not a relay name".into());
    }
    let key: [u8; 32] = crate::store::unhex(key)
        .and_then(|key| key.try_into().ok())
        .ok_or("the relay key is not 64 hex digits")?;
    Ok((name.to_string(), key))
}

/// Accepts exactly the account server's certificate with the pinned SHA-256 fingerprint; the handshake signatures
/// are still checked against it (as the programs' connection does).
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
        let actual = sha256_hex(end_entity.as_ref());
        if actual == self.fingerprint {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::General(format!(
                "the account server's certificate ({actual}) is not the pinned one"
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

/// What a relay server needs to keep its link.
pub struct RelaySide {
    /// The account servers it may link to, in order (host:port, certificate fingerprint).
    pub centrals: Vec<(String, String)>,
    pub name: String,
    pub key: [u8; 32],
    pub turn: Arc<TurnConfig>,
    pub capacity_mbps: f64,
    /// A standby's copy of the accounts.
    pub standby: Option<StandbySide>,
}

/// A standby's copy of the accounts, and where it would serve them once made the account server.
pub struct StandbySide {
    pub store: Mutex<Store>,
    pub accounts: String,
    pub pin: String,
}

/// Keeps the link to an account server up for as long as the relay runs: the first of `centrals` that takes it.
pub async fn keep_linked(side: RelaySide) {
    let mut retry = RETRY_FIRST;
    loop {
        let started = Instant::now();
        for (central, pin) in &side.centrals {
            match link_once(&side, central, pin).await {
                Ok(()) => eprintln!("link to {central}: closed"),
                Err(error) => eprintln!("link to {central}: {error}"),
            }
        }
        // A link that held for a while starts the waiting over.
        if started.elapsed() > RETRY_MAX {
            retry = RETRY_FIRST;
        }
        tokio::time::sleep(retry).await;
        retry = (retry * 2).min(RETRY_MAX);
    }
}

async fn link_once(side: &RelaySide, central: &str, pin: &str) -> Result<(), String> {
    let address: SocketAddr = tokio::net::lookup_host(central)
        .await
        .map_err(|e| format!("{central}: {e}"))?
        .next()
        .ok_or("no address")?;
    let tcp = timeout(STEP_TIMEOUT, TcpStream::connect(address))
        .await
        .map_err(|_| "connecting timed out")?
        .map_err(|e| format!("connecting: {e}"))?;
    let _ = tcp.set_nodelay(true);
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let verifier = PinnedCertificate {
        fingerprint: pin
            .chars()
            .filter(char::is_ascii_hexdigit)
            .map(|c| c.to_ascii_lowercase())
            .collect(),
        provider: provider.clone(),
    };
    let config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_no_client_auth();
    let name = ServerName::try_from("queqiao-server").map_err(|e| e.to_string())?;
    let tls = timeout(
        STEP_TIMEOUT,
        tokio_rustls::TlsConnector::from(Arc::new(config)).connect(name, tcp),
    )
    .await
    .map_err(|_| "TLS timed out")?
    .map_err(|e| format!("TLS: {e}"))?;
    let limits = WebSocketConfig::default()
        .max_message_size(Some(MAX_MESSAGE))
        .max_frame_size(Some(MAX_MESSAGE));
    let (socket, _) = timeout(
        STEP_TIMEOUT,
        tokio_tungstenite::client_async_with_config(format!("ws://{central}/"), tls, Some(limits)),
    )
    .await
    .map_err(|_| "WebSocket timed out")?
    .map_err(|e| format!("WebSocket: {e}"))?;
    let (mut sink, mut stream) = socket.split();
    let send =
        |message: &FromRelay| Message::text(serde_json::to_string(message).unwrap_or_default());

    let nonce = random_hex(32)?;
    sink.send(send(&FromRelay::Hello {
        name: side.name.clone(),
        nonce: nonce.clone(),
    }))
    .await
    .map_err(|e| e.to_string())?;
    let server_nonce = match next_message(&mut stream).await? {
        ToRelay::Challenge { nonce } if nonce_valid(&nonce) => nonce,
        ToRelay::Error { message } => return Err(message),
        other => return Err(format!("unexpected: {other:?}")),
    };
    sink.send(send(&FromRelay::Proof {
        proof: proof(&side.key, &side.name, &nonce, &server_nonce),
    }))
    .await
    .map_err(|e| e.to_string())?;
    match next_message(&mut stream).await? {
        ToRelay::Welcome => {}
        ToRelay::Error { message } => return Err(message),
        other => return Err(format!("unexpected: {other:?}")),
    }
    eprintln!("link to {central}: joined as relay {}", side.name);
    // A standby asks for the accounts' changes after the last it applied.
    if let Some(standby) = &side.standby {
        let after = standby
            .store
            .lock()
            .map_err(|e| e.to_string())?
            .replica_seq()?;
        let replicate = FromRelay::Replicate {
            after,
            accounts: standby.accounts.clone(),
            pin: standby.pin.clone(),
        };
        sink.send(send(&replicate))
            .await
            .map_err(|e| e.to_string())?;
        eprintln!(
            "standby: asked for the accounts' changes after {after}{}",
            if after == 0 { " (a full copy)" } else { "" }
        );
    }

    let mut report = tokio::time::interval(REPORT_EVERY);
    let mut ping = tokio::time::interval(PING_EVERY);
    let mut last = (side.turn.relayed.load(Ordering::Relaxed), Instant::now());
    let mut heard = Instant::now();
    let mut copying = false;
    loop {
        tokio::select! {
            _ = report.tick() => {
                if heard.elapsed() > LINK_SILENCE {
                    return Err("the account server stopped answering".into());
                }
                let now = (side.turn.relayed.load(Ordering::Relaxed), Instant::now());
                let seconds = now.1.saturating_duration_since(last.1).as_secs_f64().max(0.001);
                let rate_mbps = now.0.saturating_sub(last.0) as f64 * 8.0 / seconds / 1e6;
                last = now;
                let load = FromRelay::Load {
                    address: side.turn.address(),
                    capacity_mbps: side.capacity_mbps,
                    rate_mbps,
                    allocations: side.turn.allocations.load(Ordering::Relaxed) as u32,
                };
                timeout(STEP_TIMEOUT, sink.send(send(&load))).await.map_err(|_| "sending timed out")?.map_err(|e| e.to_string())?;
            }
            _ = ping.tick() => {
                timeout(STEP_TIMEOUT, sink.send(Message::Ping(Vec::new().into()))).await.map_err(|_| "sending timed out")?.map_err(|e| e.to_string())?;
            }
            incoming = stream.next() => {
                heard = Instant::now();
                match incoming {
                    Some(Ok(Message::Text(text))) => match serde_json::from_str::<ToRelay>(&text) {
                        Ok(ToRelay::Revoke { device, programs, at }) => {
                            let programs: Vec<&str> = programs.iter().map(String::as_str).collect();
                            side.turn.revoke_at(&device, &programs, at);
                        }
                        Ok(ToRelay::Changes { reset, upto, changes }) => {
                            if let Some(standby) = &side.standby {
                                standby
                                    .store
                                    .lock()
                                    .map_err(|e| e.to_string())?
                                    .apply_changes(reset, &changes, upto)
                                    .map_err(|e| format!("standby: {e}"))?;
                                if reset {
                                    eprintln!("standby: a full copy of the accounts begins");
                                    copying = true;
                                }
                                if upto > 0 && copying {
                                    eprintln!("standby: the full copy is done, at change {upto}");
                                    copying = false;
                                }
                            }
                        }
                        Ok(ToRelay::Error { message }) => return Err(message),
                        Ok(_) | Err(_) => {}
                    },
                    Some(Ok(Message::Ping(data))) => {
                        let _ = timeout(STEP_TIMEOUT, sink.send(Message::Pong(data))).await;
                    }
                    Some(Ok(Message::Close(_))) | None => return Ok(()),
                    Some(Err(error)) => return Err(error.to_string()),
                    Some(Ok(_)) => {}
                }
            }
        }
    }
}

/// The next message from the account server (pings and the like skipped), within STEP_TIMEOUT.
async fn next_message<S>(stream: &mut S) -> Result<ToRelay, String>
where
    S: StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    let deadline = Instant::now() + STEP_TIMEOUT;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match timeout(left, stream.next()).await {
            Ok(Some(Ok(Message::Text(text)))) => {
                return serde_json::from_str(&text).map_err(|e| format!("bad message: {e}"));
            }
            Ok(Some(Ok(Message::Close(_))) | None) => {
                return Err("the account server closed the link".into());
            }
            Ok(Some(Err(error))) => return Err(error.to_string()),
            Ok(Some(Ok(_))) => {}
            Err(_) => return Err("the account server did not answer in time".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_right_key_proves_a_relay_for_this_exchange() {
        let key = [5u8; 32];
        let (relay_nonce, server_nonce) = ("a".repeat(64), "b".repeat(64));
        let good = proof(&key, "shanghai", &relay_nonce, &server_nonce);
        assert!(proof_valid(
            &key,
            "shanghai",
            &relay_nonce,
            &server_nonce,
            &good
        ));
        // Another key, another name, another exchange's nonces, or a changed proof: refused.
        assert!(!proof_valid(
            &[6u8; 32],
            "shanghai",
            &relay_nonce,
            &server_nonce,
            &good
        ));
        assert!(!proof_valid(
            &key,
            "chengdu",
            &relay_nonce,
            &server_nonce,
            &good
        ));
        assert!(!proof_valid(
            &key,
            "shanghai",
            &relay_nonce,
            &"c".repeat(64),
            &good
        ));
        let mut changed = good.clone().into_bytes();
        changed[0] = if changed[0] == b'0' { b'1' } else { b'0' };
        assert!(!proof_valid(
            &key,
            "shanghai",
            &relay_nonce,
            &server_nonce,
            &String::from_utf8(changed).unwrap()
        ));
        assert!(!proof_valid(
            &key,
            "shanghai",
            &relay_nonce,
            &server_nonce,
            "zz"
        ));
        assert!(nonce_valid(&relay_nonce) && !nonce_valid("abc") && !nonce_valid(&"g".repeat(64)));
    }

    #[test]
    fn a_relay_key_line_is_read_back_as_add_relay_wrote_it() {
        let line = format!("shanghai:{}\n", "0f".repeat(32));
        let (name, key) = parse_key_line(&line).unwrap();
        assert_eq!((name.as_str(), key), ("shanghai", [0x0f; 32]));
        assert!(parse_key_line("shanghai").is_err());
        assert!(parse_key_line(&format!("bad name:{}", "0f".repeat(32))).is_err());
        assert!(parse_key_line("shanghai:0f0f").is_err());
    }

    #[test]
    fn link_messages_are_tagged_json() {
        let load = FromRelay::Load {
            address: "198.51.100.1:3478".into(),
            capacity_mbps: 100.0,
            rate_mbps: 1.5,
            allocations: 2,
        };
        let text = serde_json::to_string(&load).unwrap();
        assert!(text.contains("\"type\":\"relay_load\""));
        assert_eq!(serde_json::from_str::<FromRelay>(&text).unwrap(), load);
        let revoke = ToRelay::Revoke {
            device: "d".into(),
            programs: vec!["agent".into()],
            at: 5,
        };
        let text = serde_json::to_string(&revoke).unwrap();
        assert_eq!(serde_json::from_str::<ToRelay>(&text).unwrap(), revoke);
    }
}
