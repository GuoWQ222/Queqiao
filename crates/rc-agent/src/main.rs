//! rc-agent: the controlled side, in the user session. Keeps the presence connection; during a
//! session it captures, encodes and sends the screen and injects the viewer's input.
//! M0: prints its banner only. Capture and encode arrive in M1, the network and input in M2.

fn main() {
    println!("{}", rc_core::banner(env!("CARGO_PKG_NAME")));
}
