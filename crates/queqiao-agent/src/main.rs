//! queqiao-agent: the controlled side. Captures the screen, encodes it and streams it to the viewer, and
//! injects the viewer's mouse and keyboard input. Runs in the logged-on user's desktop session.
//!
//! With an account (M3): the agent logs in to queqiao-server, stays online and serves each viewer of the
//! same account that connects, one at a time.
//!   queqiao-agent --server <host:port> --pin <fingerprint> --user <name> --password <password>
//!       logs in and saves the login (the refresh token, DPAPI-encrypted); later starts need no options:
//!   queqiao-agent [--sessions N] [--seconds N]
//! By IP address (M2, for tests):
//!   queqiao-agent --connect <viewer-ip:port> | --listen <ip:port>
//! Common options: [--seconds N] [--fps N] [--report <file>] [--verify-input] [--force <switches>]
//! --seconds limits the whole run; --sessions N stops after N sessions. --trust-new-key <device id> accepts
//! that device's new key (after a reinstall; sessions with a device whose key changed are refused otherwise). --verify-input checks every injected
//! pointer move and the test key (F24) and puts the pointer back at the end. --version prints the version.

#![windows_subsystem = "windows"]

mod ui;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use queqiao_account::{Account, AccountEvent, DeviceIdentity, Role as AccountRole, SavedLogin};
use queqiao_capture::{BgraImage, Duplicator, needs_restart, qpc_frequency, qpc_now};
use queqiao_codec::ScreenEncoder;
use queqiao_core::protocol::{InputEvent, Message};
use queqiao_display::PresenceEvent;
use queqiao_input::{Injector, ScreenRect, cursor_position, key_is_down, set_cursor_position};
use queqiao_transport::{
    Peer, PeerEvent, Role, START_BITRATE, Signal, Signaling, check_hello, hello_body,
};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};

/// The test key: F24 exists on no normal keyboard and no program reacts to it.
const TEST_SCANCODE: u16 = 0x76;
const VK_F24: u16 = 0x87;
/// Adaptive bitrate: the encoder never goes below this, changes at most this often, and runs at this share of
/// the estimated bandwidth (the rest is headroom for retransmissions and the pacer).
const MIN_BITRATE: u32 = 500_000;
const BITRATE_CHANGE_GAP: Duration = Duration::from_millis(250);
const BANDWIDTH_SHARE: f64 = 0.85;

/// The encoder bitrate for a bandwidth estimate.
fn adapted_bitrate(estimate: u64, max: u32) -> u32 {
    ((estimate as f64 * BANDWIDTH_SHARE) as u64)
        .clamp(u64::from(MIN_BITRATE), u64::from(max.max(MIN_BITRATE))) as u32
}

/// How long the person at the PC has to allow a session (the viewer waits 30 s for the answer).
const CONSENT_SECONDS: u32 = 25;

/// A session that is not connected after this long is given up.
const CONNECT_LIMIT: Duration = Duration::from_secs(30);

#[derive(Default)]
struct Options {
    /// A device whose new key the person confirmed (its old pin is dropped).
    trust_new_key: Option<String>,
    /// Ask the person at the PC before each session in this run (whatever the setting says).
    ask: bool,
    /// For tests: answer the consent prompt (true = Allow) as a click would, after 1 s.
    answer: Option<bool>,
    /// For tests: press the banner's End button this many seconds into a session.
    end_after: Option<u64>,
    /// For tests: save pictures of the banner and the consent prompt in this folder.
    ui_pictures: Option<PathBuf>,
    connect: Option<String>,
    listen: Option<String>,
    server: Option<String>,
    pin: Option<String>,
    user: Option<String>,
    password: Option<String>,
    sessions: Option<u32>,
    seconds: Option<u64>,
    fps: Option<u32>,
    report: Option<PathBuf>,
    verify_input: bool,
}

#[derive(Default)]
struct Stats {
    captured: u32,
    sent: u32,
    keyframes: u32,
    bytes: usize,
    keyframe_requests: u32,
    inputs: u32,
    inject_failures: u32,
    last_inject_error: String,
    moves_checked: u32,
    moves_ok: u32,
    keys_checked: u32,
    keys_ok: u32,
    agent_ms: Vec<f64>,
}

fn main() {
    let banner = queqiao_core::banner(env!("CARGO_PKG_NAME"));
    let Some(options) = parse(std::env::args().skip(1)) else {
        println!("{banner}");
        println!(
            "usage: queqiao-agent [--server <host:port> --pin <fingerprint> --user <name> --password <password>] [--sessions N]"
        );
        println!("       queqiao-agent --connect <viewer-ip:port> | --listen <ip:port>");
        println!("       common: [--seconds N] [--fps N] [--report <file>] [--verify-input]");
        return;
    };
    let mut lines = vec![banner];
    let passed = match run(&options, &mut lines) {
        Ok(passed) => passed,
        Err(error) => {
            lines.push(format!("ERROR {error}"));
            false
        }
    };
    lines.push(format!("RESULT {}", if passed { "PASS" } else { "FAIL" }));
    match &options.report {
        Some(path) => {
            let _ = std::fs::write(path, lines.join("\r\n") + "\r\n");
        }
        None => lines.iter().for_each(|line| println!("{line}")),
    }
    std::process::exit(if passed { 0 } else { 1 });
}

fn parse(mut args: impl Iterator<Item = String>) -> Option<Options> {
    let mut options = Options::default();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--connect" => options.connect = args.next(),
            "--listen" => options.listen = args.next(),
            "--server" => options.server = args.next(),
            "--pin" => options.pin = args.next(),
            "--user" => options.user = args.next(),
            "--password" => options.password = args.next(),
            "--sessions" => options.sessions = args.next().and_then(|v| v.parse().ok()),
            "--seconds" => options.seconds = args.next().and_then(|v| v.parse().ok()),
            "--fps" => options.fps = args.next().and_then(|v| v.parse().ok()),
            "--report" => options.report = args.next().map(PathBuf::from),
            "--verify-input" => options.verify_input = true,
            "--trust-new-key" => options.trust_new_key = args.next(),
            "--ask" => options.ask = true,
            "--answer" => options.answer = Some(args.next()? == "allow"),
            "--end-after" => options.end_after = args.next().and_then(|v| v.parse().ok()),
            "--ui-pictures" => options.ui_pictures = args.next().map(PathBuf::from),
            "--force" => queqiao_core::set_forced(&args.next()?),
            _ => return None,
        }
    }
    Some(options)
}

fn run(options: &Options, lines: &mut Vec<String>) -> Result<bool, String> {
    lines.extend(queqiao_core::platform::prepare_process());
    let end = options
        .seconds
        .map(|s| Instant::now() + Duration::from_secs(s));
    // Nothing happens unseen: the tray icon whenever the agent runs, a banner during every session.
    let settings = ui::Settings::load();
    let mut shown = settings.clone();
    shown.ask_before_session |= options.ask;
    let ui = ui::Ui::start(&shown, options.ui_pictures.clone())?;
    lines.push(format!(
        "tray icon {}; this account's devices may control this PC: {}; ask first: {}",
        if ui.tray_shown {
            "shown"
        } else {
            "not shown (no taskbar yet)"
        },
        if settings.allow_account_control {
            "yes"
        } else {
            "no"
        },
        if shown.ask_before_session {
            "yes"
        } else {
            "no"
        }
    ));
    if options.connect.is_none() && options.listen.is_none() {
        return run_account(options, end, &ui, settings, lines);
    }
    let mut signal = match (&options.connect, &options.listen) {
        (Some(address), _) => Signal::connect(address, Duration::from_secs(60)),
        (None, Some(address)) => Signal::listen(address, Duration::from_secs(120)),
        (None, None) => unreachable!("checked above"),
    }
    .map_err(|e| e.to_string())?;
    signal.hello("agent").map_err(|e| e.to_string())?;
    let viewer = signal.peer_address().map_err(|e| e.to_string())?;
    let mut deferred = Vec::new();
    let outcome = session(
        options,
        &mut signal,
        &viewer.to_string(),
        "direct",
        end,
        &ui,
        &mut deferred,
        lines,
    )?;
    Ok(outcome.passed)
}

/// Logged in to queqiao-server: waits for viewers of the account and serves them one after another.
/// When the server connection drops, it reconnects with the saved login (waiting 1 s, 2 s, ... 30 s).
fn run_account(
    options: &Options,
    end: Option<Instant>,
    ui: &ui::Ui,
    mut settings: ui::Settings,
    lines: &mut Vec<String>,
) -> Result<bool, String> {
    let identity = DeviceIdentity::load_or_create().map_err(|e| e.to_string())?;
    let mut login = SavedLogin::load(AccountRole::Agent).unwrap_or_default();
    if let Some(server) = &options.server {
        if login.server != *server {
            login.refresh_token = None;
        }
        login.server = server.clone();
    }
    if let Some(pin) = &options.pin {
        login.pin = pin.clone();
    }
    if login.server.is_empty() {
        return Err("not logged in: start once with --server <host:port> --pin <fingerprint> --user <name> --password <password>".into());
    }
    let password = options.user.as_deref().zip(options.password.as_deref());
    let mut account = Account::connect(&mut login, &identity, AccountRole::Agent, password)
        .map_err(|e| e.to_string())?;
    login.save(AccountRole::Agent).map_err(|e| e.to_string())?;
    if let Some(device) = &options.trust_new_key {
        account.trust_new_key(device).map_err(|e| e.to_string())?;
        lines.push(format!("the key of device {device} will be pinned anew"));
    }
    lines.push(format!(
        "logged in as {} at {}; this device: {} ({}); {} devices on the account",
        account.user,
        login.server,
        identity.name,
        identity.id,
        account.devices.len()
    ));

    let (mut sessions, mut passed) = (0u32, true);
    let mut retry = Duration::from_secs(1);
    let mut deferred: Vec<PresenceEvent> = Vec::new();
    loop {
        if end.is_some_and(|end| Instant::now() > end)
            || options.sessions.is_some_and(|n| sessions >= n)
        {
            break;
        }
        // What the person did in the tray (also during the last session).
        deferred.extend(std::iter::from_fn(|| ui.try_event()));
        let mut quit = false;
        for event in deferred.drain(..) {
            match event {
                PresenceEvent::Quit => quit = true,
                PresenceEvent::SetAllow(on) => {
                    settings.allow_account_control = on;
                    let _ = settings.save();
                    lines.push(format!(
                        "the person at this PC set: this account's devices may control it: {on}"
                    ));
                }
                PresenceEvent::SetAsk(on) => {
                    settings.ask_before_session = on;
                    let _ = settings.save();
                    lines.push(format!(
                        "the person at this PC set: ask before each session: {on}"
                    ));
                }
                PresenceEvent::EndSession | PresenceEvent::Consent(_) => {}
            }
        }
        if quit {
            lines.push("quit from the tray icon".into());
            break;
        }
        match account.next_event(Duration::from_millis(200)) {
            Some(AccountEvent::Signal {
                from,
                from_name,
                from_role: AccountRole::Viewer,
                session: id,
                kind,
                body,
                signature,
            }) if kind == "hello" => {
                // Only a device of this account whose key matches its pin, signing what it sends.
                if let Err(error) =
                    account.check_signal(&from, AccountRole::Viewer, &id, &kind, &body, &signature)
                {
                    lines.push(format!("refused a session request: {error}"));
                    continue;
                }
                let mut signal = account.session(&from, AccountRole::Viewer, &id);
                if let Err(error) = check_hello(&body, "agent") {
                    let _ = signal.send("error", &error.0);
                    lines.push(format!("refused viewer {from_name}: {error}"));
                    continue;
                }
                if !settings.allow_account_control {
                    let _ = signal.send(
                        "error",
                        "control by this account's devices is turned off on that PC",
                    );
                    lines.push(format!(
                        "refused viewer {from_name}: control by this account's devices is off"
                    ));
                    continue;
                }
                if options.ask || settings.ask_before_session {
                    match ask_consent(ui, &from_name, options.answer) {
                        Ok(true) => {
                            lines.push(format!("the person at this PC allowed {from_name}"))
                        }
                        Ok(false) => {
                            let _ = signal.send("error", "the person at that PC declined");
                            lines.push(format!("the person at this PC declined {from_name}"));
                            continue;
                        }
                        Err(error) => {
                            let _ = signal
                                .send("error", "the consent prompt could not be shown on that PC");
                            lines.push(format!(
                                "refused viewer {from_name}: consent prompt: {error}"
                            ));
                            continue;
                        }
                    }
                }
                signal
                    .send("hello", &hello_body("agent"))
                    .map_err(|e| e.to_string())?;
                sessions += 1;
                lines.push(format!("session {sessions}: viewer {from_name} ({from})"));
                let started_at = std::time::SystemTime::now();
                match session(
                    options,
                    &mut signal,
                    &from_name,
                    &from,
                    end,
                    ui,
                    &mut deferred,
                    lines,
                ) {
                    Ok(outcome) => {
                        passed &= outcome.passed;
                        ui::record_session(&from_name, &from, started_at, &outcome.ended);
                    }
                    Err(error) => {
                        lines.push(format!("session {sessions} failed: {error}"));
                        ui::record_session(
                            &from_name,
                            &from,
                            started_at,
                            &format!("failed: {error}"),
                        );
                        passed = false;
                    }
                }
            }
            Some(AccountEvent::Closed(reason)) => {
                lines.push(format!("server connection lost: {reason}"));
                loop {
                    if end.is_some_and(|end| Instant::now() + retry > end) {
                        return Err("the server connection was lost".into());
                    }
                    std::thread::sleep(retry);
                    match Account::connect(&mut login, &identity, AccountRole::Agent, None) {
                        Ok(new) => {
                            account = new;
                            retry = Duration::from_secs(1);
                            lines.push("reconnected".into());
                            break;
                        }
                        Err(error) if error.0.contains("log in again") => return Err(error.0),
                        Err(_) => retry = (retry * 2).min(Duration::from_secs(30)),
                    }
                }
            }
            Some(AccountEvent::Error(message)) => lines.push(format!("server: {message}")),
            Some(
                AccountEvent::Devices(_)
                | AccountEvent::Signal { .. }
                | AccountEvent::IceServers { .. },
            )
            | None => {}
        }
    }
    lines.push(format!("{sessions} sessions"));
    Ok(passed && options.sessions.is_none_or(|n| sessions >= n))
}

/// Asks the person at the PC whether `viewer` may control it; false when denied or unanswered.
fn ask_consent(ui: &ui::Ui, viewer: &str, answer: Option<bool>) -> Result<bool, String> {
    ui.ask(viewer, CONSENT_SECONDS)?;
    if let Some(answer) = answer {
        std::thread::sleep(Duration::from_secs(1));
        ui.press_consent(answer);
    }
    let deadline = Instant::now() + Duration::from_secs(u64::from(CONSENT_SECONDS) + 2);
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Ok(false);
        }
        if let Some(PresenceEvent::Consent(allowed)) = ui.wait_event(left) {
            return Ok(allowed);
        }
    }
}

/// How a session went.
struct SessionOutcome {
    passed: bool,
    ended: String,
}

/// Takes the banner down however the session ends.
struct BannerGuard<'a>(&'a ui::Ui);

impl Drop for BannerGuard<'_> {
    fn drop(&mut self) {
        self.0.session_ended();
    }
}

/// One session with one viewer: capture, encode and send the screen, inject the viewer's input. The banner
/// shows it on this PC the whole time; tray and banner actions that are not about the session are put in
/// `deferred`.
#[allow(clippy::too_many_arguments)]
fn session(
    options: &Options,
    signal: &mut dyn Signaling,
    viewer: &str,
    viewer_id: &str,
    end: Option<Instant>,
    ui: &ui::Ui,
    deferred: &mut Vec<PresenceEvent>,
    lines: &mut Vec<String>,
) -> Result<SessionOutcome, String> {
    let _ = viewer_id;
    ui.session_started(viewer)
        .map_err(|e| format!("the banner could not be shown, so the session is refused: {e}"))?;
    let _banner = BannerGuard(ui);
    let frequency = qpc_frequency() as f64;
    let ms = |ticks: i64| ticks as f64 * 1000.0 / frequency;

    let mut duplicator = Duplicator::primary().map_err(describe)?;
    let info = duplicator.info().clone();
    let (device, context) = (duplicator.device().clone(), duplicator.context().clone());
    let snapshot = duplicator.snapshot_image().map_err(describe)?;
    let latest = upload(&device, &snapshot)?;
    let fps = options
        .fps
        .unwrap_or((info.refresh_hz.round() as u32).clamp(1, 60));
    let screen = (snapshot.width, snapshot.height);
    let mut encoder = ScreenEncoder::new(&device, &context, screen, fps, options.fps.is_some())
        .map_err(|e| format!("encoder: {}", describe(e)))?;
    let config = encoder.config().clone();
    lines.push(format!(
        "screen {}x{} at {:.0} Hz on {}",
        info.width, info.height, info.refresh_hz, info.adapter
    ));
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
    let injector = Injector::new(ScreenRect {
        left: info.left,
        top: info.top,
        width: info.width,
        height: info.height,
    });

    lines.push(format!("viewer: {viewer}"));
    let started = Instant::now();
    let peer = Peer::start(Role::Agent, signal).map_err(|e| e.to_string())?;
    lines.extend(peer.candidates().iter().cloned());
    // Adaptive bitrate: start at the transport's starting estimate, then follow the estimate (85 % of it),
    // never above the size-based bitrate the encoder was opened with.
    let max_bitrate = config.bitrate;
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
    let mut lowest_bitrate = encoder.bitrate();
    let mut last_bitrate_change: Option<Instant> = None;

    let pointer_start = cursor_position();
    let interval = Duration::from_secs_f64(1.0 / f64::from(config.fps));
    let mut stats = Stats::default();
    let mut connected_after: Option<Duration> = None;
    // The GDI snapshot is the first picture to send; its screen time is unknown (0).
    let mut waiting: Option<(i64, i64)> = Some((0, qpc_now()));
    let mut force_keyframe = false;
    let mut last_encode: Option<Instant> = None;
    let mut closed = None;

    let mut end_pressed = false;
    let mut ended_here = false;
    'session: loop {
        if end.is_some_and(|end| Instant::now() > end) {
            break;
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
                other => deferred.push(other),
            }
        }
        if connected_after.is_none() && started.elapsed() > CONNECT_LIMIT {
            closed = Some(format!(
                "could not connect within {} s",
                CONNECT_LIMIT.as_secs()
            ));
            break;
        }
        while let Some(event) = peer.try_event() {
            match event {
                PeerEvent::Connected => connected_after = Some(started.elapsed()),
                PeerEvent::KeyframeRequested => {
                    stats.keyframe_requests += 1;
                    force_keyframe = true;
                    waiting.get_or_insert((0, qpc_now()));
                }
                PeerEvent::Data(bytes) => match Message::decode(&bytes) {
                    Some(Message::Input(event)) => {
                        stats.inputs += 1;
                        match injector.inject(&event) {
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
                PeerEvent::ChannelOpen | PeerEvent::Video { .. } => {}
            }
        }

        // Capture: wait for a screen change, at most until the next frame is due and at most 5 ms, so
        // input from the viewer is handled quickly.
        let now = Instant::now();
        let due = last_encode.map_or(now, |last| last + interval);
        let wait = if waiting.is_some() {
            due.saturating_duration_since(now).as_millis().min(5) as u32
        } else {
            5
        };
        match duplicator.acquire(wait) {
            Ok(Some(frame)) if frame.present_qpc != 0 => {
                stats.captured += 1;
                unsafe { context.CopyResource(&latest, &frame.texture) };
                waiting = Some((frame.present_qpc, frame.acquired_qpc));
            }
            Ok(_) => {}
            Err(error) if needs_restart(&error) => {
                return Err("the desktop switched (login screen, UAC or a display change)".into());
            }
            Err(error) => return Err(describe(error)),
        }

        // Encode as soon as there is a change and the frame time has passed.
        if connected_after.is_some()
            && Instant::now() >= due
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
                peer.send_video(frame.data, time_90khz);
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
            "input check: pointer at the expected place {}/{}, test key state right {}/{}",
            stats.moves_ok, stats.moves_checked, stats.keys_ok, stats.keys_checked
        ));
    }
    let ended = closed.unwrap_or_else(|| "time is up".into());
    lines.push(format!("ended: {ended}"));
    let input_ok = !options.verify_input
        || (stats.moves_checked > 0
            && stats.moves_ok == stats.moves_checked
            && stats.keys_checked == 2
            && stats.keys_ok == 2);
    let passed =
        connected_after.is_some() && stats.sent > 0 && stats.inject_failures == 0 && input_ok;
    Ok(SessionOutcome { passed, ended })
}

/// After an injected event: is the pointer where it should be, is the test key in the right state?
fn verify(injector: &Injector, event: &InputEvent, stats: &mut Stats) {
    match *event {
        InputEvent::MouseMove { x, y } => {
            let expected = injector.screen().desktop_point(x, y);
            let actual = cursor_position();
            stats.moves_checked += 1;
            if (expected.0 - actual.0).abs() <= 2 && (expected.1 - actual.1).abs() <= 2 {
                stats.moves_ok += 1;
            }
        }
        InputEvent::Key { scancode, down, .. } if scancode == TEST_SCANCODE => {
            stats.keys_checked += 1;
            if key_is_down(VK_F24) == down {
                stats.keys_ok += 1;
            }
        }
        _ => {}
    }
}

fn upload(device: &ID3D11Device, image: &BgraImage) -> Result<ID3D11Texture2D, String> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: image.width,
        Height: image.height,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let data = D3D11_SUBRESOURCE_DATA {
        pSysMem: image.pixels.as_ptr().cast(),
        SysMemPitch: image.width * 4,
        SysMemSlicePitch: 0,
    };
    let mut out = None;
    unsafe { device.CreateTexture2D(&desc, Some(&data), Some(&mut out)) }.map_err(describe)?;
    out.ok_or_else(|| "CreateTexture2D returned nothing".to_string())
}

fn summary(values: &mut [f64]) -> String {
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

fn describe(error: windows::core::Error) -> String {
    format!("0x{:08X} {}", error.code().0, error.message())
}
