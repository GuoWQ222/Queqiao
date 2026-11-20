//! M1 step 1 check: captures the main screen for a few seconds and writes a report.
//!
//! Desktop duplication needs the interactive session (not session 0, so not straight from an SSH
//! shell): `scripts\run_in_session.ps1` starts this probe there. It has no console window; all output
//! goes to the report file.
//!
//! Usage: capture_probe <out_dir> [seconds]
//! Writes <out_dir>\capture_probe.txt and <out_dir>\capture_thumb.bmp (one frame, at most 480 px wide).

#![windows_subsystem = "windows"]

use std::path::{Path, PathBuf};

use queqiao_capture::{BgraImage, Duplicator, needs_restart, qpc_frequency, qpc_now};
use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};

fn main() {
    let mut args = std::env::args().skip(1);
    let out_dir = PathBuf::from(args.next().unwrap_or_else(|| ".".into()));
    let seconds: f64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(5.0);
    let mut lines = Vec::new();
    let passed = match run(&out_dir, seconds, &mut lines) {
        Ok(passed) => passed,
        Err(error) => {
            lines.push(format!("ERROR {error}"));
            false
        }
    };
    lines.push(format!("RESULT {}", if passed { "PASS" } else { "FAIL" }));
    let _ = std::fs::create_dir_all(&out_dir);
    let _ = std::fs::write(
        out_dir.join("capture_probe.txt"),
        lines.join("\r\n") + "\r\n",
    );
    std::process::exit(if passed { 0 } else { 1 });
}

fn run(out_dir: &Path, seconds: f64, lines: &mut Vec<String>) -> Result<bool, String> {
    let mut session = 0;
    let _ = unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &mut session) };
    lines.push(format!("session {session} (session 0 cannot capture)"));
    let dpi = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    lines.push(format!("per-monitor DPI aware: {}", dpi.is_ok()));

    let mut duplicator = Duplicator::new(0).map_err(describe)?;
    let info = duplicator.info().clone();
    lines.push(format!("gpu: {}", info.adapter));
    lines.push(format!(
        "screen: {} {}x{} at {:.2} Hz, rotation {}, {}",
        info.output, info.width, info.height, info.refresh_hz, info.rotation, info.method
    ));

    let frequency = qpc_frequency() as f64;
    let end = qpc_now() + (seconds * frequency) as i64;
    let (mut updates, mut mouse_only, mut timeouts, mut restarts, mut merged) = (0, 0, 0, 0, 0);
    let mut first_frame = None;
    let mut image = None;
    let mut copy_ms = 0.0;
    let mut delays_ms = Vec::new();
    while qpc_now() < end {
        let frame = match duplicator.acquire(100) {
            Ok(Some(frame)) => frame,
            Ok(None) => {
                timeouts += 1;
                continue;
            }
            Err(error) if needs_restart(&error) => {
                restarts += 1;
                duplicator = Duplicator::new(0).map_err(describe)?;
                continue;
            }
            Err(error) => return Err(describe(error)),
        };
        // Run 1 copied the first frame, a mouse-only update, and it was all black. Record what the first
        // frame holds, and take the thumbnail from the first real screen update.
        if first_frame.is_none() {
            let kind = if frame.present_qpc == 0 {
                "mouse-only"
            } else {
                "screen update"
            };
            let first = duplicator.copy_to_cpu(&frame).map_err(describe)?;
            first_frame = Some((kind, brightness(&first).1));
        }
        if frame.present_qpc == 0 {
            mouse_only += 1;
            continue;
        }
        updates += 1;
        merged += frame.accumulated.saturating_sub(1);
        if image.is_none() {
            let started = qpc_now();
            image = Some(duplicator.copy_to_cpu(&frame).map_err(describe)?);
            copy_ms = (qpc_now() - started) as f64 * 1000.0 / frequency;
        }
        // Skip the first update: its present time can be from before the probe started.
        if updates > 1 {
            delays_ms.push((frame.acquired_qpc - frame.present_qpc) as f64 * 1000.0 / frequency);
        }
    }
    lines.push(format!(
        "{seconds:.1} s: {updates} screen updates ({:.1}/s), {mouse_only} mouse-only, \
         {timeouts} waits of 100 ms with no change, {merged} merged, {restarts} restarts",
        f64::from(updates) / seconds
    ));
    if !delays_ms.is_empty() {
        delays_ms.sort_by(f64::total_cmp);
        let pick = |q: f64| delays_ms[((delays_ms.len() - 1) as f64 * q).round() as usize];
        lines.push(format!(
            "screen update -> acquired, ms: median {:.2}, p95 {:.2}, max {:.2}",
            pick(0.5),
            pick(0.95),
            pick(1.0)
        ));
    }

    if let Some((kind, lit)) = first_frame {
        lines.push(format!(
            "first frame: {kind}, non-black pixels {:.1} %",
            lit * 100.0
        ));
    }
    let Some(image) = image else {
        lines.push("no screen update during the run (nothing changed on screen?)".into());
        return Ok(false);
    };
    lines.push(format!(
        "copy to memory: {copy_ms:.2} ms for {}x{}",
        image.width, image.height
    ));
    let (mean_luma, lit) = brightness(&image);
    lines.push(format!(
        "mean brightness {mean_luma:.1} / 255, non-black pixels {:.1} %",
        lit * 100.0
    ));
    let (thumb_width, thumb_height, bgr) = thumbnail(&image, 480);
    let thumb_path = out_dir.join("capture_thumb.bmp");
    std::fs::create_dir_all(out_dir).map_err(|e| e.to_string())?;
    write_bmp(&thumb_path, thumb_width, thumb_height, &bgr).map_err(|e| e.to_string())?;
    lines.push(format!(
        "thumbnail: {} ({thumb_width}x{thumb_height})",
        thumb_path.display()
    ));
    let size = (image.width, image.height);
    let size_ok = size == (info.width, info.height) || size == (info.height, info.width);
    lines.push(format!("frame size matches the screen mode: {size_ok}"));
    Ok(size_ok && lit > 0.01)
}

fn describe(error: windows::core::Error) -> String {
    format!("0x{:08X} {}", error.code().0, error.message())
}

/// Mean luma (0-255) and the fraction of pixels that are not black.
fn brightness(image: &BgraImage) -> (f64, f64) {
    let mut luma_sum = 0u64;
    let mut lit = 0u64;
    for &[b, g, r, _] in image.pixels.as_chunks::<4>().0 {
        let (b, g, r) = (u64::from(b), u64::from(g), u64::from(r));
        luma_sum += (299 * r + 587 * g + 114 * b) / 1000;
        if r + g + b > 24 {
            lit += 1;
        }
    }
    let count = (image.pixels.len() / 4).max(1) as f64;
    (luma_sum as f64 / count, lit as f64 / count)
}

/// Box-filtered copy at most `max_width` wide: BGR, 3 bytes per pixel, top row first.
fn thumbnail(image: &BgraImage, max_width: u32) -> (u32, u32, Vec<u8>) {
    let factor = image.width.div_ceil(max_width).max(1) as usize;
    let width = image.width as usize / factor;
    let height = image.height as usize / factor;
    let stride = image.width as usize * 4;
    let mut bgr = Vec::with_capacity(width * height * 3);
    for ty in 0..height {
        for tx in 0..width {
            let mut sum = [0usize; 3];
            for y in ty * factor..(ty + 1) * factor {
                let row = &image.pixels[y * stride..];
                for x in tx * factor..(tx + 1) * factor {
                    sum[0] += usize::from(row[x * 4]);
                    sum[1] += usize::from(row[x * 4 + 1]);
                    sum[2] += usize::from(row[x * 4 + 2]);
                }
            }
            bgr.extend(sum.map(|s| (s / (factor * factor)) as u8));
        }
    }
    (width as u32, height as u32, bgr)
}

/// 24-bit BMP: rows stored bottom-up, each padded to 4 bytes.
fn write_bmp(path: &Path, width: u32, height: u32, bgr: &[u8]) -> std::io::Result<()> {
    let row_size = (width * 3).div_ceil(4) * 4;
    let data_size = row_size * height;
    let mut out = Vec::with_capacity(54 + data_size as usize);
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&(54 + data_size).to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&54u32.to_le_bytes());
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&width.to_le_bytes());
    out.extend_from_slice(&height.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&24u16.to_le_bytes());
    out.extend_from_slice(&[0; 24]);
    let row_bytes = width as usize * 3;
    for row in bgr.chunks_exact(row_bytes).rev() {
        out.extend_from_slice(row);
        out.resize(out.len() + (row_size as usize - row_bytes), 0);
    }
    std::fs::write(path, out)
}
