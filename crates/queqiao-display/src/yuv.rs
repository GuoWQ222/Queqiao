//! NV12 -> BGRA on the CPU with bilinear scaling, for devices without a video processor. Colours:
//! BT.709 limited range in (as the encoders produce), full-range RGB out.

/// Where one output column (or row) samples the input: between `first` and `second`, `weight` / 256
/// of the way to `second`.
#[derive(Clone, Copy)]
struct Tap {
    first: usize,
    second: usize,
    weight: i32,
}

fn taps(source: usize, target: usize) -> Vec<Tap> {
    let last = source.max(1) - 1;
    (0..target)
        .map(|i| {
            let position = ((i as f64 + 0.5) * source as f64 / target as f64 - 0.5).max(0.0);
            let first = (position.floor() as usize).min(last);
            Tap {
                first,
                second: (first + 1).min(last),
                weight: (((position - first as f64) * 256.0).round() as i32).clamp(0, 256),
            }
        })
        .collect()
}

/// Draws an NV12 picture (`width` x `height`, rows `pitch` bytes apart, UV rows after `height` Y rows)
/// into `out`, a BGRA image of `out_width` x `out_height`, scaled to fit with the aspect ratio kept and
/// black bars around it.
pub fn nv12_to_bgra(
    nv12: &[u8],
    pitch: usize,
    (width, height): (usize, usize),
    out: &mut [u8],
    (out_width, out_height): (usize, usize),
) {
    let scale = (out_width as f64 / width as f64).min(out_height as f64 / height as f64);
    let fit_w = ((width as f64 * scale).round() as usize).clamp(1, out_width);
    let fit_h = ((height as f64 * scale).round() as usize).clamp(1, out_height);
    let (left, top) = ((out_width - fit_w) / 2, (out_height - fit_h) / 2);
    let (luma, chroma) = nv12.split_at(pitch * height);
    let (columns, rows) = (taps(width, fit_w), taps(height, fit_h));
    let (chroma_columns, chroma_rows) = (taps(width / 2, fit_w), taps(height / 2, fit_h));
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get().clamp(1, 8));
    let band = out_height.div_ceil(threads).max(1);
    std::thread::scope(|scope| {
        for (index, rows_out) in out.chunks_mut(band * out_width * 4).enumerate() {
            let (columns, rows) = (&columns, &rows);
            let (chroma_columns, chroma_rows) = (&chroma_columns, &chroma_rows);
            scope.spawn(move || {
                for (offset, line) in rows_out.chunks_mut(out_width * 4).enumerate() {
                    let y = index * band + offset;
                    line.fill(0);
                    for pixel in line.as_chunks_mut::<4>().0 {
                        pixel[3] = 255;
                    }
                    if y < top || y >= top + fit_h {
                        continue;
                    }
                    let (row, chroma_row) = (rows[y - top], chroma_rows[y - top]);
                    for x in 0..fit_w {
                        let luma_value = sample(luma, pitch, 1, 0, row, columns[x]);
                        let column = chroma_columns[x];
                        let u = sample(chroma, pitch, 2, 0, chroma_row, column);
                        let v = sample(chroma, pitch, 2, 1, chroma_row, column);
                        let [r, g, b] = to_rgb(luma_value, u, v);
                        let i = (left + x) * 4;
                        line[i..i + 3].copy_from_slice(&[b, g, r]);
                    }
                }
            });
        }
    });
}

/// Bilinear sample of one plane: `step` bytes between samples of a row, starting at byte `start`.
#[inline]
fn sample(plane: &[u8], pitch: usize, step: usize, start: usize, row: Tap, column: Tap) -> i32 {
    let at = |y: usize, x: usize| i32::from(plane[y * pitch + x * step + start]);
    let lerp = |a: i32, b: i32, w: i32| a + (((b - a) * w + 128) >> 8);
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

/// BT.709 limited range to full-range RGB (coefficients x 256).
#[inline]
fn to_rgb(y: i32, u: i32, v: i32) -> [u8; 3] {
    let (c, d, e) = (298 * (y - 16), u - 128, v - 128);
    let clamp = |value: i32| ((value + 128) >> 8).clamp(0, 255) as u8;
    [
        clamp(c + 459 * e),
        clamp(c - 55 * d - 136 * e),
        clamp(c + 541 * d),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A flat NV12 picture.
    fn flat(width: usize, height: usize, (y, u, v): (u8, u8, u8)) -> Vec<u8> {
        let mut nv12 = vec![y; width * height];
        nv12.extend([u, v].repeat(width * height / 4));
        nv12
    }

    #[test]
    fn white_black_red_round_trip() {
        assert_eq!(to_rgb(235, 128, 128), [255, 255, 255]);
        assert_eq!(to_rgb(16, 128, 128), [0, 0, 0]);
        // The CPU encoder path turns pure red into Y 63, U 102, V 240.
        assert_eq!(to_rgb(63, 102, 240), [255, 1, 0]);
    }

    #[test]
    fn fits_with_black_bars() {
        // 4x2 white picture into an 8x8 window: 8x4 picture, 2 black rows above and below.
        let nv12 = flat(4, 2, (235, 128, 128));
        let mut out = vec![7u8; 8 * 8 * 4];
        nv12_to_bgra(&nv12, 4, (4, 2), &mut out, (8, 8));
        let row = |y: usize| &out[y * 32..(y + 1) * 32];
        assert!(
            row(0)
                .as_chunks::<4>()
                .0
                .iter()
                .all(|p| *p == [0, 0, 0, 255])
        );
        assert!(
            row(2)
                .as_chunks::<4>()
                .0
                .iter()
                .all(|p| *p == [255, 255, 255, 255])
        );
        assert!(
            row(5)
                .as_chunks::<4>()
                .0
                .iter()
                .all(|p| *p == [255, 255, 255, 255])
        );
        assert!(
            row(7)
                .as_chunks::<4>()
                .0
                .iter()
                .all(|p| *p == [0, 0, 0, 255])
        );
    }
}
