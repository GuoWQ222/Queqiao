//! M1 check: the whole local loop on one PC - capture -> convert -> encode -> decode -> window - with the
//! delay of each stage measured. The window shows the screen it is on, so it captures itself: the screen
//! keeps changing and the loop runs continuously (a hall-of-mirrors picture is expected).
//!
//! The first picture comes from a GDI screenshot, so it appears even when nothing on screen changes.
//! Everything else follows the machine: encoder and decoder of any vendor or OpenH264, GPU or CPU
//! conversion, flip or blit swap chain. Runs in the desktop session via scripts\run_in_session.ps1; the
//! window closes by itself.
//!
//! Usage: loopback <out_dir> [seconds=8] [fps=N] [width=960] [force=STAGE:CHOICE,...] [throttling=on]
//!   force: encode:software|hardware, decode:software|hardware, convert:cpu, adapter:warp
//! Writes <out_dir>\loopback.txt and <out_dir>\first_picture.bmp (the first decoded picture, 480 wide).

#![windows_subsystem = "windows"]

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use queqiao_capture::{BgraImage, Duplicator, needs_restart, qpc_frequency, qpc_now};
use queqiao_codec::{
    DecodedFrame, DecoderChoice, EncodedFrame, EncoderChoice, EncoderConfig, H264Decoder,
    H264Encoder, Nv12Converter, candidate_sizes,
};
use queqiao_display::{Presenter, Window, nv12_to_bgra};
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_WARP;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::IDXGIAdapter;
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};

/// Software encoders start at no more than 1080p and 30 fps (the agent will adapt from there).
const SOFTWARE_START: (u32, u32, u32) = (1920, 1080, 30);

/// When each stage happened for one encoded frame (QueryPerformanceCounter ticks).
struct Timing {
    /// When the change reached the screen; 0 for the GDI snapshot (unknown).
    screen: i64,
    captured: i64,
    submitted: i64,
}

#[derive(Default)]
struct Stages {
    screen_to_captured: Vec<f64>,
    captured_to_submitted: Vec<f64>,
    encode: Vec<f64>,
    decode: Vec<f64>,
    show: Vec<f64>,
    total: Vec<f64>,
}

fn main() {
    let mut args = std::env::args().skip(1);
    let out_dir = PathBuf::from(args.next().unwrap_or_else(|| ".".into()));
    let options: HashMap<String, String> = args
        .filter_map(|a| {
            a.split_once('=')
                .map(|(k, v)| (k.to_string(), v.to_string()))
        })
        .collect();
    if let Some(spec) = options.get("force") {
        // Before anything else runs: same as setting QUEQIAO_FORCE in the environment.
        unsafe { std::env::set_var("QUEQIAO_FORCE", spec) };
    }
    let mut lines = Vec::new();
    let passed = match run(&out_dir, &options, &mut lines) {
        Ok(passed) => passed,
        Err(error) => {
            lines.push(format!("ERROR {error}"));
            false
        }
    };
    lines.push(format!("RESULT {}", if passed { "PASS" } else { "FAIL" }));
    let _ = std::fs::create_dir_all(&out_dir);
    let report = lines.join("\r\n") + "\r\n";
    let _ = std::fs::write(out_dir.join("loopback.txt"), report);
    std::process::exit(if passed { 0 } else { 1 });
}

fn run(
    out_dir: &Path,
    options: &HashMap<String, String>,
    lines: &mut Vec<String>,
) -> Result<bool, String> {
    let number = |key: &str| options.get(key).and_then(|v| v.parse::<u32>().ok());
    if options.get("throttling").map(String::as_str) != Some("on") {
        disable_power_throttling()?;
    }
    let _ = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    let force = std::env::var("QUEQIAO_FORCE").unwrap_or_default();
    lines.push(format!(
        "forced: {}",
        if force.is_empty() { "nothing" } else { &force }
    ));

    let mut duplicator = Duplicator::primary().map_err(describe)?;
    let info = duplicator.info().clone();
    let frequency = qpc_frequency() as f64;
    let start = qpc_now();
    let warp = queqiao_core::forced("adapter").as_deref() == Some("warp");
    let (device, context) = if warp {
        warp_device()?
    } else {
        (duplicator.device().clone(), duplicator.context().clone())
    };
    lines.push(format!(
        "screen: {}x{} at {:.0} Hz on {}; pipeline on {}",
        info.width,
        info.height,
        info.refresh_hz,
        info.adapter,
        if warp {
            "Microsoft WARP (no GPU)"
        } else {
            &info.adapter
        }
    ));

    // The first picture: a GDI screenshot, so a still screen is not a blank window.
    let snapshot = duplicator
        .snapshot_image()
        .map_err(|e| format!("snapshot: {}", describe(e)))?;
    let (width, height) = (snapshot.width, snapshot.height);
    let latest = upload(&device, &snapshot)?;

    let window_width = number("width").unwrap_or(960) as i32;
    let window_height = (i64::from(window_width) * i64::from(height) / i64::from(width)) as i32;
    let mut window = Window::new(
        "Queqiao M1 loopback (closes by itself)",
        window_width,
        window_height,
    )
    .map_err(|e| format!("window: {}", describe(e)))?;
    let mut presenter = Presenter::new(&device, &context, &window)
        .map_err(|e| format!("presenter: {}", describe(e)))?;

    let wanted_fps = number("fps").unwrap_or((info.refresh_hz.round() as u32).clamp(1, 60));
    let (mut encoder, config) = open_encoder(
        &device,
        &context,
        (width, height),
        wanted_fps,
        number("fps"),
        lines,
    )?;
    let fps = config.fps;
    let size = (config.width, config.height);
    let mut converter = Nv12Converter::new(&device, &context, (width, height), size, fps)
        .map_err(|e| format!("converter: {}", describe(e)))?;
    let ring = (0..4)
        .map(|_| converter.create_output(&device))
        .collect::<windows::core::Result<Vec<_>>>()
        .map_err(|e| format!("NV12 textures: {}", describe(e)))?;
    let mut decoder = H264Decoder::new(&device, &context, DecoderChoice::from_env())
        .map_err(|e| format!("decoder: {}", describe(e)))?;
    lines.push(format!(
        "encoder: {} ({}) at {}x{}, {fps} fps, {:.1} Mbit/s",
        encoder.name(),
        if encoder.is_hardware() {
            "hardware"
        } else {
            "software"
        },
        size.0,
        size.1,
        f64::from(config.bitrate) / 1e6
    ));
    lines.push(format!("convert: {}", converter.description()));
    lines.push(format!("decoder: {}", decoder.name()));

    let seconds = f64::from(number("seconds").unwrap_or(8));
    let end = start + (seconds * frequency) as i64;
    let interval = (frequency / f64::from(fps)) as i64;
    let duration = 10_000_000 / i64::from(fps);
    let mut next_tick = qpc_now();
    // The snapshot is the first thing to send; its screen time is unknown.
    let mut waiting: Option<(i64, i64)> = Some((0, qpc_now()));
    let mut timings: HashMap<i64, Timing> = HashMap::new();
    let mut stages = Stages::default();
    let (mut index, mut captured, mut shown) = (0u32, 0u32, 0u32);
    let mut first_shown: Option<i64> = None;
    let mut encoded: Vec<EncodedFrame> = Vec::new();
    let mut decoded: Vec<DecodedFrame> = Vec::new();
    let mut thumbnail_saved = false;
    let ms = |ticks: i64| ticks as f64 * 1000.0 / frequency;

    while qpc_now() < end {
        if !window.pump() {
            lines.push("the window was closed".into());
            break;
        }
        // Capture: wait for a screen change, at most until the next frame is due.
        let wait = ms(next_tick - qpc_now()).clamp(0.0, 16.0) as u32;
        match duplicator.acquire(wait) {
            Ok(Some(frame)) if frame.present_qpc != 0 => {
                captured += 1;
                if warp {
                    let image = duplicator.copy_to_cpu(&frame).map_err(describe)?;
                    let pixels = image.pixels.as_ptr().cast();
                    unsafe {
                        context.UpdateSubresource(&latest, 0, None, pixels, image.width * 4, 0)
                    };
                } else {
                    unsafe { context.CopyResource(&latest, &frame.texture) };
                }
                waiting = Some((frame.present_qpc, frame.acquired_qpc));
            }
            Ok(_) => {}
            Err(error) if needs_restart(&error) => {
                return Err("the desktop switched (login screen, UAC or a mode change)".into());
            }
            Err(error) => return Err(describe(error)),
        }
        // Encode the newest picture once per frame time.
        let now = qpc_now();
        if now >= next_tick {
            next_tick = if now - next_tick > interval {
                now + interval
            } else {
                next_tick + interval
            };
            encoder.poll(&mut encoded).map_err(describe)?;
            if let Some((screen, captured_at)) = waiting
                && encoder.wants_input()
            {
                waiting = None;
                let submitted = qpc_now();
                let nv12 = &ring[index as usize % ring.len()];
                converter.convert(&latest, nv12).map_err(describe)?;
                let time = i64::from(index) * duration;
                encoder.encode(nv12, time, index == 0).map_err(describe)?;
                timings.insert(
                    time,
                    Timing {
                        screen,
                        captured: captured_at,
                        submitted,
                    },
                );
                index += 1;
                // Wait for this frame's H.264 (hardware encoders deliver it asynchronously).
                let deadline = submitted + (frequency * 0.1) as i64;
                while encoded.is_empty() && qpc_now() < deadline {
                    encoder.poll(&mut encoded).map_err(describe)?;
                    std::thread::yield_now();
                }
            }
        }
        // Decode and show what the encoder has produced.
        encoder.poll(&mut encoded).map_err(describe)?;
        for frame in std::mem::take(&mut encoded) {
            let encoded_at = qpc_now();
            decoder
                .decode(&frame.data, frame.time, &mut decoded)
                .map_err(|e| format!("decode: {}", describe(e)))?;
            let decoded_at = qpc_now();
            for picture in decoded.drain(..) {
                presenter
                    .show(&picture.texture, (picture.width, picture.height), &window)
                    .map_err(|e| format!("show: {}", describe(e)))?;
                let shown_at = qpc_now();
                shown += 1;
                first_shown.get_or_insert(shown_at);
                if !thumbnail_saved {
                    thumbnail_saved = true;
                    save_thumbnail(
                        &device,
                        &context,
                        &picture,
                        &out_dir.join("first_picture.bmp"),
                    )?;
                }
                let Some(timing) = timings.remove(&picture.time) else {
                    continue;
                };
                if timing.screen != 0 {
                    stages
                        .screen_to_captured
                        .push(ms(timing.captured - timing.screen));
                    stages.total.push(ms(shown_at - timing.screen));
                }
                stages
                    .captured_to_submitted
                    .push(ms(timing.submitted - timing.captured));
                stages.encode.push(ms(encoded_at - timing.submitted));
                stages.decode.push(ms(decoded_at - encoded_at));
                stages.show.push(ms(shown_at - decoded_at));
            }
        }
    }
    let elapsed = ms(qpc_now() - start) / 1000.0;

    lines.push(format!(
        "decoding ran on: {}",
        if decoder.is_hardware() { "GPU" } else { "CPU" }
    ));
    lines.push(format!("present: {}", presenter.description()));
    let first = first_shown.map(|t| ms(t - start));
    lines.push(format!(
        "first picture shown {} ms after start (GDI snapshot)",
        first.map_or("never".to_string(), |v| format!("{v:.0}"))
    ));
    lines.push(format!(
        "{elapsed:.1} s: {captured} screen changes captured, {index} frames encoded, {shown} shown ({:.1} per second)",
        f64::from(shown) / elapsed
    ));
    lines.push(format!(
        "screen change -> captured, ms: {}",
        summary(&mut stages.screen_to_captured)
    ));
    lines.push(format!(
        "captured -> conversion starts (waiting for the frame time), ms: {}",
        summary(&mut stages.captured_to_submitted)
    ));
    lines.push(format!(
        "convert + encode, ms: {}",
        summary(&mut stages.encode)
    ));
    lines.push(format!("decode, ms: {}", summary(&mut stages.decode)));
    lines.push(format!(
        "show (convert + present), ms: {}",
        summary(&mut stages.show)
    ));
    lines.push(format!(
        "screen change -> shown, ms: {}",
        summary(&mut stages.total)
    ));
    lines.push(format!(
        "first picture: {}",
        out_dir.join("first_picture.bmp").display()
    ));
    // The window captures itself, so the screen changes only after a picture is shown and the display
    // refreshes: the frame rate here follows the display's refresh rate and the loop's delay (measured: 52
    // per second on a 320 Hz screen, 27 on a 60 Hz one with every change shown). Throughput at a fixed rate
    // is checked by encode_probe; this loop checks the first picture, losses and the delay.
    let first_ok = first.is_some_and(|v| v <= 1000.0);
    let nothing_lost = shown + 2 >= index;
    let median_total = percentile(&stages.total, 0.5);
    let delay_ok = median_total < 50.0;
    lines.push(format!(
        "checks: first picture within 1 s {first_ok}, every encoded frame shown {nothing_lost}, median delay < 50 ms {delay_ok}"
    ));
    Ok(first_ok && nothing_lost && delay_ok)
}

/// The largest candidate size an encoder accepts; software encoders start at `SOFTWARE_START`.
fn open_encoder(
    device: &ID3D11Device,
    context: &ID3D11DeviceContext,
    screen: (u32, u32),
    fps: u32,
    fps_option: Option<u32>,
    lines: &mut Vec<String>,
) -> Result<(H264Encoder, EncoderConfig), String> {
    let mut choice = EncoderChoice::from_env();
    let mut limit = (u32::MAX, u32::MAX);
    let mut fps = fps;
    for _attempt in 0..2 {
        for (w, h) in candidate_sizes(screen.0, screen.1) {
            if w.max(h) > limit.0 || w.min(h) > limit.1 {
                continue;
            }
            let config = EncoderConfig::for_screen(w, h, fps);
            match H264Encoder::new(device, context, &config, choice) {
                Ok(encoder) if encoder.is_hardware() || limit.0 != u32::MAX => {
                    return Ok((encoder, config));
                }
                Ok(encoder) => {
                    // Software: reopen within the software starting limits.
                    for reason in encoder.skipped() {
                        lines.push(format!("skipped: {reason}"));
                    }
                    let (long, short, max_fps) = SOFTWARE_START;
                    limit = (long, short);
                    fps = fps_option.unwrap_or(fps.min(max_fps));
                    choice = EncoderChoice::Software;
                    break;
                }
                Err(error) => lines.push(format!("{w}x{h} refused: {}", error.message())),
            }
        }
    }
    Err("no encoder accepted any size".into())
}

/// Saves a decoded NV12 picture as a 480-wide BMP (through the CPU colour conversion).
fn save_thumbnail(
    device: &ID3D11Device,
    context: &ID3D11DeviceContext,
    picture: &DecodedFrame,
    path: &Path,
) -> Result<(), String> {
    let mut desc = D3D11_TEXTURE2D_DESC::default();
    unsafe { picture.texture.GetDesc(&mut desc) };
    desc.Usage = D3D11_USAGE_STAGING;
    desc.BindFlags = 0;
    desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
    desc.MiscFlags = 0;
    let mut staging = None;
    unsafe { device.CreateTexture2D(&desc, None, Some(&mut staging)) }.map_err(describe)?;
    let staging = staging.ok_or("no staging texture")?;
    let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
    unsafe {
        context.CopyResource(&staging, &picture.texture);
        context
            .Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
            .map_err(describe)?;
    }
    let (w, h) = (picture.width as usize, picture.height as usize);
    let pitch = mapped.RowPitch as usize;
    let nv12 = unsafe { std::slice::from_raw_parts(mapped.pData.cast::<u8>(), pitch * h * 3 / 2) };
    let (tw, th) = (480, (480 * h / w).max(1));
    let mut bgra = vec![0u8; tw * th * 4];
    nv12_to_bgra(nv12, pitch, (w, h), &mut bgra, (tw, th));
    unsafe { context.Unmap(&staging, 0) };
    let bgr: Vec<u8> = bgra
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|p| [p[0], p[1], p[2]])
        .collect();
    std::fs::create_dir_all(path.parent().unwrap_or(Path::new("."))).map_err(|e| e.to_string())?;
    write_bmp(path, tw as u32, th as u32, &bgr).map_err(|e| e.to_string())
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

fn upload(device: &ID3D11Device, image: &BgraImage) -> Result<ID3D11Texture2D, String> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: image.width,
        Height: image.height,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let data = D3D11_SUBRESOURCE_DATA {
        pSysMem: image.pixels.as_ptr().cast(),
        SysMemPitch: image.width * 4,
        SysMemSlicePitch: 0,
    };
    let mut out = None;
    unsafe { device.CreateTexture2D(&desc, Some(&data), Some(&mut out)) }.map_err(describe)?;
    out.ok_or_else(|| "CreateTexture2D returned nothing".to_string())
}

fn warp_device() -> Result<(ID3D11Device, ID3D11DeviceContext), String> {
    let mut device = None;
    let mut context = None;
    unsafe {
        D3D11CreateDevice(
            None::<&IDXGIAdapter>,
            D3D_DRIVER_TYPE_WARP,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )
    }
    .map_err(|e| format!("WARP device: {}", describe(e)))?;
    match (device, context) {
        (Some(device), Some(context)) => Ok((device, context)),
        _ => Err("WARP device not created".into()),
    }
}

/// Asks Windows not to power-throttle this process (the agent will do the same).
fn disable_power_throttling() -> Result<(), String> {
    use windows::Win32::System::Threading::{
        GetCurrentProcess, PROCESS_POWER_THROTTLING_CURRENT_VERSION,
        PROCESS_POWER_THROTTLING_EXECUTION_SPEED, PROCESS_POWER_THROTTLING_STATE,
        ProcessPowerThrottling, SetProcessInformation,
    };
    let state = PROCESS_POWER_THROTTLING_STATE {
        Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
        ControlMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
        StateMask: 0,
    };
    unsafe {
        SetProcessInformation(
            GetCurrentProcess(),
            ProcessPowerThrottling,
            (&raw const state).cast(),
            size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
        )
    }
    .map_err(describe)
}

fn percentile(sorted: &[f64], q: f64) -> f64 {
    if sorted.is_empty() {
        return f64::NAN;
    }
    sorted[((sorted.len() - 1) as f64 * q).round() as usize]
}

fn summary(values: &mut [f64]) -> String {
    values.sort_by(f64::total_cmp);
    format!(
        "median {:.2}, p95 {:.2}, max {:.2} (n={})",
        percentile(values, 0.5),
        percentile(values, 0.95),
        percentile(values, 1.0),
        values.len()
    )
}

fn describe(error: windows::core::Error) -> String {
    format!("0x{:08X} {}", error.code().0, error.message())
}
