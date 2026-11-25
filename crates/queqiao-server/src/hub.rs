//! Connections: TLS, then a WebSocket, then a login or resume within 15 s. Each connected program (agent
//! or viewer of a device) gets a channel; the hub forwards signals between the programs of one account only
//! and pushes the account's device list whenever a program connects or leaves.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use queqiao_core::account::{ClientMessage, Device, Role, ServerMessage, TurnAccess};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Notify;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
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
/// wrong passwords per account name from one address and from all together (counted the same way for names that
/// do not exist, so a refusal says nothing about whether an account exists). A valid saved login is never held up:
/// not by its address's lockout, nor by its account's.
const ADDRESS_FAILURES: u32 = 20;
const ADDRESS_WINDOW: Duration = Duration::from_secs(600);
const ACCOUNT_FAILURES: u32 = 5;
const ACCOUNT_TOTAL_FAILURES: u32 = 50;
const ACCOUNT_WINDOW: Duration = Duration::from_secs(900);
/// Wrong help codes for one device before its help requests close (the person opens them again for a new code).
const HELP_FAILURES: u32 = 5;

/// A viewer's ask to help without a code, waiting for the person at the agent's PC.
struct PendingAsk {
    request: u64,
    viewer_device: String,
    viewer_name: String,
    viewer_user: String,
    viewer_sender: Outbox,
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
    /// Per account and address (ACCOUNT_FAILURES): one address cannot shut an account's password logins for everyone
    /// else (the security review of 2026-11-22: 5 tries anywhere did).
    by_account: HashMap<(String, Option<IpAddr>), Window>,
    /// Per account from all addresses together (ACCOUNT_TOTAL_FAILURES): guessing from many.
    by_account_total: HashMap<String, Window>,
}

impl Failures {
    /// Counts a try for `address` and `account` before it is checked: Err(minutes until either may try again) when
    /// one of them has none left (ADDRESS_FAILURES in ADDRESS_WINDOW, ACCOUNT_FAILURES in ACCOUNT_WINDOW).
    fn begin(
        &mut self,
        now: Instant,
        address: Option<IpAddr>,
        account: Option<&str>,
    ) -> Result<(), u64> {
        if let Some(minutes) = self.wait(now, address, account) {
            return Err(minutes);
        }
        self.count(now, address, account);
        Ok(())
    }

    /// Minutes until `address` or `account` may try again, when either has no tries left. Never panics (a panic
    /// with the lock held would poison it, and every limit would be gone).
    fn wait(
        &mut self,
        now: Instant,
        address: Option<IpAddr>,
        account: Option<&str>,
    ) -> Option<u64> {
        self.by_address
            .retain(|_, w| now.saturating_duration_since(w.since) < ADDRESS_WINDOW);
        self.by_account
            .retain(|_, w| now.saturating_duration_since(w.since) < ACCOUNT_WINDOW);
        self.by_account_total
            .retain(|_, w| now.saturating_duration_since(w.since) < ACCOUNT_WINDOW);
        let wait = |window: Option<&Window>, limit: u32, length: Duration| {
            let window = window.filter(|w| w.count >= limit)?;
            let left = length.saturating_sub(now.saturating_duration_since(window.since));
            Some(left.as_secs().div_ceil(60).max(1))
        };
        let by_address =
            address.and_then(|a| wait(self.by_address.get(&a), ADDRESS_FAILURES, ADDRESS_WINDOW));
        let by_account = account.and_then(|a| {
            wait(
                self.by_account.get(&(a.to_string(), address)),
                ACCOUNT_FAILURES,
                ACCOUNT_WINDOW,
            )
        });
        let by_total = account.and_then(|a| {
            wait(
                self.by_account_total.get(a),
                ACCOUNT_TOTAL_FAILURES,
                ACCOUNT_WINDOW,
            )
        });
        by_address.max(by_account).max(by_total)
    }

    /// One more try for `address` and `account`.
    fn count(&mut self, now: Instant, address: Option<IpAddr>, account: Option<&str>) {
        let fresh = || Window {
            count: 0,
            since: now,
        };
        if let Some(address) = address {
            self.by_address.entry(address).or_insert_with(fresh).count += 1;
        }
        if let Some(account) = account {
            self.by_account
                .entry((account.to_string(), address))
                .or_insert_with(fresh)
                .count += 1;
            self.by_account_total
                .entry(account.to_string())
                .or_insert_with(fresh)
                .count += 1;
        }
    }

    /// Takes back a try begin counted: it was `right` (the account starts again), or no guess at all.
    fn forgive(&mut self, address: Option<IpAddr>, account: Option<&str>, right: bool) {
        if let Some(window) = address.and_then(|a| self.by_address.get_mut(&a)) {
            window.count = window.count.saturating_sub(1);
        }
        if let Some(account) = account {
            let key = (account.to_string(), address);
            if right {
                self.by_account.remove(&key);
            } else if let Some(window) = self.by_account.get_mut(&key) {
                window.count = window.count.saturating_sub(1);
            }
            // The total does not start again with a right one: tries from elsewhere still count.
            if let Some(window) = self.by_account_total.get_mut(account) {
                window.count = window.count.saturating_sub(1);
            }
        }
    }
}

/// What a login tells the account's other devices.
enum DeviceNews {
    Known,
    New,
    KeyChanged,
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

/// The address the limits count by (failures, new accounts, connections waiting, help asks), also kept for
/// Wake-on-LAN: an IPv6 address by its /64, since one network hands out endless addresses of it (the security review
/// of 2026-11-22), and IPv4 written as IPv6 as IPv4.
fn network_of(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => {
                let [a, b, c, d, ..] = v6.segments();
                IpAddr::V6(Ipv6Addr::new(a, b, c, d, 0, 0, 0, 0))
            }
        },
        v4 => v4,
    }
}

/// To a connection's task: a message for its client, or the order to close (a newer connection of the
/// same device and program took its place).
enum Outgoing {
    Message(ServerMessage),
    Kick,
}

/// How long a help grant lets the helper start a session (the security review of 2026-11-22: grants stayed).
const GRANT_LIFETIME: Duration = Duration::from_secs(300);
/// After a no, the same viewer may ask that PC again only after this.
const ASK_AFTER_NO: Duration = Duration::from_secs(30);
/// Requests without a code: at most ASKS_PER_WINDOW to one PC in ASK_WINDOW.
const ASKS_PER_WINDOW: usize = 6;
const ASK_WINDOW: Duration = Duration::from_secs(600);

/// Most bytes waiting to go out to one connection: one that does not read them is closed, not given more memory (the
/// security review of 2026-11-22: everything sent to a device that had stopped reading was kept, without limit).
const MAX_QUEUED: usize = 4 << 20;
/// How long one message may take to go out before the connection is given up (a stalled reader held it forever).
const SEND_TIMEOUT: Duration = Duration::from_secs(10);

/// What waits to go out to one connection: already as text, its bytes counted.
enum Queued {
    Text(String),
    Kick,
}

/// The way to one connection: messages queue as text, MAX_QUEUED bytes at most; past that the connection is closed
/// (`overflow`, which needs no room in the queue).
#[derive(Clone)]
struct Outbox {
    sender: UnboundedSender<Queued>,
    queued: Arc<AtomicUsize>,
    overflow: Arc<Notify>,
}

impl Outbox {
    fn new() -> (Outbox, UnboundedReceiver<Queued>) {
        let (sender, receiver) = unbounded_channel();
        let outbox = Outbox {
            sender,
            queued: Arc::new(AtomicUsize::new(0)),
            overflow: Arc::new(Notify::new()),
        };
        (outbox, receiver)
    }

    /// Queues `item`; with MAX_QUEUED bytes waiting already, the connection is closed instead.
    fn send(&self, item: Outgoing) -> Result<(), ()> {
        match item {
            Outgoing::Kick => self.sender.send(Queued::Kick).map_err(|_| ()),
            Outgoing::Message(message) => {
                let text = serde_json::to_string(&message).map_err(|_| ())?;
                let size = text.len();
                if self.queued.fetch_add(size, Ordering::Relaxed) + size > MAX_QUEUED {
                    self.queued.fetch_sub(size, Ordering::Relaxed);
                    self.overflow.notify_one();
                    return Err(());
                }
                self.sender.send(Queued::Text(text)).map_err(|_| ())
            }
        }
    }

    /// `size` bytes went out (or were dropped with the connection).
    fn sent(&self, size: usize) {
        self.queued.fetch_sub(size, Ordering::Relaxed);
    }
}

/// Connections not logged in yet: at most MAX_WAITING, and MAX_WAITING_PER_ADDRESS from one address (each may take
/// 35 s before it logs in: without a limit, idle connections used up the server's sockets).
const MAX_WAITING: usize = 2048;
const MAX_WAITING_PER_ADDRESS: u32 = 32;

#[derive(Default)]
struct Waiting {
    total: usize,
    by_address: HashMap<IpAddr, u32>,
}

impl Waiting {
    /// Whether a connection from `address` may wait to log in (counted if so).
    fn enter(&mut self, address: IpAddr) -> bool {
        let from_there = self.by_address.entry(address).or_default();
        if self.total >= MAX_WAITING || *from_there >= MAX_WAITING_PER_ADDRESS {
            return false;
        }
        *from_there += 1;
        self.total += 1;
        true
    }

    fn leave(&mut self, address: IpAddr) {
        if let Some(from_there) = self.by_address.get_mut(&address) {
            *from_there = from_there.saturating_sub(1);
            if *from_there == 0 {
                self.by_address.remove(&address);
            }
        }
        self.total = self.total.saturating_sub(1);
    }
}

/// One connection counted in Waiting until it logs in or ends.
struct WaitingTurn {
    hub: Arc<Hub>,
    address: IpAddr,
}

impl Drop for WaitingTurn {
    fn drop(&mut self) {
        self.hub
            .waiting
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .leave(self.address);
    }
}

struct Connection {
    id: u64,
    user_id: i64,
    device_name: String,
    sender: Outbox,
}

pub struct Hub {
    /// New accounts may be made from the programs' login forms (`serve --allow-signup`).
    signup: bool,
    /// When each address last made accounts (the last hour's).
    signups: Mutex<HashMap<IpAddr, Vec<Instant>>>,
    store: Mutex<Store>,
    failures: Mutex<Failures>,
    waiting: Mutex<Waiting>,
    turn: Option<Arc<TurnConfig>>,
    online: Mutex<HashMap<(String, Role), Connection>>,
    next_id: AtomicU64,
    /// Agents taking help requests, by device id.
    help: Mutex<HashMap<String, HelpOffer>>,
    /// (viewer device, agent device) of different accounts that may signal each other: the viewer gave the
    /// agent's help code. Ends when the agent closes help or leaves.
    /// (viewer, agent) pairs a help code or an allowed ask let in, with when: for GRANT_LIFETIME.
    grants: Mutex<HashMap<(String, String), Instant>>,
    /// Requests without a code per agent (when, which viewer), and the declined ones (viewer, agent): when.
    ask_log: Mutex<HashMap<String, Vec<(Instant, String)>>>,
    declined: Mutex<HashMap<(String, String), Instant>>,
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
            waiting: Mutex::new(Waiting::default()),
            turn,
            online: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            help: Mutex::new(HashMap::new()),
            grants: Mutex::new(HashMap::new()),
            ask_log: Mutex::new(HashMap::new()),
            declined: Mutex::new(HashMap::new()),
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
            // A failed accept (no sockets left, a reset connection) is not the end of the server.
            let (tcp, peer) = match listener.accept().await {
                Ok(accepted) => accepted,
                Err(error) => {
                    eprintln!("accept: {error}");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
            };
            let entered = self
                .waiting
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .enter(network_of(peer.ip()));
            if !entered {
                // Too many connections waiting to log in (from there, or at all): this one is closed at once.
                continue;
            }
            let turn = WaitingTurn {
                hub: self.clone(),
                address: network_of(peer.ip()),
            };
            let hub = self.clone();
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                if let Err(error) = hub.connection(tcp, acceptor, turn).await {
                    eprintln!("{peer}: {error}");
                }
            });
        }
    }

    async fn connection(
        self: Arc<Hub>,
        tcp: TcpStream,
        acceptor: TlsAcceptor,
        waiting: WaitingTurn,
    ) -> Result<(), String> {
        let _ = tcp.set_nodelay(true);
        let address = tcp.peer_addr().ok().map(|peer| network_of(peer.ip()));
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
        // Each try counts before it is checked, so that tries at once on many connections count as many (the security
        // review of 2026-11-22: they were all checked, then counted); a right one, or one that was no guess, is
        // forgiven after. New accounts likewise: at most SIGNUPS_PER_HOUR made from one address.
        // A saved login that is valid logs in whatever the address's lockout (the security review of 2026-11-22: 20
        // wrong passwords from an address that many share, an office's or a school's, cut every PC behind it off).
        // Only one that is not valid counts as a try.
        let valid_resume = match &first {
            ClientMessage::Resume {
                refresh_token,
                device_id,
                role,
                ..
            } => self.resume_valid(refresh_token, device_id, *role),
            _ => false,
        };
        let counted = if valid_resume {
            Ok(())
        } else {
            self.begin_try(address, account_name.as_deref())
        };
        let registering = matches!(first, ClientMessage::Register { .. });
        let signup = match (&counted, registering) {
            (Ok(()), true) => Some(self.begin_signup(address)),
            _ => None,
        };
        // Password hashing takes tens of milliseconds: off the network threads.
        let hub = self.clone();
        let checked = match (counted, signup) {
            (Err(minutes), _) => Err(Refusal::from(format!(
                "too many failed logins: try again in {minutes} minutes"
            ))),
            (Ok(()), Some(Err(minutes))) => {
                self.forgive(address, account_name.as_deref(), false);
                Err(Refusal::from(format!(
                    "too many new accounts from this address: try again in {minutes} minutes"
                )))
            }
            (Ok(()), _) => {
                let checked = tokio::task::spawn_blocking(move || hub.authenticate(first))
                    .await
                    .map_err(|e| e.to_string())?;
                match &checked {
                    // Nothing was counted for it.
                    _ if valid_resume => {}
                    Ok(_) => self.forgive(address, account_name.as_deref(), true),
                    Err(refusal) if !refusal.guess => {
                        self.forgive(address, account_name.as_deref(), false)
                    }
                    Err(_) => {}
                }
                if registering && checked.is_err() {
                    self.cancel_signup(address);
                }
                checked
            }
        };
        let (identity, new_token, new_device) = match checked {
            Ok(result) => result,
            Err(Refusal { message, .. }) => {
                // A failed login waits a little, which slows down guessing.
                tokio::time::sleep(Duration::from_secs(1)).await;
                let reply = serde_json::to_string(&ServerMessage::Error {
                    message: message.clone(),
                })
                .unwrap_or_default();
                let _ = timeout(SEND_TIMEOUT, sink.send(Message::text(reply))).await;
                return Err(format!("login refused: {message}"));
            }
        };

        // Logged in: no longer counted among the connections waiting to.
        drop(waiting);
        // Where the device connects from: Wake-on-LAN looks for another device behind the same address.
        if let (Some(address), Ok(store)) = (address, self.store.lock()) {
            let _ = store.set_last_address(&identity.device_id, &address.to_string());
        }
        let (sender, mut receiver) = Outbox::new();
        let overflow = sender.overflow.clone();
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
        // The account's other devices are told of a new device, or of a known one with a new key.
        let name = identity.device_name.clone();
        match new_device {
            DeviceNews::New => self.notify_account(
                identity.user_id,
                Some(&key),
                ServerMessage::DeviceJoined { name },
            ),
            DeviceNews::KeyChanged => self.notify_account(
                identity.user_id,
                Some(&key),
                ServerMessage::DeviceKeyChanged { name },
            ),
            DeviceNews::Known => {}
        }

        loop {
            tokio::select! {
                incoming = timeout(IDLE, stream.next()) => match incoming {
                    Ok(Some(Ok(Message::Text(text)))) => match serde_json::from_str::<ClientMessage>(&text) {
                        Ok(ClientMessage::ChangePassword { old_password, new_password }) => {
                            self.clone().change_password(&identity, old_password, new_password, &sender, address).await;
                        }
                        Ok(message) => self.handle(&identity, message, &sender, address),
                        Err(error) => {
                            let message = format!("bad message: {error}");
                            let _ = sender.send(Outgoing::Message(ServerMessage::Error { message }));
                        }
                    },
                    Ok(Some(Ok(Message::Ping(data)))) => {
                        if !matches!(timeout(SEND_TIMEOUT, sink.send(Message::Pong(data))).await, Ok(Ok(()))) {
                            break;
                        }
                    }
                    Ok(Some(Ok(Message::Close(_))) | None | Some(Err(_))) | Err(_) => break,
                    Ok(Some(Ok(_))) => {}
                },
                outgoing = receiver.recv() => match outgoing {
                    Some(Queued::Text(text)) => {
                        let size = text.len();
                        let sent = timeout(SEND_TIMEOUT, sink.send(Message::text(text))).await;
                        sender.sent(size);
                        if !matches!(sent, Ok(Ok(()))) {
                            break;
                        }
                    }
                    Some(Queued::Kick) | None => {
                        let _ = timeout(SEND_TIMEOUT, sink.close()).await;
                        break;
                    }
                },
                () = overflow.notified() => {
                    eprintln!("{}: closed: it did not read what was sent to it ({MAX_QUEUED} bytes waiting)", identity.device_id);
                    break;
                }
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
            grants.retain(|(_, agent), _| agent != device_id);
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
            // The same viewer may ask again only after ASK_AFTER_NO (as UU: "too frequent, please wait").
            if let Ok(mut declined) = self.declined.lock() {
                declined.retain(|_, at| at.elapsed() < ASK_AFTER_NO);
                declined.insert(
                    (ask.viewer_device.clone(), identity.device_id.clone()),
                    Instant::now(),
                );
            }
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
            grants.retain(|_, at| at.elapsed() < GRANT_LIFETIME);
            grants.insert(
                (ask.viewer_device.clone(), identity.device_id.clone()),
                Instant::now(),
            );
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

    /// Whether `from` may signal `to` across accounts (a help grant in either direction, for GRANT_LIFETIME: it no
    /// longer stays until the agent goes or closes help, as the security review of 2026-11-22 found).
    fn granted(&self, from: &str, to: &str) -> bool {
        self.grants.lock().is_ok_and(|grants| {
            [(from, to), (to, from)].iter().any(|(a, b)| {
                grants
                    .get(&(a.to_string(), b.to_string()))
                    .is_some_and(|at| at.elapsed() < GRANT_LIFETIME)
            })
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
        sender: &Outbox,
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
            // Not again and again (as UU: "too frequent, please wait"): after a no, the same viewer waits ASK_AFTER_NO;
            // a PC is asked ASKS_PER_WINDOW times in ASK_WINDOW at most.
            let declined_lately = self.declined.lock().is_ok_and(|declined| {
                declined
                    .get(&(identity.device_id.clone(), agent_id.clone()))
                    .is_some_and(|at| at.elapsed() < ASK_AFTER_NO)
            });
            let crowded = self.ask_log.lock().is_ok_and(|mut log| {
                let recent = log.entry(agent_id.clone()).or_default();
                recent.retain(|(at, _)| at.elapsed() < ASK_WINDOW);
                let full = recent.len() >= ASKS_PER_WINDOW;
                if !full && !declined_lately {
                    recent.push((Instant::now(), identity.device_id.clone()));
                }
                full
            });
            if declined_lately || crowded {
                return refuse("too many requests: please wait a moment, then ask again");
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
                    grants.retain(|_, at| at.elapsed() < GRANT_LIFETIME);
                    grants.insert(
                        (identity.device_id.clone(), agent_id.clone()),
                        Instant::now(),
                    );
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

    /// Minutes until `address` may ask again (wrong help IDs and codes count against it; the codes have their own
    /// limit as well, HELP_FAILURES).
    fn locked(&self, address: Option<IpAddr>, account: Option<&str>) -> Option<u64> {
        self.failures
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .wait(Instant::now(), address, account)
    }

    fn record_failure(&self, address: Option<IpAddr>, account: Option<&str>) {
        self.failures
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .count(Instant::now(), address, account);
    }

    /// Whether a saved login is valid: a SHA-256 lookup, no password hashing.
    fn resume_valid(&self, refresh_token: &str, device_id: &str, role: Role) -> bool {
        self.store.lock().is_ok_and(|store| {
            store
                .check_refresh_token(refresh_token, device_id, role_name(role))
                .ok()
                .flatten()
                .is_some()
        })
    }

    /// Counts a try (a password, or a saved login) for `address` and `account` before it is checked (Failures::begin).
    fn begin_try(&self, address: Option<IpAddr>, account: Option<&str>) -> Result<(), u64> {
        self.failures
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .begin(Instant::now(), address, account)
    }

    /// Takes a counted try back: it was right, or no guess (Failures::forgive).
    fn forgive(&self, address: Option<IpAddr>, account: Option<&str>, right: bool) {
        self.failures
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .forgive(address, account, right);
    }

    /// Checks the current password, sets the new one, revokes every saved login of the account, gives this
    /// connection a new one and closes the account's other connections.
    /// The current password counts as a try for the account and the address, as a login does (the security review of
    /// 2026-11-22: a saved login, stolen with one device, could otherwise guess it without limit and take the account).
    async fn change_password(
        self: Arc<Hub>,
        identity: &Identity,
        old_password: String,
        new_password: String,
        sender: &Outbox,
        address: Option<IpAddr>,
    ) {
        let account = identity.user.to_lowercase();
        if let Err(minutes) = self.begin_try(address, Some(&account)) {
            let message = format!("too many wrong passwords: try again in {minutes} minutes");
            let _ = sender.send(Outgoing::Message(ServerMessage::Error { message }));
            return;
        }
        let hub = self.clone();
        let (user_id, user, device_id, role) = (
            identity.user_id,
            identity.user.clone(),
            identity.device_id.clone(),
            identity.role,
        );
        let changed = tokio::task::spawn_blocking(move || -> Result<String, String> {
            if new_password.len() < 8 {
                return Err("the new password needs at least 8 characters".into());
            }
            // Argon2 (the old password checked, the new one hashed) without the store's lock.
            let record = hub
                .store
                .lock()
                .map_err(|e| e.to_string())?
                .password_record(&user)?;
            if record
                .check(&old_password)
                .is_none_or(|row| row.id != user_id)
            {
                return Err("the current password is wrong".into());
            }
            let hash = crate::store::new_password_hash(&new_password)?;
            let store = hub.store.lock().map_err(|e| e.to_string())?;
            store.set_password_hash(user_id, &hash)?;
            store.issue_refresh_token(user_id, &device_id, role_name(role))
        })
        .await
        .unwrap_or_else(|e| Err(e.to_string()));
        let wrong = changed
            .as_ref()
            .is_err_and(|message| message.contains("current password is wrong"));
        if !wrong {
            self.forgive(address, Some(&account), changed.is_ok());
        }
        match changed {
            Ok(refresh_token) => {
                let _ = sender.send(Outgoing::Message(ServerMessage::PasswordChanged {
                    refresh_token,
                }));
                let this = (identity.device_id.clone(), identity.role);
                // The relay credentials of the account's other programs end with their saved logins.
                for device in self.device_list(identity.user_id) {
                    for role in [Role::Agent, Role::Viewer] {
                        if (device.id.as_str(), role)
                            != (identity.device_id.as_str(), identity.role)
                        {
                            self.revoke_relay(&device.id, role);
                        }
                    }
                }
                self.disconnect_account(
                    identity.user_id,
                    Some(&this),
                    "the account's password was changed: log in again",
                );
            }
            Err(message) => {
                if wrong {
                    // As a failed login: slower guessing.
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
                let _ = sender.send(Outgoing::Message(ServerMessage::Error { message }));
            }
        }
    }

    /// The relay credentials `role` of `device` was given stop working (TurnConfig::revoke).
    fn revoke_relay(&self, device: &str, role: Role) {
        if let Some(turn) = &self.turn {
            turn.revoke(device, &[role_name(role)]);
        }
    }

    /// Tells the account's connections (all but `except`) `message`.
    fn notify_account(&self, user_id: i64, except: Option<&(String, Role)>, notice: ServerMessage) {
        if let Ok(online) = self.online.lock() {
            for (key, connection) in online.iter() {
                if connection.user_id == user_id && Some(key) != except {
                    let _ = connection.sender.send(Outgoing::Message(notice.clone()));
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

    /// Takes one of the SIGNUPS_PER_HOUR new accounts `address` may make, before the account is made, so that
    /// attempts at once (on many connections) count as many: Err(minutes to wait) when none is left.
    fn begin_signup(&self, address: Option<IpAddr>) -> Result<(), u64> {
        let Some(address) = address else {
            return Ok(());
        };
        let mut signups = self
            .signups
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let recent = signups.entry(address).or_default();
        recent.retain(|at| at.elapsed() < Duration::from_secs(3600));
        if recent.len() >= SIGNUPS_PER_HOUR {
            let oldest = recent.iter().min().map_or(0, |at| at.elapsed().as_secs());
            return Err((3600u64.saturating_sub(oldest) / 60).max(1));
        }
        recent.push(Instant::now());
        Ok(())
    }

    /// Gives back what begin_signup took: no account was made (everything is checked before one is).
    fn cancel_signup(&self, address: Option<IpAddr>) {
        if let Some(address) = address {
            let mut signups = self
                .signups
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(recent) = signups.get_mut(&address) {
                recent.pop();
            }
        }
    }

    fn authenticate(
        &self,
        first: ClientMessage,
    ) -> Result<(Identity, Option<String>, DeviceNews), Refusal> {
        if let ClientMessage::Register {
            user,
            password,
            device_id,
            device_name,
            role,
            public_key,
            machine,
        } = first
        {
            if !self.signup {
                return Err(
                    "this server does not take new accounts: ask whoever runs it"
                        .to_string()
                        .into(),
                );
            }
            // Everything first: nothing is made (and no password hashed) for an attempt that cannot succeed, and an
            // account once made logs in (the security review of 2026-11-22: a bad device id made the account, then
            // failed, and was not counted as a new account).
            check_new_account(&user, &password)?;
            check_device_fields(&device_id, &device_name)?;
            check_device_key(&public_key)?;
            // Both answers count as tries for the address (the security review of 2026-11-22: they told anyone which
            // names and device ids exist, without limit).
            let taken = |store: &crate::store::Store| -> Result<(), Refusal> {
                if store.device_owner(&device_id)?.is_some() {
                    return Err(guess(
                        "this device belongs to another account: remove it there first (More... in that account's device list)",
                    ));
                }
                if store.user_exists(&user)? {
                    return Err(guess("that user name is taken"));
                }
                Ok(())
            };
            taken(&*self.store.lock().map_err(|e| e.to_string())?)?;
            // Argon2 without the store's lock (the security review of 2026-11-22: every other connection waited for
            // it); looked at again once it is made.
            let hash = crate::store::new_password_hash(&password)?;
            {
                let store = self.store.lock().map_err(|e| e.to_string())?;
                taken(&store)?;
                store.add_user_hashed(&user, &hash).map_err(|error| {
                    if error.contains("UNIQUE") || error.contains("taken") {
                        guess("that user name is taken")
                    } else {
                        Refusal::from(error)
                    }
                })?;
            }
            // Logged in as with a password.
            return self.authenticate(ClientMessage::Login {
                user,
                password,
                device_id,
                device_name,
                role,
                public_key,
                machine,
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
                machine,
            } => {
                // The device's fields, and whether it may log in to this account, before the password: once that is
                // right nothing refuses the login any more, and a new device is announced (the security review of
                // 2026-11-22: a bad field after a right password had its own answer, neither counted nor announced,
                // so a stolen password could be tried unseen).
                check_device_fields(&device_id, &device_name)?;
                check_device_key(&public_key)?;
                // A tag is 64 hex digits (SHA-256) or nothing.
                if !(machine.is_empty() || check_device_key(&machine).is_ok()) {
                    return Err("bad machine tag".to_string().into());
                }
                // Argon2 without the store's lock: the hash read, checked, then the store locked again.
                let record = {
                    let store = self.store.lock().map_err(|e| e.to_string())?;
                    let record = store.password_record(&user)?;
                    store
                        .device_admissible(record.id, &device_id, &device_name, &machine)
                        .map_err(|message| guess(&message))?;
                    record
                };
                let row = record
                    .check(&password)
                    .ok_or_else(|| guess("wrong user name or password"))?;
                let store = self.store.lock().map_err(|e| e.to_string())?;
                // The same PC reinstalled under a new device id: its earlier device goes (with its saved logins), so
                // the list holds each PC once.
                let replaced = store.same_machine(row.id, &device_id, &device_name, &machine)?;
                for old in &replaced {
                    store.remove_device(row.id, old)?;
                }
                // A password login may give a known device a new key: its other devices are told (the security review
                // of 2026-11-22: it happened silently).
                let earlier_key = store.public_key(&device_id).unwrap_or_default();
                let new_device =
                    store.register_device(row.id, &device_id, &device_name, &public_key, true)?;
                if !machine.is_empty() {
                    store.set_machine(&device_id, &machine)?;
                }
                // A reinstalled PC is told to the other devices as a new key of the same PC.
                let news = if new_device && replaced.is_empty() {
                    DeviceNews::New
                } else if new_device
                    || (!earlier_key.is_empty() && !earlier_key.eq_ignore_ascii_case(&public_key))
                {
                    DeviceNews::KeyChanged
                } else {
                    DeviceNews::Known
                };
                let token = store.issue_refresh_token(row.id, &device_id, role_name(role))?;
                drop(store);
                for old in &replaced {
                    for role in [Role::Agent, Role::Viewer] {
                        self.revoke_relay(old, role);
                    }
                    let user_id = row.id;
                    self.disconnect(
                        |(device, _), connection| connection.user_id == user_id && device == old,
                        "this PC was installed again: its new device takes this one's place",
                    );
                }
                let identity = Identity {
                    user_id: row.id,
                    user: row.name,
                    device_id,
                    device_name,
                    role,
                };
                Ok((identity, Some(token), news))
            }
            ClientMessage::Resume {
                refresh_token,
                device_id,
                device_name,
                role,
                public_key,
            } => {
                let store = self.store.lock().map_err(|e| e.to_string())?;
                let row = store
                    .check_refresh_token(&refresh_token, &device_id, role_name(role))?
                    .ok_or_else(|| {
                        guess("the saved login has expired or was revoked: log in again")
                    })?;
                check_device_fields(&device_id, &device_name)?;
                let news = if store.register_device(
                    row.id,
                    &device_id,
                    &device_name,
                    &public_key,
                    false,
                )? {
                    DeviceNews::New
                } else {
                    DeviceNews::Known
                };
                let identity = Identity {
                    user_id: row.id,
                    user: row.name,
                    device_id,
                    device_name,
                    role,
                };
                Ok((identity, None, news))
            }
            _ => Err("log in first".into()),
        }
    }

    fn handle(
        &self,
        identity: &Identity,
        message: ClientMessage,
        sender: &Outbox,
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
                        let (username, password, expires) = config.credentials(
                            identity.user_id,
                            &identity.device_id,
                            role_name(identity.role),
                        );
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
                    for role in [Role::Agent, Role::Viewer] {
                        self.revoke_relay(&device_id, role);
                    }
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
            ClientMessage::RefreshHelpId => {
                // Only an agent has a help ID.
                if identity.role != Role::Agent {
                    return;
                }
                let renewed = self
                    .store
                    .lock()
                    .ok()
                    .map(|store| store.new_help_id(&identity.device_id));
                match renewed {
                    Some(Ok(help_id)) => {
                        // While help is open the agent shows the new ID at once; otherwise the next opening does.
                        let open = self
                            .help
                            .lock()
                            .is_ok_and(|help| help.contains_key(&identity.device_id));
                        if open {
                            let _ =
                                sender.send(Outgoing::Message(ServerMessage::HelpOpen { help_id }));
                        }
                    }
                    _ => {
                        let message = "no new help ID for this device".into();
                        let _ = sender.send(Outgoing::Message(ServerMessage::Error { message }));
                    }
                }
            }
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
                self.revoke_relay(&identity.device_id, identity.role);
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

/// A device's public key as the clients send it: 64 hex digits (Ed25519).
fn check_device_key(public_key: &str) -> Result<(), String> {
    if public_key.len() != 64 || !public_key.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("bad device key".into());
    }
    Ok(())
}

/// Characters that change the direction of text (a right-to-left mark and the like): never in a name.
fn direction_mark(c: char) -> bool {
    matches!(c, '\u{200E}' | '\u{200F}' | '\u{061C}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

fn check_device_fields(device_id: &str, device_name: &str) -> Result<(), String> {
    // Names are shown in prompts and banners: no line breaks or direction marks that could hide what follows (the
    // security review of 2026-11-22).
    if device_name
        .chars()
        .any(|c| c.is_control() || direction_mark(c))
    {
        return Err("bad device id or name".into());
    }
    let id_ok = (8..=64).contains(&device_id.len())
        && device_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-');
    if !id_ok || device_name.trim().is_empty() || device_name.len() > 100 {
        return Err("bad device id or name".into());
    }
    Ok(())
}

#[cfg(test)]
mod failure_tests {
    use super::*;

    #[test]
    fn tries_at_once_count_as_many() {
        let now = Instant::now();
        let address: IpAddr = "198.51.100.7".parse().unwrap();
        // Ten tries for one account at once, as from ten connections: only ACCOUNT_FAILURES are checked.
        let mut failures = Failures::default();
        let results: Vec<_> = (0..10)
            .map(|_| failures.begin(now, Some(address), Some("alice")))
            .collect();
        assert_eq!(
            results.iter().filter(|r| r.is_ok()).count(),
            ACCOUNT_FAILURES as usize
        );
        assert!(results[9].unwrap_err() >= 1);
        // The right password (one of them): the account starts again.
        failures.forgive(Some(address), Some("alice"), true);
        assert!(failures.begin(now, Some(address), Some("alice")).is_ok());
        // Tries that were no guess are taken back, however many.
        let other: IpAddr = "203.0.113.9".parse().unwrap();
        for _ in 0..ADDRESS_FAILURES * 3 {
            assert!(failures.begin(now, Some(other), None).is_ok());
            failures.forgive(Some(other), None, false);
        }
        // One address, many accounts: ADDRESS_FAILURES at most.
        let mut failures = Failures::default();
        let passed = (0..ADDRESS_FAILURES + 5)
            .filter(|i| {
                failures
                    .begin(now, Some(address), Some(&format!("user{i}")))
                    .is_ok()
            })
            .count();
        assert_eq!(passed, ADDRESS_FAILURES as usize);
        // At the window's very end and after it: no panic, and tries again.
        assert!(
            failures
                .wait(now + ADDRESS_WINDOW, Some(address), None)
                .is_none()
        );
        let later = now + ADDRESS_WINDOW + Duration::from_secs(1);
        assert!(failures.begin(later, Some(address), Some("x")).is_ok());
    }

    #[test]
    fn one_address_does_not_lock_an_account_for_the_others() {
        let now = Instant::now();
        let mut failures = Failures::default();
        let attacker: IpAddr = "198.51.100.7".parse().unwrap();
        let owner: IpAddr = "203.0.113.9".parse().unwrap();
        for _ in 0..ACCOUNT_FAILURES {
            assert!(failures.begin(now, Some(attacker), Some("alice")).is_ok());
        }
        assert!(failures.begin(now, Some(attacker), Some("alice")).is_err());
        assert!(
            failures.begin(now, Some(owner), Some("alice")).is_ok(),
            "the owner, elsewhere, may still try"
        );
        // From many addresses together: ACCOUNT_TOTAL_FAILURES at most.
        let mut failures = Failures::default();
        let passed = (0..ACCOUNT_TOTAL_FAILURES * 2)
            .filter(|i| {
                let address = IpAddr::from([10, 0, (*i / 250) as u8, (*i % 250) as u8]);
                failures.begin(now, Some(address), Some("alice")).is_ok()
            })
            .count();
        assert_eq!(passed, ACCOUNT_TOTAL_FAILURES as usize);
    }

    #[test]
    fn ipv6_counts_by_its_network() {
        let a: IpAddr = "2001:db8:1:2:aaaa::1".parse().unwrap();
        let b: IpAddr = "2001:db8:1:2:bbbb::9".parse().unwrap();
        let other: IpAddr = "2001:db8:1:3::1".parse().unwrap();
        assert_eq!(network_of(a), network_of(b));
        assert_ne!(network_of(a), network_of(other));
        let mapped: IpAddr = "::ffff:198.51.100.7".parse().unwrap();
        let v4: IpAddr = "198.51.100.7".parse().unwrap();
        assert_eq!(network_of(mapped), v4);
        assert_eq!(network_of(v4), v4);
        // Endless addresses of one /64 share one count.
        let now = Instant::now();
        let mut failures = Failures::default();
        let passed = (0..ADDRESS_FAILURES * 2)
            .filter(|i| {
                let address = IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 1, 2, 0, 0, 0, *i as u16));
                failures.begin(now, Some(network_of(address)), None).is_ok()
            })
            .count();
        assert_eq!(passed, ADDRESS_FAILURES as usize);
    }
}

#[cfg(test)]
mod outbox_tests {
    use super::*;
    use futures_util::FutureExt;

    #[test]
    fn a_connection_that_does_not_read_is_closed_not_fed() {
        let (outbox, mut receiver) = Outbox::new();
        let big = ServerMessage::Error {
            message: "x".repeat(1 << 20),
        };
        // Four of 1 MB fit (a little over 4 MB with their quotes would not: three and a bit).
        let mut queued = 0;
        while outbox.send(Outgoing::Message(big.clone())).is_ok() {
            queued += 1;
            assert!(queued < 10, "the queue has no end");
        }
        assert!(queued >= 3);
        assert!(outbox.queued.load(Ordering::Relaxed) <= MAX_QUEUED);
        // The overflow closes the connection: its signal is there without room in the queue.
        assert!(outbox.overflow.notified().now_or_never().is_some());
        // What went out makes room again.
        if let Ok(Queued::Text(text)) = receiver.try_recv() {
            outbox.sent(text.len());
        }
        assert!(outbox.send(Outgoing::Message(big)).is_ok());
        // A kick always gets in.
        assert!(outbox.send(Outgoing::Kick).is_ok());
    }

    #[test]
    fn connections_waiting_to_log_in_are_limited() {
        let mut waiting = Waiting::default();
        let one: IpAddr = "198.51.100.7".parse().unwrap();
        let passed = (0..MAX_WAITING_PER_ADDRESS + 10)
            .filter(|_| waiting.enter(one))
            .count();
        assert_eq!(passed, MAX_WAITING_PER_ADDRESS as usize);
        waiting.leave(one);
        assert!(waiting.enter(one));
        // Many addresses: MAX_WAITING in all.
        let mut waiting = Waiting::default();
        let passed = (0..MAX_WAITING as u32 + 100)
            .filter(|i| {
                waiting.enter(IpAddr::from([
                    10,
                    (i >> 16) as u8,
                    (i >> 8) as u8,
                    *i as u8,
                ]))
            })
            .count();
        assert_eq!(passed, MAX_WAITING);
    }
}
