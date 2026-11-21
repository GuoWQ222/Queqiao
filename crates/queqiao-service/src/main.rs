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

mod helper;
mod install;
mod launch;
mod log;
mod logon;
mod service;

/// The service's name, and the registry key under Services that holds its settings.
pub const NAME: &str = "Queqiao";

fn usage() -> String {
    format!(
        "{}\nusage: queqiao-service install [--agent-arguments \"<arguments>\"] [--logon-arguments \"<arguments>\"] [--no-restart]\n       queqiao-service uninstall | status\n(Windows starts it as a service; `desktop` is for the service only.)",
        queqiao_core::banner(env!("CARGO_PKG_NAME"))
    )
}

fn main() {
    queqiao_core::platform::install_crash_log("queqiao-service");
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("install") => install::install(&args[1..]),
        Some("uninstall") => install::uninstall(),
        Some("status") => install::status(),
        Some("desktop") => helper::run(&args[1..]),
        Some("--version" | "--help") => {
            println!("{}", usage());
            Ok(())
        }
        // Started by hand: what this program is for.
        None => service::dispatch().map(|was_service| {
            if !was_service {
                println!("{}", usage());
            }
        }),
        _ => Err(usage()),
    };
    if let Err(error) = result {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
