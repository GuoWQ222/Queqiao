//! M1 check for the encoders: H.264 encoding of the screen in real time, with every path selectable.
//!
//! Captures one desktop image, then for each frame scrolls it a little (a stand-in for scrolling a page,
//! so every frame changes), converts BGRA -> NV12 and encodes it at the screen's frame rate (at most 60
//! by default). The encoder is whatever the machine offers: a hardware encoder of any vendor first,
//! OpenH264 on the CPU otherwise. Runs in the desktop session via scripts\run_in_session.ps1 (no
//! console window).
//!
//! Usage: encode_probe <out_dir> [frames=N] [fps=N] [mbps=N] [force=STAGE:CHOICE,...]
//!   force=encode:software   OpenH264 even when a hardware encoder exists
//!   force=encode:hardware   hardware only (fails without one)
//!   force=adapter:warp      convert and encode on Microsoft's software adapter (WARP): a PC without a
//!                           GPU, as far as everything after capture is concerned
//!   force=convert:cpu       BGRA -> NV12 on the CPU instead of the GPU video processor
//!   throttling=on           keep Windows power throttling (EcoQoS). By default the probe opts out, as the
//!                           agent will: Windows otherwise runs a windowless process on slow efficiency
//!                           cores (measured on a 12th-gen Intel laptop: OpenH264 2.7x slower)
//! Writes <out_dir>\encode_probe.txt (report) and <out_dir>\encode_probe.h264 (the stream).

#![windows_subsystem = "windows"]

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use queqiao_capture::{BgraImage, Duplicator, qpc_frequency, qpc_now};
use queqiao_codec::{
    EncodedFrame, EncoderChoice, EncoderConfig, H264Encoder, Nv12Converter, candidate_sizes,
    nal_types,
};
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_WARP;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::IDXGIAdapter;
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};

/// Software encoders run on the CPU: they start at no more than 1080p and 30 fps. (The agent will adapt
/// to the measured encode time; this is the starting point.)
const SOFTWARE_START: (u32, u32, u32) = (1920, 1080, 30);

struct Request {
    screen: (u32, u32),
    fps: u32,
    mbps: Option<u32>,
    choice: EncoderChoice,
    /// Largest size to try (long side, short side).
    limit: (u32, u32),
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
        // Same as setting QUEQIAO_FORCE before starting (a scheduled task cannot set it). Nothing else
        // runs yet, so changing the environment here is safe.
        unsafe { std::env::set_var("QUEQIAO_FORCE", spec) };
    }
    if let Some(preset) = options.get("nvenc_preset") {
        // NVENC preset P1..P7 for measurements (see nvenc.rs).
        unsafe { std::env::set_var("QUEQIAO_NVENC_PRESET", preset) };
    }
    let number = |key: &str| options.get(key).and_then(|v| v.parse::<u32>().ok());
    let mut lines = Vec::new();
    if options.get("throttling").map(String::as_str) == Some("on") {
        lines.push("power throttling: Windows default (throttling=on)".into());
    } else {
        let result = disable_power_throttling();
        lines.push(format!(
            "power throttling opt-out: {}",
            result.map_or_else(|e| e, |()| "ok".into())
        ));
    }
    let passed = match run(
        &out_dir,
        number("frames").unwrap_or(300),
        number("fps"),
        number("mbps"),
        &mut lines,
    ) {
        Ok(passed) => passed,
        Err(error) => {
            lines.push(format!("ERROR {error}"));
            false
        }
    };
    lines.push(format!("RESULT {}", if passed { "PASS" } else { "FAIL" }));
    let _ = std::fs::create_dir_all(&out_dir);
    let report = lines.join("\r\n") + "\r\n";
    let _ = std::fs::write(out_dir.join("encode_probe.txt"), report);
    std::process::exit(if passed { 0 } else { 1 });
}

fn run(
    out_dir: &Path,
    frames: u32,
    fps: Option<u32>,
    mbps: Option<u32>,
    lines: &mut Vec<String>,
) -> Result<bool, String> {
    let _ = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    let force = std::env::var("QUEQIAO_FORCE").unwrap_or_default();
    lines.push(format!(
        "forced: {}",
        if force.is_empty() { "nothing" } else { &force }
    ));
    let mut duplicator = Duplicator::primary().map_err(describe)?;
    let info = duplicator.info().clone();
    let screen_fps = (info.refresh_hz.round() as u32).clamp(1, 60);
    lines.push(format!(
        "screen: {} {}x{} at {:.0} Hz on {}",
        info.output, info.width, info.height, info.refresh_hz, info.adapter
    ));

    // One real desktop image (the first frame after starting can be an empty mouse-only update).
    let frequency = qpc_frequency() as f64;
    let deadline = qpc_now() + (10.0 * frequency) as i64;
    let image = loop {
        if qpc_now() > deadline {
            return Err("no screen update within 10 s".into());
        }
        if let Some(frame) = duplicator.acquire(100).map_err(describe)?
            && frame.present_qpc != 0
        {
            break duplicator.copy_to_cpu(&frame).map_err(describe)?;
        }
    };
    duplicator.release().map_err(describe)?;

    // The device that converts and encodes: the capturing GPU, or WARP when forced.
    let warp = queqiao_core::forced("adapter").as_deref() == Some("warp");
    let (device, context) = if warp {
        warp_device(lines)?
    } else {
        (duplicator.device().clone(), duplicator.context().clone())
    };
    lines.push(format!(
        "convert + encode on: {}",
        if warp {
            "Microsoft WARP (software adapter)"
        } else {
            &info.adapter
        }
    ));
    let source = upload(&device, &image)?;
    let (width, height) = (image.width, image.height);

    let mut request = Request {
        screen: (width, height),
        fps: fps.unwrap_or(screen_fps),
        mbps,
        choice: EncoderChoice::from_env(),
        limit: (u32::MAX, u32::MAX),
    };
    let (mut encoder, mut config) = open_encoder(&device, &context, &request, lines)?;
    if !encoder.is_hardware() {
        let (long, short, max_fps) = SOFTWARE_START;
        request.limit = (long, short);
        request.fps = fps.unwrap_or(request.fps.min(max_fps));
        request.choice = EncoderChoice::Software;
        let fits = config.width.max(config.height) <= long
            && config.width.min(config.height) <= short
            && config.fps <= request.fps;
        if !fits {
            drop(encoder);
            (encoder, config) = open_encoder(&device, &context, &request, lines)?;
        }
    }
    for reason in encoder.skipped() {
        lines.push(format!("skipped: {reason}"));
    }
    lines.push(format!(
        "encoder: {} ({}) at {}x{}, {} fps, {:.1} Mbit/s",
        encoder.name(),
        if encoder.is_hardware() {
            "hardware"
        } else {
            "software"
        },
        config.width,
        config.height,
        config.fps,
        f64::from(config.bitrate) / 1e6
    ));
    if !encoder.refused_settings().is_empty() {
        let refused = encoder.refused_settings().join(", ");
        lines.push(format!(
            "settings the encoder refused (it uses its defaults): {refused}"
        ));
    }

    let fps = config.fps;
    let size = (config.width, config.height);
    let mut converter = Nv12Converter::new(&device, &context, (width, height), size, fps)
        .map_err(|e| format!("BGRA -> NV12 converter: {}", describe(e)))?;
    lines.push(format!("convert: {}", converter.description()));
    let ring = (0..4)
        .map(|_| converter.create_output(&device))
        .collect::<windows::core::Result<Vec<_>>>()
        .map_err(|e| format!("NV12 textures: {}", describe(e)))?;
    let scene = upload(&device, &image)?;
    let step = (height / 180).max(2);

    let interval = frequency / f64::from(fps);
    let duration = 10_000_000 / i64::from(fps);
    let mut submitted: HashMap<i64, (i64, i64)> = HashMap::new();
    let mut outputs: Vec<(EncodedFrame, i64)> = Vec::new();
    let mut late = 0;
    let start = qpc_now();
    for i in 0..frames {
        let target = start + (f64::from(i) * interval) as i64;
        while qpc_now() < target {
            collect(&mut encoder, &mut outputs)?;
            std::thread::yield_now();
        }
        let began = qpc_now();
        scroll(
            &context,
            &source,
            &scene,
            (width, height),
            (i * step) % height,
        );
        let nv12 = &ring[i as usize % ring.len()];
        converter.convert(&scene, nv12).map_err(describe)?;
        while !encoder.wants_input() {
            collect(&mut encoder, &mut outputs)?;
            if (qpc_now() - began) as f64 > frequency * 0.5 {
                return Err(format!(
                    "the encoder asked for no input for 500 ms (frame {i})"
                ));
            }
            std::thread::yield_now();
        }
        let time = i64::from(i) * duration;
        let submit = qpc_now();
        if (submit - target) as f64 > interval {
            late += 1;
        }
        encoder.encode(nv12, time, i == 0).map_err(describe)?;
        submitted.insert(time, (began, submit));
        collect(&mut encoder, &mut outputs)?;
    }
    encoder.drain().map_err(describe)?;
    let drain_deadline = qpc_now() + (2.0 * frequency) as i64;
    while !encoder.drained() && qpc_now() < drain_deadline {
        collect(&mut encoder, &mut outputs)?;
        std::thread::yield_now();
    }
    collect(&mut encoder, &mut outputs)?;
    let elapsed = (qpc_now() - start) as f64 / frequency;

    // Report.
    let ms = |ticks: i64| ticks as f64 * 1000.0 / frequency;
    let mut encode_ms = Vec::new();
    let mut total_ms = Vec::new();
    for (frame, arrived) in &outputs {
        if let Some((began, submit)) = submitted.get(&frame.time) {
            encode_ms.push(ms(arrived - submit));
            total_ms.push(ms(arrived - began));
        }
    }
    let keyframes = outputs.iter().filter(|(f, _)| f.keyframe).count();
    let bytes: usize = outputs.iter().map(|(f, _)| f.data.len()).sum();
    let mut delta_kb: Vec<f64> = outputs
        .iter()
        .filter(|(f, _)| !f.keyframe)
        .map(|(f, _)| f.data.len() as f64 / 1024.0)
        .collect();
    let seconds = f64::from(frames) / f64::from(fps);
    lines.push(format!(
        "{frames} frames in, {} out ({keyframes} keyframes), {late} submitted late, {:.1} s for {:.1} s of video",
        outputs.len(),
        elapsed,
        seconds
    ));
    lines.push(format!(
        "encode time (frame in -> H.264 out), ms: {}",
        summary(&mut encode_ms)
    ));
    lines.push(format!(
        "scroll + convert + encode, ms: {}",
        summary(&mut total_ms)
    ));
    let first = outputs.first().map(|(f, _)| f);
    if let Some(first) = first {
        lines.push(format!(
            "first frame: {:.1} KB, keyframe {}, NAL types {:?}",
            first.data.len() as f64 / 1024.0,
            first.keyframe,
            nal_types(&first.data)
        ));
    }
    lines.push(format!("other frames, KB: {}", summary(&mut delta_kb)));
    if let Some(timing) = encoder.software_timing() {
        let n = f64::from(timing.frames.max(1));
        lines.push(format!(
            "software time per frame, ms: GPU -> memory + U/V split {:.2}, OpenH264 {:.2}",
            timing.copy_ms / n,
            timing.encode_ms / n
        ));
    }
    lines.push(format!(
        "stream: {:.2} MB, {:.1} Mbit/s average",
        bytes as f64 / 1e6,
        bytes as f64 * 8.0 / seconds / 1e6
    ));

    std::fs::create_dir_all(out_dir).map_err(|e| e.to_string())?;
    let stream: Vec<u8> = outputs.iter().flat_map(|(f, _)| f.data.clone()).collect();
    let stream_path = out_dir.join("encode_probe.h264");
    std::fs::write(&stream_path, &stream).map_err(|e| e.to_string())?;
    lines.push(format!("stream file: {}", stream_path.display()));

    let starts_clean = first.is_some_and(|f| {
        let types = nal_types(&f.data);
        f.keyframe && types.contains(&7) && types.contains(&8) && types.contains(&5)
    });
    encode_ms.sort_by(f64::total_cmp);
    let keeps_up = percentile(&encode_ms, 0.95) < 1000.0 / f64::from(fps);
    let all_out = outputs.len() == frames as usize;
    lines.push(format!(
        "checks: all frames out {all_out}, starts with SPS+PPS+IDR {starts_clean}, p95 encode < frame time {keeps_up}"
    ));
    Ok(all_out && starts_clean && keeps_up)
}

/// The largest candidate size within `request.limit` that an encoder of `request.choice` accepts.
fn open_encoder(
    device: &ID3D11Device,
    context: &ID3D11DeviceContext,
    request: &Request,
    lines: &mut Vec<String>,
) -> Result<(H264Encoder, EncoderConfig), String> {
    let (screen_w, screen_h) = request.screen;
    for (w, h) in candidate_sizes(screen_w, screen_h) {
        if w.max(h) > request.limit.0 || w.min(h) > request.limit.1 {
            continue;
        }
        let mut config = EncoderConfig::for_screen(w, h, request.fps);
        if let Some(mbps) = request.mbps {
            config.bitrate = mbps * 1_000_000;
        }
        match H264Encoder::new(device, context, &config, request.choice) {
            Ok(encoder) => return Ok((encoder, config)),
            Err(error) => lines.push(format!("{w}x{h} refused: {}", error.message())),
        }
    }
    Err("no encoder accepted any size".into())
}

fn collect(
    encoder: &mut H264Encoder,
    outputs: &mut Vec<(EncodedFrame, i64)>,
) -> Result<(), String> {
    let mut frames = Vec::new();
    encoder.poll(&mut frames).map_err(describe)?;
    let now = qpc_now();
    outputs.extend(frames.into_iter().map(|f| (f, now)));
    Ok(())
}

/// Asks Windows not to power-throttle this process (no EcoQoS: full clock speed, any core).
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

/// Microsoft's software Direct3D 11 adapter (WARP), as on a PC without a usable GPU. Tries with video
/// support first (the converter needs it), then without, and reports both attempts.
fn warp_device(lines: &mut Vec<String>) -> Result<(ID3D11Device, ID3D11DeviceContext), String> {
    let with_video = D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT;
    for (label, flags) in [
        ("with video support", with_video),
        ("without video support", D3D11_CREATE_DEVICE_BGRA_SUPPORT),
    ] {
        let mut device = None;
        let mut context = None;
        let created = unsafe {
            D3D11CreateDevice(
                None::<&IDXGIAdapter>,
                D3D_DRIVER_TYPE_WARP,
                HMODULE::default(),
                flags,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )
        };
        match (created, device, context) {
            (Ok(()), Some(device), Some(context)) => {
                lines.push(format!("WARP device {label}: created"));
                return Ok((device, context));
            }
            (Err(error), _, _) => lines.push(format!("WARP device {label}: {}", describe(error))),
            _ => lines.push(format!("WARP device {label}: nothing returned")),
        }
    }
    Err("no WARP device".into())
}

/// A BGRA texture on `device` holding `image`, usable by the video processor.
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

/// Copies `source` into `scene` moved up by `offset` rows, wrapping around (looks like a scrolling page).
fn scroll(
    context: &ID3D11DeviceContext,
    source: &ID3D11Texture2D,
    scene: &ID3D11Texture2D,
    (width, height): (u32, u32),
    offset: u32,
) {
    let lower = D3D11_BOX {
        left: 0,
        top: offset,
        front: 0,
        right: width,
        bottom: height,
        back: 1,
    };
    let upper = D3D11_BOX {
        bottom: offset,
        top: 0,
        ..lower
    };
    unsafe {
        context.CopySubresourceRegion(scene, 0, 0, 0, 0, source, 0, Some(&lower));
        if offset > 0 {
            context.CopySubresourceRegion(scene, 0, 0, height - offset, 0, source, 0, Some(&upper));
        }
    }
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
