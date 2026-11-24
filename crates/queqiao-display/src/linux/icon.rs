//! Queqiao's icon on Linux: the magpie of assets/queqiao.ico (the same file Windows embeds), read from the program
//! itself: the image made for the size asked for (or the nearest larger one), as RGBA, rows from the top.

/// assets/queqiao.ico, compiled in.
const ICO: &[u8] = include_bytes!("../../../../assets/queqiao.ico");

/// The icon as `(width, height, RGBA)`, the image nearest to `size` (larger preferred).
pub(crate) fn icon(size: u32) -> Option<(u32, u32, Vec<u8>)> {
    let count = usize::from(u16::from_le_bytes(ICO.get(4..6)?.try_into().ok()?));
    let mut entries = Vec::new();
    for index in 0..count {
        let at = 6 + index * 16;
        let entry = ICO.get(at..at + 16)?;
        let side = |byte: u8| if byte == 0 { 256 } else { u32::from(byte) };
        let length = u32::from_le_bytes(entry[8..12].try_into().ok()?) as usize;
        let offset = u32::from_le_bytes(entry[12..16].try_into().ok()?) as usize;
        entries.push((
            side(entry[0]),
            side(entry[1]),
            ICO.get(offset..offset + length)?,
        ));
    }
    let (width, height, data) = entries
        .iter()
        .filter(|(w, _, _)| *w >= size)
        .min_by_key(|(w, _, _)| *w)
        .or_else(|| entries.iter().max_by_key(|(w, _, _)| *w))?;
    let rgba = if data.starts_with(b"\x89PNG") {
        decode_png(data)?
    } else {
        decode_bitmap(data, *width, *height)?
    };
    Some((*width, *height, rgba))
}

fn decode_png(data: &[u8]) -> Option<Vec<u8>> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(data));
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().ok()?;
    let mut buffer = vec![0u8; reader.output_buffer_size()];
    let frame = reader.next_frame(&mut buffer).ok()?;
    match frame.color_type {
        png::ColorType::Rgba => Some(buffer[..frame.buffer_size()].to_vec()),
        png::ColorType::Rgb => Some(
            buffer[..frame.buffer_size()]
                .as_chunks::<3>()
                .0
                .iter()
                .flat_map(|p| [p[0], p[1], p[2], 255])
                .collect(),
        ),
        _ => None,
    }
}

/// A 32-bit icon bitmap: BITMAPINFOHEADER (height doubled for the mask), BGRA rows from the bottom.
fn decode_bitmap(data: &[u8], width: u32, height: u32) -> Option<Vec<u8>> {
    let header = u32::from_le_bytes(data.get(0..4)?.try_into().ok()?) as usize;
    let bits = u16::from_le_bytes(data.get(14..16)?.try_into().ok()?);
    if bits != 32 {
        return None;
    }
    let (w, h) = (width as usize, height as usize);
    let pixels = data.get(header..header + w * h * 4)?;
    let mut rgba = Vec::with_capacity(w * h * 4);
    for row in (0..h).rev() {
        for p in pixels[row * w * 4..(row + 1) * w * 4].as_chunks::<4>().0 {
            rgba.extend_from_slice(&[p[2], p[1], p[0], p[3]]);
        }
    }
    Some(rgba)
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_magpie_comes_in_each_size() {
        for size in [16, 22, 32, 48, 256] {
            let (width, height, rgba) = super::icon(size).expect("an icon");
            assert!(
                width >= size.min(256) && width == height,
                "{size}: {width}x{height}"
            );
            assert_eq!(rgba.len(), (width * height * 4) as usize);
            assert!(
                rgba.as_chunks::<4>().0.iter().any(|p| p[3] > 200),
                "{size}: not all clear"
            );
        }
    }
}
