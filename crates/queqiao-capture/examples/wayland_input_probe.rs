//! Tests (docs/LINUX.md): input on Wayland while the screen is captured, as an agent's session has it. Opens the
//! capture (on GNOME and KDE the RemoteDesktop portal session: the desktop asks, scripts/portal_approve.py answers in
//! tests), keeps taking frames on a thread, and moves the pointer to the input test's five places through
//! queqiao-input's injector, each `pause` ms apart, clicks there, then presses F24 (or PROBE_SCANCODE, hex).
//! With `still`, no input: only the pictures of the screen while it stays as it is.
//! Usage: wayland_input_probe [settle seconds] [pause ms] [still]

#[cfg(target_os = "linux")]
fn main() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    use std::time::Duration;

    use queqiao_core::protocol::InputEvent;

    let mut args = std::env::args().skip(1);
    let settle: u64 = args.next().and_then(|v| v.parse().ok()).unwrap_or(3);
    let pause: u64 = args.next().and_then(|v| v.parse().ok()).unwrap_or(400);
    let still = args.next().is_some_and(|v| v == "still");
    let mut duplicator = match queqiao_capture::Duplicator::primary() {
        Ok(duplicator) => duplicator,
        Err(error) => {
            println!("PROBE_FAIL capture: {error}");
            return;
        }
    };
    let info = duplicator.info().clone();
    println!("capture: {} ({}x{})", info.output, info.width, info.height);
    let stop = Arc::new(AtomicBool::new(false));
    let frames = Arc::new(AtomicU32::new(0));
    let capture = {
        let (stop, frames) = (stop.clone(), frames.clone());
        std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                if let Ok(Some(_)) = duplicator.acquire(100) {
                    frames.fetch_add(1, Ordering::Relaxed);
                }
                // The pointer's shape where it comes apart from the pictures: its alpha as text, to see it is one.
                if let Some(shape) = duplicator.take_pointer_shape() {
                    println!(
                        "pointer shape {}x{}, hot spot ({}, {})",
                        shape.width, shape.height, shape.hot_x, shape.hot_y
                    );
                    for row in shape.data.chunks(shape.pitch.max(1) as usize).take(24) {
                        let line: String = row
                            .chunks(4)
                            .take(24)
                            .map(|pixel| match pixel.get(3) {
                                Some(&alpha) if alpha > 192 => '#',
                                Some(&alpha) if alpha > 32 => '+',
                                _ => '.',
                            })
                            .collect();
                        println!("  {line}");
                    }
                }
                if let Some(visible) = duplicator.take_pointer_visible() {
                    println!("pointer visible: {visible}");
                }
            }
        })
    };
    std::thread::sleep(Duration::from_secs(settle));
    // Pictures of the screen as it was (the input below changes it): a still screen must send one too.
    println!("frames before input: {}", frames.load(Ordering::Relaxed));
    if still {
        stop.store(true, Ordering::Relaxed);
        let _ = capture.join();
        println!("PROBE_DONE");
        return;
    }
    // Only now: new input devices make some compositors draw (a change on the screen).
    let injector = queqiao_input::Injector::new(queqiao_input::ScreenRect {
        left: info.left,
        top: info.top,
        width: info.width,
        height: info.height,
    });
    println!("input through {}", injector.describe());
    for (fx, fy) in [
        (0.25, 0.25),
        (0.75, 0.25),
        (0.75, 0.75),
        (0.25, 0.75),
        (0.5, 0.5),
    ] {
        let event = InputEvent::MouseMove {
            x: (65535.0 * fx) as u16,
            y: (65535.0 * fy) as u16,
        };
        // As the agent does before each event (its pointer check).
        let before = queqiao_input::cursor_position();
        println!(
            "{event:?} (pointer {before:?}): {:?}",
            injector.inject(&event)
        );
        std::thread::sleep(Duration::from_millis(pause));
    }
    // A click where the pointer is (a desktop that focuses on click gives that window the keyboard).
    for down in [true, false] {
        let event = InputEvent::MouseButton {
            button: queqiao_core::protocol::MouseButton::Left,
            down,
        };
        println!("{event:?}: {:?}", injector.inject(&event));
        std::thread::sleep(Duration::from_millis(pause));
    }
    for down in [true, false] {
        let event = InputEvent::Key {
            // F24, or another key (PROBE_SCANCODE, hex).
            scancode: std::env::var("PROBE_SCANCODE")
                .ok()
                .and_then(|code| u16::from_str_radix(code.trim_start_matches("0x"), 16).ok())
                .unwrap_or(0x76),
            extended: false,
            down,
        };
        println!("{event:?}: {:?}", injector.inject(&event));
        std::thread::sleep(Duration::from_millis(pause));
    }
    stop.store(true, Ordering::Relaxed);
    let _ = capture.join();
    println!("frames meanwhile: {}", frames.load(Ordering::Relaxed));
    println!("PROBE_DONE");
}

#[cfg(not(target_os = "linux"))]
fn main() {}
