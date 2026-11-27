//! The Windows program (docs/LINUX.md: the Linux one is written beside it).
//!
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

use crate::{autostart, lock_gate, service_link, ui, wol_check};

use std::path::PathBuf;
use std::time::{Duration, Instant};

use queqiao_capture::{BgraImage, Duplicator};
use queqiao_transport::{Signal, Signaling};
use service_link::Links;

use crate::options::*;
use crate::session::*;
use windows::Win32::Foundation::RECT;

use windows::Win32::Graphics::Direct3D11::*;

use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};

pub(crate) fn main() {
    queqiao_core::platform::install_crash_log("queqiao-agent");
    // Tests: a crash on purpose, to see the report written.
    #[cfg(feature = "test-switches")]
    if std::env::args().any(|arg| arg == "--crash-test") {
        panic!("crash test (asked for with --crash-test)");
    }
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
            println!(
                "usage: queqiao-agent (started at sign-in, or by the Queqiao service; it takes no options)"
            );
        }
        // --version is answered by the banner; any other option is refused.
        let asked_version = std::env::args().nth(1).as_deref() == Some("--version");
        std::process::exit(if asked_version { 0 } else { 2 });
    };
    let mut lines = vec![banner];
    let mut not_logged_in = false;
    let mut already_running = false;
    let passed = match run(&options, &mut lines) {
        Ok(passed) => passed,
        Err(error) => {
            not_logged_in = error.starts_with("not logged in");
            already_running = error == autostart::ALREADY_RUNNING;
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
    // Started by the service: 3 tells it there is nothing to do until the next sign-in. 4: another agent runs in this
    // session (the installer does not take that for a failed start).
    let code = if passed {
        0
    } else if not_logged_in && options.service_channel.is_some() {
        3
    } else if already_running {
        4
    } else {
        1
    };
    std::process::exit(code);
}

fn run(options: &Options, lines: &mut Vec<String>) -> Result<bool, String> {
    lines.extend(queqiao_core::platform::prepare_process());
    // Windows' firewall asks once whether the agent may take connections (allowed, sessions reach this PC directly
    // from behind any NAT). Shipping builds; test builds only when asked (QUEQIAO_FORCE=firewall:ask), so tests on the
    // developer's PC never put the dialog in front of its user.
    if !cfg!(feature = "test-switches")
        || queqiao_core::forced("firewall").as_deref() == Some("ask")
    {
        queqiao_transport::firewall::let_windows_ask();
    }
    if options.wol_check {
        let check = wol_check::check();
        lines.extend(wol_check::report(&check));
        lines.push("--- the advice shown in the tray:".into());
        lines.extend(wol_check::advice(&check).lines().map(str::to_string));
        return Ok(true);
    }
    lines.push(format!(
        "integrity: {}{}",
        queqiao_core::platform::own_integrity_level()
            .map_or("unknown", queqiao_core::platform::integrity_name),
        if queqiao_core::platform::runs_raised() {
            " (raised by the service: what it starts runs at Medium)"
        } else {
            ""
        }
    ));
    if options.test_start_program {
        let cmd =
            PathBuf::from(std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into()))
                .join(r"System32\cmd.exe");
        lines.push(
            match queqiao_core::platform::start_program(&cmd, "/d /c exit 0") {
                Ok((_, level)) => format!(
                    "test: a program it starts runs at {} integrity",
                    level.map_or("unknown", queqiao_core::platform::integrity_name)
                ),
                Err(error) => format!("test: a program did not start: {error}"),
            },
        );
    }
    let mut links = Links::open(
        options.service_channel.as_deref(),
        options.desktop_channel.as_deref(),
    )?;
    if links.service.is_some() {
        lines.push(
            "started by the Queqiao service: the sign-in, lock, Ctrl+Alt+Del and UAC screens come through its desktop helper"
                .into(),
        );
    }
    let handed = if options.logon {
        // The service starts this agent as LocalService, which no program of a person can do: started otherwise,
        // a program could hand it a login of its own over pipes it made and have it run unseen (no tray icon, no
        // banner), as found by the security review of 2026-11-22.
        if !queqiao_core::platform::runs_as_local_service() {
            return Err(
                "the agent for the sign-in screen runs as LocalService only (the Queqiao service starts it)"
                    .into(),
            );
        }
        let service = links
            .service
            .as_mut()
            .ok_or("--logon is for the Queqiao service only")?;
        let identity = service.identity(Duration::from_secs(10))?;
        // Nobody is signed in: a viewer gives the Windows password of the one who set this up (as UU).
        lock_gate::set_sign_in_owner(&identity.owner_account);
        lines.push(format!(
            "the agent for the sign-in screen, as {} ({})",
            identity.device_name, identity.device_id
        ));
        lines.push(format!(
            "privileges: {}",
            queqiao_core::platform::own_privileges().join(", ")
        ));
        Some(identity)
    } else {
        None
    };
    // Started without options (at sign-in, or by hand): one agent per user session.
    if !options.logon
        && options.server.is_none()
        && options.connect.is_none()
        && options.listen.is_none()
    {
        // With the Queqiao service running, Queqiao for the person signed in is the one it starts (the Run key's
        // copy, started at sign-in as well, leaves).
        if options.service_channel.is_none() && autostart::service_runs() {
            if options.show {
                queqiao_display::show_running_main();
            }
            lines.push("the Queqiao service runs Queqiao on this PC: this copy leaves".into());
            return Ok(true);
        }
        // Not for the agent the service started: the service starts one per session, and a program of the person
        // that took the name first would otherwise keep it from running (the security review of 2026-11-22).
        if options.service_channel.is_none()
            && let Err(error) = autostart::claim_single_instance()
        {
            // Opened again (the Start menu): the Queqiao that runs already shows its window.
            if options.show && queqiao_display::show_running_main() {
                lines.push("Queqiao runs already here: its main window is shown".into());
                return Ok(true);
            }
            return Err(error);
        }
    }
    let end = options
        .seconds
        .map(|s| Instant::now() + Duration::from_secs(s));
    // Nothing happens unseen: the tray icon whenever the agent runs, a banner during every session.
    // At the sign-in screen: only what the person allowed (sessions from the account's devices without asking),
    // nothing that needs someone signed in (clipboard, files, command lines).
    let settings = match &handed {
        Some(handed) => ui::Settings {
            allow_account_control: handed.allow_account_control,
            ask_before_session: false,
            share_clipboard: false,
            receive_files: false,
            allow_terminal: false,
            ..ui::Settings::default()
        },
        None => ui::Settings::load(),
    };
    let mut shown = settings.clone();
    shown.ask_before_session |= options.ask;
    let ui = if options.logon {
        ui::Ui::headless()?
    } else {
        ui::Ui::start(&shown, options.ui_pictures.clone())?
    };
    if options.show && !options.logon {
        ui.show_main(None);
    }
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
        return crate::account::run_account(options, end, &ui, settings, &mut links, handed, lines);
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
            controller: viewer.to_string(),
        },
        &mut links,
        &mut deferred,
        lines,
    )?;
    Ok(outcome.passed)
}

/// The privacy test's square: 320 x 200 pixels, 100 pixels in from the captured monitor's top left corner.
const PRIVACY_TEST_SIZE: (i32, i32) = (320, 200);

const PRIVACY_TEST_OFFSET: i32 = 100;

/// Magenta (0x00BBGGRR), rare on a desktop.
pub(crate) const PRIVACY_TEST_COLOR: u32 = 0x00FF_00FF;

pub(crate) fn privacy_test_area(left: i32, top: i32) -> RECT {
    RECT {
        left: left + PRIVACY_TEST_OFFSET,
        top: top + PRIVACY_TEST_OFFSET,
        right: left + PRIVACY_TEST_OFFSET + PRIVACY_TEST_SIZE.0,
        bottom: top + PRIVACY_TEST_OFFSET + PRIVACY_TEST_SIZE.1,
    }
}

/// Pixels of the test colour inside the test square of a captured picture (monitor coordinates).
fn privacy_test_pixels(image: &BgraImage) -> usize {
    let (width, height) = (image.width as usize, image.height as usize);
    let start = PRIVACY_TEST_OFFSET as usize;
    let (right, bottom) = (
        (start + PRIVACY_TEST_SIZE.0 as usize).min(width),
        (start + PRIVACY_TEST_SIZE.1 as usize).min(height),
    );
    (start..bottom)
        .flat_map(|y| (start..right).map(move |x| (y * width + x) * 4))
        .filter(|&at| image.pixels[at..at + 3] == [0xFF, 0x00, 0xFF])
        .count()
}

/// The privacy test (the square is up): capture must not see it; with the exclusion turned off it must, so the
/// check itself is shown to work. Through GDI (a screenshot now) and desktop duplication (frames as they come).
pub(crate) fn privacy_check(duplicator: &mut Duplicator, ui: &ui::Ui) -> String {
    let look = |duplicator: &mut Duplicator| {
        std::thread::sleep(Duration::from_millis(400));
        let snapshot = duplicator.snapshot_image().ok();
        let gdi = snapshot.as_ref().map_or(usize::MAX, privacy_test_pixels);
        let other = snapshot.as_ref().map(privacy_other_pixels);
        let (mut frames, mut most) = (0, 0);
        let until = Instant::now() + Duration::from_millis(800);
        while Instant::now() < until {
            if let Ok(Some(frame)) = duplicator.acquire(50)
                && let Ok(image) = duplicator.copy_to_cpu(&frame)
            {
                frames += 1;
                most = most.max(privacy_test_pixels(&image));
            }
        }
        (gdi, frames, most, other)
    };
    let hidden = look(duplicator);
    let shown = match ui.set_privacy_excluded(false) {
        Ok(()) => look(duplicator),
        Err(error) => return format!("privacy test: FAIL (turning the exclusion off: {error})"),
    };
    let _ = ui.set_privacy_excluded(true);
    let square = (PRIVACY_TEST_SIZE.0 * PRIVACY_TEST_SIZE.1) as usize;
    // All but a few pixels (the square is plain; a pointer or a window above it may cover a little).
    let seen = |pixels: usize| pixels * 100 >= square * 99;
    let passed = hidden.0 == 0 && hidden.2 == 0 && seen(shown.0) && (shown.1 == 0 || seen(shown.2));
    format!(
        "privacy test: {} (test colour pixels of {square}: hidden from capture: GDI {}, duplication {} in {} frames; \
         not hidden: GDI {}, duplication {} in {} frames; other colours there: {})",
        if passed { "PASS" } else { "FAIL" },
        hidden.0,
        hidden.2,
        hidden.1,
        shown.0,
        shown.2,
        shown.1,
        shown.3.flatten().unwrap_or_else(|| "none".into())
    )
}

/// Where in the test square the picture is not the test colour, and what is there instead (the first such
/// pixel): "x a..b, y c..d, first [B, G, R] at (x, y)" in square coordinates.
fn privacy_other_pixels(image: &BgraImage) -> Option<String> {
    let width = image.width as usize;
    let start = PRIVACY_TEST_OFFSET as usize;
    let (mut x_range, mut y_range) = ((usize::MAX, 0), (usize::MAX, 0));
    let mut first: Option<([u8; 3], usize, usize)> = None;
    for y in 0..PRIVACY_TEST_SIZE.1 as usize {
        for x in 0..PRIVACY_TEST_SIZE.0 as usize {
            let at = ((start + y) * width + start + x) * 4;
            let Some(pixel) = image.pixels.get(at..at + 3) else {
                continue;
            };
            if pixel != [0xFF, 0x00, 0xFF] {
                x_range = (x_range.0.min(x), x_range.1.max(x));
                y_range = (y_range.0.min(y), y_range.1.max(y));
                first.get_or_insert(([pixel[0], pixel[1], pixel[2]], x, y));
            }
        }
    }
    first.map(|(colour, x, y)| {
        format!(
            "x {}..{}, y {}..{}, first {colour:?} at ({x}, {y})",
            x_range.0, x_range.1, y_range.0, y_range.1
        )
    })
}

pub(crate) fn upload(device: &ID3D11Device, image: &BgraImage) -> Result<ID3D11Texture2D, String> {
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

pub(crate) fn describe(error: windows::core::Error) -> String {
    format!("0x{:08X} {}", error.code().0, error.message())
}

/// The viewer beside this program, or the person's own installation's (the service's agent runs from Program Files,
/// where the viewer is not).
fn viewer_program() -> Option<PathBuf> {
    let here = std::env::current_exe()
        .ok()?
        .with_file_name("queqiao-viewer.exe");
    if here.is_file() {
        return Some(here);
    }
    // The person's own installation, wherever they chose to put it.
    let theirs = queqiao_core::platform::installation_folder().join("queqiao-viewer.exe");
    theirs.is_file().then_some(theirs)
}

/// Starts the viewer for the main window (a session to a device, or helping someone).
pub(crate) fn open_viewer(arguments: &[&str], lines: &mut Vec<String>) {
    let Some(program) = viewer_program() else {
        queqiao_display::show_error(
            queqiao_core::text::tr("Queqiao"),
            queqiao_core::text::tr("The Queqiao viewer is not installed for you on this PC."),
        );
        return;
    };
    let arguments: Vec<String> = arguments
        .iter()
        .map(|argument| queqiao_core::platform::quote(argument))
        .collect();
    match queqiao_core::platform::start_program(&program, &arguments.join(" ")) {
        Ok((id, level)) => lines.push(format!(
            "main window: the viewer started ({id}, {} integrity)",
            level.map_or("unknown", queqiao_core::platform::integrity_name)
        )),
        Err(error) => lines.push(format!("main window: the viewer did not start: {error}")),
    }
}

/// Opens a folder in Explorer (the received files).
pub(crate) fn open_folder(folder: &std::path::Path) {
    let explorer =
        PathBuf::from(std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into()))
            .join("explorer.exe");
    let _ = queqiao_core::platform::start_program(
        &explorer,
        &queqiao_core::platform::quote(&folder.to_string_lossy()),
    );
}

/// The owner of a message box shown without a window of its own.
pub(crate) fn no_owner() -> windows::Win32::Foundation::HWND {
    windows::Win32::Foundation::HWND::default()
}

/// The picture to encode next (docs/LINUX.md): a texture on the screen's GPU here, an image in memory on Linux.
pub(crate) type Latest = ID3D11Texture2D;

/// Copies a captured frame into `latest`, which outlives the frame.
pub(crate) fn keep_frame(
    context: &ID3D11DeviceContext,
    latest: &mut Latest,
    frame: &queqiao_capture::Frame,
) {
    unsafe { context.CopyResource(&*latest, &frame.texture) };
}
