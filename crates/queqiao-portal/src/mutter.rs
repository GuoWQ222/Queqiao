//! GNOME's own screen cast and remote desktop (mutter's org.gnome.Mutter.ScreenCast and .RemoteDesktop on the
//! session bus, the interfaces GNOME's own remote desktop service and its portal use; their description is mutter's
//! data/dbus-interfaces): one monitor's PipeWire stream and the input, without the portal's question. Only where the
//! person at the PC turned it on ("Control GNOME without asking", for the account's own devices: someone helping from
//! another account still goes through the portal and its question).
//!
//! A remote desktop session first, then a screen cast session tied to it ("remote-desktop-session-id"), recording the
//! primary monitor (connector "") or, for Super Screen, a virtual monitor (RecordVirtual: mutter makes it at the size
//! the PipeWire stream agrees on, and removes it when the stream or the session ends); starting the remote desktop
//! session starts both. The session lives while this program's bus connection does (mutter ends it when the
//! connection goes) and is stopped when dropped.

use std::collections::HashMap;
use std::os::fd::OwnedFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

const RD_NAME: &str = "org.gnome.Mutter.RemoteDesktop";
const RD_PATH: &str = "/org/gnome/Mutter/RemoteDesktop";
const RD_SESSION: &str = "org.gnome.Mutter.RemoteDesktop.Session";
const SC_NAME: &str = "org.gnome.Mutter.ScreenCast";
const SC_PATH: &str = "/org/gnome/Mutter/ScreenCast";
const SC_SESSION: &str = "org.gnome.Mutter.ScreenCast.Session";
const SC_STREAM: &str = "org.gnome.Mutter.ScreenCast.Stream";

/// RemoteDesktop's device types: keyboard 1, pointer 2.
const KEYBOARD_AND_POINTER: u32 = 3;
/// ScreenCast's cursor modes.
const CURSOR_EMBEDDED: u32 = 1;
const CURSOR_METADATA: u32 = 2;

fn error(what: &str) -> impl Fn(zbus::Error) -> String + '_ {
    move |e| format!("GNOME's remote desktop ({what}): {e}")
}

/// What a session records.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Source {
    /// The primary monitor.
    Monitor,
    /// A virtual monitor of this size (Super Screen).
    Virtual(u32, u32),
}

/// One mutter session: its bus connection, the remote desktop session (the input) and the stream recorded.
pub(crate) struct MutterSession {
    connection: Connection,
    remote: OwnedObjectPath,
    /// The stream's object path: absolute pointer moves name it.
    stream: String,
    pub(crate) node: u32,
    pub(crate) size: (u32, u32),
    pub(crate) position: (i32, i32),
    pub(crate) pointer_apart: bool,
    /// ScreenCast's API version (what the report says).
    pub(crate) version: i32,
    /// Stopped already (Super Screen going off stops its session while the capture and the input still hold it).
    stopped: AtomicBool,
}

fn proxy(
    connection: &Connection,
    name: &'static str,
    path: &str,
    interface: &'static str,
) -> Result<Proxy<'static>, String> {
    Proxy::new(connection, name, path.to_string(), interface).map_err(error("connect"))
}

/// A (x, y) pair of a stream's parameters ("size", "position").
fn pair(parameters: &HashMap<String, OwnedValue>, key: &str) -> Option<(i32, i32)> {
    let value = Value::from(parameters.get(key)?.try_clone().ok()?);
    <(i32, i32)>::try_from(value).ok()
}

impl MutterSession {
    pub(crate) fn open(source: Source) -> Result<MutterSession, String> {
        let connection = Connection::session().map_err(error("the session bus"))?;
        let desktop = proxy(&connection, RD_NAME, RD_PATH, RD_NAME)?;
        let devices: u32 = desktop
            .get_property("SupportedDeviceTypes")
            .map_err(error("its devices"))?;
        if devices & KEYBOARD_AND_POINTER != KEYBOARD_AND_POINTER {
            return Err(format!(
                "GNOME's remote desktop offers no keyboard and pointer (device types {devices})"
            ));
        }
        let remote: OwnedObjectPath = desktop
            .call("CreateSession", &())
            .map_err(error("CreateSession"))?;
        let session = proxy(&connection, RD_NAME, remote.as_str(), RD_SESSION)?;
        let stop = |session: &Proxy| {
            let _ = session.call_method("Stop", &());
        };
        let opened = (|| {
            let id: String = session
                .get_property("SessionId")
                .map_err(error("SessionId"))?;
            let cast = proxy(&connection, SC_NAME, SC_PATH, SC_NAME)?;
            let version: i32 = cast.get_property("Version").unwrap_or(1);
            let mut properties: HashMap<&str, Value> = HashMap::new();
            properties.insert("remote-desktop-session-id", Value::from(id.as_str()));
            let screen: OwnedObjectPath = cast
                .call("CreateSession", &(properties,))
                .map_err(error("ScreenCast CreateSession"))?;
            let screen = proxy(&connection, SC_NAME, screen.as_str(), SC_SESSION)?;
            // The pointer apart (metadata) from version 2 of the interface on; drawn into the pictures before.
            let pointer_apart = version >= 2;
            let mut record: HashMap<&str, Value> = HashMap::new();
            if version >= 2 {
                record.insert(
                    "cursor-mode",
                    Value::U32(if pointer_apart {
                        CURSOR_METADATA
                    } else {
                        CURSOR_EMBEDDED
                    }),
                );
            }
            let stream: OwnedObjectPath = match source {
                Source::Monitor => screen
                    .call("RecordMonitor", &("", record))
                    .map_err(error("RecordMonitor"))?,
                Source::Virtual(..) => {
                    // A monitor of the platform from version 3 on: GNOME treats it as a real one, not as a screen
                    // being shared.
                    if version >= 3 {
                        record.insert("is-platform", Value::Bool(true));
                    }
                    screen
                        .call("RecordVirtual", &(record,))
                        .map_err(error("RecordVirtual"))?
                }
            };
            let stream_proxy = proxy(&connection, SC_NAME, stream.as_str(), SC_STREAM)?;
            // The stream's PipeWire node comes as a signal once started: listened for first.
            let mut added = stream_proxy
                .receive_signal("PipeWireStreamAdded")
                .map_err(error("PipeWireStreamAdded"))?;
            let (node_sent, node_came) = mpsc::channel();
            std::thread::Builder::new()
                .name("queqiao-mutter-node".into())
                .spawn(move || {
                    if let Some(message) = added.next() {
                        let _ = node_sent.send(message.body().deserialize::<u32>());
                    }
                })
                .map_err(|e| e.to_string())?;
            session.call_method("Start", &()).map_err(error("Start"))?;
            let node = node_came
                .recv_timeout(Duration::from_secs(10))
                .map_err(|_| "GNOME's remote desktop gave no stream within 10 s".to_string())?
                .map_err(|e| format!("GNOME's remote desktop: its stream's node: {e}"))?;
            let parameters: HashMap<String, OwnedValue> =
                stream_proxy.get_property("Parameters").unwrap_or_default();
            // A virtual monitor has the size asked for (it is made once the stream agrees on it); where it goes in
            // the layout is mutter's (the stream's positions are the monitor's own).
            let size = match source {
                Source::Monitor => pair(&parameters, "size")
                    .map_or((1920, 1080), |(w, h)| (w.max(1) as u32, h.max(1) as u32)),
                Source::Virtual(width, height) => (width.max(1), height.max(1)),
            };
            let position = pair(&parameters, "position").unwrap_or((0, 0));
            Ok::<_, String>((
                stream.as_str().to_string(),
                node,
                size,
                position,
                pointer_apart,
                version,
            ))
        })();
        match opened {
            Ok((stream, node, size, position, pointer_apart, version)) => Ok(MutterSession {
                connection,
                remote,
                stream,
                node,
                size,
                position,
                pointer_apart,
                version,
                stopped: AtomicBool::new(false),
            }),
            Err(e) => {
                stop(&session);
                Err(e)
            }
        }
    }

    fn session(&self) -> Result<Proxy<'_>, String> {
        proxy(&self.connection, RD_NAME, self.remote.as_str(), RD_SESSION)
    }

    pub(crate) fn pointer_to(&self, x: f64, y: f64) -> Result<(), String> {
        const METHOD: &str = "NotifyPointerMotionAbsolute";
        let body = (self.stream.as_str(), x, y);
        self.session()?
            .call_method(METHOD, &body)
            .map(|_| ())
            .map_err(error(METHOD))
    }

    pub(crate) fn pointer_by(&self, dx: f64, dy: f64) -> Result<(), String> {
        const METHOD: &str = "NotifyPointerMotionRelative";
        self.session()?
            .call_method(METHOD, &(dx, dy))
            .map(|_| ())
            .map_err(error(METHOD))
    }

    pub(crate) fn button(&self, code: i32, down: bool) -> Result<(), String> {
        const METHOD: &str = "NotifyPointerButton";
        self.session()?
            .call_method(METHOD, &(code, down))
            .map(|_| ())
            .map_err(error(METHOD))
    }

    /// Axis 0 vertical, 1 horizontal (as the portal's).
    pub(crate) fn scroll(&self, vertical: bool, steps: i32) -> Result<(), String> {
        const METHOD: &str = "NotifyPointerAxisDiscrete";
        let body = (u32::from(!vertical), steps);
        self.session()?
            .call_method(METHOD, &body)
            .map(|_| ())
            .map_err(error(METHOD))
    }

    pub(crate) fn key(&self, code: i32, down: bool) -> Result<(), String> {
        const METHOD: &str = "NotifyKeyboardKeycode";
        let body = (code.max(0) as u32, down);
        self.session()?
            .call_method(METHOD, &body)
            .map(|_| ())
            .map_err(error(METHOD))
    }

    pub(crate) fn key_symbol(&self, keysym: i32, down: bool) -> Result<(), String> {
        const METHOD: &str = "NotifyKeyboardKeysym";
        let body = (keysym as u32, down);
        self.session()?
            .call_method(METHOD, &body)
            .map(|_| ())
            .map_err(error(METHOD))
    }

    /// A connection to the person's PipeWire (the stream's node is there; the portal would hand one over instead).
    pub(crate) fn pipewire(&self) -> Result<OwnedFd, String> {
        let runtime =
            std::env::var_os("XDG_RUNTIME_DIR").ok_or("no XDG_RUNTIME_DIR for PipeWire")?;
        let name = std::env::var_os("PIPEWIRE_REMOTE").unwrap_or_else(|| "pipewire-0".into());
        let path = std::path::Path::new(&runtime).join(name);
        let socket = std::os::unix::net::UnixStream::connect(&path)
            .map_err(|e| format!("PipeWire ({}): {e}", path.display()))?;
        Ok(OwnedFd::from(socket))
    }

    /// Stops the session now (its stream ends; a virtual monitor goes); once.
    pub(crate) fn stop(&self) {
        if !self.stopped.swap(true, Ordering::SeqCst)
            && let Ok(session) = self.session()
        {
            let _ = session.call_method("Stop", &());
        }
    }
}

impl Drop for MutterSession {
    fn drop(&mut self) {
        self.stop();
    }
}
