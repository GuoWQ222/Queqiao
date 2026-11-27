//! The client side of queqiao-server: a TLS WebSocket that only accepts the server certificate whose
//! SHA-256 fingerprint was pinned, login or resume, the account's device list, and the signaling of a
//! session ([`SessionSignal`], which carries a WebRTC offer and answer through the server).
//!
//! The connection runs on its own thread ("queqiao-account"); the program talks to it through channels.

mod config;
/// Sealed for the person running this (Windows: DPAPI; Linux: their own key file and ChaCha20-Poly1305): for
/// secrets in settings (a fixed help code).
pub fn protect_secret(data: &[u8]) -> Result<Vec<u8>, String> {
    config::protect(data).map_err(|e| e.to_string())
}

pub fn unprotect_secret(data: &[u8]) -> Result<Vec<u8>, String> {
    config::unprotect(data).map_err(|e| e.to_string())
}

pub mod connections;
#[cfg(target_os = "linux")]
mod keyring;
mod keys;
mod local_password;
mod notes;
mod relay_rtt;
mod standbys;
mod tls;

use std::collections::VecDeque;
use std::net::{IpAddr, SocketAddr};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use queqiao_core::account::{PathReport, PunchSettings, ServerMessage, TurnAccess, signed_bytes};
use queqiao_transport::{Error, IceServers, Interruption, SessionRequest, Signaling, TurnServer};
use tungstenite::Message;

pub use config::{DeviceIdentity, SavedLogin, folder as data_folder};
pub use keys::{DeviceKey, KnownDevices, Pin, verify};
pub use local_password::{check_local_password, hash_local_password};
pub use notes::{DeviceNote, DeviceNotes};
pub use queqiao_core::account::{ClientMessage, Device, Role};

/// What the server tells the program after the welcome.
#[derive(Clone, Debug)]
pub enum AccountEvent {
    Devices(Vec<Device>),
    /// A device the account had not had logged in to it (its name).
    DeviceJoined(String),
    /// A device of the account logged in with a new key (its name).
    DeviceKeyChanged(String),
    Signal {
        from: String,
        from_name: String,
        from_role: Role,
        session: String,
        kind: String,
        body: String,
        /// The sender's signature; check it with [`Account::check_signal`] before acting on the signal.
        signature: String,
    },
    /// The server refused something (for example a signal to a device that is offline).
    Error(String),
    /// Something the person should know (e.g. a new device logged in to the account).
    Notice(String),
    /// The answer to `ChangePassword`: the new refresh token.
    PasswordChanged(String),
    /// The answer to `GetIceServers`.
    IceServers {
        stun: Option<String>,
        turn: Option<TurnAccess>,
        backups: Vec<TurnAccess>,
        punch: Option<PunchSettings>,
    },
    /// Agent: help requests are open under this help ID.
    HelpOpen(String),
    /// Agent: help requests were closed (why).
    HelpClosed(String),
    /// Agent: `device` of account `user` gave the help code; its session request may follow.
    HelpRequested {
        device: Device,
        user: String,
    },
    /// Viewer: the help code was right; a session with `device` may start.
    HelpGranted(Device),
    /// Agent: `device` of account `user` asks to help without a code; answer with `answer_help`.
    HelpAsked {
        request: u64,
        device: Device,
        user: String,
    },
    /// Agent: send Wake-on-LAN packets for the account's offline device `name`.
    WakeRequest {
        name: String,
        macs: Vec<String>,
    },
    /// Agent: `by`, a device of the account, ended the session controlling this PC.
    EndControl(String),
    Closed(String),
}

pub struct Account {
    commands: Sender<ClientMessage>,
    events: Receiver<AccountEvent>,
    pending: VecDeque<AccountEvent>,
    local_ip: IpAddr,
    /// How the server connection was made (each network card tried), for the reports.
    route: Vec<String>,
    pub user: String,
    pub device_id: String,
    pub devices: Vec<Device>,
    thread: Option<JoinHandle<()>>,
    key: DeviceKey,
    known: KnownDevices,
    /// Devices of other accounts joined by a help code (their keys came with the server's answer), with the
    /// account they belong to.
    helpers: Vec<(Device, String)>,
    /// The connection to the server ended (seen by `still_trusted`).
    closed: bool,
    role: Role,
    /// What the server can do beyond the first protocol (`queqiao_core::account::features`).
    features: Vec<String>,
    /// This login made the account (a new name in the one login form).
    pub created: bool,
    /// Making the connection again when it is lost during a session (`keep_connected`): the login (its refresh token)
    /// and this device, the attempt under way, when the connection was lost, when to try next and how often it was
    /// tried, whether the server refused the login (then the session ends at once), when this connection was made,
    /// and how often a new one took over.
    login: SavedLogin,
    identity: DeviceIdentity,
    reconnecting: Option<Receiver<Result<Account, OpenError>>>,
    lost_at: Option<Instant>,
    next_attempt: Instant,
    attempts: u32,
    refused: bool,
    opened_at: Instant,
    pub reconnections: u32,
    /// The connection follows this PC's best network ([`Account::watch_network`]).
    network: NetworkWatch,
    /// Path reports the server asked for (one per `IceServers` with `PunchSettings::reports`, at most 4 waiting).
    report_credits: u32,
}

/// How long a session goes on without the server connection while it is made again (a network change: the old one
/// is gone, the new network may take a few seconds). The other side stays trusted meanwhile: its key was checked when
/// the session started, and a refused login (this device removed, its login revoked) ends the session at once.
pub const RECONNECT_GRACE: Duration = Duration::from_secs(60);

/// How often the connection's network is looked at ([`Account::watch_network`]).
const WATCH_EVERY: Duration = Duration::from_secs(1);
/// How long a better network must have been there before the connection moves to it (a Wi-Fi just joined may not reach
/// the internet yet).
const MOVE_AFTER: Duration = Duration::from_secs(3);
/// The least time between two moves (a Wi-Fi coming and going does not toss the connection back and forth).
const MOVE_GAP: Duration = Duration::from_secs(30);

/// What [`Account::watch_network`] keeps: when it last looked, the better network seen and since when, a move to it
/// under way (a new connection made there), and when the last move began.
struct NetworkWatch {
    checked: Instant,
    better: Option<(std::net::Ipv4Addr, Instant)>,
    moving: Option<Receiver<Result<Account, OpenError>>>,
    moved_at: Option<Instant>,
}

impl NetworkWatch {
    fn new() -> NetworkWatch {
        NetworkWatch {
            checked: Instant::now(),
            better: None,
            moving: None,
            moved_at: None,
        }
    }
}

/// A login that did not work: `refused` when the server answered no (as opposed to not being reached).
struct OpenError {
    error: Error,
    refused: bool,
}

impl From<Error> for OpenError {
    fn from(error: Error) -> OpenError {
        OpenError {
            error,
            refused: false,
        }
    }
}

impl From<std::io::Error> for OpenError {
    fn from(error: std::io::Error) -> OpenError {
        Error::from(error).into()
    }
}

/// How a connection logs in.
enum Credentials<'a> {
    /// The saved refresh token.
    Saved,
    /// A name and password; `create`: a name the server does not know makes the account (one form for both).
    Password(&'a str, &'a str, bool),
    /// A new account with this name and password.
    NewAccount(&'a str, &'a str),
}

impl Account {
    /// Connects to `login.server`, checking the pinned certificate, and logs in: with the password when
    /// one is given (the new refresh token is put into `login`), otherwise with the saved refresh token.
    /// The device's key comes with `device`; the keys of other devices are pinned in `known_devices.json`
    /// (in memory for a device made with [`DeviceIdentity::in_memory`]).
    pub fn connect(
        login: &mut SavedLogin,
        device: &DeviceIdentity,
        role: Role,
        password: Option<(&str, &str)>,
    ) -> Result<Account, Error> {
        let credentials = match password {
            Some((user, password)) => Credentials::Password(user, password, false),
            None => Credentials::Saved,
        };
        Self::open(login, device, role, credentials)
    }

    /// One form for logging in and signing up (the user, 2026-11-26): a known name logs in with its password; a name
    /// the server does not know makes the account and logs in (on a server that takes new accounts; `created` then
    /// says so, for the person to be told). An earlier server refuses a new name as a wrong login.
    pub fn log_in_or_create(
        login: &mut SavedLogin,
        device: &DeviceIdentity,
        role: Role,
        user: &str,
        password: &str,
    ) -> Result<Account, Error> {
        Self::open(
            login,
            device,
            role,
            Credentials::Password(user, password, true),
        )
    }

    /// A new account (on a server that takes new accounts: `serve --allow-signup`), logged in at once: the refresh
    /// token is put into `login`.
    pub fn register(
        login: &mut SavedLogin,
        device: &DeviceIdentity,
        role: Role,
        user: &str,
        password: &str,
    ) -> Result<Account, Error> {
        Self::open(login, device, role, Credentials::NewAccount(user, password))
    }

    fn open(
        login: &mut SavedLogin,
        device: &DeviceIdentity,
        role: Role,
        credentials: Credentials<'_>,
    ) -> Result<Account, Error> {
        Self::open_with(login, device, role, credentials).map_err(|failed| failed.error)
    }

    /// As `open`; the error says whether the server refused the login (as opposed to not being reached).
    fn open_with(
        login: &mut SavedLogin,
        device: &DeviceIdentity,
        role: Role,
        credentials: Credentials<'_>,
    ) -> Result<Account, OpenError> {
        let key = device
            .key
            .clone()
            .ok_or_else(|| Error("the device has no key".into()))?;
        let known = if device.in_memory {
            KnownDevices::with_pins(device.preset_pins.clone().unwrap_or_default())
        } else {
            KnownDevices::load()?
        };
        // The account server; when it cannot be reached, the standbys it named, in order (one of them may have been
        // made the account server: it has everyone's logins). Test devices keep them only in test builds' files.
        let keeps_standbys = !device.in_memory || cfg!(feature = "test-files");
        let standbys = if keeps_standbys {
            standbys::load(&login.server, &login.pin)
        } else {
            Vec::new()
        };
        let mut reached = reach(&login.server, &login.pin);
        let mut via_standby = false;
        if reached.is_err() {
            for standby in &standbys {
                if let Ok((socket, mut route, local_ip)) = reach(&standby.server, &standby.pin) {
                    route.push(format!(
                        "{} could not be reached: connected to its standby {}",
                        login.server, standby.server
                    ));
                    reached = Ok((socket, route, local_ip));
                    via_standby = true;
                    break;
                }
            }
        }
        let (mut socket, route, local_ip) = reached?;

        let first = match credentials {
            Credentials::Password(user, password, create) => ClientMessage::Login {
                user: user.to_string(),
                password: password.to_string(),
                device_id: device.id.clone(),
                device_name: device.name.clone(),
                role,
                public_key: key.public_hex().to_string(),
                machine: device.machine(),
                create,
            },
            Credentials::NewAccount(user, password) => ClientMessage::Register {
                user: user.to_string(),
                password: password.to_string(),
                device_id: device.id.clone(),
                device_name: device.name.clone(),
                role,
                public_key: key.public_hex().to_string(),
                machine: device.machine(),
            },
            Credentials::Saved => ClientMessage::Resume {
                refresh_token: login.refresh_token.clone().ok_or_else(|| {
                    Error("no saved login: give the user name and password".into())
                })?,
                device_id: device.id.clone(),
                device_name: device.name.clone(),
                role,
                public_key: key.public_hex().to_string(),
            },
        };
        socket
            .send(Message::text(
                serde_json::to_string(&first).map_err(tls::err)?,
            ))
            .map_err(tls::err)?;
        let welcome = loop {
            match socket.read().map_err(tls::err)? {
                Message::Text(text) => {
                    break serde_json::from_str::<ServerMessage>(&text).map_err(tls::err)?;
                }
                Message::Close(_) => {
                    return Err(Error("the server closed the connection".into()).into());
                }
                _ => {}
            }
        };
        let (user, device_id, devices, features, created, relays) = match welcome {
            ServerMessage::Welcome {
                user,
                device_id,
                refresh_token,
                devices,
                features,
                created,
                relays,
                standbys: named,
            } => {
                if let Some(token) = refresh_token {
                    login.refresh_token = Some(token);
                }
                // The account server's own word on its standbys (a standby's welcome changes nothing kept).
                if keeps_standbys && !via_standby {
                    let _ = standbys::save(&login.server, &login.pin, &named);
                }
                (user, device_id, devices, features, created, relays)
            }
            ServerMessage::Error { message } => {
                return Err(OpenError {
                    error: Error(message),
                    refused: true,
                });
            }
            other => return Err(Error(format!("unexpected answer: {other:?}")).into()),
        };

        let (command_sender, command_receiver) = mpsc::channel();
        let (event_sender, event_receiver) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("queqiao-account".into())
            .spawn(move || {
                let reason = tls::run(socket, command_receiver, &event_sender);
                let _ = event_sender.send(AccountEvent::Closed(reason));
            })?;
        // How far each relay is, for the server to choose the nearest for each session (in the background).
        if features
            .iter()
            .any(|f| f == queqiao_core::account::features::RELAYS)
            && !relays.is_empty()
        {
            relay_rtt::spawn(local_ip, relays, command_sender.clone());
        }
        Ok(Account {
            commands: command_sender,
            events: event_receiver,
            pending: VecDeque::new(),
            local_ip,
            route,
            user,
            device_id,
            devices,
            thread: Some(thread),
            key,
            known,
            role,
            helpers: Vec::new(),
            closed: false,
            features,
            created,
            login: login.clone(),
            identity: device.clone(),
            reconnecting: None,
            lost_at: None,
            next_attempt: Instant::now(),
            attempts: 0,
            refused: false,
            opened_at: Instant::now(),
            reconnections: 0,
            report_credits: 0,
            network: NetworkWatch::new(),
        })
    }

    /// Whether the server can do `feature` (`queqiao_core::account::features`).
    pub fn supports(&self, feature: &str) -> bool {
        self.features.iter().any(|f| f == feature)
    }

    /// Agent: tells the account who controls this device now (None: nobody), on a server that shows it.
    pub fn set_control_state(&self, controller: Option<&str>) {
        if self.supports(queqiao_core::account::features::CONTROL_STATE) {
            self.send(ClientMessage::ControlState {
                controller: controller.unwrap_or_default().to_string(),
            });
        }
    }

    /// Ends the session controlling the account's device `device_id` (its "Disconnect"), on a server that can.
    pub fn end_control(&self, device_id: &str) -> Result<(), Error> {
        if !self.supports(queqiao_core::account::features::CONTROL_STATE) {
            return Err(Error(
                "the server cannot end another device's session (update it)".into(),
            ));
        }
        self.send(ClientMessage::EndControl {
            device_id: device_id.to_string(),
        });
        Ok(())
    }

    /// Checks a signal from another device: it must come from a device of the account whose key matches the
    /// key this PC pinned for it (pinned now if it is the first contact), signed over what it says.
    pub fn check_signal(
        &mut self,
        from: &str,
        from_role: Role,
        session: &str,
        kind: &str,
        body: &str,
        signature: &str,
    ) -> Result<(), Error> {
        let helper = self
            .helpers
            .iter()
            .map(|(device, _)| device)
            .find(|d| d.id == from);
        let Some(device) = self.devices.iter().find(|d| d.id == from).or(helper) else {
            return Err(Error(format!(
                "a signal from {from}, which is not a device of this account (nor joined by a help code)"
            )));
        };
        let (key, name) = (device.public_key.clone(), device.name.clone());
        if key.is_empty() {
            return Err(Error(format!(
                "{name} has no device key (an older Queqiao): refused"
            )));
        }
        if let Pin::Changed(_) = self.known.check(from, &key) {
            return Err(Error(format!(
                "the key of {name} differs from the one this PC first saw: refused. If {name} was reinstalled, \
                 confirm its new key (--trust-new-key {from})"
            )));
        }
        let message = signed_bytes(
            session,
            kind,
            from,
            from_role,
            &self.device_id,
            self.role,
            body,
        );
        if !verify(&key, &message, signature) {
            return Err(Error(format!(
                "a signal claiming to be from {name} is not signed by its key: refused"
            )));
        }
        Ok(())
    }

    /// Removes `device` from the account (its saved logins end, it is disconnected); the device list
    /// update follows, or an `AccountEvent::Error`.
    pub fn remove_device(&self, device: &str) {
        self.send(ClientMessage::RemoveDevice {
            device_id: device.to_string(),
        });
    }

    /// Changes the account's password; returns this device's new refresh token (save it: every saved login
    /// of the account, this one's included, was revoked, and the account's other connections were closed).
    pub fn change_password(
        &mut self,
        old_password: &str,
        new_password: &str,
    ) -> Result<String, Error> {
        self.send(ClientMessage::ChangePassword {
            old_password: old_password.to_string(),
            new_password: new_password.to_string(),
        });
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut other = Vec::new();
        let result = loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break Err(Error(
                    "the server did not answer the password change".into(),
                ));
            }
            match self.next_event(left) {
                Some(AccountEvent::PasswordChanged(token)) => break Ok(token),
                Some(AccountEvent::Error(message)) => break Err(Error(message)),
                Some(AccountEvent::Closed(reason)) => break Err(Error(reason)),
                Some(event) => other.push(event),
                None => {}
            }
        };
        for event in other.into_iter().rev() {
            self.pending.push_front(event);
        }
        result
    }

    /// Accepts a new key for `device` (after the person confirmed it, e.g. a reinstalled PC).
    pub fn trust_new_key(&mut self, device: &str) -> Result<(), Error> {
        self.known.forget(device)
    }

    fn signature(&self, to: &str, to_role: Role, session: &str, kind: &str, body: &str) -> String {
        self.key.sign(&signed_bytes(
            session,
            kind,
            &self.device_id,
            self.role,
            to,
            to_role,
            body,
        ))
    }

    /// Our address as seen on the connection to the server (the one other devices can reach).
    pub fn local_ip(&self) -> IpAddr {
        self.local_ip
    }

    /// How the server connection was made: each network card tried and what came of it.
    pub fn route(&self) -> String {
        self.route.join("; ")
    }

    pub fn send(&self, message: ClientMessage) {
        let _ = self.commands.send(message);
    }

    /// The next event, waiting at most `timeout`. Device list updates are also kept in `devices`.
    /// During a session (while the main loop does not read events): what arrived is taken in and stays queued for
    /// `next_event`. Whether `device` may go on: still on the account, or a helper let in, and this login still on
    /// (as UU: a device taken out of the account is logged out; a changed password closes the account's other
    /// connections, this one too).
    pub fn still_trusted(&mut self, device: &str) -> bool {
        loop {
            match self.events.try_recv() {
                Ok(event) => {
                    match &event {
                        AccountEvent::Devices(devices) => self.devices = devices.clone(),
                        AccountEvent::Closed(_) => self.closed = true,
                        _ => {}
                    }
                    self.pending.push_back(event);
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.closed = true;
                    break;
                }
            }
        }
        self.keep_connected();
        // Without the connection for a moment (it is being made again): the session goes on, up to RECONNECT_GRACE.
        let connection = !self.closed
            || (!self.refused
                && self
                    .lost_at
                    .is_some_and(|lost| lost.elapsed() < RECONNECT_GRACE));
        connection
            && (self.devices.iter().any(|d| d.id == device)
                || self.helpers.iter().any(|(d, _)| d.id == device))
    }

    /// Whether the connection to the server is up now (during a session it may be lost and made again).
    pub fn online(&self) -> bool {
        !self.closed
    }

    /// During a session (called through `still_trusted`, once a second): once the server connection is lost, makes it
    /// again in the background with the saved refresh token (1, 2, 4, then every 8 s), and takes the new one in when it
    /// is made (its address too: after a network change the session gathers on it). A refused login is final. Tests:
    /// QUEQIAO_FORCE=account-drop:<seconds> ends this connection that long after it was made, once.
    fn keep_connected(&mut self) {
        self.watch_network();
        if !self.closed
            && self.reconnections == 0
            && queqiao_core::forced("account-drop")
                .and_then(|seconds| seconds.parse::<u64>().ok())
                .is_some_and(|seconds| self.opened_at.elapsed() >= Duration::from_secs(seconds))
        {
            // Dropping the command channel ends the connection thread, as a lost connection does.
            let (sender, _) = mpsc::channel();
            drop(std::mem::replace(&mut self.commands, sender));
            self.closed = true;
            self.pending
                .push_back(AccountEvent::Closed("QUEQIAO_FORCE=account-drop".into()));
        }
        // The server closes the older connection as a move's new one logs in: that one takes over (watch_network).
        if !self.closed || self.refused || self.network.moving.is_some() {
            return;
        }
        let now = Instant::now();
        self.lost_at.get_or_insert(now);
        if let Some(attempt) = &self.reconnecting {
            match attempt.try_recv() {
                Ok(Ok(fresh)) => {
                    self.take_over(fresh);
                    return;
                }
                Ok(Err(failed)) => {
                    self.reconnecting = None;
                    self.refused = failed.refused;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => return,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => self.reconnecting = None,
            }
        }
        if self.refused || now < self.next_attempt {
            return;
        }
        self.attempts += 1;
        self.next_attempt = now + Duration::from_secs(1 << self.attempts.min(4).saturating_sub(1));
        let (sender, receiver) = mpsc::channel();
        let (mut login, identity, role) = (self.login.clone(), self.identity.clone(), self.role);
        let spawned = std::thread::Builder::new()
            .name("queqiao-account-again".into())
            .spawn(move || {
                let _ = sender.send(Account::open_with(
                    &mut login,
                    &identity,
                    role,
                    Credentials::Saved,
                ));
            });
        if spawned.is_ok() {
            self.reconnecting = Some(receiver);
        }
    }

    /// Once a second ([`Account::keep_connected`] in a session, [`Account::next_event`] between sessions): the
    /// connection follows this PC's best network (the user, 2026-11-27, "Do as you suggest": Wi-Fi or a cable before
    /// mobile data, the other kept as the way back).
    /// - Its address is gone (that network went away): the connection is closed at once and made again on the network
    ///   left. Before, a connection on a vanished network was noticed only after 60 s of silence; a session's restart,
    ///   which needs the server, ran out of time meanwhile (sunshine's mobile data switched off while on Wi-Fi too:
    ///   frozen, then ended after 40 s, 2026-11-27).
    /// - A network the system routes before this one (Windows puts Wi-Fi and Ethernet before mobile data) has been up
    ///   for [`MOVE_AFTER`]: a new connection is made there and takes this one's place (the server closes the older
    ///   one), at most once in [`MOVE_GAP`]. A session follows the connection's address (restart.rs): it gathers on the
    ///   new network, whose candidates then come first.
    fn watch_network(&mut self) {
        let now = Instant::now();
        if let Some(moving) = &self.network.moving {
            match moving.try_recv() {
                Ok(Ok(fresh)) => {
                    self.network.moving = None;
                    self.take_over(fresh);
                    return;
                }
                Ok(Err(_)) | Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.network.moving = None
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
        if self.closed || now < self.network.checked + WATCH_EVERY {
            return;
        }
        self.network.checked = now;
        let IpAddr::V4(ip) = self.local_ip else {
            return;
        };
        if ip.is_loopback() || ip.is_unspecified() {
            return;
        }
        if std::net::UdpSocket::bind((ip, 0)).is_err() {
            // Dropping the command channel ends the connection thread, as a lost connection does; made again at once.
            let (sender, _) = mpsc::channel();
            drop(std::mem::replace(&mut self.commands, sender));
            self.closed = true;
            self.attempts = 0;
            self.next_attempt = now;
            self.pending.push_back(AccountEvent::Closed(format!(
                "this PC's network changed: {ip} is no longer one of its addresses"
            )));
            return;
        }
        if self.network.moving.is_some() || self.refused {
            return;
        }
        let uplinks = queqiao_transport::uplinks();
        let Some(first) = uplinks
            .first()
            .copied()
            .filter(|first| *first != ip && uplinks.contains(&ip))
        else {
            self.network.better = None;
            return;
        };
        let since = match self.network.better {
            Some((same, since)) if same == first => since,
            _ => {
                self.network.better = Some((first, now));
                now
            }
        };
        let rested = self.network.moved_at.is_none_or(|at| now >= at + MOVE_GAP);
        if now < since + MOVE_AFTER || !rested {
            return;
        }
        // The new connection goes out of the first uplink (reach tries them in route order).
        self.network.moved_at = Some(now);
        let (sender, receiver) = mpsc::channel();
        let (mut login, identity, role) = (self.login.clone(), self.identity.clone(), self.role);
        let spawned = std::thread::Builder::new()
            .name("queqiao-account-move".into())
            .spawn(move || {
                let _ = sender.send(Account::open_with(
                    &mut login,
                    &identity,
                    role,
                    Credentials::Saved,
                ));
            });
        if spawned.is_ok() {
            self.network.moving = Some(receiver);
        }
    }

    /// A new connection takes the lost one's place: what was queued stays (the lost connection's Closed goes), the
    /// devices let in by a help code stay; a new refresh token is kept as the login kept it.
    fn take_over(&mut self, fresh: Account) {
        let mut pending = std::mem::take(&mut self.pending);
        pending.retain(|event| !matches!(event, AccountEvent::Closed(_)));
        let helpers = std::mem::take(&mut self.helpers);
        let reconnections = self.reconnections + 1;
        let old_moved_at = self.network.moved_at;
        // The old one goes; its thread is not waited for (after a lost connection it has ended; after the test switch
        // it ends when its socket does).
        let mut old = std::mem::replace(self, fresh);
        drop(old.thread.take());
        drop(old);
        self.pending = pending;
        self.helpers = helpers;
        self.reconnections = reconnections;
        self.network.moved_at = old_moved_at;
        if !self.identity.in_memory {
            let _ = self.login.save(self.role);
        }
    }

    /// An event `next_event` gave that is not the caller's to handle (a running session's signal, taken by its
    /// `SessionSignal`): back at the front of the queue.
    pub fn put_back(&mut self, event: AccountEvent) {
        self.pending.push_front(event);
    }

    pub fn next_event(&mut self, timeout: Duration) -> Option<AccountEvent> {
        // Between sessions too: a connection whose network went away is noticed in a second, not after 60 s of
        // silence, and the program makes it again on the network left; a better network is moved to.
        self.watch_network();
        let event = match self.pending.pop_front() {
            Some(event) => Some(event),
            None => match self.events.recv_timeout(timeout) {
                Ok(event) => Some(event),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => {
                    Some(AccountEvent::Closed("connection thread ended".into()))
                }
            },
        };
        // A move to a better network under way when this connection closes (the server closes the older connection
        // as the new one logs in): the new one takes over, and nothing is lost.
        if matches!(event, Some(AccountEvent::Closed(_)))
            && let Some(moving) = self.network.moving.take()
            && let Ok(Ok(fresh)) = moving.recv_timeout(Duration::from_secs(15))
        {
            self.take_over(fresh);
            return None;
        }
        match &event {
            Some(AccountEvent::Devices(devices)) => self.devices = devices.clone(),
            Some(AccountEvent::HelpRequested { device, user }) => self.add_helper(device, user),
            Some(AccountEvent::HelpGranted(device)) => self.add_helper(device, ""),
            _ => {}
        }
        event
    }

    fn add_helper(&mut self, device: &Device, user: &str) {
        self.helpers.retain(|(known, _)| known.id != device.id);
        self.helpers.push((device.clone(), user.to_string()));
    }

    /// The account of `device` when a help code joined it (another account), or None for the account's own.
    pub fn helper_account(&self, device: &str) -> Option<&str> {
        if self.devices.iter().any(|d| d.id == device) {
            return None;
        }
        self.helpers
            .iter()
            .find(|(known, _)| known.id == device)
            .map(|(_, user)| user.as_str())
    }

    /// Agent: takes help requests with `code` (answered by `AccountEvent::HelpOpen` with the help ID).
    pub fn open_help(&self, code: &str) {
        self.send(ClientMessage::OpenHelp {
            code: code.to_string(),
        });
    }

    /// The keys of the account's devices as this device pinned them (handed to the agent at the sign-in screen).
    pub fn pins(&self) -> Vec<(String, String)> {
        self.known.pins()
    }

    pub fn close_help(&self) {
        self.send(ClientMessage::CloseHelp);
    }

    /// Agent: a new help ID (the old one stops working at once).
    pub fn refresh_help_id(&self) {
        self.send(ClientMessage::RefreshHelpId);
    }

    /// Agent: the person's answer to `AccountEvent::HelpAsked`.
    pub fn answer_help(&self, request: u64, allow: bool) {
        self.send(ClientMessage::HelpAnswer { request, allow });
    }

    /// Viewer: asks to help the device with `help_id`; returns it (a device of another account) when the code
    /// is right.
    pub fn request_help(&mut self, help_id: &str, code: &str) -> Result<Device, Error> {
        self.send(ClientMessage::RequestHelp {
            help_id: help_id.to_string(),
            code: code.to_string(),
        });
        // Without a code the person there decides (their prompt waits 25 s).
        let wait = if code.trim().is_empty() { 45 } else { 15 };
        let deadline = Instant::now() + Duration::from_secs(wait);
        let mut other = Vec::new();
        let result = loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break Err(Error("no answer to the help request".into()));
            }
            match self.next_event(left) {
                Some(AccountEvent::HelpGranted(device)) => break Ok(device),
                Some(AccountEvent::Error(message)) => break Err(Error(message)),
                Some(AccountEvent::Closed(reason)) => break Err(Error(reason)),
                Some(event) => other.push(event),
                None => {}
            }
        };
        for event in other.into_iter().rev() {
            self.pending.push_front(event);
        }
        result
    }

    /// Signaling for one session with the program `role` on `device`.
    pub fn session(&mut self, device: &str, role: Role, session: &str) -> SessionSignal<'_> {
        SessionSignal {
            account: self,
            device: device.to_string(),
            role,
            session: session.to_string(),
        }
    }
}

impl Drop for Account {
    fn drop(&mut self) {
        // Dropping the command channel ends the connection thread.
        let (sender, _) = mpsc::channel();
        drop(std::mem::replace(&mut self.commands, sender));
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// A new random session id.
pub fn new_session_id() -> String {
    config::random_hex(8)
}

/// Carries one session's offer and answer through the server.
pub struct SessionSignal<'a> {
    account: &'a mut Account,
    device: String,
    role: Role,
    session: String,
}

impl Signaling for SessionSignal<'_> {
    fn peer_trusted(&mut self) -> bool {
        self.account.still_trusted(&self.device)
    }

    /// An agent's session: another device's hello (checked: a device of the account, or one a help code joined, with
    /// its pinned key's signature; anything else is dropped), or a device of the account ending this session.
    fn interruption(&mut self) -> Option<Interruption> {
        if self.account.role != Role::Agent {
            return None;
        }
        // What arrived is taken in first (it waits in `pending` for the program's own loop).
        let _ = self.account.still_trusted(&self.device);
        let at = self.account.pending.iter().position(|event| match event {
            AccountEvent::Signal {
                from_role: Role::Viewer,
                kind,
                session,
                ..
            } => kind == "hello" && *session != self.session,
            AccountEvent::EndControl(_) => true,
            _ => false,
        })?;
        match self.account.pending.remove(at)? {
            AccountEvent::EndControl(by) => Some(Interruption::Ended { by }),
            AccountEvent::Signal {
                from,
                from_name,
                from_role,
                session,
                kind,
                body,
                signature,
            } => {
                self.account
                    .check_signal(&from, from_role, &session, &kind, &body, &signature)
                    .ok()?;
                let helper = self.account.helper_account(&from).map(str::to_string);
                Some(Interruption::Request(SessionRequest {
                    from,
                    from_name,
                    helper,
                    session,
                    body,
                    signature,
                }))
            }
            _ => None,
        }
    }

    fn answer_request(&mut self, request: &SessionRequest, kind: &str, body: &str) {
        let signature =
            self.account
                .signature(&request.from, Role::Viewer, &request.session, kind, body);
        self.account.send(ClientMessage::Signal {
            to: request.from.clone(),
            to_role: Role::Viewer,
            session: request.session.clone(),
            kind: kind.to_string(),
            body: body.to_string(),
            signature,
        });
    }

    /// Back at the front, as it came: the program's loop checks it again (signature, account, consent) once this
    /// session ended.
    fn requeue(&mut self, request: SessionRequest) {
        self.account.pending.push_front(AccountEvent::Signal {
            from: request.from,
            from_name: request.from_name,
            from_role: Role::Viewer,
            session: request.session,
            kind: "hello".to_string(),
            body: request.body,
            signature: request.signature,
        });
    }

    fn set_controller(&mut self, controller: Option<&str>) {
        self.account.set_control_state(controller);
    }

    /// A message of `kind` from the other side of this session that came meanwhile (an ICE restart's request, offer or
    /// answer), its signature and key checked as at the start; anything else stays queued.
    fn take(&mut self, kind: &str) -> Option<String> {
        // What arrived is taken in first (and the connection kept up).
        let _ = self.account.still_trusted(&self.device);
        let at = self.account.pending.iter().position(|event| {
            matches!(event, AccountEvent::Signal { from, session, kind: got, .. }
                if *from == self.device && *session == self.session && got == kind)
        })?;
        match self.account.pending.remove(at)? {
            AccountEvent::Signal {
                from,
                from_role,
                session,
                kind,
                body,
                signature,
                ..
            } => self
                .account
                .check_signal(&from, from_role, &session, &kind, &body, &signature)
                .ok()
                .map(|()| body),
            _ => None,
        }
    }

    fn online(&mut self) -> bool {
        let _ = self.account.still_trusted(&self.device);
        self.account.online()
    }

    /// Only what the server asked for (one per session it gave the relays of, with reports on), and only valid words.
    fn report_path(&mut self, report: &PathReport) {
        if self.account.report_credits == 0 || !report.valid() {
            return;
        }
        self.account.report_credits -= 1;
        self.account.send(ClientMessage::PathReport {
            report: report.clone(),
        });
    }

    fn send(&mut self, kind: &str, body: &str) -> Result<(), Error> {
        let signature = self
            .account
            .signature(&self.device, self.role, &self.session, kind, body);
        self.account.send(ClientMessage::Signal {
            to: self.device.clone(),
            to_role: self.role,
            session: self.session.clone(),
            kind: kind.to_string(),
            body: body.to_string(),
            signature,
        });
        Ok(())
    }

    fn expect(&mut self, kind: &str) -> Result<String, Error> {
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut other = Vec::new();
        let result = loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break Err(Error(format!("no {kind} from the other side within 30 s")));
            }
            match self.account.next_event(left) {
                Some(AccountEvent::Signal {
                    from,
                    from_role,
                    session,
                    kind: got,
                    body,
                    signature,
                    ..
                }) if from == self.device && session == self.session => {
                    // Nothing from the other side is used before its signature and key are checked.
                    if let Err(error) = self
                        .account
                        .check_signal(&from, from_role, &session, &got, &body, &signature)
                    {
                        break Err(error);
                    }
                    if got == kind {
                        break Ok(body);
                    }
                    // The other side refused (declined by the person there, control turned off, ...).
                    if got == "error" {
                        break Err(Error(body));
                    }
                    break Err(Error(format!("expected {kind}, got {got}: {body}")));
                }
                Some(AccountEvent::Closed(reason)) => break Err(Error(reason)),
                Some(AccountEvent::Error(message)) => break Err(Error(message)),
                Some(event) => other.push(event),
                None => {}
            }
        };
        // Events for the program (device list changes, other sessions) are kept for it, in order.
        for event in other.into_iter().rev() {
            self.account.pending.push_front(event);
        }
        result
    }

    fn local_ip(&self) -> Result<IpAddr, Error> {
        Ok(self.account.local_ip)
    }

    /// Asks the server for its STUN and TURN servers and fresh relay credentials (waits up to 5 s). A server with
    /// several relays is told the other device, so both ends get the same one, the nearest to both.
    fn ice_servers(&mut self) -> Result<IceServers, Error> {
        let relays = self
            .account
            .supports(queqiao_core::account::features::RELAYS);
        let peer = relays.then(|| self.device.clone());
        // Tests: QUEQIAO_FORCE=relay:<IP> asks for the relay at that address first.
        let prefer = queqiao_core::forced("relay").filter(|_| relays);
        self.account
            .send(ClientMessage::GetIceServers { peer, prefer });
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut other = Vec::new();
        let result = loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break Err(Error(
                    "the server did not send its STUN and TURN servers".into(),
                ));
            }
            match self.account.next_event(left) {
                Some(AccountEvent::IceServers {
                    stun,
                    turn,
                    backups,
                    punch,
                }) => {
                    // A report may go for each session the server asked one of (`report_path`).
                    let punch = punch.unwrap_or_default();
                    if punch.reports {
                        self.account.report_credits = (self.account.report_credits + 1).min(4);
                    }
                    let server = |access: TurnAccess| -> Result<TurnServer, Error> {
                        Ok(TurnServer {
                            address: resolve(&access.address)?,
                            username: access.username,
                            password: access.password,
                            name: access.name,
                        })
                    };
                    let turn = match turn {
                        Some(access) => Some(server(access)?),
                        None => None,
                    };
                    // A backup whose name does not resolve is left out, not the session.
                    let backups = backups.into_iter().filter_map(|a| server(a).ok()).collect();
                    let stun = match stun {
                        Some(address) => Some(resolve(&address)?),
                        None => None,
                    };
                    break Ok(IceServers {
                        stun,
                        turn,
                        backups,
                        punch,
                    });
                }
                Some(AccountEvent::Closed(reason)) => break Err(Error(reason)),
                Some(event) => other.push(event),
                None => {}
            }
        };
        for event in other.into_iter().rev() {
            self.account.pending.push_front(event);
        }
        result
    }
}

/// Connects to `server` (straight out of this PC's network card, each of its addresses in turn) and opens TLS with
/// `pin` and the WebSocket: the socket, the way it went (for the reports), and this PC's address on it.
fn reach(server: &str, pin: &str) -> Result<(tls::Socket, Vec<String>, IpAddr), Error> {
    // By name too past a proxy's made-up addresses (resolve_server), each address in turn.
    let addresses = queqiao_transport::resolve_server(server)?;
    let mut failures = Vec::new();
    let mut connected = None;
    for address in addresses {
        // Straight out of this PC's network card, past a proxy's TUN adapter, for a server on the internet.
        let (tcp, tried) =
            queqiao_transport::connect_direct_noted(address, Duration::from_secs(10));
        match tcp {
            Ok(tcp) => {
                // The way it went, after any address that did not answer first.
                let mut route = failures.clone();
                route.extend(tried);
                connected = Some((tcp, route));
                break;
            }
            Err(e) => failures.push(format!("{address}: {e} ({})", tried.join("; "))),
        }
    }
    let (tcp, route) = connected
        .ok_or_else(|| Error(format!("cannot reach {server}: {}", failures.join("; "))))?;
    tcp.set_nodelay(true)?;
    let local_ip = tcp.local_addr()?.ip();
    let socket = tls::open(tcp, server, pin)?;
    Ok((socket, route, local_ip))
}

fn resolve(address: &str) -> Result<SocketAddr, Error> {
    queqiao_transport::resolve_server(address)?
        .into_iter()
        .find(SocketAddr::is_ipv4)
        .ok_or_else(|| Error(format!("no IPv4 address for {address}")))
}
