//! Tests (docs/LINUX.md): the RemoteDesktop portal's input, step by step. Opens the portal session (the desktop asks;
//! scripts/portal_approve.py answers in tests), waits `settle` seconds, moves the pointer to the input test's five
//! places (fractions of the stream), each `pause` ms apart, then presses and releases F24. Prints each step.
//! Usage: portal_input_probe [settle seconds] [pause ms]

#[cfg(target_os = "linux")]
fn main() {
    use std::time::Duration;

    let mut args = std::env::args().skip(1);
    let settle: u64 = args.next().and_then(|v| v.parse().ok()).unwrap_or(3);
    let pause: u64 = args.next().and_then(|v| v.parse().ok()).unwrap_or(400);
    let session = match queqiao_portal::session() {
        Ok(session) => session,
        Err(error) => {
            println!("PROBE_FAIL {error}");
            return;
        }
    };
    println!("session: {}", session.describe());
    std::thread::sleep(Duration::from_secs(settle));
    let (width, height) = session.size;
    for (fx, fy) in [
        (0.25, 0.25),
        (0.75, 0.25),
        (0.75, 0.75),
        (0.25, 0.75),
        (0.5, 0.5),
    ] {
        let (x, y) = (f64::from(width - 1) * fx, f64::from(height - 1) * fy);
        let result = session.pointer_to(x, y);
        println!("pointer to ({x:.0}, {y:.0}): {result:?}");
        std::thread::sleep(Duration::from_millis(pause));
    }
    for down in [true, false] {
        println!(
            "F24 {}: {:?}",
            if down { "down" } else { "up" },
            session.key(194, down)
        );
        std::thread::sleep(Duration::from_millis(pause));
    }
    println!("PROBE_DONE");
}

#[cfg(not(target_os = "linux"))]
fn main() {}
