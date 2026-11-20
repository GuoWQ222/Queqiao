//! queqiao-viewer: the controlling side. Login, device list, and the viewer window that decodes and
//! presents the remote screen and sends local mouse and keyboard input.
//! M0: prints its banner only. Decode and display arrive in M1, the network and input in M2.

fn main() {
    println!("{}", queqiao_core::banner(env!("CARGO_PKG_NAME")));
}
