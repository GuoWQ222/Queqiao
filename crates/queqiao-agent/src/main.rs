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

// On every system (docs/LINUX.md step 3): the command line and the session with a viewer.
mod lock_gate;
mod options;
mod session;

// Windows: the program (win.rs) and what only Windows has.
#[cfg(windows)]
mod autostart;
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
/// What the session needs from the system it runs on (`Latest`, `upload`, `keep_frame`, `describe`, the privacy
/// test): win.rs or linux.rs.
#[cfg(windows)]
use win as platform;

// Linux: the program (linux.rs) and the same modules as Windows', for what the session uses of them.
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
#[path = "linux/power.rs"]
mod power;
#[cfg(target_os = "linux")]
#[path = "linux/service_link.rs"]
mod service_link;
#[cfg(target_os = "linux")]
#[path = "linux/ui.rs"]
mod ui;
#[cfg(target_os = "linux")]
use linux as platform;

#[cfg(windows)]
fn main() {
    win::main()
}

#[cfg(target_os = "linux")]
fn main() {
    linux::main()
}
