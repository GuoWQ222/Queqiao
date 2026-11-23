//! What the person at the controlled PC sees and can do (security point 2: nothing happens unseen):
//! - a tray icon whenever the agent runs, with a menu: end the session, let this account's devices control
//!   the PC, ask before each session, quit;
//! - a banner at the top of the screen during every session, with an End button;
//! - a prompt (Allow / Deny, denied after a countdown) before a session when the PC asks first.
//!
//! All on one thread (the agent's UI thread), polled with [`Presence::next`].

use std::time::{Duration, Instant};

use queqiao_core::text::{tr, trf};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW,
    Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, Result, w};

use crate::forms::{Form, Kind, set_text};
use crate::main_window::{
    ConnectionRow, MainAction, MainDevice, MainWindow, Page, Shown, TransferRow,
};

/// What the person did.
/// Checks a typed settings password.
pub type PasswordCheck = Box<dyn Fn(&str) -> bool>;

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

const TRAY_CALLBACK: u32 = WM_APP + 1;
const TRAY_ID: u32 = 1;
// Menu and button ids.
const MENU_END: u16 = 200;
pub(crate) const MENU_ALLOW: u16 = 201;
pub(crate) const MENU_ASK: u16 = 202;
const MENU_QUIT: u16 = 203;
pub(crate) const MENU_CLIPBOARD: u16 = 204;
pub(crate) const MENU_FILES: u16 = 205;
pub(crate) const MENU_TERMINAL: u16 = 206;
pub(crate) const MENU_AUTOSTART: u16 = 207;
const ID_END: u16 = 210;
pub(crate) const ID_NEW_CODE: u16 = 213;
pub(crate) const ID_HELP_CLOSE: u16 = 214;
const ID_COPY_INVITATION: u16 = 218;
pub(crate) const MENU_HELP: u16 = 208;
pub(crate) const MENU_LOCK_AFTER: u16 = 209;
pub(crate) const MENU_LOGIN: u16 = 215;
pub(crate) const MENU_UPDATE_CHECK: u16 = 216;
pub(crate) const MENU_AUTO_UPDATE: u16 = 217;
pub(crate) const MENU_FIXED_CODE: u16 = 219;
pub(crate) const MENU_WOL_CHECK: u16 = 222;
/// The shortest fixed help code (letters and digits).
pub const MIN_FIXED_CODE: usize = 8;
pub(crate) const MENU_PASSWORD: u16 = 220;
/// Menu items tests can choose (the same path as a click).
pub const TEST_MENU_TERMINAL: u16 = MENU_TERMINAL;
pub const TEST_MENU_LOGIN: u16 = MENU_LOGIN;
pub const TEST_MENU_UPDATE_CHECK: u16 = MENU_UPDATE_CHECK;
const ID_ALLOW: u16 = 211;
const ID_DENY: u16 = 212;
/// The tray window asks for its menu (not a control id).
const SHOW_MENU: u16 = 0xFFFE;
/// A left click on the tray icon: the main window (not a control id).
const SHOW_MAIN: u16 = 0xFFFC;
/// "Open Queqiao" in the tray menu.
const MENU_OPEN: u16 = 223;
/// The help modes and files while someone helps (the main window's Settings page).
pub(crate) const MENU_HELP_BOTH: u16 = 224;
pub(crate) const MENU_HELP_CODE_ONLY: u16 = 225;
pub(crate) const MENU_HELP_ASK_ONLY: u16 = 226;
pub(crate) const MENU_HELP_FILES: u16 = 227;

static REGISTER: std::sync::Once = std::sync::Once::new();
const TRAY_CLASS: PCWSTR = w!("QueqiaoPresence");

thread_local! {
    static TASKBAR_CREATED: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

struct Prompt {
    form: Form,
    countdown: HWND,
    deadline: Instant,
}

pub struct Presence {
    tray: HWND,
    tray_shown: bool,
    banner: Option<Form>,
    /// The help window (help ID and code), while help requests are open.
    help: Option<Form>,
    /// The help window's invitation text and its status line.
    help_invitation: String,
    help_status: Option<HWND>,
    prompt: Option<Prompt>,
    allow: bool,
    ask: bool,
    clipboard: bool,
    files: bool,
    terminal: bool,
    autostart: bool,
    lock_after: bool,
    /// While someone of another account controls the PC, the settings (and Quit) cannot be changed from here:
    /// they hold the mouse too.
    settings_locked: bool,
    /// Checks the settings password, when one is set: settings and Quit ask for it.
    password_check: Option<PasswordCheck>,
    viewer: Option<String>,
    account: AccountState,
    auto_update: bool,
    fixed_code: bool,
    /// The main window, made when it is first opened.
    main: Option<MainWindow>,
    /// What it shows, kept also while it is closed.
    user: String,
    devices: Vec<MainDevice>,
    transfers: Vec<TransferRow>,
    connections: Vec<ConnectionRow>,
    /// This PC's help ID and code while help is open.
    help_now: Option<(String, String)>,
    version: String,
    /// Queqiao's own icon, for the tray.
    icon: HICON,
    help_mode: HelpMode,
    help_files: bool,
}

impl Presence {
    /// The tray icon, with the current settings.
    pub fn new(
        allow: bool,
        ask: bool,
        clipboard: bool,
        files: bool,
        terminal: bool,
        autostart: bool,
        lock_after: bool,
    ) -> Result<Presence> {
        let instance = unsafe { GetModuleHandleW(None) }?;
        REGISTER.call_once(|| unsafe {
            let class = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(tray_proc),
                hInstance: instance.into(),
                lpszClassName: TRAY_CLASS,
                ..Default::default()
            };
            RegisterClassExW(&class);
            TASKBAR_CREATED.set(RegisterWindowMessageW(w!("TaskbarCreated")));
        });
        if TASKBAR_CREATED.get() == 0 {
            TASKBAR_CREATED.set(unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) });
        }
        // A hidden window that owns the icon and gets its clicks.
        let tray = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                TRAY_CLASS,
                w!("Queqiao"),
                WINDOW_STYLE::default(),
                0,
                0,
                0,
                0,
                None,
                None,
                Some(instance.into()),
                None,
            )
        }?;
        // Started by the service, the person's agent runs at High integrity: of what the person's programs (Medium)
        // send, only the tray icon's messages come through (Explorer's clicks and restart, a second copy's "show").
        for message in [TRAY_CALLBACK, TASKBAR_CREATED.get()] {
            let _ = unsafe {
                windows::Win32::UI::WindowsAndMessaging::ChangeWindowMessageFilterEx(
                    tray,
                    message,
                    windows::Win32::UI::WindowsAndMessaging::MSGFLT_ALLOW,
                    None,
                )
            };
        }
        let mut presence = Presence {
            tray,
            tray_shown: false,
            banner: None,
            prompt: None,
            help: None,
            help_invitation: String::new(),
            help_status: None,
            allow,
            ask,
            clipboard,
            files,
            terminal,
            autostart,
            lock_after,
            settings_locked: false,
            password_check: None,
            viewer: None,
            account: AccountState::Online,
            auto_update: true,
            fixed_code: false,
            main: None,
            user: String::new(),
            devices: Vec::new(),
            transfers: Vec::new(),
            connections: Vec::new(),
            help_now: None,
            version: queqiao_core::VERSION.to_string(),
            icon: crate::icon::app_icon(unsafe { GetSystemMetrics(SM_CXSMICON) }.max(16)),
            help_mode: HelpMode::default(),
            help_files: false,
        };
        presence.update_tray();
        Ok(presence)
    }

    /// True once the icon is in the notification area (false without a taskbar, e.g. Explorer not running;
    /// it is added when the taskbar comes back).
    pub fn tray_shown(&self) -> bool {
        self.tray_shown
    }

    pub fn set_settings(&mut self, allow: bool, ask: bool) {
        self.allow = allow;
        self.ask = ask;
    }

    /// The banner for a session with `viewer`.
    pub fn session_started(&mut self, viewer: &str) -> Result<()> {
        self.viewer = Some(viewer.to_string());
        self.banner = None;
        let form = Form::create(tr("Queqiao"), 440, 40, Kind::Banner)?;
        form.label(
            &format!(
                "\u{25CF}  {}",
                trf("{viewer} is controlling this PC", &[("viewer", viewer)])
            ),
            [14, 11, 320, 20],
        )?;
        form.button(tr("End"), ID_END, false, [346, 6, 84, 28])?;
        form.show();
        self.banner = Some(form);
        self.update_tray();
        Ok(())
    }

    pub fn session_ended(&mut self) {
        self.viewer = None;
        self.banner = None;
        self.update_tray();
    }

    /// Asks the person `question` (who wants to control the PC); denied after `seconds`.
    pub fn ask_consent(&mut self, question: &str, seconds: u32) -> Result<()> {
        self.prompt = None;
        let form = Form::create(
            tr("Queqiao - allow remote control?"),
            420,
            170,
            Kind::Prompt,
        )?;
        form.text(question, [20, 18, 380, 40])?;
        let countdown = form.label("", [20, 66, 380, 20])?;
        // Deny is the default button: Enter (or Esc, or closing the window) never lets anyone in.
        form.button(tr("Deny"), ID_DENY, true, [190, 122, 100, 30])?;
        form.button(tr("Allow"), ID_ALLOW, false, [300, 122, 100, 30])?;
        form.show();
        self.prompt = Some(Prompt {
            form,
            countdown,
            deadline: Instant::now() + Duration::from_secs(u64::from(seconds)),
        });
        self.update_countdown();
        Ok(())
    }

    /// Presses Allow or Deny on the prompt (for tests: the same path as a click).
    pub fn press_consent(&self, allow: bool) {
        if let Some(prompt) = &self.prompt {
            prompt.form.post(if allow { ID_ALLOW } else { ID_DENY });
        }
    }

    /// Presses End on the banner (for tests).
    pub fn press_end(&self) {
        if let Some(banner) = &self.banner {
            banner.post(ID_END);
        }
    }

    /// The banner's and the prompt's windows, for pictures and checks.
    /// Puts the session banner above other always-on-top windows (the privacy screen), so the person at
    /// this PC can always see the session and end it.
    pub fn raise_banner(&self) {
        if let Some(banner) = self.banner_window() {
            let _ = unsafe {
                SetWindowPos(
                    banner,
                    Some(HWND_TOPMOST),
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                )
            };
        }
    }

    /// Shows the session banner again if something hid or minimised it, above the other always-on-top windows (the
    /// security review of 2026-11-22: a program of the person could hide it; at High integrity their ordinary programs
    /// cannot reach it, elevated ones still could). True if it had to.
    pub fn keep_banner_shown(&self) -> bool {
        let Some(banner) = self.banner_window() else {
            return false;
        };
        let hidden = unsafe { !IsWindowVisible(banner).as_bool() || IsIconic(banner).as_bool() };
        if hidden {
            let _ = unsafe { ShowWindow(banner, SW_SHOWNOACTIVATE) };
            self.raise_banner();
        }
        hidden
    }

    /// Whether the session banner is above `other` in the window order (true without a banner).
    pub fn banner_above(&self, other: HWND) -> bool {
        let Some(banner) = self.banner_window() else {
            return true;
        };
        let mut current = banner;
        // Walk down from the banner; finding `other` means the banner is above it.
        for _ in 0..100_000 {
            match unsafe { GetWindow(current, GW_HWNDNEXT) } {
                Ok(next) if !next.is_invalid() => {
                    if next == other {
                        return true;
                    }
                    current = next;
                }
                _ => return false,
            }
        }
        false
    }

    pub fn banner_window(&self) -> Option<HWND> {
        self.banner.as_ref().map(|form| form.hwnd)
    }

    pub fn prompt_window(&self) -> Option<HWND> {
        self.prompt.as_ref().map(|prompt| prompt.form.hwnd)
    }

    /// Handles window messages for up to `wait_ms` and returns what the person did, if anything.
    /// Shows the help ID and code to give to whoever helps (replacing an earlier window).
    pub fn show_help(&mut self, help_id: &str, code: &str) -> Result<()> {
        self.help = None;
        self.help_now = Some((help_id.to_string(), code.to_string()));
        self.help_invitation = queqiao_core::invitation::text(help_id, code);
        // With the main window open, its Help page shows them (no second window).
        if let Some(main) = self.main.as_mut()
            && main.visible()
        {
            main.set_help(self.help_now.clone());
            main.set_page(Page::Help);
            return Ok(());
        }
        if let Some(main) = self.main.as_mut() {
            main.set_help(self.help_now.clone());
        }
        let form = Form::create(tr("Queqiao - let someone help you"), 420, 230, Kind::Dialog)?;
        form.text(
            tr("Give these to the person who helps you. They can ask once to control this PC; you decide in a prompt."),
            [20, 14, 380, 44],
        )?;
        // The ID in groups of three, the code in two halves: easier to read out.
        let spaced = queqiao_core::invitation::spaced_id(help_id);
        let halves = queqiao_core::invitation::halved_code(code);
        form.label(&format!("{}   {spaced}", tr("Help ID:")), [20, 74, 380, 24])?;
        form.label(&format!("{}   {halves}", tr("Code:")), [20, 106, 380, 24])?;
        let status = form.label("", [20, 138, 380, 24])?;
        form.button(
            tr("Copy invitation"),
            ID_COPY_INVITATION,
            false,
            [20, 176, 150, 30],
        )?;
        form.button(tr("New code"), ID_NEW_CODE, false, [190, 176, 100, 30])?;
        form.button(tr("Close"), ID_HELP_CLOSE, true, [300, 176, 100, 30])?;
        form.show();
        self.help = Some(form);
        self.help_invitation = queqiao_core::invitation::text(help_id, code);
        self.help_status = Some(status);
        Ok(())
    }

    pub fn hide_help(&mut self) {
        self.help = None;
        self.help_now = None;
        if let Some(main) = self.main.as_mut() {
            main.set_help(None);
        }
    }

    /// Opens the main window (made the first time), on `page` if given.
    pub fn show_main(&mut self, page: Option<Page>) -> Result<()> {
        if self.main.is_none() {
            let mut main = MainWindow::new(&self.version)?;
            main.set_account(&self.user, self.account);
            main.set_devices(self.devices.clone());
            main.set_transfers(self.transfers.clone());
            main.set_connections(self.connections.clone());
            main.set_help(self.help_now.clone());
            main.set_settings(self.shown());
            self.main = Some(main);
        }
        if let Some(main) = self.main.as_mut() {
            if let Some(page) = page {
                main.set_page(page);
            }
            main.show();
        }
        Ok(())
    }

    /// The main window's handle (tests).
    pub fn main_window(&self) -> Option<HWND> {
        self.main.as_ref().map(|main| main.hwnd)
    }

    /// Pictures of the main window's pages (tests): main_devices.bmp ... in `folder`.
    pub fn save_main_pictures(&mut self, folder: &std::path::Path) -> std::io::Result<()> {
        self.show_main(None).map_err(std::io::Error::other)?;
        match self.main.as_mut() {
            Some(main) => main.save_pages(folder),
            None => Ok(()),
        }
    }

    /// A notification from the tray icon (Windows shows it as a toast): something the person should know.
    pub fn notify(&mut self, text: &str) {
        let mut data = NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.tray,
            uID: TRAY_ID,
            uFlags: windows::Win32::UI::Shell::NIF_INFO,
            dwInfoFlags: windows::Win32::UI::Shell::NIIF_WARNING,
            ..Default::default()
        };
        for (slot, unit) in data
            .szInfoTitle
            .iter_mut()
            .zip(tr("Queqiao").encode_utf16().take(63))
        {
            *slot = unit;
        }
        for (slot, unit) in data.szInfo.iter_mut().zip(text.encode_utf16().take(255)) {
            *slot = unit;
        }
        let _ = unsafe { Shell_NotifyIconW(NIM_MODIFY, &data) };
    }

    /// The account's name, as the main window shows it.
    pub fn set_user(&mut self, user: &str) {
        self.user = user.to_string();
        if let Some(main) = self.main.as_mut() {
            main.set_account(&self.user, self.account);
        }
    }

    /// The account's devices, as the main window shows them.
    pub fn set_devices(&mut self, devices: Vec<MainDevice>) {
        if let Some(main) = self.main.as_mut() {
            main.set_devices(devices.clone());
        }
        self.devices = devices;
    }

    /// The files sent and received, newest first, as the main window shows them.
    pub fn set_transfers(&mut self, transfers: Vec<TransferRow>) {
        if let Some(main) = self.main.as_mut() {
            main.set_transfers(transfers.clone());
        }
        self.transfers = transfers;
    }

    /// The sessions this PC started, newest first: each device's page in the main window lists its own.
    pub fn set_connections(&mut self, connections: Vec<ConnectionRow>) {
        if let Some(main) = self.main.as_mut() {
            main.set_connections(connections.clone());
        }
        self.connections = connections;
    }

    /// The help mode and whether files go while someone helps (Settings).
    pub fn set_help_settings(&mut self, mode: HelpMode, files: bool) {
        self.help_mode = mode;
        self.help_files = files;
        self.refresh_main();
    }

    fn shown(&self) -> Shown {
        Shown {
            help_mode: self.help_mode,
            help_files: self.help_files,
            allow: self.allow,
            ask: self.ask,
            clipboard: self.clipboard,
            files: self.files,
            terminal: self.terminal,
            autostart: self.autostart,
            lock_after: self.lock_after,
            auto_update: self.auto_update,
            password: self.password_check.is_some(),
            fixed_code: self.fixed_code,
        }
    }

    /// The main window's settings and account line again, after a change.
    fn refresh_main(&mut self) {
        let shown = self.shown();
        if let Some(main) = self.main.as_mut() {
            main.set_settings(shown);
            main.set_account(&self.user, self.account);
        }
    }

    /// What a main window action does: an event for the agent, or nothing (handled here).
    fn main_event(&mut self, action: MainAction) -> Option<PresenceEvent> {
        match action {
            MainAction::Choose(item) => {
                let event = self.choose(item);
                self.refresh_main();
                event
            }
            MainAction::Connect(id) => Some(PresenceEvent::Connect(id)),
            MainAction::Files(id) => Some(PresenceEvent::SendFiles(id)),
            MainAction::Terminal(id) => Some(PresenceEvent::OpenCommandLine(id)),
            MainAction::Wake(id) => Some(PresenceEvent::Wake(id)),
            MainAction::More(id) => {
                let owner = self.main.as_ref()?.hwnd;
                let name = self
                    .devices
                    .iter()
                    .find(|device| device.id == id)
                    .map_or(id.clone(), |device| device.name.clone());
                let chosen = crate::forms::popup_menu(
                    owner,
                    &[(1, tr("Remove from this account..."), false)],
                )?;
                let question = trf(
                    "Take {name} out of your account? It can no longer control your devices or be controlled until it logs in again.",
                    &[("name", &name)],
                );
                (chosen == 1 && crate::forms::confirm(owner, tr("Queqiao"), &question))
                    .then_some(PresenceEvent::RemoveDevice(id))
            }
            MainAction::HelpSomeone { help_id, code } => {
                Some(PresenceEvent::HelpSomeone { help_id, code })
            }
            MainAction::CopyInvitation => {
                if let Some(main) = self.main.as_ref()
                    && crate::forms::copy_text(main.hwnd, &self.help_invitation)
                {
                    crate::forms::show_info(
                        tr("Queqiao"),
                        tr("Copied: send it to the person who helps you."),
                    );
                }
                None
            }
            MainAction::NewHelpId => Some(PresenceEvent::NewHelpId),
            MainAction::OpenReceived => Some(PresenceEvent::OpenReceivedFiles),
            MainAction::ClearTransfers => Some(PresenceEvent::ClearTransfers),
            MainAction::Account => match self.account {
                AccountState::LoggedOut => {
                    let event = self.choose(MENU_LOGIN);
                    self.refresh_main();
                    event
                }
                _ => {
                    let owner = self.main.as_ref()?.hwnd;
                    crate::forms::confirm(
                        owner,
                        tr("Queqiao"),
                        tr("Log this PC out of your account? Your devices can no longer control it until it logs in again."),
                    )
                    .then_some(PresenceEvent::LogOut)
                }
            },
            MainAction::Closed => None,
        }
    }

    pub fn help_window(&self) -> Option<HWND> {
        self.help.as_ref().map(|form| form.hwnd)
    }

    pub fn next(&mut self, wait_ms: u32) -> Option<PresenceEvent> {
        // The prompt pumps for all windows of this thread (with keyboard navigation for itself).
        let pumped = match (&self.prompt, &self.banner) {
            (Some(prompt), _) => prompt.form.next_command(wait_ms),
            (None, Some(banner)) => banner.next_command(wait_ms),
            (None, None) => {
                // With keyboard navigation (Tab, Enter) for the main window.
                crate::forms::pump_for(wait_ms, self.main.as_ref().map(|main| main.hwnd));
                None
            }
        };
        while let Some(action) = self.main.as_mut().and_then(MainWindow::take) {
            if let Some(event) = self.main_event(action) {
                return Some(event);
            }
        }
        // The help window's buttons (its closing too) are its own.
        if let Some(hwnd) = self.help.as_ref().map(|form| form.hwnd) {
            while let Some(command) = crate::forms::take_action(hwnd) {
                match command {
                    ID_COPY_INVITATION => {
                        if crate::forms::copy_text(hwnd, &self.help_invitation)
                            && let Some(status) = self.help_status
                        {
                            crate::forms::set_text(
                                status,
                                tr("Copied: send it to the person who helps you."),
                            );
                        }
                    }
                    ID_NEW_CODE => return Some(PresenceEvent::HelpNewCode),
                    ID_HELP_CLOSE | crate::forms::ID_CANCEL | crate::forms::CLOSED => {
                        self.help = None;
                        return Some(PresenceEvent::HelpClose);
                    }
                    _ => {}
                }
            }
        }
        let mut commands: Vec<u16> = pumped.into_iter().collect();
        for hwnd in [
            self.prompt.as_ref().map(|p| p.form.hwnd),
            self.banner.as_ref().map(|b| b.hwnd),
            Some(self.tray),
        ]
        .into_iter()
        .flatten()
        {
            while let Some(command) = crate::forms::take_action(hwnd) {
                commands.push(command);
            }
        }
        if self.prompt.is_some() {
            self.update_countdown();
            if self
                .prompt
                .as_ref()
                .is_some_and(|prompt| Instant::now() >= prompt.deadline)
            {
                self.prompt = None;
                return Some(PresenceEvent::Consent(false));
            }
        }
        for command in commands {
            if let Some(event) = self.event_for(command) {
                return Some(event);
            }
        }
        None
    }

    fn event_for(&mut self, command: u16) -> Option<PresenceEvent> {
        match command {
            ID_ALLOW | ID_DENY if self.prompt.is_some() => {
                self.prompt = None;
                Some(PresenceEvent::Consent(command == ID_ALLOW))
            }
            // Esc or closing the prompt denies.
            crate::forms::ID_CANCEL | crate::forms::CLOSED if self.prompt.is_some() => {
                self.prompt = None;
                Some(PresenceEvent::Consent(false))
            }
            ID_END | MENU_END => Some(PresenceEvent::EndSession),
            SHOW_MENU => self.show_menu(),
            SHOW_MAIN => match self.show_main(None) {
                Ok(()) => Some(PresenceEvent::MainShown),
                Err(_) => None,
            },
            TASKBAR_RESTARTED => {
                self.tray_shown = false;
                self.update_tray();
                None
            }
            _ => None,
        }
    }

    fn show_menu(&mut self) -> Option<PresenceEvent> {
        let menu = unsafe { CreatePopupMenu() }.ok()?;
        let locked = self.settings_locked;
        let checked = |on: bool| {
            let flags = if on {
                MF_STRING | MF_CHECKED
            } else {
                MF_STRING
            };
            if locked { flags | MF_GRAYED } else { flags }
        };
        let status = wide(&match (&self.viewer, self.account) {
            (Some(viewer), _) => trf(
                "Queqiao: {viewer} is controlling this PC",
                &[("viewer", viewer)],
            ),
            (None, AccountState::Online) => tr("Queqiao: ready").into(),
            (None, AccountState::Offline) => {
                tr("Queqiao: cannot reach the server, trying again").into()
            }
            (None, AccountState::LoggedOut) => tr("Queqiao: not logged in").into(),
        });
        let login = wide(tr("Log in..."));
        let end = wide(tr("End the session"));
        let allow = wide(tr("Let my account's devices control this PC"));
        let ask = wide(tr("Ask me before each session"));
        let clipboard = wide(tr("Share the clipboard"));
        let files = wide(tr("Receive files"));
        let terminal = wide(tr("Allow remote command lines"));
        let autostart = wide(tr("Start with Windows"));
        let lock_after = wide(tr("Lock this PC when a session ends"));
        let quit = wide(tr("Quit Queqiao on this PC"));
        let check_updates = wide(tr("Check for updates..."));
        let wol_check = wide(tr("Wake-on-LAN check..."));
        let auto_update = wide(tr("Install updates automatically"));
        let password = wide(if self.password_check.is_some() {
            tr("Remove the settings password...")
        } else {
            tr("Lock these settings with a password...")
        });
        let help = wide(tr("Let someone help me..."));
        let fixed_code = wide(if self.fixed_code {
            tr("Remove the fixed help code")
        } else {
            tr("A fixed help code for this PC...")
        });
        let open = wide(tr("Open Queqiao"));
        unsafe {
            let _ = AppendMenuW(
                menu,
                MF_STRING | MF_DEFAULT,
                usize::from(MENU_OPEN),
                PCWSTR(open.as_ptr()),
            );
            let _ = AppendMenuW(menu, MF_STRING | MF_GRAYED, 0, PCWSTR(status.as_ptr()));
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
            if self.viewer.is_some() {
                let _ = AppendMenuW(menu, MF_STRING, usize::from(MENU_END), PCWSTR(end.as_ptr()));
            }
            if self.account != AccountState::Online {
                let _ = AppendMenuW(
                    menu,
                    checked(false),
                    usize::from(MENU_LOGIN),
                    PCWSTR(login.as_ptr()),
                );
            }
            let _ = AppendMenuW(
                menu,
                MF_STRING,
                usize::from(MENU_HELP),
                PCWSTR(help.as_ptr()),
            );
            let _ = AppendMenuW(
                menu,
                checked(false),
                usize::from(MENU_FIXED_CODE),
                PCWSTR(fixed_code.as_ptr()),
            );
            let _ = AppendMenuW(
                menu,
                checked(self.allow),
                usize::from(MENU_ALLOW),
                PCWSTR(allow.as_ptr()),
            );
            let _ = AppendMenuW(
                menu,
                checked(self.ask),
                usize::from(MENU_ASK),
                PCWSTR(ask.as_ptr()),
            );
            let _ = AppendMenuW(
                menu,
                checked(self.clipboard),
                usize::from(MENU_CLIPBOARD),
                PCWSTR(clipboard.as_ptr()),
            );
            let _ = AppendMenuW(
                menu,
                checked(self.files),
                usize::from(MENU_FILES),
                PCWSTR(files.as_ptr()),
            );
            let _ = AppendMenuW(
                menu,
                checked(self.terminal),
                usize::from(MENU_TERMINAL),
                PCWSTR(terminal.as_ptr()),
            );
            let _ = AppendMenuW(
                menu,
                checked(self.autostart),
                usize::from(MENU_AUTOSTART),
                PCWSTR(autostart.as_ptr()),
            );
            let _ = AppendMenuW(
                menu,
                checked(self.lock_after),
                usize::from(MENU_LOCK_AFTER),
                PCWSTR(lock_after.as_ptr()),
            );
            let _ = AppendMenuW(
                menu,
                checked(false),
                usize::from(MENU_PASSWORD),
                PCWSTR(password.as_ptr()),
            );
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
            let _ = AppendMenuW(
                menu,
                checked(self.auto_update),
                usize::from(MENU_AUTO_UPDATE),
                PCWSTR(auto_update.as_ptr()),
            );
            let _ = AppendMenuW(
                menu,
                MF_STRING,
                usize::from(MENU_UPDATE_CHECK),
                PCWSTR(check_updates.as_ptr()),
            );
            let _ = AppendMenuW(
                menu,
                MF_STRING,
                usize::from(MENU_WOL_CHECK),
                PCWSTR(wol_check.as_ptr()),
            );
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
            let _ = AppendMenuW(
                menu,
                MF_STRING,
                usize::from(MENU_QUIT),
                PCWSTR(quit.as_ptr()),
            );
            let mut at = POINT::default();
            let _ = GetCursorPos(&mut at);
            // The menu closes when clicking elsewhere only if its owner is in front.
            let _ = SetForegroundWindow(self.tray);
            let chosen = TrackPopupMenu(
                menu,
                TPM_RIGHTBUTTON | TPM_RETURNCMD,
                at.x,
                at.y,
                None,
                self.tray,
                None,
            )
            .0;
            let _ = PostMessageW(Some(self.tray), WM_NULL, WPARAM(0), LPARAM(0));
            let _ = DestroyMenu(menu);
            self.choose(u16::try_from(chosen).unwrap_or(0))
        }
    }

    /// The settings password's check (None: no password).
    pub fn set_password_check(&mut self, check: Option<PasswordCheck>) {
        self.password_check = check;
        self.refresh_main();
    }

    /// Whether a fixed help code is set (the menu offers to remove it).
    pub fn set_fixed_code(&mut self, set: bool) {
        self.fixed_code = set;
        self.refresh_main();
    }

    /// Whether updates are installed by themselves (the menu's check mark).
    pub fn set_auto_update(&mut self, on: bool) {
        self.auto_update = on;
        self.refresh_main();
    }

    /// How this PC stands with the account server (shown in the menu; "Log in..." while not online).
    pub fn set_account_state(&mut self, state: AccountState) {
        self.account = state;
        self.refresh_main();
    }

    /// Locks the settings (true while someone of another account controls the PC) or unlocks them.
    pub fn set_settings_locked(&mut self, locked: bool) {
        self.settings_locked = locked;
    }

    /// What choosing tray menu item `item` does (also the path tests take).
    pub fn choose(&mut self, item: u16) -> Option<PresenceEvent> {
        let setting = matches!(
            item,
            MENU_ALLOW
                | MENU_ASK
                | MENU_CLIPBOARD
                | MENU_FILES
                | MENU_TERMINAL
                | MENU_AUTOSTART
                | MENU_LOCK_AFTER
                | MENU_AUTO_UPDATE
                | MENU_QUIT
                | MENU_HELP_BOTH
                | MENU_HELP_CODE_ONLY
                | MENU_HELP_ASK_ONLY
                | MENU_HELP_FILES
        );
        let protected =
            setting || item == MENU_PASSWORD || item == MENU_LOGIN || item == MENU_FIXED_CODE;
        if item == MENU_OPEN {
            return self.show_main(None).ok().map(|()| PresenceEvent::MainShown);
        }
        if self.settings_locked && protected {
            return Some(PresenceEvent::SettingsLocked);
        }
        if protected && let Some(check) = &self.password_check {
            let typed = crate::forms::ask_secret(tr("Queqiao"), tr("The settings password:"));
            if !typed.is_some_and(|typed| check(&typed)) {
                return Some(PresenceEvent::SettingsLocked);
            }
        }
        if item == MENU_FIXED_CODE {
            if self.fixed_code {
                return Some(PresenceEvent::SetFixedHelpCode(None));
            }
            let typed = crate::forms::ask_text(
                tr("Queqiao"),
                tr("A fixed help code (8 or more letters or digits):"),
                "",
            )?;
            let code = crate::forms::normalize_code(&typed);
            if code.len() < MIN_FIXED_CODE {
                crate::forms::show_error(
                    tr("Queqiao"),
                    tr("A fixed help code needs 8 or more letters or digits."),
                );
                return None;
            }
            return Some(PresenceEvent::SetFixedHelpCode(Some(code)));
        }
        if item == MENU_PASSWORD {
            if self.password_check.is_some() {
                return Some(PresenceEvent::SetPassword(None));
            }
            let first =
                crate::forms::ask_secret(tr("Queqiao"), tr("A password for these settings:"))?;
            let again = crate::forms::ask_secret(tr("Queqiao"), tr("The same password again:"))?;
            return (!first.is_empty() && first == again)
                .then_some(PresenceEvent::SetPassword(Some(first)));
        }
        {
            match item {
                MENU_END => Some(PresenceEvent::EndSession),
                MENU_LOGIN => Some(PresenceEvent::LogIn),
                MENU_HELP => Some(PresenceEvent::OpenHelp),
                MENU_ALLOW => {
                    self.allow = !self.allow;
                    Some(PresenceEvent::SetAllow(self.allow))
                }
                MENU_ASK => {
                    self.ask = !self.ask;
                    Some(PresenceEvent::SetAsk(self.ask))
                }
                MENU_CLIPBOARD => {
                    self.clipboard = !self.clipboard;
                    Some(PresenceEvent::SetClipboard(self.clipboard))
                }
                MENU_FILES => {
                    self.files = !self.files;
                    Some(PresenceEvent::SetFiles(self.files))
                }
                MENU_TERMINAL => {
                    self.terminal = !self.terminal;
                    Some(PresenceEvent::SetTerminal(self.terminal))
                }
                MENU_AUTOSTART => {
                    self.autostart = !self.autostart;
                    Some(PresenceEvent::SetAutostart(self.autostart))
                }
                MENU_LOCK_AFTER => {
                    self.lock_after = !self.lock_after;
                    Some(PresenceEvent::SetLockAfter(self.lock_after))
                }
                MENU_AUTO_UPDATE => {
                    self.auto_update = !self.auto_update;
                    Some(PresenceEvent::SetAutoUpdate(self.auto_update))
                }
                MENU_UPDATE_CHECK => Some(PresenceEvent::CheckUpdates),
                MENU_WOL_CHECK => Some(PresenceEvent::CheckWakeOnLan),
                MENU_QUIT => Some(PresenceEvent::Quit),
                MENU_HELP_BOTH | MENU_HELP_CODE_ONLY | MENU_HELP_ASK_ONLY => {
                    self.help_mode = match item {
                        MENU_HELP_CODE_ONLY => HelpMode::CodeOnly,
                        MENU_HELP_ASK_ONLY => HelpMode::AskOnly,
                        _ => HelpMode::CodeAndAsk,
                    };
                    Some(PresenceEvent::SetHelpMode(self.help_mode))
                }
                MENU_HELP_FILES => {
                    self.help_files = !self.help_files;
                    Some(PresenceEvent::SetHelpFiles(self.help_files))
                }
                _ => None,
            }
        }
    }

    fn update_countdown(&self) {
        if let Some(prompt) = &self.prompt {
            let left = prompt
                .deadline
                .saturating_duration_since(Instant::now())
                .as_secs()
                + 1;
            set_text(
                prompt.countdown,
                &trf(
                    "Denied automatically in {seconds} s unless you allow it.",
                    &[("seconds", &left.to_string())],
                ),
            );
        }
    }

    /// Adds the icon, or updates its tooltip.
    fn update_tray(&mut self) {
        let tip = match &self.viewer {
            Some(viewer) => trf(
                "Queqiao: {viewer} is controlling this PC",
                &[("viewer", viewer)],
            ),
            None => tr("Queqiao: ready for your devices").into(),
        };
        let mut data = NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.tray,
            uID: TRAY_ID,
            uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP,
            uCallbackMessage: TRAY_CALLBACK,
            hIcon: self.icon,
            ..Default::default()
        };
        for (slot, unit) in data.szTip.iter_mut().zip(tip.encode_utf16().take(127)) {
            *slot = unit;
        }
        let action = if self.tray_shown { NIM_MODIFY } else { NIM_ADD };
        self.tray_shown = unsafe { Shell_NotifyIconW(action, &data) }.as_bool() || self.tray_shown;
    }
}

impl Drop for Presence {
    fn drop(&mut self) {
        let data = NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.tray,
            uID: TRAY_ID,
            ..Default::default()
        };
        unsafe {
            let _ = Shell_NotifyIconW(NIM_DELETE, &data);
            let _ = DestroyWindow(self.tray);
            let _ = DestroyIcon(self.icon);
        }
    }
}

extern "system" fn tray_proc(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let mouse = (lparam.0 & 0xFFFF) as u32;
    // A left click opens the main window, a right click the menu.
    if message == TRAY_CALLBACK && mouse == WM_LBUTTONUP {
        crate::forms::push_action(hwnd, SHOW_MAIN);
        return LRESULT(0);
    }
    if message == TRAY_CALLBACK && mouse == WM_RBUTTONUP {
        crate::forms::push_action(hwnd, SHOW_MENU);
        return LRESULT(0);
    }
    if message != 0 && message == TASKBAR_CREATED.get() {
        // Explorer restarted: the icon must be added again (the next update does it).
        crate::forms::push_action(hwnd, TASKBAR_RESTARTED);
        return LRESULT(0);
    }
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}

/// Asks the Queqiao already running in this session to show its main window (Queqiao opened again from the Start
/// menu); false when none runs here.
pub fn show_running_main() -> bool {
    let Ok(tray) = (unsafe { FindWindowW(TRAY_CLASS, PCWSTR::null()) }) else {
        return false;
    };
    let mut process = 0u32;
    unsafe {
        GetWindowThreadProcessId(tray, Some(&mut process));
        // It may come to the front: this program was just started by the person.
        let _ = AllowSetForegroundWindow(process);
        PostMessageW(
            Some(tray),
            TRAY_CALLBACK,
            WPARAM(0),
            LPARAM(WM_LBUTTONUP as isize),
        )
        .is_ok()
    }
}

/// Explorer restarted (not a control id).
const TASKBAR_RESTARTED: u16 = 0xFFFD;

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

/// The session banner on its own, for the service's desktop helper: on the sign-in, lock and UAC screens there is
/// no tray, and the person's own banner is on their desktop, out of sight. The same look, with End. Made and
/// pumped on one thread, which must be on the desktop it is to appear on.
pub struct SessionBanner {
    form: Form,
}

impl SessionBanner {
    pub fn show(viewer: &str) -> Result<SessionBanner> {
        let form = Form::create(tr("Queqiao"), 440, 40, Kind::Banner)?;
        form.label(
            &format!(
                "\u{25CF}  {}",
                trf("{viewer} is controlling this PC", &[("viewer", viewer)])
            ),
            [14, 11, 320, 20],
        )?;
        form.button(tr("End"), ID_END, false, [346, 6, 84, 28])?;
        form.show();
        Ok(SessionBanner { form })
    }

    /// Handles window messages for up to `wait_ms`; true once End was pressed.
    pub fn end_pressed(&self, wait_ms: u32) -> bool {
        matches!(self.form.next_command(wait_ms), Some(ID_END))
    }

    /// Presses End (for tests: the same path as a click).
    pub fn press_end(&self) {
        self.form.post(ID_END);
    }
}
