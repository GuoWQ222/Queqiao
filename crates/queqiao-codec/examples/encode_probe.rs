//! M1 check for the encoders: H.264 encoding of the screen in real time, with every path selectable.
//!
//! Captures one desktop image, then for each frame scrolls it a little (a stand-in for scrolling a page,
//! so every frame changes), converts BGRA -> NV12 and encodes it at the screen's frame rate (at most 60
//! by default). The encoder is whatever the machine offers: a hardware encoder of any vendor first,
//! OpenH264 on the CPU otherwise. Runs in the desktop session via scripts\run_in_session.ps1 (no
//! console window).
//!
//! Usage: encode_probe <out_dir> [frames=N] [fps=N] [mbps=N] [switch=S:MBPS] [force=STAGE:CHOICE,...]
//!   switch_keyframe=1       with switch=: a keyframe right after the change
//!   switch=S:MBPS           change the bitrate to MBPS after S seconds, as the agent does when the bandwidth
//!                           estimate changes (the report gives the stream's rate each second)
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
    Codec, DecodedFrame, EncodeFormat, EncodedFrame, EncoderChoice, EncoderConfig, H264Decoder,
    H264Encoder, Nv12Converter, candidate_sizes, hevc_nal_types, nal_types,
};
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_UNKNOWN, D3D_DRIVER_TYPE_WARP,
};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIAdapter, IDXGIAdapter1, IDXGIDevice, IDXGIFactory1,
};
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};
use windows::core::Interface;

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
    // Media Foundation encoders, for measurements: the rate control mode and how a new bitrate is given (mf.rs).
    if let Some(mode) = options.get("mf_rc") {
        unsafe { std::env::set_var("QUEQIAO_MF_RC", mode) };
    }
    // What NVENC or oneVPL make (force=encode:nvenc or encode:vpl): h264, h264_444, hevc, hevc10, hevc444, hevc444_10.
    // decode=vpl: the stream decoded back with Intel's oneVPL decoder and its first picture compared with the desktop.
    if let Some(decoder) = options.get("decode") {
        unsafe { std::env::set_var("QUEQIAO_PROBE_DECODE", decoder) };
    }
    // adapter=amd|nvidia|intel: the GPU for source=pattern (else the default one).
    if let Some(adapter) = options.get("adapter") {
        unsafe { std::env::set_var("QUEQIAO_PROBE_ADAPTER", adapter) };
    }
    // source=pattern: a test picture instead of the desktop (nothing of the screen is captured).
    if let Some(source) = options.get("source") {
        unsafe { std::env::set_var("QUEQIAO_PROBE_SOURCE", source) };
    }
    // keep_first=N: the first N frames also into encode_probe_first.frames (each after its 4-byte length), small
    // enough to check on another machine (decode_file=).
    if let Some(count) = options.get("keep_first") {
        unsafe { std::env::set_var("QUEQIAO_PROBE_KEEP_FIRST", count) };
    }
    if let Some(format) = options.get("format") {
        unsafe { std::env::set_var("QUEQIAO_ENCODE_FORMAT", format) };
    }
    if let Some(how) = options.get("mf_switch") {
        unsafe { std::env::set_var("QUEQIAO_MF_SWITCH", how) };
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
    let result = if let Some(path) = options.get("decode_file") {
        decode_only(Path::new(path), &mut lines)
    } else {
        run(
            &out_dir,
            number("frames").unwrap_or(300),
            number("fps"),
            number("mbps"),
            options.get("switch").map(String::as_str),
            options.get("switch_keyframe").is_some_and(|v| v == "1"),
            &mut lines,
        )
    };
    let passed = match result {
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
    switch: Option<&str>,
    switch_keyframe: bool,
    lines: &mut Vec<String>,
) -> Result<bool, String> {
    let _ = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    let force = std::env::var("QUEQIAO_FORCE").unwrap_or_default();
    lines.push(format!(
        "forced: {}",
        if force.is_empty() { "nothing" } else { &force }
    ));
    let frequency = qpc_frequency() as f64;
    // The picture: one real desktop image, or the test pattern (source=pattern: nothing of the screen captured).
    let pattern = std::env::var("QUEQIAO_PROBE_SOURCE").as_deref() == Ok("pattern");
    let (image, device, context, screen_fps) = if pattern {
        let (device, context, adapter) = hardware_device(lines)?;
        lines.push(format!(
            "source: test pattern 1920x1080 (no screen capture); convert + encode on: {adapter}"
        ));
        (test_pattern(1920, 1080), device, context, 60)
    } else {
        let mut duplicator = Duplicator::primary().map_err(describe)?;
        let info = duplicator.info().clone();
        let screen_fps = (info.refresh_hz.round() as u32).clamp(1, 60);
        lines.push(format!(
            "screen: {} {}x{} at {:.0} Hz on {}",
            info.output, info.width, info.height, info.refresh_hz, info.adapter
        ));

        // One real desktop image (the first frame after starting can be an empty mouse-only update).
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
        (image, device, context, screen_fps)
    };
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
    let input_format = encoder.input_format();
    let mut converter =
        Nv12Converter::with_format(&device, &context, (width, height), size, fps, input_format)
            .map_err(|e| format!("BGRA -> {input_format:?} converter: {}", describe(e)))?;
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
    // switch=S:MBPS: the bitrate changed after S seconds.
    let switch = switch.and_then(|spec| {
        let (at, mbps) = spec.split_once(':')?;
        Some((at.parse::<u32>().ok()? * fps, mbps.parse::<f64>().ok()?))
    });
    let start = qpc_now();
    for i in 0..frames {
        if let Some((at, mbps)) = switch
            && i == at
        {
            let result = encoder.set_bitrate((mbps * 1e6) as u32);
            lines.push(format!(
                "bitrate set to {mbps} Mbit/s at frame {i}: {}",
                result.map_or_else(describe, |()| "accepted".into())
            ));
        }
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
        // switch_keyframe=1: a keyframe right after the bitrate change.
        let keyframe = i == 0 || (switch_keyframe && switch.is_some_and(|(at, _)| i == at));
        encoder.encode(nv12, time, keyframe).map_err(describe)?;
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
        "encode time (frame in -> stream out), ms: {}",
        summary(&mut encode_ms)
    ));
    lines.push(format!(
        "scroll + convert + encode, ms: {}",
        summary(&mut total_ms)
    ));
    let hevc = encoder.format().codec == Codec::H265;
    let types_of = |data: &[u8]| {
        if hevc {
            hevc_nal_types(data)
        } else {
            nal_types(data)
        }
    };
    let first = outputs.first().map(|(f, _)| f);
    if let Some(first) = first {
        lines.push(format!(
            "first frame: {:.1} KB, keyframe {}, NAL types {:?}",
            first.data.len() as f64 / 1024.0,
            first.keyframe,
            types_of(&first.data)
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
    // The stream's rate each second of video (frame times are in 100 ns).
    let mut per_second = vec![0usize; (seconds.ceil() as usize).max(1)];
    for (frame, _) in &outputs {
        let second = (frame.time.max(0) / 10_000_000) as usize;
        if let Some(bytes) = per_second.get_mut(second) {
            *bytes += frame.data.len();
        }
    }
    lines.push(format!(
        "Mbit/s each second: {}",
        per_second
            .iter()
            .map(|bytes| format!("{:.1}", *bytes as f64 * 8.0 / 1e6))
            .collect::<Vec<_>>()
            .join(" ")
    ));

    std::fs::create_dir_all(out_dir).map_err(|e| e.to_string())?;
    if let Some(count) = std::env::var("QUEQIAO_PROBE_KEEP_FIRST")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
    {
        let mut kept = Vec::new();
        for (frame, _) in outputs.iter().take(count) {
            kept.extend_from_slice(&(frame.data.len() as u32).to_le_bytes());
            kept.extend_from_slice(&frame.data);
        }
        let path = out_dir.join("encode_probe_first.frames");
        std::fs::write(&path, &kept).map_err(|e| e.to_string())?;
        lines.push(format!(
            "first {count} frames: {} ({:.0} kB)",
            path.display(),
            kept.len() as f64 / 1e3
        ));
    }
    let stream: Vec<u8> = outputs.iter().flat_map(|(f, _)| f.data.clone()).collect();
    let stream_path = out_dir.join(if hevc {
        "encode_probe.h265"
    } else {
        "encode_probe.h264"
    });
    std::fs::write(&stream_path, &stream).map_err(|e| e.to_string())?;
    lines.push(format!("stream file: {}", stream_path.display()));

    let decoded_ok = if std::env::var("QUEQIAO_PROBE_DECODE").as_deref() == Ok("vpl") {
        match decode_back(
            &device,
            &context,
            encoder.format(),
            &outputs,
            &image,
            pattern,
            fps,
        ) {
            Ok((line, passed)) => {
                lines.push(line);
                passed
            }
            Err(error) => {
                lines.push(format!("decoded back: failed: {error}"));
                false
            }
        }
    } else {
        true
    };
    // H.264: SPS, PPS, IDR; H.265: VPS, SPS, PPS, IDR (with or without leading pictures).
    let starts_clean = first.is_some_and(|f| {
        let types = types_of(&f.data);
        f.keyframe
            && if hevc {
                [32, 33, 34].iter().all(|t| types.contains(t))
                    && (types.contains(&19) || types.contains(&20))
            } else {
                types.contains(&7) && types.contains(&8) && types.contains(&5)
            }
    });
    // A blank or unreadable input encodes to almost nothing: the moving desktop must use a fair share of the rate.
    let average = bytes as f64 * 8.0 / seconds;
    let not_blank = average > f64::from(config.bitrate) * 0.2
        || std::env::var("QUEQIAO_PROBE_DECODE").as_deref() == Ok("vpl");
    encode_ms.sort_by(f64::total_cmp);
    let keeps_up = percentile(&encode_ms, 0.95) < 1000.0 / f64::from(fps);
    let all_out = outputs.len() == frames as usize;
    lines.push(format!(
        "checks: all frames out {all_out}, starts with parameter sets + IDR {starts_clean}, p95 encode < frame time {keeps_up}, stream not blank {not_blank}, decoded back {decoded_ok}"
    ));
    Ok(all_out && starts_clean && keeps_up && not_blank && decoded_ok)
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
        // The format asked for (format=, for NVENC, oneVPL and AMF): H.265, 4:4:4, 10-bit.
        let mut config =
            EncoderConfig::for_screen(w, h, request.fps).with_format(EncodeFormat::from_env());
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

/// The stream decoded back with Intel's oneVPL decoder: a line for the report, and whether it passed (`judge`). The
/// first picture is the source before any scrolling.
fn decode_back(
    device: &ID3D11Device,
    context: &ID3D11DeviceContext,
    wanted: EncodeFormat,
    outputs: &[(EncodedFrame, i64)],
    image: &BgraImage,
    pattern: bool,
    fps: u32,
) -> Result<(String, bool), String> {
    let mut decoder = H264Decoder::vpl(device, context, wanted.codec).map_err(describe)?;
    let mut pictures = Vec::new();
    let mut first = None;
    let mut count = 0;
    for (frame, _) in outputs {
        pictures.clear();
        decoder
            .decode(&frame.data, frame.time, &mut pictures)
            .map_err(describe)?;
        count += pictures.len();
        if first.is_none()
            && let Some(picture) = pictures.first()
        {
            // At once: the decoder reuses its textures a few pictures later.
            first = Some(compare(device, context, picture, image, fps)?);
        }
    }
    let score = first.ok_or("no picture decoded")?;
    let (verdict, passed) = judge(&score, pattern, decoder.stream(), wanted);
    let line = format!(
        "decoded back ({}): {count} of {} pictures; {verdict}",
        decoder.name(),
        outputs.len()
    );
    Ok((line, passed))
}

/// PSNR (R, G, B) of the first decoded picture against its source: over the whole picture, and for the test pattern
/// over its colour part (bars and ramps: flat colour every format must keep) and its detail part (one-pixel coloured
/// stripes, checks and text: what 4:2:0 blurs and 4:4:4 keeps), a margin away from where they meet.
struct Score {
    whole: [f64; 3],
    colour: [f64; 3],
    detail: [f64; 3],
}

/// The verdict on a decoded picture: the stream must be the format asked for (chroma, depth: an encoder may quietly
/// make 4:2:0); the test pattern's colour part at least 30 dB in every channel (a wrong matrix, range or channel
/// order falls far below), and for 4:4:4 its detail part at least 25 dB (4:2:0 measures 11-14 there); the desktop
/// at least 25 dB as a whole (4:2:0 on coloured text measured 28-35).
fn judge(score: &Score, pattern: bool, stream: &str, wanted: EncodeFormat) -> (String, bool) {
    let rgb = |v: [f64; 3]| format!("{:.1}/{:.1}/{:.1}", v[0], v[1], v[2]);
    let low = |v: [f64; 3]| v[0].min(v[1]).min(v[2]);
    let right_format = stream.is_empty() || stream == wanted.name();
    let (text, quality) = if pattern {
        let detail_ok = !wanted.chroma444 || low(score.detail) >= 25.0;
        (
            format!(
                "PSNR R/G/B against the test pattern: colour {}, detail {}, whole {} dB",
                rgb(score.colour),
                rgb(score.detail),
                rgb(score.whole)
            ),
            low(score.colour) >= 30.0 && detail_ok,
        )
    } else {
        (
            format!("PSNR R/G/B against the desktop: {} dB", rgb(score.whole)),
            low(score.whole) >= 25.0,
        )
    };
    (
        format!(
            "stream {} (asked {}){}; {text}",
            if stream.is_empty() { "?" } else { stream },
            wanted.name(),
            if right_format { "" } else { " WRONG FORMAT" }
        ),
        right_format && quality,
    )
}

/// PSNR (R, G, B) of a decoded picture shown as the viewer shows it (YUV -> RGB on the GPU) against `image`.
fn compare(
    device: &ID3D11Device,
    context: &ID3D11DeviceContext,
    picture: &DecodedFrame,
    image: &BgraImage,
    fps: u32,
) -> Result<Score, String> {
    let (width, height) = (picture.width, picture.height);
    if (width, height) != (image.width, image.height) {
        return Err(format!(
            "decoded {width}x{height}, desktop {}x{}: not compared",
            image.width, image.height
        ));
    }
    let mut converter =
        Nv12Converter::to_rgb(device, context, (width, height), fps).map_err(describe)?;
    let rgb = converter.create_output(device).map_err(describe)?;
    converter
        .convert(&picture.texture, &rgb)
        .map_err(|e| format!("YUV -> RGB: {}", describe(e)))?;
    let mut desc = D3D11_TEXTURE2D_DESC::default();
    unsafe { rgb.GetDesc(&mut desc) };
    desc.Usage = D3D11_USAGE_STAGING;
    desc.BindFlags = 0;
    desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
    desc.MiscFlags = 0;
    let mut staging: Option<ID3D11Texture2D> = None;
    unsafe { device.CreateTexture2D(&desc, None, Some(&mut staging)) }.map_err(describe)?;
    let staging = staging.ok_or("staging texture")?;
    let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
    unsafe {
        context.CopyResource(&staging, &rgb);
        context
            .Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
            .map_err(describe)?;
    }
    let pitch = mapped.RowPitch as usize;
    let (w, h) = (width as usize, height as usize);
    let shown = unsafe { std::slice::from_raw_parts(mapped.pData.cast::<u8>(), pitch * h) };
    // Squared differences per row and channel (B, G, R as in BGRA).
    let mut rows = vec![[0f64; 3]; h];
    for (y, sums) in rows.iter_mut().enumerate() {
        let row = &shown[y * pitch..y * pitch + w * 4];
        let original = &image.pixels[y * w * 4..(y + 1) * w * 4];
        for (a, b) in row
            .as_chunks::<4>()
            .0
            .iter()
            .zip(original.as_chunks::<4>().0)
        {
            for channel in 0..3 {
                let d = f64::from(a[channel]) - f64::from(b[channel]);
                sums[channel] += d * d;
            }
        }
    }
    unsafe { context.Unmap(&staging, 0) };
    // (R, G, B) over rows `range`.
    let psnr = |range: std::ops::Range<usize>| {
        let count = (range.len() * w).max(1) as f64;
        let mut sums = [0f64; 3];
        for row in &rows[range] {
            for channel in 0..3 {
                sums[channel] += row[channel];
            }
        }
        let one = |sum: f64| {
            let mse = sum / count;
            if mse == 0.0 {
                99.0
            } else {
                10.0 * (255.0 * 255.0 / mse).log10()
            }
        };
        [one(sums[2]), one(sums[1]), one(sums[0])]
    };
    // The pattern's colour part is its first four bands of six, the detail part the last two.
    let split = h * 4 / 6;
    let margin = 16.min(split);
    Ok(Score {
        whole: psnr(0..h),
        colour: psnr(0..split - margin),
        detail: psnr((split + margin).min(h)..h),
    })
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
/// A test picture instead of the screen (source=pattern), so nothing of the desktop is captured: colour bars (100 %
/// and 75 %), a grey ramp and a colour ramp (banding shows in 8 bits), one-pixel coloured stripes and checks (chroma
/// detail that 4:2:0 blurs and 4:4:4 keeps), and dark blue "text" on paper.
fn test_pattern(width: u32, height: u32) -> BgraImage {
    let (w, h) = (width as usize, height as usize);
    let mut pixels = vec![0u8; w * h * 4];
    // White, yellow, cyan, green, magenta, red, blue, black (R, G, B).
    let bars: [(u8, u8, u8); 8] = [
        (255, 255, 255),
        (255, 255, 0),
        (0, 255, 255),
        (0, 255, 0),
        (255, 0, 255),
        (255, 0, 0),
        (0, 0, 255),
        (0, 0, 0),
    ];
    for y in 0..h {
        for x in 0..w {
            let ramp = (x * 255 / (w - 1).max(1)) as u8;
            let (r, g, b) = match y * 6 / h {
                0 => bars[x * 8 / w],
                1 => {
                    let (r, g, b) = bars[x * 8 / w];
                    (r / 4 * 3, g / 4 * 3, b / 4 * 3)
                }
                2 => (ramp, ramp, ramp),
                3 => (ramp, 64, 255 - ramp),
                4 if x < w / 2 => {
                    if x % 2 == 0 {
                        (255, 0, 0)
                    } else {
                        (0, 0, 255)
                    }
                }
                4 => {
                    if (x + y) % 2 == 0 {
                        (0, 255, 0)
                    } else {
                        (255, 0, 255)
                    }
                }
                _ => {
                    // Strokes of 1-2 pixels in rows of "characters".
                    let (cx, cy) = (x % 9, y % 16);
                    let stroke = cy < 12 && (cx == (y / 16 + x / 9) % 7 || cy == (x / 9) % 11);
                    if stroke {
                        (20, 30, 140)
                    } else {
                        (250, 248, 236)
                    }
                }
            };
            let i = (y * w + x) * 4;
            pixels[i] = b;
            pixels[i + 1] = g;
            pixels[i + 2] = r;
            pixels[i + 3] = 255;
        }
    }
    BgraImage {
        width,
        height,
        pixels,
    }
}

/// The adapter of a GPU maker (QUEQIAO_PROBE_ADAPTER: amd, nvidia, intel), if there is one.
fn adapter_of(maker: &str) -> Result<Option<IDXGIAdapter1>, String> {
    let vendor = match maker {
        "amd" => 0x1002,
        "nvidia" => 0x10DE,
        "intel" => 0x8086,
        _ => return Err(format!("adapter={maker}: amd, nvidia or intel")),
    };
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }.map_err(describe)?;
    for index in 0.. {
        let Ok(adapter) = (unsafe { factory.EnumAdapters1(index) }) else {
            break;
        };
        if unsafe { adapter.GetDesc1() }.is_ok_and(|desc| desc.VendorId == vendor) {
            return Ok(Some(adapter));
        }
    }
    Ok(None)
}

/// The default hardware adapter's device, or the one QUEQIAO_PROBE_ADAPTER names (source=pattern: no screen is
/// captured, so no duplication device).
fn hardware_device(
    lines: &mut Vec<String>,
) -> Result<(ID3D11Device, ID3D11DeviceContext, String), String> {
    let chosen = match std::env::var("QUEQIAO_PROBE_ADAPTER") {
        Ok(maker) => Some(
            adapter_of(&maker)?.ok_or_else(|| format!("no {maker} graphics adapter on this PC"))?,
        ),
        Err(_) => None,
    };
    let mut device = None;
    let mut context = None;
    unsafe {
        D3D11CreateDevice(
            chosen
                .as_ref()
                .map(|a| a.cast::<IDXGIAdapter>())
                .transpose()
                .map_err(describe)?
                .as_ref(),
            if chosen.is_some() {
                D3D_DRIVER_TYPE_UNKNOWN
            } else {
                D3D_DRIVER_TYPE_HARDWARE
            },
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )
    }
    .map_err(describe)?;
    let device: ID3D11Device = device.ok_or("no device")?;
    let context = context.ok_or("no device context")?;
    let adapter = device
        .cast::<IDXGIDevice>()
        .and_then(|dxgi| unsafe { dxgi.GetAdapter() })
        .and_then(|adapter| unsafe { adapter.GetDesc() })
        .map(|desc| {
            let end = desc
                .Description
                .iter()
                .position(|c| *c == 0)
                .unwrap_or(desc.Description.len());
            String::from_utf16_lossy(&desc.Description[..end])
        })
        .unwrap_or_else(|_| "default hardware adapter".into());
    lines.push(format!("device: {adapter} (default hardware adapter)"));
    Ok((device, context, adapter))
}

/// decode_file=<path> (with source=pattern and format=): decodes the first frames another machine's probe kept
/// (keep_first=, each frame after its 4-byte little-endian length) with Intel's oneVPL decoder and compares the first
/// picture with the test pattern. So an NVENC stream made on an NVIDIA PC is checked where oneVPL decodes.
fn decode_only(path: &Path, lines: &mut Vec<String>) -> Result<bool, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut frames = Vec::new();
    let mut at = 0usize;
    while at + 4 <= bytes.len() {
        let length = u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap_or_default()) as usize;
        at += 4;
        let end = (at + length).min(bytes.len());
        frames.push(bytes[at..end].to_vec());
        at = end;
    }
    lines.push(format!(
        "decode only: {} frames from {}",
        frames.len(),
        path.display()
    ));
    let (device, context, _) = hardware_device(lines)?;
    let wanted = EncodeFormat::from_env();
    let mut decoder = H264Decoder::vpl(&device, &context, wanted.codec).map_err(describe)?;
    let mut pictures = Vec::new();
    let mut first = None;
    let mut count = 0;
    for (index, data) in frames.iter().enumerate() {
        pictures.clear();
        decoder
            .decode(data, index as i64, &mut pictures)
            .map_err(describe)?;
        count += pictures.len();
        if first.is_none()
            && let Some(picture) = pictures.first()
        {
            let pattern = test_pattern(picture.width, picture.height);
            first = Some(compare(&device, &context, picture, &pattern, 60)?);
        }
    }
    let score = first.ok_or("no picture decoded")?;
    let (verdict, passed) = judge(&score, true, decoder.stream(), wanted);
    lines.push(format!(
        "decoded ({}): {count} of {} pictures; {verdict}",
        decoder.name(),
        frames.len()
    ));
    Ok(passed)
}

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
