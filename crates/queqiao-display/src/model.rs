//! What the tray, the banner and the main window report and show, on every system (docs/LINUX.md): the data, apart
//! from the windows that show it (Windows: presence.rs, main_window.rs, forms.rs, window.rs, overlay.rs; Linux: linux/).

use std::path::PathBuf;

use queqiao_core::text::{tr, trf};
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
    /// The main window: connect to this device, taking control from the device that has it (the person said Take
    /// Control).
    TakeOver(String),
    /// The main window's "being controlled" banner: Disconnect (the session another device has with it ends).
    EndControl(String),
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
    /// The device controlling it now (its name), empty when none: one controls a PC at a time (the user, 2026-11-26,
    /// as UU Remote). Its page shows the "being controlled" banner, and Connect asks before taking control.
    pub controlled_by: String,
}

/// The device page's banner while a device of the account controls that PC (UU Remote's "被控中"): its label and
/// text.
pub fn controlled_banner() -> (&'static str, &'static str) {
    (
        tr("Being controlled"),
        tr("A device on your account is currently controlling this computer."),
    )
}

/// The question before taking control of a PC that `controller` (a device of the account) controls: the title, the
/// text and the action's label (beside Cancel), as UU Remote's "接管设备".
pub fn take_over_question(controller: &str) -> (String, String, String) {
    (
        tr("Take Control").to_string(),
        trf(
            "Taking control will disconnect {device}. Do you want to continue?",
            &[("device", controller)],
        ),
        tr("Take Control").to_string(),
    )
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
    /// A slider of the control center (the volume) moved to `value` (0 to 100).
    Value {
        id: u16,
        value: u8,
    },
}

/// What an entry of the file transfer window is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileKind {
    File,
    Folder,
    /// A drive or the file system's root (in the places).
    Drive,
    /// One of the person's own folders (in the places).
    Place,
}

/// One entry of a pane.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileItem {
    /// As that PC names it (in the places: the full path).
    pub name: String,
    /// What the pane shows ("Desktop" for a place's full path).
    pub label: String,
    pub kind: FileKind,
    pub size: u64,
    /// Seconds since 1970 (UTC); 0 when unknown.
    pub modified: i64,
}

/// One side of the file transfer window: this PC (left) or the other (right).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FilePane {
    /// The folder shown, as that PC writes it ("" : the places).
    pub folder: String,
    pub items: Vec<FileItem>,
    /// "Reading...", or why the folder could not be read; empty when all is well.
    pub status: String,
    /// The places for the folder box's list: (label, path).
    pub places: Vec<(String, String)>,
    pub can_go_back: bool,
    pub can_go_up: bool,
}

/// Where a row of the transfer list stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JobRowState {
    Waiting,
    Going,
    Paused,
    Done,
    Failed(String),
    Cancelled,
}

/// One row of the file transfer window's list: something sent or fetched.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JobRow {
    pub id: u32,
    pub name: String,
    /// Several files (a folder).
    pub folder: bool,
    /// This PC sends it (false: it comes here).
    pub sending: bool,
    /// Where it comes from and goes to ("" : that PC's Queqiao downloads folder).
    pub from: String,
    pub to: String,
    pub bytes: u64,
    pub bytes_done: u64,
    pub state: JobRowState,
    pub can_pause: bool,
    /// What arrived here, to show in its folder.
    pub local_path: Option<PathBuf>,
}

/// A row's own commands (its buttons and right-click menu).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobCommand {
    ShowInFolder,
    Pause,
    Resume,
    Cancel,
    /// Off the list (a finished one; a running one is cancelled first).
    Remove,
}

/// What the person did in the file transfer window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileWindowEvent {
    /// Show this folder in a pane ("" : the places); `remote`: the other PC's pane.
    Open {
        remote: bool,
        path: String,
    },
    /// Show the entry `name` of the pane's folder (a double-click on a folder).
    OpenItem {
        remote: bool,
        name: String,
    },
    Up {
        remote: bool,
    },
    Back {
        remote: bool,
    },
    Refresh {
        remote: bool,
    },
    /// Send these entries of a pane (their names in its folder) into the other pane's folder.
    Send {
        from_remote: bool,
        names: Vec<String>,
    },
    /// Files dropped on the window from Explorer: into the other PC's folder.
    Dropped(Vec<PathBuf>),
    PauseAll,
    ResumeAll,
    CancelAll,
    ClearFinished,
    Job {
        id: u32,
        command: JobCommand,
    },
    Closed,
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

/// A page of the main window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Page {
    Devices,
    Help,
    Transfers,
    Settings,
}

impl Page {
    pub(crate) const ALL: [Page; 4] = [Page::Devices, Page::Help, Page::Transfers, Page::Settings];

    pub(crate) fn title(self) -> &'static str {
        match self {
            Page::Devices => tr("Devices"),
            Page::Help => tr("Help"),
            Page::Transfers => tr("Transfers"),
            Page::Settings => tr("Settings"),
        }
    }

    pub(crate) fn subtitle(self) -> &'static str {
        match self {
            Page::Devices => tr("Devices logged in to your account can control each other."),
            Page::Help => tr(
                "Help across accounts: always with the help ID and code, and only after an Allow.",
            ),
            Page::Transfers => tr("Files sent and received in sessions."),
            Page::Settings => tr("What may happen on this PC, and how Queqiao runs."),
        }
    }
}

/// What the login form asks for.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LoginFields {
    /// host:port of queqiao-server.
    pub server: String,
    /// The server certificate's SHA-256 fingerprint, as queqiao-server prints it.
    pub pin: String,
    pub user: String,
    pub password: String,
}

/// What the person did in the login form.
pub enum LoginAction {
    Submit(LoginFields),
    /// "Create an account": a new account with these fields (on a server that takes new accounts).
    Register(LoginFields),
    Cancel,
}

/// What the person did in the "help someone" form.
pub enum HelpAction {
    /// The help ID (digits only) and the code (letters and digits, upper case).
    Submit {
        help_id: String,
        code: String,
    },
    Cancel,
}

/// What the person did in the device list.
pub enum ListAction {
    /// Connect to the device with this id (double-click, Enter or the Connect button).
    Connect(String),
    /// "Help someone...": connect to another account's device with its help ID and code.
    Help,
    /// Wake the device with this id (Wake-on-LAN through another device on its network).
    Wake(String),
    /// "More..." for the device with this id (rename, favourite, remove).
    More(String),
    LogOut,
    Closed,
}

/// One line of the device list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceRow {
    pub id: String,
    pub name: String,
    pub status: String,
}
/// A size as UU Remote shows it ("1.30 KB", "9.32 MB").
pub fn size_text(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.2} {}", UNITS[unit])
    }
}
