//! What the viewer shows: a plain Win32 [`Window`] (which also collects the mouse and keyboard input
//! to send) and a [`Presenter`] that puts decoded NV12 pictures into it through a D3D11 swap chain (GPU
//! video processor, or the CPU on devices without one).

#[cfg(windows)]
mod cursor;
#[cfg(windows)]
mod forms;
#[cfg(windows)]
mod icon;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(windows)]
mod main_window;
mod model;
#[cfg(windows)]
mod overlay;
#[cfg(windows)]
mod presence;
#[cfg(windows)]
mod present;
#[cfg(windows)]
mod privacy;
#[cfg(windows)]
mod shortcuts;
#[cfg(windows)]
mod window;
mod yuv;

#[cfg(windows)]
pub use cursor::RemoteCursor;
#[cfg(windows)]
pub use forms::{
    DeviceList, DeviceRow, HelpAction, HelpForm, ListAction, LoginAction, LoginFields, LoginForm,
    PICK_CLOSED, PasswordAction, PasswordForm, PickList, SetupAction, SetupForm, ask_secret,
    ask_text, confirm, copy_text, owner_from_raw, pick_files, popup_menu, save_screen_picture,
    save_window_picture, show_error, show_info,
};
#[cfg(windows)]
pub use icon::app_icon;
#[cfg(target_os = "linux")]
pub use linux::{
    ConsentPrompt, Hwnd, PICK_CLOSED, PasswordAction, PasswordForm, PickList, Presenter, Rect,
    RemoteCursor, SessionBanner, SessionOverlay, Window, ask_text, fit, owner_from_raw, pick_files,
    popup_menu, save_screen_picture, show_error, show_info,
};
#[cfg(windows)]
pub use main_window::Page;
pub use model::{
    AccountState, BAR_END, BAR_MORE, BarButton, ConnectionRow, Figures, HelpMode, MainDevice,
    PasswordCheck, PresenceEvent, TransferRow, WindowEvent, normalize_code,
};
#[cfg(windows)]
pub use overlay::SessionOverlay;
#[cfg(windows)]
pub use presence::{
    Presence, SessionBanner, TEST_MENU_LOGIN, TEST_MENU_TERMINAL, TEST_MENU_UPDATE_CHECK,
    show_running_main,
};
#[cfg(windows)]
pub use present::{Presenter, fit};
#[cfg(windows)]
pub use privacy::PrivacyScreen;
#[cfg(windows)]
pub use window::Window;
pub use yuv::{Layout, nv12_to_bgra, picture_to_bgra};
