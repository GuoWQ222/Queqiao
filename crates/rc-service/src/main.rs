//! rc-service: Windows service (LocalSystem) on each controlled PC. Starts and guards rc-agent in
//! the active console session (so it reaches the login and UAC desktops), Ctrl+Alt+Del, remote restart.
//! M0: prints its banner only. The service arrives in M5.

fn main() {
    println!("{}", rc_core::banner(env!("CARGO_PKG_NAME")));
}
