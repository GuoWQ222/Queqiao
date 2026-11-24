//! The Windows program (docs/LINUX.md: the Linux one is written beside it).
//!
//! queqiao-viewer: the controlling side. Shows the remote screen in a window and sends mouse and
//! keyboard input made in that window.
//!
//! With an account (M3), started without options: the login form (once; the login is then saved,
//! DPAPI-encrypted), then the account's devices; double-click one to control it.
//! For tests and scripts:
//!   queqiao-viewer [--server <host:port> --pin <fingerprint> --user <name> --password <password>]
//!                  [--device <name or id> | --list | --logout | --ui-test <name or id>]
//!   --device connects without any window but the session's; --list prints the devices; --logout ends the
//!   saved login; --ui-test goes through the login form and the device list as a user would (filled and
//!   pressed by the program) and saves pictures of both next to the report.
//! By IP address (M2): queqiao-viewer --listen <ip:port> | --connect <agent-ip:port>
//! Common options: [--seconds N] [--width N] [--report <file>] [--input-test] [--force <switches>]
//! --force sets test switches as QUEQIAO_FORCE does (for example network:relay).
//! --input-test sends a fixed sequence instead of the window's input: five pointer moves and a press of
//! F24 (a key no program uses); the agent run with --verify-input checks them. --version prints the
//! version.

use crate::options::*;
use crate::session::*;

use std::time::Duration;

use queqiao_core::text::tr;
use queqiao_display::show_error;
use queqiao_transport::{Addresses, Signal, Signaling};
use windows::Win32::Foundation::HMODULE;

use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE, D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_UNKNOWN, D3D_DRIVER_TYPE_WARP,
};

use windows::Win32::Graphics::Direct3D11::*;

use windows::Win32::Graphics::Dxgi::{IDXGIAdapter, IDXGIDevice};

use windows::core::Interface;

pub(crate) fn main() {
    queqiao_core::platform::install_crash_log("queqiao-viewer");
    #[cfg(feature = "test-switches")]
    if std::env::args().any(|arg| arg == "--crash-test") {
        panic!("crash test (asked for with --crash-test)");
    }
    // A terminal window (started by a session of this viewer): it runs until its command line ends.
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("--terminal-pipe")
        && let Some(pipe) = args.get(2)
    {
        let title = match args.iter().position(|arg| arg == "--terminal-title") {
            Some(at) => args.get(at + 1).cloned().unwrap_or_default(),
            None => String::new(),
        };
        let test =
            cfg!(feature = "test-switches") && args.iter().any(|arg| arg == "--terminal-test");
        std::process::exit(queqiao_terminal::run_helper(pipe, &title, test));
    }
    let banner = queqiao_core::banner(env!("CARGO_PKG_NAME"));
    let parsed = match args.get(1).map(String::as_str) {
        Some("--invite") => Some(invitation_options(&args[2..])),
        _ => parse(args.iter().skip(1).cloned()),
    };
    let Some(options) = parsed else {
        println!("{banner}");
        if cfg!(feature = "test-switches") {
            println!(
                "usage: queqiao-viewer [--server <host:port> --pin <fingerprint> --user <name> --password <password>]"
            );
            println!(
                "                      [--device <name or id> | --list | --logout | --ui-test <name or id>]"
            );
            println!("       queqiao-viewer --listen <ip:port> | --connect <agent-ip:port>");
            println!("       common: [--seconds N] [--width N] [--report <file>] [--input-test]");
        } else {
            println!(
                "usage: queqiao-viewer (the device list) | --invite <queqiao: link> | --open <device> | --help-connect <link>"
            );
        }
        // --version is answered by the banner; any other option is refused.
        let asked_version = args.get(1).map(String::as_str) == Some("--version");
        std::process::exit(if asked_version { 0 } else { 2 });
    };
    let mut lines = vec![banner];
    let mut failure = None;
    let passed = match run(&options, &mut lines) {
        Ok(passed) => passed,
        Err(error) => {
            lines.push(format!("ERROR {error}"));
            failure = Some(error);
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
    if let Some(error) = failure
        && options.interactive()
    {
        show_error(tr("Queqiao"), &error);
    }
    std::process::exit(if passed { 0 } else { 1 });
}

fn run(options: &Options, lines: &mut Vec<String>) -> Result<bool, String> {
    lines.extend(queqiao_core::platform::prepare_process());
    if !options.direct() {
        return crate::account::run_account(options, lines);
    }
    let (device, context, gpu) = create_device()?;
    lines.push(format!("device: {gpu}"));
    let mut signal = match (&options.connect, &options.listen) {
        (Some(address), _) => Signal::connect(address, Duration::from_secs(60)),
        (None, Some(address)) => Signal::listen(address, Duration::from_secs(120)),
        (None, None) => unreachable!("direct() checked it"),
    }
    .map_err(|e| e.to_string())?;
    signal.hello("viewer").map_err(|e| e.to_string())?;
    let agent = signal.peer_address().map_err(|e| e.to_string())?;
    session(
        options,
        &device,
        &context,
        &mut signal,
        &agent.to_string(),
        Addresses::All,
        lines,
    )
}

/// The GPU's D3D11 device with video support: the GPU showing the primary screen first (the window opens there; no
/// picture crosses between GPUs), then the others discrete first (the user's choice, 2026-11-23); Microsoft's
/// software adapter (WARP) without a usable GPU or when forced (QUEQIAO_FORCE=adapter:warp).
pub(crate) fn create_device() -> Result<(ID3D11Device, ID3D11DeviceContext, String), String> {
    let forced_warp = queqiao_core::forced("adapter").as_deref() == Some("warp");
    let video = D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT;
    let mut failures = Vec::new();
    if !forced_warp {
        for (adapter, name) in queqiao_capture::gpus_screen_first() {
            let mut device = None;
            let mut context = None;
            let created = unsafe {
                D3D11CreateDevice(
                    &adapter,
                    D3D_DRIVER_TYPE_UNKNOWN,
                    HMODULE::default(),
                    video,
                    None,
                    D3D11_SDK_VERSION,
                    Some(&mut device),
                    None,
                    Some(&mut context),
                )
            };
            match (created, device, context) {
                (Ok(()), Some(device), Some(context)) => return Ok((device, context, name)),
                (Err(error), _, _) => failures.push(format!("{name}: {}", describe(error))),
                _ => failures.push(format!("{name}: nothing returned")),
            }
        }
    }
    let video = D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT;
    let attempts: &[(D3D_DRIVER_TYPE, D3D11_CREATE_DEVICE_FLAG)] = if forced_warp {
        &[(D3D_DRIVER_TYPE_WARP, D3D11_CREATE_DEVICE_BGRA_SUPPORT)]
    } else {
        &[
            (D3D_DRIVER_TYPE_HARDWARE, video),
            (D3D_DRIVER_TYPE_WARP, D3D11_CREATE_DEVICE_BGRA_SUPPORT),
        ]
    };
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

pub(crate) fn describe(error: windows::core::Error) -> String {
    format!("0x{:08X} {}", error.code().0, error.message())
}
