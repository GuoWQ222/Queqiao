//! That PC's pointer shape as the session window's cursor on X11 (Windows: cursor.rs). X cursors are ARGB pictures
//! (RENDER), which cannot invert what lies below as Windows' monochrome and masked-colour pointers can: those pixels
//! are drawn black (the text cursor's I-beam on a light page, as remote-desktop programs show it).

use queqiao_core::protocol::{PointerKind, PointerShape};
use x11rb::connection::Connection;
use x11rb::protocol::render::{self, ConnectionExt as _, PictType};
use x11rb::protocol::xproto::{ConnectionExt as _, ImageFormat};
use x11rb::rust_connection::RustConnection;

/// A pointer shape ready to be made an X cursor: premultiplied ARGB, top-down.
pub struct RemoteCursor {
    width: u16,
    height: u16,
    hot_x: u16,
    hot_y: u16,
    argb: Vec<u32>,
}

impl RemoteCursor {
    /// The cursor for `shape` (None if it is not a valid shape).
    pub fn from_shape(shape: &PointerShape) -> Option<RemoteCursor> {
        if !shape.is_valid() {
            return None;
        }
        let (width, height) = (usize::from(shape.width), usize::from(shape.height));
        let argb = (0..width * height)
            .map(|at| {
                let (x, y) = (at % width, at / width);
                match shape.kind {
                    PointerKind::Monochrome => {
                        let row = width.div_ceil(8);
                        let bit = |plane: usize| {
                            shape.data[(plane * height + y) * row + x / 8] & (0x80 >> (x % 8)) != 0
                        };
                        match (bit(0), bit(1)) {
                            // AND 0: black or white; AND 1: transparent, or inverted (drawn black).
                            (false, false) => 0xFF00_0000,
                            (false, true) => 0xFFFF_FFFF,
                            (true, false) => 0,
                            (true, true) => 0xFF00_0000,
                        }
                    }
                    PointerKind::Color | PointerKind::MaskedColor => {
                        let pixel = &shape.data[at * 4..at * 4 + 4];
                        let (b, g, r, a) = (
                            u32::from(pixel[0]),
                            u32::from(pixel[1]),
                            u32::from(pixel[2]),
                            u32::from(pixel[3]),
                        );
                        let a = match shape.kind {
                            // Alpha 0xFF: XOR with the screen (shown as its colour; black XORs to nothing); 0: replaces.
                            PointerKind::MaskedColor if a == 0xFF && (r | g | b) == 0 => 0,
                            PointerKind::MaskedColor => 0xFF,
                            _ => a,
                        };
                        let premultiply = |c: u32| (c * a + 127) / 255;
                        (a << 24) | (premultiply(r) << 16) | (premultiply(g) << 8) | premultiply(b)
                    }
                }
            })
            .collect();
        Some(RemoteCursor {
            width: shape.width,
            height: shape.height,
            hot_x: shape.hot_x.min(shape.width.saturating_sub(1)),
            hot_y: shape.hot_y.min(shape.height.saturating_sub(1)),
            argb,
        })
    }

    /// A cursor that shows nothing (the pointer hidden).
    pub(super) fn invisible() -> RemoteCursor {
        RemoteCursor {
            width: 1,
            height: 1,
            hot_x: 0,
            hot_y: 0,
            argb: vec![0],
        }
    }
}

/// An X cursor made from a [`RemoteCursor`]; the window that made it frees it (`free`).
pub(super) struct XCursor {
    pub(super) id: u32,
}

impl XCursor {
    /// The cursor on `connection` (the window's own). None without RENDER's ARGB format.
    pub(super) fn new(
        connection: &RustConnection,
        root: u32,
        cursor: &RemoteCursor,
    ) -> Option<XCursor> {
        let formats = connection.render_query_pict_formats().ok()?.reply().ok()?;
        let argb = formats.formats.iter().find(|f| {
            f.type_ == PictType::DIRECT
                && f.depth == 32
                && f.direct.alpha_mask == 0xFF
                && f.direct.alpha_shift == 24
                && f.direct.red_shift == 16
                && f.direct.green_shift == 8
                && f.direct.blue_shift == 0
        })?;
        let (width, height) = (cursor.width, cursor.height);
        let pixmap = connection.generate_id().ok()?;
        connection
            .create_pixmap(32, pixmap, root, width, height)
            .ok()?;
        let gc = connection.generate_id().ok()?;
        connection.create_gc(gc, pixmap, &Default::default()).ok()?;
        let bytes: Vec<u8> = cursor.argb.iter().flat_map(|p| p.to_le_bytes()).collect();
        let _ = connection.put_image(
            ImageFormat::Z_PIXMAP,
            pixmap,
            gc,
            width,
            height,
            0,
            0,
            0,
            32,
            &bytes,
        );
        let picture = connection.generate_id().ok()?;
        let _ = connection.render_create_picture(
            picture,
            pixmap,
            argb.id,
            &render::CreatePictureAux::new(),
        );
        let id = connection.generate_id().ok()?;
        let made = connection
            .render_create_cursor(id, picture, cursor.hot_x, cursor.hot_y)
            .ok()
            .and_then(|cookie| cookie.check().ok())
            .is_some();
        let _ = connection.render_free_picture(picture);
        let _ = connection.free_gc(gc);
        let _ = connection.free_pixmap(pixmap);
        let _ = connection.flush();
        made.then_some(XCursor { id })
    }

    pub(super) fn free(self, connection: &RustConnection) {
        let _ = connection.free_cursor(self.id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shape(kind: PointerKind, data: Vec<u8>) -> PointerShape {
        PointerShape {
            kind,
            width: 2,
            height: 1,
            hot_x: 1,
            hot_y: 0,
            data,
        }
    }

    #[test]
    fn every_kind_becomes_premultiplied_argb() {
        // Monochrome 2x1: AND row then XOR row, one byte each: pixel 0 black (0/0), pixel 1 transparent (1/0).
        let mono =
            RemoteCursor::from_shape(&shape(PointerKind::Monochrome, vec![0x40, 0x00])).unwrap();
        assert_eq!(mono.argb, vec![0xFF00_0000, 0]);
        assert_eq!((mono.hot_x, mono.hot_y), (1, 0));
        // Colour: half-transparent red becomes premultiplied.
        let color =
            RemoteCursor::from_shape(&shape(PointerKind::Color, vec![0, 0, 255, 128, 0, 0, 0, 0]))
                .unwrap();
        assert_eq!(color.argb, vec![0x8080_0000, 0]);
        // Masked colour: replace (alpha 0) is opaque; XOR with black is nothing.
        let masked = RemoteCursor::from_shape(&shape(
            PointerKind::MaskedColor,
            vec![255, 0, 0, 0, 0, 0, 0, 255],
        ))
        .unwrap();
        assert_eq!(masked.argb, vec![0xFF00_00FF, 0]);
        let mut bad = shape(PointerKind::Color, vec![0; 8]);
        bad.data.pop();
        assert!(RemoteCursor::from_shape(&bad).is_none());
    }
}
