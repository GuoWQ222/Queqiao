//! What the viewer shows: a plain Win32 [`Window`] (which also collects the mouse and keyboard input
//! to send) and a [`Presenter`] that puts decoded NV12 pictures into it through a D3D11 swap chain (GPU
//! video processor, or the CPU on devices without one).

mod forms;
mod presence;
mod present;
mod privacy;
mod shortcuts;
mod window;
mod yuv;

pub use forms::{
    DeviceList, DeviceRow, HelpAction, HelpForm, ListAction, LoginAction, LoginFields, LoginForm,
    PICK_CLOSED, PickList, ask_text, confirm, normalize_code, popup_menu, save_window_picture,
    show_error,
};
pub use presence::{PasswordCheck, Presence, PresenceEvent, SessionBanner, TEST_MENU_TERMINAL};
pub use present::{Presenter, fit};
pub use privacy::PrivacyScreen;
pub use window::{Window, WindowEvent};
pub use yuv::nv12_to_bgra;
