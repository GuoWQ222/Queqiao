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
const MENU_ALLOW: u16 = 201;
const MENU_ASK: u16 = 202;
const MENU_QUIT: u16 = 203;
const MENU_CLIPBOARD: u16 = 204;
const MENU_FILES: u16 = 205;
const MENU_TERMINAL: u16 = 206;
const MENU_AUTOSTART: u16 = 207;
const ID_END: u16 = 210;
const ID_NEW_CODE: u16 = 213;
const ID_HELP_CLOSE: u16 = 214;
const MENU_HELP: u16 = 208;
const MENU_LOCK_AFTER: u16 = 209;
const MENU_LOGIN: u16 = 215;
const MENU_PASSWORD: u16 = 220;
/// Menu items tests can choose (the same path as a click).
pub const TEST_MENU_TERMINAL: u16 = MENU_TERMINAL;
pub const TEST_MENU_LOGIN: u16 = MENU_LOGIN;
const ID_ALLOW: u16 = 211;
const ID_DENY: u16 = 212;
/// The tray window asks for its menu (not a control id).
const SHOW_MENU: u16 = 0xFFFE;

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
        let mut presence = Presence {
            tray,
            tray_shown: false,
            banner: None,
            prompt: None,
            help: None,
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
        let form = Form::create(tr("Queqiao - let someone help you"), 420, 230, Kind::Dialog)?;
        form.text(
            tr("Give these to the person who helps you. They can ask once to control this PC; you decide in a prompt."),
            [20, 14, 380, 44],
        )?;
        // The ID in groups of three, the code in two halves: easier to read out.
        let spaced: String = help_id
            .chars()
            .enumerate()
            .flat_map(|(at, c)| {
                (at > 0 && at % 3 == 0)
                    .then_some(' ')
                    .into_iter()
                    .chain(Some(c))
            })
            .collect();
        let halves = if code.len() == 8 {
            format!("{}-{}", &code[..4], &code[4..])
        } else {
            code.to_string()
        };
        form.label(&format!("{}   {spaced}", tr("Help ID:")), [20, 74, 380, 24])?;
        form.label(&format!("{}   {halves}", tr("Code:")), [20, 106, 380, 24])?;
        form.button(tr("New code"), ID_NEW_CODE, false, [190, 176, 100, 30])?;
        form.button(tr("Close"), ID_HELP_CLOSE, true, [300, 176, 100, 30])?;
        form.show();
        self.help = Some(form);
        Ok(())
    }

    pub fn hide_help(&mut self) {
        self.help = None;
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
                crate::forms::pump(wait_ms);
                None
            }
        };
        // The help window's buttons (its closing too) are its own.
        if let Some(hwnd) = self.help.as_ref().map(|form| form.hwnd) {
            while let Some(command) = crate::forms::take_action(hwnd) {
                match command {
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
        let password = wide(if self.password_check.is_some() {
            tr("Remove the settings password...")
        } else {
            tr("Lock these settings with a password...")
        });
        let help = wide(tr("Let someone help me..."));
        unsafe {
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
    }

    /// How this PC stands with the account server (shown in the menu; "Log in..." while not online).
    pub fn set_account_state(&mut self, state: AccountState) {
        self.account = state;
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
                | MENU_QUIT
        );
        let protected = setting || item == MENU_PASSWORD || item == MENU_LOGIN;
        if self.settings_locked && protected {
            return Some(PresenceEvent::SettingsLocked);
        }
        if protected && let Some(check) = &self.password_check {
            let typed = crate::forms::ask_secret(tr("Queqiao"), tr("The settings password:"));
            if !typed.is_some_and(|typed| check(&typed)) {
                return Some(PresenceEvent::SettingsLocked);
            }
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
                MENU_QUIT => Some(PresenceEvent::Quit),
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
            hIcon: unsafe { LoadIconW(None, IDI_SHIELD) }.unwrap_or_default(),
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
        }
    }
}

extern "system" fn tray_proc(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let mouse = (lparam.0 & 0xFFFF) as u32;
    if message == TRAY_CALLBACK && (mouse == WM_RBUTTONUP || mouse == WM_LBUTTONUP) {
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
