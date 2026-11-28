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

// On every system (docs/LINUX.md steps 3 and 5): the command line, the session with a viewer, the account mode.
mod account;
/// Drawing on this PC's screen: the viewer's pen strokes over it (help sessions).
mod annotate;
/// Sharp pictures on a narrow link: the bitrate the link carried, held while the screen is still; a keyframe after motion.
mod hold;
mod lock_gate;
mod options;
/// The Queqiao service's channels (requests, the desktop helper), on both systems.
mod service_link;
mod session;
mod ui;
/// Wake-on-LAN: the magic packets, and this PC's cards (from Windows or from /sys/class/net).
mod wake;

// Windows: the program (win.rs) and what only Windows has.
#[cfg(windows)]
mod autostart;
#[cfg(windows)]
mod power;
#[cfg(windows)]
mod update;
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
#[path = "linux/autostart.rs"]
mod autostart;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
#[path = "linux/power.rs"]
mod power;
/// Super Screen on Linux: GNOME's virtual monitor for the session.
#[cfg(target_os = "linux")]
#[path = "linux/super_screen.rs"]
mod super_screen;
#[cfg(target_os = "linux")]
#[path = "linux/update.rs"]
mod update;
#[cfg(target_os = "linux")]
#[path = "linux/wol_check.rs"]
mod wol_check;
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
