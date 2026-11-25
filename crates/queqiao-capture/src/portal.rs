//! Screen capture on Wayland (docs/LINUX.md): the ScreenCast portal (xdg-desktop-portal, which GNOME, KDE and the
//! wlroots desktops each answer) opens a PipeWire stream of the monitor, read by pipewire.rs. The desktop asks the
//! person which screen to share (the wlroots portal can be set to share one without asking, as the tests do); the
//! pointer is drawn into the pictures. Frames come when the screen changed, as X11's DAMAGE gives them, and are handed
//! out in B, G, R, X order as the X11 capture's are. Where a still screen sends no first frame (wlroots), the screen is
//! damaged without a visible change until one comes (wayland_nudge.rs).

use std::sync::Arc;
use std::time::{Duration, Instant};

use queqiao_portal::PortalSession;

use crate::pipewire::{self, Stream};
use crate::wayland_nudge::Nudge;
use crate::{BgraImage, Error, Frame, OutputInfo, Result, qpc_now};

/// Whether this is a Wayland session (its programs talk to the compositor, and X11 reaches only XWayland's windows).
pub(crate) fn wayland_session() -> bool {
    let set = |name: &str| std::env::var_os(name).is_some_and(|value| !value.is_empty());
    set("WAYLAND_DISPLAY")
        && (std::env::var("XDG_SESSION_TYPE").is_ok_and(|kind| kind == "wayland")
            || !set("DISPLAY"))
}

pub(crate) struct PortalDuplicator {
    // Kept for the stream's life: the portal ends the cast when its session closes (shared with the input).
    portal: Arc<PortalSession>,
    stream: Stream,
    info: OutputInfo,
    /// The frame handed out last (B, G, R, X rows of `stride` bytes) and its number.
    pixels: Vec<u8>,
    stride: usize,
    seen: u64,
    /// Until the first picture: when the stream opened, the transparent surface that damages the screen (None
    /// before the first poke; an error where the compositor has none) and the last poke.
    opened: Instant,
    nudge: Option<std::result::Result<Nudge, String>>,
    poked: Option<Instant>,
}

// The stream's raw parts are PipeWire's, used under its lock.
unsafe impl Send for PortalDuplicator {}

impl PortalDuplicator {
    /// The portal session's monitor (the one the person, or the portal's settings, choose).
    pub(crate) fn new() -> Result<Self> {
        let portal = queqiao_portal::session().map_err(Error::new)?;
        let fd = portal.pipewire().map_err(Error::new)?;
        let stream = Stream::open(fd, portal.node, portal.size).map_err(Error::new)?;
        let mut duplicator = PortalDuplicator {
            info: OutputInfo {
                adapter: "Wayland".into(),
                output: portal.describe(),
                width: portal.size.0,
                height: portal.size.1,
                refresh_hz: 60.0,
                rotation: 0,
                method: "ScreenCast portal + PipeWire",
                left: portal.position.0,
                top: portal.position.1,
            },
            portal,
            stream,
            pixels: Vec::new(),
            stride: 0,
            seen: 0,
            opened: Instant::now(),
            nudge: None,
            poked: None,
        };
        // The format agreed says the size PipeWire really sends (pictures may not come yet: a still screen).
        let size = duplicator
            .stream
            .size(Duration::from_secs(5))
            .map_err(Error::new)?
            .ok_or_else(|| {
                Error::new("the ScreenCast portal agreed on no picture format within 5 s".into())
            })?;
        duplicator.info.width = size.0;
        duplicator.info.height = size.1;
        let _ = &duplicator.portal;
        Ok(duplicator)
    }

    pub(crate) fn info(&self) -> &OutputInfo {
        &self.info
    }

    /// Before the first picture: damages the screen (after 100 ms without one, then every 250 ms) where the
    /// compositor lets this (the layer shell), so a still screen sends one. More than once: xdg-desktop-portal-wlr may
    /// give the first change to a capture of an earlier session still waiting for one.
    fn nudge_until_first(&mut self) {
        if self.seen > 0
            || self.opened.elapsed() < Duration::from_millis(100)
            || self
                .poked
                .is_some_and(|at| at.elapsed() < Duration::from_millis(250))
        {
            return;
        }
        self.poked = Some(Instant::now());
        if let Ok(nudge) = self.nudge.get_or_insert_with(Nudge::new)
            && nudge.poke().is_err()
        {
            self.nudge = Some(Err("the poke failed".into()));
        }
    }

    /// The next frame: the newest since the last one, at once if one came, else after waiting up to `timeout_ms`.
    pub(crate) fn acquire(&mut self, timeout_ms: u32) -> Result<Option<Frame>> {
        self.nudge_until_first();
        let (width, height) = (self.info.width, self.info.height);
        let (pixels, stride) = (&mut self.pixels, &mut self.stride);
        let taken = self
            .stream
            .next(
                self.seen,
                Duration::from_millis(u64::from(timeout_ms)),
                |frame| {
                    if (frame.width, frame.height) != (width, height) {
                        return Err(Error::restart(format!(
                            "the shared screen is {}x{} now (was {width}x{height})",
                            frame.width, frame.height
                        )));
                    }
                    pixels.clear();
                    pixels.extend_from_slice(&frame.pixels);
                    *stride = frame.stride;
                    // R, G, B order (RGBx, RGBA): to B, G, R, X as the encoders take it.
                    if frame.format == pipewire::VIDEO_FORMAT_RGBX
                        || frame.format == pipewire::VIDEO_FORMAT_RGBA
                    {
                        for pixel in pixels.as_chunks_mut::<4>().0 {
                            pixel.swap(0, 2);
                        }
                    }
                    Ok((frame.sequence, frame.arrived_qpc))
                },
            )
            .map_err(Error::restart)?;
        let Some(taken) = taken else {
            return Ok(None);
        };
        let (sequence, arrived) = taken?;
        if self.pixels.len() < self.stride * height as usize {
            return Err(Error::restart(format!(
                "a picture of {} bytes for {width}x{height} (stride {})",
                self.pixels.len(),
                self.stride
            )));
        }
        let accumulated = (sequence - self.seen).min(u64::from(u32::MAX)) as u32;
        self.seen = sequence;
        // The first picture came: the surface goes.
        self.nudge = None;
        Ok(Some(Frame {
            present_qpc: arrived.max(1),
            acquired_qpc: qpc_now(),
            accumulated: accumulated.max(1),
            width,
            height,
            stride: self.stride,
            pixels: self.pixels.as_ptr(),
        }))
    }

    /// The screen now: the newest picture, as a BGRA image.
    pub(crate) fn snapshot_image(&self) -> Result<BgraImage> {
        let (width, height) = (self.info.width as usize, self.info.height as usize);
        self.stream
            .next(0, Duration::from_secs(2), |frame| {
                let mut pixels = Vec::with_capacity(width * height * 4);
                for row in frame.pixels.chunks(frame.stride.max(1)).take(height) {
                    pixels.extend_from_slice(&row[..(width * 4).min(row.len())]);
                }
                let rgb = frame.format == pipewire::VIDEO_FORMAT_RGBX
                    || frame.format == pipewire::VIDEO_FORMAT_RGBA;
                for pixel in pixels.as_chunks_mut::<4>().0 {
                    if rgb {
                        pixel.swap(0, 2);
                    }
                    pixel[3] = 255;
                }
                BgraImage {
                    width: width as u32,
                    height: height as u32,
                    pixels,
                }
            })
            .map_err(Error::new)?
            .ok_or_else(|| Error::new("no picture from the ScreenCast portal".into()))
    }
}
