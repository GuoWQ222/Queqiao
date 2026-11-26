//! Windows' firewall asks the person, once per program, whether it may take connections from other computers: its
//! "Allow access" dialog (Private and Public networks) appears when a program without a rule starts listening.
//! Allowed, the firewall lets in a session's packets from any port of the other side, so a session reaches this PC
//! directly whatever NAT the other side is behind (as UU Remote does: its rules on the PC were made by this dialog,
//! measured 2026-11-27). Refused, or decided before, nothing shows. Punching (`punch`) finds direct paths without it.

use std::net::TcpListener;
use std::time::Duration;

/// Listens on every address for a moment (accepting nothing) on a thread of its own, so Windows asks if it has not.
pub fn let_windows_ask() {
    let _ = std::thread::Builder::new()
        .name("queqiao-firewall".into())
        .spawn(|| {
            let v4 = TcpListener::bind(("0.0.0.0", 0));
            let v6 = TcpListener::bind(("::", 0));
            // Long enough for the firewall service to see the listening socket; the dialog then waits for the person.
            std::thread::sleep(Duration::from_secs(3));
            drop((v4, v6));
        });
}
