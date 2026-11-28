//! What the display crate gives on Linux (docs/LINUX.md), with the API of Windows' modules:
//! - the viewer's session window (window.rs, present.rs, cursor.rs): X11 through x11rb (on Wayland desktops through
//!   XWayland), a connection and window per session thread as on Windows, which a single toolkit event loop would not
//!   allow;
//! - Queqiao's own windows (gui.rs: egui on one GUI thread): the tray and the main window (presence.rs), the session
//!   banner and consent prompt (banner.rs), dialogs (dialogs.rs);
//! - what is not on Linux yet (the session bar, lists, the password form, the privacy screen) is refused with a message.

/// Drawing on the controlled PC's screen: its X11 window (crate::annotation has the rest).
pub(crate) mod annotation;
mod banner;
mod control_center;
mod cursor;
mod dialogs;
mod file_window;
mod forms;
mod gui;
mod icon;
mod lists;
mod login;
mod main_view;
mod overlay;
mod presence;
mod present;
mod theme;
mod window;

pub use banner::{ConsentPrompt, SessionBanner};
pub use cursor::RemoteCursor;
pub use dialogs::{ask_secret, ask_text, confirm_take_over, show_error, show_info};
pub use file_window::{FileWindow, show_in_folder};
pub use forms::{owner_from_raw, pick_files, popup_menu, save_screen_picture, save_window_picture};
pub use lists::{DeviceList, HelpForm, PICK_CLOSED, PasswordAction, PasswordForm, PickList};
pub use login::LoginForm;
pub use overlay::SessionOverlay;
pub use presence::{Presence, PrivacyScreen, show_running_main};
pub use present::{Presenter, fit};
pub use window::{Hwnd, Rect, Window};

/// A display error (Windows: `windows::core::Error`): its text, as `message()` gives Windows'.
#[derive(Clone, Debug)]
pub struct Error(String);

impl Error {
    pub fn message(&self) -> String {
        self.0.clone()
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

impl From<String> for Error {
    fn from(text: String) -> Error {
        Error(text)
    }
}

impl From<&str> for Error {
    fn from(text: &str) -> Error {
        Error(text.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;

fn x11_error(error: impl std::fmt::Display) -> Error {
    Error(format!("X11: {error}"))
}

/// Yes or no, asked of the person (Windows: forms::confirm with its owner window).
pub fn confirm(_owner: Hwnd, title: &str, text: &str) -> bool {
    dialogs::confirm(title, text)
}
