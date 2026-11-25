//! Screen capture on Wayland (docs/LINUX.md): the ScreenCast portal (xdg-desktop-portal, which GNOME, KDE and the
//! wlroots desktops each answer) opens a PipeWire stream of the monitor, read by pipewire.rs. The desktop asks the
//! person which screen to share (the wlroots portal can be set to share one without asking, as the tests do); the
//! pointer is drawn into the pictures. Frames come when the screen changed, as X11's DAMAGE gives them, and are handed
//! out in B, G, R, X order as the X11 capture's are.

use std::time::Duration;

use ashpd::desktop::PersistMode;
use ashpd::desktop::Session;
use ashpd::desktop::screencast::{CursorMode, Screencast, SelectSourcesOptions, SourceType};

use crate::pipewire::{self, Stream};
use crate::{BgraImage, Error, Frame, OutputInfo, Result, qpc_now};

/// Whether this is a Wayland session (its programs talk to the compositor, and X11 reaches only XWayland's windows).
pub(crate) fn wayland_session() -> bool {
    let set = |name: &str| std::env::var_os(name).is_some_and(|value| !value.is_empty());
    set("WAYLAND_DISPLAY")
        && (std::env::var("XDG_SESSION_TYPE").is_ok_and(|kind| kind == "wayland")
            || !set("DISPLAY"))
}

pub(crate) struct PortalDuplicator {
    // Kept for the stream's life: the portal ends the cast when its session closes.
    _proxy: Screencast,
    _session: Session<Screencast>,
    stream: Stream,
    info: OutputInfo,
    /// The frame handed out last (B, G, R, X rows of `stride` bytes) and its number.
    pixels: Vec<u8>,
    stride: usize,
    seen: u64,
}

// The stream's raw parts are PipeWire's, used under its lock.
unsafe impl Send for PortalDuplicator {}

impl PortalDuplicator {
    /// A ScreenCast session of one monitor (the one the person, or the portal's settings, choose).
    pub(crate) fn new() -> Result<Self> {
        let portal_error =
            |error: ashpd::Error| Error::new(format!("the ScreenCast portal: {error}"));
        let (proxy, session, fd, node, size, position) = async_io::block_on(async {
            let proxy = Screencast::new().await?;
            let session = proxy.create_session(Default::default()).await?;
            proxy
                .select_sources(
                    &session,
                    SelectSourcesOptions::default()
                        .set_cursor_mode(CursorMode::Embedded)
                        .set_sources(enumflags2::BitFlags::from(SourceType::Monitor))
                        .set_multiple(false)
                        .set_persist_mode(PersistMode::DoNot),
                )
                .await?;
            let response = proxy
                .start(&session, None, Default::default())
                .await?
                .response()?;
            let stream = response
                .streams()
                .first()
                .cloned()
                .ok_or(ashpd::Error::NoResponse)?;
            let fd = proxy
                .open_pipe_wire_remote(&session, Default::default())
                .await?;
            Ok((
                proxy,
                session,
                fd,
                stream.pipe_wire_node_id(),
                stream.size(),
                stream.position(),
            ))
        })
        .map_err(portal_error)?;
        let size = size.map_or((1920, 1080), |(w, h)| (w.max(1) as u32, h.max(1) as u32));
        let stream = Stream::open(fd, node, size).map_err(Error::new)?;
        let mut duplicator = PortalDuplicator {
            _proxy: proxy,
            _session: session,
            stream,
            info: OutputInfo {
                adapter: "Wayland".into(),
                output: format!("portal stream {node}"),
                width: size.0,
                height: size.1,
                refresh_hz: 60.0,
                rotation: 0,
                method: "ScreenCast portal + PipeWire",
                left: position.map_or(0, |(x, _)| x),
                top: position.map_or(0, |(_, y)| y),
            },
            pixels: Vec::new(),
            stride: 0,
            seen: 0,
        };
        // The first picture says the size PipeWire really sends.
        let first = duplicator
            .stream
            .next(0, Duration::from_secs(5), |frame| {
                (frame.width, frame.height)
            })
            .map_err(Error::new)?
            .ok_or_else(|| Error::new("the ScreenCast portal sent no picture within 5 s".into()))?;
        duplicator.info.width = first.0;
        duplicator.info.height = first.1;
        Ok(duplicator)
    }

    pub(crate) fn info(&self) -> &OutputInfo {
        &self.info
    }

    /// The next frame: the newest since the last one, at once if one came, else after waiting up to `timeout_ms`.
    pub(crate) fn acquire(&mut self, timeout_ms: u32) -> Result<Option<Frame>> {
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
