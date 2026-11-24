//! Encode probe for Linux (docs/LINUX.md): paints a test picture with a moving box on the X display ($DISPLAY, in a
//! window over the whole screen), captures it as the agent does (acquire -> keep -> release -> encode the kept frame)
//! with the encoder Queqiao picks (`QUEQIAO_FORCE=encode:software` or `encode:nvenc` chooses), decodes the H.264 with
//! OpenH264 and compares it with the captured pixels (BT.709 limited range: the encoder's input). Reports each stage's
//! time, the frames' sizes, the PSNR, and the delay from a change being seen to its encoded frame.
//!   linux_encode_probe [frames]
//! Prints PROBE_PASS when every frame came out, the first is a keyframe, every frame decodes and luma PSNR averages
//! at least 32 dB; else PROBE_FAIL <why>. The screen must be one the encoder takes unscaled (OpenH264: up to 1080p).

#[cfg(target_os = "linux")]
fn main() {
    linux::run();
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("linux_encode_probe is for Linux; on Windows run encode_probe");
}

#[cfg(target_os = "linux")]
mod linux {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    use openh264::formats::YUVSource;
    use queqiao_capture::{Duplicator, qpc_frequency, qpc_now};
    use queqiao_codec::ScreenEncoder;
    use x11rb::connection::{Connection, RequestConnection};
    use x11rb::protocol::xproto::{
        ChangeGCAux, ConnectionExt as _, CreateGCAux, CreateWindowAux, ImageFormat, Rectangle,
        WindowClass,
    };
    use x11rb::wrapper::ConnectionExt as _;

    const BOX: u16 = 160;

    /// The test picture: a gradient, text-like rows of small marks, thin stripes, colour blocks.
    fn background(width: usize, height: usize) -> Vec<u8> {
        let mut pixels = vec![0u8; width * height * 4];
        for y in 0..height {
            for x in 0..width {
                let bgr: [u8; 3] = match x * 4 / width {
                    0 => [
                        (x * 1024 / width).min(255) as u8,
                        (y * 255 / height) as u8,
                        96,
                    ],
                    1 => {
                        let mark = y % 16 < 10 && (x * 7 + y * 13) % 11 < 3;
                        if mark { [20, 20, 20] } else { [245, 245, 240] }
                    }
                    2 => {
                        if (x / 3) % 2 == 0 {
                            [40, 40, 40]
                        } else {
                            [230, 230, 230]
                        }
                    }
                    _ => [[0, 0, 255], [0, 200, 0], [255, 0, 0]][(y * 3 / height).min(2)],
                };
                let i = (y * width + x) * 4;
                pixels[i..i + 4].copy_from_slice(&[bgr[0], bgr[1], bgr[2], 255]);
            }
        }
        pixels
    }

    /// Paints the picture once, then moves a box over it about 60 times a second until `stop`.
    fn paint(stop: &AtomicBool) -> Result<(), Box<dyn std::error::Error>> {
        let (connection, screen_number) = x11rb::connect(None)?;
        let screen = &connection.setup().roots[screen_number];
        let (width, height, depth) = (
            screen.width_in_pixels,
            screen.height_in_pixels,
            screen.root_depth,
        );
        let window = connection.generate_id()?;
        connection.create_window(
            depth,
            window,
            screen.root,
            0,
            0,
            width,
            height,
            0,
            WindowClass::INPUT_OUTPUT,
            screen.root_visual,
            &CreateWindowAux::new().override_redirect(1),
        )?;
        connection.map_window(window)?;
        let pixmap = connection.generate_id()?;
        connection.create_pixmap(depth, pixmap, window, width, height)?;
        let gc = connection.generate_id()?;
        connection.create_gc(gc, window, &CreateGCAux::new())?;
        let picture = background(usize::from(width), usize::from(height));
        let row_bytes = usize::from(width) * 4;
        let band = ((connection.maximum_request_bytes() - 64) / row_bytes).clamp(1, 64);
        for (index, rows) in picture.chunks(band * row_bytes).enumerate() {
            connection.put_image(
                ImageFormat::Z_PIXMAP,
                pixmap,
                gc,
                width,
                (rows.len() / row_bytes) as u16,
                0,
                (index * band) as i16,
                0,
                depth,
                rows,
            )?;
        }
        connection.copy_area(pixmap, window, gc, 0, 0, 0, 0, width, height)?;
        connection.sync()?;
        let (mut x, mut y, mut step) = (0i16, 0i16, 0u32);
        while !stop.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(16));
            connection.copy_area(pixmap, window, gc, x, y, x, y, BOX, BOX)?;
            step += 1;
            x = ((step * 23) % u32::from(width - BOX)) as i16;
            y = ((step * 11) % u32::from(height - BOX)) as i16;
            let colour = [0xff_8800u32, 0x00_88ff, 0x22_cc22][step as usize % 3];
            connection.change_gc(gc, &ChangeGCAux::new().foreground(colour))?;
            let area = Rectangle {
                x,
                y,
                width: BOX,
                height: BOX,
            };
            connection.poly_fill_rectangle(window, gc, &[area])?;
            connection.sync()?;
        }
        connection.destroy_window(window)?;
        connection.sync()?;
        Ok(())
    }

    /// BT.709 limited range, as the converter (cpu_convert.rs): Y per pixel, U and V per 2x2 block.
    fn yuv_of(bgra: &[u8], stride: usize, width: usize, height: usize) -> [Vec<u8>; 3] {
        let rgb = |x: usize, y: usize| {
            let i = y * stride + x * 4;
            [bgra[i + 2], bgra[i + 1], bgra[i]].map(i32::from)
        };
        let mut luma = vec![0u8; width * height];
        for y in 0..height {
            for x in 0..width {
                let [r, g, b] = rgb(x, y);
                luma[y * width + x] = (((47 * r + 157 * g + 16 * b + 128) >> 8) + 16) as u8;
            }
        }
        let (cw, ch) = (width / 2, height / 2);
        let (mut u, mut v) = (vec![0u8; cw * ch], vec![0u8; cw * ch]);
        for y in 0..ch {
            for x in 0..cw {
                let mut sum = [0; 3];
                for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                    let value = rgb(2 * x + dx, 2 * y + dy);
                    for c in 0..3 {
                        sum[c] += value[c];
                    }
                }
                let [r, g, b] = sum.map(|total| (total + 2) / 4);
                let cb = ((-26 * r - 86 * g + 112 * b + 128) >> 8) + 128;
                let cr = ((112 * r - 102 * g - 10 * b + 128) >> 8) + 128;
                u[y * cw + x] = cb.clamp(16, 240) as u8;
                v[y * cw + x] = cr.clamp(16, 240) as u8;
            }
        }
        [luma, u, v]
    }

    fn psnr(reference: &[u8], decoded: &[u8], stride: usize, width: usize) -> f64 {
        let mut sum = 0f64;
        for (row, reference_row) in reference.chunks(width).enumerate() {
            let decoded_row = &decoded[row * stride..row * stride + width];
            for (a, b) in reference_row.iter().zip(decoded_row) {
                let d = f64::from(*a) - f64::from(*b);
                sum += d * d;
            }
        }
        let mse = sum / reference.len() as f64;
        if mse == 0.0 {
            99.0
        } else {
            10.0 * (255.0 * 255.0 / mse).log10()
        }
    }

    fn average(values: &[f64]) -> f64 {
        values.iter().sum::<f64>() / values.len().max(1) as f64
    }

    fn fail(why: &str) -> ! {
        println!("PROBE_FAIL {why}");
        std::process::exit(1);
    }

    pub fn run() {
        let wanted: usize = std::env::args()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .unwrap_or(120);
        let stop = Arc::new(AtomicBool::new(false));
        let painter = {
            let stop = stop.clone();
            std::thread::spawn(move || paint(&stop).map_err(|e| e.to_string()))
        };
        std::thread::sleep(Duration::from_millis(500));
        let mut duplicator =
            Duplicator::primary().unwrap_or_else(|e| fail(&format!("capture: {}", e.message())));
        let (width, height) = (duplicator.info().width, duplicator.info().height);
        let mut encoder = ScreenEncoder::new(
            duplicator.device(),
            duplicator.context(),
            (width, height),
            60,
            false,
        )
        .unwrap_or_else(|e| fail(&format!("encoder: {}", e.message())));
        let config = encoder.config().clone();
        println!(
            "screen   {width}x{height}\nencoder  {} at {}x{}, {} fps, {} kbit/s\nconvert  {}\nskipped  {:?}",
            encoder.encoder().name(),
            config.width,
            config.height,
            config.fps,
            config.bitrate / 1000,
            encoder.converter_description(),
            encoder.encoder().skipped()
        );
        if (config.width, config.height) != (width, height) {
            stop.store(true, Ordering::Relaxed);
            fail("the encoder scales this screen: use one it takes unscaled");
        }
        let mut decoder = openh264::decoder::Decoder::new()
            .unwrap_or_else(|e| fail(&format!("OpenH264 decoder: {e}")));
        let (w, h) = (width as usize, height as usize);
        let ms = |a: Instant, b: Instant| (b - a).as_secs_f64() * 1e3;
        let (mut acquire, mut keep, mut encode, mut decode) = (vec![], vec![], vec![], vec![]);
        let (mut delay, mut sizes, mut y_psnr, mut u_psnr, mut v_psnr) =
            (vec![], vec![], vec![], vec![], vec![]);
        let mut problems = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(60);
        while sizes.len() < wanted && Instant::now() < deadline {
            let started = Instant::now();
            let frame = match duplicator.acquire(200) {
                Ok(Some(frame)) => frame,
                Ok(None) => continue,
                Err(e) => fail(&format!("acquire: {}", e.message())),
            };
            let acquired = Instant::now();
            encoder.keep(&frame);
            let kept = Instant::now();
            // The probe's own copy for the comparison (its time is left out of the delay).
            let (pixels, stride) = (frame.pixels().to_vec(), frame.stride);
            let copied = Instant::now();
            let seen = frame.present_qpc;
            let _ = duplicator.release();
            let before_encode = Instant::now();
            let index = sizes.len();
            let out = encoder
                .encode_kept(false)
                .unwrap_or_else(|e| fail(&format!("encode: {}", e.message())));
            let encoded = Instant::now();
            if seen > 0 {
                let seen_to_now = (qpc_now() - seen) as f64 * 1e3 / qpc_frequency() as f64;
                delay.push(seen_to_now - ms(kept, copied));
            }
            let reference = yuv_of(&pixels, stride, w, h);
            acquire.push(ms(started, acquired));
            keep.push(ms(acquired, kept));
            encode.push(ms(before_encode, encoded));
            if out.len() != 1 {
                problems.push(format!("frame {index}: {} encoded frames", out.len()));
            }
            for frame in &out {
                sizes.push(frame.data.len());
                if frame.keyframe != (index == 0) {
                    problems.push(format!("frame {index}: keyframe {}", frame.keyframe));
                }
                let decode_start = Instant::now();
                match decoder.decode(&frame.data) {
                    Ok(Some(yuv)) => {
                        decode.push(ms(decode_start, Instant::now()));
                        let (ys, us, vs) = yuv.strides();
                        y_psnr.push(psnr(&reference[0], yuv.y(), ys, w));
                        u_psnr.push(psnr(&reference[1], yuv.u(), us, w / 2));
                        v_psnr.push(psnr(&reference[2], yuv.v(), vs, w / 2));
                    }
                    Ok(None) => problems.push(format!("frame {index}: no picture decoded")),
                    Err(e) => problems.push(format!("frame {index}: decode {e}")),
                }
            }
        }
        stop.store(true, Ordering::Relaxed);
        if let Ok(Err(e)) = painter.join() {
            problems.push(format!("painter: {e}"));
        }
        let bytes: f64 = sizes.iter().map(|&n| n as f64).sum();
        println!(
            "frames   {} (wanted {wanted}), {:.0} kB average, first {} kB",
            sizes.len(),
            bytes / sizes.len().max(1) as f64 / 1e3,
            sizes.first().copied().unwrap_or(0) / 1000
        );
        println!(
            "time ms  acquire {:.2}  keep {:.2}  convert+encode {:.2}  decode {:.2}",
            average(&acquire),
            average(&keep),
            average(&encode),
            average(&decode)
        );
        let mut sorted = delay.clone();
        sorted.sort_by(f64::total_cmp);
        println!(
            "delay ms seen -> encoded: median {:.2}, max {:.2}",
            sorted.get(sorted.len() / 2).copied().unwrap_or(0.0),
            sorted.last().copied().unwrap_or(0.0)
        );
        if let Some(timing) = encoder.encoder().software_timing() {
            println!("software {timing:?}");
        }
        let (y, u, v) = (average(&y_psnr), average(&u_psnr), average(&v_psnr));
        let worst = y_psnr.iter().copied().fold(f64::INFINITY, f64::min);
        println!("PSNR dB  Y {y:.1} (worst {worst:.1})  U {u:.1}  V {v:.1}");
        for problem in problems.iter().take(5) {
            println!("problem  {problem}");
        }
        if sizes.len() < wanted {
            fail(&format!("{} frames of {wanted}", sizes.len()));
        }
        if !problems.is_empty() {
            fail(&format!("{} problems", problems.len()));
        }
        if y < 32.0 {
            fail(&format!("luma PSNR {y:.1} dB"));
        }
        println!("PROBE_PASS");
    }
}
