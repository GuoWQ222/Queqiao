//! The core of Queqiao's phone and tablet apps (docs/MOBILE.md): the account (log in, the saved login, the device
//! list), one session at a time as a viewer through the server (signalling, the WebRTC transport, the data channel),
//! and input. The pictures and the sound go to the app as they come (H.264/H.265 in Annex B, Opus packets): the phone
//! decodes them with its own hardware (MediaCodec, VideoToolbox).
//!
//! An engine thread does the work; the app talks to it through `Core`: commands as JSON (`command`), events as JSON
//! (`next_event`), frames and sound packets from their own queues (`next_frame`, `next_audio`). `ffi` gives the same
//! to C (iOS), `android` to Java/Kotlin through JNI.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use queqiao_account::{
    Account, AccountEvent, ClientMessage, DeviceIdentity, SavedLogin, new_session_id,
};
use queqiao_core::account::Role as AccountRole;
use queqiao_core::protocol::{
    CodecPreference, Command as RemoteCommand, InputEvent, Message, MouseButton, Quality,
    VideoCodec, hevc,
};
use queqiao_transport::{Addresses, Peer, PeerEvent, Role, Signaling};
use serde_json::{Value, json};

#[cfg(target_os = "android")]
mod android;
pub mod ffi;
#[cfg(not(target_os = "android"))]
pub mod pcm;

/// Why a session ended when the PC left it.
const PC_LEFT: &str = "the PC ended the session";

/// One frame of the picture, as it came.
pub struct Frame {
    pub data: Arc<[u8]>,
    pub codec: VideoCodec,
    /// The frame's time in 90 kHz units.
    pub time_90khz: u64,
    /// After a loss (frames missing before it): the app's decoder should wait for a keyframe.
    pub after_loss: bool,
}

/// One Opus packet of the controlled PC's sound (48 kHz).
pub struct AudioPacket {
    pub data: Arc<[u8]>,
    pub time_48khz: u64,
}

/// A queue the engine fills and the app empties, waited on.
struct Queue<T> {
    items: Mutex<VecDeque<T>>,
    ready: Condvar,
}

impl<T> Queue<T> {
    fn new() -> Self {
        Queue {
            items: Mutex::new(VecDeque::new()),
            ready: Condvar::new(),
        }
    }

    /// Adds `item`; with more than `limit` waiting, the oldest go.
    fn push(&self, item: T, limit: usize) {
        if let Ok(mut items) = self.items.lock() {
            items.push_back(item);
            while items.len() > limit {
                items.pop_front();
            }
            self.ready.notify_one();
        }
    }

    fn pop(&self, timeout: Duration) -> Option<T> {
        let items = self.items.lock().ok()?;
        let (mut items, _) = self
            .ready
            .wait_timeout_while(items, timeout, |items| items.is_empty())
            .ok()?;
        items.pop_front()
    }

    fn clear(&self) {
        if let Ok(mut items) = self.items.lock() {
            items.clear();
        }
    }
}

/// What the app asks for (JSON, see `Core::command`).
enum Request {
    Login {
        server: String,
        pin: String,
        user: String,
        password: String,
    },
    Resume,
    LogOut,
    Connect(String),
    Disconnect,
    Input(InputEvent),
    Remote(RemoteCommand),
    Password(String),
    Clipboard(String),
    Text(String),
    RemoveDevice(String),
    /// Help the device with this help ID (another account's), with its code (empty: the person there decides).
    Help {
        id: String,
        code: String,
    },
    /// Wake the account's offline device up (Wake-on-LAN through another of its devices).
    Wake(String),
    Stop,
}

/// The core: an engine thread and the app's side of it.
pub struct Core {
    requests: Sender<Request>,
    events: Mutex<Receiver<String>>,
    frames: Arc<Queue<Frame>>,
    audio: Arc<Queue<AudioPacket>>,
    stopped: Arc<AtomicBool>,
    #[cfg(not(target_os = "android"))]
    opus: Mutex<Option<pcm::Decoder>>,
}

impl Core {
    /// Starts the engine. `folder`: where this device keeps its identity, key and saved login (the app's own files);
    /// `device_name`: as the device list shows this phone.
    pub fn start(folder: &str, device_name: &str) -> Core {
        // The account crate keeps its files in queqiao_core::config_folder(): on Android and iOS the app's folder.
        if !folder.is_empty() {
            // SAFETY: set once, before the engine thread and anything that reads the environment start.
            unsafe { std::env::set_var("XDG_CONFIG_HOME", folder) };
        }
        let (requests, incoming) = mpsc::channel();
        let (events_out, events) = mpsc::channel();
        let frames = Arc::new(Queue::new());
        let audio = Arc::new(Queue::new());
        let stopped = Arc::new(AtomicBool::new(false));
        let engine = Engine {
            requests: incoming,
            events: events_out,
            frames: frames.clone(),
            audio: audio.clone(),
            device_name: device_name.to_string(),
            account: None,
            session: None,
        };
        let stop = stopped.clone();
        std::thread::Builder::new()
            .name("queqiao-mobile".into())
            .spawn(move || {
                engine.run();
                stop.store(true, Ordering::Relaxed);
            })
            .expect("the engine thread");
        Core {
            requests,
            events: Mutex::new(events),
            frames,
            audio,
            stopped,
            #[cfg(not(target_os = "android"))]
            opus: Mutex::new(None),
        }
    }

    /// A command as JSON: {"login": {"server", "pin", "user", "password"}}, {"resume": {}}, {"log_out": {}},
    /// {"connect": {"device": id}}, {"disconnect": {}}, {"move": {"x", "y"}} (0..1 of the picture),
    /// {"button": {"button": "left"|"right"|"middle", "down": bool}}, {"wheel": {"delta", "horizontal"}},
    /// {"key": {"scancode", "extended", "down"}}, {"command": "ctrl_alt_del"|"lock"|...}, {"password": text},
    /// {"clipboard": text}, {"text": typed text, as characters on the PC}, {"remove_device": id},
    /// {"help": {"id", "code"}}, {"wake": id}, {"picture": {"fps", "quality": "smooth"|"balanced"|"sharp"}},
    /// {"resolution": short side or 0}, {"screen": n}, {"max_bitrate": kbit/s or 0}, {"mute_speakers": bool},
    /// {"lock_after": bool}. False for something it does not know.
    pub fn command(&self, json: &str) -> bool {
        match parse(json) {
            Some(request) => self.requests.send(request).is_ok(),
            None => false,
        }
    }

    /// The next event as JSON, waiting up to `timeout` (None: nothing came).
    pub fn next_event(&self, timeout: Duration) -> Option<String> {
        self.events.lock().ok()?.recv_timeout(timeout).ok()
    }

    pub fn next_frame(&self, timeout: Duration) -> Option<Frame> {
        self.frames.pop(timeout)
    }

    pub fn next_audio(&self, timeout: Duration) -> Option<AudioPacket> {
        self.audio.pop(timeout)
    }

    /// The next sound packet decoded into `pcm` (48 kHz stereo float, interleaved): the samples per channel (0 for a
    /// packet that would not decode) and its time in 48 kHz units; None when nothing came.
    #[cfg(not(target_os = "android"))]
    pub fn next_audio_pcm(&self, timeout: Duration, pcm: &mut [f32]) -> Option<(usize, u64)> {
        let packet = self.audio.pop(timeout)?;
        let mut decoder = self.opus.lock().ok()?;
        if decoder.is_none() {
            *decoder = pcm::Decoder::new().ok();
        }
        let samples = decoder
            .as_mut()
            .and_then(|d| d.decode(&packet.data, pcm).ok())
            .unwrap_or(0);
        Some((samples, packet.time_48khz))
    }

    /// Whether the engine has ended (after `stop`).
    pub fn stopped(&self) -> bool {
        self.stopped.load(Ordering::Relaxed)
    }
}

impl Drop for Core {
    fn drop(&mut self) {
        let _ = self.requests.send(Request::Stop);
    }
}

/// The codec as the apps number it: 0 H.264, 1 AV1, 2 H.265.
pub fn codec_number(codec: VideoCodec) -> u32 {
    match codec {
        VideoCodec::H264 => 0,
        VideoCodec::Av1 => 1,
        VideoCodec::H265 => 2,
    }
}

/// A JSON command as a request.
fn parse(json: &str) -> Option<Request> {
    let value: Value = serde_json::from_str(json).ok()?;
    let (name, body) = value.as_object()?.iter().next()?;
    let text = |key: &str| {
        body.get(key)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    let number = |key: &str| body.get(key).and_then(Value::as_f64).unwrap_or(0.0);
    let flag = |key: &str| body.get(key).and_then(Value::as_bool).unwrap_or(false);
    Some(match name.as_str() {
        "login" => Request::Login {
            server: text("server"),
            pin: text("pin"),
            user: text("user"),
            password: text("password"),
        },
        "resume" => Request::Resume,
        "log_out" => Request::LogOut,
        "connect" => Request::Connect(text("device")),
        "disconnect" => Request::Disconnect,
        "move" => Request::Input(InputEvent::MouseMove {
            x: (number("x").clamp(0.0, 1.0) * 65535.0) as u16,
            y: (number("y").clamp(0.0, 1.0) * 65535.0) as u16,
        }),
        "button" => Request::Input(InputEvent::MouseButton {
            button: match text("button").as_str() {
                "right" => MouseButton::Right,
                "middle" => MouseButton::Middle,
                _ => MouseButton::Left,
            },
            down: flag("down"),
        }),
        "wheel" => Request::Input(InputEvent::Wheel {
            delta: number("delta").clamp(-32768.0, 32767.0) as i16,
            horizontal: flag("horizontal"),
        }),
        "key" => Request::Input(InputEvent::Key {
            scancode: number("scancode") as u16,
            extended: flag("extended"),
            down: flag("down"),
        }),
        "command" => Request::Remote(match body.as_str()? {
            "ctrl_alt_del" => RemoteCommand::CtrlAltDel,
            "lock" => RemoteCommand::Lock,
            "restart" => RemoteCommand::Restart,
            "privacy_on" => RemoteCommand::PrivacyOn,
            "privacy_off" => RemoteCommand::PrivacyOff,
            "sound_on" => RemoteCommand::SoundOn,
            "sound_off" => RemoteCommand::SoundOff,
            _ => return None,
        }),
        "password" => Request::Password(body.as_str()?.to_string()),
        "clipboard" => Request::Clipboard(body.as_str()?.to_string()),
        "text" => Request::Text(body.as_str()?.to_string()),
        "remove_device" => Request::RemoveDevice(body.as_str()?.to_string()),
        "help" => Request::Help {
            id: text("id"),
            code: text("code"),
        },
        "wake" => Request::Wake(body.as_str()?.to_string()),
        "picture" => Request::Remote(RemoteCommand::Picture {
            fps: number("fps").clamp(0.0, 240.0) as u8,
            quality: match text("quality").as_str() {
                "smooth" => Quality::Smooth,
                "sharp" => Quality::Sharp,
                _ => Quality::Balanced,
            },
        }),
        "resolution" => Request::Remote(RemoteCommand::Resolution(body.as_u64()?.min(8192) as u16)),
        "screen" => Request::Remote(RemoteCommand::Screen(body.as_u64()?.min(255) as u8)),
        "max_bitrate" => Request::Remote(RemoteCommand::MaxBitrate(
            body.as_u64()?.min(1_000_000) as u32
        )),
        "mute_speakers" => Request::Remote(RemoteCommand::MuteSpeakers(body.as_bool()?)),
        "lock_after" => Request::Remote(RemoteCommand::LockAfter(body.as_bool()?)),
        _ => return None,
    })
}

/// The session in progress.
struct Session {
    peer: Peer,
    device: String,
    started: Instant,
    frames: u64,
    channel_open: bool,
    /// The figures of the second going on (frames, bytes), and when it began.
    second_frames: u32,
    second_bytes: usize,
    second_started: Instant,
    /// The last round trip (Ping to Pong), ms.
    rtt_ms: Option<f64>,
    ping_id: u32,
    relayed: bool,
}

struct Engine {
    requests: Receiver<Request>,
    events: Sender<String>,
    frames: Arc<Queue<Frame>>,
    audio: Arc<Queue<AudioPacket>>,
    device_name: String,
    account: Option<Account>,
    session: Option<Session>,
}

impl Engine {
    fn emit(&self, event: Value) {
        let _ = self.events.send(event.to_string());
    }

    fn run(mut self) {
        loop {
            // The app's requests first.
            loop {
                match self.requests.try_recv() {
                    Ok(Request::Stop) | Err(mpsc::TryRecvError::Disconnected) => {
                        self.end_session("the app stopped");
                        return;
                    }
                    Ok(request) => self.handle(request),
                    Err(mpsc::TryRecvError::Empty) => break,
                }
            }
            let mut busy = false;
            // The session's events.
            let events: Vec<PeerEvent> = match &self.session {
                Some(session) => std::iter::from_fn(|| session.peer.try_event())
                    .take(256)
                    .collect(),
                None => Vec::new(),
            };
            for event in events {
                busy = true;
                if let Some(ended) = self.on_peer(event) {
                    self.end_session(&ended);
                    break;
                }
            }
            // Once a second in a session: a round trip measured (Ping), and the figures for the app (as the desktop's card).
            let mut figures = None;
            if let Some(session) = self.session.as_mut()
                && session.channel_open
                && session.second_started.elapsed() >= Duration::from_secs(1)
            {
                let seconds = session.second_started.elapsed().as_secs_f64();
                let fps = f64::from(session.second_frames) / seconds;
                let mbps = session.second_bytes as f64 * 8.0 / seconds / 1e6;
                session.second_frames = 0;
                session.second_bytes = 0;
                session.second_started = Instant::now();
                session.ping_id = session.ping_id.wrapping_add(1);
                let sent_us = session.started.elapsed().as_micros() as u64;
                session.peer.send_data(
                    Message::Ping {
                        id: session.ping_id,
                        sent_us,
                    }
                    .encode(),
                );
                figures = Some(json!({"stats": {
                    "fps": (fps * 10.0).round() / 10.0,
                    "mbps": (mbps * 100.0).round() / 100.0,
                    "rtt_ms": session.rtt_ms.map(|ms| (ms * 10.0).round() / 10.0),
                    "relayed": session.relayed,
                }}));
            }
            if let Some(figures) = figures {
                self.emit(figures);
            }
            // The account's: waited on when nothing else is going on.
            let wait = if busy || self.session.is_some() {
                Duration::from_millis(2)
            } else {
                Duration::from_millis(50)
            };
            match self.account.as_mut() {
                Some(account) => {
                    if let Some(event) = account.next_event(wait) {
                        self.on_account(event);
                    }
                }
                None => std::thread::sleep(wait),
            }
        }
    }

    fn handle(&mut self, request: Request) {
        match request {
            Request::Login {
                server,
                pin,
                user,
                password,
            } => {
                let mut login = SavedLogin {
                    server,
                    pin,
                    refresh_token: None,
                };
                self.log_in(&mut login, Some((&user, &password)));
            }
            Request::Resume => match SavedLogin::load(AccountRole::Viewer) {
                Some(mut login) if login.refresh_token.is_some() => self.log_in(&mut login, None),
                _ => self.emit(json!({"logged_out": {}})),
            },
            Request::LogOut => {
                self.end_session("logged out");
                self.account = None;
                let _ = SavedLogin::default().save(AccountRole::Viewer);
                self.emit(json!({"logged_out": {}}));
            }
            Request::Connect(device) => self.connect(&device),
            Request::Disconnect => self.end_session("ended by the person"),
            Request::Input(event) => self.send(Message::Input(event)),
            Request::Remote(command) => self.send(Message::Command(command)),
            Request::Password(password) => self.send(Message::Password(password)),
            Request::Clipboard(text) => self.send(Message::ClipboardText(text)),
            // Long text in pieces of at most MAX_TYPE_TEXT bytes (on character boundaries).
            Request::Text(text) => {
                let mut piece = String::new();
                for c in text.chars() {
                    if piece.len() + c.len_utf8() > queqiao_core::protocol::MAX_TYPE_TEXT {
                        self.send(Message::TypeText(std::mem::take(&mut piece)));
                    }
                    piece.push(c);
                }
                if !piece.is_empty() {
                    self.send(Message::TypeText(piece));
                }
            }
            Request::RemoveDevice(device) => {
                if let Some(account) = &self.account {
                    account.remove_device(&device);
                }
            }
            Request::Wake(device) => {
                if let Some(account) = &self.account {
                    account.send(ClientMessage::Wake { device_id: device });
                }
            }
            Request::Help { id, code } => self.help(&id, &code),
            Request::Stop => {}
        }
    }

    fn send(&self, message: Message) {
        if let Some(session) = &self.session
            && session.channel_open
        {
            session.peer.send_data(message.encode());
        }
    }

    fn log_in(&mut self, login: &mut SavedLogin, password: Option<(&str, &str)>) {
        let device = match DeviceIdentity::load_or_create() {
            Ok(mut device) => {
                if !self.device_name.is_empty() {
                    device.name = self.device_name.clone();
                }
                device
            }
            Err(error) => {
                self.emit(json!({"error": {"what": "device", "message": error.to_string()}}));
                return;
            }
        };
        match Account::connect(login, &device, AccountRole::Viewer, password) {
            Ok(account) => {
                let _ = login.save(AccountRole::Viewer);
                self.emit(json!({"logged_in": {"device": device.id, "route": account.route()}}));
                self.account = Some(account);
            }
            Err(error) => {
                self.emit(json!({"error": {"what": "login", "message": error.to_string()}}));
            }
        }
    }

    fn on_account(&mut self, event: AccountEvent) {
        match event {
            AccountEvent::Devices(devices) => {
                let list: Vec<Value> = devices
                    .iter()
                    .map(|device| {
                        json!({
                            "id": device.id,
                            "name": device.name,
                            "online": device.online,
                            "controllable": device.controllable,
                        })
                    })
                    .collect();
                self.emit(json!({"devices": list}));
            }
            AccountEvent::Notice(text) => self.emit(json!({"notice": text})),
            AccountEvent::Error(text) => {
                self.emit(json!({"error": {"what": "server", "message": text}}))
            }
            AccountEvent::Closed(reason) => {
                self.account = None;
                self.emit(json!({"error": {"what": "server", "message": reason}}));
            }
            _ => {}
        }
    }

    /// Helping someone (as the desktop's Help someone): their help ID and code; when the code is right, a session with
    /// that PC. Without a code the person there is asked (the server waits for their answer).
    fn help(&mut self, help_id: &str, code: &str) {
        let help_id: String = help_id
            .chars()
            .filter(char::is_ascii_alphanumeric)
            .collect();
        let Some(account) = self.account.as_mut() else {
            self.emit(json!({"error": {"what": "help", "message": "not logged in"}}));
            return;
        };
        match account.request_help(&help_id, code.trim()) {
            Ok(device) => {
                self.emit(json!({"help_granted": {"id": device.id, "name": device.name}}));
                self.connect(&device.id);
            }
            Err(error) => {
                self.emit(json!({"error": {"what": "help", "message": error.to_string()}}));
            }
        }
    }

    fn connect(&mut self, device: &str) {
        self.end_session("another session");
        if self.account.is_none() {
            self.emit(json!({"error": {"what": "connect", "message": "not logged in"}}));
            return;
        }
        self.emit(json!({"session": {"state": "connecting", "device": device}}));
        let Some(account) = self.account.as_mut() else {
            return;
        };
        let id = new_session_id();
        let started = Instant::now();
        let peer = {
            let mut signal = account.session(device, AccountRole::Agent, &id);
            match signal.hello("viewer") {
                Ok(_) => Peer::start_with(Role::Viewer, &mut signal, Addresses::All)
                    .map_err(|e| e.to_string()),
                Err(error) => Err(error.to_string()),
            }
        };
        match peer {
            Ok(peer) => {
                self.frames.clear();
                self.audio.clear();
                self.session = Some(Session {
                    peer,
                    device: device.to_string(),
                    started,
                    frames: 0,
                    channel_open: false,
                    second_frames: 0,
                    second_bytes: 0,
                    second_started: Instant::now(),
                    rtt_ms: None,
                    ping_id: 0,
                    relayed: false,
                });
            }
            Err(error) => {
                self.emit(
                    json!({"session": {"state": "failed", "device": device, "message": error}}),
                );
            }
        }
    }

    /// One event of the session; Some(why) when it ends.
    fn on_peer(&mut self, event: PeerEvent) -> Option<String> {
        let session = self.session.as_mut()?;
        match event {
            PeerEvent::Connected => {
                let device = session.device.clone();
                let after = session.started.elapsed().as_millis() as u64;
                self.emit(
                    json!({"session": {"state": "connected", "device": device, "after_ms": after}}),
                );
            }
            PeerEvent::ChannelOpen => {
                session.channel_open = true;
                // The phone decodes H.264, and H.265 Main where its hardware does (most phones since 2016).
                session.peer.send_data(
                    Message::Command(RemoteCommand::VideoCodec {
                        prefer: CodecPreference::Automatic,
                        can_decode_av1: false,
                        can_decode_hevc: hevc::MAIN,
                        chroma444: false,
                        ten_bit: false,
                    })
                    .encode(),
                );
            }
            PeerEvent::Video {
                data,
                time_90khz,
                contiguous,
                codec,
                ..
            } => {
                session.frames += 1;
                session.second_frames += 1;
                session.second_bytes += data.len();
                let first = session.frames == 1;
                self.frames.push(
                    Frame {
                        data,
                        codec,
                        time_90khz,
                        after_loss: !contiguous,
                    },
                    120,
                );
                if first {
                    let after = session.started.elapsed().as_millis() as u64;
                    self.emit(json!({"session": {"state": "picture", "after_ms": after}}));
                }
            }
            PeerEvent::Audio { data, time_48khz } => {
                self.audio.push(AudioPacket { data, time_48khz }, 100);
            }
            PeerEvent::Path(path) => {
                session.relayed = path.starts_with("relayed");
                self.emit(json!({"session": {"state": "path", "path": path, "relayed": path.starts_with("relayed")}}));
            }
            PeerEvent::Data(bytes) => match Message::decode(&bytes) {
                Some(Message::Pong { id, sent_us }) if id == session.ping_id => {
                    let now = session.started.elapsed().as_micros() as u64;
                    session.rtt_ms = Some(now.saturating_sub(sent_us) as f64 / 1000.0);
                }
                Some(Message::ClipboardText(text)) => self.emit(json!({"clipboard": text})),
                Some(Message::Notice(text)) => self.emit(json!({"notice": text})),
                Some(Message::PasswordNeeded { reason, tries_left }) => {
                    self.emit(json!({"password_needed": {"reason": format!("{reason:?}"), "tries_left": tries_left}}));
                }
                Some(Message::PasswordResult { ok, tries_left }) => {
                    self.emit(json!({"password_result": {"ok": ok, "tries_left": tries_left}}));
                }
                Some(Message::Screens { current, sizes }) => {
                    let sizes: Vec<Value> = sizes.iter().map(|(w, h)| json!([w, h])).collect();
                    self.emit(json!({"screens": {"current": current, "sizes": sizes}}));
                }
                Some(Message::Leave) => return Some(PC_LEFT.to_string()),
                _ => {}
            },
            PeerEvent::Closed(reason) => return Some(reason),
            _ => {}
        }
        None
    }

    fn end_session(&mut self, why: &str) {
        if let Some(session) = self.session.take() {
            let seconds = session.started.elapsed().as_secs();
            // Tell the PC at once (as the desktop viewer does), unless it ended the session itself.
            if why != PC_LEFT {
                session.peer.send_data(Message::Leave.encode());
                std::thread::sleep(Duration::from_millis(100));
            }
            drop(session.peer);
            self.frames.clear();
            self.emit(json!({"session": {"state": "ended", "device": session.device, "why": why, "seconds": seconds, "frames": session.frames}}));
        }
    }
}
