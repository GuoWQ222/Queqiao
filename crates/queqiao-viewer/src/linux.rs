//! The Linux program (docs/LINUX.md steps 3 and 5): the viewer with an account (account.rs, shared with Windows: the
//! login, the device list, help links) or by IP address (M2, `--connect` / `--listen` in a test build), with the session
//! shared with Windows (session.rs). Also what the session needs from this system: the error text, the device (nothing
//! to open: pictures are drawn on the CPU).

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
    // A terminal window (started by a session of this viewer, in a terminal emulator): it runs until its command line
    // ends.
    if args.get(1).map(String::as_str) == Some("--terminal-pipe")
        && let Some(socket) = args.get(2)
    {
        let title = match args.iter().position(|arg| arg == "--terminal-title") {
            Some(at) => args.get(at + 1).cloned().unwrap_or_default(),
            None => String::new(),
        };
        let test =
            cfg!(feature = "test-switches") && args.iter().any(|arg| arg == "--terminal-test");
        std::process::exit(queqiao_terminal::run_helper(socket, &title, test));
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
