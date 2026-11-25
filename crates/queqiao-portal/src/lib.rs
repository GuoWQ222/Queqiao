//! The desktop portal session of a Wayland desktop (docs/LINUX.md), shared by the capture (its PipeWire stream) and the
//! input (its RemoteDesktop calls): one session for one Queqiao session, closed when both let go of it.
//!
//! - Where the desktop offers the RemoteDesktop portal (GNOME, KDE): a RemoteDesktop session with the keyboard and the
//!   pointer, carrying the ScreenCast stream of one monitor. Input goes through it (`NotifyPointerMotionAbsolute` on the
//!   stream, buttons and keys as Linux input codes).
//! - Elsewhere (the wlroots desktops, whose portal has no RemoteDesktop): a ScreenCast session; input goes through the
//!   compositor's virtual pointer and keyboard (queqiao-input).
//!
//! The desktop asks the person at the PC once (its own dialog: Queqiao never goes round it). Where the portal can keep
//! that answer, the session asks to (persist mode "until revoked") and its restore token is kept in
//! `$XDG_CONFIG_HOME/queqiao/` (0600): later sessions start without asking until the person revokes it in the
//! desktop's settings. xdg-desktop-portal keeps RemoteDesktop answers from version 2 of that interface (1.17 and later);
//! earlier, GNOME and KDE ask each session.

#![cfg(target_os = "linux")]

use std::os::fd::OwnedFd;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, Weak};

use ashpd::desktop::remote_desktop::{
    Axis, DeviceType, KeyState, RemoteDesktop, SelectDevicesOptions,
};
use ashpd::desktop::screencast::{CursorMode, Screencast, SelectSourcesOptions, SourceType};
use ashpd::desktop::{PersistMode, Session};
use enumflags2::BitFlags;

/// How the session was made.
enum Kind {
    RemoteDesktop {
        proxy: RemoteDesktop,
        session: Session<RemoteDesktop>,
    },
    ScreenCast {
        session: Session<Screencast>,
    },
}

/// One portal session: one monitor's PipeWire stream, and on RemoteDesktop desktops the input.
pub struct PortalSession {
    _screencast: Screencast,
    kind: Kind,
    /// The PipeWire node of the stream, its size and place (logical pixels).
    pub node: u32,
    pub size: (u32, u32),
    pub position: (i32, i32),
    /// The connection to PipeWire, taken once by the capture.
    pipewire: Mutex<Option<OwnedFd>>,
    /// Whether the session went to the desktop without an earlier answer (the person is asked). False: the kept
    /// answer was offered; the desktop asks again only where it no longer applies, and the portal does not say which
    /// (each start answers with a new token, as tokens are good once).
    pub asked: bool,
    /// The pointer apart from the pictures, as metadata of the stream (where the portal offers that mode), as on
    /// Windows and X11: the viewer draws it. Else the portal draws it into the pictures.
    pub pointer_apart: bool,
}

static SHARED: Mutex<Weak<PortalSession>> = Mutex::new(Weak::new());

/// The session of this Queqiao session: the one open, or a new one (the desktop may ask the person first).
pub fn session() -> Result<Arc<PortalSession>, String> {
    let mut shared = SHARED
        .lock()
        .map_err(|_| "the portal session broke".to_string())?;
    if let Some(session) = shared.upgrade() {
        return Ok(session);
    }
    let session = Arc::new(PortalSession::open()?);
    *shared = Arc::downgrade(&session);
    Ok(session)
}

/// Where a kept answer's restore token lives.
fn token_file(kind: &str) -> Option<PathBuf> {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
    Some(config.join("queqiao").join(format!("portal-{kind}.token")))
}

fn read_token(kind: &str) -> Option<String> {
    let text = std::fs::read_to_string(token_file(kind)?).ok()?;
    let token = text.trim().to_string();
    (!token.is_empty() && token.len() < 256).then_some(token)
}

fn save_token(kind: &str, token: Option<&str>) {
    let Some(path) = token_file(kind) else {
        return;
    };
    match token {
        Some(token) => {
            use std::io::Write;
            use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
            if let Some(folder) = path.parent() {
                let _ = std::fs::DirBuilder::new()
                    .recursive(true)
                    .mode(0o700)
                    .create(folder);
            }
            if let Ok(mut file) = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&path)
            {
                let _ = file.write_all(token.as_bytes());
            }
        }
        None => {
            let _ = std::fs::remove_file(path);
        }
    }
}

fn portal_error(error: ashpd::Error) -> String {
    format!("the desktop portal: {error}")
}

impl PortalSession {
    fn open() -> Result<PortalSession, String> {
        async_io::block_on(async {
            let screencast = Screencast::new().await.map_err(portal_error)?;
            // The pointer apart where the portal offers it (GNOME, KDE); drawn into the pictures elsewhere.
            let pointer_apart = screencast
                .available_cursor_modes()
                .await
                .is_ok_and(|modes| modes.contains(CursorMode::Metadata));
            let cursor_mode = if pointer_apart {
                CursorMode::Metadata
            } else {
                CursorMode::Embedded
            };
            let sources = || {
                SelectSourcesOptions::default()
                    .set_cursor_mode(cursor_mode)
                    .set_sources(BitFlags::from(SourceType::Monitor))
                    .set_multiple(false)
            };
            // RemoteDesktop where the desktop offers it (with the keyboard and the pointer).
            let remote = match RemoteDesktop::new().await {
                Ok(proxy) => match proxy.available_device_types().await {
                    Ok(types)
                        if types.contains(DeviceType::Keyboard)
                            && types.contains(DeviceType::Pointer) =>
                    {
                        Some(proxy)
                    }
                    _ => None,
                },
                Err(_) => None,
            };
            let (kind, response) = match remote {
                Some(proxy) => {
                    let session = proxy
                        .create_session(Default::default())
                        .await
                        .map_err(portal_error)?;
                    let token = read_token("remote-desktop");
                    let mut devices = SelectDevicesOptions::default()
                        .set_devices(DeviceType::Keyboard | DeviceType::Pointer);
                    // Answers are kept from version 2 of RemoteDesktop.
                    if proxy.version() >= 2 {
                        devices = devices
                            .set_persist_mode(PersistMode::ExplicitlyRevoked)
                            .set_restore_token(token.as_deref());
                    }
                    proxy
                        .select_devices(&session, devices)
                        .await
                        .map_err(portal_error)?;
                    screencast
                        .select_sources(&session, sources())
                        .await
                        .map_err(portal_error)?;
                    let started = proxy
                        .start(&session, None, Default::default())
                        .await
                        .map_err(portal_error)?
                        .response()
                        .map_err(portal_error)?;
                    let devices = started.devices();
                    if !devices.contains(DeviceType::Pointer)
                        || !devices.contains(DeviceType::Keyboard)
                    {
                        return Err(format!(
                            "the person did not allow the keyboard and the pointer (devices granted: {devices:?})"
                        ));
                    }
                    if proxy.version() >= 2 {
                        save_token("remote-desktop", started.restore_token());
                    }
                    let stream = started
                        .streams()
                        .first()
                        .cloned()
                        .ok_or("the portal shared no screen")?;
                    let asked = token.is_none() || proxy.version() < 2;
                    (Kind::RemoteDesktop { proxy, session }, (stream, asked))
                }
                None => {
                    let session = screencast
                        .create_session(Default::default())
                        .await
                        .map_err(portal_error)?;
                    let token = read_token("screencast");
                    let options = if screencast.version() >= 4 {
                        sources()
                            .set_persist_mode(PersistMode::ExplicitlyRevoked)
                            .set_restore_token(token.as_deref())
                    } else {
                        sources()
                    };
                    screencast
                        .select_sources(&session, options)
                        .await
                        .map_err(portal_error)?;
                    let started = screencast
                        .start(&session, None, Default::default())
                        .await
                        .map_err(portal_error)?
                        .response()
                        .map_err(portal_error)?;
                    if screencast.version() >= 4 {
                        save_token("screencast", started.restore_token());
                    }
                    let stream = started
                        .streams()
                        .first()
                        .cloned()
                        .ok_or("the portal shared no screen")?;
                    let asked = token.is_none() || screencast.version() < 4;
                    (Kind::ScreenCast { session }, (stream, asked))
                }
            };
            let (stream, asked) = response;
            let pipewire = match &kind {
                Kind::RemoteDesktop { session, .. } => {
                    screencast
                        .open_pipe_wire_remote(session, Default::default())
                        .await
                }
                Kind::ScreenCast { session } => {
                    screencast
                        .open_pipe_wire_remote(session, Default::default())
                        .await
                }
            }
            .map_err(portal_error)?;
            let size = stream
                .size()
                .map_or((1920, 1080), |(w, h)| (w.max(1) as u32, h.max(1) as u32));
            Ok(PortalSession {
                _screencast: screencast,
                kind,
                node: stream.pipe_wire_node_id(),
                size,
                position: stream.position().unwrap_or((0, 0)),
                pipewire: Mutex::new(Some(pipewire)),
                asked,
                pointer_apart,
            })
        })
    }

    /// A connection to PipeWire for the stream: the one opened with the session, then a new one each time (a capture
    /// that starts again).
    pub fn pipewire(&self) -> Result<OwnedFd, String> {
        if let Some(fd) = self.pipewire.lock().ok().and_then(|mut fd| fd.take()) {
            return Ok(fd);
        }
        async_io::block_on(match &self.kind {
            Kind::RemoteDesktop { session, .. } => Box::pin(
                self._screencast
                    .open_pipe_wire_remote(session, Default::default()),
            )
                as std::pin::Pin<
                    Box<dyn std::future::Future<Output = Result<OwnedFd, ashpd::Error>>>,
                >,
            Kind::ScreenCast { session } => Box::pin(
                self._screencast
                    .open_pipe_wire_remote(session, Default::default()),
            ),
        })
        .map_err(portal_error)
    }

    /// Whether input goes through this session (RemoteDesktop).
    pub fn has_input(&self) -> bool {
        matches!(self.kind, Kind::RemoteDesktop { .. })
    }

    /// What the session is, for the reports.
    pub fn describe(&self) -> String {
        format!(
            "{} portal, {}x{} stream (node {}){}{}",
            if self.has_input() {
                "RemoteDesktop"
            } else {
                "ScreenCast"
            },
            self.size.0,
            self.size.1,
            self.node,
            if self.pointer_apart {
                ", the pointer apart"
            } else {
                ", the pointer in the pictures"
            },
            if self.asked {
                ""
            } else {
                ", the person's earlier answer offered"
            }
        )
    }

    fn remote(&self) -> Result<(&RemoteDesktop, &Session<RemoteDesktop>), String> {
        match &self.kind {
            Kind::RemoteDesktop { proxy, session } => Ok((proxy, session)),
            Kind::ScreenCast { .. } => Err("no input through this portal session".into()),
        }
    }

    /// Makes the session's pointer and keyboard part of the desktop's seat now, before the person's input. GNOME
    /// adds them only with their first event, and on a PC with no keyboard or mouse of its own (a server, a virtual
    /// machine) programs learn of them after that event: the first key pressed or the first click would reach no
    /// program. A move by nothing brings in the pointer; the keyboard needs a key pressed: KEY_UNKNOWN, which has no
    /// meaning in the keymaps (no symbol), so neither the desktop nor a program acts on it.
    pub fn bring_in_devices(&self) {
        const KEY_UNKNOWN: i32 = 240;
        let _ = self.pointer_by(0.0, 0.0);
        let _ = self.key(KEY_UNKNOWN, true);
        let _ = self.key(KEY_UNKNOWN, false);
    }

    /// The pointer to (`x`, `y`) of the stream (logical pixels).
    pub fn pointer_to(&self, x: f64, y: f64) -> Result<(), String> {
        let (proxy, session) = self.remote()?;
        async_io::block_on(proxy.notify_pointer_motion_absolute(
            session,
            self.node,
            x,
            y,
            Default::default(),
        ))
        .map_err(portal_error)
    }

    pub fn pointer_by(&self, dx: f64, dy: f64) -> Result<(), String> {
        let (proxy, session) = self.remote()?;
        async_io::block_on(proxy.notify_pointer_motion(session, dx, dy, Default::default()))
            .map_err(portal_error)
    }

    /// A button (a Linux input code, BTN_LEFT ...).
    pub fn button(&self, code: i32, down: bool) -> Result<(), String> {
        let (proxy, session) = self.remote()?;
        let state = if down {
            KeyState::Pressed
        } else {
            KeyState::Released
        };
        async_io::block_on(proxy.notify_pointer_button(session, code, state, Default::default()))
            .map_err(portal_error)
    }

    /// Wheel notches: positive is down (vertical) or right (horizontal).
    pub fn scroll(&self, vertical: bool, steps: i32) -> Result<(), String> {
        let (proxy, session) = self.remote()?;
        let axis = if vertical {
            Axis::Vertical
        } else {
            Axis::Horizontal
        };
        async_io::block_on(proxy.notify_pointer_axis_discrete(
            session,
            axis,
            steps,
            Default::default(),
        ))
        .map_err(portal_error)
    }

    /// A key (a Linux input code, evdev).
    pub fn key(&self, code: i32, down: bool) -> Result<(), String> {
        let (proxy, session) = self.remote()?;
        let state = if down {
            KeyState::Pressed
        } else {
            KeyState::Released
        };
        async_io::block_on(proxy.notify_keyboard_keycode(session, code, state, Default::default()))
            .map_err(portal_error)
    }
}

impl Drop for PortalSession {
    fn drop(&mut self) {
        let _ = async_io::block_on(async {
            match &self.kind {
                Kind::RemoteDesktop { session, .. } => session.close().await,
                Kind::ScreenCast { session } => session.close().await,
            }
        });
    }
}
