//! The viewer's decoder on Linux (docs/LINUX.md), with the API of Windows' `H264Decoder`: NVIDIA's NVDEC on CUDA
//! (nvdec_linux.rs: H.264, H.265, AV1) where there is one, else OpenH264 on the CPU for H.264 (as Windows uses when no
//! GPU decoder works). Either way the pictures are NV12 in memory, which the presenter draws. VA-API comes next.

use openh264::OpenH264API;
use openh264::decoder::{Decoder, DecoderConfig};
use openh264::formats::YUVSource;
use queqiao_capture::{Context, Device};

use crate::VideoCodec;
use crate::config::DecoderChoice;
use crate::linux_screen::{Error, Result};

/// How many decoded pictures may be held at once (as Windows' ring of textures; here each frame owns its picture).
pub const RING: usize = 4;

/// How a picture's samples lie in memory (each plane `rows` rows, rows `pitch` bytes apart).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PictureFormat {
    /// 8-bit luma, then 8-bit U V pairs, half as many rows (OpenH264's and NVDEC's 4:2:0).
    Nv12,
    /// 16-bit samples, 10 bits in the high bits, as NV12 (NVDEC's 10-bit 4:2:0).
    P016,
    /// Three 8-bit planes: Y, U, V (NVDEC's 4:4:4).
    Yuv444,
    /// Three 16-bit planes, 10 bits in the high bits (NVDEC's 10-bit 4:4:4).
    Yuv444P16,
}

/// A decoded picture in memory (Windows: a texture).
pub struct Picture {
    pub data: Vec<u8>,
    pub pitch: usize,
    pub rows: usize,
    pub format: PictureFormat,
}

/// One decoded picture.
pub struct DecodedFrame {
    /// The picture (named as Windows' texture, so the viewer reads the same).
    pub texture: Picture,
    pub width: u32,
    pub height: u32,
    /// The time given to [`H264Decoder::decode`] with the frame's data, in 100 ns units.
    pub time: i64,
}

enum Inner {
    OpenH264(Decoder),
    Nvdec(crate::nvdec_linux::NvdecDecoder),
}

pub struct H264Decoder {
    inner: Inner,
    name: String,
    skipped: Vec<String>,
}

impl H264Decoder {
    pub fn new(device: &Device, context: &Context, choice: DecoderChoice) -> Result<Self> {
        Self::with_codec(VideoCodec::H264, device, context, choice)
    }

    /// A decoder for `codec`: NVDEC where the GPU has it (Auto, Hardware, Nvdec), else OpenH264 for H.264 (Auto,
    /// Software). H.265 and AV1 need NVDEC.
    pub fn with_codec(
        codec: VideoCodec,
        _device: &Device,
        _context: &Context,
        choice: DecoderChoice,
    ) -> Result<Self> {
        let mut skipped = Vec::new();
        if matches!(
            choice,
            DecoderChoice::Auto | DecoderChoice::Hardware | DecoderChoice::Nvdec
        ) {
            match crate::nvdec_linux::NvdecDecoder::new(codec) {
                Ok(decoder) => {
                    return Ok(Self {
                        name: decoder.name.clone(),
                        inner: Inner::Nvdec(decoder),
                        skipped,
                    });
                }
                Err(error) if choice == DecoderChoice::Auto => skipped.push(error),
                Err(error) => return Err(Error::new(error)),
            }
        }
        match choice {
            DecoderChoice::Auto | DecoderChoice::Software => {}
            other => {
                return Err(Error::new(format!(
                    "{other:?}: no such decoder on Linux yet"
                )));
            }
        }
        if codec != VideoCodec::H264 {
            return Err(Error::new(format!(
                "{codec:?}: no decoder for it here ({})",
                skipped.join("; ")
            )));
        }
        let decoder = Decoder::with_api_config(OpenH264API::from_source(), DecoderConfig::new())
            .map_err(|e| Error::new(format!("OpenH264 decoder: {e}")))?;
        Ok(Self {
            inner: Inner::OpenH264(decoder),
            name: "OpenH264 decoder (software)".into(),
            skipped,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// What the stream is, as the decoder sees it.
    pub fn stream(&self) -> String {
        match &self.inner {
            Inner::OpenH264(_) => "H.264 4:2:0".into(),
            Inner::Nvdec(decoder) => decoder.stream(),
        }
    }

    pub fn is_hardware(&self) -> bool {
        matches!(self.inner, Inner::Nvdec(_))
    }

    /// Decoders tried before this one, each with the reason it was not used.
    pub fn skipped(&self) -> &[String] {
        &self.skipped
    }

    /// Decodes one frame's data; the picture (if one came out) goes into `out`.
    pub fn decode(&mut self, data: &[u8], time: i64, out: &mut Vec<DecodedFrame>) -> Result<()> {
        let decoder = match &mut self.inner {
            Inner::Nvdec(decoder) => return decoder.decode(data, time, out).map_err(Error::new),
            Inner::OpenH264(decoder) => decoder,
        };
        let decoded = decoder
            .decode(data)
            .map_err(|e| Error::new(format!("OpenH264 decoder: {e}")))?;
        let Some(yuv) = decoded else {
            return Ok(());
        };
        let (width, height) = yuv.dimensions();
        let (y_stride, u_stride, v_stride) = yuv.strides();
        let mut nv12 = Vec::with_capacity(width * height * 3 / 2);
        for row in yuv.y().chunks(y_stride).take(height) {
            nv12.extend_from_slice(&row[..width]);
        }
        let rows = yuv.u().chunks(u_stride).zip(yuv.v().chunks(v_stride));
        for (u_row, v_row) in rows.take(height / 2) {
            for (&u, &v) in u_row[..width / 2].iter().zip(&v_row[..width / 2]) {
                nv12.push(u);
                nv12.push(v);
            }
        }
        out.push(DecodedFrame {
            texture: Picture {
                data: nv12,
                pitch: width,
                rows: height,
                format: PictureFormat::Nv12,
            },
            width: width as u32,
            height: height as u32,
            time,
        });
        Ok(())
    }
}

/// Whether this PC can decode `codec`: H.264 always (OpenH264 is built in), H.265 with NVDEC. AV1 not yet: NVDEC decodes
/// it (nvdec_linux.rs), but no AV1 stream has been through it end to end here (Linux makes none yet), so the viewer
/// does not ask for it.
pub fn can_decode(codec: VideoCodec) -> bool {
    let caps = crate::nvdec_linux::caps();
    match codec {
        VideoCodec::H264 => true,
        VideoCodec::H265 => caps.is_some_and(|caps| caps.hevc != 0),
        VideoCodec::Av1 => false,
    }
}

/// Whether the GPU decodes AV1 itself: not offered yet (see `can_decode`).
pub fn av1_decodes_on_gpu(_device: &Device) -> bool {
    false
}

/// What H.265 this PC decodes (`protocol::hevc` bits): what NVDEC decodes (Main, Main 10, 4:4:4, 4:4:4 10-bit).
pub fn hevc_decode_caps(_device: &Device, _context: &Context) -> u8 {
    crate::nvdec_linux::caps().map_or(0, |caps| caps.hevc)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::linux_screen::ScreenEncoder;

    #[test]
    fn encoded_frames_decode_to_nv12_in_memory() {
        let (width, height) = (640usize, 360usize);
        let mut encoder =
            ScreenEncoder::new(&Device, &Context, (width as u32, height as u32), 30, false)
                .unwrap();
        let mut decoder = H264Decoder::new(&Device, &Context, DecoderChoice::Software).unwrap();
        // Mid grey: Y 126, U V 128 in BT.709 limited range.
        let pixels = vec![128u8; width * height * 4];
        let mut out = Vec::new();
        for _ in 0..3 {
            for frame in encoder.encode_bgra(&pixels, width * 4, false).unwrap() {
                decoder.decode(&frame.data, frame.time, &mut out).unwrap();
            }
        }
        assert_eq!(out.len(), 3);
        let picture = &out[2].texture;
        assert_eq!((out[2].width, out[2].height), (640, 360));
        assert_eq!(picture.data.len(), width * height * 3 / 2);
        let y = picture.data[width * height / 2 + width / 2];
        let uv = &picture.data[width * height..width * height + 2];
        assert!((i32::from(y) - 126).abs() <= 2, "Y {y}");
        assert!(
            uv.iter().all(|&c| (i32::from(c) - 128).abs() <= 2),
            "UV {uv:?}"
        );
        // H.265 only where NVDEC decodes it.
        assert_eq!(
            H264Decoder::with_codec(VideoCodec::H265, &Device, &Context, DecoderChoice::Auto)
                .is_ok(),
            can_decode(VideoCodec::H265)
        );
    }

    /// NVDEC, where this PC has it: H.264, H.265 and H.265 4:4:4 from the encoder decode to the grey they were made of,
    /// in the layout NVDEC gives (NV12, or three planes for 4:4:4). Without an NVIDIA GPU the test says so and passes
    /// (the software path is tested above).
    #[test]
    fn nvdec_decodes_into_memory() {
        let Some(caps) = crate::nvdec_linux::caps() else {
            eprintln!("no NVDEC on this PC: skipped");
            return;
        };
        let (width, height) = (1280usize, 720usize);
        let hevc444 = caps.hevc & queqiao_core::protocol::hevc::CHROMA444 != 0;
        for (name, supported) in [
            ("h264", caps.h264),
            ("hevc", caps.hevc != 0),
            ("hevc444", hevc444),
        ] {
            if !supported {
                continue;
            }
            let format = crate::EncodeFormat::from_name(name).unwrap();
            let codec = match format.codec {
                crate::Codec::H264 => VideoCodec::H264,
                crate::Codec::H265 => VideoCodec::H265,
            };
            let Ok(mut encoder) = ScreenEncoder::with_format(
                &Device,
                &Context,
                (width as u32, height as u32),
                30,
                false,
                None,
                codec,
                format,
            ) else {
                eprintln!("{name}: no encoder here: skipped");
                continue;
            };
            let mut decoder =
                H264Decoder::with_codec(codec, &Device, &Context, DecoderChoice::Nvdec).unwrap();
            assert!(decoder.is_hardware(), "{}", decoder.name());
            let pixels = vec![128u8; width * height * 4];
            let mut out = Vec::new();
            for index in 0..5 {
                for frame in encoder.encode_bgra(&pixels, width * 4, index == 0).unwrap() {
                    decoder.decode(&frame.data, frame.time, &mut out).unwrap();
                }
            }
            assert!(out.len() >= 4, "{name}: {} pictures", out.len());
            let last = out.last().unwrap();
            assert_eq!((last.width, last.height), (1280, 720), "{name}");
            let picture = &last.texture;
            let plane = width * height;
            let y = picture.data[plane / 2 + width / 2];
            let (u, v) = if format.chroma444 {
                assert_eq!(picture.format, PictureFormat::Yuv444, "{name}");
                assert_eq!(picture.data.len(), plane * 3, "{name}");
                (
                    picture.data[plane + plane / 2],
                    picture.data[2 * plane + plane / 2],
                )
            } else {
                assert_eq!(picture.format, PictureFormat::Nv12, "{name}");
                assert_eq!(picture.data.len(), plane * 3 / 2, "{name}");
                (picture.data[plane], picture.data[plane + 1])
            };
            assert!((i32::from(y) - 126).abs() <= 3, "{name}: Y {y}");
            assert!(
                [u, v].iter().all(|&c| (i32::from(c) - 128).abs() <= 3),
                "{name}: U V {u} {v}"
            );
            assert_eq!(
                decoder.stream().contains("4:4:4"),
                format.chroma444,
                "{name}"
            );
        }
    }
}
