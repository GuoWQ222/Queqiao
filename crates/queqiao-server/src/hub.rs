//! Connections: TLS, then a WebSocket, then a login or resume within 15 s. Each connected program (agent
//! or viewer of a device) gets a channel; the hub forwards signals between the programs of one account only
//! and pushes the account's device list whenever a program connects or leaves.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use queqiao_core::account::{ClientMessage, Device, Role, ServerMessage, TurnAccess};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};
use tokio::time::timeout;
use tokio_rustls::TlsAcceptor;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;

use crate::store::Store;
use crate::turn::TurnConfig;

/// Messages are small (an SDP is a few kB): anything over 256 kB is refused.
const MAX_MESSAGE: usize = 256 * 1024;
/// A connection that sends nothing for this long is dropped (clients ping every 20 s).
const IDLE: Duration = Duration::from_secs(60);

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
    store: Mutex<Store>,
    turn: Option<Arc<TurnConfig>>,
    online: Mutex<HashMap<(String, Role), Connection>>,
    next_id: AtomicU64,
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
    pub fn new(store: Store, turn: Option<Arc<TurnConfig>>) -> Arc<Hub> {
        Arc::new(Hub {
            store: Mutex::new(store),
            turn,
            online: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
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
        // Password hashing takes tens of milliseconds: off the network threads.
        let hub = self.clone();
        let checked = tokio::task::spawn_blocking(move || hub.authenticate(first))
            .await
            .map_err(|e| e.to_string())?;
        let (identity, new_token) = match checked {
            Ok(result) => result,
            Err(message) => {
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

        loop {
            tokio::select! {
                incoming = timeout(IDLE, stream.next()) => match incoming {
                    Ok(Some(Ok(Message::Text(text)))) => match serde_json::from_str::<ClientMessage>(&text) {
                        Ok(message) => self.handle(&identity, message, &sender),
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
        }
        self.push_devices(identity.user_id);
        Ok(())
    }

    fn authenticate(&self, first: ClientMessage) -> Result<(Identity, Option<String>), String> {
        let store = self.store.lock().map_err(|e| e.to_string())?;
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
                    .ok_or("wrong user name or password")?;
                check_device_fields(&device_id, &device_name)?;
                store.register_device(row.id, &device_id, &device_name, &public_key, true)?;
                let token = store.issue_refresh_token(row.id, &device_id, role_name(role))?;
                let identity = Identity {
                    user_id: row.id,
                    user: row.name,
                    device_id,
                    device_name,
                    role,
                };
                Ok((identity, Some(token)))
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
                    .ok_or("the saved login has expired or was revoked: log in again")?;
                check_device_fields(&device_id, &device_name)?;
                store.register_device(row.id, &device_id, &device_name, &public_key, false)?;
                let identity = Identity {
                    user_id: row.id,
                    user: row.name,
                    device_id,
                    device_name,
                    role,
                };
                Ok((identity, None))
            }
            _ => Err("log in first".into()),
        }
    }

    fn handle(
        &self,
        identity: &Identity,
        message: ClientMessage,
        sender: &UnboundedSender<Outgoing>,
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
                    // Only between the devices of one account.
                    (target.user_id == identity.user_id).then(|| {
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
            ClientMessage::Logout => {
                if let Ok(store) = self.store.lock() {
                    let _ = store.revoke_token(&identity.device_id, role_name(identity.role));
                }
            }
            ClientMessage::Login { .. } | ClientMessage::Resume { .. } => {
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
