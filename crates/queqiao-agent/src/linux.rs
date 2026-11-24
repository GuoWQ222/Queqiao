//! The Linux program (docs/LINUX.md step 3): the agent by IP address (M2, `--connect` / `--listen` in a test build), with
//! the session shared with Windows (session.rs). Accounts, the service, the tray and the GUI come in the next steps;
//! started otherwise it says so. Also what the session needs from this system: the picture to encode (in memory), the
//! error text, the privacy test (not here yet).

use std::time::{Duration, Instant};

use queqiao_capture::{BgraImage, Context, Device, Duplicator, Frame};
use queqiao_transport::{Signal, Signaling};

use crate::options::{Options, parse};
use crate::service_link::Links;
use crate::session::{Permissions, session};
use crate::ui;

/// The picture to encode next: an image in memory (Windows: a texture on the screen's GPU).
pub(crate) type Latest = BgraImage;

pub(crate) fn upload(_device: &Device, image: &BgraImage) -> Result<Latest, String> {
    Ok(BgraImage {
        width: image.width,
        height: image.height,
        pixels: image.pixels.clone(),
    })
}

/// Copies a captured frame into `latest`, which outlives the frame (a frame is valid until the next capture).
pub(crate) fn keep_frame(_context: &Context, latest: &mut Latest, frame: &Frame) {
    let row = frame.width as usize * 4;
    latest.width = frame.width;
    latest.height = frame.height;
    latest.pixels.resize(row * frame.height as usize, 0);
    for (target, source) in latest
        .pixels
        .chunks_exact_mut(row)
        .zip(frame.pixels().chunks(frame.stride))
    {
        target.copy_from_slice(&source[..row]);
    }
}

pub(crate) fn describe(error: impl std::fmt::Display) -> String {
    error.to_string()
}

/// The privacy test's colour (magenta, 0x00BBGGRR) and square, as on Windows.
pub(crate) const PRIVACY_TEST_COLOR: u32 = 0x00FF_00FF;

pub(crate) fn privacy_test_area(left: i32, top: i32) -> ui::Area {
    (left + 100, top + 100, left + 420, top + 300)
}

pub(crate) fn privacy_check(_duplicator: &mut Duplicator, _ui: &ui::Ui) -> String {
    "privacy test: not on Linux yet (no privacy screen)".into()
}

pub(crate) fn main() {
    queqiao_core::platform::install_crash_log("queqiao-agent");
    let banner = queqiao_core::banner(env!("CARGO_PKG_NAME"));
    let Some(options) = parse(std::env::args().skip(1)) else {
        println!("{banner}");
        if cfg!(feature = "test-switches") {
            println!("usage: queqiao-agent --connect <viewer-ip:port> | --listen <ip:port>");
            println!("       common: [--seconds N] [--fps N] [--report <file>] [--verify-input]");
        } else {
            println!(
                "usage: queqiao-agent (on Linux, by IP address in a test build so far: docs/LINUX.md)"
            );
        }
        let asked_version = std::env::args().nth(1).as_deref() == Some("--version");
        std::process::exit(if asked_version { 0 } else { 2 });
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
            let _ = std::fs::write(path, lines.join("\n") + "\n");
        }
        None => lines.iter().for_each(|line| println!("{line}")),
    }
    std::process::exit(if passed { 0 } else { 1 });
}

fn run(options: &Options, lines: &mut Vec<String>) -> Result<bool, String> {
    lines.extend(queqiao_core::platform::prepare_process());
    // What needs the account, the tray or Windows: refused by name rather than left unheeded.
    let unsupported = [
        ("--server", options.server.is_some()),
        ("--pin", options.pin.is_some()),
        ("--user", options.user.is_some()),
        ("--password", options.password.is_some()),
        ("--sessions", options.sessions.is_some()),
        ("--trust-new-key", options.trust_new_key.is_some()),
        ("--test-device", options.test_device.is_some()),
        ("--test-tray-login", options.test_tray_login),
        ("--test-main-window", options.test_main_window),
        ("--test-press-setting", options.test_press_setting),
        ("--test-start-program", options.test_start_program),
        ("--ask", options.ask),
        ("--answer", options.answer.is_some()),
        ("--help-code", options.help_code.is_some()),
        ("--help-id-file", options.help_id_file.is_some()),
        ("--fixed-help-code", options.fixed_help_code.is_some()),
        ("--help-mode", options.help_mode.is_some()),
        ("--wol-check", options.wol_check),
        ("--lock-after-session", options.lock_after_session),
    ];
    let refused: Vec<&str> = unsupported
        .iter()
        .filter(|(_, given)| *given)
        .map(|(name, _)| *name)
        .collect();
    if !refused.is_empty() {
        return Err(format!(
            "not on Linux yet (accounts, the tray and help requests come next, docs/LINUX.md): {}",
            refused.join(" ")
        ));
    }
    if options.connect.is_none() && options.listen.is_none() {
        return Err(
            "on Linux the agent runs by IP address so far (--connect or --listen, a test build); accounts, the \
             service and the tray come next (docs/LINUX.md)"
                .into(),
        );
    }
    let mut links = Links::open(
        options.service_channel.as_deref(),
        options.desktop_channel.as_deref(),
    )?;
    let end = options
        .seconds
        .map(|s| Instant::now() + Duration::from_secs(s));
    let settings = ui::Settings::load();
    let ui = ui::Ui::start(&settings, options.ui_pictures.clone())?;
    lines.push(format!(
        "tray icon {}; a banner shows each session; clipboard {}, files {}, command lines {}",
        if ui.tray_shown {
            "shown"
        } else {
            "none on Linux yet"
        },
        if settings.share_clipboard {
            "shared"
        } else {
            "not shared"
        },
        if settings.receive_files {
            "accepted"
        } else {
            "refused"
        },
        if settings.allow_terminal {
            "allowed"
        } else {
            "refused"
        },
    ));
    let mut signal = match (&options.connect, &options.listen) {
        (Some(address), _) => Signal::connect(address, Duration::from_secs(60)),
        (None, Some(address)) => Signal::listen(address, Duration::from_secs(120)),
        (None, None) => unreachable!("checked above"),
    }
    .map_err(|e| e.to_string())?;
    let hello = signal.hello("agent").map_err(|e| e.to_string())?;
    let viewer = signal.peer_address().map_err(|e| e.to_string())?;
    let mut deferred = Vec::new();
    let outcome = session(
        options,
        &mut signal,
        &viewer.to_string(),
        "direct",
        end,
        &ui,
        Permissions {
            clipboard: settings.share_clipboard,
            files: settings.receive_files,
            terminal: settings.allow_terminal,
            helper: false,
            fec: hello.fec,
        },
        &mut links,
        &mut deferred,
        lines,
    )?;
    Ok(outcome.passed)
}
