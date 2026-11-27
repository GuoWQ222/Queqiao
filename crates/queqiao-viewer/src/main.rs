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
#![cfg_attr(windows, windows_subsystem = "windows")]

// On every system (docs/LINUX.md step 3): the command line, the session with an agent, the session's keys.
/// The account mode, on every system.
mod account;
mod control;
mod files_ui;
/// A session thread's signaling through the device list's account (ICE restarts during the session).
mod link;
use queqiao_core::hotkeys;
mod options;
mod session;
mod system_keys;

#[cfg(windows)]
mod win;
/// What the session needs from the system it runs on (`describe`, `create_device`): win.rs or linux.rs.
#[cfg(windows)]
use win as platform;

#[cfg(target_os = "linux")]
mod linux;
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
