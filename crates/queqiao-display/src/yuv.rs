//! Decoded pictures -> BGRA on the CPU with bilinear scaling, for devices without a video processor (or one that
//! refuses the decoder's pictures). The pictures as the decoders give them: NV12 and P010 (4:2:0, 8 and 10 bits),
//! AYUV and Y410 (4:4:4, 8 and 10 bits), BT.709 limited range in (as the encoders produce), full-range RGB out; and
//! BGRA (NVIDIA's decoder converts itself), only scaled.

/// How a decoded picture lies in memory: rows `pitch` bytes apart; 4:2:0's colour rows (U V interleaved, half size)
/// after its luma rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout {
    /// 8-bit luma, then 8-bit U V pairs.
    Nv12,
    /// 16-bit luma (10 bits in the high bits), then 16-bit U V pairs.
    P010,
    /// 4 bytes a pixel: V, U, Y, alpha.
    Ayuv,
    /// 32 bits a pixel: U in bits 0-9, Y in 10-19, V in 20-29.
    Y410,
    /// 4 bytes a pixel: blue, green, red, alpha.
    Bgra,
}

impl Layout {
    /// The bytes a picture of this layout spans, its planes `rows` rows high (the texture's height, which can be
    /// more than the picture's).
    pub fn bytes(self, pitch: usize, rows: usize) -> usize {
        match self {
            Layout::Nv12 | Layout::P010 => pitch * (rows + rows / 2),
            Layout::Ayuv | Layout::Y410 | Layout::Bgra => pitch * rows,
        }
    }
}

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
    size: (usize, usize),
    out: &mut [u8],
    out_size: (usize, usize),
) {
    picture_to_bgra(nv12, pitch, size.1, Layout::Nv12, size, out, out_size);
}

/// Draws a picture of `layout` (`width` x `height` shown, its planes `rows` rows high, rows `pitch` bytes apart)
/// into `out`, a BGRA image of `out_width` x `out_height`, scaled to fit with the aspect ratio kept and black bars
/// around it. `picture` must span [`Layout::bytes`] (`pitch`, `rows`).
pub fn picture_to_bgra(
    picture: &[u8],
    pitch: usize,
    rows: usize,
    layout: Layout,
    (width, height): (usize, usize),
    out: &mut [u8],
    (out_width, out_height): (usize, usize),
) {
    let scale = (out_width as f64 / width as f64).min(out_height as f64 / height as f64);
    let fit_w = ((width as f64 * scale).round() as usize).clamp(1, out_width);
    let fit_h = ((height as f64 * scale).round() as usize).clamp(1, out_height);
    let (left, top) = ((out_width - fit_w) / 2, (out_height - fit_h) / 2);
    let (first_plane, chroma) = picture.split_at((pitch * rows).min(picture.len()));
    let (columns, row_taps) = (taps(width, fit_w), taps(height, fit_h));
    let half = matches!(layout, Layout::Nv12 | Layout::P010);
    let (chroma_columns, chroma_rows) = if half {
        (taps(width / 2, fit_w), taps(height / 2, fit_h))
    } else {
        (columns.clone(), row_taps.clone())
    };
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get().clamp(1, 8));
    let band = out_height.div_ceil(threads).max(1);
    std::thread::scope(|scope| {
        for (index, rows_out) in out.chunks_mut(band * out_width * 4).enumerate() {
            let (columns, row_taps) = (&columns, &row_taps);
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
                    let (row, chroma_row) = (row_taps[y - top], chroma_rows[y - top]);
                    for x in 0..fit_w {
                        let (column, chroma_column) = (columns[x], chroma_columns[x]);
                        let (luma, colour) = (first_plane, chroma);
                        let [r, g, b] = match layout {
                            Layout::Nv12 => to_rgb(
                                bilinear(|r, c| i32::from(luma[r * pitch + c]) * 4, row, column),
                                bilinear(
                                    |r, c| i32::from(colour[r * pitch + c * 2]) * 4,
                                    chroma_row,
                                    chroma_column,
                                ),
                                bilinear(
                                    |r, c| i32::from(colour[r * pitch + c * 2 + 1]) * 4,
                                    chroma_row,
                                    chroma_column,
                                ),
                            ),
                            Layout::P010 => to_rgb(
                                bilinear(|r, c| ten(luma, r * pitch + c * 2), row, column),
                                bilinear(
                                    |r, c| ten(colour, r * pitch + c * 4),
                                    chroma_row,
                                    chroma_column,
                                ),
                                bilinear(
                                    |r, c| ten(colour, r * pitch + c * 4 + 2),
                                    chroma_row,
                                    chroma_column,
                                ),
                            ),
                            Layout::Ayuv => to_rgb(
                                bilinear(
                                    |r, c| i32::from(luma[r * pitch + c * 4 + 2]) * 4,
                                    row,
                                    column,
                                ),
                                bilinear(
                                    |r, c| i32::from(luma[r * pitch + c * 4 + 1]) * 4,
                                    row,
                                    column,
                                ),
                                bilinear(
                                    |r, c| i32::from(luma[r * pitch + c * 4]) * 4,
                                    row,
                                    column,
                                ),
                            ),
                            Layout::Y410 => to_rgb(
                                bilinear(|r, c| packed(luma, r * pitch + c * 4, 10), row, column),
                                bilinear(|r, c| packed(luma, r * pitch + c * 4, 0), row, column),
                                bilinear(|r, c| packed(luma, r * pitch + c * 4, 20), row, column),
                            ),
                            Layout::Bgra => {
                                let channel = |k: usize| {
                                    bilinear(
                                        |r, c| i32::from(luma[r * pitch + c * 4 + k]),
                                        row,
                                        column,
                                    )
                                    .clamp(0, 255) as u8
                                };
                                [channel(2), channel(1), channel(0)]
                            }
                        };
                        let i = (left + x) * 4;
                        line[i..i + 3].copy_from_slice(&[b, g, r]);
                    }
                }
            });
        }
    });
}

/// The 10-bit sample in the high bits of the 16-bit little-endian word at byte `i` (P010).
#[inline]
fn ten(plane: &[u8], i: usize) -> i32 {
    i32::from(u16::from_le_bytes([plane[i], plane[i + 1]]) >> 6)
}

/// The 10 bits at `shift` of the 32-bit little-endian word at byte `i` (Y410).
#[inline]
fn packed(plane: &[u8], i: usize, shift: u32) -> i32 {
    let word = u32::from_le_bytes([plane[i], plane[i + 1], plane[i + 2], plane[i + 3]]);
    ((word >> shift) & 0x3FF) as i32
}

/// Bilinear sample at `row`, `column` of the samples `at` (row, column) gives.
#[inline]
fn bilinear(at: impl Fn(usize, usize) -> i32, row: Tap, column: Tap) -> i32 {
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

/// BT.709 limited range, 10-bit code values (8-bit ones times 4), to full-range RGB (coefficients x 256).
#[inline]
fn to_rgb(y: i32, u: i32, v: i32) -> [u8; 3] {
    let (c, d, e) = (298 * (y - 64), u - 512, v - 512);
    let clamp = |value: i32| ((value + 512) >> 10).clamp(0, 255) as u8;
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

    /// The one colour of a flat 4x2 picture of `layout` drawn 4x2 (no scaling).
    fn shown(layout: Layout, picture: &[u8], pitch: usize) -> [u8; 4] {
        let mut out = vec![7u8; 4 * 2 * 4];
        picture_to_bgra(picture, pitch, 2, layout, (4, 2), &mut out, (4, 2));
        let first: [u8; 4] = out[..4].try_into().unwrap();
        assert!(
            out.as_chunks::<4>().0.iter().all(|p| *p == first),
            "{layout:?} not flat: {out:?}"
        );
        first
    }

    #[test]
    fn white_black_red_round_trip() {
        assert_eq!(to_rgb(940, 512, 512), [255, 255, 255]);
        assert_eq!(to_rgb(64, 512, 512), [0, 0, 0]);
        // The CPU encoder path turns pure red into Y 63, U 102, V 240.
        assert_eq!(to_rgb(63 * 4, 102 * 4, 240 * 4), [255, 1, 0]);
    }

    #[test]
    fn every_layout_shows_the_same_red() {
        // Red in BT.709 limited range: Y 63, U 102, V 240 (8 bits); 252, 408, 960 (10 bits).
        let red = [0, 1, 255, 255];
        assert_eq!(shown(Layout::Nv12, &flat(4, 2, (63, 102, 240)), 4), red);
        let mut p010: Vec<u8> = [252u16 << 6; 8]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        p010.extend(
            [408u16 << 6, 960 << 6]
                .repeat(2)
                .iter()
                .flat_map(|v| v.to_le_bytes()),
        );
        assert_eq!(shown(Layout::P010, &p010, 8), red);
        let ayuv = [240u8, 102, 63, 255].repeat(8);
        assert_eq!(shown(Layout::Ayuv, &ayuv, 16), red);
        let word = 408u32 | (252 << 10) | (960 << 20) | (3 << 30);
        let y410: Vec<u8> = [word; 8].iter().flat_map(|w| w.to_le_bytes()).collect();
        assert_eq!(shown(Layout::Y410, &y410, 16), red);
        let bgra = [0u8, 1, 255, 255].repeat(8);
        assert_eq!(shown(Layout::Bgra, &bgra, 16), red);
    }

    #[test]
    fn planes_of_a_taller_texture() {
        // A 4x2 picture in a texture of 4 rows (decoders may pad): the colour plane after all 4 luma rows.
        let mut nv12 = vec![63u8; 4 * 4];
        nv12.extend([102u8, 240].repeat(4));
        let mut out = vec![0u8; 4 * 2 * 4];
        picture_to_bgra(&nv12, 4, 4, Layout::Nv12, (4, 2), &mut out, (4, 2));
        assert_eq!(&out[..4], &[0, 1, 255, 255]);
        assert_eq!(Layout::Nv12.bytes(4, 4), nv12.len());
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
