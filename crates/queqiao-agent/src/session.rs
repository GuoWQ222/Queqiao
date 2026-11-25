//! The agent's session with one viewer, on every system (docs/LINUX.md step 3): the screen captured, encoded and
//! sent, the viewer's input injected, the clipboard, files, sound, command lines and the viewer's commands. What
//! differs per system comes through `crate::platform` (Windows: win.rs; Linux: linux.rs) and the platform layers
//! of the crates; moved from win.rs.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use queqiao_audio::{Capture, TEST_TONE_HZ, Tone};
use queqiao_capture::{
    BgraImage, Context, Device, Duplicator, OutputInfo, needs_restart, qpc_frequency, qpc_now,
};
use queqiao_codec::{EncodeFormat, ScreenEncoder};
use queqiao_core::protocol::MAX_CLIPBOARD_TEXT;
use queqiao_core::protocol::{Command, InputEvent, Message};
use queqiao_display::PresenceEvent;
use queqiao_files::{ImageReceiver, ImageSender, image_fingerprint, test_picture};
use queqiao_files::{Outcome, Transfers, downloads_folder};
use queqiao_input::{
    ClipContent, Clipboard, Injector, ScreenRect, cursor_position, key_is_down, set_cursor_position,
};
use queqiao_local::FromHelper;
use queqiao_terminal::{TerminalMessage, Terminals};
use queqiao_transport::{Addresses, Peer, PeerEvent, Role, START_BITRATE, Signaling, Stream};

use crate::options::Options;
use crate::platform::{
    Latest, PRIVACY_TEST_COLOR, describe, keep_frame, privacy_check, privacy_test_area, upload,
};
use crate::service_link::{self, Links};
use crate::{lock_gate, power, ui};

/// The test key: F24 exists on no normal keyboard and no program reacts to it.
pub(crate) const TEST_SCANCODE: u16 = 0x76;

pub(crate) const VK_F24: u16 = 0x87;

/// Adaptive bitrate: the encoder never goes below this, changes at most this often, and runs at this share of
/// the estimated bandwidth (the rest is headroom for retransmissions and the pacer).
pub(crate) const MIN_BITRATE: u32 = 500_000;

pub(crate) const BITRATE_CHANGE_GAP: Duration = Duration::from_millis(250);

pub(crate) const BANDWIDTH_SHARE: f64 = 0.85;

/// The encoder bitrate for a bandwidth estimate.
pub(crate) fn adapted_bitrate(estimate: u64, max: u32) -> u32 {
    ((estimate as f64 * BANDWIDTH_SHARE) as u64)
        .clamp(u64::from(MIN_BITRATE), u64::from(max.max(MIN_BITRATE))) as u32
}

/// A session that is not connected after this long is given up.
pub(crate) const CONNECT_LIMIT: Duration = Duration::from_secs(30);

#[derive(Default)]
pub(crate) struct Stats {
    pub(crate) captured: u32,
    pub(crate) sent: u32,
    pub(crate) keyframes: u32,
    pub(crate) bytes: usize,
    pub(crate) keyframe_requests: u32,
    pub(crate) inputs: u32,
    pub(crate) inject_failures: u32,
    pub(crate) last_inject_error: String,
    pub(crate) moves_checked: u32,
    pub(crate) moves_ok: u32,
    /// Game mouse: relative moves checked, and those that moved the pointer the right way.
    pub(crate) deltas_checked: u32,
    pub(crate) deltas_ok: u32,
    pub(crate) keys_checked: u32,
    pub(crate) keys_ok: u32,
    /// Each test key check: down or up, how long the state took (or that it never came).
    pub(crate) key_notes: Vec<String>,
    pub(crate) agent_ms: Vec<f64>,
}

/// Whether this PC (its primary screen's GPU) has an AV1 encoder: asked once per process.
pub(crate) static AV1_ENCODER: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

/// Asks for [`AV1_ENCODER`] in the background (a D3D11 device of its own, Windows' AV1 encoder opened and closed).
pub(crate) fn start_av1_probe() {
    if AV1_ENCODER.get().is_some() {
        return;
    }
    let _ = std::thread::Builder::new()
        .name("queqiao-av1-probe".into())
        .spawn(|| {
            AV1_ENCODER.get_or_init(|| match queqiao_capture::screen_device(None) {
                Ok(screen) => queqiao_codec::can_encode(
                    queqiao_core::protocol::VideoCodec::Av1,
                    &screen.device,
                ),
                Err(_) => false,
            });
        });
}

/// How long one pass of the session loop spent in each stage, and its slowest peer event: a pass over a limit is
/// named in the report (a stall of the loop holds input, pings and the picture alike).
pub(crate) struct Stages {
    pub(crate) started: Instant,
    pub(crate) last: Instant,
    pub(crate) slowest: (&'static str, Duration),
    pub(crate) event: (&'static str, Duration),
    pub(crate) current_event: &'static str,
    pub(crate) current_kind: (Option<u8>, Option<u8>),
    pub(crate) event_kind: (Option<u8>, Option<u8>),
}

impl Stages {
    fn new() -> Stages {
        let now = Instant::now();
        Stages {
            started: now,
            last: now,
            slowest: ("", Duration::ZERO),
            event: ("", Duration::ZERO),
            current_event: "",
            current_kind: (None, None),
            event_kind: (None, None),
        }
    }

    fn mark(&mut self, stage: &'static str) {
        let now = Instant::now();
        if now - self.last > self.slowest.1 {
            self.slowest = (stage, now - self.last);
        }
        self.last = now;
    }

    /// The event being handled: its kind, and for a message the protocol's first two bytes (the message kind; a
    /// command's own kind).
    fn event(&mut self, name: &'static str, kind: (Option<u8>, Option<u8>)) {
        self.current_event = name;
        self.current_kind = kind;
    }

    fn event_done(&mut self, started: Instant) {
        let took = started.elapsed();
        if took > self.event.1 {
            self.event = (self.current_event, took);
            self.event_kind = self.current_kind;
        }
    }

    /// "the session loop stopped 5503 ms, 5490 ms of it in the viewer's messages (slowest: data, 5488 ms)".
    fn slow(&self, limit: Duration) -> Option<String> {
        let total = self.started.elapsed();
        (total > limit).then(|| {
            format!(
                "the session loop stopped {} ms, {} ms of it in {}{}",
                total.as_millis(),
                self.slowest.1.as_millis(),
                self.slowest.0,
                if self.event.1 > Duration::from_millis(50) {
                    let kind = match self.event_kind {
                        (Some(first), Some(second)) => format!(" of kind {first}/{second}"),
                        (Some(first), None) => format!(" of kind {first}"),
                        _ => String::new(),
                    };
                    format!(
                        " (slowest event: {}{kind}, {} ms)",
                        self.event.0,
                        self.event.1.as_millis()
                    )
                } else {
                    String::new()
                }
            )
        })
    }
}

/// A peer event's kind, for the report.
pub(crate) fn peer_event_name(event: &PeerEvent) -> &'static str {
    match event {
        PeerEvent::Connected => "connected",
        PeerEvent::ChannelOpen => "channel open",
        PeerEvent::Video { .. } => "video",
        PeerEvent::Data(_) => "a message",
        PeerEvent::StreamOpen(_) => "stream open",
        PeerEvent::StreamData(..) => "stream data",
        PeerEvent::KeyframeRequested => "keyframe request",
        PeerEvent::Path(_) => "path",
        PeerEvent::Note(_) => "note",
        PeerEvent::Bitrate(_) => "bitrate",
        PeerEvent::Audio { .. } => "audio",
        PeerEvent::Closed(_) => "closed",
    }
}

/// The codec "Automatic" picks: AV1 when this PC encodes it on its GPU and the viewer decodes it on its GPU (as
/// fast as H.264 and fewer bits for the same picture: measured 2026-11-22, 13.8 ms median screen change -> shown
/// against 14.9 ms), otherwise H.264.
pub(crate) fn automatic_codec(
    viewer_av1: bool,
    agent_av1: bool,
) -> queqiao_core::protocol::VideoCodec {
    if viewer_av1 && agent_av1 {
        queqiao_core::protocol::VideoCodec::Av1
    } else {
        queqiao_core::protocol::VideoCodec::H264
    }
}

/// The H.265 to send for the viewer's wishes, without what it cannot decode (`can_decode`: `protocol::hevc` bits).
pub(crate) fn hevc_format(can_decode: u8, chroma444: bool, ten_bit: bool) -> EncodeFormat {
    use queqiao_core::protocol::hevc;
    let (chroma444, ten_bit) = if can_decode & hevc::bit(chroma444, ten_bit) != 0 {
        (chroma444, ten_bit)
    } else if chroma444 && can_decode & hevc::CHROMA444 != 0 {
        (true, false)
    } else if ten_bit && can_decode & hevc::MAIN10 != 0 {
        (false, true)
    } else {
        (false, false)
    };
    EncodeFormat {
        codec: queqiao_codec::Codec::H265,
        chroma444,
        ten_bit,
    }
}

/// What to try for `format`, best first: as it is; 4:4:4 at 8 bits (Linux's NVENC path makes no 10-bit; 4:4:4 before
/// 10 bits, as `hevc_format` prefers for a screen's text); 4:2:0 (AMD's encoders make no 4:4:4); then 8 bits. Only
/// what the viewer decodes (`can_decode`: `protocol::hevc` bits).
pub(crate) fn fallbacks(
    codec: queqiao_core::protocol::VideoCodec,
    format: EncodeFormat,
    can_decode: u8,
) -> Vec<EncodeFormat> {
    if codec != queqiao_core::protocol::VideoCodec::H265 {
        return vec![format];
    }
    let mut list = vec![format];
    for next in [
        EncodeFormat {
            ten_bit: false,
            ..format
        },
        EncodeFormat {
            chroma444: false,
            ..format
        },
        EncodeFormat {
            chroma444: false,
            ten_bit: false,
            ..format
        },
    ] {
        let decodable =
            can_decode & queqiao_core::protocol::hevc::bit(next.chroma444, next.ten_bit) != 0;
        if decodable && !list.contains(&next) {
            list.push(next);
        }
    }
    list
}

/// "H.264", "AV1", or for H.265 its kind ("H.265 4:4:4 10-bit").
pub(crate) fn picture_name(
    codec: queqiao_core::protocol::VideoCodec,
    format: EncodeFormat,
) -> String {
    match codec {
        queqiao_core::protocol::VideoCodec::H265 => format.name(),
        _ => codec.name().to_string(),
    }
}

/// The largest pointer shape sent each way (a 128 x 128 colour shape is 64 KB: one data channel message).
pub(crate) const POINTER_LIMIT: u32 = 128;

/// Windows' pointer shape in the protocol's form (rows tightly packed); None if too large or of an unknown kind.
pub(crate) fn pointer_shape(
    raw: &queqiao_capture::PointerShapeRaw,
) -> Option<queqiao_core::protocol::PointerShape> {
    use queqiao_core::protocol::{PointerKind, PointerShape};
    let (kind, height, row) = match raw.kind {
        1 => (
            PointerKind::Monochrome,
            raw.height / 2,
            raw.width.div_ceil(8),
        ),
        2 => (PointerKind::Color, raw.height, raw.width * 4),
        4 => (PointerKind::MaskedColor, raw.height, raw.width * 4),
        _ => return None,
    };
    if raw.width == 0
        || height == 0
        || raw.width > POINTER_LIMIT
        || height > POINTER_LIMIT
        || raw.pitch < row
    {
        return None;
    }
    let rows = if kind == PointerKind::Monochrome {
        height * 2
    } else {
        height
    };
    let mut data = Vec::with_capacity((row * rows) as usize);
    for y in 0..rows as usize {
        let start = y * raw.pitch as usize;
        data.extend_from_slice(raw.data.get(start..start + row as usize)?);
    }
    let shape = PointerShape {
        kind,
        width: raw.width as u16,
        height: height as u16,
        hot_x: raw.hot_x.clamp(0, raw.width as i32 - 1) as u16,
        hot_y: raw.hot_y.clamp(0, height as i32 - 1) as u16,
        data,
    };
    shape.is_valid().then_some(shape)
}

/// The local keyboard and mouse back (the privacy screen went, or Windows' own screen came up).
pub(crate) fn end_local_block(
    block: &mut Option<queqiao_input::LocalInputBlock>,
    lines: &mut Vec<String>,
) {
    if let Some(block) = block.take() {
        lines.push(format!(
            "privacy: the local keyboard and mouse work again ({} local events were held back)",
            block.swallowed()
        ));
    }
}

/// Starts capturing this PC's sound for the session (None, with a report line, when it cannot).
pub(crate) fn start_sound(
    peer: &Peer,
    options: &Options,
    lines: &mut Vec<String>,
) -> Option<Capture> {
    if options.logon {
        lines.push("sound: none at the sign-in screen".into());
        return None;
    }
    match peer.audio_sender() {
        Ok(sender) => Some(Capture::start(
            options.sound_device.clone(),
            Box::new(move |packet, time| sender.send(packet, time)),
        )),
        Err(error) => {
            lines.push(format!("sound: not sent ({error})"));
            None
        }
    }
}

pub(crate) fn sound_line(capture: &Capture) -> String {
    let stats = capture.stats();
    format!(
        "sound: from {}: {} packets ({:.0} kB) sent, {} silent frames left out",
        if stats.device.is_empty() {
            "no device"
        } else {
            &stats.device
        },
        stats.packets,
        stats.bytes as f64 / 1e3,
        stats.silent
    )
}

/// Carries out a command from the viewer; returns what to tell the person controlling.
pub(crate) fn run_command(command: Command, dry_run: bool) -> String {
    match command {
        // SendSAS works only for a service running as SYSTEM (M5).
        Command::CtrlAltDel => {
            "Ctrl+Alt+Del needs the Queqiao service on that PC, which is not running".into()
        }
        Command::Lock if dry_run => "the PC would be locked now (test run)".into(),
        Command::Lock => match power::lock() {
            Ok(()) => "the PC is locked".into(),
            Err(error) => format!("locking failed: {error}"),
        },
        Command::Restart if dry_run => format!(
            "the PC would restart in {} s (test run)",
            power::RESTART_DELAY
        ),
        Command::Restart => match power::restart() {
            Ok(()) => format!("the PC restarts in {} s", power::RESTART_DELAY),
            Err(error) => format!("restarting failed: {error}"),
        },
        // Handled by the session (they change its state).
        Command::SoundOn
        | Command::SoundOff
        | Command::PrivacyOn
        | Command::PrivacyOff
        | Command::MuteSpeakers(_)
        | Command::Resolution(_)
        | Command::Screen(_)
        | Command::MaxBitrate(_)
        | Command::RestartQueqiao
        | Command::Microphone(_)
        | Command::VideoCodec { .. }
        | Command::Picture { .. } => "changed".into(),
    }
}

/// Everything that belongs to the screen being shown.
pub(crate) struct OpenedScreen {
    pub(crate) duplicator: Duplicator,
    pub(crate) info: OutputInfo,
    pub(crate) device: Device,
    pub(crate) context: Context,
    pub(crate) latest: Latest,
    pub(crate) screen: (u32, u32),
    pub(crate) encoder: ScreenEncoder,
    pub(crate) injector: Injector,
}

/// Opens screen `index`: capture (on the GPU that drives it), a first picture, an encoder for its size (within
/// `limit`) and input mapped to it.
pub(crate) fn open_screen(
    index: usize,
    fps: u32,
    fixed_fps: bool,
    limit: Option<(u32, u32)>,
    codec: queqiao_core::protocol::VideoCodec,
) -> Result<OpenedScreen, String> {
    let duplicator = Duplicator::new(index).map_err(describe)?;
    let info = duplicator.info().clone();
    let (device, context) = (duplicator.device().clone(), duplicator.context().clone());
    let snapshot = duplicator.snapshot_image().map_err(describe)?;
    let latest = upload(&device, &snapshot)?;
    let screen = (snapshot.width, snapshot.height);
    let encoder =
        ScreenEncoder::with_codec(&device, &context, screen, fps, fixed_fps, limit, codec)
            .map_err(|e| format!("encoder: {}", describe(e)))?;
    let injector = Injector::new(ScreenRect {
        left: info.left,
        top: info.top,
        width: info.width,
        height: info.height,
    });
    Ok(OpenedScreen {
        duplicator,
        info,
        device,
        context,
        latest,
        screen,
        encoder,
        injector,
    })
}

/// What a session may do: the person's settings, and less for someone helping from another account.
pub(crate) struct Permissions {
    pub(crate) clipboard: bool,
    pub(crate) files: bool,
    pub(crate) terminal: bool,
    /// Another account's device, joined by a help code: no privacy screen, no restart.
    pub(crate) helper: bool,
    /// The viewer rebuilds lost video packets from parity (its hello said "fec"): it is sent parity.
    pub(crate) fec: bool,
}

/// Files copied to the clipboard go over at once up to this many bytes in all.
pub(crate) const CLIPBOARD_FILES_LIMIT: u64 = 512 << 20;

/// The size of files and folders (folders counted with everything in them, links not followed).
pub(crate) fn clipboard_size(paths: &[PathBuf]) -> u64 {
    let mut total = 0u64;
    let mut pending: Vec<PathBuf> = paths.to_vec();
    while let Some(path) = pending.pop() {
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&path) {
                pending.extend(entries.flatten().map(|entry| entry.path()));
            }
        } else if meta.is_file() {
            total = total.saturating_add(meta.len());
        }
        if total > CLIPBOARD_FILES_LIMIT {
            break;
        }
    }
    total
}

/// One report line per finished or failed file; finished ones go into the list of transfers (main window).
/// `at`: how long the session has been on (the report says when each file was done).
pub(crate) fn report_files(
    outcomes: Vec<Outcome>,
    peer: &str,
    lines: &mut Vec<String>,
    at: Duration,
) {
    let at = at.as_secs_f64();
    for outcome in outcomes {
        queqiao_files::history::record(&outcome, peer);
        lines.push(match outcome {
            Outcome::Sent { name, size } => {
                format!("file sent: {name} ({size} bytes, checked by the viewer) after {at:.1} s")
            }
            Outcome::Received { path, size, .. } => {
                format!(
                    "file received: {} ({size} bytes, SHA-256 checked) after {at:.1} s",
                    path.display()
                )
            }
            Outcome::Failed { name, reason } => format!("file failed: {name}: {reason}"),
        });
    }
}

/// How a session went.
/// The start of the clipboard text the tests send (each side checks it got the other's).
pub(crate) const CLIPBOARD_TEST_TEXT: &str = "Queqiao clipboard test";

/// Clipboard texts in a session.
#[derive(Default)]
pub(crate) struct ClipboardStats {
    pub(crate) received: u32,
    pub(crate) sent: u32,
    pub(crate) test_received: bool,
    pub(crate) test_sent: bool,
    /// The last text from the other side (not sent back, not set again).
    pub(crate) last_in: Option<String>,
}

impl ClipboardStats {
    fn received(&mut self, text: &str) {
        self.received += 1;
        self.test_received |= text.starts_with(CLIPBOARD_TEST_TEXT);
    }

    fn line(&self) -> String {
        format!(
            "clipboard: {} received, {} sent; test text from the other side received: {}",
            self.received,
            self.sent,
            if self.test_received { "yes" } else { "no" }
        )
    }
}

pub(crate) struct SessionOutcome {
    pub(crate) passed: bool,
    /// How it ended, for the session history (Windows' account mode keeps one).
    #[cfg_attr(not(windows), allow(dead_code))]
    pub(crate) ended: String,
}

/// Takes the banner down however the session ends.
pub(crate) struct BannerGuard<'a>(&'a ui::Ui);

impl Drop for BannerGuard<'_> {
    fn drop(&mut self) {
        self.0.session_ended();
    }
}

/// One session with one viewer: capture, encode and send the screen, inject the viewer's input. The banner
/// shows it on this PC the whole time; tray and banner actions that are not about the session are put in
/// `deferred`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn session(
    options: &Options,
    signal: &mut dyn Signaling,
    viewer: &str,
    viewer_id: &str,
    end: Option<Instant>,
    ui: &ui::Ui,
    permissions: Permissions,
    links: &mut Links,
    deferred: &mut Vec<PresenceEvent>,
    lines: &mut Vec<String>,
) -> Result<SessionOutcome, String> {
    let _ = viewer_id;
    let Permissions {
        clipboard: share_clipboard,
        files: receive_files,
        terminal: allow_terminal,
        helper,
        fec,
    } = permissions;
    // Told once per secure desktop that someone helping cannot operate it.
    let mut secure_refused_told = false;
    ui.session_started(viewer)
        .map_err(|e| format!("the banner could not be shown, so the session is refused: {e}"))?;
    let _banner = BannerGuard(ui);
    let frequency = qpc_frequency() as f64;
    let ms = |ticks: i64| ticks as f64 * 1000.0 / frequency;

    // This PC's own capture; none while another desktop takes input (the sign-in, lock, Ctrl+Alt+Del or UAC
    // screen), whose pictures come from the service's desktop helper.
    let (mut secure_switches, mut secure_frames, mut secure_inputs) = (0u32, 0u32, 0u32);
    if let Some(link) = links.desktop.as_mut() {
        for name in link.poll_desktop() {
            lines.push(format!("input desktop: {name}"));
        }
        link.session(true);
        link.banner(Some(viewer));
    }
    let (mut duplicator, mut info, mut device, mut context, snapshot) = match Duplicator::primary()
    {
        Ok(own) => {
            let snapshot = own.snapshot_image().map_err(describe)?;
            let (info, device, context) = (
                own.info().clone(),
                own.device().clone(),
                own.context().clone(),
            );
            (Some(own), info, device, context, snapshot)
        }
        Err(error) => {
            let Some(link) = links.desktop.as_mut() else {
                return Err(describe(error));
            };
            // No screens to see from here (the agent at the sign-in screen runs in session 0): a device on the first
            // GPU, and the screen's place as the helper says.
            let opened = queqiao_capture::screen_device(None)
                .or_else(|_| queqiao_capture::first_device())
                .map_err(describe)?;
            link.capture(opened.index);
            let snapshot = first_helper_picture(link)?;
            let mut info = opened.info;
            match link.screen {
                Some(screen) => {
                    (info.left, info.top) = (screen.left, screen.top);
                    (info.width, info.height) = (screen.width, screen.height);
                }
                None => (info.width, info.height) = (snapshot.width, snapshot.height),
            }
            secure_switches += 1;
            lines.push(format!(
                "this PC's own desktop does not take input now ({}): pictures and input through the desktop helper",
                describe(error)
            ));
            (None, info, opened.device, opened.context, snapshot)
        }
    };
    let mut latest = upload(&device, &snapshot)?;
    let fps = options
        .fps
        .unwrap_or((info.refresh_hz.round() as u32).clamp(1, 60));
    let mut screen = (snapshot.width, snapshot.height);
    let mut encoder = ScreenEncoder::new(&device, &context, screen, fps, options.fps.is_some())
        .map_err(|e| format!("encoder: {}", describe(e)))?;
    let config = encoder.config().clone();
    lines.push(format!(
        "screen {}x{} at {:.0} Hz on {}",
        info.width, info.height, info.refresh_hz, info.adapter
    ));
    // Wayland: which portal session (and whether the person's earlier answer was kept).
    if info.adapter == "Wayland" {
        lines.push(format!("captured through the {}", info.output));
    }
    // Discrete before integrated: the screen's GPU captures, and encodes too (moving each picture to another GPU is
    // not built), so a screen on the integrated GPU is said.
    if let Some(first) = queqiao_capture::preferred_gpu_besides(&info.adapter) {
        lines.push(format!(
            "the screen is on {}; {first} comes first (discrete before integrated), but encoding stays on the screen's GPU",
            info.adapter
        ));
    }
    lines.push(format!(
        "encoder: {} ({}) at {}x{}, {} fps, {:.1} Mbit/s; convert: {}",
        encoder.encoder().name(),
        if encoder.encoder().is_hardware() {
            "hardware"
        } else {
            "software"
        },
        config.width,
        config.height,
        config.fps,
        f64::from(config.bitrate) / 1e6,
        encoder.converter_description()
    ));
    let mut injector = Injector::new(ScreenRect {
        left: info.left,
        top: info.top,
        width: info.width,
        height: info.height,
    });
    #[cfg(target_os = "linux")]
    lines.push(format!("input through {}", injector.describe()));
    // The screen shown (counted as queqiao_capture::screens does) and the picture size the viewer chose.
    let mut screen_index = queqiao_capture::screens()
        .ok()
        .and_then(|list| list.iter().position(|s| s.primary))
        .unwrap_or(0);
    let mut resolution_limit: Option<(u32, u32)> = None;

    lines.push(format!("viewer: {viewer}"));
    let started = Instant::now();
    // Someone helping from another account learns only the address the server connection uses.
    let addresses = if helper {
        Addresses::RouteOnly
    } else {
        Addresses::All
    };
    let peer = Peer::start_with(Role::Agent, signal, addresses).map_err(|e| e.to_string())?;
    lines.extend(peer.candidates().iter().cloned());
    // Parity for the video (as UU Remote): lost packets are rebuilt at once instead of after a round trip.
    peer.send_fec(fec);
    lines.push(if fec {
        "video parity (FEC): on, the viewer rebuilds lost packets from it".to_string()
    } else {
        "video parity (FEC): off, the viewer does not use it".to_string()
    });
    // This PC's sound, captured and sent from its own thread (the viewer can turn it off).
    let mut sound = start_sound(&peer, options, lines);
    let _tone = if options.sound_test {
        Tone::start(options.sound_device.clone(), TEST_TONE_HZ)
            .map_err(|error| lines.push(format!("sound test: no tone ({error})")))
            .ok()
    } else {
        None
    };
    let mut sound_report = Vec::new();
    let mut privacy_check_due = false;
    // The viewer's microphone: played into a virtual microphone while it comes; the test listens to that.
    let mut microphone: Option<(queqiao_audio::Player, queqiao_audio::VirtualMicrophone)> = None;
    let mut microphone_packets = 0u64;
    type ToneCheck = std::thread::JoinHandle<Result<(u32, u32), String>>;
    let mut microphone_check: Option<ToneCheck> = None;
    // Whether this PC has an AV1 encoder: asked in the background from now, ready before the viewer says which codec
    // it wants (asked then, in this loop, it held input and pings for about 450 ms at each session's start).
    start_av1_probe();
    // That PC's pointer shape as last sent, and how many were sent or left out.
    let mut last_pointer: Option<queqiao_core::protocol::PointerShape> = None;
    let (mut pointer_shapes, mut pointer_skipped) = (0u32, 0u32);
    // The local keyboard and mouse off while the privacy screen is up.
    let mut local_block: Option<queqiao_input::LocalInputBlock> = None;
    // Muted speakers: whether they were muted before, to put back when the session ends.
    let mut speakers_before: Option<bool> = None;
    // Adaptive bitrate: start at the transport's starting estimate, then follow the estimate (85 % of it),
    // never above the size-based bitrate the encoder was opened with.
    // The viewer's picture settings change the highest bitrate (quality) and the frame interval (frame rate).
    let mut size_bitrate = config.bitrate;
    // The quality the viewer chose (a share of the size-based bitrate), kept when the size changes.
    let mut quality_factor = 1.0f64;
    // A bitrate ceiling the viewer typed in (bit/s), instead of the preset's share of the size-based one.
    let mut custom_bitrate: Option<u32> = None;
    let mut max_bitrate = size_bitrate;
    let mut last_estimate = START_BITRATE;
    peer.set_bitrate_range(u64::from(MIN_BITRATE), u64::from(max_bitrate));
    let start_bitrate = adapted_bitrate(START_BITRATE, max_bitrate);
    if start_bitrate != encoder.bitrate()
        && let Err(error) = encoder.set_bitrate(start_bitrate)
    {
        lines.push(format!(
            "the encoder keeps {:.1} Mbit/s: {}",
            f64::from(encoder.bitrate()) / 1e6,
            describe(error)
        ));
    }
    let mut bitrate_changes = 0u32;
    let mut course: Vec<String> = Vec::new();
    let mut course_at = (Instant::now(), 0usize, 0u32, 0u32);
    // The report: every frame's size and each keyframe's (kB).
    let mut frame_sizes: Vec<f64> = Vec::new();
    let mut keyframe_sizes: Vec<String> = Vec::new();
    let mut lowest_bitrate = encoder.bitrate();
    let mut last_bitrate_change: Option<Instant> = None;

    let pointer_start = cursor_position();
    let mut interval = Duration::from_secs_f64(1.0 / f64::from(config.fps));
    let screen_fps = (info.refresh_hz.round() as u32).clamp(1, 240);
    let mut stats = Stats::default();
    let mut connected_after: Option<Duration> = None;
    // The GDI snapshot is the first picture to send; its screen time is unknown (0).
    let mut waiting: Option<(i64, i64)> = Some((0, qpc_now()));
    let mut force_keyframe = false;
    let mut last_encode: Option<Instant> = None;
    let mut closed = None;

    // The clipboard both ways, while the person at this PC allows it.
    let clipboard = if share_clipboard {
        Clipboard::start()
            .map_err(|error| lines.push(format!("clipboard: not shared ({error})")))
            .ok()
    } else {
        lines.push("clipboard: not shared (turned off on this PC)".into());
        None
    };
    let mut clipboard_stats = ClipboardStats::default();
    let mut channel_open_at: Option<Instant> = None;
    // Files both ways: from the viewer into Downloads\Queqiao (while the person at this PC allows it).
    let mut transfers = Transfers::new(downloads_folder(), receive_files);
    // Clipboard images both ways, on their own stream.
    let (mut images, mut image_in) = (ImageSender::default(), ImageReceiver::default());
    let (mut images_received, mut images_sent, mut image_test_received) = (0u32, 0u32, false);
    // The image last received: not put on the clipboard again, nor sent back (as for text).
    let mut last_image_in: Option<u64> = None;
    // Files from the other side's clipboard copies, by copy; they go on this clipboard as they arrive.
    let mut clip_files: std::collections::HashMap<u32, Vec<PathBuf>> =
        std::collections::HashMap::new();
    let mut clip_files_set = 0u32;
    // Remote command lines (while the person at this PC allows them).
    let mut terminals = Terminals::new(allow_terminal);
    for path in &options.send_files {
        let _ = transfers.send(path);
    }
    let mut end_pressed = false;
    let mut ended_here = false;
    let mut last_own_try = Instant::now();
    let mut size_noted = false;
    // A newly opened screen of this PC (after a desktop or display change) replaces the old one.
    macro_rules! adopt {
        ($opened:expr) => {{
            let opened = $opened;
            duplicator = Some(opened.duplicator);
            info = opened.info;
            (device, context) = (opened.device, opened.context);
            latest = opened.latest;
            screen = opened.screen;
            encoder = opened.encoder;
            injector = opened.injector;
            size_bitrate = encoder.config().bitrate;
            max_bitrate = custom_bitrate
                .unwrap_or(((f64::from(size_bitrate) * quality_factor) as u32).max(MIN_BITRATE));
            peer.set_bitrate_range(u64::from(MIN_BITRATE), u64::from(max_bitrate));
            let _ = encoder.set_bitrate(adapted_bitrate(last_estimate, max_bitrate));
            force_keyframe = true;
            waiting = Some((0, qpc_now()));
        }};
    }
    let mut trust_checked = Instant::now();
    // The lock screen (as UU): the picture and input wait for this PC's Windows password; at the sign-in screen (nobody
    // signed in) for the password of the Windows user who set it up.
    let mut gate = if options.logon {
        lock_gate::LockGate::signed_out()
    } else {
        lock_gate::LockGate::new(options.test_locked)
    };
    // Passes of the loop that took long: a note names the slowest stage (at most 8 notes).
    let mut slow_notes = 0u32;
    'session: loop {
        let mut watch = Stages::new();
        // The report: once a second for the first two minutes, the bandwidth estimate, the encoder's bitrate, what
        // was sent and how many frames (K: a keyframe among them), to see how the picture's bitrate followed.
        if course.len() < 120 && course_at.0.elapsed() >= Duration::from_secs(1) {
            let seconds = course_at.0.elapsed().as_secs_f64();
            course.push(format!(
                "{:.1}/{:.1}/{:.1}@{:.0}{}",
                last_estimate as f64 / 1e6,
                f64::from(encoder.bitrate()) / 1e6,
                (stats.bytes - course_at.1) as f64 * 8.0 / seconds / 1e6,
                f64::from(stats.sent - course_at.2) / seconds,
                if stats.keyframes > course_at.3 {
                    "K"
                } else {
                    ""
                }
            ));
            course_at = (Instant::now(), stats.bytes, stats.sent, stats.keyframes);
        }
        if gate.refresh() {
            let (reason, tries_left) = if helper {
                (queqiao_core::protocol::PasswordReason::OnlyTheOwner, 0)
            } else if gate.signed_out_mode() {
                (
                    queqiao_core::protocol::PasswordReason::SignedOut,
                    gate.tries_left(),
                )
            } else {
                (
                    queqiao_core::protocol::PasswordReason::Locked,
                    gate.tries_left(),
                )
            };
            peer.send_data(Message::PasswordNeeded { reason, tries_left }.encode());
            lines.push(format!(
                "lock: this PC is locked: its picture and input wait for its Windows password ({tries_left} tries)"
            ));
        }
        if end.is_some_and(|end| Instant::now() > end) {
            break;
        }
        // Once a second: a viewer taken out of the account, or this PC's login ended (the password was changed), ends
        // the session (as UU: a device taken out of the account is logged out).
        if trust_checked.elapsed() >= Duration::from_secs(1) {
            trust_checked = Instant::now();
            if !signal.peer_trusted() {
                closed = Some(
                    "the viewer's device is no longer on this account, or this PC's login ended"
                        .to_string(),
                );
                break 'session;
            }
        }
        if !end_pressed
            && options
                .end_after
                .is_some_and(|seconds| started.elapsed() >= Duration::from_secs(seconds))
        {
            ui.press_end();
            end_pressed = true;
        }
        while let Some(event) = ui.try_event() {
            match event {
                PresenceEvent::EndSession => {
                    closed = Some("ended by the person at this PC".to_string());
                    ended_here = true;
                    break 'session;
                }
                PresenceEvent::Quit => {
                    closed = Some("ended by the person at this PC (quit)".to_string());
                    ended_here = true;
                    deferred.push(PresenceEvent::Quit);
                    break 'session;
                }
                PresenceEvent::SettingsLocked => lines.push(
                    "a setting was chosen while someone of another account controls this PC: refused".into(),
                ),
                other => deferred.push(other),
            }
        }
        // End pressed on the helper's banner (on the sign-in, lock or UAC screen).
        if links
            .desktop
            .as_mut()
            .is_some_and(|link| std::mem::take(&mut link.end_pressed))
        {
            closed = Some("ended by the person at this PC".to_string());
            ended_here = true;
            break 'session;
        }
        // The helper could not show its banner on the screen taking input (sign-in, lock, UAC): nothing goes on unseen.
        if let Some(reason) = links
            .desktop
            .as_mut()
            .and_then(|link| link.no_banner.take())
        {
            closed = Some(format!(
                "the banner could not be shown on the screen in use ({reason}): ended"
            ));
            break 'session;
        }
        if connected_after.is_none() && started.elapsed() > CONNECT_LIMIT {
            closed = Some(format!(
                "could not connect within {} s",
                CONNECT_LIMIT.as_secs()
            ));
            break;
        }
        watch.mark("the start of the pass (course, gate, trust, UI)");
        while let Some(event) = peer.try_event() {
            let event_started = Instant::now();
            let event_name = peer_event_name(&event);
            let kind = match &event {
                PeerEvent::Data(bytes) => (bytes.first().copied(), bytes.get(1).copied()),
                _ => (None, None),
            };
            watch.event(event_name, kind);
            match event {
                PeerEvent::Connected => connected_after = Some(started.elapsed()),
                PeerEvent::KeyframeRequested => {
                    stats.keyframe_requests += 1;
                    force_keyframe = true;
                    waiting.get_or_insert((0, qpc_now()));
                }
                PeerEvent::Data(bytes) => match Message::decode(&bytes) {
                    Some(Message::Input(_)) if gate.closed() => {}
                    Some(Message::Input(event)) => {
                        stats.inputs += 1;
                        // On another desktop (sign-in, lock, Ctrl+Alt+Del, UAC) the desktop helper injects; never for
                        // someone helping from another account: the person at this PC answers those screens.
                        if duplicator.is_none()
                            && let Some(link) = links.desktop.as_mut()
                        {
                            if helper {
                                if !secure_refused_told {
                                    secure_refused_told = true;
                                    let notice = queqiao_core::text::tr("The person at that PC answers this screen (sign-in, UAC or Ctrl+Alt+Del) themselves: someone helping cannot operate it.").to_string();
                                    peer.send_data(Message::Notice(notice).encode());
                                    lines.push("helper: input on a secure desktop refused".into());
                                }
                                continue;
                            }
                            link.input(event, injector.screen());
                            secure_inputs += 1;
                            continue;
                        }
                        secure_refused_told = false;
                        let before = cursor_position();
                        match injector.inject(&event) {
                            Ok(())
                                if options.verify_input
                                    && matches!(event, InputEvent::MouseDelta { .. }) =>
                            {
                                verify_delta(&event, before, &mut stats)
                            }
                            Ok(()) if options.verify_input => verify(&injector, &event, &mut stats),
                            Ok(()) => {}
                            Err(error) => {
                                stats.inject_failures += 1;
                                stats.last_inject_error = error;
                            }
                        }
                    }
                    Some(Message::Ping { id, sent_us }) => {
                        peer.send_data(Message::Pong { id, sent_us }.encode());
                    }
                    Some(Message::Leave) => {
                        closed = Some("the viewer left".to_string());
                        break 'session;
                    }
                    // Only while asked for, and never from someone helping.
                    Some(Message::Password(password)) if !helper && gate.closed() => {
                        let right = gate.check(&password, options.test_password.as_deref());
                        let tries_left = gate.tries_left();
                        peer.send_data(
                            Message::PasswordResult {
                                ok: right,
                                tries_left,
                            }
                            .encode(),
                        );
                        if right {
                            lines.push(format!(
                                "lock: the viewer gave this PC's Windows password ({} frames sent before)",
                                stats.sent
                            ));
                            force_keyframe = true;
                            waiting.get_or_insert((0, qpc_now()));
                        } else {
                            lines.push(format!("lock: wrong password ({tries_left} tries left)"));
                            if tries_left == 0 {
                                closed = Some("too many wrong passwords for this PC".to_string());
                                break 'session;
                            }
                        }
                    }
                    Some(Message::Password(_)) => {}
                    Some(Message::Command(
                        Command::PrivacyOn
                        | Command::Restart
                        | Command::RestartQueqiao
                        | Command::CtrlAltDel,
                    )) if helper => {
                        let notice =
                            "someone helping from another account cannot do that on this PC"
                                .to_string();
                        lines.push(format!("command from the helper refused: {notice}"));
                        peer.send_data(Message::Notice(notice).encode());
                    }
                    Some(Message::Command(Command::PrivacyOn)) => {
                        let look = if options.privacy_test {
                            ui::PrivacyLook {
                                area: Some(privacy_test_area(info.left, info.top)),
                                color: PRIVACY_TEST_COLOR,
                            }
                        } else {
                            ui::PrivacyLook {
                                area: None,
                                color: 0x0010_1010,
                            }
                        };
                        let notice = match ui.set_privacy(Some(look)) {
                            Ok(banner_above) => {
                                privacy_check_due = options.privacy_test;
                                lines.push(format!(
                                    "privacy: the session banner is above the dark screen: {}",
                                    if banner_above { "yes" } else { "no" }
                                ));
                                // Nobody types or clicks unseen there meanwhile: its own keyboard and mouse are
                                // off (Ctrl+Alt+Del gives them back).
                                match queqiao_input::LocalInputBlock::start() {
                                    Ok(block) => {
                                        local_block = Some(block);
                                        lines.push(
                                            "privacy: the local keyboard and mouse are off".into(),
                                        );
                                        "that PC's screen is dark and its keyboard and mouse are off: only you see and use it".to_string()
                                    }
                                    Err(error) => {
                                        lines.push(format!("privacy: {error}"));
                                        "that PC's screen is dark: only you see it (its keyboard and mouse still work)".to_string()
                                    }
                                }
                            }
                            Err(error) => format!("that PC's screen could not be hidden: {error}"),
                        };
                        lines.push(format!("command from the viewer: PrivacyOn: {notice}"));
                        peer.send_data(Message::Notice(notice).encode());
                    }
                    Some(Message::Command(Command::PrivacyOff)) => {
                        let _ = ui.set_privacy(None);
                        end_local_block(&mut local_block, lines);
                        let notice = "that PC's screen is shown again".to_string();
                        lines.push(format!("command from the viewer: PrivacyOff: {notice}"));
                        peer.send_data(Message::Notice(notice).encode());
                    }
                    Some(Message::Command(Command::Picture { fps, quality })) => {
                        // At most what the screen shows; 0 asks for exactly that.
                        let frames = if fps == 0 {
                            screen_fps
                        } else {
                            u32::from(fps).min(screen_fps)
                        };
                        interval = Duration::from_secs_f64(1.0 / f64::from(frames));
                        quality_factor = quality.bitrate_factor();
                        max_bitrate = custom_bitrate.unwrap_or(
                            ((f64::from(size_bitrate) * quality_factor) as u32).max(MIN_BITRATE),
                        );
                        peer.set_bitrate_range(u64::from(MIN_BITRATE), u64::from(max_bitrate));
                        let target = adapted_bitrate(last_estimate, max_bitrate);
                        let changed = encoder.set_bitrate(target).is_ok();
                        let notice = format!(
                            "picture: up to {frames} fps, quality {quality:?}: up to {:.1} Mbit/s{}",
                            f64::from(max_bitrate) / 1e6,
                            if changed {
                                ""
                            } else {
                                " (the encoder keeps its bitrate)"
                            }
                        );
                        lines.push(format!("command from the viewer: {notice}"));
                        peer.send_data(Message::Notice(notice).encode());
                    }
                    Some(Message::Command(Command::MuteSpeakers(on))) => {
                        let device = options.sound_device.as_deref();
                        let notice = if on {
                            match queqiao_audio::set_mute(device, true) {
                                Ok(before) => {
                                    speakers_before.get_or_insert(before);
                                    "that PC's speakers are muted (you still hear its sound)"
                                        .to_string()
                                }
                                Err(error) => {
                                    format!("that PC's speakers could not be muted: {error}")
                                }
                            }
                        } else {
                            let before = speakers_before.take().unwrap_or(false);
                            let _ = queqiao_audio::set_mute(device, before);
                            "that PC's speakers play again".to_string()
                        };
                        lines.push(format!("command from the viewer: {notice}"));
                        peer.send_data(Message::Notice(notice).encode());
                    }
                    Some(Message::Command(Command::Resolution(short))) => {
                        // A new encoder for the new size (the screen is still captured whole and scaled).
                        let limit =
                            (short > 0).then(|| (u32::from(short) * 16 / 9, u32::from(short)));
                        resolution_limit = limit;
                        let notice = match ScreenEncoder::with_codec(
                            &device,
                            &context,
                            screen,
                            fps,
                            options.fps.is_some(),
                            limit,
                            encoder.codec(),
                        ) {
                            Ok(new) => {
                                encoder = new;
                                force_keyframe = true;
                                size_bitrate = encoder.config().bitrate;
                                max_bitrate = custom_bitrate.unwrap_or(
                                    ((f64::from(size_bitrate) * quality_factor) as u32)
                                        .max(MIN_BITRATE),
                                );
                                peer.set_bitrate_range(
                                    u64::from(MIN_BITRATE),
                                    u64::from(max_bitrate),
                                );
                                let _ = encoder
                                    .set_bitrate(adapted_bitrate(last_estimate, max_bitrate));
                                format!(
                                    "picture size {}x{}",
                                    encoder.config().width,
                                    encoder.config().height
                                )
                            }
                            Err(error) => format!("the picture size stays: {}", describe(error)),
                        };
                        lines.push(format!("command from the viewer: {notice}"));
                        peer.send_data(Message::Notice(notice).encode());
                    }
                    Some(Message::Command(Command::VideoCodec {
                        prefer,
                        can_decode_av1,
                        can_decode_hevc,
                        chroma444,
                        ten_bit,
                    })) => {
                        use queqiao_core::protocol::{CodecPreference, VideoCodec, hevc};
                        // Waits for the background probe if it is still at it.
                        let can_encode_av1 = *AV1_ENCODER
                            .get_or_init(|| queqiao_codec::can_encode(VideoCodec::Av1, &device));
                        let (wanted, format) = match prefer {
                            CodecPreference::Av1 if can_decode_av1 && can_encode_av1 => {
                                (VideoCodec::Av1, EncodeFormat::H264)
                            }
                            CodecPreference::H265 if can_decode_hevc & hevc::MAIN != 0 => (
                                VideoCodec::H265,
                                hevc_format(can_decode_hevc, chroma444, ten_bit),
                            ),
                            CodecPreference::Automatic => (
                                automatic_codec(can_decode_av1, can_encode_av1),
                                EncodeFormat::H264,
                            ),
                            _ => (VideoCodec::H264, EncodeFormat::H264),
                        };
                        let making = Instant::now();
                        let same = wanted == encoder.codec()
                            && (wanted != VideoCodec::H265 || format == encoder.format());
                        let mut notice = if same {
                            format!(
                                "picture: {} ({})",
                                picture_name(wanted, format),
                                encoder.encoder().name()
                            )
                        } else {
                            // H.265: as asked, then with less (4:2:0, 8 bits) when this PC's encoder cannot.
                            let mut made = None;
                            let mut refused = Vec::new();
                            for format in fallbacks(wanted, format, can_decode_hevc) {
                                match ScreenEncoder::with_format(
                                    &device,
                                    &context,
                                    screen,
                                    fps,
                                    options.fps.is_some(),
                                    resolution_limit,
                                    wanted,
                                    format,
                                ) {
                                    Ok(new) => {
                                        made = Some((new, format));
                                        break;
                                    }
                                    Err(error) => refused.push(format!(
                                        "{}: {}",
                                        picture_name(wanted, format),
                                        describe(error)
                                    )),
                                }
                            }
                            match made {
                                Some((new, format)) => {
                                    encoder = new;
                                    force_keyframe = true;
                                    size_bitrate = encoder.config().bitrate;
                                    max_bitrate = custom_bitrate.unwrap_or(
                                        ((f64::from(size_bitrate) * quality_factor) as u32)
                                            .max(MIN_BITRATE),
                                    );
                                    peer.set_bitrate_range(
                                        u64::from(MIN_BITRATE),
                                        u64::from(max_bitrate),
                                    );
                                    let _ = encoder
                                        .set_bitrate(adapted_bitrate(last_estimate, max_bitrate));
                                    lines.push(format!(
                                        "{} ms: the encoder for {} took {} ms to make",
                                        started.elapsed().as_millis(),
                                        picture_name(wanted, format),
                                        making.elapsed().as_millis()
                                    ));
                                    for line in &refused {
                                        lines.push(format!("not made: {line}"));
                                    }
                                    format!(
                                        "picture: {} ({})",
                                        picture_name(wanted, format),
                                        encoder.encoder().name()
                                    )
                                }
                                None => format!(
                                    "picture: {} did not start ({}); {} stays",
                                    picture_name(wanted, format),
                                    refused.join("; "),
                                    picture_name(encoder.codec(), encoder.format())
                                ),
                            }
                        };
                        if prefer == CodecPreference::Av1 && wanted != VideoCodec::Av1 {
                            notice.push_str(if can_encode_av1 {
                                " (AV1 asked for, but the viewer cannot decode it)"
                            } else {
                                " (AV1 asked for, but this PC has no AV1 encoder)"
                            });
                        }
                        if prefer == CodecPreference::H265 && wanted != VideoCodec::H265 {
                            notice.push_str(" (H.265 asked for, but the viewer cannot decode it)");
                        }
                        lines.push(format!("command from the viewer: {notice}"));
                        peer.send_data(Message::Notice(notice).encode());
                    }
                    Some(Message::Command(Command::Microphone(on))) if helper => {
                        let _ = on;
                        let notice =
                            "someone helping from another account cannot send a microphone to this PC"
                                .to_string();
                        lines.push(format!("command from the helper refused: {notice}"));
                        peer.send_data(Message::Notice(notice).encode());
                    }
                    Some(Message::Command(Command::Microphone(on))) => {
                        let notice = if !on {
                            microphone = None;
                            "the microphone from you stopped".to_string()
                        } else {
                            match queqiao_audio::virtual_microphone() {
                                Some(cable) => {
                                    let player = queqiao_audio::Player::start(Some(cable.speaker.clone()), false);
                                    // Tests: listen to the virtual microphone for the viewer's test tone.
                                    if options.mic_check && microphone_check.is_none() {
                                        let listen_to = cable.microphone.clone();
                                        microphone_check = Some(std::thread::spawn(move || {
                                            std::thread::sleep(Duration::from_millis(1500));
                                            queqiao_audio::listen_for_tone(&listen_to, Duration::from_secs(3))
                                        }));
                                    }
                                    let text = format!(
                                        "your microphone plays into \"{}\" on that PC; its programs hear it on the microphone \"{}\"",
                                        cable.speaker, cable.microphone
                                    );
                                    microphone = Some((player, cable));
                                    text
                                }
                                None => "that PC has no virtual microphone (VB-CABLE, for example, gives it one)".to_string(),
                            }
                        };
                        lines.push(format!("command from the viewer: microphone: {notice}"));
                        peer.send_data(Message::Notice(notice).encode());
                    }
                    Some(Message::Command(Command::Screen(_))) if duplicator.is_none() => {
                        let notice =
                            "the screen can be changed once this PC's own desktop takes input again"
                                .to_string();
                        lines.push(format!("command from the viewer: {notice}"));
                        peer.send_data(Message::Notice(notice).encode());
                    }
                    Some(Message::Command(Command::Screen(index))) => {
                        let fixed = options.fps.is_some();
                        // DXGI lets a process duplicate a screen once: the current duplication goes first. If the
                        // new screen cannot be opened, the one before is opened again (or the session ends).
                        drop(duplicator.take());
                        let (opened, shown) = match open_screen(
                            usize::from(index),
                            fps,
                            fixed,
                            resolution_limit,
                            encoder.codec(),
                        ) {
                            Ok(opened) => (opened, true),
                            Err(error) => {
                                lines.push(format!(
                                    "screen {} cannot be shown: {error}",
                                    usize::from(index) + 1
                                ));
                                let back = open_screen(
                                    screen_index,
                                    fps,
                                    fixed,
                                    resolution_limit,
                                    encoder.codec(),
                                )
                                .map_err(|again| {
                                    format!(
                                        "the screen before cannot be shown again either: {again}"
                                    )
                                })?;
                                (back, false)
                            }
                        };
                        duplicator = Some(opened.duplicator);
                        info = opened.info;
                        (device, context) = (opened.device, opened.context);
                        latest = opened.latest;
                        screen = opened.screen;
                        encoder = opened.encoder;
                        injector = opened.injector;
                        if shown {
                            screen_index = usize::from(index);
                        }
                        size_bitrate = encoder.config().bitrate;
                        max_bitrate = custom_bitrate.unwrap_or(
                            ((f64::from(size_bitrate) * quality_factor) as u32).max(MIN_BITRATE),
                        );
                        peer.set_bitrate_range(u64::from(MIN_BITRATE), u64::from(max_bitrate));
                        let _ = encoder.set_bitrate(adapted_bitrate(last_estimate, max_bitrate));
                        // The screen's first picture goes at once.
                        force_keyframe = true;
                        waiting = Some((0, qpc_now()));
                        let notice = if shown {
                            format!(
                                "screen {} ({}x{}), picture {}x{}",
                                usize::from(index) + 1,
                                info.width,
                                info.height,
                                encoder.config().width,
                                encoder.config().height
                            )
                        } else {
                            format!(
                                "screen {} cannot be shown; screen {} again",
                                usize::from(index) + 1,
                                screen_index + 1
                            )
                        };
                        lines.push(format!("command from the viewer: {notice}"));
                        peer.send_data(Message::Notice(notice).encode());
                    }
                    Some(Message::Command(Command::MaxBitrate(kbit))) => {
                        // 0.5 to 200 Mbit/s; 0 goes back to the preset.
                        custom_bitrate = (kbit > 0).then(|| kbit.clamp(500, 200_000) * 1000);
                        max_bitrate = custom_bitrate.unwrap_or(
                            ((f64::from(size_bitrate) * quality_factor) as u32).max(MIN_BITRATE),
                        );
                        peer.set_bitrate_range(u64::from(MIN_BITRATE), u64::from(max_bitrate));
                        let changed = encoder
                            .set_bitrate(adapted_bitrate(last_estimate, max_bitrate))
                            .is_ok();
                        let notice = format!(
                            "bitrate: up to {:.1} Mbit/s{}",
                            f64::from(max_bitrate) / 1e6,
                            if changed {
                                ""
                            } else {
                                " (the encoder keeps its bitrate)"
                            }
                        );
                        lines.push(format!("command from the viewer: {notice}"));
                        peer.send_data(Message::Notice(notice).encode());
                    }
                    Some(Message::Command(Command::SoundOff)) => {
                        if let Some(capture) = sound.take() {
                            sound_report.push(sound_line(&capture));
                        }
                        lines.push("the viewer turned this PC's sound off".into());
                    }
                    Some(Message::Command(Command::SoundOn)) => {
                        if sound.is_none() {
                            sound = start_sound(&peer, options, lines);
                        }
                        lines.push("the viewer turned this PC's sound on".into());
                    }
                    Some(Message::Command(Command::CtrlAltDel)) if links.service.is_some() => {
                        let notice = links
                            .service
                            .as_mut()
                            .map(|service| service.send_sas())
                            .unwrap_or_default();
                        lines.push(format!("command from the viewer: CtrlAltDel: {notice}"));
                        peer.send_data(Message::Notice(notice).encode());
                    }
                    Some(Message::Command(Command::RestartQueqiao)) => {
                        let notice = match &links.service {
                            Some(_) if options.dry_run_commands => {
                                "Queqiao would restart on that PC now (test run)"
                            }
                            Some(_) => "Queqiao restarts on that PC: connect again in a few seconds",
                            None => "restarting Queqiao needs the Queqiao service on that PC, which is not running",
                        }
                        .to_string();
                        lines.push(format!("command from the viewer: RestartQueqiao: {notice}"));
                        peer.send_data(Message::Notice(notice).encode());
                        if !options.dry_run_commands
                            && let Some(service) = links.service.as_mut()
                        {
                            // The notice first; then the service ends this agent.
                            std::thread::sleep(Duration::from_millis(300));
                            if let Err(error) = service.restart() {
                                lines.push(format!("restarting Queqiao failed: {error}"));
                            }
                        }
                    }
                    Some(Message::Command(command)) => {
                        let notice = run_command(command, options.dry_run_commands);
                        lines.push(format!("command from the viewer: {command:?}: {notice}"));
                        peer.send_data(Message::Notice(notice).encode());
                    }
                    Some(Message::ClipboardText(text)) => {
                        clipboard_stats.received(&text);
                        if let Some(clipboard) = &clipboard
                            && clipboard_stats.last_in.as_ref() != Some(&text)
                        {
                            clipboard.set_text(text.clone());
                            clipboard_stats.last_in = Some(text);
                        }
                    }
                    _ => {}
                },
                PeerEvent::Closed(reason) => {
                    closed = Some(reason);
                    break 'session;
                }
                PeerEvent::Path(path) => lines.push(format!(
                    "path after {} ms: {path}",
                    started.elapsed().as_millis()
                )),
                PeerEvent::Note(note) => {
                    lines.push(format!("{} ms: {note}", started.elapsed().as_millis()))
                }
                PeerEvent::Bitrate(estimate) => {
                    last_estimate = estimate;
                    let target = adapted_bitrate(estimate, max_bitrate);
                    let current = encoder.bitrate();
                    let big_enough = target.abs_diff(current) > current / 20;
                    let spaced =
                        last_bitrate_change.is_none_or(|at| at.elapsed() >= BITRATE_CHANGE_GAP);
                    if big_enough && spaced && encoder.set_bitrate(target).is_ok() {
                        bitrate_changes += 1;
                        lowest_bitrate = lowest_bitrate.min(target);
                        last_bitrate_change = Some(Instant::now());
                    }
                }
                PeerEvent::ChannelOpen => {
                    channel_open_at = Some(Instant::now());
                    // The viewer can offer the other screens in its menu.
                    if let Ok(list) = queqiao_capture::screens() {
                        let sizes = list
                            .iter()
                            .map(|s| (s.width.min(65535) as u16, s.height.min(65535) as u16))
                            .collect();
                        let current = screen_index.min(255) as u8;
                        peer.send_data(Message::Screens { current, sizes }.encode());
                    }
                }
                PeerEvent::StreamOpen(Stream::Files) => {
                    report_files(transfers.opened(), viewer, lines, started.elapsed())
                }
                PeerEvent::StreamOpen(Stream::Clipboard) => {}
                PeerEvent::StreamData(Stream::Clipboard, bytes) => {
                    if let Some(result) = image_in.handle(&bytes) {
                        match result {
                            Ok(image) => {
                                images_received += 1;
                                image_test_received |= image == test_picture(1);
                                let print = image_fingerprint(&image);
                                if last_image_in != Some(print) {
                                    last_image_in = Some(print);
                                    if let Some(clipboard) = &clipboard {
                                        clipboard.set(ClipContent::Image(image));
                                    }
                                }
                            }
                            Err(error) => lines.push(format!("clipboard image dropped: {error}")),
                        }
                    }
                }
                PeerEvent::StreamOpen(Stream::Terminal) => {}
                PeerEvent::StreamData(Stream::Terminal, bytes) => {
                    if let Some(message) = TerminalMessage::decode(&bytes) {
                        for reply in terminals.handle(message) {
                            match &reply {
                                TerminalMessage::Opened { id, shell } => {
                                    lines.push(format!("command line {id} opened ({shell})"))
                                }
                                TerminalMessage::Failed { id, reason } => {
                                    lines.push(format!("command line {id} failed: {reason}"))
                                }
                                _ => {}
                            }
                            peer.send_stream(Stream::Terminal, reply.encode());
                        }
                    }
                }
                PeerEvent::StreamData(Stream::Files, bytes) => {
                    for reply in transfers.on_message(&bytes) {
                        peer.send_stream(Stream::Files, reply);
                    }
                }
                // The viewer's microphone, into the virtual microphone.
                PeerEvent::Audio { data, time_48khz } => {
                    if let Some((player, _)) = &microphone {
                        player.push(data, time_48khz);
                        microphone_packets += 1;
                    }
                }
                PeerEvent::Video { .. } => {}
            }
            watch.event_done(event_started);
        }
        watch.mark("the viewer's messages");

        for message in transfers.take(peer.room(Stream::Files)) {
            peer.send_stream(Stream::Files, message);
        }
        let outcomes = transfers.outcomes();
        for outcome in &outcomes {
            if let Outcome::Received { batch, top, .. } = outcome
                && *batch > 0
            {
                let files = clip_files.entry(*batch).or_default();
                if !files.contains(top) {
                    files.push(top.clone());
                }
                clip_files_set += 1;
                if let Some(clipboard) = &clipboard {
                    clipboard.set(ClipContent::Files(files.clone()));
                }
            }
        }
        if outcomes
            .iter()
            .any(|o| matches!(o, Outcome::Sent { .. } | Outcome::Received { .. }))
        {
            lines.extend(transfers.take_courses());
        }
        report_files(outcomes, viewer, lines, started.elapsed());
        for message in images.take(peer.room(Stream::Clipboard)) {
            peer.send_stream(Stream::Clipboard, message);
        }
        for message in terminals.poll(peer.room(Stream::Terminal)) {
            if let TerminalMessage::Exited { id, code } = &message {
                lines.push(format!("command line {id} ended with exit code {code}"));
            }
            peer.send_stream(Stream::Terminal, message.encode());
        }

        if privacy_check_due {
            privacy_check_due = false;
            if let Some(own) = duplicator.as_mut() {
                lines.push(privacy_check(own, ui));
            }
        }
        if let Some(capture) = &sound {
            for note in capture.notes() {
                lines.push(format!("{} ms: {note}", started.elapsed().as_millis()));
            }
        }
        if let Some(opened) = channel_open_at {
            match clipboard.as_ref().and_then(Clipboard::changed) {
                Some(ClipContent::Text(text))
                    if clipboard_stats.last_in.as_ref() != Some(&text)
                        && text.len() <= MAX_CLIPBOARD_TEXT =>
                {
                    peer.send_data(Message::ClipboardText(text).encode());
                    clipboard_stats.sent += 1;
                }
                // Files copied here go over at once (not those that came from the other side).
                Some(ClipContent::Files(paths)) if !helper || receive_files => {
                    let own = downloads_folder();
                    let paths: Vec<PathBuf> = paths
                        .into_iter()
                        .filter(|path| !path.starts_with(&own))
                        .collect();
                    if !paths.is_empty() && clipboard_size(&paths) <= CLIPBOARD_FILES_LIMIT {
                        let _ = transfers.send_clipboard(&paths);
                        lines.push(format!("clipboard files: sending {}", paths.len()));
                    }
                }
                // Queued (compressed) to go on the clipboard stream.
                Some(ClipContent::Image(image))
                    if last_image_in != Some(image_fingerprint(&image))
                        && images.send(&image).is_ok() =>
                {
                    images_sent += 1
                }
                _ => {}
            }
            if options.clipboard_test
                && !clipboard_stats.test_sent
                && opened.elapsed() >= Duration::from_secs(1)
            {
                let text = format!("{CLIPBOARD_TEST_TEXT} (from the agent)");
                peer.send_data(Message::ClipboardText(text).encode());
                clipboard_stats.sent += 1;
                if images.send(&test_picture(2)).is_ok() {
                    images_sent += 1;
                }
                clipboard_stats.test_sent = true;
            }
        }

        watch.mark("files, clipboard, command lines, sound, the channel's first seconds");
        // Capture: wait for a screen change, at most until the next frame is due and at most 5 ms, so
        // input from the viewer is handled quickly.
        let now = Instant::now();
        let due = last_encode.map_or(now, |last| last + interval);
        let wait = if waiting.is_some() {
            due.saturating_duration_since(now).as_millis().min(5) as u32
        } else {
            5
        };
        let fixed = options.fps.is_some();
        // The pointer: its shape when it changed (the viewer's cursor takes it), and when it hides or shows.
        if let Some(own) = duplicator.as_mut() {
            if let Some(raw) = own.take_pointer_shape() {
                match pointer_shape(&raw) {
                    Some(shape) if last_pointer.as_ref() != Some(&shape) => {
                        peer.send_data(Message::PointerShape(shape.clone()).encode());
                        pointer_shapes += 1;
                        last_pointer = Some(shape);
                    }
                    Some(_) => {}
                    None => pointer_skipped += 1,
                }
            }
            if let Some(visible) = own.take_pointer_visible() {
                peer.send_data(Message::PointerVisible(visible).encode());
            }
        }
        let acquired = duplicator.as_mut().map(|own| own.acquire(wait));
        match acquired {
            Some(Ok(Some(frame))) if frame.present_qpc != 0 => {
                stats.captured += 1;
                keep_frame(&context, &mut latest, &frame);
                waiting = Some((frame.present_qpc, frame.acquired_qpc));
            }
            Some(Ok(_)) => {}
            Some(Err(error)) if needs_restart(&error) => {
                // The desktop switched (sign-in, lock, Ctrl+Alt+Del or UAC screen) or the display changed.
                duplicator = None;
                match open_screen(screen_index, fps, fixed, resolution_limit, encoder.codec()) {
                    Ok(opened) => {
                        adopt!(opened);
                        lines.push(format!(
                            "{} ms: the display changed: captured again ({}x{})",
                            started.elapsed().as_millis(),
                            info.width,
                            info.height
                        ));
                    }
                    Err(error) => match links.desktop.as_mut() {
                        Some(link) if !link.closed => {
                            link.capture(screen_index);
                            secure_switches += 1;
                            last_own_try = Instant::now();
                            lines.push(format!(
                                "{} ms: another desktop takes input ({error}): pictures and input through the desktop helper",
                                started.elapsed().as_millis()
                            ));
                            // Windows' own screen came up (Ctrl+Alt+Del, the lock screen, a UAC prompt): the dark
                            // screen and the local block end, so the person there is never shut out.
                            if local_block.is_some() {
                                let _ = ui.set_privacy(None);
                                end_local_block(&mut local_block, lines);
                                peer.send_data(
                                    Message::Notice(
                                        "Windows' own screen came up on that PC (Ctrl+Alt+Del, lock or UAC): its screen is shown again and its keyboard and mouse work"
                                            .into(),
                                    )
                                    .encode(),
                                );
                            }
                        }
                        _ => {
                            return Err(format!(
                                "the desktop switched (sign-in, lock or UAC screen, or a display change) and cannot be captured: {error}"
                            ));
                        }
                    },
                }
            }
            Some(Err(error)) => return Err(describe(error)),
            None => {
                // Pictures from the desktop helper until this PC's own desktop takes input again.
                let Some(link) = links.desktop.as_mut() else {
                    return Err("nothing captures this PC's screen".into());
                };
                let gap = last_own_try.elapsed();
                let back = ((link.on_own_desktop() || link.closed)
                    && gap >= Duration::from_millis(250))
                    || gap >= Duration::from_secs(3);
                if back {
                    last_own_try = Instant::now();
                    if let Ok(opened) =
                        open_screen(screen_index, fps, fixed, resolution_limit, encoder.codec())
                    {
                        link.stop_capture();
                        adopt!(opened);
                        lines.push(format!(
                            "{} ms: this PC's own desktop takes input again: captured here",
                            started.elapsed().as_millis()
                        ));
                    }
                }
                if let Some(link) = links.desktop.as_mut().filter(|_| duplicator.is_none()) {
                    if link.closed {
                        return Err(
                            "the desktop helper is gone and this PC's own desktop cannot be captured"
                                .into(),
                        );
                    }
                    match link.next(Duration::from_millis(u64::from(wait))) {
                        Some(FromHelper::Frame {
                            width,
                            height,
                            pixels,
                        }) => {
                            if (width, height) == screen {
                                latest = upload(
                                    &device,
                                    &BgraImage {
                                        width,
                                        height,
                                        pixels,
                                    },
                                )?;
                                stats.captured += 1;
                                secure_frames += 1;
                                waiting = Some((0, qpc_now()));
                            } else if !size_noted {
                                size_noted = true;
                                lines.push(format!(
                                    "a {width}x{height} picture from the desktop helper does not fit the {}x{} screen: left out",
                                    screen.0, screen.1
                                ));
                            }
                        }
                        Some(FromHelper::Failed(error)) => lines.push(format!(
                            "{} ms: desktop helper: {error}",
                            started.elapsed().as_millis()
                        )),
                        Some(FromHelper::Desktop(name)) => lines.push(format!(
                            "{} ms: input desktop: {name}",
                            started.elapsed().as_millis()
                        )),
                        Some(
                            FromHelper::EndPressed
                            | FromHelper::NoBanner(_)
                            | FromHelper::Screen { .. },
                        )
                        | None => {}
                    }
                }
            }
        }
        // While capturing here: the helper's word on desktop changes (and no pictures).
        if duplicator.is_some()
            && let Some(link) = links.desktop.as_mut()
        {
            for name in link.poll_desktop() {
                lines.push(format!(
                    "{} ms: input desktop: {name}",
                    started.elapsed().as_millis()
                ));
            }
        }

        watch.mark("capture");
        // Encode as soon as there is a change and the frame time has passed.
        if connected_after.is_some()
            && Instant::now() >= due
            && !gate.closed()
            && let Some((screen_time, captured_time)) = waiting.take()
        {
            let frames = encoder
                .encode(&latest, force_keyframe)
                .map_err(|e| format!("encode: {}", describe(e)))?;
            force_keyframe = false;
            last_encode = Some(Instant::now());
            for frame in frames {
                let time_90khz = (frame.time.max(0) as u64) * 9 / 1000;
                stats.sent += 1;
                stats.bytes += frame.data.len();
                stats.keyframes += u32::from(frame.keyframe);
                if frame_sizes.len() < 200_000 {
                    frame_sizes.push(frame.data.len() as f64 / 1e3);
                }
                if frame.keyframe && keyframe_sizes.len() < 100 {
                    keyframe_sizes.push(format!("{:.0}", frame.data.len() as f64 / 1e3));
                }
                peer.send_video(frame.data, time_90khz, encoder.codec());
                let origin = if screen_time != 0 {
                    screen_time
                } else {
                    captured_time
                };
                let agent_ms = ms(qpc_now() - origin);
                // Pictures without a screen time (the start-up screenshot, keyframes sent again on
                // request) say nothing about the delay of a screen change.
                if screen_time != 0 {
                    stats.agent_ms.push(agent_ms);
                }
                let info = Message::FrameInfo {
                    time_90khz,
                    agent_ms: agent_ms as f32,
                };
                peer.send_data(info.encode());
            }
        }
        watch.mark("encode");
        if let Some(note) = watch.slow(Duration::from_millis(300))
            && slow_notes < 8
        {
            slow_notes += 1;
            lines.push(format!("{} ms: {note}", started.elapsed().as_millis()));
        }
    }
    if let Some(link) = links.desktop.as_mut() {
        link.banner(None);
        link.session(false);
        lines.push(format!(
            "desktop helper: {secure_switches} switches to another desktop, {secure_frames} pictures from it, {secure_inputs} inputs passed to it"
        ));
    }
    if closed.is_none() || ended_here {
        // Our end (time is up, or the person here ended it): tell the viewer at once.
        peer.send_data(Message::Leave.encode());
        std::thread::sleep(Duration::from_millis(100));
    }
    drop(peer);
    if options.verify_input {
        set_cursor_position(pointer_start.0, pointer_start.1);
    }

    let elapsed = started.elapsed().as_secs_f64();
    lines.push(format!(
        "connected after {}",
        connected_after.map_or("never".into(), |d| format!("{} ms", d.as_millis()))
    ));
    lines.push(format!(
        "{elapsed:.1} s: {} screen changes, {} frames sent ({} keyframes, {:.2} MB, {:.1} Mbit/s), {} keyframe requests",
        stats.captured,
        stats.sent,
        stats.keyframes,
        stats.bytes as f64 / 1e6,
        stats.bytes as f64 * 8.0 / elapsed / 1e6,
        stats.keyframe_requests
    ));
    lines.push(format!(
        "screen change -> sent, ms: {}",
        summary(&mut stats.agent_ms)
    ));
    lines.push(clipboard_stats.line());
    if clip_files_set > 0 {
        lines.push(format!(
            "clipboard files: {clip_files_set} put on the clipboard from the other side"
        ));
        // What the clipboard holds now (the watcher puts them there on its own thread).
        std::thread::sleep(Duration::from_millis(300));
        let held = queqiao_input::clipboard_files(None).unwrap_or_default();
        let names: Vec<String> = held.iter().map(|path| path.display().to_string()).collect();
        lines.push(format!("clipboard now holds: {}", names.join(", ")));
    }
    lines.push(format!(
        "clipboard images: {images_received} received, {images_sent} sent; test image from the other side received: {}",
        if image_test_received { "yes" } else { "no" }
    ));
    // The speakers go back to how the person left them, however the session ended.
    if let Some(before) = speakers_before.take() {
        let restored = queqiao_audio::set_mute(options.sound_device.as_deref(), before);
        lines.push(format!(
            "speakers: put back (muted: {before}){}",
            if restored.is_ok() { "" } else { " - failed" }
        ));
    }
    if !terminals.is_empty() {
        lines.push(format!(
            "{} command lines closed with the session",
            terminals.len()
        ));
    }
    drop(terminals);
    sound_report.extend(sound.as_ref().map(sound_line));
    if microphone.is_some() || microphone_packets > 0 {
        lines.push(format!(
            "microphone from the viewer: {microphone_packets} packets played"
        ));
    }
    if let Some(check) = microphone_check.take() {
        lines.push(match check.join() {
            Ok(Ok((heard, tone))) => format!(
                "microphone check: {} (the test tone on the virtual microphone in {tone} of {heard} frames with sound)",
                if tone >= 50 { "PASS" } else { "FAIL" }
            ),
            Ok(Err(error)) => format!("microphone check: FAIL ({error})"),
            Err(_) => "microphone check: FAIL (its thread failed)".into(),
        });
    }
    lines.extend(sound_report);
    if transfers.busy() {
        lines.push("files: a transfer was cut off when the session ended".into());
    }
    lines.push(format!(
        "pointer: {pointer_shapes} shapes sent{}",
        if pointer_skipped > 0 {
            format!(", {pointer_skipped} not (too large or of an unknown kind)")
        } else {
            String::new()
        }
    ));
    lines.push(format!(
        "bitrate course, every second (estimate/encoder/sent Mbit/s@frames, K a keyframe): {}",
        course.join(" ")
    ));
    lines.push(format!(
        "frame size, kB: {}; keyframes, kB: {}",
        summary(&mut frame_sizes),
        keyframe_sizes.join(" ")
    ));
    lines.push(format!(
        "bitrate: started at {:.1}, lowest {:.1}, last {:.1}, highest allowed {:.1} Mbit/s; {bitrate_changes} changes",
        f64::from(start_bitrate) / 1e6,
        f64::from(lowest_bitrate) / 1e6,
        f64::from(encoder.bitrate()) / 1e6,
        f64::from(max_bitrate) / 1e6
    ));
    lines.push(format!(
        "input: {} events, {} refused{}",
        stats.inputs,
        stats.inject_failures,
        if stats.inject_failures > 0 {
            format!(" ({})", stats.last_inject_error)
        } else {
            String::new()
        }
    ));
    if options.verify_input {
        lines.push(format!(
            "input check: pointer at the expected place {}/{}, test key state right {}/{}{}",
            stats.moves_ok,
            stats.moves_checked,
            stats.keys_ok,
            stats.keys_checked,
            if stats.deltas_checked > 0 {
                format!(
                    ", relative moves right {}/{}",
                    stats.deltas_ok, stats.deltas_checked
                )
            } else {
                String::new()
            }
        ));
        lines.push(format!("test key checks: {}", stats.key_notes.join(", ")));
    }
    let ended = closed.unwrap_or_else(|| "time is up".into());
    lines.push(format!("ended: {ended}"));
    let input_ok = !options.verify_input
        || (stats.moves_checked > 0
            && stats.moves_ok == stats.moves_checked
            && stats.keys_checked == 2
            && stats.keys_ok == 2
            && stats.deltas_ok == stats.deltas_checked);
    let passed =
        connected_after.is_some() && stats.sent > 0 && stats.inject_failures == 0 && input_ok;
    Ok(SessionOutcome { passed, ended })
}

/// After an injected event: is the pointer where it should be, is the test key in the right state?
/// A relative move must move the pointer the same way (the distance follows the PC's pointer speed), unless it
/// was already at that edge of the desktop.
pub(crate) fn verify_delta(event: &InputEvent, before: (i32, i32), stats: &mut Stats) {
    let InputEvent::MouseDelta { dx, dy } = *event else {
        return;
    };
    let after = cursor_position();
    let right_way =
        |moved: i32, wanted: i16| wanted == 0 || moved.signum() == i32::from(wanted.signum());
    stats.deltas_checked += 1;
    if right_way(after.0 - before.0, dx) && right_way(after.1 - before.1, dy) {
        stats.deltas_ok += 1;
    }
}

/// Windows applies injected input asynchronously (SendInput returns before the key state or the pointer has
/// changed, later still when another program has a low-level hook): a check waits up to this long for it. The time
/// it took is reported. 50 ms was too short on sunshine on 2026-11-23 (its key down "NOT within 50 ms" once, in 0 ms
/// otherwise): up to a second tells a late key from a lost one.
pub(crate) const VERIFY_WAIT: Duration = Duration::from_millis(1000);

/// Whether `done` turns true within [`VERIFY_WAIT`].
pub(crate) fn within_verify_wait(mut done: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + VERIFY_WAIT;
    loop {
        if done() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

pub(crate) fn verify(injector: &Injector, event: &InputEvent, stats: &mut Stats) {
    match *event {
        InputEvent::MouseMove { x, y } => {
            let expected = injector.screen().desktop_point(x, y);
            stats.moves_checked += 1;
            if within_verify_wait(|| {
                let actual = cursor_position();
                (expected.0 - actual.0).abs() <= 2 && (expected.1 - actual.1).abs() <= 2
            }) {
                stats.moves_ok += 1;
            }
        }
        InputEvent::Key { scancode, down, .. } if scancode == TEST_SCANCODE => {
            stats.keys_checked += 1;
            let checking = Instant::now();
            let ok = within_verify_wait(|| key_is_down(VK_F24) == down);
            stats.key_notes.push(format!(
                "{} {}",
                if down { "down" } else { "up" },
                if ok {
                    format!("in {} ms", checking.elapsed().as_millis())
                } else {
                    format!("NOT within {} ms", VERIFY_WAIT.as_millis())
                }
            ));
            if ok {
                stats.keys_ok += 1;
            }
        }
        _ => {}
    }
}

/// The desktop helper's first picture (a session that starts while another desktop takes input).
pub(crate) fn first_helper_picture(
    link: &mut service_link::DesktopLink,
) -> Result<BgraImage, String> {
    let until = Instant::now() + Duration::from_secs(3);
    let mut failure = String::new();
    while let Some(left) = until.checked_duration_since(Instant::now()) {
        match link.next(left) {
            Some(FromHelper::Frame {
                width,
                height,
                pixels,
            }) => {
                return Ok(BgraImage {
                    width,
                    height,
                    pixels,
                });
            }
            Some(FromHelper::Failed(error)) => failure = error,
            Some(
                FromHelper::Desktop(_)
                | FromHelper::EndPressed
                | FromHelper::NoBanner(_)
                | FromHelper::Screen { .. },
            ) => {}
            None if link.closed => break,
            None => {}
        }
    }
    Err(format!(
        "the desktop helper sent no picture within 3 s{}",
        if failure.is_empty() {
            String::new()
        } else {
            format!(" ({failure})")
        }
    ))
}

pub(crate) fn summary(values: &mut [f64]) -> String {
    values.sort_by(f64::total_cmp);
    let at = |q: f64| {
        values
            .get(((values.len().max(1) - 1) as f64 * q).round() as usize)
            .copied()
            .unwrap_or(f64::NAN)
    };
    format!(
        "median {:.2}, p95 {:.2}, max {:.2} (n={})",
        at(0.5),
        at(0.95),
        at(1.0),
        values.len()
    )
}

/// A device or account name as prompts and banners show it: no control or direction characters (a name with line
/// breaks or a right-to-left mark could push the account out of sight: the security review of 2026-11-22), at most
/// 40 characters.
pub(crate) fn clean_name(name: &str) -> String {
    let direction = |c: char| matches!(c, '\u{200E}' | '\u{200F}' | '\u{061C}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}');
    let kept: String = name
        .chars()
        .filter(|c| !c.is_control() && !direction(*c))
        .collect();
    let kept = kept.trim();
    if kept.chars().count() > 40 {
        format!("{}…", kept.chars().take(39).collect::<String>())
    } else {
        kept.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn h265_falls_back_to_what_the_viewer_decodes() {
        use queqiao_core::protocol::{VideoCodec, hevc};
        let format = |chroma444, ten_bit| EncodeFormat {
            codec: queqiao_codec::Codec::H265,
            chroma444,
            ten_bit,
        };
        let all = hevc::MAIN | hevc::MAIN10 | hevc::CHROMA444 | hevc::CHROMA444_10;
        // 4:4:4 10-bit: then 4:4:4 at 8 bits, 4:2:0 10-bit, 4:2:0.
        assert_eq!(
            fallbacks(VideoCodec::H265, format(true, true), all),
            vec![
                format(true, true),
                format(true, false),
                format(false, true),
                format(false, false)
            ]
        );
        // Never what the viewer cannot decode (here no Main 10).
        assert_eq!(
            fallbacks(VideoCodec::H265, format(true, true), all & !hevc::MAIN10),
            vec![
                format(true, true),
                format(true, false),
                format(false, false)
            ]
        );
        assert_eq!(
            fallbacks(VideoCodec::H265, format(false, true), all),
            vec![format(false, true), format(false, false)]
        );
        assert_eq!(
            fallbacks(VideoCodec::H264, EncodeFormat::H264, 0),
            vec![EncodeFormat::H264]
        );
    }
}
