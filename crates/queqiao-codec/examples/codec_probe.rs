//! M1 step 4a check: H.264 round trip (convert -> encode -> decode) on synthetic screen content, with every
//! path selectable. It captures nothing, so it also runs outside the desktop session (over SSH).
//!
//! The content imitates a scrolling web page: a light background, lines of dark word-like blocks and
//! coloured boxes, moved up a few rows per frame. Each frame is converted to NV12, encoded and decoded; the
//! decoded luma (Y) is compared with the encoder's input (PSNR).
//!
//! Usage: codec_probe [width=1920] [height=1080] [frames=120] [fps=60] [force=STAGE:CHOICE,...]
//!   force: encode:software|hardware, decode:software|hardware, convert:cpu, adapter:warp
//! Prints the report; exit code 0 = PASS.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use queqiao_codec::{
    DecodedFrame, DecoderChoice, EncodedFrame, EncoderChoice, EncoderConfig, H264Decoder,
    H264Encoder, Nv12Converter, nal_types,
};
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{IDXGIAdapter, IDXGIDevice};
use windows::core::Interface;

/// Lowest acceptable luma PSNR: below about 30 dB text on screen content gets visibly smeared.
const MIN_PSNR_DB: f64 = 30.0;

#[derive(Default)]
struct Stats {
    encoded: usize,
    decoded: usize,
    bytes: usize,
    psnr: Vec<f64>,
    decode_ms: Vec<f64>,
    mismatched: usize,
    first_nal_types: Vec<u8>,
}

fn main() {
    let options: HashMap<String, String> = std::env::args()
        .skip(1)
        .filter_map(|a| {
            a.split_once('=')
                .map(|(k, v)| (k.to_string(), v.to_string()))
        })
        .collect();
    if let Some(spec) = options.get("force") {
        // Before anything else runs: same as setting QUEQIAO_FORCE in the environment.
        unsafe { std::env::set_var("QUEQIAO_FORCE", spec) };
    }
    let number = |key: &str, default: u32| {
        options
            .get(key)
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(default)
    };
    let (width, height) = (number("width", 1920) & !1, number("height", 1080) & !1);
    let (frames, fps) = (number("frames", 120), number("fps", 60).max(1));
    let mut lines = Vec::new();
    let passed = match run(width, height, frames, fps, &mut lines) {
        Ok(passed) => passed,
        Err(error) => {
            lines.push(format!("ERROR {error}"));
            false
        }
    };
    lines.push(format!("RESULT {}", if passed { "PASS" } else { "FAIL" }));
    for line in &lines {
        println!("{line}");
    }
    std::process::exit(if passed { 0 } else { 1 });
}

fn run(
    width: u32,
    height: u32,
    frames: u32,
    fps: u32,
    lines: &mut Vec<String>,
) -> Result<bool, String> {
    let force = std::env::var("QUEQIAO_FORCE").unwrap_or_default();
    lines.push(format!(
        "forced: {}; {width}x{height}, {frames} frames at {fps} fps",
        if force.is_empty() { "nothing" } else { &force }
    ));
    let warp = queqiao_core::forced("adapter").as_deref() == Some("warp");
    let (device, context, gpu) = create_device(warp)?;
    lines.push(format!("device: {gpu}"));

    let (w, h) = (width as usize, height as usize);
    let canvas = page(w, h * 2);
    let bgra = bgra_texture(&device, width, height)?;
    let mut converter =
        Nv12Converter::new(&device, &context, (width, height), (width, height), fps)
            .map_err(|e| format!("converter: {}", describe(e)))?;
    let ring = (0..4)
        .map(|_| converter.create_output(&device))
        .collect::<windows::core::Result<Vec<_>>>()
        .map_err(|e| format!("NV12 textures: {}", describe(e)))?;
    let config = EncoderConfig::for_screen(width, height, fps);
    let mut encoder = H264Encoder::new(&device, &context, &config, EncoderChoice::from_env())
        .map_err(|e| format!("encoder: {}", describe(e)))?;
    let mut decoder = H264Decoder::new(&device, &context, DecoderChoice::from_env())
        .map_err(|e| format!("decoder: {}", describe(e)))?;
    lines.push(format!("convert: {}", converter.description()));
    lines.push(format!(
        "encoder: {} ({}), {:.1} Mbit/s",
        encoder.name(),
        kind(encoder.is_hardware()),
        f64::from(config.bitrate) / 1e6
    ));
    lines.push(format!("decoder: {}", decoder.name()));
    for reason in encoder.skipped().iter().chain(decoder.skipped()) {
        lines.push(format!("skipped: {reason}"));
    }

    let mut reader = LumaReader::default();
    let mut sources: HashMap<i64, Vec<u8>> = HashMap::new();
    let mut stats = Stats::default();
    let mut pending: Vec<EncodedFrame> = Vec::new();
    let step = (h / 180).max(2);
    let duration = 10_000_000 / i64::from(fps);
    let started = Instant::now();
    for i in 0..frames {
        let offset = (i as usize * step) % h;
        let rows = &canvas[offset * w * 4..];
        unsafe { context.UpdateSubresource(&bgra, 0, None, rows.as_ptr().cast(), width * 4, 0) };
        let nv12 = &ring[i as usize % ring.len()];
        converter.convert(&bgra, nv12).map_err(describe)?;
        let time = i64::from(i) * duration;
        sources.insert(time, reader.read(&device, &context, nv12)?);
        let deadline = Instant::now() + Duration::from_secs(1);
        while !encoder.wants_input() {
            encoder.poll(&mut pending).map_err(describe)?;
            if Instant::now() > deadline {
                return Err(format!(
                    "the encoder asked for no input for 1 s (frame {i})"
                ));
            }
            std::thread::yield_now();
        }
        encoder.encode(nv12, time, i == 0).map_err(describe)?;
        encoder.poll(&mut pending).map_err(describe)?;
        let ready = std::mem::take(&mut pending);
        decode_and_compare(
            &mut decoder,
            ready,
            &mut sources,
            &mut reader,
            &device,
            &context,
            &mut stats,
        )?;
    }
    encoder.drain().map_err(describe)?;
    let deadline = Instant::now() + Duration::from_secs(2);
    while !encoder.drained() && Instant::now() < deadline {
        encoder.poll(&mut pending).map_err(describe)?;
        std::thread::yield_now();
    }
    encoder.poll(&mut pending).map_err(describe)?;
    let ready = std::mem::take(&mut pending);
    decode_and_compare(
        &mut decoder,
        ready,
        &mut sources,
        &mut reader,
        &device,
        &context,
        &mut stats,
    )?;
    let elapsed = started.elapsed().as_secs_f64();

    // The decoder's kind is only final after decoding (Windows' decoder may fall back to its CPU path).
    lines.push(format!(
        "decoding ran on: {}",
        if decoder.is_hardware() { "GPU" } else { "CPU" }
    ));
    lines.push(format!(
        "{frames} frames: {} encoded ({:.1} Mbit/s), {} decoded, {} without a matching input, {:.1} s",
        stats.encoded,
        stats.bytes as f64 * 8.0 / (f64::from(frames) / f64::from(fps)) / 1e6,
        stats.decoded,
        stats.mismatched,
        elapsed
    ));
    lines.push(format!(
        "first frame NAL types: {:?}",
        stats.first_nal_types
    ));
    lines.push(format!(
        "decode call, ms: {}",
        summary(&mut stats.decode_ms)
    ));
    lines.push(format!("luma PSNR, dB: {}", summary(&mut stats.psnr)));
    let min_psnr = stats.psnr.first().copied().unwrap_or(0.0);
    let all_decoded = stats.decoded == frames as usize && stats.mismatched == 0;
    let quality = min_psnr >= MIN_PSNR_DB;
    let keeps_up = percentile(&stats.decode_ms, 0.95) < 1000.0 / f64::from(fps);
    lines.push(format!(
        "checks: every frame decoded {all_decoded}, lowest PSNR >= {MIN_PSNR_DB} dB {quality}, p95 decode < frame time {keeps_up}"
    ));
    Ok(all_decoded && quality && keeps_up)
}

#[allow(clippy::too_many_arguments)]
fn decode_and_compare(
    decoder: &mut H264Decoder,
    encoded: Vec<EncodedFrame>,
    sources: &mut HashMap<i64, Vec<u8>>,
    reader: &mut LumaReader,
    device: &ID3D11Device,
    context: &ID3D11DeviceContext,
    stats: &mut Stats,
) -> Result<(), String> {
    for frame in encoded {
        if stats.encoded == 0 {
            stats.first_nal_types = nal_types(&frame.data);
        }
        stats.encoded += 1;
        stats.bytes += frame.data.len();
        let mut decoded: Vec<DecodedFrame> = Vec::new();
        let started = Instant::now();
        decoder
            .decode(&frame.data, frame.time, &mut decoded)
            .map_err(|e| format!("decode: {}", describe(e)))?;
        stats.decode_ms.push(started.elapsed().as_secs_f64() * 1e3);
        for picture in decoded {
            stats.decoded += 1;
            let Some(source) = sources.remove(&picture.time) else {
                stats.mismatched += 1;
                continue;
            };
            let luma = reader.read(device, context, &picture.texture)?;
            if luma.len() != source.len() {
                stats.mismatched += 1;
                continue;
            }
            stats.psnr.push(psnr(&source, &luma));
        }
    }
    Ok(())
}

/// Luma PSNR in dB (99 for identical pictures).
fn psnr(a: &[u8], b: &[u8]) -> f64 {
    let squared: u64 = a
        .iter()
        .zip(b)
        .map(|(&x, &y)| u64::from(x.abs_diff(y)).pow(2))
        .sum();
    if squared == 0 {
        return 99.0;
    }
    let mse = squared as f64 / a.len() as f64;
    10.0 * (255.0 * 255.0 / mse).log10()
}

/// Reads the Y plane of NV12 textures back to memory (rows packed).
#[derive(Default)]
struct LumaReader {
    staging: Option<(ID3D11Texture2D, (u32, u32))>,
}

impl LumaReader {
    fn read(
        &mut self,
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
        nv12: &ID3D11Texture2D,
    ) -> Result<Vec<u8>, String> {
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { nv12.GetDesc(&mut desc) };
        let size = (desc.Width, desc.Height);
        if self.staging.as_ref().is_none_or(|(_, s)| *s != size) {
            desc.Usage = D3D11_USAGE_STAGING;
            desc.BindFlags = 0;
            desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
            desc.MiscFlags = 0;
            let mut out = None;
            unsafe { device.CreateTexture2D(&desc, None, Some(&mut out)) }.map_err(describe)?;
            self.staging = out.map(|t| (t, size));
        }
        let staging = &self.staging.as_ref().ok_or("no staging texture")?.0;
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        unsafe {
            context.CopyResource(staging, nv12);
            context
                .Map(staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
                .map_err(describe)?;
        }
        let (width, height, pitch) = (size.0 as usize, size.1 as usize, mapped.RowPitch as usize);
        let data = unsafe { std::slice::from_raw_parts(mapped.pData.cast::<u8>(), pitch * height) };
        let mut luma = Vec::with_capacity(width * height);
        for row in data.chunks(pitch).take(height) {
            luma.extend_from_slice(&row[..width]);
        }
        unsafe { context.Unmap(staging, 0) };
        Ok(luma)
    }
}

/// A light "web page" as BGRA: lines of dark word-like blocks (some blue, like links) and coloured boxes.
fn page(width: usize, height: usize) -> Vec<u8> {
    let mut pixels = [248u8, 248, 248, 255].repeat(width * height);
    let mut seed = 0x2545_F491_4F6C_DD1D_u64;
    let mut random = move |limit: usize| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed % limit as u64) as usize
    };
    let mut fill = |x: usize, y: usize, w: usize, h: usize, colour: [u8; 4], stripes: bool| {
        for row in y..(y + h).min(height) {
            for column in x..(x + w).min(width) {
                if stripes && (column - x) % 3 == 2 {
                    continue;
                }
                let i = (row * width + column) * 4;
                pixels[i..i + 4].copy_from_slice(&colour);
            }
        }
    };
    for top in (16..height.saturating_sub(24)).step_by(22) {
        let mut x = 24;
        loop {
            let word = 12 + random(60);
            if x + word + 24 >= width {
                break;
            }
            let colour = if random(4) == 0 {
                [190, 90, 30, 255]
            } else {
                [40, 40, 40, 255]
            };
            fill(x, top, word, 12, colour, true);
            x += word + 6 + random(6);
        }
    }
    for _ in 0..(width * height / 150_000).max(4) {
        let (w, h) = (80 + random(220), 60 + random(160));
        let (x, y) = (
            random(width.saturating_sub(w).max(1)),
            random(height.saturating_sub(h).max(1)),
        );
        let colour = [random(256) as u8, random(256) as u8, random(256) as u8, 255];
        fill(x, y, w, h, colour, false);
    }
    pixels
}

fn create_device(warp: bool) -> Result<(ID3D11Device, ID3D11DeviceContext, String), String> {
    let (driver, flags) = if warp {
        (D3D_DRIVER_TYPE_WARP, D3D11_CREATE_DEVICE_BGRA_SUPPORT)
    } else {
        let video = D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT;
        (D3D_DRIVER_TYPE_HARDWARE, video)
    };
    let mut device = None;
    let mut context = None;
    unsafe {
        D3D11CreateDevice(
            None::<&IDXGIAdapter>,
            driver,
            HMODULE::default(),
            flags,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )
    }
    .map_err(|e| format!("D3D11 device: {}", describe(e)))?;
    let (Some(device), Some(context)) = (device, context) else {
        return Err("D3D11 device not created".into());
    };
    let name = device
        .cast::<IDXGIDevice>()
        .and_then(|d| unsafe { d.GetAdapter() })
        .and_then(|a| unsafe { a.GetDesc() })
        .map(|desc| {
            let text = &desc.Description;
            let len = text.iter().position(|&c| c == 0).unwrap_or(text.len());
            String::from_utf16_lossy(&text[..len])
        })
        .unwrap_or_else(|_| "unknown".into());
    Ok((device, context, name))
}

fn bgra_texture(device: &ID3D11Device, width: u32, height: u32) -> Result<ID3D11Texture2D, String> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
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
    let mut out = None;
    unsafe { device.CreateTexture2D(&desc, None, Some(&mut out)) }.map_err(describe)?;
    out.ok_or_else(|| "CreateTexture2D returned nothing".to_string())
}

fn kind(hardware: bool) -> &'static str {
    if hardware { "hardware" } else { "software" }
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
        "min {:.2}, median {:.2}, p95 {:.2}, max {:.2} (n={})",
        percentile(values, 0.0),
        percentile(values, 0.5),
        percentile(values, 0.95),
        percentile(values, 1.0),
        values.len()
    )
}

fn describe(error: windows::core::Error) -> String {
    format!("0x{:08X} {}", error.code().0, error.message())
}
