//! BGRA -> NV12 on the CPU, with bilinear scaling: the fallback for devices without a video processor.
//! Colours: full-range RGB in, BT.709 limited range (16-235 / 16-240) out, as the GPU path produces.
//! The picture is split into bands of rows, one thread each. For 10-bit encoding (Linux's NVENC): the same into
//! P010 (BT.709 limited range in 10 bits: 64-940 / 64-960), and the screen's pixels widened to R10G10B10A2.

/// Where one output column (or row) samples the input: between `first` and `second`, `weight` / 256
/// of the way to `second`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Tap {
    pub first: usize,
    pub second: usize,
    pub weight: i32,
}

/// Bilinear sampling positions for scaling `source` samples to `target` samples (pixel centres aligned).
pub(crate) fn taps(source: u32, target: u32) -> Vec<Tap> {
    let last = source.max(1) as usize - 1;
    (0..target)
        .map(|i| {
            let position = (f64::from(i) + 0.5) * f64::from(source) / f64::from(target) - 0.5;
            let position = position.max(0.0);
            let first = (position.floor() as usize).min(last);
            Tap {
                first,
                second: (first + 1).min(last),
                weight: (((position - first as f64) * 256.0).round() as i32).clamp(0, 256),
            }
        })
        .collect()
}

/// Scales and converts `bgra` (rows `pitch` bytes apart) into `out`: the Y plane (columns x rows), then
/// the interleaved U/V plane (half as many rows). The output width and height must be even.
pub(crate) fn bgra_to_nv12(
    bgra: &[u8],
    pitch: usize,
    columns: &[Tap],
    rows: &[Tap],
    out: &mut Vec<u8>,
) {
    let (width, height) = (columns.len(), rows.len());
    out.resize(width * height * 3 / 2, 0);
    let (luma, chroma) = out.split_at_mut(width * height);
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get().clamp(1, 8));
    let band = ((height / 2).div_ceil(threads) * 2).max(2);
    std::thread::scope(|scope| {
        let bands = luma
            .chunks_mut(band * width)
            .zip(chroma.chunks_mut(band / 2 * width));
        for (index, (luma_band, chroma_band)) in bands.enumerate() {
            let first_row = index * band;
            let band_rows = &rows[first_row..first_row + luma_band.len() / width];
            scope.spawn(move || {
                convert_band(bgra, pitch, columns, band_rows, luma_band, chroma_band);
            });
        }
    });
}

fn convert_band(
    bgra: &[u8],
    pitch: usize,
    columns: &[Tap],
    rows: &[Tap],
    luma: &mut [u8],
    chroma: &mut [u8],
) {
    let width = columns.len();
    for pair in 0..rows.len() / 2 {
        for x in (0..width).step_by(2) {
            let mut sum = [0; 3];
            for dy in 0..2 {
                for dx in 0..2 {
                    let rgb = sample(bgra, pitch, rows[2 * pair + dy], columns[x + dx]);
                    luma[(2 * pair + dy) * width + x + dx] = to_luma(rgb);
                    for (total, value) in sum.iter_mut().zip(rgb) {
                        *total += value;
                    }
                }
            }
            let (u, v) = to_chroma(sum.map(|total| (total + 2) / 4));
            chroma[pair * width + x] = u;
            chroma[pair * width + x + 1] = v;
        }
    }
}

// 10-bit encoding is on Linux (NVENC on CUDA); Windows converts on its GPU.
#[cfg(any(target_os = "linux", test))]
/// As `bgra_to_nv12`, into P010 (10-bit 4:2:0, as NVENC takes it): the Y plane, then the interleaved U/V plane, each
/// sample 16 bits little-endian with its 10 bits at the top. `out` holds the bytes (2 per sample).
pub(crate) fn bgra_to_p010(
    bgra: &[u8],
    pitch: usize,
    columns: &[Tap],
    rows: &[Tap],
    out: &mut Vec<u8>,
) {
    let (width, height) = (columns.len(), rows.len());
    out.resize(width * height * 3, 0);
    let (luma, chroma) = out.split_at_mut(width * height * 2);
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get().clamp(1, 8));
    let band = ((height / 2).div_ceil(threads) * 2).max(2);
    std::thread::scope(|scope| {
        let bands = luma
            .chunks_mut(band * width * 2)
            .zip(chroma.chunks_mut(band / 2 * width * 2));
        for (index, (luma_band, chroma_band)) in bands.enumerate() {
            let first_row = index * band;
            let band_rows = &rows[first_row..first_row + luma_band.len() / (width * 2)];
            scope.spawn(move || {
                convert_band_p010(bgra, pitch, columns, band_rows, luma_band, chroma_band);
            });
        }
    });
}

#[cfg(any(target_os = "linux", test))]
fn convert_band_p010(
    bgra: &[u8],
    pitch: usize,
    columns: &[Tap],
    rows: &[Tap],
    luma: &mut [u8],
    chroma: &mut [u8],
) {
    let width = columns.len();
    let put = |plane: &mut [u8], index: usize, value: i32| {
        plane[index * 2..index * 2 + 2].copy_from_slice(&((value as u16) << 6).to_le_bytes());
    };
    for pair in 0..rows.len() / 2 {
        for x in (0..width).step_by(2) {
            let mut sum = [0; 3];
            for dy in 0..2 {
                for dx in 0..2 {
                    let rgb = sample(bgra, pitch, rows[2 * pair + dy], columns[x + dx]);
                    put(luma, (2 * pair + dy) * width + x + dx, to_luma10(rgb));
                    for (total, value) in sum.iter_mut().zip(rgb) {
                        *total += value;
                    }
                }
            }
            let (u, v) = to_chroma10(sum.map(|total| (total + 2) / 4));
            put(chroma, pair * width + x, u);
            put(chroma, pair * width + x + 1, v);
        }
    }
}

#[cfg(any(target_os = "linux", test))]
/// The screen's B, G, R, X pixels (`width` x `height`, rows `pitch` bytes apart) as R10G10B10A2 words, red in the
/// lowest bits (NVENC's ABGR10): each 8-bit value widened to 10 bits (v << 2 | v >> 6: 0 stays 0, 255 becomes 1023).
pub(crate) fn bgra_to_rgb10(
    bgra: &[u8],
    pitch: usize,
    width: usize,
    height: usize,
    out: &mut Vec<u8>,
) {
    out.resize(width * height * 4, 0);
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get().clamp(1, 8));
    let band = height.div_ceil(threads).max(1);
    let widen = |value: u8| u32::from(value) << 2 | u32::from(value) >> 6;
    std::thread::scope(|scope| {
        for (index, rows) in out.chunks_mut(band * width * 4).enumerate() {
            scope.spawn(move || {
                for (row, line) in rows.chunks_mut(width * 4).enumerate() {
                    let source = &bgra[(index * band + row) * pitch..];
                    for (pixel, word) in source.chunks(4).zip(line.chunks_mut(4)) {
                        let value = widen(pixel[2])
                            | widen(pixel[1]) << 10
                            | widen(pixel[0]) << 20
                            | 3 << 30;
                        word.copy_from_slice(&value.to_le_bytes());
                    }
                }
            });
        }
    });
}

/// Bilinear sample as [R, G, B].
#[inline]
fn sample(bgra: &[u8], pitch: usize, row: Tap, column: Tap) -> [i32; 3] {
    let at = |y: usize, x: usize| {
        let i = y * pitch + x * 4;
        [bgra[i + 2], bgra[i + 1], bgra[i]].map(i32::from)
    };
    let top = lerp(
        at(row.first, column.first),
        at(row.first, column.second),
        column.weight,
    );
    let bottom = lerp(
        at(row.second, column.first),
        at(row.second, column.second),
        column.weight,
    );
    lerp(top, bottom, row.weight)
}

#[inline]
fn lerp(a: [i32; 3], b: [i32; 3], weight: i32) -> [i32; 3] {
    [0, 1, 2].map(|c| a[c] + (((b[c] - a[c]) * weight + 128) >> 8))
}

/// BT.709 luma, limited range: 16 (black) to 235 (white). Coefficients x 219/255 x 256.
#[inline]
fn to_luma([r, g, b]: [i32; 3]) -> u8 {
    (((47 * r + 157 * g + 16 * b + 128) >> 8) + 16) as u8
}

/// BT.709 chroma, limited range: 16 to 240 around 128. Coefficients x 224/255 x 256.
#[inline]
fn to_chroma([r, g, b]: [i32; 3]) -> (u8, u8) {
    let u = ((-26 * r - 86 * g + 112 * b + 128) >> 8) + 128;
    let v = ((112 * r - 102 * g - 10 * b + 128) >> 8) + 128;
    (u.clamp(16, 240) as u8, v.clamp(16, 240) as u8)
}

#[cfg(any(target_os = "linux", test))]
/// BT.709 luma, limited range in 10 bits: 64 (black) to 940 (white). Coefficients x 876/255 x 256.
#[inline]
fn to_luma10([r, g, b]: [i32; 3]) -> i32 {
    ((187 * r + 629 * g + 63 * b + 128) >> 8) + 64
}

#[cfg(any(target_os = "linux", test))]
/// BT.709 chroma, limited range in 10 bits: 64 to 960 around 512. Coefficients x 896/255 x 256.
#[inline]
fn to_chroma10([r, g, b]: [i32; 3]) -> (i32, i32) {
    let u = ((-103 * r - 347 * g + 450 * b + 128) >> 8) + 512;
    let v = ((450 * r - 409 * g - 41 * b + 128) >> 8) + 512;
    (u.clamp(64, 960), v.clamp(64, 960))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(width: usize, height: usize, bgra: [u8; 4]) -> Vec<u8> {
        bgra.repeat(width * height)
    }

    fn convert(image: &[u8], input: (u32, u32), output: (u32, u32)) -> Vec<u8> {
        let mut out = Vec::new();
        let (columns, rows) = (taps(input.0, output.0), taps(input.1, output.1));
        bgra_to_nv12(image, input.0 as usize * 4, &columns, &rows, &mut out);
        out
    }

    #[test]
    fn same_size_taps_are_exact() {
        let identity = taps(4, 4);
        assert!(
            identity
                .iter()
                .enumerate()
                .all(|(i, t)| t.first == i && t.weight == 0)
        );
    }

    #[test]
    fn white_black_and_red_match_bt709_limited_range() {
        let white = convert(&solid(4, 2, [255, 255, 255, 255]), (4, 2), (4, 2));
        assert_eq!(
            white,
            [235, 235, 235, 235, 235, 235, 235, 235, 128, 128, 128, 128]
        );
        let black = convert(&solid(2, 2, [0, 0, 0, 255]), (2, 2), (2, 2));
        assert_eq!(black, [16, 16, 16, 16, 128, 128]);
        // Red: Y = 16 + 219 * 0.2126 = 62.6, U = 128 - 224 * 0.1146 = 102.3, V = 240.
        let red = convert(&solid(2, 2, [0, 0, 255, 255]), (2, 2), (2, 2));
        assert_eq!(red, [63, 63, 63, 63, 102, 240]);
    }

    /// P010 samples back as 10-bit values.
    fn p010(image: &[u8], input: (u32, u32), output: (u32, u32)) -> Vec<u16> {
        let mut out = Vec::new();
        let (columns, rows) = (taps(input.0, output.0), taps(input.1, output.1));
        bgra_to_p010(image, input.0 as usize * 4, &columns, &rows, &mut out);
        out.chunks(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]) >> 6)
            .collect()
    }

    #[test]
    fn ten_bit_white_black_and_red_match_bt709_limited_range() {
        let white = p010(&solid(2, 2, [255, 255, 255, 255]), (2, 2), (2, 2));
        assert_eq!(white, [940, 940, 940, 940, 512, 512]);
        let black = p010(&solid(2, 2, [0, 0, 0, 255]), (2, 2), (2, 2));
        assert_eq!(black, [64, 64, 64, 64, 512, 512]);
        // Red: Y = 64 + 876 * 0.2126 = 250.2, U = 512 - 896 * 0.1146 = 409.3, V = 960.
        let red = p010(&solid(2, 2, [0, 0, 255, 255]), (2, 2), (2, 2));
        assert_eq!(red, [250, 250, 250, 250, 409, 960]);
        // The low 6 bits of every sample are 0 (P010 keeps the 10 bits at the top).
        let mut raw = Vec::new();
        bgra_to_p010(
            &solid(2, 2, [12, 34, 56, 255]),
            8,
            &taps(2, 2),
            &taps(2, 2),
            &mut raw,
        );
        assert!(raw.chunks(2).all(|pair| pair[0] & 0x3F == 0));
    }

    #[test]
    fn ten_bit_scaling_keeps_flat_colour_in_every_band() {
        let out = p010(&solid(64, 48, [128, 128, 128, 255]), (64, 48), (34, 22));
        assert_eq!(out.len(), 34 * 22 * 3 / 2);
        let (luma, chroma) = out.split_at(34 * 22);
        assert!(luma.iter().all(|&y| y == luma[0]));
        assert!(chroma.iter().all(|&c| c == 512));
    }

    #[test]
    fn rgb10_widens_each_colour_with_red_lowest() {
        // B, G, R, X = 0, 128, 255: red 1023, green 514, blue 0, alpha 3.
        let mut out = Vec::new();
        // Rows 12 bytes apart (padding after 2 pixels) to check the pitch is followed.
        let mut image = vec![0u8; 24];
        for row in 0..2 {
            for column in 0..2 {
                image[row * 12 + column * 4..row * 12 + column * 4 + 4]
                    .copy_from_slice(&[0, 128, 255, 0]);
            }
        }
        bgra_to_rgb10(&image, 12, 2, 2, &mut out);
        assert_eq!(out.len(), 16);
        let expected = 1023 | 514 << 10 | 3 << 30;
        assert!(
            out.chunks(4)
                .all(|word| u32::from_le_bytes(word.try_into().unwrap()) == expected)
        );
    }

    #[test]
    fn scaling_keeps_flat_colour_and_odd_bands_fill_everything() {
        let grey = solid(64, 48, [128, 128, 128, 255]);
        let out = convert(&grey, (64, 48), (34, 22));
        assert_eq!(out.len(), 34 * 22 * 3 / 2);
        let (luma, chroma) = out.split_at(34 * 22);
        assert!(luma.iter().all(|&y| y == luma[0]));
        assert!(chroma.iter().all(|&c| c == 128));
    }
}
