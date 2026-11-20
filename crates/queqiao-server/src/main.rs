//! queqiao-server: accounts, devices, presence, WebSocket signaling and the STUN/TURN relay.
//! M0: prints its banner only. Accounts and signaling arrive in M3, the relay in M4.

fn main() {
    println!("{}", queqiao_core::banner(env!("CARGO_PKG_NAME")));
}
