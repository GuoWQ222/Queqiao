//! What the viewer shows: a plain Win32 [`Window`] (which also collects the mouse and keyboard input
//! to send) and a [`Presenter`] that puts decoded NV12 pictures into it through a D3D11 swap chain (GPU
//! video processor, or the CPU on devices without one).

mod present;
mod window;
mod yuv;

pub use present::{Presenter, fit};
pub use window::{Window, WindowEvent};
pub use yuv::nv12_to_bgra;
