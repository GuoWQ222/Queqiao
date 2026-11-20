//! The client side of queqiao-server: a TLS WebSocket that only accepts the server certificate whose
//! SHA-256 fingerprint was pinned, login or resume, the account's device list, and the signaling of a
//! session ([`SessionSignal`], which carries a WebRTC offer and answer through the server).
//!
//! The connection runs on its own thread ("queqiao-account"); the program talks to it through channels.

mod config;
mod keys;
mod tls;

use std::collections::VecDeque;
use std::net::{IpAddr, SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use queqiao_core::account::{ServerMessage, TurnAccess, signed_bytes};
use queqiao_transport::{Error, IceServers, Signaling, TurnServer};
use tungstenite::Message;

pub use config::{DeviceIdentity, SavedLogin, folder as data_folder};
pub use keys::{DeviceKey, KnownDevices, Pin, verify};
pub use queqiao_core::account::{ClientMessage, Device, Role};

/// What the server tells the program after the welcome.
#[derive(Clone, Debug)]
pub enum AccountEvent {
    Devices(Vec<Device>),
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
    /// The answer to `GetIceServers`.
    IceServers {
        stun: Option<String>,
        turn: Option<TurnAccess>,
    },
    Closed(String),
}

pub struct Account {
    commands: Sender<ClientMessage>,
    events: Receiver<AccountEvent>,
    pending: VecDeque<AccountEvent>,
    local_ip: IpAddr,
    pub user: String,
    pub device_id: String,
    pub devices: Vec<Device>,
    thread: Option<JoinHandle<()>>,
    key: DeviceKey,
    known: KnownDevices,
    role: Role,
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
        let key = device
            .key
            .clone()
            .ok_or_else(|| Error("the device has no key".into()))?;
        let known = if device.in_memory {
            KnownDevices::in_memory()
        } else {
            KnownDevices::load()?
        };
        let address = login
            .server
            .to_socket_addrs()?
            .next()
            .ok_or_else(|| Error(format!("no address for {}", login.server)))?;
        let tcp = TcpStream::connect_timeout(&address, Duration::from_secs(10))
            .map_err(|e| Error(format!("cannot reach {}: {e}", login.server)))?;
        tcp.set_nodelay(true)?;
        let local_ip = tcp.local_addr()?.ip();
        let mut socket = tls::open(tcp, &login.server, &login.pin)?;

        let first = match password {
            Some((user, password)) => ClientMessage::Login {
                user: user.to_string(),
                password: password.to_string(),
                device_id: device.id.clone(),
                device_name: device.name.clone(),
                role,
                public_key: key.public_hex().to_string(),
            },
            None => ClientMessage::Resume {
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
                Message::Close(_) => return Err(Error("the server closed the connection".into())),
                _ => {}
            }
        };
        let (user, device_id, devices) = match welcome {
            ServerMessage::Welcome {
                user,
                device_id,
                refresh_token,
                devices,
            } => {
                if let Some(token) = refresh_token {
                    login.refresh_token = Some(token);
                }
                (user, device_id, devices)
            }
            ServerMessage::Error { message } => return Err(Error(message)),
            other => return Err(Error(format!("unexpected answer: {other:?}"))),
        };

        let (command_sender, command_receiver) = mpsc::channel();
        let (event_sender, event_receiver) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("queqiao-account".into())
            .spawn(move || {
                let reason = tls::run(socket, command_receiver, &event_sender);
                let _ = event_sender.send(AccountEvent::Closed(reason));
            })?;
        Ok(Account {
            commands: command_sender,
            events: event_receiver,
            pending: VecDeque::new(),
            local_ip,
            user,
            device_id,
            devices,
            thread: Some(thread),
            key,
            known,
            role,
        })
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
        let Some(device) = self.devices.iter().find(|d| d.id == from) else {
            return Err(Error(format!(
                "a signal from {from}, which is not a device of this account"
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

    pub fn send(&self, message: ClientMessage) {
        let _ = self.commands.send(message);
    }

    /// The next event, waiting at most `timeout`. Device list updates are also kept in `devices`.
    pub fn next_event(&mut self, timeout: Duration) -> Option<AccountEvent> {
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
        if let Some(AccountEvent::Devices(devices)) = &event {
            self.devices = devices.clone();
        }
        event
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

    /// Asks the server for its STUN and TURN servers and fresh relay credentials (waits up to 5 s).
    fn ice_servers(&mut self) -> Result<IceServers, Error> {
        self.account.send(ClientMessage::GetIceServers);
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
                Some(AccountEvent::IceServers { stun, turn }) => {
                    let turn = match turn {
                        Some(access) => Some(TurnServer {
                            address: resolve(&access.address)?,
                            username: access.username,
                            password: access.password,
                        }),
                        None => None,
                    };
                    let stun = match stun {
                        Some(address) => Some(resolve(&address)?),
                        None => None,
                    };
                    break Ok(IceServers { stun, turn });
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

fn resolve(address: &str) -> Result<SocketAddr, Error> {
    address
        .to_socket_addrs()?
        .find(SocketAddr::is_ipv4)
        .ok_or_else(|| Error(format!("no IPv4 address for {address}")))
}
