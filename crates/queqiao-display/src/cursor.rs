//! That PC's pointer shape as this window's cursor: the local pointer (no delay) looks like the remote one (an I-beam
//! over text, arrows at a window's edge). Made from the shape the agent sends, as Windows' own cursors are made: an
//! AND mask and an XOR mask (monochrome), a colour bitmap with alpha (colour), or a colour bitmap with an AND mask
//! where the alpha said XOR (masked colour).

use queqiao_core::protocol::{PointerKind, PointerShape};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateBitmap, CreateDIBSection, DIB_RGB_COLORS,
    DeleteObject, HBITMAP, HGDIOBJ,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateIconIndirect, DestroyIcon, HCURSOR, HICON, ICONINFO,
};

pub struct RemoteCursor(HCURSOR);

impl RemoteCursor {
    pub fn handle(&self) -> HCURSOR {
        self.0
    }

    /// The cursor for `shape` (None if it is not a valid shape or Windows refused it).
    pub fn from_shape(shape: &PointerShape) -> Option<RemoteCursor> {
        if !shape.is_valid() {
            return None;
        }
        let (width, height) = (u32::from(shape.width), u32::from(shape.height));
        let (mask, color) = match shape.kind {
            PointerKind::Monochrome => {
                // Both masks in one bitmap twice the height; GDI wants rows padded to 16 bits.
                let bits =
                    padded_rows(&shape.data, width.div_ceil(8) as usize, height as usize * 2);
                (monochrome(width, height * 2, &bits)?, None)
            }
            PointerKind::Color => {
                let empty = vec![0u8; row_bytes(width) * height as usize];
                (
                    monochrome(width, height, &empty)?,
                    Some(color_bitmap(width, height, &shape.data)?),
                )
            }
            PointerKind::MaskedColor => {
                // Alpha 0xFF: XOR with the screen (AND mask bit 1); 0: replace (bit 0). The colour bitmap's alpha
                // is cleared so that Windows uses the mask, not blending.
                let row = row_bytes(width);
                let mut and = vec![0u8; row * height as usize];
                let mut pixels = shape.data.clone();
                for y in 0..height as usize {
                    for x in 0..width as usize {
                        let at = (y * width as usize + x) * 4;
                        if pixels[at + 3] == 0xFF {
                            and[y * row + x / 8] |= 0x80 >> (x % 8);
                        }
                        pixels[at + 3] = 0;
                    }
                }
                (
                    monochrome(width, height, &and)?,
                    Some(color_bitmap(width, height, &pixels)?),
                )
            }
        };
        let info = ICONINFO {
            fIcon: false.into(),
            xHotspot: u32::from(shape.hot_x),
            yHotspot: u32::from(shape.hot_y),
            hbmMask: mask,
            hbmColor: color.unwrap_or_default(),
        };
        let icon = unsafe { CreateIconIndirect(&info) };
        // The cursor holds copies.
        unsafe {
            let _ = DeleteObject(HGDIOBJ(mask.0));
            if let Some(color) = color {
                let _ = DeleteObject(HGDIOBJ(color.0));
            }
        }
        icon.ok().map(|icon| RemoteCursor(HCURSOR(icon.0)))
    }
}

impl Drop for RemoteCursor {
    fn drop(&mut self) {
        let _ = unsafe { DestroyIcon(HICON(self.0.0)) };
    }
}

/// Bytes per row of a 1-bit bitmap for GDI (padded to 16 bits).
fn row_bytes(width: u32) -> usize {
    (width as usize).div_ceil(16) * 2
}

/// Rows of `row` bytes padded to GDI's 16-bit rows.
fn padded_rows(data: &[u8], row: usize, rows: usize) -> Vec<u8> {
    let padded = row.div_ceil(2) * 2;
    let mut out = vec![0u8; padded * rows];
    for y in 0..rows {
        out[y * padded..y * padded + row].copy_from_slice(&data[y * row..y * row + row]);
    }
    out
}

fn monochrome(width: u32, height: u32, bits: &[u8]) -> Option<HBITMAP> {
    let bitmap = unsafe {
        CreateBitmap(
            width as i32,
            height as i32,
            1,
            1,
            Some(bits.as_ptr().cast()),
        )
    };
    (!bitmap.is_invalid()).then_some(bitmap)
}

/// A 32-bit top-down bitmap with `pixels` (B, G, R, A).
fn color_bitmap(width: u32, height: u32, pixels: &[u8]) -> Option<HBITMAP> {
    let info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width as i32,
            biHeight: -(height as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut bits = std::ptr::null_mut();
    let bitmap =
        unsafe { CreateDIBSection(None, &info, DIB_RGB_COLORS, &mut bits, None, 0) }.ok()?;
    if bits.is_null() {
        let _ = unsafe { DeleteObject(HGDIOBJ(bitmap.0)) };
        return None;
    }
    unsafe { std::ptr::copy_nonoverlapping(pixels.as_ptr(), bits.cast::<u8>(), pixels.len()) };
    Some(bitmap)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shape(kind: PointerKind, width: u16, height: u16) -> PointerShape {
        PointerShape {
            kind,
            width,
            height,
            hot_x: 1,
            hot_y: 1,
            data: vec![0x5A; PointerShape::expected_len(kind, width, height)],
        }
    }

    #[test]
    fn every_kind_makes_a_cursor() {
        for kind in [
            PointerKind::Monochrome,
            PointerKind::Color,
            PointerKind::MaskedColor,
        ] {
            for (width, height) in [(32, 32), (17, 9), (48, 48)] {
                assert!(
                    RemoteCursor::from_shape(&shape(kind, width, height)).is_some(),
                    "{kind:?} {width}x{height}"
                );
            }
        }
        // Not a valid shape: none.
        let mut bad = shape(PointerKind::Color, 32, 32);
        bad.data.pop();
        assert!(RemoteCursor::from_shape(&bad).is_none());
    }

    #[test]
    fn rows_are_padded_for_gdi() {
        assert_eq!(padded_rows(&[1, 2, 3], 3, 1), vec![1, 2, 3, 0]);
        assert_eq!(padded_rows(&[1, 2, 3, 4], 2, 2), vec![1, 2, 3, 4]);
        assert_eq!(row_bytes(17), 4);
        assert_eq!(row_bytes(32), 4);
    }
}
