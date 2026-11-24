//! What the clipboard holds and the checks on it, on every system (docs/LINUX.md): moved from clipboard.rs (Windows);
//! Linux's clipboard (clipboard_x11.rs) shares them. Images go between the two sides as device-independent bitmaps
//! (CF_DIB: header and pixels).

/// Images larger than this are left alone (64 MB).
pub(crate) const MAX_IMAGE: usize = 64 << 20;

/// What the clipboard holds: text, or an image as a device-independent bitmap (CF_DIB bytes).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClipContent {
    Text(String),
    Image(Vec<u8>),
    /// Files and folders copied in Explorer (CF_HDROP).
    Files(Vec<std::path::PathBuf>),
}

impl ClipContent {
    // Windows' watcher knows its own content by it; X11's by the selection's owner.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub(crate) fn fingerprint(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.hash(&mut hasher);
        hasher.finish()
    }
}

impl std::hash::Hash for ClipContent {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        match self {
            ClipContent::Text(text) => (0u8, text).hash(state),
            ClipContent::Image(bytes) => (1u8, bytes).hash(state),
            ClipContent::Files(paths) => (2u8, paths).hash(state),
        }
    }
}

/// Whether `bytes` is a well-formed device-independent bitmap (CF_DIB): a known header, a sane size, 1 plane, a
/// usual bit depth, no compression but bit fields, and as many bytes as its header says (the security review of
/// 2026-11-22: images from the other side went on the clipboard unchecked).
pub fn dib_ok(bytes: &[u8]) -> bool {
    let u32_at = |at: usize| {
        bytes
            .get(at..at + 4)
            .map(|b| u32::from_le_bytes(b.try_into().unwrap_or_default()))
    };
    let u16_at = |at: usize| {
        bytes
            .get(at..at + 2)
            .map(|b| u16::from_le_bytes(b.try_into().unwrap_or_default()))
    };
    let (Some(header), Some(width), Some(height), Some(planes), Some(bits), Some(compression)) = (
        u32_at(0),
        u32_at(4).map(|v| v as i32),
        u32_at(8).map(|v| v as i32),
        u16_at(12),
        u16_at(14),
        u32_at(16),
    ) else {
        return false;
    };
    let used_colors = u32_at(32).unwrap_or(0);
    if !matches!(header, 40 | 52 | 56 | 108 | 124)
        || !(1..=16384).contains(&width)
        || height == 0
        || height.unsigned_abs() > 16384
        || planes != 1
        || !matches!(bits, 1 | 4 | 8 | 16 | 24 | 32)
        || !matches!(compression, 0 | 3)
    {
        return false;
    }
    let palette = if bits <= 8 {
        let most = 1u64 << bits;
        let count = if used_colors == 0 {
            most
        } else {
            u64::from(used_colors)
        };
        if count > most {
            return false;
        }
        count * 4
    } else {
        0
    };
    // Bit fields after a 40-byte header: three masks.
    let masks = if compression == 3 && header == 40 {
        12
    } else {
        0
    };
    let stride = (u64::from(width as u32) * u64::from(bits)).div_ceil(32) * 4;
    let needed = u64::from(header) + palette + masks + stride * u64::from(height.unsigned_abs());
    needed <= bytes.len() as u64
}
