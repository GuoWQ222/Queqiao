//! What the viewer shows: a plain Win32 [`Window`] (which also collects the mouse and keyboard input
//! to send) and a [`Presenter`] that puts decoded NV12 pictures into it through a D3D11 swap chain (GPU
//! video processor, or the CPU on devices without one).

mod cursor;
mod forms;
mod icon;
mod main_window;
mod overlay;
mod presence;
mod present;
mod privacy;
mod shortcuts;
mod window;
mod yuv;

pub use cursor::RemoteCursor;
pub use forms::{
    DeviceList, DeviceRow, HelpAction, HelpForm, ListAction, LoginAction, LoginFields, LoginForm,
    PICK_CLOSED, PasswordAction, PasswordForm, PickList, SetupAction, SetupForm, ask_secret,
    ask_text, confirm, copy_text, normalize_code, pick_files, popup_menu, save_screen_picture,
    save_window_picture, show_error, show_info,
};
pub use icon::app_icon;
pub use main_window::{ConnectionRow, MainDevice, Page, TransferRow};
pub use overlay::{BAR_END, BAR_MORE, BarButton, Figures, SessionOverlay};
pub use presence::{
    AccountState, HelpMode, PasswordCheck, Presence, PresenceEvent, SessionBanner, TEST_MENU_LOGIN,
    TEST_MENU_TERMINAL, TEST_MENU_UPDATE_CHECK, show_running_main,
};
pub use present::{Presenter, fit};
pub use privacy::PrivacyScreen;
pub use window::{Window, WindowEvent};
pub use yuv::nv12_to_bgra;
