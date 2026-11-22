//! The H.264 decoder used by the viewer. First choice: Windows' H.264 decoder with D3D11 video
//! acceleration (it decodes on the GPU of any vendor). Fallback: OpenH264 on the CPU, as UU Remote does.
//! Both hand out NV12 textures on our device. `QUEQIAO_FORCE=decode:software` or `decode:hardware`
//! forces one kind.

use windows::Win32::Foundation::E_FAIL;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_NV12, DXGI_SAMPLE_DESC};
use windows::core::{Error, Result};

use crate::mf_decode::MfDecoder;
use crate::software_decode::SoftwareDecoder;

/// Which kind of decoder to use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecoderChoice {
    /// GPU first, OpenH264 if the GPU cannot decode.
    Auto,
    Hardware,
    Software,
}

impl DecoderChoice {
    /// `Auto`, unless the test switch `QUEQIAO_FORCE` says `decode:hardware` or `decode:software`.
    pub fn from_env() -> Self {
        match queqiao_core::forced("decode").as_deref() {
            Some("hardware") => Self::Hardware,
            Some("software") => Self::Software,
            _ => Self::Auto,
        }
    }
}

/// One decoded picture.
pub struct DecodedFrame {
    /// NV12, the picture's size. Owned by the decoder and reused after [`RING`] more frames.
    pub texture: ID3D11Texture2D,
    pub width: u32,
    pub height: u32,
    /// The time given to [`H264Decoder::decode`] with the frame's data, in 100 ns units.
    pub time: i64,
}

/// How many decoded textures exist before the first one is written again.
pub const RING: usize = 4;

enum Backend {
    Hardware(Box<MfDecoder>),
    Software(Box<SoftwareDecoder>),
}

pub struct H264Decoder {
    backend: Backend,
    skipped: Vec<String>,
}

impl H264Decoder {
    pub fn new(
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
        choice: DecoderChoice,
    ) -> Result<Self> {
        Self::with_codec(crate::VideoCodec::H264, device, context, choice)
    }

    /// A decoder for `codec`: H.264 as `new`; AV1 through Windows' AV1 decoder only (no software fallback built in).
    pub fn with_codec(
        codec: crate::VideoCodec,
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
        choice: DecoderChoice,
    ) -> Result<Self> {
        let mut skipped = Vec::new();
        if codec != crate::VideoCodec::H264 {
            return MfDecoder::open(device, context, codec)
                .map(|decoder| Self {
                    backend: Backend::Hardware(Box::new(decoder)),
                    skipped: Vec::new(),
                })
                .map_err(|error| {
                    Error::new(
                        E_FAIL,
                        format!("{} decoding: {}", codec.name(), error.message()),
                    )
                });
        }
        if choice == DecoderChoice::Software {
            skipped.push("GPU decoding: not tried (software forced)".into());
        } else {
            match MfDecoder::open(device, context, codec) {
                Ok(decoder) => {
                    return Ok(Self {
                        backend: Backend::Hardware(Box::new(decoder)),
                        skipped,
                    });
                }
                Err(error) => skipped.push(format!("GPU decoding: {}", error.message())),
            }
        }
        if choice != DecoderChoice::Hardware {
            match SoftwareDecoder::open(device, context) {
                Ok(decoder) => {
                    return Ok(Self {
                        backend: Backend::Software(Box::new(decoder)),
                        skipped,
                    });
                }
                Err(error) => skipped.push(error.message()),
            }
        }
        Err(Error::new(E_FAIL, skipped.join("; ")))
    }

    pub fn name(&self) -> &str {
        match &self.backend {
            Backend::Hardware(decoder) => &decoder.name,
            Backend::Software(decoder) => &decoder.name,
        }
    }

    /// True when frames are decoded on the GPU (the Windows decoder can also fall back to the CPU
    /// inside; then its frames arrive in memory and this turns false).
    pub fn is_hardware(&self) -> bool {
        match &self.backend {
            Backend::Hardware(decoder) => decoder.cpu_frames == 0,
            Backend::Software(_) => false,
        }
    }

    /// Decoders tried before this one, each with the reason it was not used.
    pub fn skipped(&self) -> &[String] {
        &self.skipped
    }

    /// Decodes one access unit (Annex-B H.264, as the encoder produced it) and appends the pictures
    /// that are ready to `out`.
    pub fn decode(&mut self, data: &[u8], time: i64, out: &mut Vec<DecodedFrame>) -> Result<()> {
        match &mut self.backend {
            Backend::Hardware(decoder) => decoder.decode(data, time, out),
            Backend::Software(decoder) => decoder.decode(data, time, out),
        }
    }
}

/// A few NV12 textures of one size, handed out in turn.
pub(crate) struct Nv12Ring {
    textures: Vec<ID3D11Texture2D>,
    next: usize,
    size: (u32, u32),
}

impl Nv12Ring {
    pub(crate) fn new() -> Self {
        Self {
            textures: Vec::new(),
            next: 0,
            size: (0, 0),
        }
    }

    pub(crate) fn next(
        &mut self,
        device: &ID3D11Device,
        size: (u32, u32),
    ) -> Result<ID3D11Texture2D> {
        if self.size != size || self.textures.is_empty() {
            let desc = D3D11_TEXTURE2D_DESC {
                Width: size.0,
                Height: size.1,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_NV12,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: 0,
                CPUAccessFlags: 0,
                MiscFlags: 0,
            };
            self.textures.clear();
            for _ in 0..RING {
                let mut out: Option<ID3D11Texture2D> = None;
                unsafe { device.CreateTexture2D(&desc, None, Some(&mut out)) }?;
                self.textures.extend(out);
            }
            self.size = size;
            self.next = 0;
        }
        let texture = self.textures[self.next % self.textures.len()].clone();
        self.next = (self.next + 1) % self.textures.len();
        Ok(texture)
    }
}
