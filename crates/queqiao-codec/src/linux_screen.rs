//! The agent's encoding pipeline on Linux, with the API of Windows' `ScreenEncoder` (docs/LINUX.md): the captured
//! frame (B, G, R, X in memory) -> NV12 at the encode size on the CPU (cpu_convert.rs: BT.709 limited range,
//! bilinear scaling, one thread per band) -> the encoder. OpenH264 within the software limits (as on Windows when no
//! GPU encoder works); NVENC on CUDA and VA-API come next and take the screen's own size.

use std::time::Instant;

use queqiao_capture::{Context, Device, Frame};

use crate::config::{EncodedFrame, EncoderConfig, SoftwareTiming};
use crate::cpu_convert::{Tap, bgra_to_nv12, taps};
use crate::software_core::SoftwareCore;
use crate::{EncodeFormat, SOFTWARE_START, VideoCodec, candidate_sizes};

/// An encoder error (Windows: `windows::core::Error`).
#[derive(Clone, Debug)]
pub struct Error(String);

impl Error {
    pub fn new(message: impl Into<String>) -> Error {
        Error(message.into())
    }

    pub fn message(&self) -> String {
        self.0.clone()
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

/// The encoder a `ScreenEncoder` opened: its name and kind, as Windows' `H264Encoder` tells them.
pub struct H264Encoder {
    core: SoftwareCore,
}

impl H264Encoder {
    pub fn name(&self) -> &str {
        &self.core.name
    }

    pub fn is_hardware(&self) -> bool {
        false
    }

    /// Where a software encoder's time goes.
    pub fn software_timing(&self) -> Option<SoftwareTiming> {
        Some(self.core.timing)
    }

    pub fn format(&self) -> EncodeFormat {
        EncodeFormat::H264
    }
}

pub struct ScreenEncoder {
    encoder: H264Encoder,
    config: EncoderConfig,
    columns: Vec<Tap>,
    rows: Vec<Tap>,
    nv12: Vec<u8>,
    /// The frame kept for the next encode (`keep`), as Windows copies the latest frame into a texture of its own:
    /// a captured frame is valid only until the next capture.
    kept: Vec<u8>,
    kept_stride: usize,
    index: u32,
    notes: Vec<String>,
    bitrate: u32,
}

impl ScreenEncoder {
    /// An encoder for a screen of `screen` pixels at up to `fps` frames per second. With `fixed_fps` the rate is kept
    /// even for the software encoder (normally capped at 30).
    pub fn new(
        device: &Device,
        context: &Context,
        screen: (u32, u32),
        fps: u32,
        fixed_fps: bool,
    ) -> Result<Self> {
        Self::with_limit(device, context, screen, fps, fixed_fps, None)
    }

    /// As `new`, with the picture no larger than `limit` (long side, short side), e.g. (1280, 720).
    pub fn with_limit(
        device: &Device,
        context: &Context,
        screen: (u32, u32),
        fps: u32,
        fixed_fps: bool,
        limit: Option<(u32, u32)>,
    ) -> Result<Self> {
        Self::with_codec(
            device,
            context,
            screen,
            fps,
            fixed_fps,
            limit,
            VideoCodec::H264,
        )
    }

    /// As `with_limit`, in `codec`: H.264 for now on Linux (AV1 and H.265 need a GPU encoder there).
    pub fn with_codec(
        device: &Device,
        context: &Context,
        screen: (u32, u32),
        fps: u32,
        fixed_fps: bool,
        limit: Option<(u32, u32)>,
        codec: VideoCodec,
    ) -> Result<Self> {
        let format = match codec {
            VideoCodec::H265 => EncodeFormat::from_name("hevc").unwrap_or(EncodeFormat::H264),
            _ => EncodeFormat::H264,
        };
        Self::with_format(
            device, context, screen, fps, fixed_fps, limit, codec, format,
        )
    }

    /// As `with_codec`, making `format`.
    #[allow(clippy::too_many_arguments)]
    pub fn with_format(
        _device: &Device,
        _context: &Context,
        screen: (u32, u32),
        fps: u32,
        fixed_fps: bool,
        limit: Option<(u32, u32)>,
        codec: VideoCodec,
        format: EncodeFormat,
    ) -> Result<Self> {
        if codec != VideoCodec::H264 || format != EncodeFormat::H264 {
            return Err(Error::new(format!(
                "{} needs a GPU encoder, which Linux does not have here yet (OpenH264 makes H.264)",
                format.name()
            )));
        }
        // OpenH264: within the software starting limits, as on Windows.
        let (long, short, max_fps) = SOFTWARE_START;
        let limit = limit.unwrap_or((u32::MAX, u32::MAX));
        let limit = (limit.0.min(long), limit.1.min(short));
        let fps = if fixed_fps {
            fps.max(1)
        } else {
            fps.clamp(1, max_fps)
        };
        let mut notes = Vec::new();
        let (width, height) = candidate_sizes(screen.0, screen.1)
            .into_iter()
            .find(|(w, h)| (*w).max(*h) <= limit.0 && (*w).min(*h) <= limit.1)
            .ok_or_else(|| Error::new(format!("no size for a {}x{} screen", screen.0, screen.1)))?;
        if (width, height) != screen {
            notes.push(format!(
                "{}x{} encoded at {width}x{height} (the software encoder's limit)",
                screen.0, screen.1
            ));
        }
        let config = EncoderConfig::for_screen(width, height, fps);
        let core = SoftwareCore::open(&config).map_err(Error::new)?;
        Ok(ScreenEncoder {
            encoder: H264Encoder { core },
            columns: taps(screen.0, width),
            rows: taps(screen.1, height),
            nv12: Vec::new(),
            kept: Vec::new(),
            kept_stride: 0,
            index: 0,
            bitrate: config.bitrate,
            config,
            notes,
        })
    }

    pub fn codec(&self) -> VideoCodec {
        self.config.codec
    }

    pub fn format(&self) -> EncodeFormat {
        self.encoder.format()
    }

    pub fn config(&self) -> &EncoderConfig {
        &self.config
    }

    pub fn encoder(&self) -> &H264Encoder {
        &self.encoder
    }

    /// The bitrate now (it starts at `config().bitrate` and changes with `set_bitrate`).
    pub fn bitrate(&self) -> u32 {
        self.bitrate
    }

    /// Changes the bitrate for the next frames (adaptive bitrate: the network's estimate).
    pub fn set_bitrate(&mut self, bitrate: u32) -> Result<()> {
        self.encoder.core.set_bitrate(bitrate).map_err(Error::new)?;
        self.bitrate = bitrate;
        Ok(())
    }

    pub fn converter_description(&self) -> String {
        format!(
            "CPU (BT.709, bilinear, {} threads)",
            std::thread::available_parallelism().map_or(1, |n| n.get().clamp(1, 8))
        )
    }

    /// Sizes that were refused or changed before this one was chosen.
    pub fn notes(&self) -> &[String] {
        &self.notes
    }

    /// Keeps a copy of `frame` for the next `encode_kept` (the frame itself is valid only until the next capture).
    pub fn keep(&mut self, frame: &Frame) {
        self.kept.clear();
        self.kept.extend_from_slice(frame.pixels());
        self.kept_stride = frame.stride;
    }

    /// Encodes the frame kept last; nothing before one was kept.
    pub fn encode_kept(&mut self, keyframe: bool) -> Result<Vec<EncodedFrame>> {
        if self.kept.is_empty() {
            return Ok(Vec::new());
        }
        let kept = std::mem::take(&mut self.kept);
        let result = self.encode_bgra(&kept, self.kept_stride, keyframe);
        self.kept = kept;
        result
    }

    /// Converts and encodes `frame` (the screen's size) now.
    pub fn encode(&mut self, frame: &Frame, keyframe: bool) -> Result<Vec<EncodedFrame>> {
        self.encode_bgra(frame.pixels(), frame.stride, keyframe)
    }

    /// Converts and encodes B, G, R, X pixels of the screen's size (rows `stride` bytes apart). Returns the frames
    /// that came out: this one (the software encoder is synchronous).
    pub fn encode_bgra(
        &mut self,
        pixels: &[u8],
        stride: usize,
        keyframe: bool,
    ) -> Result<Vec<EncodedFrame>> {
        let needed = stride * self.rows.last().map_or(0, |row| row.second + 1);
        if pixels.len() < needed {
            return Err(Error::new(format!(
                "a frame of {} bytes, {needed} needed",
                pixels.len()
            )));
        }
        let started = Instant::now();
        bgra_to_nv12(pixels, stride, &self.columns, &self.rows, &mut self.nv12);
        let copy_ms = started.elapsed().as_secs_f64() * 1e3;
        let (width, height) = (self.config.width as usize, self.config.height as usize);
        let (y, uv) = self.nv12.split_at(width * height);
        let time = i64::from(self.index) * 10_000_000 / i64::from(self.config.fps);
        let first = self.index == 0;
        self.encoder
            .core
            .encode_nv12(
                y,
                uv,
                width,
                (width, height),
                time,
                keyframe || first,
                copy_ms,
            )
            .map_err(Error::new)?;
        self.index += 1;
        let mut out = Vec::new();
        self.encoder.core.poll(&mut out);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A picture with something in it: a gradient with a box at `offset`.
    fn picture(width: usize, height: usize, offset: usize) -> Vec<u8> {
        let mut pixels = vec![0u8; width * height * 4];
        for y in 0..height {
            for x in 0..width {
                let i = (y * width + x) * 4;
                let inside = (offset..offset + 64).contains(&x) && y < 64;
                let value = if inside { 255 } else { (x * 255 / width) as u8 };
                pixels[i..i + 4].copy_from_slice(&[value, (y * 255 / height) as u8, 128, 255]);
            }
        }
        pixels
    }

    #[test]
    fn frames_come_out_and_decode_at_the_encode_size() {
        // 2560x1440 goes down to the software limit's size.
        let (width, height) = (2560usize, 1440usize);
        let mut encoder =
            ScreenEncoder::new(&Device, &Context, (width as u32, height as u32), 60, false)
                .unwrap();
        let config = encoder.config().clone();
        assert!(config.width <= 1920 && config.height <= 1080, "{config:?}");
        assert_eq!(config.fps, 30);
        assert!(!encoder.notes().is_empty());
        let mut decoder = openh264::decoder::Decoder::new().unwrap();
        let mut decoded = 0;
        for index in 0..5 {
            let frames = encoder
                .encode_bgra(&picture(width, height, index * 40), width * 4, false)
                .unwrap();
            assert_eq!(frames.len(), 1);
            assert_eq!(frames[0].keyframe, index == 0, "frame {index}");
            if let Some(yuv) = decoder.decode(&frames[0].data).unwrap() {
                use openh264::formats::YUVSource;
                assert_eq!(
                    yuv.dimensions(),
                    (config.width as usize, config.height as usize)
                );
                decoded += 1;
            }
        }
        assert_eq!(decoded, 5);
        // The bitrate follows, a kept frame encodes, a short buffer is refused.
        encoder.set_bitrate(3_000_000).unwrap();
        assert_eq!(encoder.bitrate(), 3_000_000);
        assert!(encoder.encode_bgra(&[0u8; 16], width * 4, false).is_err());
        assert!(encoder.encode_kept(false).unwrap().is_empty());
        let timing = encoder.encoder().software_timing().unwrap();
        assert_eq!(timing.frames, 5);
    }

    #[test]
    fn other_codecs_need_a_gpu_encoder_for_now() {
        let error = ScreenEncoder::with_codec(
            &Device,
            &Context,
            (1920, 1080),
            30,
            false,
            None,
            VideoCodec::Av1,
        )
        .err()
        .unwrap();
        assert!(error.message().contains("GPU"), "{error}");
    }
}
