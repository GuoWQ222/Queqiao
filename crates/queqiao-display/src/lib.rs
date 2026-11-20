//! What the viewer shows: a plain Win32 [`Window`] and a [`Presenter`] that puts decoded NV12 pictures
//! into it through a D3D11 swap chain (GPU video processor, or the CPU on devices without one).

mod present;
mod window;
mod yuv;

pub use present::Presenter;
pub use window::Window;
pub use yuv::nv12_to_bgra;
