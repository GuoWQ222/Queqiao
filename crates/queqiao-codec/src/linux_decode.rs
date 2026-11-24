//! The viewer's decoder on Linux (docs/LINUX.md), with the API of Windows' `H264Decoder`: OpenH264 on the CPU (as
//! Windows uses when no GPU decoder works), its I420 pictures packed to NV12 in memory, which the presenter draws.
//! NVIDIA's NVDEC on CUDA and VA-API come next (H.265, AV1, decoding on the GPU).

use openh264::OpenH264API;
use openh264::decoder::{Decoder, DecoderConfig};
use openh264::formats::YUVSource;
use queqiao_capture::{Context, Device};

use crate::VideoCodec;
use crate::config::DecoderChoice;
use crate::linux_screen::{Error, Result};

/// How many decoded pictures may be held at once (as Windows' ring of textures; here each frame owns its picture).
pub const RING: usize = 4;

/// A decoded picture in memory: NV12, rows `pitch` bytes apart, `rows` luma rows then half as many U V rows (Windows:
/// an NV12 texture).
pub struct Picture {
    pub data: Vec<u8>,
    pub pitch: usize,
    pub rows: usize,
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

pub struct H264Decoder {
    decoder: Decoder,
    name: String,
    skipped: Vec<String>,
}

impl H264Decoder {
    pub fn new(device: &Device, context: &Context, choice: DecoderChoice) -> Result<Self> {
        Self::with_codec(VideoCodec::H264, device, context, choice)
    }

    /// A decoder for `codec`: H.264 with OpenH264; H.265 and AV1 need a GPU decoder, not on Linux yet.
    pub fn with_codec(
        codec: VideoCodec,
        _device: &Device,
        _context: &Context,
        choice: DecoderChoice,
    ) -> Result<Self> {
        if codec != VideoCodec::H264 {
            return Err(Error::new(format!(
                "{codec:?}: no decoder on Linux yet (NVDEC and VA-API come next)"
            )));
        }
        let mut skipped = Vec::new();
        match choice {
            DecoderChoice::Auto | DecoderChoice::Software => {}
            other => {
                return Err(Error::new(format!(
                    "{other:?}: no such decoder on Linux yet"
                )));
            }
        }
        if choice == DecoderChoice::Auto {
            skipped.push("GPU decoders: none on Linux yet".into());
        }
        let decoder = Decoder::with_api_config(OpenH264API::from_source(), DecoderConfig::new())
            .map_err(|e| Error::new(format!("OpenH264 decoder: {e}")))?;
        Ok(Self {
            decoder,
            name: "OpenH264 decoder (software)".into(),
            skipped,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// What the stream is, as the decoder sees it.
    pub fn stream(&self) -> String {
        "H.264 4:2:0".into()
    }

    pub fn is_hardware(&self) -> bool {
        false
    }

    /// Decoders tried before this one, each with the reason it was not used.
    pub fn skipped(&self) -> &[String] {
        &self.skipped
    }

    /// Decodes one frame's data; the picture (if one came out) goes into `out`.
    pub fn decode(&mut self, data: &[u8], time: i64, out: &mut Vec<DecodedFrame>) -> Result<()> {
        let decoded = self
            .decoder
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
            },
            width: width as u32,
            height: height as u32,
            time,
        });
        Ok(())
    }
}

/// Whether this PC can decode `codec` (H.264 always: OpenH264 is built in).
pub fn can_decode(codec: VideoCodec) -> bool {
    codec == VideoCodec::H264
}

/// Whether the GPU decodes AV1 itself: not on Linux yet.
pub fn av1_decodes_on_gpu(_device: &Device) -> bool {
    false
}

/// What H.265 this PC decodes (`protocol::hevc` bits): none on Linux yet.
pub fn hevc_decode_caps(_device: &Device, _context: &Context) -> u8 {
    0
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
        assert!(
            H264Decoder::with_codec(VideoCodec::H265, &Device, &Context, DecoderChoice::Auto)
                .is_err()
        );
    }
}
