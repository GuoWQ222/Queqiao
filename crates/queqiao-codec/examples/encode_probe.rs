//! M1 step 2 check: hardware H.264 encoding of the screen, entirely on the GPU.
//!
//! Captures one desktop image, then for each frame scrolls it a little (a stand-in for scrolling a page,
//! so every frame changes), converts BGRA -> NV12 on the GPU and encodes it in real time at the screen's
//! frame rate (at most 60 by default). Nothing here is tied to one GPU: the encoder is whichever
//! hardware H.264 encoder Windows offers for the capturing GPU, and the size, rate and bitrate come
//! from the screen. Runs in the desktop session via scripts\run_in_session.ps1 (no console window).
//!
//! Usage: encode_probe <out_dir> [frames] [fps] [mbps]
//! Writes <out_dir>\encode_probe.txt (report) and <out_dir>\encode_probe.h264 (the stream).

#![windows_subsystem = "windows"]

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use queqiao_capture::{Duplicator, qpc_frequency, qpc_now};
use queqiao_codec::{
    EncodedFrame, EncoderConfig, H264Encoder, Nv12Converter, candidate_sizes, nal_types,
};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};

fn main() {
    let mut args = std::env::args().skip(1);
    let out_dir = PathBuf::from(args.next().unwrap_or_else(|| ".".into()));
    let mut number = || args.next().and_then(|s| s.parse::<u32>().ok());
    let (frames, fps, mbps) = (number().unwrap_or(300), number(), number());
    let mut lines = Vec::new();
    let passed = match run(&out_dir, frames, fps, mbps, &mut lines) {
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
    let mut duplicator = Duplicator::new(0).map_err(describe)?;
    let info = duplicator.info().clone();
    let fps = fps.unwrap_or((info.refresh_hz.round() as u32).clamp(1, 60));
    lines.push(format!(
        "gpu: {}; screen {}x{} at {:.0} Hz; encoding at {fps} fps",
        info.adapter, info.width, info.height, info.refresh_hz
    ));
    let device = duplicator.device().clone();
    let context = duplicator.context().clone();

    // One real desktop image (the first frame after starting can be an empty mouse-only update).
    let frequency = qpc_frequency() as f64;
    let deadline = qpc_now() + (10.0 * frequency) as i64;
    let source = loop {
        if qpc_now() > deadline {
            return Err("no screen update within 10 s".into());
        }
        if let Some(frame) = duplicator.acquire(100).map_err(describe)?
            && frame.present_qpc != 0
        {
            let copy = bgra_texture(&device, &frame.texture)?;
            unsafe { context.CopyResource(&copy, &frame.texture) };
            break copy;
        }
    };
    duplicator.release().map_err(describe)?;
    let mut desc = D3D11_TEXTURE2D_DESC::default();
    unsafe { source.GetDesc(&mut desc) };
    let (width, height) = (desc.Width, desc.Height);

    // The largest size the encoder accepts, starting with the screen's own size.
    let mut chosen = None;
    for (w, h) in candidate_sizes(width, height) {
        let mut config = EncoderConfig::for_screen(w, h, fps);
        if let Some(mbps) = mbps {
            config.bitrate = mbps * 1_000_000;
        }
        match H264Encoder::new(&device, &config) {
            Ok(encoder) => {
                chosen = Some((encoder, config));
                break;
            }
            Err(error) => lines.push(format!("{w}x{h} refused: {}", error.message())),
        }
    }
    let (mut encoder, config) = chosen.ok_or("no encoder accepted any size")?;
    lines.push(format!(
        "encoder: {} at {}x{}, {:.1} Mbit/s constant bitrate",
        encoder.name(),
        config.width,
        config.height,
        f64::from(config.bitrate) / 1e6
    ));
    if !encoder.refused_settings().is_empty() {
        let refused = encoder.refused_settings().join(", ");
        lines.push(format!(
            "settings the encoder refused (it uses its defaults): {refused}"
        ));
    }

    let size = (config.width, config.height);
    let converter =
        Nv12Converter::new(&device, &context, (width, height), size, fps).map_err(describe)?;
    let ring = (0..4)
        .map(|_| converter.create_output(&device))
        .collect::<windows::core::Result<Vec<_>>>()
        .map_err(describe)?;
    let scene = bgra_texture(&device, &source)?;
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
    }
    encoder.drain().map_err(describe)?;
    let drain_deadline = qpc_now() + (2.0 * frequency) as i64;
    while !encoder.drained() && qpc_now() < drain_deadline {
        collect(&mut encoder, &mut outputs)?;
        std::thread::yield_now();
    }

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
        "{frames} frames in, {} out ({keyframes} keyframes), {late} submitted late, drained: {}",
        outputs.len(),
        encoder.drained()
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
    let p95 = percentile(&encode_ms, 0.95);
    let keeps_up = p95 < 1000.0 / f64::from(fps);
    lines.push(format!(
        "checks: all frames out {}, starts with SPS+PPS+IDR {starts_clean}, p95 encode < frame time {keeps_up}",
        outputs.len() == frames as usize
    ));
    Ok(outputs.len() == frames as usize && starts_clean && keeps_up)
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

/// A GPU texture like `like` (size, BGRA format) that we own, usable by the video processor.
fn bgra_texture(device: &ID3D11Device, like: &ID3D11Texture2D) -> Result<ID3D11Texture2D, String> {
    let mut desc = D3D11_TEXTURE2D_DESC::default();
    unsafe { like.GetDesc(&mut desc) };
    desc.Usage = D3D11_USAGE_DEFAULT;
    desc.BindFlags = (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32;
    desc.CPUAccessFlags = 0;
    desc.MiscFlags = 0;
    let mut out = None;
    unsafe { device.CreateTexture2D(&desc, None, Some(&mut out)) }.map_err(describe)?;
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
