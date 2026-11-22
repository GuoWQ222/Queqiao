//! The H.264 encoder used by the agent. First choice: the GPU's hardware encoder through Media
//! Foundation (any vendor). Fallback: OpenH264 on the CPU, which works on every PC. UU Remote uses the
//! same two kinds (hardware encoders first, OpenH264 last). `QUEQIAO_FORCE=encode:software` or
//! `encode:hardware` forces one kind, so both paths can be tested on any machine.

use windows::Win32::Foundation::E_FAIL;
use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D};
use windows::core::{Error, Result};

use crate::mf::MfEncoder;
use crate::nvenc::NvencEncoder;
use crate::software::SoftwareEncoder;

/// What to encode.
#[derive(Clone, Debug)]
pub struct EncoderConfig {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    /// Target bitrate in bits per second (constant bitrate).
    pub bitrate: u32,
    /// H.264 (every encoder), or AV1 (hardware encoders through Media Foundation only).
    pub codec: crate::VideoCodec,
}

impl EncoderConfig {
    /// Starting settings for a picture of this size and rate. The bitrate suits desktop content (about
    /// 0.1 bit per pixel per frame, kept between 2 and 50 Mbit/s); the network's bandwidth estimate
    /// replaces it once a session runs.
    pub fn for_screen(width: u32, height: u32, fps: u32) -> Self {
        let bits = f64::from(width) * f64::from(height) * f64::from(fps) * 0.1;
        Self {
            width,
            height,
            fps,
            bitrate: bits.clamp(2e6, 50e6) as u32,
            codec: crate::VideoCodec::H264,
        }
    }
}

/// One encoded frame.
pub struct EncodedFrame {
    /// Annex-B H.264 (NAL units, each after a 00 00 00 01 or 00 00 01 start code), or an AV1 temporal unit (OBUs).
    pub data: Vec<u8>,
    /// The sample time given to [`H264Encoder::encode`], in 100 ns units.
    pub time: i64,
    /// An I or IDR frame: a decoder can start here.
    pub keyframe: bool,
}

/// Where a software encoder's time goes, summed over all frames so far.
#[derive(Clone, Copy, Debug, Default)]
pub struct SoftwareTiming {
    pub frames: u32,
    /// GPU texture -> memory, and splitting U and V for OpenH264.
    pub copy_ms: f64,
    /// OpenH264 itself.
    pub encode_ms: f64,
}

/// Which kind of encoder to use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EncoderChoice {
    /// Hardware first, software if no hardware encoder works.
    Auto,
    /// Hardware only, through Media Foundation (any vendor).
    Hardware,
    /// NVIDIA's encoder driven directly (NVENC SDK), for comparison with Media Foundation.
    Nvenc,
    Software,
}

impl EncoderChoice {
    /// `Auto`, unless the test switch `QUEQIAO_FORCE` says `encode:hardware` (or `encode:mf`),
    /// `encode:nvenc` or `encode:software`.
    pub fn from_env() -> Self {
        match queqiao_core::forced("encode").as_deref() {
            Some("hardware" | "mf") => Self::Hardware,
            Some("nvenc") => Self::Nvenc,
            Some("software") => Self::Software,
            _ => Self::Auto,
        }
    }
}

enum Backend {
    Hardware(MfEncoder),
    Nvenc(Box<NvencEncoder>),
    /// Boxed: OpenH264's encoder state is several kilobytes.
    Software(Box<SoftwareEncoder>),
}

pub struct H264Encoder {
    backend: Backend,
    skipped: Vec<String>,
}

impl H264Encoder {
    /// Opens an encoder for NV12 textures of `config`'s size on `device`.
    pub fn new(
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
        config: &EncoderConfig,
        choice: EncoderChoice,
    ) -> Result<Self> {
        let mut skipped = Vec::new();
        // AV1: only the GPU's encoder through Media Foundation.
        if config.codec != crate::VideoCodec::H264 {
            return MfEncoder::open(device, config, &mut skipped)
                .map(|encoder| Self {
                    backend: Backend::Hardware(encoder),
                    skipped: Vec::new(),
                })
                .ok_or_else(|| Error::new(E_FAIL, skipped.join("; ")));
        }
        if choice == EncoderChoice::Nvenc {
            let encoder = NvencEncoder::open(device, config)?;
            return Ok(Self {
                backend: Backend::Nvenc(Box::new(encoder)),
                skipped,
            });
        }
        if choice == EncoderChoice::Software {
            skipped.push("hardware encoders: not tried (software forced)".into());
        } else if let Some(encoder) = MfEncoder::open(device, config, &mut skipped) {
            return Ok(Self {
                backend: Backend::Hardware(encoder),
                skipped,
            });
        }
        if choice != EncoderChoice::Hardware {
            match SoftwareEncoder::open(device, context, config) {
                Ok(encoder) => {
                    return Ok(Self {
                        backend: Backend::Software(Box::new(encoder)),
                        skipped,
                    });
                }
                Err(error) => skipped.push(error.message()),
            }
        }
        Err(Error::new(E_FAIL, skipped.join("; ")))
    }

    /// The encoder's name, e.g. "NVIDIA H.264 Encoder MFT" or "OpenH264 (software, 4 threads)".
    pub fn name(&self) -> &str {
        match &self.backend {
            Backend::Hardware(encoder) => &encoder.name,
            Backend::Nvenc(encoder) => &encoder.name,
            Backend::Software(encoder) => &encoder.name,
        }
    }

    pub fn is_hardware(&self) -> bool {
        !matches!(self.backend, Backend::Software(_))
    }

    /// For the software encoder: where its time went (none for hardware encoders).
    pub fn software_timing(&self) -> Option<SoftwareTiming> {
        match &self.backend {
            Backend::Software(encoder) => Some(encoder.timing),
            _ => None,
        }
    }

    /// Encoders tried before this one, each with the reason it was not used.
    pub fn skipped(&self) -> &[String] {
        &self.skipped
    }

    /// Settings the encoder did not accept (it then uses its own default for them).
    pub fn refused_settings(&self) -> &[&'static str] {
        match &self.backend {
            Backend::Hardware(encoder) => &encoder.refused,
            _ => &[],
        }
    }

    /// True if the encoder takes another frame now.
    pub fn wants_input(&self) -> bool {
        match &self.backend {
            Backend::Hardware(encoder) => encoder.wants_input(),
            _ => true,
        }
    }

    /// Collects finished frames into `out` without waiting.
    pub fn poll(&mut self, out: &mut Vec<EncodedFrame>) -> Result<()> {
        match &mut self.backend {
            Backend::Hardware(encoder) => encoder.poll(out),
            Backend::Nvenc(encoder) => {
                encoder.poll(out);
                Ok(())
            }
            Backend::Software(encoder) => {
                encoder.poll(out);
                Ok(())
            }
        }
    }

    /// Sends one NV12 frame (only when [`H264Encoder::wants_input`]). `time` is in 100 ns units;
    /// `keyframe` forces an IDR frame.
    pub fn encode(&mut self, nv12: &ID3D11Texture2D, time: i64, keyframe: bool) -> Result<()> {
        match &mut self.backend {
            Backend::Hardware(encoder) => encoder.encode(nv12, time, keyframe),
            Backend::Nvenc(encoder) => encoder.encode(nv12, time, keyframe),
            Backend::Software(encoder) => encoder.encode(nv12, time, keyframe),
        }
    }

    /// Changes the bitrate (bits per second) for the next frames, without a keyframe.
    pub fn set_bitrate(&mut self, bitrate: u32) -> Result<()> {
        match &mut self.backend {
            Backend::Hardware(encoder) => encoder.set_bitrate(bitrate),
            Backend::Nvenc(encoder) => encoder.set_bitrate(bitrate),
            Backend::Software(encoder) => encoder.set_bitrate(bitrate),
        }
    }

    /// Asks for every frame still inside the encoder; keep calling `poll` until `drained`.
    pub fn drain(&mut self) -> Result<()> {
        match &mut self.backend {
            Backend::Hardware(encoder) => encoder.drain(),
            _ => Ok(()),
        }
    }

    pub fn drained(&self) -> bool {
        match &self.backend {
            Backend::Hardware(encoder) => encoder.drained,
            _ => true,
        }
    }
}
