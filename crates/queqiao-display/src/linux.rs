//! The viewer's session window on Linux (docs/LINUX.md), with the API of Windows' window.rs, present.rs, cursor.rs,
//! overlay.rs and the forms the session opens. X11 through x11rb (on Wayland desktops through XWayland): each session
//! thread has a connection and window of its own, as on Windows, which a single toolkit event loop would not allow.
//! The forms, the main window and the session bar come with the GUI (docs/LINUX.md step 5); until then the bar is
//! refused, and the few dialogs a session opens use zenity when it is there.

mod banner;
mod cursor;
mod forms;
mod gui;
mod overlay;
mod present;
mod window;

pub use banner::{ConsentPrompt, SessionBanner};
pub use cursor::RemoteCursor;
pub use forms::{
    PICK_CLOSED, PasswordAction, PasswordForm, PickList, ask_text, owner_from_raw, pick_files,
    popup_menu, save_screen_picture, show_error, show_info,
};
pub use overlay::SessionOverlay;
pub use present::{Presenter, fit};
pub use window::{Hwnd, Rect, Window};

/// A display error, as text (Windows: `windows::core::Error`).
pub type Result<T> = std::result::Result<T, String>;

fn x11_error(error: impl std::fmt::Display) -> String {
    format!("X11: {error}")
}
