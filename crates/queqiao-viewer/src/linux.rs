//! The Linux program (docs/LINUX.md step 3): the viewer by IP address (M2, `--connect` / `--listen` in a test build),
//! with the session shared with Windows (session.rs). The account's device list, the login and the main window come
//! with the GUI (step 5); started otherwise it says so. Also what the session needs from this system: the error text,
//! the device (nothing to open: pictures are drawn on the CPU).

use std::time::Duration;

use queqiao_capture::{Context, Device};
use queqiao_transport::{Addresses, Signal, Signaling};

use crate::options::{Options, invitation_options, parse};
use crate::session::session;

pub(crate) fn describe(error: impl std::fmt::Display) -> String {
    error.to_string()
}

/// The device a session draws with, and its name (Windows: a D3D11 device on the best GPU).
pub(crate) fn create_device() -> Result<(Device, Context, String), String> {
    Ok((Device, Context, "the CPU (X11)".into()))
}

pub(crate) fn main() {
    queqiao_core::platform::install_crash_log("queqiao-viewer");
    let args: Vec<String> = std::env::args().collect();
    let banner = queqiao_core::banner(env!("CARGO_PKG_NAME"));
    let parsed = match args.get(1).map(String::as_str) {
        Some("--invite") => Some(invitation_options(&args[2..])),
        _ => parse(args.iter().skip(1).cloned()),
    };
    let Some(options) = parsed else {
        println!("{banner}");
        if cfg!(feature = "test-switches") {
            println!("usage: queqiao-viewer --listen <ip:port> | --connect <agent-ip:port>");
            println!("       common: [--seconds N] [--width N] [--report <file>] [--input-test]");
        } else {
            println!(
                "usage: queqiao-viewer (on Linux, by IP address in a test build so far: docs/LINUX.md)"
            );
        }
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
            let _ = std::fs::write(path, lines.join("\n") + "\n");
        }
        None => lines.iter().for_each(|line| println!("{line}")),
    }
    // Started by a person (from the desktop): the error in a message too.
    if let Some(error) = failure
        && options.interactive()
    {
        queqiao_display::show_error(queqiao_core::text::tr("Queqiao"), &error);
    }
    std::process::exit(if passed { 0 } else { 1 });
}

fn run(options: &Options, lines: &mut Vec<String>) -> Result<bool, String> {
    lines.extend(queqiao_core::platform::prepare_process());
    // What needs the account or its forms: refused by name rather than left unheeded.
    let unsupported = [
        ("--server", options.server.is_some()),
        ("--pin", options.pin.is_some()),
        ("--user", options.user.is_some()),
        ("--password", options.password.is_some()),
        ("--signup", options.signup),
        ("--trust-new-key", options.trust_new_key.is_some()),
        ("--test-device", options.test_device.is_some()),
        ("--also-device", options.also_device.is_some()),
        ("--remove-device", options.remove_device.is_some()),
        ("--help-id", options.help_id.is_some()),
        ("--help-code", options.help_code.is_some()),
        ("--test-submit-help", options.test_submit_help),
    ];
    let refused: Vec<&str> = unsupported
        .iter()
        .filter(|(_, given)| *given)
        .map(|(name, _)| *name)
        .collect();
    if !refused.is_empty() {
        return Err(format!(
            "not on Linux yet (accounts and help links come next, docs/LINUX.md): {}",
            refused.join(" ")
        ));
    }
    if !options.direct() {
        return Err(
            "on Linux the viewer connects by IP address so far (--connect or --listen, a test build); the device \
             list, the login and help links come with the GUI (docs/LINUX.md)"
                .into(),
        );
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
