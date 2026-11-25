//! A stand-in PC for testing the phone apps where no Queqiao agent runs (the iOS Simulator on a Mac runner): it logs in
//! to the account as an agent, takes one session from a device of the account, streams a moving test picture
//! (OpenH264, 640x360, 30 fps), answers round-trip pings, and counts the input and the typed text it receives. Not a
//! real agent: no screen captured, nothing injected.
//! Usage: test_agent <folder> <server> <pin> <user> <password> <device name> <seconds> <report file>
//! The report's lines: "session: yes|no", "frames sent: N", "input events: N", "typed: <text>", "ended: <why>".

use std::fmt::Write as _;
use std::time::{Duration, Instant};

use openh264::OpenH264API;
use openh264::encoder::{BitRate, Encoder, EncoderConfig, FrameRate, RateControlMode, UsageType};
use openh264::formats::YUVSlices;
use queqiao_account::{Account, AccountEvent, DeviceIdentity, SavedLogin};
use queqiao_core::account::Role as AccountRole;
use queqiao_core::protocol::{Message, VideoCodec};
use queqiao_transport::{Addresses, Peer, PeerEvent, Role, Signaling, check_hello, hello_body};

const WIDTH: usize = 640;
const HEIGHT: usize = 360;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 9 {
        println!(
            "usage: test_agent <folder> <server> <pin> <user> <password> <device name> <seconds> <report>"
        );
        return;
    }
    // SAFETY: set before anything else runs (the account crate keeps its files there).
    unsafe { std::env::set_var("XDG_CONFIG_HOME", &args[1]) };
    let seconds: u64 = args[7].parse().unwrap_or(60);
    let mut report = String::new();
    let result = run(&args, seconds, &mut report);
    if let Err(error) = result {
        let _ = writeln!(report, "error: {error}");
    }
    let _ = std::fs::write(&args[8], &report);
    print!("{report}");
}

fn run(args: &[String], seconds: u64, report: &mut String) -> Result<(), String> {
    let mut device = DeviceIdentity::load_or_create().map_err(|e| e.to_string())?;
    device.name = args[6].clone();
    let mut login = SavedLogin {
        server: args[2].clone(),
        pin: args[3].clone(),
        refresh_token: None,
    };
    let mut account = Account::connect(
        &mut login,
        &device,
        AccountRole::Agent,
        Some((&args[4], &args[5])),
    )
    .map_err(|e| e.to_string())?;
    println!("online as {}", device.name);
    let deadline = Instant::now() + Duration::from_secs(seconds);
    // One session request (a hello from a device of the account).
    let (from, id, body) = loop {
        if Instant::now() > deadline {
            let _ = writeln!(report, "session: no");
            return Ok(());
        }
        match account.next_event(Duration::from_millis(200)) {
            Some(AccountEvent::Signal {
                from,
                from_role: AccountRole::Viewer,
                session,
                kind,
                body,
                signature,
                ..
            }) if kind == "hello" => {
                account
                    .check_signal(
                        &from,
                        AccountRole::Viewer,
                        &session,
                        &kind,
                        &body,
                        &signature,
                    )
                    .map_err(|e| format!("refused the session: {e}"))?;
                break (from, session, body);
            }
            Some(AccountEvent::Closed(reason)) => {
                return Err(format!("the server connection closed: {reason}"));
            }
            _ => {}
        }
    };
    check_hello(&body, "agent").map_err(|e| e.to_string())?;
    let peer = {
        let mut signal = account.session(&from, AccountRole::Viewer, &id);
        signal
            .send("hello", &hello_body("agent"))
            .map_err(|e| e.to_string())?;
        Peer::start_with(Role::Agent, &mut signal, Addresses::All).map_err(|e| e.to_string())?
    };
    let _ = writeln!(report, "session: yes");
    let config = EncoderConfig::new()
        .usage_type(UsageType::ScreenContentRealTime)
        .rate_control_mode(RateControlMode::Bitrate)
        .bitrate(BitRate::from_bps(1_500_000))
        .max_frame_rate(FrameRate::from_hz(30.0));
    let mut encoder = Encoder::with_api_config(OpenH264API::from_source(), config)
        .map_err(|e| format!("OpenH264: {e}"))?;
    let (mut y, u, v) = (
        vec![0u8; WIDTH * HEIGHT],
        vec![90u8; WIDTH * HEIGHT / 4],
        vec![160u8; WIDTH * HEIGHT / 4],
    );
    let started = Instant::now();
    let (mut frames, mut inputs, mut typed, mut open) = (0u64, 0u64, String::new(), false);
    let mut keyframe = true;
    let mut next_frame = Instant::now();
    let ended = 'session: loop {
        if Instant::now() > deadline {
            peer.send_data(Message::Leave.encode());
            break "time is up".to_string();
        }
        while let Some(event) = peer.try_event() {
            match event {
                PeerEvent::ChannelOpen => open = true,
                PeerEvent::KeyframeRequested => keyframe = true,
                PeerEvent::Closed(reason) => break 'session reason,
                PeerEvent::Data(bytes) => match Message::decode(&bytes) {
                    Some(Message::Input(_)) => inputs += 1,
                    Some(Message::TypeText(text)) => {
                        inputs += 1;
                        typed.push_str(&text);
                    }
                    Some(Message::Ping { id, sent_us }) => {
                        peer.send_data(Message::Pong { id, sent_us }.encode())
                    }
                    Some(Message::Leave) => break 'session "the viewer left".to_string(),
                    _ => {}
                },
                _ => {}
            }
        }
        if open && Instant::now() >= next_frame {
            next_frame += Duration::from_millis(33);
            // A bright bar moving across a dark gradient, and a block that blinks each second.
            let bar = (frames as usize * 8) % WIDTH;
            for row in 0..HEIGHT {
                for column in 0..WIDTH {
                    let mut value = (column * 120 / WIDTH + row * 60 / HEIGHT) as u8 + 16;
                    if column.abs_diff(bar) < 24 {
                        value = 235;
                    }
                    if (frames / 30) % 2 == 0 && row < 60 && column < 60 {
                        value = 200;
                    }
                    y[row * WIDTH + column] = value;
                }
            }
            if keyframe {
                encoder.force_intra_frame();
                keyframe = false;
            }
            let source =
                YUVSlices::new((&y, &u, &v), (WIDTH, HEIGHT), (WIDTH, WIDTH / 2, WIDTH / 2));
            let data = encoder
                .encode(&source)
                .map_err(|e| format!("OpenH264: {e}"))?
                .to_vec();
            if !data.is_empty() {
                let time_90khz = (started.elapsed().as_micros() as u64) * 9 / 100;
                peer.send_video(data, time_90khz, VideoCodec::H264);
                frames += 1;
            }
        }
        std::thread::sleep(Duration::from_millis(2));
    };
    let _ = writeln!(report, "frames sent: {frames}");
    let _ = writeln!(report, "input events: {inputs}");
    let _ = writeln!(report, "typed: {}", typed.escape_debug());
    let _ = writeln!(report, "ended: {ended}");
    std::thread::sleep(Duration::from_millis(200));
    drop(peer);
    Ok(())
}
