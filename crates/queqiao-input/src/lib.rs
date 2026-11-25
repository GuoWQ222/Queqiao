//! Input on the controlled PC, one API on each system (docs/LINUX.md): [`Injector`] injects the viewer's mouse and
//! keyboard events.
//!
//! Pointer positions arrive relative to the captured screen (0..=65535 across its width and height) and are mapped
//! onto the whole desktop, so any screen of a multi-monitor PC works. Keys arrive as hardware scancodes, so the
//! controlled PC's keyboard layout decides the characters ([`scancode`] maps them for Linux).
//! - Windows (`win.rs`): `SendInput`; the clipboard and the local input block too. The process must be per-monitor
//!   DPI aware (the agent is), so desktop positions are physical pixels.
//! - Linux under X11 (`x11.rs`): the XTEST extension.

#[cfg(windows)]
mod block;
mod clip;
#[cfg(windows)]
mod clipboard;
#[cfg(target_os = "linux")]
mod clipboard_x11;
pub mod scancode;
#[cfg(target_os = "linux")]
mod wayland;
#[cfg(windows)]
mod win;
#[cfg(target_os = "linux")]
mod x11;

#[cfg(windows)]
pub use block::LocalInputBlock;
pub use clip::ClipContent;
#[cfg(windows)]
pub use clipboard::{Clipboard, read_files as clipboard_files, read_text as clipboard_text};
#[cfg(target_os = "linux")]
pub use clipboard_x11::{
    Clipboard, LocalInputBlock, read_files as clipboard_files, read_text as clipboard_text,
};
#[cfg(windows)]
pub use win::*;
#[cfg(target_os = "linux")]
pub use x11::*;

/// The captured screen's place on the desktop, in physical pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScreenRect {
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
}

impl ScreenRect {
    /// The desktop pixel that a protocol position (0..=65535 on each axis) stands for.
    pub fn desktop_point(&self, x: u16, y: u16) -> (i32, i32) {
        let span = |size: u32, value: u16| {
            (i64::from(size.saturating_sub(1)) * i64::from(value) / 65535) as i32
        };
        (
            self.left + span(self.width, x),
            self.top + span(self.height, y),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_positions_map_onto_the_screen() {
        let screen = ScreenRect {
            left: -1920,
            top: 0,
            width: 1920,
            height: 1080,
        };
        assert_eq!(screen.desktop_point(0, 0), (-1920, 0));
        assert_eq!(screen.desktop_point(65535, 65535), (-1, 1079));
        // 1919 * 32768 / 65535 = 959.5 and 1079 * 32768 / 65535 = 539.5, rounded down.
        assert_eq!(screen.desktop_point(32768, 32768), (-961, 539));
    }
}
