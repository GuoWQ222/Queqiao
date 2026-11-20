//! BGRA -> NV12 on the CPU, with bilinear scaling: the fallback for devices without a video processor.
//! Colours: full-range RGB in, BT.709 limited range (16-235 / 16-240) out, as the GPU path produces.
//! The picture is split into bands of rows, one thread each.

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
