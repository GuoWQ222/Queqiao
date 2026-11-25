//! Screen capture on Linux under X11, with the same API as the Windows `Duplicator` (docs/LINUX.md):
//! - the pixels of one monitor through MIT-SHM: the X server writes them into memory shared with this process, one
//!   copy, no socket transfer;
//! - changes from the DAMAGE extension, so an unchanged screen costs nothing and `acquire` waits for a change (or its
//!   timeout) as Desktop Duplication does;
//! - the pointer's shape from XFixes (32-bit ARGB, the Windows "colour" kind 2); a fully transparent shape is a hidden
//!   pointer;
//! - the monitors from RandR (each a part of the one X screen).
//!
//! A frame stays valid until the next `acquire` or `release`, as on Windows: its pixels are the shared segment.

use std::os::fd::AsRawFd;
use std::time::Instant;

use x11rb::connection::{Connection, RequestConnection};
use x11rb::protocol::Event;
use x11rb::protocol::damage::{self, ConnectionExt as _};
use x11rb::protocol::randr::ConnectionExt as _;
use x11rb::protocol::shm::ConnectionExt as _;
use x11rb::protocol::xfixes::{self, ConnectionExt as _};
use x11rb::protocol::xproto::{self, ImageFormat};
use x11rb::rust_connection::RustConnection;

use crate::{BgraImage, OutputInfo, PointerShapeRaw, ScreenSummary};

/// One update of the screen: its pixels (B, G, R, X rows of `stride` bytes) in the shared segment.
pub struct Frame {
    /// When the change was seen (the monotonic clock, `qpc_now`); 0 if only the pointer moved.
    pub present_qpc: i64,
    /// When `acquire` returned.
    pub acquired_qpc: i64,
    /// Changes merged into this frame (DAMAGE reports since the last one).
    pub accumulated: u32,
    pub width: u32,
    pub height: u32,
    pub stride: usize,
    pub(crate) pixels: *const u8,
}

impl Frame {
    /// The frame's pixels: `height` rows of `stride` bytes, B, G, R, X each pixel.
    pub fn pixels(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.pixels, self.stride * self.height as usize) }
    }
}

/// The device frames are on, as Windows names its D3D11 device: on Linux frames are in memory, and an encoder on a GPU
/// (NVENC on CUDA, VA-API) opens what it needs itself, so this only lets the shared session code read the same.
#[derive(Clone, Debug, Default)]
pub struct Device;

/// The device's context (Windows: the immediate context); nothing on Linux.
#[derive(Clone, Debug, Default)]
pub struct Context;

/// A screen's device and place without capturing it, as Windows opens one (for a process that sees no screens, and the
/// encoder probe): on Linux the device is nothing to open, and the place comes from RandR.
pub struct ScreenDevice {
    pub device: Device,
    pub context: Context,
    pub info: OutputInfo,
    pub index: usize,
}

/// Screen `index` (as `screens` counts them), or the primary one.
pub fn screen_device(index: Option<usize>) -> Result<ScreenDevice> {
    let index = match index {
        Some(index) => index,
        None => screens()?.iter().position(|s| s.primary).unwrap_or(0),
    };
    let duplicator = Duplicator::new(index)?;
    Ok(ScreenDevice {
        device: Device,
        context: Context,
        info: duplicator.info().clone(),
        index,
    })
}

/// The primary screen's device; for a process that sees no screens (the Queqiao service's agent for the sign-in
/// screen, which has no display of its own), one whose place the desktop helper gives.
pub fn first_device() -> Result<ScreenDevice> {
    screen_device(None).or_else(|_| {
        Ok(ScreenDevice {
            device: Device,
            context: Context,
            info: OutputInfo {
                adapter: "KMS".into(),
                output: "the screen (through the desktop helper)".into(),
                width: 0,
                height: 0,
                refresh_hz: 60.0,
                rotation: 0,
                method: "the desktop helper (KMS)",
                left: 0,
                top: 0,
            },
            index: 0,
        })
    })
}

/// Another GPU that would be preferred to the one driving the screen (Windows: discrete before integrated); on Linux
/// the screen's pictures are in memory, so none is.
pub fn preferred_gpu_besides(_adapter: &str) -> Option<String> {
    None
}

/// A System V shared memory segment attached here and to the X server.
struct Segment {
    id: i32,
    address: *mut u8,
    size: usize,
    /// The X server's name for it.
    xid: u32,
}

impl Segment {
    fn new(connection: &RustConnection, size: usize) -> Result<Segment> {
        let id = unsafe { libc::shmget(libc::IPC_PRIVATE, size, libc::IPC_CREAT | 0o600) };
        if id < 0 {
            return Err(Error::new(format!(
                "shmget: {}",
                std::io::Error::last_os_error()
            )));
        }
        let address = unsafe { libc::shmat(id, std::ptr::null(), 0) };
        if address as isize == -1 {
            unsafe { libc::shmctl(id, libc::IPC_RMID, std::ptr::null_mut()) };
            return Err(Error::new(format!(
                "shmat: {}",
                std::io::Error::last_os_error()
            )));
        }
        let segment = Segment {
            id,
            address: address.cast(),
            size,
            xid: connection.generate_id().map_err(Error::from_x11)?,
        };
        connection
            .shm_attach(segment.xid, id as u32, false)
            .map_err(Error::from_x11)?
            .check()
            .map_err(Error::from_x11)?;
        // Marked for removal now: it goes as soon as both sides detached, also if this process ends abruptly.
        unsafe { libc::shmctl(id, libc::IPC_RMID, std::ptr::null_mut()) };
        Ok(segment)
    }
}

impl Drop for Segment {
    fn drop(&mut self) {
        unsafe { libc::shmdt(self.address.cast()) };
        let _ = self.id;
    }
}

/// Capture of one monitor of an X display.
/// The screen's updates, as Windows' Duplicator: from the X server (MIT-SHM, `X11Duplicator`), or in a Wayland
/// session from the ScreenCast portal through PipeWire (portal.rs), where X11 reaches only XWayland's windows.
pub struct Duplicator(Capture);

enum Capture {
    X11(Box<X11Duplicator>),
    Portal(Box<crate::portal::PortalDuplicator>),
}

impl Duplicator {
    /// The primary monitor (RandR's primary, else the first; on Wayland the one shared).
    pub fn primary() -> Result<Self> {
        Self::new(usize::MAX)
    }

    /// Monitor `index` of `screens()` (`usize::MAX`: the primary one). On Wayland the portal shares the monitor the
    /// person (or its settings) choose.
    pub fn new(index: usize) -> Result<Self> {
        if crate::portal::wayland_session() {
            return Ok(Duplicator(Capture::Portal(Box::new(
                crate::portal::PortalDuplicator::new()?,
            ))));
        }
        Ok(Duplicator(Capture::X11(Box::new(X11Duplicator::new(
            index,
        )?))))
    }

    pub fn info(&self) -> &OutputInfo {
        match &self.0 {
            Capture::X11(capture) => &capture.info,
            Capture::Portal(capture) => capture.info(),
        }
    }

    /// The device frames are on (as on Windows; see [`Device`]).
    pub fn device(&self) -> &Device {
        &Device
    }

    /// The device's context (as on Windows; see [`Context`]).
    pub fn context(&self) -> &Context {
        &Context
    }

    /// The next frame: at once if the screen changed since the last one, else after waiting up to `timeout_ms` for a
    /// change; `None` if nothing changed. The frame is valid until the next `acquire` or `release`.
    pub fn acquire(&mut self, timeout_ms: u32) -> Result<Option<Frame>> {
        match &mut self.0 {
            Capture::X11(capture) => capture.acquire(timeout_ms),
            Capture::Portal(capture) => capture.acquire(timeout_ms),
        }
    }

    pub fn release(&mut self) -> Result<()> {
        if let Capture::X11(capture) = &mut self.0 {
            capture.holding_frame = false;
        }
        Ok(())
    }

    /// The frame's pixels as a packed BGRA image (alpha set).
    pub fn copy_to_cpu(&mut self, frame: &Frame) -> Result<BgraImage> {
        let row = frame.width as usize * 4;
        let mut pixels = Vec::with_capacity(row * frame.height as usize);
        for line in frame
            .pixels()
            .chunks(frame.stride.max(1))
            .take(frame.height as usize)
        {
            pixels.extend_from_slice(&line[..row.min(line.len())]);
        }
        for pixel in pixels.as_chunks_mut::<4>().0 {
            pixel[3] = 255;
        }
        Ok(BgraImage {
            width: frame.width,
            height: frame.height,
            pixels,
        })
    }

    /// The screen now, as a BGRA image (not counted as a frame).
    pub fn snapshot_image(&self) -> Result<BgraImage> {
        match &self.0 {
            Capture::X11(capture) => capture.snapshot_image(),
            Capture::Portal(capture) => capture.snapshot_image(),
        }
    }

    /// A new pointer shape since it was last taken (on Wayland where the portal gives the pointer apart; else it is
    /// in the pictures).
    pub fn take_pointer_shape(&mut self) -> Option<PointerShapeRaw> {
        match &mut self.0 {
            Capture::X11(capture) => capture.pointer_shape.take(),
            Capture::Portal(capture) => capture.take_pointer_shape(),
        }
    }

    /// The pointer's visibility, when it changed since it was last taken.
    pub fn take_pointer_visible(&mut self) -> Option<bool> {
        match &mut self.0 {
            Capture::X11(capture) => capture.take_pointer_visible(),
            Capture::Portal(capture) => capture.take_pointer_visible(),
        }
    }
}

/// The X server's screen: MIT-SHM, DAMAGE for changes, XFixes for the pointer.
pub(crate) struct X11Duplicator {
    connection: RustConnection,
    root: u32,
    info: OutputInfo,
    segment: Segment,
    damage: u32,
    /// DAMAGE reports not yet in a frame, and when the first of them came.
    changes: u32,
    changed_at: i64,
    /// The first frame is the whole screen, changed or not.
    first: bool,
    holding_frame: bool,
    pointer_shape: Option<PointerShapeRaw>,
    pointer_visible: Option<bool>,
    pointer_visible_changed: bool,
}

// The connection and the segment are used from one thread at a time.
unsafe impl Send for X11Duplicator {}

impl X11Duplicator {
    /// Monitor `index` of `screens()` (`usize::MAX`: the primary one).
    fn new(index: usize) -> Result<Self> {
        let (connection, screen_number) = x11rb::connect(None).map_err(Error::from_x11)?;
        let root = connection.setup().roots[screen_number].root;
        for (name, extension) in [
            ("MIT-SHM", x11rb::protocol::shm::X11_EXTENSION_NAME),
            ("DAMAGE", damage::X11_EXTENSION_NAME),
            ("XFIXES", xfixes::X11_EXTENSION_NAME),
        ] {
            if connection
                .extension_information(extension)
                .map_err(Error::from_x11)?
                .is_none()
            {
                return Err(Error::new(format!("the X server has no {name}")));
            }
        }
        damage::ConnectionExt::damage_query_version(&connection, 1, 1)
            .map_err(Error::from_x11)?
            .reply()
            .map_err(Error::from_x11)?;
        connection
            .xfixes_query_version(5, 0)
            .map_err(Error::from_x11)?
            .reply()
            .map_err(Error::from_x11)?;
        let monitors = monitors(&connection, root)?;
        let chosen = if index == usize::MAX {
            monitors.iter().position(|m| m.primary).unwrap_or(0)
        } else {
            index
        };
        let monitor = monitors
            .get(chosen)
            .ok_or_else(|| Error::new(format!("no screen {index} ({} found)", monitors.len())))?;
        let size = monitor.width as usize * monitor.height as usize * 4;
        let segment = Segment::new(&connection, size)?;
        let damage = connection.generate_id().map_err(Error::from_x11)?;
        connection
            .damage_create(damage, root, damage::ReportLevel::NON_EMPTY)
            .map_err(Error::from_x11)?;
        connection
            .xfixes_select_cursor_input(root, xfixes::CursorNotifyMask::DISPLAY_CURSOR)
            .map_err(Error::from_x11)?;
        connection.flush().map_err(Error::from_x11)?;
        let info = OutputInfo {
            adapter: "X11".into(),
            output: monitor.name.clone(),
            width: monitor.width,
            height: monitor.height,
            refresh_hz: monitor.refresh_hz,
            rotation: 0,
            method: "MIT-SHM",
            left: monitor.left,
            top: monitor.top,
        };
        let mut duplicator = X11Duplicator {
            connection,
            root,
            info,
            segment,
            damage,
            changes: 0,
            changed_at: 0,
            first: true,
            holding_frame: false,
            pointer_shape: None,
            pointer_visible: None,
            pointer_visible_changed: false,
        };
        duplicator.read_pointer();
        Ok(duplicator)
    }

    fn acquire(&mut self, timeout_ms: u32) -> Result<Option<Frame>> {
        self.holding_frame = false;
        self.drain_events()?;
        if self.changes == 0 && !self.first {
            if !self.wait(timeout_ms)? {
                return Ok(None);
            }
            self.drain_events()?;
            if self.changes == 0 {
                return Ok(None);
            }
        }
        // The changes so far are in this frame: the next report is a new change.
        self.connection
            .damage_subtract(self.damage, x11rb::NONE, x11rb::NONE)
            .map_err(Error::from_x11)?;
        let (width, height) = (self.info.width, self.info.height);
        self.connection
            .shm_get_image(
                self.root,
                self.info.left as i16,
                self.info.top as i16,
                width as u16,
                height as u16,
                !0,
                ImageFormat::Z_PIXMAP.into(),
                self.segment.xid,
                0,
            )
            .map_err(Error::from_x11)?
            .reply()
            .map_err(Error::from_x11)?;
        let present = if self.first {
            qpc_now()
        } else {
            self.changed_at
        };
        let frame = Frame {
            present_qpc: present.max(1),
            acquired_qpc: qpc_now(),
            accumulated: self.changes.max(1),
            width,
            height,
            stride: width as usize * 4,
            pixels: self.segment.address,
        };
        self.first = false;
        self.changes = 0;
        self.holding_frame = true;
        Ok(Some(frame))
    }

    fn snapshot_image(&self) -> Result<BgraImage> {
        // A plain GetImage of the monitor, apart from the shared memory and the changes counted (as Windows takes a GDI
        // screenshot beside desktop duplication).
        let info = &self.info;
        let reply = xproto::get_image(
            &self.connection,
            ImageFormat::Z_PIXMAP,
            self.root,
            info.left as i16,
            info.top as i16,
            info.width as u16,
            info.height as u16,
            !0,
        )
        .map_err(Error::from_x11)?
        .reply()
        .map_err(Error::from_x11)?;
        let size = info.width as usize * info.height as usize * 4;
        if reply.depth < 24 || reply.data.len() < size {
            return Err(Error::new(format!(
                "a screenshot of depth {} with {} bytes",
                reply.depth,
                reply.data.len()
            )));
        }
        let mut pixels = reply.data;
        pixels.truncate(size);
        for pixel in pixels.as_chunks_mut::<4>().0 {
            pixel[3] = 255;
        }
        Ok(BgraImage {
            width: info.width,
            height: info.height,
            pixels,
        })
    }

    fn take_pointer_visible(&mut self) -> Option<bool> {
        if !self.pointer_visible_changed {
            return None;
        }
        self.pointer_visible_changed = false;
        self.pointer_visible
    }

    /// Waits up to `timeout_ms` for an event from the X server; true if one may have come.
    fn wait(&mut self, timeout_ms: u32) -> Result<bool> {
        let mut poll = libc::pollfd {
            fd: self.connection.stream().as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let ready = unsafe { libc::poll(&mut poll, 1, timeout_ms as i32) };
        if ready < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                return Ok(false);
            }
            return Err(Error::new(format!("poll: {error}")));
        }
        Ok(ready > 0)
    }

    /// Takes the events already here: screen changes, pointer shapes.
    fn drain_events(&mut self) -> Result<()> {
        let mut shape_changed = false;
        while let Some(event) = self.connection.poll_for_event().map_err(Error::from_x11)? {
            match event {
                Event::DamageNotify(_) => {
                    if self.changes == 0 {
                        self.changed_at = qpc_now();
                    }
                    self.changes += 1;
                }
                Event::XfixesCursorNotify(_) => shape_changed = true,
                _ => {}
            }
        }
        if shape_changed {
            self.read_pointer();
        }
        Ok(())
    }

    /// The pointer's current shape, kept to be taken; a shape with nothing visible is a hidden pointer.
    fn read_pointer(&mut self) {
        let Ok(Ok(image)) = self
            .connection
            .xfixes_get_cursor_image()
            .map(|cookie| cookie.reply())
        else {
            return;
        };
        let visible = image.cursor_image.iter().any(|argb| argb >> 24 != 0);
        if self.pointer_visible != Some(visible) {
            self.pointer_visible = Some(visible);
            self.pointer_visible_changed = true;
        }
        if !visible {
            return;
        }
        // ARGB words (premultiplied) as B, G, R, A bytes: the "colour" kind Windows gives.
        let data: Vec<u8> = image
            .cursor_image
            .iter()
            .flat_map(|argb| argb.to_le_bytes())
            .collect();
        self.pointer_shape = Some(PointerShapeRaw {
            kind: 2,
            width: u32::from(image.width),
            height: u32::from(image.height),
            pitch: u32::from(image.width) * 4,
            hot_x: i32::from(image.xhot),
            hot_y: i32::from(image.yhot),
            data,
        });
    }
}

impl Drop for X11Duplicator {
    fn drop(&mut self) {
        let _ = self.connection.damage_destroy(self.damage);
        let _ = self.connection.shm_detach(self.segment.xid);
        let _ = self.connection.flush();
        let _ = self.segment.size;
    }
}

/// One monitor: its place on the X screen.
#[derive(Clone, Debug)]
pub struct Monitor {
    pub name: String,
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
    pub primary: bool,
    pub refresh_hz: f64,
}

/// The monitors (RandR 1.5's list; without RandR, the whole screen as one).
pub fn monitors(connection: &RustConnection, root: u32) -> Result<Vec<Monitor>> {
    let listed = connection
        .randr_get_monitors(root, true)
        .ok()
        .and_then(|cookie| cookie.reply().ok());
    let mut found: Vec<Monitor> = listed
        .map(|reply| {
            reply
                .monitors
                .iter()
                .map(|monitor| Monitor {
                    name: atom_name(connection, monitor.name)
                        .unwrap_or_else(|| format!("monitor {}", monitor.name)),
                    left: i32::from(monitor.x),
                    top: i32::from(monitor.y),
                    width: u32::from(monitor.width),
                    height: u32::from(monitor.height),
                    primary: monitor.primary,
                    refresh_hz: 60.0,
                })
                .collect()
        })
        .unwrap_or_default();
    if found.is_empty() {
        let screen = connection
            .setup()
            .roots
            .iter()
            .find(|screen| screen.root == root)
            .ok_or_else(|| Error::new("no X screen".into()))?;
        found.push(Monitor {
            name: "screen".into(),
            left: 0,
            top: 0,
            width: u32::from(screen.width_in_pixels),
            height: u32::from(screen.height_in_pixels),
            primary: true,
            refresh_hz: 60.0,
        });
    }
    Ok(found)
}

fn atom_name(connection: &RustConnection, atom: u32) -> Option<String> {
    let reply = xproto::ConnectionExt::get_atom_name(connection, atom)
        .ok()?
        .reply()
        .ok()?;
    Some(String::from_utf8_lossy(&reply.name).into_owned())
}

/// The monitors as `Duplicator::new` counts them.
pub fn screens() -> Result<Vec<ScreenSummary>> {
    let (connection, screen_number) = x11rb::connect(None).map_err(Error::from_x11)?;
    let root = connection.setup().roots[screen_number].root;
    Ok(monitors(&connection, root)?
        .into_iter()
        .enumerate()
        .map(|(index, monitor)| ScreenSummary {
            index,
            width: monitor.width,
            height: monitor.height,
            primary: monitor.primary,
        })
        .collect())
}

/// A capture error; `restart` when capturing must start again (the X server's connection broke, or the screen's
/// size changed under it), as Windows' DXGI_ERROR_ACCESS_LOST.
#[derive(Clone, Debug)]
pub struct Error {
    message: String,
    restart: bool,
}

impl Error {
    pub fn new(message: String) -> Error {
        Error {
            message,
            restart: false,
        }
    }

    /// An error after which capturing starts again.
    pub(crate) fn restart(message: impl Into<String>) -> Error {
        Error {
            message: message.into(),
            restart: true,
        }
    }

    fn from_x11(error: impl std::fmt::Display) -> Error {
        Error {
            message: format!("X11: {error}"),
            restart: true,
        }
    }

    pub fn message(&self) -> String {
        self.message.clone()
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

/// True if capturing must start again (as on Windows).
pub fn needs_restart(error: &Error) -> bool {
    error.restart
}

/// The monotonic clock in nanoseconds: the clock of [`Frame::present_qpc`] (Windows' QueryPerformanceCounter there).
pub fn qpc_now() -> i64 {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    let start = *START.get_or_init(Instant::now);
    // From a start of this process's own: positive, and 292 years before it wraps.
    start.elapsed().as_nanos() as i64 + 1
}

/// Ticks of `qpc_now` per second.
pub fn qpc_frequency() -> i64 {
    1_000_000_000
}
