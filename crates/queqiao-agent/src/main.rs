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
#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
mod autostart;
#[cfg(windows)]
mod lock_gate;
#[cfg(windows)]
mod power;
#[cfg(windows)]
mod service_link;
#[cfg(windows)]
mod ui;
#[cfg(windows)]
mod update;
#[cfg(windows)]
mod wake;
#[cfg(windows)]
mod win;
#[cfg(windows)]
mod wol_check;

#[cfg(windows)]
fn main() {
    win::main()
}

/// Not yet written for this system (docs/LINUX.md): the banner for `--version`, otherwise a short notice.
#[cfg(not(windows))]
fn main() {
    let banner = queqiao_core::banner(env!("CARGO_PKG_NAME"));
    if std::env::args().any(|argument| argument == "--version") {
        println!("{banner}");
        return;
    }
    eprintln!("{banner}: not available on this system yet (docs/LINUX.md)");
    std::process::exit(2);
}
