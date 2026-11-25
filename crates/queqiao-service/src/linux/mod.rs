//! The Linux program (docs/LINUX.md): the Queqiao service, as root under systemd (`queqiao.service`), and its desktop
//! helper. As on Windows it has no network of its own: everything from the network goes through the agent, which runs
//! as the person signed in.
//!
//! - `queqiao-service run`: what systemd starts. For the session on the PC's own screen (seat0, as logind says) it
//!   keeps two processes running with a private channel between them: the desktop helper (root, with only the
//!   capabilities it needs: the screen through KMS and input through uinput, which work at the sign-in and lock
//!   screens too) and the agent: as the person signed in there, or, at the sign-in screen, as the system user
//!   `queqiao` with the device the person handed over (when they allow control there).
//! - `queqiao-service desktop --channel <read>,<write> --session <id> [--sign-in]`: the desktop helper; only the
//!   service starts it.
//! - `queqiao-service install-update <package> <version>`: installs an update the service checked, in a systemd unit
//!   of its own (installing the package restarts the service), and puts the earlier package back if the new one does
//!   not keep running.
//! - `queqiao-service status`: whether it runs, and what it serves now.

mod helper;
mod launch;
pub mod log;
mod logind;
mod logon;
mod supervise;
mod update;

/// The service's state (root only): the device for the sign-in screen, the notices for the next person, updates.
pub const STATE: &str = "/var/lib/queqiao";
/// While it runs (systemd's RuntimeDirectory): its process number and what it serves.
pub const RUNTIME: &str = "/run/queqiao";

fn usage() -> String {
    format!(
        "{}\nusage: queqiao-service run | status\n(systemd starts `run` as queqiao.service; `desktop` and `install-update` are for the service only.)",
        queqiao_core::banner(env!("CARGO_PKG_NAME"))
    )
}

pub fn main() {
    queqiao_core::platform::install_crash_log("queqiao-service");
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("run") => supervise::run(),
        Some("desktop") => helper::run(&args[1..]),
        Some("install-update") => update::install(&args[1..]),
        Some("status") => {
            println!("{}", supervise::status());
            Ok(())
        }
        Some("--version" | "--help") | None => {
            println!("{}", usage());
            Ok(())
        }
        _ => Err(usage()),
    };
    if let Err(error) = result {
        log::line(&error);
        std::process::exit(1);
    }
}
