//! queqiao-service: the Windows service (LocalSystem) on a controlled PC, and its desktop helper. It has no
//! network of its own: everything from the network goes through the agent, which runs as the person signed in.
//!
//! - Started by Windows: in the session at the PC's screen (the console session) it keeps two processes
//!   running, with a private channel between them: the desktop helper (SYSTEM, the only account that may see and
//!   use the sign-in, lock, Ctrl+Alt+Del and UAC screens) and the agent of the person signed in (as them). It sends
//!   Ctrl+Alt+Del (SendSAS) and restarts both on the agent's request.
//! - `queqiao-service install [--agent-arguments "<arguments>"] [--logon-arguments "<arguments>"] [--no-restart]`,
//!   `uninstall`, `status`: set-up,
//!   by an administrator. The programs must be in a folder only administrators can change.
//! - `queqiao-service desktop --channel <read>,<write>`: the desktop helper; only the service starts it.

#[cfg(windows)]
mod helper;
#[cfg(windows)]
mod install;
#[cfg(windows)]
mod launch;
#[cfg(windows)]
mod log;
#[cfg(windows)]
mod logon;
#[cfg(windows)]
mod secure_folder;
#[cfg(windows)]
mod service;
#[cfg(windows)]
mod update;
#[cfg(windows)]
mod win;
#[cfg(windows)]
pub use win::NAME;

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
