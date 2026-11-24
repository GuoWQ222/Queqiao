//! The Linux program (docs/LINUX.md steps 3 and 5): the agent with an account (account.rs, shared with Windows), or by
//! IP address (M2, `--connect` / `--listen` in a test build), with the session shared with Windows (session.rs). The
//! Queqiao service (its sign-in screen agent) comes in step 6. Also what the session and the account mode need from
//! this system: the picture to encode (in memory), the error text, the privacy test (not here yet), the file manager,
//! the viewer.

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use queqiao_capture::{BgraImage, Context, Device, Duplicator, Frame};
use queqiao_transport::{Signal, Signaling};

use crate::options::{Options, parse};
use crate::service_link::Links;
use crate::session::{Permissions, session};
use crate::{autostart, ui, wol_check};

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

pub(crate) fn privacy_test_area(left: i32, top: i32) -> queqiao_display::Rect {
    queqiao_display::Rect {
        left: left + 100,
        top: top + 100,
        right: left + 420,
        bottom: top + 300,
    }
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
            println!(
                "usage: queqiao-agent [--server <host:port> --pin <fingerprint> --user <name> --password <password>] [--sessions N]"
            );
            println!("       queqiao-agent --connect <viewer-ip:port> | --listen <ip:port>");
            println!("       common: [--seconds N] [--fps N] [--report <file>] [--verify-input]");
        } else {
            println!("usage: queqiao-agent (started at sign-in; it takes no options)");
        }
        let asked_version = std::env::args().nth(1).as_deref() == Some("--version");
        std::process::exit(if asked_version { 0 } else { 2 });
    };
    let mut lines = vec![banner];
    let mut already_running = false;
    let passed = match run(&options, &mut lines) {
        Ok(passed) => passed,
        Err(error) => {
            already_running = error == autostart::ALREADY_RUNNING;
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
    // 4: another agent runs for this user (as on Windows).
    std::process::exit(if passed {
        0
    } else if already_running {
        4
    } else {
        1
    });
}

fn run(options: &Options, lines: &mut Vec<String>) -> Result<bool, String> {
    lines.extend(queqiao_core::platform::prepare_process());
    if options.wol_check {
        let check = wol_check::check();
        lines.extend(wol_check::report(&check));
        lines.push("--- the advice shown in the tray:".into());
        lines.extend(wol_check::advice(&check).lines().map(str::to_string));
        return Ok(true);
    }
    // Windows' own (the service's sign-in screen agent, integrity levels): refused by name rather than left unheeded.
    let unsupported = [
        ("--logon", options.logon),
        ("--test-start-program", options.test_start_program),
    ];
    let refused: Vec<&str> = unsupported
        .iter()
        .filter(|(_, given)| *given)
        .map(|(name, _)| *name)
        .collect();
    if !refused.is_empty() {
        return Err(format!(
            "not on Linux (the Queqiao service comes with docs/LINUX.md step 6): {}",
            refused.join(" ")
        ));
    }
    let mut links = Links::open(
        options.service_channel.as_deref(),
        options.desktop_channel.as_deref(),
    )?;
    // Started without options (at sign-in, or by hand): one agent per user; opened again, it shows the running one.
    if options.server.is_none()
        && options.connect.is_none()
        && options.listen.is_none()
        && let Err(error) = autostart::claim_single_instance()
    {
        if options.show && queqiao_display::show_running_main() {
            lines.push("Queqiao runs already here: its main window is shown".into());
            return Ok(true);
        }
        return Err(error);
    }
    let end = options
        .seconds
        .map(|s| Instant::now() + Duration::from_secs(s));
    // Nothing happens unseen: the tray icon (or the main window) whenever the agent runs, a banner during every
    // session.
    let settings = ui::Settings::load();
    let mut shown = settings.clone();
    shown.ask_before_session |= options.ask;
    let ui = ui::Ui::start(&shown, options.ui_pictures.clone())?;
    if options.show {
        ui.show_main(None);
    }
    lines.push(format!(
        "tray icon {}; this account's devices may control this PC: {}; ask first: {}",
        if ui.tray_shown {
            "shown"
        } else {
            "not shown (no StatusNotifierItem tray on this desktop: the main window is open instead)"
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
    lines.push(format!(
        "clipboard {}, files {}, command lines {}",
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
    if options.connect.is_none() && options.listen.is_none() {
        return crate::account::run_account(options, end, &ui, settings, &mut links, None, lines);
    }
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

/// Opens a folder in the desktop's file manager (the received files).
pub(crate) fn open_folder(folder: &std::path::Path) {
    if let Ok(child) = Command::new("xdg-open")
        .arg(folder)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        reap(child);
    }
}

/// The owner of a message box shown without a window of its own.
pub(crate) fn no_owner() -> queqiao_display::Hwnd {
    queqiao_display::Hwnd(0)
}

/// The viewer beside this program, or the system's (a package puts both in /usr/bin).
fn viewer_program() -> Option<PathBuf> {
    let here = std::env::current_exe()
        .ok()?
        .with_file_name("queqiao-viewer");
    if here.is_file() {
        return Some(here);
    }
    ["/usr/bin/queqiao-viewer", "/usr/local/bin/queqiao-viewer"]
        .into_iter()
        .map(PathBuf::from)
        .find(|path| path.is_file())
}

/// Starts the viewer for the main window (a session to a device, or helping someone). The arguments go to it as they
/// are (no shell in between).
pub(crate) fn open_viewer(arguments: &[&str], lines: &mut Vec<String>) {
    let Some(program) = viewer_program() else {
        queqiao_display::show_error(
            queqiao_core::text::tr("Queqiao"),
            queqiao_core::text::tr("The Queqiao viewer is not installed for you on this PC."),
        );
        return;
    };
    match Command::new(&program)
        .args(arguments)
        .stdin(Stdio::null())
        .spawn()
    {
        Ok(child) => {
            lines.push(format!("main window: the viewer started ({})", child.id()));
            reap(child);
        }
        Err(error) => lines.push(format!("main window: the viewer did not start: {error}")),
    }
}

/// Waits for a started program on a thread of its own, so it leaves no zombie behind when it ends.
fn reap(mut child: Child) {
    let _ = std::thread::Builder::new()
        .name("queqiao-reap".into())
        .spawn(move || {
            let _ = child.wait();
        });
}
