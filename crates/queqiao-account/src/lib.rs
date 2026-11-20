//! The client side of queqiao-server: a TLS WebSocket that only accepts the server certificate whose
//! SHA-256 fingerprint was pinned, login or resume, the account's device list, and the signaling of a
//! session ([`SessionSignal`], which carries a WebRTC offer and answer through the server).
//!
//! The connection runs on its own thread ("queqiao-account"); the program talks to it through channels.

mod config;
mod tls;

use std::collections::VecDeque;
use std::net::{IpAddr, TcpStream, ToSocketAddrs};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use queqiao_core::account::ServerMessage;
use queqiao_transport::{Error, Signaling};
use tungstenite::Message;

pub use config::{DeviceIdentity, SavedLogin};
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
    },
    /// The server refused something (for example a signal to a device that is offline).
    Error(String),
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
}

impl Account {
    /// Connects to `login.server`, checking the pinned certificate, and logs in: with the password when
    /// one is given (the new refresh token is put into `login`), otherwise with the saved refresh token.
    pub fn connect(
        login: &mut SavedLogin,
        device: &DeviceIdentity,
        role: Role,
        password: Option<(&str, &str)>,
    ) -> Result<Account, Error> {
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
            },
            None => ClientMessage::Resume {
                refresh_token: login.refresh_token.clone().ok_or_else(|| {
                    Error("no saved login: give the user name and password".into())
                })?,
                device_id: device.id.clone(),
                device_name: device.name.clone(),
                role,
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
        })
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
        self.account.send(ClientMessage::Signal {
            to: self.device.clone(),
            to_role: self.role,
            session: self.session.clone(),
            kind: kind.to_string(),
            body: body.to_string(),
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
                    session,
                    kind: got,
                    body,
                    ..
                }) if from == self.device && session == self.session => {
                    if got == kind {
                        break Ok(body);
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
}
