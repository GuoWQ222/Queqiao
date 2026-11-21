//! Connections: TLS, then a WebSocket, then a login or resume within 15 s. Each connected program (agent
//! or viewer of a device) gets a channel; the hub forwards signals between the programs of one account only
//! and pushes the account's device list whenever a program connects or leaves.

use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use queqiao_core::account::{ClientMessage, Device, Role, ServerMessage, TurnAccess};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};
use tokio::time::timeout;
use tokio_rustls::TlsAcceptor;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;

use sha2::{Digest, Sha256};

use crate::store::Store;
use crate::turn::TurnConfig;

/// Messages are small (an SDP is a few kB): anything over 256 kB is refused.
const MAX_MESSAGE: usize = 256 * 1024;
/// A connection that sends nothing for this long is dropped (clients ping every 20 s).
const IDLE: Duration = Duration::from_secs(60);
/// Failed logins (wrong password, or a saved login that is not valid) allowed per address in a window, and
/// wrong passwords per account name (counted the same way for names that do not exist, so a refusal says
/// nothing about whether an account exists; saved logins keep working during an account's lockout).
const ADDRESS_FAILURES: u32 = 20;
const ADDRESS_WINDOW: Duration = Duration::from_secs(600);
const ACCOUNT_FAILURES: u32 = 5;
const ACCOUNT_WINDOW: Duration = Duration::from_secs(900);
/// Wrong help codes for one device before its help requests close (the person opens them again for a new code).
const HELP_FAILURES: u32 = 5;

/// A viewer's ask to help without a code, waiting for the person at the agent's PC.
struct PendingAsk {
    request: u64,
    viewer_device: String,
    viewer_name: String,
    viewer_user: String,
    viewer_sender: UnboundedSender<Outgoing>,
}

/// An agent's open help requests: the code's SHA-256 and the wrong tries so far.
struct HelpOffer {
    code_hash: [u8; 32],
    wrong: u32,
}

/// Failures counted since `since`.
struct Window {
    count: u32,
    since: Instant,
}

#[derive(Default)]
struct Failures {
    by_address: HashMap<IpAddr, Window>,
    by_account: HashMap<String, Window>,
}

/// Why a login was refused, and whether it counts as a guess.
struct Refusal {
    message: String,
    guess: bool,
}

impl From<String> for Refusal {
    fn from(message: String) -> Refusal {
        Refusal {
            message,
            guess: false,
        }
    }
}

impl From<&str> for Refusal {
    fn from(message: &str) -> Refusal {
        Refusal::from(message.to_string())
    }
}

/// New accounts from one address in an hour, at most.
const SIGNUPS_PER_HOUR: usize = 3;

/// A new account's name: 3 to 32 letters, digits, dot, dash or underscore, starting with a letter or digit; its
/// password: 8 to 128 characters, not the name.
fn check_new_account(user: &str, password: &str) -> Result<(), String> {
    let name_ok = (3..=32).contains(&user.len())
        && user
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric())
        && user
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'));
    if !name_ok {
        return Err("a user name has 3 to 32 letters, digits, dots, dashes or underscores, and starts with a letter or digit".into());
    }
    let length = password.chars().count();
    if !(8..=128).contains(&length) || password.eq_ignore_ascii_case(user) {
        return Err("a password has 8 to 128 characters and is not the user name".into());
    }
    Ok(())
}

fn guess(message: &str) -> Refusal {
    Refusal {
        message: message.to_string(),
        guess: true,
    }
}

/// To a connection's task: a message for its client, or the order to close (a newer connection of the
/// same device and program took its place).
enum Outgoing {
    Message(ServerMessage),
    Kick,
}

struct Connection {
    id: u64,
    user_id: i64,
    device_name: String,
    sender: UnboundedSender<Outgoing>,
}

pub struct Hub {
    /// New accounts may be made from the programs' login forms (`serve --allow-signup`).
    signup: bool,
    /// When each address last made accounts (the last hour's).
    signups: Mutex<HashMap<IpAddr, Vec<Instant>>>,
    store: Mutex<Store>,
    failures: Mutex<Failures>,
    turn: Option<Arc<TurnConfig>>,
    online: Mutex<HashMap<(String, Role), Connection>>,
    next_id: AtomicU64,
    /// Agents taking help requests, by device id.
    help: Mutex<HashMap<String, HelpOffer>>,
    /// (viewer device, agent device) of different accounts that may signal each other: the viewer gave the
    /// agent's help code. Ends when the agent closes help or leaves.
    grants: Mutex<HashSet<(String, String)>>,
    /// Asks without a code, by agent device: one at a time each.
    asks: Mutex<HashMap<String, PendingAsk>>,
}

/// Who a connection is, after login.
struct Identity {
    user_id: i64,
    user: String,
    device_id: String,
    device_name: String,
    role: Role,
}

impl Hub {
    pub fn new(store: Store, turn: Option<Arc<TurnConfig>>, signup: bool) -> Arc<Hub> {
        Arc::new(Hub {
            signup,
            signups: Mutex::new(HashMap::new()),
            store: Mutex::new(store),
            failures: Mutex::new(Failures::default()),
            turn,
            online: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            help: Mutex::new(HashMap::new()),
            grants: Mutex::new(HashSet::new()),
            asks: Mutex::new(HashMap::new()),
        })
    }

    pub async fn serve(
        self: Arc<Hub>,
        listen: SocketAddr,
        acceptor: TlsAcceptor,
    ) -> std::io::Result<()> {
        let listener = TcpListener::bind(listen).await?;
        loop {
            let (tcp, peer) = listener.accept().await?;
            let hub = self.clone();
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                if let Err(error) = hub.connection(tcp, acceptor).await {
                    eprintln!("{peer}: {error}");
                }
            });
        }
    }

    async fn connection(
        self: Arc<Hub>,
        tcp: TcpStream,
        acceptor: TlsAcceptor,
    ) -> Result<(), String> {
        let _ = tcp.set_nodelay(true);
        let address = tcp.peer_addr().ok().map(|peer| peer.ip());
        let tls = timeout(Duration::from_secs(10), acceptor.accept(tcp))
            .await
            .map_err(|_| "TLS handshake timed out")?
            .map_err(|e| format!("TLS: {e}"))?;
        let limits = WebSocketConfig::default()
            .max_message_size(Some(MAX_MESSAGE))
            .max_frame_size(Some(MAX_MESSAGE));
        let socket = timeout(
            Duration::from_secs(10),
            tokio_tungstenite::accept_async_with_config(tls, Some(limits)),
        )
        .await
        .map_err(|_| "WebSocket handshake timed out")?
        .map_err(|e| format!("WebSocket: {e}"))?;
        let (mut sink, mut stream) = socket.split();

        let first = match timeout(Duration::from_secs(15), stream.next()).await {
            Ok(Some(Ok(Message::Text(text)))) => {
                serde_json::from_str::<ClientMessage>(&text).map_err(|e| e.to_string())?
            }
            _ => return Err("no login".into()),
        };
        let account_name = match &first {
            ClientMessage::Login { user, .. } | ClientMessage::Register { user, .. } => {
                Some(user.trim().to_lowercase())
            }
            _ => None,
        };
        let locked = self.locked(address, account_name.as_deref());
        // New accounts: at most SIGNUPS_PER_HOUR made from one address.
        let registering = matches!(first, ClientMessage::Register { .. });
        let signup_wait = if locked.is_none() && registering {
            self.signup_wait(address)
        } else {
            None
        };
        // Password hashing takes tens of milliseconds: off the network threads.
        let hub = self.clone();
        let checked = match (locked, signup_wait) {
            (Some(minutes), _) => Err(Refusal::from(format!(
                "too many failed logins: try again in {minutes} minutes"
            ))),
            (None, Some(minutes)) => Err(Refusal::from(format!(
                "too many new accounts from this address: try again in {minutes} minutes"
            ))),
            (None, None) => tokio::task::spawn_blocking(move || hub.authenticate(first))
                .await
                .map_err(|e| e.to_string())?,
        };
        if registering && checked.is_ok() {
            self.record_signup(address);
        }
        if checked.as_ref().is_err_and(|refusal| refusal.guess) {
            self.record_failure(address, account_name.as_deref());
        } else if checked.is_ok()
            && let Some(name) = &account_name
            && let Ok(mut failures) = self.failures.lock()
        {
            failures.by_account.remove(name);
        }
        let (identity, new_token, new_device) = match checked {
            Ok(result) => result,
            Err(Refusal { message, .. }) => {
                // A failed login waits a little, which slows down guessing.
                tokio::time::sleep(Duration::from_secs(1)).await;
                let reply = serde_json::to_string(&ServerMessage::Error {
                    message: message.clone(),
                })
                .unwrap_or_default();
                let _ = sink.send(Message::text(reply)).await;
                return Err(format!("login refused: {message}"));
            }
        };

        // Where the device connects from: Wake-on-LAN looks for another device behind the same address.
        if let (Some(address), Ok(store)) = (address, self.store.lock()) {
            let _ = store.set_last_address(&identity.device_id, &address.to_string());
        }
        let (sender, mut receiver) = unbounded_channel();
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let key = (identity.device_id.clone(), identity.role);
        let replaced = self.online.lock().map_err(|e| e.to_string())?.insert(
            key.clone(),
            Connection {
                id,
                user_id: identity.user_id,
                device_name: identity.device_name.clone(),
                sender: sender.clone(),
            },
        );
        if let Some(old) = replaced {
            let _ = old.sender.send(Outgoing::Kick);
        }
        let welcome = ServerMessage::Welcome {
            user: identity.user.clone(),
            device_id: identity.device_id.clone(),
            refresh_token: new_token,
            devices: self.device_list(identity.user_id),
        };
        let _ = sender.send(Outgoing::Message(welcome));
        self.push_devices(identity.user_id);
        if new_device {
            let message = format!(
                "A new device, {}, logged in to your account.",
                identity.device_name
            );
            self.notify_account(identity.user_id, Some(&key), &message);
        }

        loop {
            tokio::select! {
                incoming = timeout(IDLE, stream.next()) => match incoming {
                    Ok(Some(Ok(Message::Text(text)))) => match serde_json::from_str::<ClientMessage>(&text) {
                        Ok(ClientMessage::ChangePassword { old_password, new_password }) => {
                            self.clone().change_password(&identity, old_password, new_password, &sender).await;
                        }
                        Ok(message) => self.handle(&identity, message, &sender, address),
                        Err(error) => {
                            let message = format!("bad message: {error}");
                            let _ = sender.send(Outgoing::Message(ServerMessage::Error { message }));
                        }
                    },
                    Ok(Some(Ok(Message::Ping(data)))) => {
                        if sink.send(Message::Pong(data)).await.is_err() {
                            break;
                        }
                    }
                    Ok(Some(Ok(Message::Close(_))) | None | Some(Err(_))) | Err(_) => break,
                    Ok(Some(Ok(_))) => {}
                },
                outgoing = receiver.recv() => match outgoing {
                    Some(Outgoing::Message(message)) => {
                        let text = serde_json::to_string(&message).map_err(|e| e.to_string())?;
                        if sink.send(Message::text(text)).await.is_err() {
                            break;
                        }
                    }
                    Some(Outgoing::Kick) | None => {
                        let _ = sink.close().await;
                        break;
                    }
                },
            }
        }

        // Leave only if this connection is still the device's current one (not replaced by a newer one).
        if let Ok(mut online) = self.online.lock()
            && online
                .get(&key)
                .is_some_and(|connection| connection.id == id)
        {
            online.remove(&key);
            drop(online);
            // An agent that leaves takes its help requests and grants with it.
            if identity.role == Role::Agent {
                self.close_help(&identity.device_id);
            }
        }
        self.push_devices(identity.user_id);
        Ok(())
    }

    /// Wake-on-LAN for an offline device of the account: another agent of the account that connects from the
    /// same address (so most likely the same network) sends the magic packets.
    fn wake(&self, identity: &Identity, device_id: &str) -> Result<String, String> {
        let not_ours = || format!("{device_id} is not a device of this account");
        let store = self.store.lock().map_err(|e| e.to_string())?;
        let target = store.wake_info(device_id)?.ok_or_else(not_ours)?;
        let (name, macs, address) = (target.name, target.macs, target.address);
        if target.user_id != identity.user_id {
            return Err(not_ours());
        }
        let online = self.online.lock().map_err(|e| e.to_string())?;
        if online.contains_key(&(device_id.to_string(), Role::Agent)) {
            return Err(format!("{name} is online already"));
        }
        if macs.is_empty() {
            return Err(format!(
                "{name} has not told its network cards yet (it must have run Queqiao once)"
            ));
        }
        let helper = online.iter().find(|((device, role), connection)| {
            *role == Role::Agent
                && connection.user_id == identity.user_id
                && device != device_id
                && store
                    .wake_info(device)
                    .ok()
                    .flatten()
                    .is_some_and(|there| !address.is_empty() && there.address == address)
        });
        match helper {
            Some((_, connection)) => {
                let request = ServerMessage::WakeRequest {
                    name: name.clone(),
                    macs,
                };
                let _ = connection.sender.send(Outgoing::Message(request));
                Ok(format!(
                    "{} sends the wake-up packet to {name}",
                    connection.device_name
                ))
            }
            None => Err(format!(
                "no other device of this account is on {name}'s network right now (waking needs one there)"
            )),
        }
    }

    fn close_help(&self, device_id: &str) {
        if let Ok(mut help) = self.help.lock() {
            help.remove(device_id);
        }
        // An ask waiting for this PC gets its answer: no.
        if let Some(ask) = self
            .asks
            .lock()
            .ok()
            .and_then(|mut asks| asks.remove(device_id))
        {
            let message = "that PC stopped taking help requests".into();
            let _ = ask
                .viewer_sender
                .send(Outgoing::Message(ServerMessage::Error { message }));
        }
        if let Ok(mut grants) = self.grants.lock() {
            grants.retain(|(_, agent)| agent != device_id);
        }
    }

    /// The person's answer to an ask without a code: yes grants as a right code would (and uses up the help
    /// request); no tells the viewer.
    fn answer_help(&self, identity: &Identity, request: u64, allow: bool) {
        let ask = self.asks.lock().ok().and_then(|mut asks| {
            asks.get(&identity.device_id)
                .is_some_and(|ask| ask.request == request)
                .then(|| asks.remove(&identity.device_id))
                .flatten()
        });
        let Some(ask) = ask else {
            return;
        };
        if !allow {
            let message = "the person at that PC declined".into();
            let _ = ask
                .viewer_sender
                .send(Outgoing::Message(ServerMessage::Error { message }));
            return;
        }
        if let Ok(mut help) = self.help.lock() {
            help.remove(&identity.device_id);
        }
        if let Ok(mut grants) = self.grants.lock() {
            grants.insert((ask.viewer_device.clone(), identity.device_id.clone()));
        }
        let (viewer_key, agent_key) = self
            .store
            .lock()
            .ok()
            .map(|store| {
                (
                    store.public_key(&ask.viewer_device).unwrap_or_default(),
                    store.public_key(&identity.device_id).unwrap_or_default(),
                )
            })
            .unwrap_or_default();
        if let Some(agent) = self.online.lock().ok().and_then(|online| {
            online
                .get(&(identity.device_id.clone(), Role::Agent))
                .map(|c| c.sender.clone())
        }) {
            let _ = agent.send(Outgoing::Message(ServerMessage::HelpRequested {
                device: Device {
                    id: ask.viewer_device.clone(),
                    name: ask.viewer_name.clone(),
                    controllable: false,
                    online: true,
                    public_key: viewer_key,
                },
                user: ask.viewer_user.clone(),
            }));
        }
        let _ = ask
            .viewer_sender
            .send(Outgoing::Message(ServerMessage::HelpGranted {
                device: Device {
                    id: identity.device_id.clone(),
                    name: identity.device_name.clone(),
                    controllable: true,
                    online: true,
                    public_key: agent_key,
                },
            }));
    }

    /// Whether `from` may signal `to` across accounts (a help grant in either direction).
    fn granted(&self, from: &str, to: &str) -> bool {
        self.grants.lock().is_ok_and(|grants| {
            grants.contains(&(from.to_string(), to.to_string()))
                || grants.contains(&(to.to_string(), from.to_string()))
        })
    }

    /// A viewer's help request: the right code for an agent taking requests grants signalling between the two
    /// (the code is then used up); wrong codes count against the address and the agent (5 close its requests).
    /// Unknown IDs and wrong codes get the same answer.
    fn request_help(
        &self,
        identity: &Identity,
        help_id: &str,
        code: &str,
        sender: &UnboundedSender<Outgoing>,
        address: Option<IpAddr>,
    ) {
        let refuse = |message: &str| {
            let _ = sender.send(Outgoing::Message(ServerMessage::Error {
                message: message.to_string(),
            }));
        };
        if identity.role != Role::Viewer {
            return refuse("only a viewer can ask to help");
        }
        if let Some(minutes) = self.locked(address, None) {
            return refuse(&format!("too many wrong tries: try again in {minutes} min"));
        }
        let wrong = "wrong help ID or code";
        let target = self
            .store
            .lock()
            .ok()
            .and_then(|store| store.device_by_help_id(help_id.trim()).ok().flatten());
        let Some((agent_id, agent_user, agent_name, agent_key)) = target else {
            self.record_failure(address, None);
            return refuse(wrong);
        };
        let agent_sender = self.online.lock().ok().and_then(|online| {
            online
                .get(&(agent_id.clone(), Role::Agent))
                .filter(|c| c.user_id == agent_user)
                .map(|c| c.sender.clone())
        });
        // No code: ask the person there (only while they take help requests; one ask at a time).
        if code.trim().is_empty() {
            let open = self
                .help
                .lock()
                .is_ok_and(|help| help.contains_key(&agent_id));
            let Some(agent_sender) = agent_sender.filter(|_| open) else {
                // Only refused asks count against the address (a real ask is not a guess).
                self.record_failure(address, None);
                return refuse(wrong);
            };
            let Ok(mut asks) = self.asks.lock() else {
                return refuse("server error");
            };
            if asks.contains_key(&agent_id) {
                return refuse("that PC is answering another request: try again in a minute");
            }
            let request = self.next_id.fetch_add(1, Ordering::Relaxed);
            let viewer_key = self
                .store
                .lock()
                .ok()
                .and_then(|store| store.public_key(&identity.device_id).ok())
                .unwrap_or_default();
            asks.insert(
                agent_id.clone(),
                PendingAsk {
                    request,
                    viewer_device: identity.device_id.clone(),
                    viewer_name: identity.device_name.clone(),
                    viewer_user: identity.user.clone(),
                    viewer_sender: sender.clone(),
                },
            );
            let _ = agent_sender.send(Outgoing::Message(ServerMessage::HelpAsked {
                request,
                device: Device {
                    id: identity.device_id.clone(),
                    name: identity.device_name.clone(),
                    controllable: false,
                    online: true,
                    public_key: viewer_key,
                },
                user: identity.user.clone(),
            }));
            // The viewer hears when the person answers.
            let _ = (agent_name, agent_key);
            return;
        }
        let hash: [u8; 32] = Sha256::digest(code.trim().as_bytes()).into();
        let verdict = {
            let Ok(mut help) = self.help.lock() else {
                return refuse("server error");
            };
            match (help.get_mut(&agent_id), &agent_sender) {
                (Some(offer), Some(_)) => {
                    // Compared in constant time.
                    let same = offer
                        .code_hash
                        .iter()
                        .zip(hash)
                        .fold(0u8, |acc, (a, b)| acc | (a ^ b))
                        == 0;
                    if same {
                        help.remove(&agent_id);
                        Ok(())
                    } else {
                        offer.wrong += 1;
                        let close = offer.wrong >= HELP_FAILURES;
                        if close {
                            help.remove(&agent_id);
                        }
                        Err(close)
                    }
                }
                _ => Err(false),
            }
        };
        match verdict {
            Ok(()) => {
                if let Ok(mut grants) = self.grants.lock() {
                    grants.insert((identity.device_id.clone(), agent_id.clone()));
                }
                let viewer_key = self
                    .store
                    .lock()
                    .ok()
                    .and_then(|store| store.public_key(&identity.device_id).ok())
                    .unwrap_or_default();
                if let Some(agent_sender) = agent_sender {
                    let _ = agent_sender.send(Outgoing::Message(ServerMessage::HelpRequested {
                        device: Device {
                            id: identity.device_id.clone(),
                            name: identity.device_name.clone(),
                            controllable: false,
                            online: true,
                            public_key: viewer_key,
                        },
                        user: identity.user.clone(),
                    }));
                }
                let _ = sender.send(Outgoing::Message(ServerMessage::HelpGranted {
                    device: Device {
                        id: agent_id,
                        name: agent_name,
                        controllable: true,
                        online: true,
                        public_key: agent_key,
                    },
                }));
            }
            Err(closed) => {
                self.record_failure(address, None);
                if closed && let Some(agent_sender) = agent_sender {
                    let reason = format!(
                        "{HELP_FAILURES} wrong codes were tried: open help again for a new code"
                    );
                    let _ =
                        agent_sender.send(Outgoing::Message(ServerMessage::HelpClosed { reason }));
                }
                refuse(wrong);
            }
        }
    }

    /// Minutes until `address` or `account` may try again, if either is locked out.
    fn locked(&self, address: Option<IpAddr>, account: Option<&str>) -> Option<u64> {
        let failures = self.failures.lock().ok()?;
        let wait = |window: Option<&Window>, limit: u32, length: Duration| {
            window
                .filter(|w| w.count >= limit && w.since.elapsed() < length)
                .map(|w| (length - w.since.elapsed()).as_secs().div_ceil(60).max(1))
        };
        let by_address = address.and_then(|a| {
            wait(
                failures.by_address.get(&a),
                ADDRESS_FAILURES,
                ADDRESS_WINDOW,
            )
        });
        let by_account = account
            .and_then(|a| wait(failures.by_account.get(a), ACCOUNT_FAILURES, ACCOUNT_WINDOW));
        by_address.max(by_account)
    }

    fn record_failure(&self, address: Option<IpAddr>, account: Option<&str>) {
        let Ok(mut failures) = self.failures.lock() else {
            return;
        };
        let count = |window: &mut Window, length: Duration| {
            if window.since.elapsed() >= length {
                *window = Window {
                    count: 0,
                    since: Instant::now(),
                };
            }
            window.count += 1;
        };
        let fresh = || Window {
            count: 0,
            since: Instant::now(),
        };
        if let Some(address) = address {
            count(
                failures.by_address.entry(address).or_insert_with(fresh),
                ADDRESS_WINDOW,
            );
        }
        if let Some(account) = account {
            count(
                failures
                    .by_account
                    .entry(account.to_string())
                    .or_insert_with(fresh),
                ACCOUNT_WINDOW,
            );
        }
        // Old entries go, so the maps stay small.
        failures
            .by_address
            .retain(|_, w| w.since.elapsed() < ADDRESS_WINDOW);
        failures
            .by_account
            .retain(|_, w| w.since.elapsed() < ACCOUNT_WINDOW);
    }

    /// Checks the current password, sets the new one, revokes every saved login of the account, gives this
    /// connection a new one and closes the account's other connections.
    async fn change_password(
        self: Arc<Hub>,
        identity: &Identity,
        old_password: String,
        new_password: String,
        sender: &UnboundedSender<Outgoing>,
    ) {
        let hub = self.clone();
        let (user_id, user, device_id, role) = (
            identity.user_id,
            identity.user.clone(),
            identity.device_id.clone(),
            identity.role,
        );
        let changed = tokio::task::spawn_blocking(move || -> Result<String, String> {
            let store = hub.store.lock().map_err(|e| e.to_string())?;
            store.change_password(user_id, &user, &old_password, &new_password)?;
            store.issue_refresh_token(user_id, &device_id, role_name(role))
        })
        .await
        .unwrap_or_else(|e| Err(e.to_string()));
        match changed {
            Ok(refresh_token) => {
                let _ = sender.send(Outgoing::Message(ServerMessage::PasswordChanged {
                    refresh_token,
                }));
                let this = (identity.device_id.clone(), identity.role);
                self.disconnect_account(
                    identity.user_id,
                    Some(&this),
                    "the account's password was changed: log in again",
                );
            }
            Err(message) => {
                if message.contains("current password is wrong") {
                    self.record_failure(None, Some(&identity.user.to_lowercase()));
                }
                let _ = sender.send(Outgoing::Message(ServerMessage::Error { message }));
            }
        }
    }

    /// Tells the account's connections (all but `except`) `message`.
    fn notify_account(&self, user_id: i64, except: Option<&(String, Role)>, message: &str) {
        if let Ok(online) = self.online.lock() {
            for (key, connection) in online.iter() {
                if connection.user_id == user_id && Some(key) != except {
                    let notice = ServerMessage::Notice {
                        message: message.to_string(),
                    };
                    let _ = connection.sender.send(Outgoing::Message(notice));
                }
            }
        }
    }

    /// Closes the account's connections (all but `except`), telling them why.
    fn disconnect_account(&self, user_id: i64, except: Option<&(String, Role)>, reason: &str) {
        self.disconnect(
            |key, connection| connection.user_id == user_id && Some(key) != except,
            reason,
        );
    }

    fn disconnect(&self, which: impl Fn(&(String, Role), &Connection) -> bool, reason: &str) {
        if let Ok(online) = self.online.lock() {
            for (key, connection) in online.iter() {
                if which(key, connection) {
                    let error = ServerMessage::Error {
                        message: reason.to_string(),
                    };
                    let _ = connection.sender.send(Outgoing::Message(error));
                    let _ = connection.sender.send(Outgoing::Kick);
                }
            }
        }
    }

    /// Minutes until `address` may make another account (None: now).
    fn signup_wait(&self, address: Option<IpAddr>) -> Option<u64> {
        let address = address?;
        let mut signups = self.signups.lock().ok()?;
        let recent = signups.entry(address).or_default();
        recent.retain(|at| at.elapsed() < Duration::from_secs(3600));
        if recent.len() >= SIGNUPS_PER_HOUR {
            let oldest = recent.iter().min().copied()?;
            return Some(((3600 - oldest.elapsed().as_secs()) / 60).max(1));
        }
        None
    }

    /// An account was made from `address` (only made ones count: a refused attempt costs little and makes nothing).
    fn record_signup(&self, address: Option<IpAddr>) {
        if let (Some(address), Ok(mut signups)) = (address, self.signups.lock()) {
            signups.entry(address).or_default().push(Instant::now());
        }
    }

    fn authenticate(
        &self,
        first: ClientMessage,
    ) -> Result<(Identity, Option<String>, bool), Refusal> {
        let store = self.store.lock().map_err(|e| e.to_string())?;
        if let ClientMessage::Register {
            user,
            password,
            device_id,
            device_name,
            role,
            public_key,
        } = first
        {
            if !self.signup {
                return Err(
                    "this server does not take new accounts: ask whoever runs it"
                        .to_string()
                        .into(),
                );
            }
            // Everything cheap first: nothing is made (and no password hashed) for an attempt that cannot succeed.
            check_new_account(&user, &password)?;
            if store.device_owner(&device_id)?.is_some() {
                return Err("this device belongs to another account: remove it there first (More... in that account's device list)".to_string().into());
            }
            if store.user_exists(&user)? {
                return Err("that user name is taken".to_string().into());
            }
            store.add_user(&user, &password).map_err(|error| {
                if error.contains("UNIQUE") {
                    "that user name is taken".to_string()
                } else {
                    error
                }
            })?;
            drop(store);
            // Logged in as with a password.
            return self.authenticate(ClientMessage::Login {
                user,
                password,
                device_id,
                device_name,
                role,
                public_key,
            });
        }
        match first {
            ClientMessage::Login {
                user,
                password,
                device_id,
                device_name,
                role,
                public_key,
            } => {
                let row = store
                    .check_password(&user, &password)?
                    .ok_or_else(|| guess("wrong user name or password"))?;
                check_device_fields(&device_id, &device_name)?;
                let new_device =
                    store.register_device(row.id, &device_id, &device_name, &public_key, true)?;
                let token = store.issue_refresh_token(row.id, &device_id, role_name(role))?;
                let identity = Identity {
                    user_id: row.id,
                    user: row.name,
                    device_id,
                    device_name,
                    role,
                };
                Ok((identity, Some(token), new_device))
            }
            ClientMessage::Resume {
                refresh_token,
                device_id,
                device_name,
                role,
                public_key,
            } => {
                let row = store
                    .check_refresh_token(&refresh_token, &device_id, role_name(role))?
                    .ok_or_else(|| {
                        guess("the saved login has expired or was revoked: log in again")
                    })?;
                check_device_fields(&device_id, &device_name)?;
                let new_device =
                    store.register_device(row.id, &device_id, &device_name, &public_key, false)?;
                let identity = Identity {
                    user_id: row.id,
                    user: row.name,
                    device_id,
                    device_name,
                    role,
                };
                Ok((identity, None, new_device))
            }
            _ => Err("log in first".into()),
        }
    }

    fn handle(
        &self,
        identity: &Identity,
        message: ClientMessage,
        sender: &UnboundedSender<Outgoing>,
        address: Option<IpAddr>,
    ) {
        match message {
            ClientMessage::Signal {
                to,
                to_role,
                session,
                kind,
                body,
                signature,
            } => {
                let delivered = self.online.lock().ok().and_then(|online| {
                    let target = online.get(&(to.clone(), to_role))?;
                    // Between the devices of one account, or two that a help code joined.
                    (target.user_id == identity.user_id || self.granted(&identity.device_id, &to))
                        .then(|| {
                            target
                                .sender
                                .send(Outgoing::Message(ServerMessage::Signal {
                                    from: identity.device_id.clone(),
                                    from_name: identity.device_name.clone(),
                                    from_role: identity.role,
                                    session,
                                    kind,
                                    body,
                                    signature,
                                }))
                                .is_ok()
                        })
                });
                // The same answer whether the device is offline or another account's (nothing to learn).
                if delivered != Some(true) {
                    let message = format!("device {to} is not online");
                    let _ = sender.send(Outgoing::Message(ServerMessage::Error { message }));
                }
            }
            ClientMessage::GetIceServers => {
                // Relay credentials only ever go to a logged-in device, for itself.
                let (stun, turn) = match &self.turn {
                    Some(config) => {
                        let (username, password, expires) = config.credentials(&identity.device_id);
                        let access = TurnAccess {
                            address: config.address(),
                            username,
                            password,
                            expires,
                        };
                        (Some(config.address()), Some(access))
                    }
                    None => (None, None),
                };
                let _ = sender.send(Outgoing::Message(ServerMessage::IceServers { stun, turn }));
            }
            ClientMessage::RemoveDevice { device_id } => {
                let removed = self
                    .store
                    .lock()
                    .ok()
                    .map(|store| store.remove_device(identity.user_id, &device_id));
                if let Some(Ok(true)) = removed {
                    let user_id = identity.user_id;
                    self.disconnect(
                        |(device, _), connection| {
                            connection.user_id == user_id && *device == device_id
                        },
                        "this device was removed from the account",
                    );
                    self.push_devices(identity.user_id);
                } else {
                    let message = format!("{device_id} is not a device of this account");
                    let _ = sender.send(Outgoing::Message(ServerMessage::Error { message }));
                }
            }
            ClientMessage::ChangePassword { .. } => {}
            ClientMessage::OpenHelp { code } => {
                let code = code.trim();
                let fits = (6..=32).contains(&code.len())
                    && code.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
                if identity.role != Role::Agent || !fits {
                    let message =
                        "help needs the agent and a code of 6 to 32 letters or digits".into();
                    let _ = sender.send(Outgoing::Message(ServerMessage::Error { message }));
                    return;
                }
                let help_id = self
                    .store
                    .lock()
                    .ok()
                    .map(|store| store.help_id(&identity.device_id));
                match help_id {
                    Some(Ok(help_id)) => {
                        if let Ok(mut help) = self.help.lock() {
                            help.insert(
                                identity.device_id.clone(),
                                HelpOffer {
                                    code_hash: Sha256::digest(code.as_bytes()).into(),
                                    wrong: 0,
                                },
                            );
                        }
                        let _ = sender.send(Outgoing::Message(ServerMessage::HelpOpen { help_id }));
                    }
                    _ => {
                        let message = "no help ID for this device".into();
                        let _ = sender.send(Outgoing::Message(ServerMessage::Error { message }));
                    }
                }
            }
            ClientMessage::CloseHelp => self.close_help(&identity.device_id),
            ClientMessage::HelpAnswer { request, allow } => {
                self.answer_help(identity, request, allow)
            }
            ClientMessage::DeviceInfo { macs } => {
                let valid = macs.len() <= 16 && macs.iter().all(|mac| is_mac(mac));
                let stored = valid
                    && self
                        .store
                        .lock()
                        .is_ok_and(|store| store.set_macs(&identity.device_id, &macs).is_ok());
                if !stored {
                    let message = "bad device information".into();
                    let _ = sender.send(Outgoing::Message(ServerMessage::Error { message }));
                }
            }
            ClientMessage::Wake { device_id } => {
                let reply = match self.wake(identity, &device_id) {
                    Ok(message) => ServerMessage::Notice { message },
                    Err(message) => ServerMessage::Error { message },
                };
                let _ = sender.send(Outgoing::Message(reply));
            }
            ClientMessage::RequestHelp { help_id, code } => {
                self.request_help(identity, &help_id, &code, sender, address)
            }
            ClientMessage::Logout => {
                if let Ok(store) = self.store.lock() {
                    let _ = store.revoke_token(&identity.device_id, role_name(identity.role));
                }
            }
            ClientMessage::Login { .. }
            | ClientMessage::Resume { .. }
            | ClientMessage::Register { .. } => {
                let message = "already logged in".into();
                let _ = sender.send(Outgoing::Message(ServerMessage::Error { message }));
            }
        }
    }

    fn device_list(&self, user_id: i64) -> Vec<Device> {
        let rows = self
            .store
            .lock()
            .ok()
            .and_then(|store| store.devices(user_id).ok())
            .unwrap_or_default();
        let online = match self.online.lock() {
            Ok(online) => online,
            Err(_) => return Vec::new(),
        };
        rows.into_iter()
            .map(|(id, name, public_key)| {
                let agent = online
                    .get(&(id.clone(), Role::Agent))
                    .filter(|c| c.user_id == user_id);
                let viewer = online
                    .get(&(id.clone(), Role::Viewer))
                    .filter(|c| c.user_id == user_id);
                let name = agent.or(viewer).map_or(name, |c| c.device_name.clone());
                Device {
                    controllable: agent.is_some(),
                    online: agent.is_some() || viewer.is_some(),
                    id,
                    name,
                    public_key,
                }
            })
            .collect()
    }

    /// Sends the account's device list to all its connected programs.
    fn push_devices(&self, user_id: i64) {
        let devices = self.device_list(user_id);
        if let Ok(online) = self.online.lock() {
            for connection in online.values().filter(|c| c.user_id == user_id) {
                let devices = devices.clone();
                let _ = connection
                    .sender
                    .send(Outgoing::Message(ServerMessage::Devices { devices }));
            }
        }
    }
}

fn role_name(role: Role) -> &'static str {
    match role {
        Role::Agent => "agent",
        Role::Viewer => "viewer",
    }
}

/// "AA-BB-CC-DD-EE-FF" (or with colons): six hex pairs.
fn is_mac(text: &str) -> bool {
    let parts: Vec<&str> = text.split(['-', ':']).collect();
    parts.len() == 6
        && parts
            .iter()
            .all(|part| part.len() == 2 && part.chars().all(|c| c.is_ascii_hexdigit()))
}

fn check_device_fields(device_id: &str, device_name: &str) -> Result<(), String> {
    let id_ok = (8..=64).contains(&device_id.len())
        && device_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-');
    if !id_ok || device_name.trim().is_empty() || device_name.len() > 100 {
        return Err("bad device id or name".into());
    }
    Ok(())
}
