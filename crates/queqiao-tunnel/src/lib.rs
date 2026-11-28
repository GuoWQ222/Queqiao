//! Port mapping: a port on the controlling PC (127.0.0.1 only) that leads to a port on the controlled PC, or on a
//! computer that PC reaches (its network), through the session. A program on the controlling PC connects to
//! 127.0.0.1:<local port> as if it were at the other end: SSH, a database, a web page served there.
//!
//! The messages share the session's "terminal" stream (reliable, ordered, with the stream's flow control) under tags
//! of their own, which an older Queqiao does not know and drops. The controlling side asks with `Open`; the
//! controlled side connects and answers `Opened` or `Failed`, and only for a device of the same account (never for
//! someone helping from another account, see [`Tunnels::serve`]). Then `Data` goes both ways, each side acknowledging
//! what it has written out (`Ack`): a side keeps at most [`WINDOW`] bytes of a connection unacknowledged, so a slow
//! program at one end holds its own connection only. `Close` ends one direction (the other may go on: a request
//! sent, its answer still coming), `Reset` the connection.
//!
//! Each connection has a thread that reads its socket and one that writes it; the session's loop moves their data
//! ([`Tunnels::poll`], [`Tunnels::handle`]) and never waits on a socket.

use std::collections::{HashMap, VecDeque};
use std::io::{ErrorKind, Read, Write};
use std::net::{Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

/// Bytes of one connection read and not yet acknowledged by the other side, at most.
pub const WINDOW: usize = 256 * 1024;
/// Bytes in one Data message at most.
pub const CHUNK: usize = 16 * 1024;
/// Connections of one session at most (each side).
pub const MAX_CONNECTIONS: usize = 64;
/// Port mappings of one session at most.
pub const MAX_RULES: usize = 16;
/// How long the controlled side tries to connect.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TunnelMessage {
    /// Controlling -> controlled: connect to `host`:`port` (as that PC names it) for connection `id`.
    Open { id: u32, host: String, port: u16 },
    /// Controlled -> controlling: connection `id` is made.
    Opened { id: u32 },
    /// Controlled -> controlling: connection `id` could not be made, or is not allowed.
    Failed { id: u32, reason: String },
    /// Either way: bytes of connection `id`.
    Data { id: u32, data: Vec<u8> },
    /// Either way: this many bytes of connection `id` written out on this side.
    Ack { id: u32, bytes: u32 },
    /// Either way: no more bytes of connection `id` come from this side.
    Close { id: u32 },
    /// Either way: connection `id` is gone.
    Reset { id: u32, reason: String },
}

// Tags apart from the command lines' (1-4, 11-14) on the shared stream.
const OPEN: u8 = 40;
const OPENED: u8 = 41;
const FAILED: u8 = 42;
const DATA: u8 = 43;
const ACK: u8 = 44;
const CLOSE: u8 = 45;
const RESET: u8 = 46;

impl TunnelMessage {
    pub fn encode(&self) -> Vec<u8> {
        let (tag, id) = match self {
            TunnelMessage::Open { id, .. } => (OPEN, id),
            TunnelMessage::Opened { id } => (OPENED, id),
            TunnelMessage::Failed { id, .. } => (FAILED, id),
            TunnelMessage::Data { id, .. } => (DATA, id),
            TunnelMessage::Ack { id, .. } => (ACK, id),
            TunnelMessage::Close { id } => (CLOSE, id),
            TunnelMessage::Reset { id, .. } => (RESET, id),
        };
        let mut out = vec![tag];
        out.extend_from_slice(&id.to_le_bytes());
        match self {
            TunnelMessage::Open { host, port, .. } => {
                out.extend_from_slice(&port.to_le_bytes());
                out.extend_from_slice(host.as_bytes());
            }
            TunnelMessage::Failed { reason, .. } | TunnelMessage::Reset { reason, .. } => {
                out.extend_from_slice(reason.as_bytes())
            }
            TunnelMessage::Data { data, .. } => out.extend_from_slice(data),
            TunnelMessage::Ack { bytes, .. } => out.extend_from_slice(&bytes.to_le_bytes()),
            TunnelMessage::Opened { .. } | TunnelMessage::Close { .. } => {}
        }
        out
    }

    /// None for anything else (a command line's message, or something unknown).
    pub fn decode(bytes: &[u8]) -> Option<TunnelMessage> {
        let (&tag, rest) = bytes.split_first()?;
        let id = u32::from_le_bytes(rest.get(..4)?.try_into().ok()?);
        let body = &rest[4..];
        let text = |bytes: &[u8]| String::from_utf8(bytes.to_vec()).ok();
        Some(match tag {
            OPEN => TunnelMessage::Open {
                id,
                port: u16::from_le_bytes(body.get(..2)?.try_into().ok()?),
                host: text(&body[2..])?,
            },
            OPENED => TunnelMessage::Opened { id },
            FAILED => TunnelMessage::Failed {
                id,
                reason: text(body)?,
            },
            DATA => TunnelMessage::Data {
                id,
                data: body.to_vec(),
            },
            ACK => TunnelMessage::Ack {
                id,
                bytes: u32::from_le_bytes(body.get(..4)?.try_into().ok()?),
            },
            CLOSE => TunnelMessage::Close { id },
            RESET => TunnelMessage::Reset {
                id,
                reason: text(body)?,
            },
            _ => return None,
        })
    }
}

/// Where a port mapping leads, as typed: "22" (that PC itself), "192.168.1.5:80", "nas.local:443", "[fe80::1]:22".
pub fn parse_target(text: &str) -> Result<(String, u16), String> {
    let text = text.trim();
    let port = |digits: &str| match digits.parse::<u16>() {
        Ok(port) if port > 0 => Ok(port),
        _ => Err(format!("{digits:?} is not a port (1 to 65535)")),
    };
    if text.is_empty() {
        return Err("a port is needed".into());
    }
    if text.bytes().all(|b| b.is_ascii_digit()) {
        return Ok((Ipv4Addr::LOCALHOST.to_string(), port(text)?));
    }
    let (host, digits) = if let Some(rest) = text.strip_prefix('[') {
        let (host, after) = rest
            .split_once(']')
            .ok_or_else(|| format!("{text:?}: a ] is missing"))?;
        let digits = after
            .strip_prefix(':')
            .ok_or_else(|| format!("{text:?}: the port is missing"))?;
        (host, digits)
    } else {
        text.rsplit_once(':').ok_or_else(|| {
            format!("{text:?}: write a port, or an address and a port (address:port)")
        })?
    };
    let valid = !host.is_empty()
        && host.len() <= 253
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ':' | '%'));
    if !valid {
        return Err(format!("{host:?} is not an address"));
    }
    Ok((host.to_string(), port(digits)?))
}

/// A port mapping of the controlling side.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rule {
    pub id: u32,
    /// Where programs on this PC connect (127.0.0.1:<port>).
    pub local: SocketAddr,
    /// Where it leads, as the other PC names it.
    pub host: String,
    pub port: u16,
    /// Connections open now, and made since the mapping began.
    pub open: usize,
    pub total: usize,
}

/// How much of one connection's reading may go on: bytes read and not yet acknowledged by the other side.
#[derive(Default)]
struct Credit {
    outstanding: Mutex<usize>,
    room: Condvar,
    stopped: AtomicBool,
}

impl Credit {
    /// Waits until fewer than WINDOW bytes are outstanding; false once stopped.
    fn wait(&self) -> bool {
        let mut outstanding = self.outstanding.lock().unwrap_or_else(|e| e.into_inner());
        while *outstanding >= WINDOW && !self.stopped.load(Ordering::Relaxed) {
            outstanding = self
                .room
                .wait_timeout(outstanding, Duration::from_millis(500))
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        !self.stopped.load(Ordering::Relaxed)
    }

    fn add(&self, bytes: usize) {
        *self.outstanding.lock().unwrap_or_else(|e| e.into_inner()) += bytes;
    }

    fn acknowledged(&self, bytes: usize) {
        let mut outstanding = self.outstanding.lock().unwrap_or_else(|e| e.into_inner());
        *outstanding = outstanding.saturating_sub(bytes);
        self.room.notify_all();
    }

    fn stop(&self) {
        self.stopped.store(true, Ordering::Relaxed);
        self.room.notify_all();
    }
}

/// What the threads report to the session's loop.
enum Event {
    Accepted {
        rule: u32,
        stream: TcpStream,
    },
    Connected {
        id: u32,
        result: Result<TcpStream, String>,
    },
    Read {
        id: u32,
        data: Vec<u8>,
    },
    /// The socket's reading ended: None at its end, else why.
    ReadEnd {
        id: u32,
        error: Option<String>,
    },
    Wrote {
        id: u32,
        bytes: usize,
    },
    /// The writer is done: all written (and the write half shut), or why not.
    WriteEnd {
        id: u32,
        error: Option<String>,
    },
}

enum Phase {
    /// Controlling side: asked the other side, no answer yet. Controlled side: connecting.
    Connecting,
    Open,
}

struct Connection {
    /// The mapping it came in by (controlling side; 0 on the controlled side).
    rule: u32,
    phase: Phase,
    stream: Option<TcpStream>,
    /// Data for the writer thread; dropped (None) once the other side closed its direction.
    writer: Option<Sender<Vec<u8>>>,
    credit: Arc<Credit>,
    /// Read here, not yet sent.
    read: VecDeque<Vec<u8>>,
    read_done: bool,
    sent_close: bool,
    got_close: bool,
    writer_done: bool,
    /// Written out here, not yet acknowledged to the other side.
    written: usize,
}

impl Connection {
    fn new(rule: u32, phase: Phase) -> Connection {
        Connection {
            rule,
            phase,
            stream: None,
            writer: None,
            credit: Arc::new(Credit::default()),
            read: VecDeque::new(),
            read_done: false,
            sent_close: false,
            got_close: false,
            writer_done: false,
            written: 0,
        }
    }

    /// Ends it at once: its threads stop, its socket is shut both ways.
    fn abort(&mut self) {
        self.credit.stop();
        self.writer = None;
        if let Some(stream) = &self.stream {
            let _ = stream.shutdown(Shutdown::Both);
        }
    }

    fn finished(&self) -> bool {
        self.sent_close && self.got_close && self.writer_done
    }
}

struct Listening {
    rule: Rule,
    stop: Arc<AtomicBool>,
}

/// The session's port mappings and connections (either side).
pub struct Tunnels {
    /// The controlled side opens connections asked for (a device of the same account).
    serving: bool,
    rules: Vec<Listening>,
    connections: HashMap<u32, Connection>,
    events: Sender<Event>,
    incoming: Receiver<Event>,
    /// Control messages waiting to be sent (before any data).
    control: VecDeque<TunnelMessage>,
    next_id: u32,
    /// Where `poll` goes on sending data, so every connection gets its turn.
    turn: usize,
    notes: Vec<String>,
}

impl Default for Tunnels {
    fn default() -> Self {
        Tunnels::new()
    }
}

impl Tunnels {
    pub fn new() -> Tunnels {
        let (events, incoming) = mpsc::channel();
        Tunnels {
            serving: false,
            rules: Vec::new(),
            connections: HashMap::new(),
            events,
            incoming,
            control: VecDeque::new(),
            next_id: 0,
            turn: 0,
            notes: Vec::new(),
        }
    }

    /// The controlled side: connections asked for are made (a device of the same account) or refused (someone
    /// helping from another account).
    pub fn serve(&mut self, on: bool) {
        self.serving = on;
    }

    /// The controlling side: programs on this PC connecting to 127.0.0.1:`local_port` reach `host`:`port` as the
    /// other PC names it. `local_port` 0: a free one.
    pub fn add_rule(&mut self, local_port: u16, host: &str, port: u16) -> Result<Rule, String> {
        if self.rules.len() >= MAX_RULES {
            return Err(format!("at most {MAX_RULES} port mappings"));
        }
        // Loopback only: the mapping is this PC's own, never its network's.
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, local_port))
            .map_err(|e| format!("port {local_port} on this PC: {e}"))?;
        let local = listener.local_addr().map_err(|e| e.to_string())?;
        listener.set_nonblocking(true).map_err(|e| e.to_string())?;
        self.next_id += 1;
        let rule = Rule {
            id: self.next_id,
            local,
            host: host.to_string(),
            port,
            open: 0,
            total: 0,
        };
        let stop = Arc::new(AtomicBool::new(false));
        let (events, id, stopped) = (self.events.clone(), rule.id, stop.clone());
        std::thread::Builder::new()
            .name("queqiao-tunnel-listen".into())
            .spawn(move || {
                while !stopped.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            let _ = stream.set_nonblocking(false);
                            if events.send(Event::Accepted { rule: id, stream }).is_err() {
                                break;
                            }
                        }
                        Err(e) if e.kind() == ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(50))
                        }
                        Err(_) => std::thread::sleep(Duration::from_millis(200)),
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        self.notes.push(format!(
            "port mapping {}: {local} -> {host}:{port} there",
            rule.id
        ));
        self.rules.push(Listening {
            rule: rule.clone(),
            stop,
        });
        Ok(rule)
    }

    /// Ends a port mapping and its connections.
    pub fn remove_rule(&mut self, id: u32) {
        let Some(at) = self.rules.iter().position(|r| r.rule.id == id) else {
            return;
        };
        let listening = self.rules.remove(at);
        listening.stop.store(true, Ordering::Relaxed);
        let ids: Vec<u32> = self
            .connections
            .iter()
            .filter(|(_, c)| c.rule == id)
            .map(|(id, _)| *id)
            .collect();
        for connection in ids {
            self.reset(connection, "the port mapping was removed");
        }
        self.notes.push(format!(
            "port mapping {id} removed ({})",
            listening.rule.local
        ));
    }

    /// The port mappings (controlling side), with their connections.
    pub fn rules(&self) -> Vec<Rule> {
        self.rules
            .iter()
            .map(|listening| {
                let mut rule = listening.rule.clone();
                rule.open = self
                    .connections
                    .values()
                    .filter(|c| c.rule == rule.id)
                    .count();
                rule
            })
            .collect()
    }

    /// Lines for the session's log since the last call.
    pub fn notes(&mut self) -> Vec<String> {
        std::mem::take(&mut self.notes)
    }

    /// A message of the shared stream: true when it was a tunnel's (handled), false for anything else.
    pub fn handle(&mut self, bytes: &[u8]) -> bool {
        let Some(message) = TunnelMessage::decode(bytes) else {
            return false;
        };
        match message {
            TunnelMessage::Open { id, host, port } => self.open_asked(id, host, port),
            TunnelMessage::Opened { id } => {
                if let Some(connection) = self.connections.get_mut(&id) {
                    connection.phase = Phase::Open;
                }
            }
            TunnelMessage::Failed { id, reason } => {
                if let Some(mut connection) = self.connections.remove(&id) {
                    self.notes.push(format!(
                        "port mapping: connection {id} refused there: {reason}"
                    ));
                    connection.abort();
                }
            }
            TunnelMessage::Data { id, data } => {
                let sent = self
                    .connections
                    .get(&id)
                    .and_then(|c| c.writer.as_ref())
                    .is_some_and(|writer| writer.send(data).is_ok());
                if !sent && self.connections.contains_key(&id) {
                    self.reset(id, "data after the end");
                }
            }
            TunnelMessage::Ack { id, bytes } => {
                if let Some(connection) = self.connections.get(&id) {
                    connection.credit.acknowledged(bytes as usize);
                }
            }
            TunnelMessage::Close { id } => {
                if let Some(connection) = self.connections.get_mut(&id) {
                    connection.got_close = true;
                    // The writer writes what it has, then shuts the socket's write half.
                    connection.writer = None;
                }
                self.drop_finished(id);
            }
            TunnelMessage::Reset { id, reason } => {
                if let Some(mut connection) = self.connections.remove(&id) {
                    self.notes.push(format!(
                        "port mapping: connection {id} ended there: {reason}"
                    ));
                    connection.abort();
                }
            }
        }
        true
    }

    /// The controlled side: connection `id` to `host`:`port` asked for.
    fn open_asked(&mut self, id: u32, host: String, port: u16) {
        let refusal = if !self.serving {
            Some("port mapping is for devices of the same account".to_string())
        } else if self.connections.len() >= MAX_CONNECTIONS {
            Some(format!("at most {MAX_CONNECTIONS} connections at once"))
        } else if self.connections.contains_key(&id) {
            Some("that connection is open already".to_string())
        } else {
            None
        };
        if let Some(reason) = refusal {
            self.control.push_back(TunnelMessage::Failed { id, reason });
            return;
        }
        self.connections
            .insert(id, Connection::new(0, Phase::Connecting));
        self.notes.push(format!(
            "port mapping: connection {id} to {host}:{port} asked"
        ));
        let events = self.events.clone();
        let _ = std::thread::Builder::new()
            .name("queqiao-tunnel-connect".into())
            .spawn(move || {
                let result = connect(&host, port);
                let _ = events.send(Event::Connected { id, result });
            });
    }

    /// Ends connection `id` here and tells the other side.
    fn reset(&mut self, id: u32, reason: &str) {
        if let Some(mut connection) = self.connections.remove(&id) {
            connection.abort();
            self.control.push_back(TunnelMessage::Reset {
                id,
                reason: reason.to_string(),
            });
            self.notes
                .push(format!("port mapping: connection {id} ended: {reason}"));
        }
    }

    fn drop_finished(&mut self, id: u32) {
        if self.connections.get(&id).is_some_and(Connection::finished) {
            if let Some(connection) = self.connections.remove(&id) {
                connection.credit.stop();
            }
            self.notes
                .push(format!("port mapping: connection {id} closed"));
        }
    }

    /// Starts the reading and writing threads of a connected socket.
    fn start(&mut self, id: u32, stream: TcpStream) -> Result<(), String> {
        let reader = stream.try_clone().map_err(|e| e.to_string())?;
        let writer = stream.try_clone().map_err(|e| e.to_string())?;
        let Some(connection) = self.connections.get_mut(&id) else {
            return Ok(());
        };
        let credit = connection.credit.clone();
        let events = self.events.clone();
        std::thread::Builder::new()
            .name("queqiao-tunnel-read".into())
            .spawn(move || read_loop(id, reader, &credit, &events))
            .map_err(|e| e.to_string())?;
        let (data, queue) = mpsc::channel::<Vec<u8>>();
        let events = self.events.clone();
        std::thread::Builder::new()
            .name("queqiao-tunnel-write".into())
            .spawn(move || write_loop(id, writer, &queue, &events))
            .map_err(|e| e.to_string())?;
        connection.writer = Some(data);
        connection.stream = Some(stream);
        Ok(())
    }

    /// What the threads did; then the messages to send, control first, data while `room` (bytes) lasts.
    pub fn poll(&mut self, room: usize) -> Vec<Vec<u8>> {
        while let Ok(event) = self.incoming.try_recv() {
            self.on_event(event);
        }
        let mut out: Vec<Vec<u8>> = self.control.drain(..).map(|m| m.encode()).collect();
        for (id, connection) in &mut self.connections {
            if connection.written > 0 {
                let bytes = connection.written.min(u32::MAX as usize) as u32;
                connection.written -= bytes as usize;
                out.push(TunnelMessage::Ack { id: *id, bytes }.encode());
            }
        }
        // Data, a chunk of each connection in turn.
        let mut room = room;
        let mut ids: Vec<u32> = self
            .connections
            .iter()
            .filter(|(_, c)| matches!(c.phase, Phase::Open) && !c.read.is_empty())
            .map(|(id, _)| *id)
            .collect();
        ids.sort_unstable();
        if !ids.is_empty() {
            let start = self.turn % ids.len();
            ids.rotate_left(start);
            self.turn = self.turn.wrapping_add(1);
        }
        let mut progress = true;
        while progress && room > 0 {
            progress = false;
            for id in &ids {
                let Some(connection) = self.connections.get_mut(id) else {
                    continue;
                };
                let Some(chunk) = connection.read.front() else {
                    continue;
                };
                if chunk.len() + 5 > room {
                    continue;
                }
                let chunk = connection.read.pop_front().unwrap_or_default();
                room -= chunk.len() + 5;
                out.push(
                    TunnelMessage::Data {
                        id: *id,
                        data: chunk,
                    }
                    .encode(),
                );
                progress = true;
            }
        }
        // A direction read to its end, all of it sent: its Close.
        let closing: Vec<u32> = self
            .connections
            .iter()
            .filter(|(_, c)| {
                matches!(c.phase, Phase::Open) && c.read_done && c.read.is_empty() && !c.sent_close
            })
            .map(|(id, _)| *id)
            .collect();
        for id in closing {
            if let Some(connection) = self.connections.get_mut(&id) {
                connection.sent_close = true;
            }
            out.push(TunnelMessage::Close { id }.encode());
            self.drop_finished(id);
        }
        out.extend(self.control.drain(..).map(|m| m.encode()));
        out
    }

    fn on_event(&mut self, event: Event) {
        match event {
            Event::Accepted { rule, stream } => {
                let Some(listening) = self.rules.iter_mut().find(|r| r.rule.id == rule) else {
                    let _ = stream.shutdown(Shutdown::Both);
                    return;
                };
                if self.connections.len() >= MAX_CONNECTIONS {
                    let _ = stream.shutdown(Shutdown::Both);
                    self.notes.push(format!(
                        "port mapping {rule}: a connection refused (at most {MAX_CONNECTIONS} at once)"
                    ));
                    return;
                }
                listening.rule.total += 1;
                let (host, port) = (listening.rule.host.clone(), listening.rule.port);
                self.next_id += 1;
                let id = self.next_id;
                self.connections
                    .insert(id, Connection::new(rule, Phase::Connecting));
                // Reading starts at once: what the program sends first waits (within WINDOW) for Opened.
                if let Err(error) = self.start(id, stream) {
                    self.connections.remove(&id);
                    self.notes.push(format!(
                        "port mapping {rule}: a connection failed here: {error}"
                    ));
                    return;
                }
                self.control
                    .push_back(TunnelMessage::Open { id, host, port });
            }
            Event::Connected { id, result } => {
                if !self.connections.contains_key(&id) {
                    if let Ok(stream) = result {
                        let _ = stream.shutdown(Shutdown::Both);
                    }
                    return;
                }
                let started = result.and_then(|stream| self.start(id, stream));
                match started {
                    Ok(()) => {
                        if let Some(connection) = self.connections.get_mut(&id) {
                            connection.phase = Phase::Open;
                        }
                        self.control.push_back(TunnelMessage::Opened { id });
                        self.notes
                            .push(format!("port mapping: connection {id} made"));
                    }
                    Err(reason) => {
                        self.connections.remove(&id);
                        self.notes
                            .push(format!("port mapping: connection {id} failed: {reason}"));
                        self.control.push_back(TunnelMessage::Failed { id, reason });
                    }
                }
            }
            Event::Read { id, data } => {
                if let Some(connection) = self.connections.get_mut(&id) {
                    connection.read.push_back(data);
                }
            }
            Event::ReadEnd { id, error } => match error {
                None => {
                    if let Some(connection) = self.connections.get_mut(&id) {
                        connection.read_done = true;
                    }
                }
                Some(error) => self.reset(id, &error),
            },
            Event::Wrote { id, bytes } => {
                if let Some(connection) = self.connections.get_mut(&id) {
                    connection.written += bytes;
                }
            }
            Event::WriteEnd { id, error } => match error {
                None => {
                    if let Some(connection) = self.connections.get_mut(&id) {
                        connection.writer_done = true;
                    }
                    self.drop_finished(id);
                }
                Some(error) => self.reset(id, &error),
            },
        }
    }

    /// Connections open now.
    pub fn connections(&self) -> usize {
        self.connections.len()
    }
}

impl Drop for Tunnels {
    fn drop(&mut self) {
        for listening in &self.rules {
            listening.stop.store(true, Ordering::Relaxed);
        }
        for connection in self.connections.values_mut() {
            connection.abort();
        }
    }
}

/// Connects to `host`:`port` (each address it names in turn, within CONNECT_TIMEOUT each).
fn connect(host: &str, port: u16) -> Result<TcpStream, String> {
    let addresses: Vec<SocketAddr> = (host, port)
        .to_socket_addrs()
        .map_err(|e| format!("{host}: {e}"))?
        .collect();
    let mut last = format!("{host}: no address");
    for address in addresses {
        match TcpStream::connect_timeout(&address, CONNECT_TIMEOUT) {
            Ok(stream) => {
                let _ = stream.set_nodelay(true);
                return Ok(stream);
            }
            Err(e) => last = format!("{address}: {e}"),
        }
    }
    Err(last)
}

fn read_loop(id: u32, mut stream: TcpStream, credit: &Credit, events: &Sender<Event>) {
    let _ = stream.set_nodelay(true);
    let mut buffer = vec![0u8; CHUNK];
    loop {
        if !credit.wait() {
            return;
        }
        match stream.read(&mut buffer) {
            Ok(0) => {
                let _ = events.send(Event::ReadEnd { id, error: None });
                return;
            }
            Ok(count) => {
                credit.add(count);
                if events
                    .send(Event::Read {
                        id,
                        data: buffer[..count].to_vec(),
                    })
                    .is_err()
                {
                    return;
                }
            }
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) => {
                if !credit.stopped.load(Ordering::Relaxed) {
                    let _ = events.send(Event::ReadEnd {
                        id,
                        error: Some(e.to_string()),
                    });
                }
                return;
            }
        }
    }
}

fn write_loop(id: u32, mut stream: TcpStream, queue: &Receiver<Vec<u8>>, events: &Sender<Event>) {
    for data in queue {
        if let Err(e) = stream.write_all(&data) {
            let _ = events.send(Event::WriteEnd {
                id,
                error: Some(e.to_string()),
            });
            return;
        }
        let _ = events.send(Event::Wrote {
            id,
            bytes: data.len(),
        });
    }
    // The other side closed its direction (or the connection is being dropped): this side's write half ends.
    let _ = stream.shutdown(Shutdown::Write);
    let _ = events.send(Event::WriteEnd { id, error: None });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn messages_survive_the_wire_and_others_are_not_theirs() {
        for message in [
            TunnelMessage::Open {
                id: 1,
                host: "nas.local".into(),
                port: 443,
            },
            TunnelMessage::Opened { id: 2 },
            TunnelMessage::Failed {
                id: 3,
                reason: "no".into(),
            },
            TunnelMessage::Data {
                id: 4,
                data: vec![0, 1, 255],
            },
            TunnelMessage::Ack {
                id: 5,
                bytes: 70_000,
            },
            TunnelMessage::Close { id: 6 },
            TunnelMessage::Reset {
                id: 7,
                reason: "gone".into(),
            },
        ] {
            assert_eq!(TunnelMessage::decode(&message.encode()), Some(message));
        }
        // A command line's Open (tag 1) and short messages are not a tunnel's.
        assert_eq!(TunnelMessage::decode(&[1, 0, 0, 0, 0, 80, 0, 24, 0]), None);
        assert_eq!(TunnelMessage::decode(&[OPEN, 1, 0]), None);
        assert_eq!(TunnelMessage::decode(&[]), None);
    }

    #[test]
    fn targets_read_as_typed() {
        assert_eq!(parse_target("22"), Ok(("127.0.0.1".into(), 22)));
        assert_eq!(
            parse_target(" 192.168.1.5:80 "),
            Ok(("192.168.1.5".into(), 80))
        );
        assert_eq!(parse_target("nas.local:443"), Ok(("nas.local".into(), 443)));
        assert_eq!(parse_target("[fe80::1]:22"), Ok(("fe80::1".into(), 22)));
        for bad in [
            "", "0", "70000", "host", "host:", ":22", "a b:22", "[::1]", "x:y",
        ] {
            assert!(parse_target(bad).is_err(), "{bad:?}");
        }
    }

    /// Both sides' messages to each other until nothing moves for a while.
    fn pump(viewer: &mut Tunnels, agent: &mut Tunnels, until: impl Fn(&Tunnels, &Tunnels) -> bool) {
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(20) {
            for message in viewer.poll(64 * 1024) {
                assert!(agent.handle(&message));
            }
            for message in agent.poll(64 * 1024) {
                assert!(viewer.handle(&message));
            }
            if until(viewer, agent) {
                return;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        panic!("the tunnels did not get there in time");
    }

    /// A server on the controlled side that answers each connection: it reads all, then sends it back reversed.
    fn reversing_server() -> u16 {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                std::thread::spawn(move || {
                    let mut stream = stream;
                    let mut all = Vec::new();
                    let _ = stream.read_to_end(&mut all);
                    all.reverse();
                    let _ = stream.write_all(&all);
                });
            }
        });
        port
    }

    #[test]
    fn a_connection_carries_both_ways_with_half_close_and_more_than_the_window() {
        let target = reversing_server();
        let mut viewer = Tunnels::new();
        let mut agent = Tunnels::new();
        agent.serve(true);
        let rule = viewer.add_rule(0, "127.0.0.1", target).unwrap();
        assert_eq!(rule.local.ip(), Ipv4Addr::LOCALHOST);
        // More than the window, so the acknowledgements must flow.
        let sent: Vec<u8> = (0..(3 * WINDOW + 12_345))
            .map(|i| (i % 251) as u8)
            .collect();
        let local = rule.local;
        let to_send = sent.clone();
        let client = std::thread::spawn(move || {
            let mut stream = TcpStream::connect(local).unwrap();
            stream.write_all(&to_send).unwrap();
            stream.shutdown(Shutdown::Write).unwrap();
            let mut back = Vec::new();
            stream.read_to_end(&mut back).unwrap();
            back
        });
        pump(&mut viewer, &mut agent, |_, _| client.is_finished());
        let back = client.join().unwrap();
        let mut expected = sent;
        expected.reverse();
        assert_eq!(back.len(), expected.len());
        assert!(back == expected);
        assert_eq!(viewer.rules()[0].total, 1);
        // Both ends forget the connection once both directions are closed.
        pump(&mut viewer, &mut agent, |v, a| {
            v.connections() == 0 && a.connections() == 0
        });
        let notes = agent.notes().join("\n");
        assert!(notes.contains("closed"), "{notes}");
    }

    #[test]
    fn someone_helping_gets_no_connection_and_a_closed_port_fails_cleanly() {
        let target = reversing_server();
        let mut viewer = Tunnels::new();
        let mut agent = Tunnels::new();
        // Not serving: refused.
        let rule = viewer.add_rule(0, "127.0.0.1", target).unwrap();
        let local = rule.local;
        let client = std::thread::spawn(move || {
            let mut stream = TcpStream::connect(local).unwrap();
            let mut back = Vec::new();
            let _ = stream.read_to_end(&mut back);
            back
        });
        pump(&mut viewer, &mut agent, |_, _| client.is_finished());
        assert!(client.join().unwrap().is_empty());
        assert!(viewer.notes().join("\n").contains("same account"));
        assert_eq!(viewer.connections(), 0);
        // Serving, but nothing listens on the port: Failed, and the program's connection ends.
        agent.serve(true);
        let closed = {
            let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
            listener.local_addr().unwrap().port()
        };
        let rule = viewer.add_rule(0, "127.0.0.1", closed).unwrap();
        let local = rule.local;
        let client = std::thread::spawn(move || {
            let mut stream = TcpStream::connect(local).unwrap();
            let mut back = Vec::new();
            let _ = stream.read_to_end(&mut back);
            back
        });
        pump(&mut viewer, &mut agent, |_, _| client.is_finished());
        assert!(client.join().unwrap().is_empty());
        assert_eq!(agent.connections(), 0);
        // A rule removed: its port is free again.
        let port = rule.local.port();
        viewer.remove_rule(rule.id);
        std::thread::sleep(Duration::from_millis(200));
        assert!(TcpListener::bind((Ipv4Addr::LOCALHOST, port)).is_ok());
    }

    #[test]
    fn data_for_a_connection_never_opened_is_dropped() {
        let mut agent = Tunnels::new();
        agent.serve(true);
        assert!(
            agent.handle(
                &TunnelMessage::Data {
                    id: 9,
                    data: vec![1]
                }
                .encode()
            )
        );
        assert!(agent.poll(1 << 20).is_empty());
        // Not a tunnel's message: left for the command lines.
        assert!(!agent.handle(&[1, 0, 0, 0, 0, 80, 0, 24, 0]));
    }
}
