//! What the viewer shows: a plain Win32 [`Window`] (which also collects the mouse and keyboard input
//! to send) and a [`Presenter`] that puts decoded NV12 pictures into it through a D3D11 swap chain (GPU
//! video processor, or the CPU on devices without one).

/// Drawing on the controlled PC's screen (the viewer's pen strokes over it).
#[cfg(any(windows, target_os = "linux"))]
mod annotation;
#[cfg(windows)]
mod control_center;
#[cfg(windows)]
mod cursor;
#[cfg(windows)]
mod file_window;
#[cfg(windows)]
mod forms;
#[cfg(windows)]
mod icon;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(windows)]
mod login_window;
#[cfg(windows)]
mod main_window;
mod model;
#[cfg(windows)]
mod overlay;
mod panel;
#[cfg(windows)]
mod presence;
#[cfg(windows)]
mod present;
#[cfg(windows)]
mod privacy;
mod settings;
#[cfg(windows)]
mod setup_wizard;
#[cfg(windows)]
mod shortcuts;
mod tray;
#[cfg(windows)]
mod window;
mod yuv;

#[cfg(any(windows, target_os = "linux"))]
pub use annotation::{DrawingArea, DrawingOverlay};
#[cfg(windows)]
pub use cursor::RemoteCursor;
#[cfg(windows)]
pub use file_window::{FileWindow, show_in_folder};
#[cfg(windows)]
pub use forms::{
    DeviceList, HelpForm, PICK_CLOSED, PasswordAction, PasswordForm, PickList, ask_secret,
    ask_text, confirm, confirm_take_over, copy_text, owner_from_raw, pick_files, popup_menu,
    save_screen_picture, save_window_picture, show_error, show_info,
};
#[cfg(windows)]
pub use icon::app_icon;
#[cfg(target_os = "linux")]
pub use linux::{
    ConsentPrompt, DeviceList, Error, FileWindow, HelpForm, Hwnd, LoginForm, PICK_CLOSED,
    PasswordAction, PasswordForm, PickList, Presence, Presenter, PrivacyScreen, Rect, RemoteCursor,
    SessionBanner, SessionOverlay, Window, ask_secret, ask_text, confirm, confirm_take_over, fit,
    owner_from_raw, pick_files, popup_menu, save_screen_picture, save_window_picture, show_error,
    show_in_folder, show_info, show_running_main,
};
#[cfg(windows)]
pub use login_window::LoginForm;
pub use model::{
    AccountState, BAR_END, BAR_MORE, BarButton, ConnectionRow, DeviceRow, Figures, FileItem,
    FileKind, FilePane, FileWindowEvent, HelpAction, HelpMode, JobCommand, JobRow, JobRowState,
    ListAction, LoginAction, LoginFields, MainDevice, Page, PasswordCheck, PresenceEvent,
    TransferRow, WindowEvent, controlled_banner, missing_login_field, normalize_code,
    sign_up_problem, size_text, take_over_question,
};
#[cfg(windows)]
pub use overlay::SessionOverlay;
pub use panel::{Mark, PanelIcon, PanelItem};
#[cfg(windows)]
pub use presence::{Presence, SessionBanner, show_running_main};
#[cfg(windows)]
pub use present::{Presenter, fit};
#[cfg(windows)]
pub use privacy::PrivacyScreen;
#[cfg(windows)]
pub use setup_wizard::{SetupWizard, WizardAction, WizardPage, WizardSetup};
pub use tray::{MIN_FIXED_CODE, TEST_MENU_LOGIN, TEST_MENU_TERMINAL, TEST_MENU_UPDATE_CHECK};
#[cfg(windows)]
pub use window::Window;
pub use yuv::{Layout, nv12_to_bgra, picture_to_bgra};
