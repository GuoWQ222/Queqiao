//! What an encoder is asked for and what comes out of it, on every system (docs/LINUX.md): moved from encoder.rs,
//! which keeps the Windows encoders.

use crate::format::EncodeFormat;

/// What to encode.
#[derive(Clone, Debug)]
pub struct EncoderConfig {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    /// Target bitrate in bits per second (constant bitrate).
    pub bitrate: u32,
    /// H.264 (every encoder), AV1 (hardware encoders through Media Foundation only), or H.265 (the GPU makers'
    /// encoders, or Media Foundation's for plain H.265).
    pub codec: crate::VideoCodec,
    /// What the GPU makers' encoders make: H.264 4:2:0 unless asked otherwise (its codec agrees with `codec`).
    pub format: EncodeFormat,
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
            format: EncodeFormat::H264,
        }
    }

    /// The codec and format for `format` (H.264 or H.265, its chroma and depth).
    pub fn with_format(self, format: EncodeFormat) -> Self {
        Self {
            codec: match format.codec {
                crate::Codec::H264 => crate::VideoCodec::H264,
                crate::Codec::H265 => crate::VideoCodec::H265,
            },
            format,
            ..self
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
    /// Intel's encoder driven directly (oneVPL, the graphics driver's runtime).
    Vpl,
    /// AMD's encoder driven directly (AMF, the graphics driver's runtime).
    Amf,
    Software,
}

impl EncoderChoice {
    /// `Auto`, unless the test switch `QUEQIAO_FORCE` says `encode:hardware` (or `encode:mf`),
    /// `encode:nvenc` or `encode:software`.
    pub fn from_env() -> Self {
        match queqiao_core::forced("encode").as_deref() {
            Some("hardware" | "mf") => Self::Hardware,
            Some("nvenc") => Self::Nvenc,
            Some("vpl") => Self::Vpl,
            Some("amf") => Self::Amf,
            Some("software") => Self::Software,
            _ => Self::Auto,
        }
    }
}
