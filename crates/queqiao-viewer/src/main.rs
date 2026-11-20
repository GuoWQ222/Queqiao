//! queqiao-viewer: the controlling side. Shows the remote screen in a window and sends mouse and
//! keyboard input made in that window.
//!
//! M2, by IP address (queqiao-server takes over finding each other in M3):
//!   queqiao-viewer --listen <ip:port> | --connect <agent-ip:port>
//!                  [--seconds N] [--width N] [--report <file>] [--input-test]
//! --input-test sends a fixed sequence instead of the window's input: five pointer moves and a press of
//! F24 (a key no program uses); the agent run with --verify-input checks them. Without arguments the
//! viewer prints its version.

#![windows_subsystem = "windows"]

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use queqiao_codec::{DecodedFrame, DecoderChoice, H264Decoder};
use queqiao_core::protocol::{InputEvent, Message, MouseButton};
use queqiao_display::{Presenter, Window, WindowEvent, fit};
use queqiao_transport::{Peer, PeerEvent, Role, Signal};
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE, D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP,
};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::{IDXGIAdapter, IDXGIDevice};
use windows::core::Interface;

/// The input test: pointer positions as fractions of the remote screen, then F24 down and up.
const TEST_POINTS: [(f64, f64); 5] = [
    (0.25, 0.25),
    (0.75, 0.25),
    (0.75, 0.75),
    (0.25, 0.75),
    (0.5, 0.5),
];
const TEST_SCANCODE: u16 = 0x76;

#[derive(Default)]
struct Options {
    connect: Option<String>,
    listen: Option<String>,
    seconds: Option<u64>,
    width: Option<i32>,
    report: Option<PathBuf>,
    input_test: bool,
}

#[derive(Default)]
struct Stats {
    received: u32,
    lost: u32,
    decoded: u32,
    shown: u32,
    bytes: usize,
    decode_ms: Vec<f64>,
    /// Arrival of the frame -> on screen, per frame time.
    viewer_ms: HashMap<u64, f64>,
    /// Screen change -> sent, reported by the agent per frame time.
    agent_ms: HashMap<u64, f64>,
    rtt_ms: Vec<f64>,
    inputs_sent: u32,
}

fn main() {
    let banner = queqiao_core::banner(env!("CARGO_PKG_NAME"));
    let Some(options) = parse(std::env::args().skip(1)) else {
        println!("{banner}");
        println!(
            "usage: queqiao-viewer --listen <ip:port> | --connect <agent-ip:port> [--seconds N] [--width N] [--report <file>] [--input-test]"
        );
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
            "--seconds" => options.seconds = args.next().and_then(|v| v.parse().ok()),
            "--width" => options.width = args.next().and_then(|v| v.parse().ok()),
            "--report" => options.report = args.next().map(PathBuf::from),
            "--input-test" => options.input_test = true,
            _ => return None,
        }
    }
    (options.connect.is_some() || options.listen.is_some()).then_some(options)
}

fn run(options: &Options, lines: &mut Vec<String>) -> Result<bool, String> {
    lines.extend(queqiao_core::platform::prepare_process());
    let (device, context, gpu) = create_device()?;
    lines.push(format!("device: {gpu}"));

    let mut signal = match (&options.connect, &options.listen) {
        (Some(address), _) => Signal::connect(address, Duration::from_secs(60)),
        (None, Some(address)) => Signal::listen(address, Duration::from_secs(120)),
        (None, None) => unreachable!("parse() requires one of them"),
    }
    .map_err(|e| e.to_string())?;
    signal.hello("viewer").map_err(|e| e.to_string())?;
    let agent = signal.peer_address().map_err(|e| e.to_string())?;
    lines.push(format!("agent at {agent}"));

    let width = options.width.unwrap_or(1280);
    let mut window = Window::new(&format!("Queqiao - {agent}"), width, width * 9 / 16)
        .map_err(|e| format!("window: {}", describe(e)))?;
    let mut presenter = Presenter::new(&device, &context, &window)
        .map_err(|e| format!("presenter: {}", describe(e)))?;
    let mut decoder = H264Decoder::new(&device, &context, DecoderChoice::from_env())
        .map_err(|e| format!("decoder: {}", describe(e)))?;
    lines.push(format!("decoder: {}", decoder.name()));

    let started = Instant::now();
    let peer = Peer::start(Role::Viewer, &mut signal).map_err(|e| e.to_string())?;
    let end = options.seconds.map(|s| started + Duration::from_secs(s));
    let mut stats = Stats::default();
    let (mut connected_after, mut first_picture_after) = (None, None);
    let mut channel_open = false;
    let mut picture_size = (0u32, 0u32);
    let mut held_keys: HashSet<(u16, bool)> = HashSet::new();
    let mut next_ping = Instant::now();
    let mut ping_id = 0u32;
    let mut test_step = 0usize;
    let mut next_test_step: Option<Instant> = None;
    let mut decoded: Vec<DecodedFrame> = Vec::new();
    let mut closed = None;

    'session: loop {
        if end.is_some_and(|end| Instant::now() > end) {
            break;
        }
        if !window.pump() {
            closed = Some("the window was closed".to_string());
            break;
        }
        for event in window.take_events() {
            if options.input_test {
                continue;
            }
            for message in translate(event, picture_size, window.client_size(), &mut held_keys) {
                peer.send_data(message.encode());
                stats.inputs_sent += 1;
            }
        }
        if options.input_test
            && channel_open
            && first_picture_after.is_some()
            && let Some(due) =
                next_test_step.or_else(|| Some(Instant::now() + Duration::from_secs(1)))
        {
            next_test_step = Some(due);
            if Instant::now() >= due && test_step < TEST_POINTS.len() + 2 {
                let event = match test_step {
                    i if i < TEST_POINTS.len() => {
                        let (x, y) = TEST_POINTS[i];
                        InputEvent::MouseMove {
                            x: (x * 65535.0) as u16,
                            y: (y * 65535.0) as u16,
                        }
                    }
                    i => InputEvent::Key {
                        scancode: TEST_SCANCODE,
                        extended: false,
                        down: i == TEST_POINTS.len(),
                    },
                };
                peer.send_data(Message::Input(event).encode());
                stats.inputs_sent += 1;
                test_step += 1;
                next_test_step = Some(due + Duration::from_millis(300));
            }
        }
        if channel_open && Instant::now() >= next_ping {
            let sent_us = started.elapsed().as_micros() as u64;
            peer.send_data(
                Message::Ping {
                    id: ping_id,
                    sent_us,
                }
                .encode(),
            );
            ping_id += 1;
            next_ping = Instant::now() + Duration::from_millis(500);
        }

        let mut event = peer.wait_event(Duration::from_millis(2));
        while let Some(current) = event.take() {
            match current {
                PeerEvent::Connected => connected_after = Some(started.elapsed()),
                PeerEvent::ChannelOpen => channel_open = true,
                PeerEvent::Video {
                    data,
                    time_90khz,
                    contiguous,
                    received,
                } => {
                    stats.received += 1;
                    stats.lost += u32::from(!contiguous);
                    stats.bytes += data.len();
                    let decode_start = Instant::now();
                    decoder
                        .decode(&data, time_90khz as i64, &mut decoded)
                        .map_err(|e| format!("decode: {}", describe(e)))?;
                    stats
                        .decode_ms
                        .push(decode_start.elapsed().as_secs_f64() * 1e3);
                    for picture in decoded.drain(..) {
                        stats.decoded += 1;
                        picture_size = (picture.width, picture.height);
                        presenter
                            .show(&picture.texture, picture_size, &window)
                            .map_err(|e| format!("show: {}", describe(e)))?;
                        stats.shown += 1;
                        first_picture_after.get_or_insert(started.elapsed());
                        let viewer_ms = received.elapsed().as_secs_f64() * 1e3;
                        stats.viewer_ms.insert(picture.time as u64, viewer_ms);
                    }
                }
                PeerEvent::Data(bytes) => match Message::decode(&bytes) {
                    Some(Message::Pong { sent_us, .. }) => {
                        let now_us = started.elapsed().as_micros() as u64;
                        stats
                            .rtt_ms
                            .push(now_us.saturating_sub(sent_us) as f64 / 1e3);
                    }
                    Some(Message::FrameInfo {
                        time_90khz,
                        agent_ms,
                    }) => {
                        stats.agent_ms.insert(time_90khz, f64::from(agent_ms));
                    }
                    _ => {}
                },
                PeerEvent::KeyframeRequested => {}
                PeerEvent::Closed(reason) => {
                    closed = Some(reason);
                    break 'session;
                }
            }
            event = peer.try_event();
        }
    }
    // Keys still held would stay down on the remote PC.
    for (scancode, extended) in held_keys.drain() {
        let up = InputEvent::Key {
            scancode,
            extended,
            down: false,
        };
        peer.send_data(Message::Input(up).encode());
    }
    std::thread::sleep(Duration::from_millis(100));
    drop(peer);

    let elapsed = started.elapsed().as_secs_f64();
    let after =
        |d: Option<Duration>| d.map_or("never".to_string(), |d| format!("{} ms", d.as_millis()));
    lines.push(format!("present: {}", presenter.description()));
    lines.push(format!(
        "connected after {}, first picture after {}",
        after(connected_after),
        after(first_picture_after)
    ));
    lines.push(format!(
        "{elapsed:.1} s: {} frames received ({} after a loss, {:.2} MB), {} decoded, {} shown",
        stats.received,
        stats.lost,
        stats.bytes as f64 / 1e6,
        stats.decoded,
        stats.shown
    ));
    lines.push(format!("decode, ms: {}", summary(&mut stats.decode_ms)));
    lines.push(format!(
        "network round trip, ms: {}",
        summary(&mut stats.rtt_ms.clone())
    ));
    let half_rtt = median(&mut stats.rtt_ms) / 2.0;
    let mut viewer: Vec<f64> = stats.viewer_ms.values().copied().collect();
    lines.push(format!("arrival -> shown, ms: {}", summary(&mut viewer)));
    let mut agent: Vec<f64> = stats.agent_ms.values().copied().collect();
    lines.push(format!(
        "agent: screen change -> sent, ms: {}",
        summary(&mut agent)
    ));
    let mut total: Vec<f64> = stats
        .viewer_ms
        .iter()
        .filter_map(|(time, viewer_ms)| {
            stats
                .agent_ms
                .get(time)
                .map(|agent_ms| agent_ms + half_rtt + viewer_ms)
        })
        .collect();
    lines.push(format!(
        "screen change -> shown (agent + half round trip + viewer), ms: {}",
        summary(&mut total)
    ));
    lines.push(format!("input events sent: {}", stats.inputs_sent));
    lines.push(format!(
        "ended: {}",
        closed.unwrap_or_else(|| "time is up".into())
    ));
    let test_done = !options.input_test || test_step == TEST_POINTS.len() + 2;
    Ok(connected_after.is_some()
        && first_picture_after.is_some()
        && stats.shown == stats.decoded
        && test_done)
}

/// Window input -> protocol messages. Pointer positions are mapped from the picture's place in the window
/// to the remote screen (0..=65535); keys are tracked so they can be released when focus is lost.
fn translate(
    event: WindowEvent,
    picture: (u32, u32),
    window: (u32, u32),
    held: &mut HashSet<(u16, bool)>,
) -> Vec<Message> {
    let input = match event {
        WindowEvent::MouseMove { x, y } => {
            if picture.0 == 0 || picture.1 == 0 {
                return Vec::new();
            }
            let area = fit(picture, window);
            let scale = |value: i32, start: i32, end: i32| {
                let span = i64::from((end - start - 1).max(1));
                (i64::from(value - start) * 65535 / span).clamp(0, 65535) as u16
            };
            InputEvent::MouseMove {
                x: scale(x, area.left, area.right),
                y: scale(y, area.top, area.bottom),
            }
        }
        WindowEvent::MouseButton { button, down } => {
            let button = match button {
                0 => MouseButton::Left,
                1 => MouseButton::Right,
                2 => MouseButton::Middle,
                3 => MouseButton::X1,
                _ => MouseButton::X2,
            };
            InputEvent::MouseButton { button, down }
        }
        WindowEvent::Wheel { delta, horizontal } => InputEvent::Wheel { delta, horizontal },
        WindowEvent::Key {
            scancode,
            extended,
            down,
        } => {
            if down {
                held.insert((scancode, extended));
            } else {
                held.remove(&(scancode, extended));
            }
            InputEvent::Key {
                scancode,
                extended,
                down,
            }
        }
        WindowEvent::FocusLost => {
            return held
                .drain()
                .map(|(scancode, extended)| {
                    Message::Input(InputEvent::Key {
                        scancode,
                        extended,
                        down: false,
                    })
                })
                .collect();
        }
    };
    vec![Message::Input(input)]
}

/// The GPU's D3D11 device with video support; Microsoft's software adapter (WARP) without a usable GPU
/// or when forced (QUEQIAO_FORCE=adapter:warp).
fn create_device() -> Result<(ID3D11Device, ID3D11DeviceContext, String), String> {
    let forced_warp = queqiao_core::forced("adapter").as_deref() == Some("warp");
    let video = D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT;
    let attempts: &[(D3D_DRIVER_TYPE, D3D11_CREATE_DEVICE_FLAG)] = if forced_warp {
        &[(D3D_DRIVER_TYPE_WARP, D3D11_CREATE_DEVICE_BGRA_SUPPORT)]
    } else {
        &[
            (D3D_DRIVER_TYPE_HARDWARE, video),
            (D3D_DRIVER_TYPE_WARP, D3D11_CREATE_DEVICE_BGRA_SUPPORT),
        ]
    };
    let mut failures = Vec::new();
    for &(driver, flags) in attempts {
        let mut device = None;
        let mut context = None;
        let created = unsafe {
            D3D11CreateDevice(
                None::<&IDXGIAdapter>,
                driver,
                HMODULE::default(),
                flags,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )
        };
        match (created, device, context) {
            (Ok(()), Some(device), Some(context)) => {
                let name = adapter_name(&device);
                return Ok((device, context, name));
            }
            (Err(error), _, _) => failures.push(describe(error)),
            _ => failures.push("nothing returned".into()),
        }
    }
    Err(format!("no D3D11 device: {}", failures.join("; ")))
}

fn adapter_name(device: &ID3D11Device) -> String {
    device
        .cast::<IDXGIDevice>()
        .and_then(|d| unsafe { d.GetAdapter() })
        .and_then(|a| unsafe { a.GetDesc() })
        .map(|desc| {
            let text = &desc.Description;
            let len = text.iter().position(|&c| c == 0).unwrap_or(text.len());
            String::from_utf16_lossy(&text[..len])
        })
        .unwrap_or_else(|_| "unknown".into())
}

fn median(values: &mut [f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(f64::total_cmp);
    values[values.len() / 2]
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
