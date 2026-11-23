//! The agent's encoding pipeline in one piece: BGRA screen texture -> NV12 at the encode size -> H.264.
//! It picks the encode size, rate and encoder for the machine: the screen's own size when a hardware
//! encoder (any vendor) accepts it, otherwise smaller sizes; OpenH264 within its starting limits when no
//! hardware encoder works.

use std::time::{Duration, Instant};

use windows::Win32::Foundation::E_FAIL;
use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D};
use windows::core::{Error, Result};

use crate::{
    EncodedFrame, EncoderChoice, EncoderConfig, H264Encoder, Nv12Converter, candidate_sizes,
};

/// Software encoders (on the CPU) start at no more than this size (long side, short side) and rate.
pub const SOFTWARE_START: (u32, u32, u32) = (1920, 1080, 30);

pub struct ScreenEncoder {
    encoder: H264Encoder,
    converter: Nv12Converter,
    ring: Vec<ID3D11Texture2D>,
    config: EncoderConfig,
    index: u32,
    notes: Vec<String>,
    bitrate: u32,
}

impl ScreenEncoder {
    /// An encoder for a screen of `screen` pixels at up to `fps` frames per second. With `fixed_fps`
    /// the rate is kept even for a software encoder (normally capped at 30).
    pub fn new(
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
        screen: (u32, u32),
        fps: u32,
        fixed_fps: bool,
    ) -> Result<Self> {
        Self::with_limit(device, context, screen, fps, fixed_fps, None)
    }

    /// As `new`, with the picture no larger than `limit` (long side, short side), e.g. (1280, 720).
    pub fn with_limit(
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
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
            crate::VideoCodec::H264,
        )
    }

    /// As `with_limit`, in `codec` (AV1: the GPU's encoder only, no software fallback).
    pub fn with_codec(
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
        screen: (u32, u32),
        fps: u32,
        fixed_fps: bool,
        limit: Option<(u32, u32)>,
        codec: crate::VideoCodec,
    ) -> Result<Self> {
        let format = match codec {
            crate::VideoCodec::H265 => {
                crate::EncodeFormat::from_name("hevc").unwrap_or(crate::EncodeFormat::H264)
            }
            _ => crate::EncodeFormat::H264,
        };
        Self::with_format(
            device, context, screen, fps, fixed_fps, limit, codec, format,
        )
    }

    /// As `with_codec`, with the GPU makers' encoders making `format` (H.265 4:4:4, 10-bit...).
    #[allow(clippy::too_many_arguments)]
    pub fn with_format(
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
        screen: (u32, u32),
        fps: u32,
        fixed_fps: bool,
        limit: Option<(u32, u32)>,
        codec: crate::VideoCodec,
        format: crate::EncodeFormat,
    ) -> Result<Self> {
        let mut notes = Vec::new();
        let mut choice = EncoderChoice::from_env();
        let mut limit = limit.unwrap_or((u32::MAX, u32::MAX));
        let mut fps = fps.max(1);
        let mut opened = None;
        'attempts: for _ in 0..2 {
            for (w, h) in candidate_sizes(screen.0, screen.1) {
                if w.max(h) > limit.0 || w.min(h) > limit.1 {
                    continue;
                }
                let config = EncoderConfig {
                    codec,
                    format,
                    ..EncoderConfig::for_screen(w, h, fps)
                };
                match H264Encoder::new(device, context, &config, choice) {
                    Ok(encoder) if encoder.is_hardware() || limit.0 != u32::MAX => {
                        opened = Some((encoder, config));
                        break 'attempts;
                    }
                    Ok(_) => {
                        // Software: open again within the software starting limits.
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
        let (encoder, config) = opened
            .ok_or_else(|| Error::new(E_FAIL, format!("no encoder: {}", notes.join("; "))))?;
        let size = (config.width, config.height);
        // NV12, or what a vendor encoder set to another format takes (P010, AYUV, Y410, RGB).
        let converter = Nv12Converter::with_format(
            device,
            context,
            screen,
            size,
            config.fps,
            encoder.input_format(),
        )?;
        let ring = (0..4)
            .map(|_| converter.create_output(device))
            .collect::<Result<Vec<_>>>()?;
        Ok(Self {
            encoder,
            converter,
            ring,
            bitrate: config.bitrate,
            config,
            index: 0,
            notes,
        })
    }

    pub fn codec(&self) -> crate::VideoCodec {
        self.config.codec
    }

    /// What the encoder makes (H.264 4:2:0 but for the GPU makers' encoders set otherwise).
    pub fn format(&self) -> crate::EncodeFormat {
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
        self.converter.description()
    }

    /// Sizes that were refused before this one was chosen.
    pub fn notes(&self) -> &[String] {
        &self.notes
    }

    /// Converts and encodes `bgra` (the screen's size). Returns the frames that came out, normally just
    /// this one (hardware encoders deliver asynchronously: this waits up to 100 ms for it).
    pub fn encode(&mut self, bgra: &ID3D11Texture2D, keyframe: bool) -> Result<Vec<EncodedFrame>> {
        let nv12 = &self.ring[self.index as usize % self.ring.len()];
        self.converter.convert(bgra, nv12)?;
        let mut out = Vec::new();
        let deadline = Instant::now() + Duration::from_millis(500);
        while !self.encoder.wants_input() {
            self.encoder.poll(&mut out)?;
            if Instant::now() > deadline {
                return Err(Error::new(E_FAIL, "the encoder took no input for 500 ms"));
            }
            std::thread::yield_now();
        }
        let time = i64::from(self.index) * 10_000_000 / i64::from(self.config.fps);
        let first = self.index == 0;
        self.encoder.encode(nv12, time, keyframe || first)?;
        self.index += 1;
        let deadline = Instant::now() + Duration::from_millis(100);
        loop {
            self.encoder.poll(&mut out)?;
            if out.iter().any(|frame| frame.time == time) || Instant::now() > deadline {
                return Ok(out);
            }
            std::thread::yield_now();
        }
    }
}
