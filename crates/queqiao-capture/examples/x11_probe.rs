//! Capture probe for Linux under X11 (docs/LINUX.md): captures the display named by $DISPLAY for a few seconds, then
//! reports the monitors, the frames (how many, how long `acquire` took, how many screen changes each held), the
//! pointer shapes, and saves the first frame as a PPM picture to check by eye.
//!   x11_probe [seconds] [picture.ppm] [#rrggbb]
//! Prints PROBE_PASS when frames came and the picture is not blank (with a colour: the screen is that one colour, a
//! test display, and every pixel of the first frame must be exactly it), else PROBE_FAIL <why>. On Windows it only says
//! that it is for Linux (capture_probe is Windows' probe).

#[cfg(target_os = "linux")]
fn main() {
    linux::run();
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("x11_probe is for Linux; on Windows run capture_probe");
}

#[cfg(target_os = "linux")]
mod linux {
    use std::time::{Duration, Instant};

    use queqiao_capture::{Duplicator, qpc_frequency, screens};

    pub fn run() {
        let args: Vec<String> = std::env::args().skip(1).collect();
        let seconds: f64 = args.first().and_then(|s| s.parse().ok()).unwrap_or(5.0);
        let picture = args.get(1).cloned().filter(|path| !path.is_empty());
        let expected: Option<[u8; 3]> = args.get(2).and_then(|hex| {
            let hex = hex.trim_start_matches('#');
            let value = u32::from_str_radix(hex, 16)
                .ok()
                .filter(|_| hex.len() == 6)?;
            Some([(value >> 16) as u8, (value >> 8) as u8, value as u8])
        });
        let mut colour_ok: Option<bool> = None;
        match screens() {
            Ok(list) => {
                for screen in list {
                    println!(
                        "screen {}: {}x{}{}",
                        screen.index,
                        screen.width,
                        screen.height,
                        if screen.primary { " (primary)" } else { "" }
                    );
                }
            }
            Err(error) => {
                println!("PROBE_FAIL screens: {error}");
                return;
            }
        }
        let mut duplicator = match Duplicator::primary() {
            Ok(duplicator) => duplicator,
            Err(error) => {
                println!("PROBE_FAIL capture: {error}");
                return;
            }
        };
        let info = duplicator.info().clone();
        println!(
            "capturing {} {}x{} at ({}, {}) by {}",
            info.output, info.width, info.height, info.left, info.top, info.method
        );
        let started = Instant::now();
        let mut frames = 0u32;
        let mut changes = 0u32;
        let mut acquire_ms: Vec<f64> = Vec::new();
        let mut latency_ms: Vec<f64> = Vec::new();
        let mut shapes = 0u32;
        let mut first_saved = false;
        let mut blank = true;
        while started.elapsed() < Duration::from_secs_f64(seconds) {
            let asked = Instant::now();
            match duplicator.acquire(50) {
                Ok(Some(frame)) => {
                    let took = asked.elapsed().as_secs_f64() * 1000.0;
                    frames += 1;
                    changes += frame.accumulated;
                    latency_ms.push(
                        (frame.acquired_qpc - frame.present_qpc) as f64 * 1000.0
                            / qpc_frequency() as f64,
                    );
                    if frames > 1 {
                        acquire_ms.push(took);
                    }
                    if !first_saved {
                        first_saved = true;
                        let image = duplicator.copy_to_cpu(&frame).expect("copy");
                        let pixels = image.pixels.as_chunks::<4>().0;
                        let first = pixels[0];
                        blank = pixels.iter().all(|p| p[..3] == first[..3]);
                        if let Some(want) = expected {
                            let wrong =
                                pixels.iter().filter(|p| [p[2], p[1], p[0]] != want).count();
                            println!(
                                "first frame against #{:02x}{:02x}{:02x}: {wrong} of {} pixels differ",
                                want[0],
                                want[1],
                                want[2],
                                pixels.len()
                            );
                            colour_ok = Some(wrong == 0);
                        }
                        if let Some(path) = &picture {
                            let mut ppm =
                                format!("P6\n{} {}\n255\n", image.width, image.height).into_bytes();
                            for pixel in pixels {
                                ppm.extend_from_slice(&[pixel[2], pixel[1], pixel[0]]);
                            }
                            std::fs::write(path, ppm).expect("picture");
                            println!("first frame saved: {path}");
                        }
                    }
                    let _ = duplicator.release();
                }
                Ok(None) => {}
                Err(error) => {
                    println!("PROBE_FAIL acquire: {error}");
                    return;
                }
            }
            if duplicator.take_pointer_shape().is_some() {
                shapes += 1;
            }
            if let Some(visible) = duplicator.take_pointer_visible() {
                println!("pointer visible: {visible}");
            }
        }
        let median = |values: &mut Vec<f64>| {
            values.sort_by(f64::total_cmp);
            values.get(values.len() / 2).copied().unwrap_or(0.0)
        };
        println!(
            "frames {frames} in {:.1} s ({changes} screen changes), acquire median {:.2} ms, change -> frame median {:.2} ms, pointer shapes {shapes}",
            started.elapsed().as_secs_f64(),
            median(&mut acquire_ms),
            median(&mut latency_ms)
        );
        if frames == 0 {
            println!("PROBE_FAIL no frame");
        } else if let Some(ok) = colour_ok {
            // The screen was one known colour (a test display): the frame must be exactly it.
            println!(
                "{}",
                if ok {
                    "PROBE_PASS"
                } else {
                    "PROBE_FAIL the first frame is not the screen's colour"
                }
            );
        } else if blank {
            println!("PROBE_FAIL the first frame is one colour");
        } else {
            println!("PROBE_PASS");
        }
    }
}
