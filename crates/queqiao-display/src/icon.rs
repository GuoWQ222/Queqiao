//! Queqiao's icon: a magpie (喜鹊) perched on a small gold arch, the magpie bridge (鹊桥) of the Qixi legend. Each
//! program carries it as icon group 1 (assets\queqiao.ico, embedded by the build scripts; drawn by
//! assets\icon\magpie_icon.js), with images made for each size, the small ones with a stronger light rim for dark
//! taskbars. A program without it (the examples, the tests) gets the earlier icon, drawn here in code: a blue rounded
//! square with a white bridge and the star it leads to.

use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateBitmap, CreateDIBSection, DIB_RGB_COLORS,
    DeleteObject,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateIconIndirect, HICON, ICONINFO, IMAGE_ICON, LR_DEFAULTCOLOR, LoadImageW,
};
use windows::core::PCWSTR;

/// The icon at `size` x `size` pixels (16 for the tray and title bars, 32 and more elsewhere).
pub fn app_icon(size: i32) -> HICON {
    let size = size.clamp(8, 256);
    resource_icon(1, size).unwrap_or_else(|| drawn_icon(size))
}

/// The magpie with its wings spread (icon group 2: assets\magpie_spread.ico), at `size` pixels: the main window's
/// card puts three of them on its bridge. None in a program without it.
pub(crate) fn spread_magpie(size: i32) -> Option<HICON> {
    resource_icon(2, size.clamp(8, 256))
}

/// Icon group `id` of this program at `size` pixels: Windows picks the image made for that size.
fn resource_icon(id: usize, size: i32) -> Option<HICON> {
    let module = unsafe { GetModuleHandleW(None) }.ok()?;
    let handle = unsafe {
        LoadImageW(
            Some(module.into()),
            // MAKEINTRESOURCE(id): the id in the pointer.
            PCWSTR(std::ptr::without_provenance(id)),
            IMAGE_ICON,
            size,
            size,
            LR_DEFAULTCOLOR,
        )
    }
    .ok()?;
    (!handle.is_invalid()).then_some(HICON(handle.0))
}

/// The earlier icon, drawn in code (for programs without the resource).
fn drawn_icon(size: i32) -> HICON {
    let pixels = draw(size as usize);
    let info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: size,
            biHeight: -size,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut bits = std::ptr::null_mut();
    let Ok(color) = (unsafe { CreateDIBSection(None, &info, DIB_RGB_COLORS, &mut bits, None, 0) })
    else {
        return HICON::default();
    };
    if bits.is_null() {
        let _ = unsafe { DeleteObject(color.into()) };
        return HICON::default();
    }
    unsafe { std::ptr::copy_nonoverlapping(pixels.as_ptr(), bits.cast::<u32>(), pixels.len()) };
    // The mask is unused with 32-bit colour (its alpha decides), but must be there.
    let mask = unsafe { CreateBitmap(size, size, 1, 1, None) };
    let icon_info = ICONINFO {
        fIcon: true.into(),
        hbmMask: mask,
        hbmColor: color,
        ..Default::default()
    };
    let icon = unsafe { CreateIconIndirect(&icon_info) }.unwrap_or_default();
    unsafe {
        let _ = DeleteObject(color.into());
        let _ = DeleteObject(mask.into());
    }
    icon
}

/// The icon's pixels, BGRA (straight alpha), rows from the top.
pub(crate) fn draw(size: usize) -> Vec<u32> {
    let n = size as f64;
    let mut out = vec![0u32; size * size];
    // Four samples per pixel for the edges.
    let offsets = [(0.25, 0.25), (0.75, 0.25), (0.25, 0.75), (0.75, 0.75)];
    for y in 0..size {
        for x in 0..size {
            let (mut square, mut white) = (0.0, 0.0);
            for (dx, dy) in offsets {
                let (px, py) = ((x as f64 + dx) / n, (y as f64 + dy) / n);
                if in_rounded_square(px, py) {
                    square += 0.25;
                    if in_bridge(px, py) || in_star(px, py) {
                        white += 0.25;
                    }
                }
            }
            if square == 0.0 {
                continue;
            }
            // Blue, a little lighter at the top; white where the bridge and the star are.
            let t = y as f64 / n;
            let (r, g, b) = (
                0.0 + 26.0 * (1.0 - t),
                103.0 + 30.0 * (1.0 - t),
                192.0 + 40.0 * (1.0 - t),
            );
            let mix = white / square;
            let channel = |c: f64| (c + (255.0 - c) * mix).round().clamp(0.0, 255.0) as u32;
            let alpha = (square * 255.0).round() as u32;
            out[y * size + x] = (alpha << 24) | (channel(r) << 16) | (channel(g) << 8) | channel(b);
        }
    }
    out
}

fn in_rounded_square(x: f64, y: f64) -> bool {
    let (margin, radius) = (0.04, 0.22);
    let (lo, hi) = (margin + radius, 1.0 - margin - radius);
    let cx = x.clamp(lo, hi);
    let cy = y.clamp(lo, hi);
    (x - cx).powi(2) + (y - cy).powi(2) <= radius * radius
        && (margin..=1.0 - margin).contains(&x)
        && (margin..=1.0 - margin).contains(&y)
}

/// The bridge: an arch (the upper half of a ring) over the lower part, with two short piers.
fn in_bridge(x: f64, y: f64) -> bool {
    let (cx, cy) = (0.5, 0.80);
    let distance = ((x - cx).powi(2) + (y - cy).powi(2)).sqrt();
    let arch = y <= cy && (0.27..=0.37).contains(&distance);
    let deck = (0.16..=0.84).contains(&x) && (0.74..=0.80).contains(&y);
    arch || deck
}

/// A small four-pointed star above the bridge (the Weaver Girl's).
fn in_star(x: f64, y: f64) -> bool {
    let (cx, cy, r): (f64, f64, f64) = (0.5, 0.27, 0.11);
    let (dx, dy) = ((x - cx).abs(), (y - cy).abs());
    // A diamond with concave sides: |dx|^0.5 + |dy|^0.5 <= r^0.5.
    dx.sqrt() + dy.sqrt() <= r.sqrt()
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_icon_has_its_shapes() {
        let size = 64;
        let pixels = super::draw(size);
        let at = |x: usize, y: usize| pixels[y * size + x];
        // Corners are transparent, the middle of the square is blue, the star's middle and the deck white.
        assert_eq!(at(0, 0) >> 24, 0);
        let blue = at(32, 40);
        assert_eq!(blue >> 24, 255);
        assert!(
            (blue & 0xFF) > ((blue >> 16) & 0xFF) + 100,
            "blue: {blue:08X}"
        );
        assert_eq!(at(32, 17) & 0x00FF_FFFF, 0x00FF_FFFF, "the star");
        assert_eq!(at(32, 49) & 0x00FF_FFFF, 0x00FF_FFFF, "the deck");
    }
}
