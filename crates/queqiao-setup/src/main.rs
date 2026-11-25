//! queqiao-setup: installs Queqiao for the person using this PC (no administrator needed), and removes it again.
//!
//! - `queqiao-setup` (as downloaded): a window with the choices, then Install.
//! - `--quiet [--autostart] [--service] [--report <file>]`: the same without a window (tests, updates).
//! - `--update [--report <file>]`: what an installed agent runs for an update it downloaded and checked: quiet, the
//!   person's choices kept, the earlier version put back if the new agent does not keep running.
//! - `--service-update [--report <file>]`: run as SYSTEM by the Queqiao service for an update it checked: its
//!   programs in %ProgramFiles%\Queqiao replaced, put back if its new agent does not keep running.
//! - `--uninstall [--quiet] [--report <file>]`: removes everything it put on this PC (Settings > Apps runs this).
//! - `--install-service`: run as an administrator (UAC) by the install when the service is chosen: the service and
//!   its agent from this installer's own files, through a folder in Program Files (install::install_service).
//! - `--remove-service`: run as an administrator by `--uninstall` when the service is installed.
//! - `--ui-picture <file>`: shows the window, saves a picture of it and leaves without installing (tests).
//! - `pack --output <installer> <file>...`: this program with those files as its resource (they are installed; see
//!   payload); sign the result afterwards (scripts\package.ps1).
//! - `release-key --output <file>`: a new release key, DPAPI-encrypted for the person running this; prints its
//!   public half (for queqiao_update::RELEASE_KEY).
//! - `release --key <file> --installer <file> --version <x.y.z> --output <folder> [--notes <text>]`: the update feed
//!   for that installer: the installer as queqiao-setup-<x.y.z>.exe, queqiao-update.json and its signature.
//! - `release-package --key <file> --package <name> --size <bytes> --sha256 <hex> --version <x.y.z> --output <folder>`:
//!   the manifest and signature for a Linux package (queqiao-update-deb.json or -rpm.json); the package goes beside.
#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
mod install;
#[cfg(windows)]
mod payload;
#[cfg(windows)]
mod service_update;
#[cfg(windows)]
mod win;

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
