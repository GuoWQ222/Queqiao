//! The agent's encoding pipeline on Linux, with the API of Windows' `ScreenEncoder` (docs/LINUX.md): the captured
//! frame (B, G, R, X in memory) -> the encoder's input on the CPU -> the encoder. NVIDIA's NVENC on CUDA first (any
//! size it takes, the screen's own first), OpenH264 within the software limits when no GPU encoder works, as on
//! Windows. Inputs: NV12 at the encode size (cpu_convert.rs: BT.709 limited range, bilinear scaling, one thread per
//! band), or for 4:4:4 the screen's pixels as they are (NVENC converts them, as on Windows). VA-API comes next.

use std::time::Instant;

use queqiao_capture::{BgraImage, Context, Device, Frame};

use crate::config::{EncodedFrame, EncoderChoice, EncoderConfig, SoftwareTiming};
use crate::cpu_convert::{Tap, bgra_to_nv12, bgra_to_p010, bgra_to_rgb10, taps};
use crate::nvenc_cuda::{Input, NvencCuda};
use crate::software_core::SoftwareCore;
use crate::{EncodeFormat, SOFTWARE_START, VideoCodec, candidate_sizes};

/// The CUDA GPU NVENC runs on: the first (the one driving the display on most desktops).
const CUDA_GPU: i32 = 0;

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

enum Backend {
    Nvenc(Box<NvencCuda>),
    /// Boxed: OpenH264's encoder state is several kilobytes.
    Software(Box<SoftwareCore>),
}

/// The encoder a `ScreenEncoder` opened, as Windows' `H264Encoder`.
pub struct H264Encoder {
    backend: Backend,
    skipped: Vec<String>,
}

impl H264Encoder {
    /// Opens an encoder for pictures of `config`'s size: NVENC unless software is forced, then OpenH264 for H.264
    /// unless a GPU encoder is forced. H.265, AV1 (NVENC API 12) and 4:4:4: NVENC only.
    pub fn new(config: &EncoderConfig, choice: EncoderChoice) -> Result<Self> {
        let mut skipped = Vec::new();
        match choice {
            EncoderChoice::Software => {
                skipped.push("hardware encoders: not tried (software forced)".into());
            }
            EncoderChoice::Vpl | EncoderChoice::Amf => {
                return Err(Error::new(format!("{choice:?}: Windows only")));
            }
            _ => match NvencCuda::open(CUDA_GPU, config, config.format) {
                Ok(encoder) => {
                    return Ok(Self {
                        backend: Backend::Nvenc(Box::new(encoder)),
                        skipped,
                    });
                }
                Err(error) => skipped.push(format!("Nvenc: {error}")),
            },
        }
        let software_possible = config.codec == VideoCodec::H264
            && config.format == EncodeFormat::H264
            && matches!(choice, EncoderChoice::Auto | EncoderChoice::Software);
        if software_possible {
            match SoftwareCore::open(config) {
                Ok(core) => {
                    return Ok(Self {
                        backend: Backend::Software(Box::new(core)),
                        skipped,
                    });
                }
                Err(error) => skipped.push(error),
            }
        }
        Err(Error::new(skipped.join("; ")))
    }

    pub fn name(&self) -> &str {
        match &self.backend {
            Backend::Nvenc(encoder) => encoder.name(),
            Backend::Software(core) => &core.name,
        }
    }

    pub fn is_hardware(&self) -> bool {
        matches!(self.backend, Backend::Nvenc(_))
    }

    /// Where a software encoder's time goes.
    pub fn software_timing(&self) -> Option<SoftwareTiming> {
        match &self.backend {
            Backend::Software(core) => Some(core.timing),
            Backend::Nvenc(_) => None,
        }
    }

    /// Encoders tried before this one, each with the reason it was not used.
    pub fn skipped(&self) -> &[String] {
        &self.skipped
    }

    /// Settings the encoder did not accept (none: Media Foundation's list on Windows).
    pub fn refused_settings(&self) -> &[&'static str] {
        &[]
    }

    /// What the encoder makes.
    pub fn format(&self) -> EncodeFormat {
        match &self.backend {
            Backend::Nvenc(encoder) => encoder.format(),
            Backend::Software(_) => EncodeFormat::H264,
        }
    }

    /// What the encoder takes from memory.
    fn input(&self) -> Input {
        match &self.backend {
            Backend::Nvenc(encoder) => encoder.input(),
            Backend::Software(_) => Input::Nv12,
        }
    }

    pub fn set_bitrate(&mut self, bitrate: u32) -> Result<()> {
        match &mut self.backend {
            Backend::Nvenc(encoder) => encoder.set_bitrate(bitrate),
            Backend::Software(core) => core.set_bitrate(bitrate),
        }
        .map_err(Error::new)
    }

    /// Encodes one picture now (both encoders are synchronous) and collects it into `out`. `planes` as `input`
    /// says, rows `pitch` bytes apart; `convert_ms` (how long making them took) goes into the software timing.
    fn encode(
        &mut self,
        planes: &[&[u8]],
        pitch: usize,
        size: (usize, usize),
        (time, keyframe): (i64, bool),
        convert_ms: f64,
        out: &mut Vec<EncodedFrame>,
    ) -> Result<()> {
        match &mut self.backend {
            Backend::Nvenc(encoder) => {
                encoder
                    .encode(planes, pitch, time, keyframe)
                    .map_err(Error::new)?;
                encoder.poll(out);
            }
            Backend::Software(core) => {
                let [y, uv] = planes else {
                    return Err(Error::new("OpenH264 takes NV12"));
                };
                core.encode_nv12(y, uv, pitch, size, time, keyframe, convert_ms)
                    .map_err(Error::new)?;
                core.poll(out);
            }
        }
        Ok(())
    }
}

/// Whether this PC can encode `codec` (as Windows' can_encode): H.264 always (OpenH264 is built in), H.265 and AV1
/// when NVENC opens for it (1280x720 at 60 fps, closed again; AV1 needs NVENC API 12 and a GPU that makes AV1).
pub fn can_encode(codec: VideoCodec, _device: &Device) -> bool {
    match codec {
        VideoCodec::H264 => true,
        VideoCodec::H265 => {
            let format = EncodeFormat::from_name("hevc").unwrap_or(EncodeFormat::H264);
            let config = EncoderConfig {
                codec,
                format,
                ..EncoderConfig::for_screen(1280, 720, 60)
            };
            NvencCuda::open(CUDA_GPU, &config, format).is_ok()
        }
        VideoCodec::Av1 => {
            let config = EncoderConfig {
                codec,
                ..EncoderConfig::for_screen(1280, 720, 60)
            };
            NvencCuda::open(CUDA_GPU, &config, EncodeFormat::H264).is_ok()
        }
    }
}

pub struct ScreenEncoder {
    encoder: H264Encoder,
    config: EncoderConfig,
    screen: (u32, u32),
    columns: Vec<Tap>,
    rows: Vec<Tap>,
    nv12: Vec<u8>,
    /// 10-bit encoding: the converted picture (P010, or R10G10B10A2 words).
    ten_bit: Vec<u8>,
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

    /// As `with_limit`, in `codec` (H.265: NVENC only; AV1: not on Linux yet).
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

    /// As `with_codec`, making `format` (4:4:4 at the screen's own size only: its pixels go to NVENC as they are).
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
        let mut notes = Vec::new();
        let mut choice = EncoderChoice::from_env();
        let mut limit = limit.unwrap_or((u32::MAX, u32::MAX));
        let mut fps = fps.max(1);
        let mut opened = None;
        // As on Windows: the screen's size or the largest that fits the limit; when only the software encoder
        // works, again within its starting limits.
        'attempts: for _ in 0..2 {
            for (w, h) in candidate_sizes(screen.0, screen.1) {
                if w.max(h) > limit.0 || w.min(h) > limit.1 {
                    continue;
                }
                if format.chroma444 && (w, h) != screen {
                    notes.push(format!(
                        "{w}x{h} refused: 4:4:4 is encoded at the screen's size"
                    ));
                    continue;
                }
                let config = EncoderConfig {
                    codec,
                    format,
                    ..EncoderConfig::for_screen(w, h, fps)
                };
                match H264Encoder::new(&config, choice) {
                    Ok(encoder) if encoder.is_hardware() || limit.0 != u32::MAX => {
                        opened = Some((encoder, config));
                        break 'attempts;
                    }
                    Ok(_) => {
                        let (long, short, max_fps) = SOFTWARE_START;
                        limit = (long, short);
                        if !fixed_fps {
                            fps = fps.min(max_fps);
                        }
                        choice = EncoderChoice::Software;
                        continue 'attempts;
                    }
                    Err(error) => notes.push(format!("{w}x{h} refused: {}", error.message())),
                }
            }
        }
        let (encoder, config) =
            opened.ok_or_else(|| Error::new(format!("no encoder: {}", notes.join("; "))))?;
        Ok(ScreenEncoder {
            columns: taps(screen.0, config.width),
            rows: taps(screen.1, config.height),
            screen,
            encoder,
            nv12: Vec::new(),
            ten_bit: Vec::new(),
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
        self.encoder.set_bitrate(bitrate)?;
        self.bitrate = bitrate;
        Ok(())
    }

    pub fn converter_description(&self) -> String {
        let threads = std::thread::available_parallelism().map_or(1, |n| n.get().clamp(1, 8));
        match self.encoder.input() {
            Input::Nv12 => format!("CPU (BT.709, bilinear, {threads} threads)"),
            Input::P010 => format!("CPU (BT.709 in 10 bits, P010, bilinear, {threads} threads)"),
            Input::Bgrx => "none (the encoder converts the screen's pixels)".into(),
            Input::Rgb10 => format!(
                "CPU (the screen's pixels widened to 10 bits a colour, {threads} threads; the encoder converts them)"
            ),
        }
    }

    /// Sizes that were refused before this one was chosen.
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

    /// Converts and encodes `image` (the screen's size, the agent's latest picture) now, as Windows' encode takes the
    /// agent's latest texture.
    pub fn encode(&mut self, image: &BgraImage, keyframe: bool) -> Result<Vec<EncodedFrame>> {
        self.encode_bgra(&image.pixels, image.width as usize * 4, keyframe)
    }

    /// Converts and encodes B, G, R, X pixels of the screen's size (rows `stride` bytes apart). Returns the frames
    /// that came out: this one (both encoders are synchronous).
    pub fn encode_bgra(
        &mut self,
        pixels: &[u8],
        stride: usize,
        keyframe: bool,
    ) -> Result<Vec<EncodedFrame>> {
        let (screen_width, screen_height) = (self.screen.0 as usize, self.screen.1 as usize);
        let needed = stride * screen_height.saturating_sub(1) + screen_width * 4;
        if pixels.len() < needed || stride < screen_width * 4 {
            return Err(Error::new(format!(
                "a frame of {} bytes, {needed} needed",
                pixels.len()
            )));
        }
        let time = i64::from(self.index) * 10_000_000 / i64::from(self.config.fps);
        let picture = (time, keyframe || self.index == 0);
        let size = (self.config.width as usize, self.config.height as usize);
        let mut out = Vec::new();
        match self.encoder.input() {
            Input::Nv12 => {
                let started = Instant::now();
                bgra_to_nv12(pixels, stride, &self.columns, &self.rows, &mut self.nv12);
                let convert_ms = started.elapsed().as_secs_f64() * 1e3;
                let (y, uv) = self.nv12.split_at(size.0 * size.1);
                self.encoder
                    .encode(&[y, uv], size.0, size, picture, convert_ms, &mut out)?;
            }
            Input::P010 => {
                bgra_to_p010(pixels, stride, &self.columns, &self.rows, &mut self.ten_bit);
                let (y, uv) = self.ten_bit.split_at(size.0 * size.1 * 2);
                self.encoder
                    .encode(&[y, uv], size.0 * 2, size, picture, 0.0, &mut out)?;
            }
            Input::Bgrx => {
                self.encoder
                    .encode(&[pixels], stride, size, picture, 0.0, &mut out)?;
            }
            Input::Rgb10 => {
                // 4:4:4 is encoded at the screen's size: the pixels as they are, widened.
                bgra_to_rgb10(
                    pixels,
                    stride,
                    screen_width,
                    screen_height,
                    &mut self.ten_bit,
                );
                self.encoder.encode(
                    &[&self.ten_bit],
                    screen_width * 4,
                    size,
                    picture,
                    0.0,
                    &mut out,
                )?;
            }
        }
        self.index += 1;
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

    /// Encodes 5 frames and decodes them with OpenH264 (H.264 only).
    fn encode_and_decode(encoder: &mut ScreenEncoder, (width, height): (usize, usize)) {
        let config = encoder.config().clone();
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
    }

    #[test]
    fn software_frames_come_out_and_decode_at_the_encode_size() {
        // 2560x1440 goes down to the software limit's size.
        let (width, height) = (2560usize, 1440usize);
        let config = EncoderConfig::for_screen(width as u32, height as u32, 60);
        let mut encoder = ScreenEncoder {
            encoder: H264Encoder {
                backend: Backend::Software(Box::new(
                    SoftwareCore::open(&EncoderConfig::for_screen(1920, 1080, 30)).unwrap(),
                )),
                skipped: Vec::new(),
            },
            config: EncoderConfig {
                width: 1920,
                height: 1080,
                fps: 30,
                ..config
            },
            screen: (width as u32, height as u32),
            columns: taps(width as u32, 1920),
            rows: taps(height as u32, 1080),
            nv12: Vec::new(),
            ten_bit: Vec::new(),
            kept: Vec::new(),
            kept_stride: 0,
            index: 0,
            notes: Vec::new(),
            bitrate: 0,
        };
        encode_and_decode(&mut encoder, (width, height));
        // The bitrate follows, a short buffer is refused, nothing is kept yet.
        encoder.set_bitrate(3_000_000).unwrap();
        assert_eq!(encoder.bitrate(), 3_000_000);
        assert!(encoder.encode_bgra(&[0u8; 16], width * 4, false).is_err());
        assert!(encoder.encode_kept(false).unwrap().is_empty());
        let timing = encoder.encoder().software_timing().unwrap();
        assert_eq!(timing.frames, 5);
    }

    #[test]
    fn the_chosen_encoder_encodes_the_screen() {
        // NVENC where the machine has it (at the screen's size), else OpenH264 within its limits.
        let (width, height) = (2560usize, 1440usize);
        let mut encoder =
            ScreenEncoder::new(&Device, &Context, (width as u32, height as u32), 60, false)
                .unwrap();
        let config = encoder.config().clone();
        eprintln!(
            "{}: {}x{} at {} fps; {:?}",
            encoder.encoder().name(),
            config.width,
            config.height,
            config.fps,
            encoder.encoder().skipped()
        );
        if encoder.encoder().is_hardware() {
            assert_eq!((config.width, config.height, config.fps), (2560, 1440, 60));
        } else {
            assert!(config.width <= 1920 && config.height <= 1080, "{config:?}");
            assert_eq!(config.fps, 30);
            assert!(!encoder.notes().is_empty() || config.width == 1920);
        }
        encode_and_decode(&mut encoder, (width, height));
        encoder.set_bitrate(3_000_000).unwrap();
        let frames = encoder
            .encode_bgra(&picture(width, height, 7), width * 4, true)
            .unwrap();
        assert!(frames[0].keyframe);
    }

    /// AV1 where NVENC makes it (API 12 and a GPU with AV1): its keyframe carries the sequence header (what a decoder
    /// starts from), the next frame does not. Elsewhere the error says AV1.
    #[test]
    fn av1_where_nvenc_makes_it() {
        let made = ScreenEncoder::with_codec(
            &Device,
            &Context,
            (1920, 1080),
            30,
            false,
            None,
            VideoCodec::Av1,
        );
        if !can_encode(VideoCodec::Av1, &Device) {
            let error = made.err().unwrap();
            assert!(error.message().contains("AV1"), "{error}");
            return;
        }
        let mut encoder = made.unwrap();
        assert!(
            encoder.encoder().name().contains("NVENC AV1"),
            "{}",
            encoder.encoder().name()
        );
        let key = encoder
            .encode_bgra(&picture(1920, 1080, 0), 1920 * 4, true)
            .unwrap();
        assert!(key[0].keyframe);
        assert!(crate::is_keyframe(VideoCodec::Av1, &key[0].data));
        let next = encoder
            .encode_bgra(&picture(1920, 1080, 32), 1920 * 4, false)
            .unwrap();
        assert!(!next[0].keyframe);
        assert!(!crate::is_keyframe(VideoCodec::Av1, &next[0].data));
    }

    #[test]
    fn nvenc_makes_each_8bit_format_where_there_is_one() {
        let probe = NvencCuda::open(
            CUDA_GPU,
            &EncoderConfig::for_screen(640, 480, 30),
            EncodeFormat::H264,
        );
        if let Err(error) = probe {
            eprintln!("skipped: no NVENC here ({error})");
            return;
        }
        let (width, height) = (1920usize, 1080usize);
        for name in ["h264", "hevc", "h264_444", "hevc444"] {
            let format = EncodeFormat::from_name(name).unwrap();
            let codec = match format.codec {
                crate::Codec::H264 => VideoCodec::H264,
                crate::Codec::H265 => VideoCodec::H265,
            };
            let mut encoder = ScreenEncoder::with_format(
                &Device,
                &Context,
                (width as u32, height as u32),
                60,
                false,
                None,
                codec,
                format,
            )
            .unwrap_or_else(|e| panic!("{name}: {e}"));
            assert!(encoder.encoder().is_hardware(), "{name}");
            assert_eq!(encoder.format(), format, "{name}");
            for index in 0..3 {
                let frames = encoder
                    .encode_bgra(&picture(width, height, index * 40), width * 4, false)
                    .unwrap();
                assert_eq!(frames.len(), 1, "{name}");
                assert_eq!(frames[0].keyframe, index == 0, "{name} frame {index}");
                if index == 0 {
                    let shape = crate::sps::shape(format.codec, &frames[0].data);
                    let expected = crate::sps::Shape {
                        width: width as u32,
                        height: height as u32,
                        chroma444: format.chroma444,
                        bit_depth: 8,
                    };
                    assert_eq!(shape, Some(expected), "{name}");
                }
            }
        }
    }
}
