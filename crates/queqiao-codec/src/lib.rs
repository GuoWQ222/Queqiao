//! Video codec for Queqiao: BGRA -> NV12 conversion on the GPU ([`Nv12Converter`]) and the hardware
//! H.264 encoder ([`H264Encoder`]). Frames stay on the GPU from capture to the encoder's output.

mod convert;
mod cpu_convert;
mod decoder;
mod encoder;
mod mf;
mod mf_decode;
mod software;
mod software_decode;

pub use convert::Nv12Converter;
pub use decoder::{DecodedFrame, DecoderChoice, H264Decoder, RING};
pub use encoder::{EncodedFrame, EncoderChoice, EncoderConfig, H264Encoder, SoftwareTiming};

/// Encode sizes to try for a screen, largest first: the screen itself (rounded down to even numbers, as
/// NV12 needs), then the same picture scaled down to fit common encoder limits (4096, 4K, 1440p, 1080p,
/// 720p boxes, turned for portrait screens). Never larger than the screen; the aspect ratio is kept.
pub fn candidate_sizes(width: u32, height: u32) -> Vec<(u32, u32)> {
    let boxes = [
        (u32::MAX, u32::MAX),
        (4096, 4096),
        (3840, 2160),
        (2560, 1440),
        (1920, 1080),
        (1280, 720),
    ];
    let mut sizes = Vec::new();
    for (long, short) in boxes {
        let (max_w, max_h) = if height > width {
            (short, long)
        } else {
            (long, short)
        };
        let scale_w = f64::from(max_w) / f64::from(width.max(1));
        let scale_h = f64::from(max_h) / f64::from(height.max(1));
        let scale = scale_w.min(scale_h).min(1.0);
        let w = ((f64::from(width) * scale) as u32) & !1;
        let h = ((f64::from(height) * scale) as u32) & !1;
        if w >= 2 && h >= 2 && !sizes.contains(&(w, h)) {
            sizes.push((w, h));
        }
    }
    sizes
}

/// NAL unit types in an Annex-B H.264 stream, in order (7 = SPS, 8 = PPS, 5 = IDR slice, 1 = other slice,
/// 6 = SEI, 9 = access unit delimiter).
pub fn nal_types(data: &[u8]) -> Vec<u8> {
    data.windows(4)
        .filter(|w| w[..3] == [0, 0, 1])
        .map(|w| w[3] & 0x1F)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_nal_types_after_3_and_4_byte_start_codes() {
        let stream = [
            0, 0, 0, 1, 0x67, 0x42, 0, 0, 0, 1, 0x68, 0xCE, 0, 0, 1, 0x65, 0x88,
        ];
        assert_eq!(nal_types(&stream), vec![7, 8, 5]);
    }

    #[test]
    fn candidate_sizes_cover_common_screens() {
        assert_eq!(
            candidate_sizes(2560, 1440),
            vec![(2560, 1440), (1920, 1080), (1280, 720)]
        );
        // Super-wide 5120x1440: the full width is beyond many H.264 encoders, so smaller sizes follow.
        assert_eq!(
            candidate_sizes(5120, 1440)[..2],
            [(5120, 1440), (4096, 1152)]
        );
        // Odd sizes (some virtual machines) are rounded down to even.
        assert_eq!(candidate_sizes(1365, 767)[0], (1364, 766));
        // Portrait screens keep their orientation.
        assert_eq!(candidate_sizes(1440, 2560)[1], (1080, 1920));
        // Small screens are never scaled up.
        assert_eq!(candidate_sizes(800, 600), vec![(800, 600)]);
    }
}
