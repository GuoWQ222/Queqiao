//! Screen capture, one API on each system (docs/LINUX.md): [`Duplicator`] gives the screen's updates as [`Frame`]s,
//! the pointer's shape and visibility, and pictures in memory ([`BgraImage`]).
//! - Windows (`win.rs`): DXGI Desktop Duplication; frames stay on the GPU as D3D11 textures on the duplicator's
//!   device. Needs a Direct3D 11 GPU and a process in the interactive session; per-monitor DPI aware.
//! - Linux under X11 (`x11.rs`): MIT-SHM into memory shared with the X server, DAMAGE for changes, XFixes for the
//!   pointer, RandR for the monitors. In a Wayland session (`portal.rs`): the ScreenCast portal's PipeWire stream
//!   (`pipewire.rs`, libpipewire loaded at run time).

/// The screens' resolution for a session: the sizes offered, a change, the screen's own size back (Windows, X11).
pub mod display_mode;
/// The screen as the display hardware shows it, for the Queqiao service's desktop helper (root).
#[cfg(target_os = "linux")]
pub mod kms;
/// Framebuffers in a GPU's tiled layout, read through EGL (for kms).
#[cfg(target_os = "linux")]
mod kms_egl;
#[cfg(target_os = "linux")]
mod pipewire;
#[cfg(target_os = "linux")]
mod portal;
#[cfg(target_os = "linux")]
mod wayland_nudge;
#[cfg(windows)]
mod win;
#[cfg(target_os = "linux")]
mod x11;

#[cfg(windows)]
pub use win::*;
#[cfg(target_os = "linux")]
pub use x11::*;

/// The screen being captured.
#[derive(Clone, Debug)]
pub struct OutputInfo {
    /// GPU name, e.g. "NVIDIA GeForce RTX 5080" (Linux: "X11").
    pub adapter: String,
    /// Windows display name, e.g. `\\.\DISPLAY1` (Linux: RandR's monitor name, e.g. "DP-1").
    pub output: String,
    pub width: u32,
    pub height: u32,
    pub refresh_hz: f64,
    /// Clockwise rotation of the display in degrees (0, 90, 180, 270).
    pub rotation: u32,
    /// `DuplicateOutput1` (we choose the pixel format) or the older `DuplicateOutput`; Linux: "MIT-SHM".
    pub method: &'static str,
    /// Top-left corner of the screen on the desktop (physical pixels).
    pub left: i32,
    pub top: i32,
}

/// A frame copied to memory: tightly packed rows, 4 bytes per pixel in B, G, R, A order.
pub struct BgraImage {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

/// The pointer's shape as Windows gives it (DXGI_OUTDUPL_POINTER_SHAPE_INFO): `kind` 1 monochrome, 2 colour,
/// 4 masked colour; rows `pitch` bytes apart; for monochrome, `height` counts both masks. Linux gives colour (2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PointerShapeRaw {
    pub kind: u32,
    pub width: u32,
    pub height: u32,
    pub pitch: u32,
    pub hot_x: i32,
    pub hot_y: i32,
    pub data: Vec<u8>,
}

/// One screen attached to the desktop, as `Duplicator::new` counts them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScreenSummary {
    pub index: usize,
    pub width: u32,
    pub height: u32,
    /// The main screen: `Duplicator::primary` opens it.
    pub primary: bool,
}
