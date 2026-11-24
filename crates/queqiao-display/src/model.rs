//! What the tray, the banner and the main window report and show, on every system (docs/LINUX.md): the data, apart
//! from the windows that show it (Windows: presence.rs, main_window.rs, forms.rs, window.rs, overlay.rs; Linux: linux/).

use std::path::PathBuf;
use std::time::Duration;

/// Checks a typed settings password.
pub type PasswordCheck = Box<dyn Fn(&str) -> bool>;

/// What the person did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PresenceEvent {
    /// End the current session (banner or tray).
    EndSession,
    /// Quit Queqiao on this PC.
    Quit,
    /// Allow this account's devices to control the PC (true) or not.
    SetAllow(bool),
    /// Ask before each session (true) or not.
    SetAsk(bool),
    /// Share the clipboard with the controlling device (true) or not.
    SetClipboard(bool),
    /// Accept files from the controlling device (true) or not.
    SetFiles(bool),
    /// Allow remote command lines (true) or not.
    SetTerminal(bool),
    /// Start with Windows (true) or not.
    SetAutostart(bool),
    /// Lock this PC when a session ends (true) or not.
    SetLockAfter(bool),
    /// A setting was chosen while settings are locked (someone of another account is controlling, or the
    /// settings password was not given): refused.
    SettingsLocked,
    /// A new settings password (Some), or none any more.
    SetPassword(Option<String>),
    /// "Let someone help me...": open help requests (show the help ID and a code).
    OpenHelp,
    /// The help window asks for a new code.
    HelpNewCode,
    /// The help window was closed: no more help requests.
    HelpClose,
    /// The answer to the prompt: allowed, or denied (also when the time ran out or it was closed).
    Consent(bool),
    /// "Log in...": the person wants to log this PC in to their account (offered while it is not online).
    LogIn,
    /// "Check for updates...".
    CheckUpdates,
    /// Install updates by itself (true) or only when asked.
    SetAutoUpdate(bool),
    /// A fixed help code for this PC (normalized: upper-case letters and digits), or none any more.
    SetFixedHelpCode(Option<String>),
    /// "Wake-on-LAN check...".
    CheckWakeOnLan,
    /// The main window: connect to this device of the account.
    Connect(String),
    /// The main window: connect to this device and send it files (a file picker in the session).
    SendFiles(String),
    /// The main window: connect to this device and open a command line on it.
    OpenCommandLine(String),
    /// The main window: wake this device up (Wake-on-LAN through a device of the account next to it).
    Wake(String),
    /// The main window: take this device out of the account (after the person confirmed it).
    RemoveDevice(String),
    /// The main window: help someone of another account with their help ID and code.
    HelpSomeone { help_id: String, code: String },
    /// The main window: log this PC's Queqiao out of the account.
    LogOut,
    /// The main window: a new help ID for this PC (the old one stops working).
    NewHelpId,
    /// The main window: open the folder received files go to.
    OpenReceivedFiles,
    /// The main window: forget the list of transfers.
    ClearTransfers,
    /// The main window was opened (the agent fills in what it shows).
    MainShown,
    /// How someone of another account may start helping (Settings).
    SetHelpMode(HelpMode),
    /// Files also while someone of another account helps (true) or not.
    SetHelpFiles(bool),
}

/// How someone of another account may start helping this PC with its help ID: UU Remote's three ways.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HelpMode {
    /// The help code, then the person at this PC allows it (the default).
    #[default]
    CodeAndAsk,
    /// The help code alone: no prompt.
    CodeOnly,
    /// No code: the person at this PC allows each request.
    AskOnly,
}

/// How this PC stands with the account server (the tray menu's first line).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccountState {
    Online,
    /// Logged in, but the server cannot be reached at the moment: Queqiao tries again.
    Offline,
    /// No login, or the server wants a new one: the menu offers "Log in...".
    LoggedOut,
}

/// One device in the sidebar.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MainDevice {
    pub id: String,
    pub name: String,
    pub online: bool,
    /// This PC itself (not to be connected to).
    pub this_pc: bool,
}

/// One file in the Transfers page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransferRow {
    /// Received here (else sent from here).
    pub received: bool,
    pub name: String,
    /// "from Office PC · 12.4 MB · 14:03" and the like.
    pub detail: String,
}

/// One session this PC started, for a device's "Recent connections".
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConnectionRow {
    pub device_id: String,
    /// "Today 15:08", "2026-11-21 09:30".
    pub when: String,
    /// "12 min" and the like.
    pub length: String,
}

/// A help code as typed (spaces, dashes, any case) in the form the server compares: letters and digits, upper
/// case.
pub fn normalize_code(typed: &str) -> String {
    typed
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_uppercase())
        .collect()
}

/// Input in the window. Positions are in pixels of the window's drawing area.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WindowEvent {
    MouseMove {
        x: i32,
        y: i32,
    },
    /// 0 left, 1 right, 2 middle, 3 back (X1), 4 forward (X2).
    MouseButton {
        button: u8,
        down: bool,
    },
    /// 120 = one notch.
    Wheel {
        delta: i16,
        horizontal: bool,
    },
    /// Hardware scancode (set 1) and the "extended" flag, as Windows reports them.
    Key {
        scancode: u16,
        extended: bool,
        down: bool,
    },
    /// The window lost the keyboard: keys still held must be released on the remote side.
    FocusLost,
    /// Files (or folders) dropped on the window from Explorer: to send to the other PC.
    FilesDropped(Vec<PathBuf>),
    /// An item added with [`Window::add_menu_items`] was chosen.
    Menu(u16),
    /// Game mouse: the mouse moved by this much (raw counts), while the pointer is held in the window.
    MouseDelta {
        dx: i32,
        dy: i32,
    },
    /// A hotkey registered with [`Window::register_hotkey`] was pressed (anywhere).
    Hotkey(i32),
}

/// A button of the bar: its glyph (Segoe MDL2 Assets), its label, and the menu id it sends to the session window.
/// `MORE` opens the session window's menu, `END` closes the session window.
#[derive(Clone, Debug)]
pub struct BarButton {
    pub glyph: &'static str,
    pub label: String,
    pub id: u16,
}

/// Ids of the two buttons the bar handles itself.
pub const BAR_MORE: u16 = 0xFFF0;
pub const BAR_END: u16 = 0xFFE0;

/// The session's figures as the card shows them.
#[derive(Clone, Debug, Default)]
pub struct Figures {
    pub relayed: bool,
    pub delay_ms: Option<f64>,
    pub fps: f64,
    /// "8.4 Mbit/s", "320 kbit/s".
    pub rate: String,
    /// "H.264 · GPU".
    pub codec: String,
    pub picture: (u32, u32),
    pub elapsed: Duration,
    /// The latest delays, oldest first.
    pub delays: Vec<f64>,
}
