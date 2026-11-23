//! A stream's shape from its sequence parameter set (H.264 or H.265, Annex B): the picture's size after cropping, its
//! chroma format and bit depth. AMD's decoder must be told them before it starts; the stream carries them only there.

use crate::format::Codec;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Shape {
    pub width: u32,
    pub height: u32,
    pub chroma444: bool,
    pub bit_depth: u32,
}

/// The shape from the first sequence parameter set in `data`, if there is one Queqiao's parser reads (H.264 with
/// scaling matrices is left out: Queqiao's encoders send none).
pub(crate) fn shape(codec: Codec, data: &[u8]) -> Option<Shape> {
    nal_units(data).into_iter().find_map(|nal| match codec {
        Codec::H264 if nal.first().is_some_and(|b| b & 0x1F == 7) => {
            h264(&mut Bits::new(&nal[1..]))
        }
        Codec::H265 if nal.first().is_some_and(|b| (b >> 1) & 0x3F == 33) && nal.len() > 2 => {
            h265(&mut Bits::new(&nal[2..]))
        }
        _ => None,
    })
}

/// The NAL units of an Annex-B stream (start codes 00 00 01 or 00 00 00 01).
fn nal_units(data: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            starts.push(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }
    starts
        .iter()
        .enumerate()
        .map(|(n, &start)| {
            let mut end = starts.get(n + 1).map_or(data.len(), |next| next - 3);
            // A four-byte start code leaves a zero before it.
            while end > start && data[end - 1] == 0 {
                end -= 1;
            }
            &data[start..end]
        })
        .collect()
}

/// Bits of a NAL unit's payload, the emulation-prevention bytes (00 00 03) taken out.
struct Bits {
    data: Vec<u8>,
    position: usize,
}

impl Bits {
    fn new(payload: &[u8]) -> Bits {
        let mut data = Vec::with_capacity(payload.len());
        let mut zeros = 0;
        for &byte in payload {
            if zeros >= 2 && byte == 3 {
                zeros = 0;
                continue;
            }
            zeros = if byte == 0 { zeros + 1 } else { 0 };
            data.push(byte);
        }
        Bits { data, position: 0 }
    }

    fn bit(&mut self) -> Option<u32> {
        let byte = *self.data.get(self.position / 8)?;
        let bit = (byte >> (7 - self.position % 8)) & 1;
        self.position += 1;
        Some(u32::from(bit))
    }

    fn bits(&mut self, count: u32) -> Option<u32> {
        let mut value = 0;
        for _ in 0..count {
            value = (value << 1) | self.bit()?;
        }
        Some(value)
    }

    fn skip(&mut self, count: usize) -> Option<()> {
        self.position += count;
        (self.position <= self.data.len() * 8).then_some(())
    }

    /// Unsigned Exp-Golomb.
    fn ue(&mut self) -> Option<u32> {
        let mut zeros = 0;
        while self.bit()? == 0 {
            zeros += 1;
            if zeros > 31 {
                return None;
            }
        }
        Some((1u32 << zeros) - 1 + self.bits(zeros)?)
    }

    /// Signed Exp-Golomb.
    fn se(&mut self) -> Option<i32> {
        let value = self.ue()?;
        Some(if value % 2 == 1 {
            value.div_ceil(2) as i32
        } else {
            -((value / 2) as i32)
        })
    }
}

fn h264(r: &mut Bits) -> Option<Shape> {
    let profile = r.bits(8)?;
    r.skip(16)?;
    r.ue()?;
    let (mut chroma, mut depth) = (1, 8);
    if matches!(
        profile,
        100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128 | 138 | 139 | 134 | 135
    ) {
        chroma = r.ue()?;
        if chroma == 3 {
            r.skip(1)?;
        }
        depth = 8 + r.ue()?;
        r.ue()?;
        r.skip(1)?;
        if r.bit()? == 1 {
            return None;
        }
    }
    r.ue()?;
    match r.ue()? {
        0 => {
            r.ue()?;
        }
        1 => {
            r.skip(1)?;
            r.se()?;
            r.se()?;
            for _ in 0..r.ue()? {
                r.se()?;
            }
        }
        _ => {}
    }
    r.ue()?;
    r.skip(1)?;
    let width_mbs = r.ue()? + 1;
    let height_units = r.ue()? + 1;
    let frame_mbs_only = r.bit()?;
    if frame_mbs_only == 0 {
        r.skip(1)?;
    }
    r.skip(1)?;
    let crop = if r.bit()? == 1 {
        (r.ue()?, r.ue()?, r.ue()?, r.ue()?)
    } else {
        (0, 0, 0, 0)
    };
    let rows_per_unit = 2 - frame_mbs_only;
    let (crop_x, crop_y) = match chroma {
        1 => (2, 2 * rows_per_unit),
        2 => (2, rows_per_unit),
        _ => (1, rows_per_unit),
    };
    Some(Shape {
        width: (width_mbs * 16).checked_sub(crop_x * (crop.0 + crop.1))?,
        height: (height_units * 16 * rows_per_unit).checked_sub(crop_y * (crop.2 + crop.3))?,
        chroma444: chroma == 3,
        bit_depth: depth,
    })
}

fn h265(r: &mut Bits) -> Option<Shape> {
    r.skip(4)?;
    let sub_layers = r.bits(3)?;
    r.skip(1)?;
    // profile_tier_level: the general part (96 bits), then each sub-layer's flags, alignment and parts.
    r.skip(96)?;
    let mut present = Vec::new();
    for _ in 0..sub_layers {
        present.push((r.bit()?, r.bit()?));
    }
    if sub_layers > 0 {
        r.skip(2 * (8 - sub_layers as usize))?;
    }
    for (profile, level) in present {
        if profile == 1 {
            r.skip(88)?;
        }
        if level == 1 {
            r.skip(8)?;
        }
    }
    r.ue()?;
    let chroma = r.ue()?;
    if chroma == 3 {
        r.skip(1)?;
    }
    let width = r.ue()?;
    let height = r.ue()?;
    let crop = if r.bit()? == 1 {
        (r.ue()?, r.ue()?, r.ue()?, r.ue()?)
    } else {
        (0, 0, 0, 0)
    };
    let depth = 8 + r.ue()?;
    let (sub_x, sub_y) = match chroma {
        1 => (2, 2),
        2 => (2, 1),
        _ => (1, 1),
    };
    Some(Shape {
        width: width.checked_sub(sub_x * (crop.0 + crop.1))?,
        height: height.checked_sub(sub_y * (crop.2 + crop.3))?,
        chroma444: chroma == 3,
        bit_depth: depth,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Writes bits and Exp-Golomb numbers, to make parameter sets for the tests.
    struct Writer {
        bits: Vec<u8>,
    }

    impl Writer {
        fn put(&mut self, value: u32, count: u32) {
            for i in (0..count).rev() {
                self.bits.push(((value >> i) & 1) as u8);
            }
        }

        fn ue(&mut self, value: u32) {
            let coded = value + 1;
            let length = 32 - coded.leading_zeros();
            self.put(0, length - 1);
            self.put(coded, length);
        }

        fn bytes(&self) -> Vec<u8> {
            let mut bytes = vec![0u8; self.bits.len().div_ceil(8)];
            for (i, bit) in self.bits.iter().enumerate() {
                bytes[i / 8] |= bit << (7 - i % 8);
            }
            bytes
        }
    }

    #[test]
    fn an_h264_sps_gives_the_cropped_size() {
        // High profile, 1920x1080 (68 rows of macroblocks, 8 rows cropped).
        let mut w = Writer { bits: Vec::new() };
        w.put(100, 8);
        w.put(0, 8);
        w.put(40, 8);
        w.ue(0);
        w.ue(1); // 4:2:0
        w.ue(0);
        w.ue(0);
        w.put(0, 1);
        w.put(0, 1); // no scaling matrices
        w.ue(0);
        w.ue(2); // pic_order_cnt_type 2
        w.ue(1);
        w.put(0, 1);
        w.ue(119);
        w.ue(67);
        w.put(1, 1); // frame_mbs_only
        w.put(1, 1);
        w.put(1, 1); // cropping
        w.ue(0);
        w.ue(0);
        w.ue(0);
        w.ue(4);
        w.put(1, 1);
        let mut stream = vec![0, 0, 0, 1, 0x67];
        stream.extend(w.bytes());
        assert_eq!(
            shape(Codec::H264, &stream),
            Some(Shape {
                width: 1920,
                height: 1080,
                chroma444: false,
                bit_depth: 8
            })
        );
    }

    #[test]
    fn an_h265_sps_gives_size_chroma_and_depth() {
        let mut w = Writer { bits: Vec::new() };
        w.put(0, 4);
        w.put(0, 3); // no sub-layers
        w.put(1, 1);
        w.put(0, 32);
        w.put(0, 32);
        w.put(0, 32); // profile_tier_level, general part
        w.ue(0);
        w.ue(3); // 4:4:4
        w.put(0, 1);
        w.ue(2240);
        w.ue(1408);
        w.put(1, 1); // conformance window: 8 rows off the bottom
        w.ue(0);
        w.ue(0);
        w.ue(0);
        w.ue(8);
        w.ue(2); // 10 bits
        w.put(1, 1);
        let mut stream = vec![0, 0, 1, 0x42, 0x01];
        stream.extend(w.bytes());
        assert_eq!(
            shape(Codec::H265, &stream),
            Some(Shape {
                width: 2240,
                height: 1400,
                chroma444: true,
                bit_depth: 10
            })
        );
    }

    #[test]
    fn emulation_prevention_bytes_are_taken_out() {
        let bits = Bits::new(&[0, 0, 3, 1, 0, 0, 3, 0]);
        assert_eq!(bits.data, vec![0, 0, 1, 0, 0, 0]);
    }
}
