//! Queqiao's main window, as UU Remote's: a sidebar with the pages (Devices, Help, Transfers, Settings) and the
//! account, the page beside it. It lives on the agent's UI thread with the tray icon (presence.rs) and is polled
//! from there; its settings go through the same `Presence::choose` as the tray menu (so the settings password and
//! the lock while someone helps apply to both). Painted with GDI (sidebar, headings, cards, the owner-drawn lists),
//! with standard controls for what is typed or ticked.

use std::cell::RefCell;
use std::ffi::c_void;
use std::path::Path;
use std::sync::Once;

use queqiao_core::text::{tr, trf};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, CreateCompatibleBitmap,
    CreateCompatibleDC, CreateFontW, CreatePen, CreateSolidBrush, DEFAULT_CHARSET, DT_CENTER,
    DT_END_ELLIPSIS, DT_LEFT, DT_NOPREFIX, DT_SINGLELINE, DT_VCENTER, DT_WORDBREAK, DeleteDC,
    DeleteObject, DrawTextW, Ellipse, EndPaint, FF_DONTCARE, FillRect, GetStockObject, HBRUSH, HDC,
    HFONT, InvalidateRect, NULL_PEN, OUT_DEFAULT_PRECIS, PAINTSTRUCT, PS_SOLID, RoundRect, SRCCOPY,
    SelectObject, SetBkMode, SetTextColor, TRANSPARENT, UpdateWindow,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::{DRAWITEMSTRUCT, MEASUREITEMSTRUCT};
use windows::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForWindow};
use windows::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, SetFocus};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, Result, w};

use crate::presence::AccountState;

const CLASS: PCWSTR = w!("QueqiaoMain");
static REGISTER: Once = Once::new();

// Layout, in 96-DPI units.
const WIDTH: i32 = 880;
const HEIGHT: i32 = 580;
const SIDEBAR: i32 = 208;
const LEFT: i32 = SIDEBAR + 32;
const CONTENT: i32 = WIDTH - LEFT - 32;
const NAV_TOP: i32 = 84;
const NAV_STEP: i32 = 44;
const NAV_HEIGHT: i32 = 40;
const DEVICE_ROW: i32 = 60;
const TRANSFER_ROW: i32 = 52;

// Colours (0x00BBGGRR), as Windows 11's own settings pages.
const SIDEBAR_COLOR: u32 = 0x00F3_F3F3;
const PAGE_COLOR: u32 = 0x00FF_FFFF;
const TEXT_COLOR: u32 = 0x001B_1B1B;
const SECONDARY: u32 = 0x0061_6161;
const ACCENT: u32 = 0x00C0_6700;
const NAV_SELECTED: u32 = 0x00E5_E5E5;
const ROW_SELECTED: u32 = 0x00FB_F1E8;
const ONLINE: u32 = 0x000E_A113;
const OFFLINE: u32 = 0x009E_9E9E;
const DIVIDER: u32 = 0x00EB_EBEB;
const CARD: u32 = 0x00F7_F7F7;

// Glyphs of Segoe MDL2 Assets (Windows 10 and later).
const GLYPH_DEVICES: &str = "\u{E7F4}";
const GLYPH_HELP: &str = "\u{E716}";
const GLYPH_TRANSFERS: &str = "\u{E896}";
const GLYPH_SETTINGS: &str = "\u{E713}";
const GLYPH_RECEIVED: &str = "\u{E896}";
const GLYPH_SENT: &str = "\u{E898}";

// Control styles (winuser.h) as plain numbers, as in forms.rs.
const ES_AUTOHSCROLL: u32 = 0x80;
const LBS_NOTIFY: u32 = 0x1;
const LBS_OWNERDRAWFIXED: u32 = 0x10;
const LBS_NOINTEGRALHEIGHT: u32 = 0x100;
const LBN_DBLCLK: u16 = 2;
const EN_CHANGE: u16 = 0x0300;
const BN_CLICKED: u16 = 0;
const BS_DEFPUSHBUTTON: u32 = 0x1;
const BS_CHECKBOX: u32 = 0x2;
const BS_RADIOBUTTON: u32 = 0x4;
const BM_SETCHECK: u32 = 0x00F1;
const SS_NOPREFIX: u32 = 0x80;
const ODS_SELECTED: u32 = 0x1;

// Command ids of the main window's own controls. The settings use the tray menu's ids (presence.rs), so that
// `Presence::choose` handles them as menu items.
pub(crate) const ID_NAV: u16 = 300; // .. 303
const ID_ACCOUNT: u16 = 310;
const ID_DEVICES: u16 = 311;
const ID_CONNECT: u16 = 312;
const ID_WAKE: u16 = 313;
const ID_MORE: u16 = 314;
const ID_HELP_ID: u16 = 320;
const ID_HELP_CODE: u16 = 321;
const ID_HELP_GO: u16 = 322;
const ID_HELP_COPY: u16 = 323;
const ID_TRANSFERS: u16 = 330;
const ID_OPEN_RECEIVED: u16 = 331;
const ID_CLEAR_TRANSFERS: u16 = 332;
const ID_CLOSE: u16 = 0xFFFF;

/// A page of the main window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Page {
    Devices,
    Help,
    Transfers,
    Settings,
}

impl Page {
    const ALL: [Page; 4] = [Page::Devices, Page::Help, Page::Transfers, Page::Settings];

    fn title(self) -> &'static str {
        match self {
            Page::Devices => tr("Devices"),
            Page::Help => tr("Help"),
            Page::Transfers => tr("Transfers"),
            Page::Settings => tr("Settings"),
        }
    }

    fn glyph(self) -> &'static str {
        match self {
            Page::Devices => GLYPH_DEVICES,
            Page::Help => GLYPH_HELP,
            Page::Transfers => GLYPH_TRANSFERS,
            Page::Settings => GLYPH_SETTINGS,
        }
    }

    fn subtitle(self) -> &'static str {
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

/// One device in the Devices page.
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

/// The settings as the Settings page shows them (the tray menu's own copy is in Presence).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Shown {
    pub help_mode: crate::presence::HelpMode,
    pub help_files: bool,
    pub allow: bool,
    pub ask: bool,
    pub clipboard: bool,
    pub files: bool,
    pub terminal: bool,
    pub autostart: bool,
    pub lock_after: bool,
    pub auto_update: bool,
    pub password: bool,
    pub fixed_code: bool,
}

/// What the person did in the main window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum MainAction {
    /// A setting or a menu command (a tray menu id: `Presence::choose`).
    Choose(u16),
    Connect(String),
    Wake(String),
    /// More... for that device: a menu (`more_menu`).
    More(String),
    HelpSomeone {
        help_id: String,
        code: String,
    },
    CopyInvitation,
    OpenReceived,
    ClearTransfers,
    /// The account line: log in (or out).
    Account,
    /// Closed: it goes to the tray, Queqiao keeps running.
    Closed,
}

/// What the window draws, kept for its window procedure (one main window per thread).
struct State {
    scale: f64,
    page: Page,
    user: String,
    account: AccountState,
    devices: Vec<MainDevice>,
    transfers: Vec<TransferRow>,
    help: Option<(String, String)>,
    fonts: Fonts,
    version: String,
}

struct Fonts {
    app: HFONT,
    title: HFONT,
    section: HFONT,
    body: HFONT,
    body_bold: HFONT,
    small: HFONT,
    big: HFONT,
    glyph: HFONT,
    glyph_large: HFONT,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

/// The controls of one page, shown only on it.
struct Placed {
    page: Page,
    hwnd: HWND,
}

pub(crate) struct MainWindow {
    pub(crate) hwnd: HWND,
    controls: Vec<Placed>,
    devices: HWND,
    no_devices: HWND,
    transfers: HWND,
    no_transfers: HWND,
    help_id: HWND,
    help_code: HWND,
    help_buttons: Vec<HWND>,
    help_open_button: HWND,
    fixed_button: HWND,
    password_button: HWND,
    checks: Vec<(u16, HWND)>,
    connect: HWND,
    wake: HWND,
    more: HWND,
}

impl MainWindow {
    /// The window, hidden (`show` brings it up).
    pub(crate) fn new(version: &str) -> Result<MainWindow> {
        let instance = unsafe { GetModuleHandleW(None) }?;
        REGISTER.call_once(|| unsafe {
            let class = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(main_proc),
                hInstance: instance.into(),
                hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                hIcon: crate::icon::app_icon(32),
                hIconSm: crate::icon::app_icon(16),
                lpszClassName: CLASS,
                ..Default::default()
            };
            RegisterClassExW(&class);
        });
        let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX;
        let ex_style = WS_EX_CONTROLPARENT | WS_EX_APPWINDOW;
        let title: Vec<u16> = tr("Queqiao").encode_utf16().chain(Some(0)).collect();
        let hwnd = unsafe {
            CreateWindowExW(
                ex_style,
                CLASS,
                PCWSTR(title.as_ptr()),
                style,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                WIDTH,
                HEIGHT,
                None,
                None,
                Some(instance.into()),
                None,
            )
        }?;
        let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96);
        let scale = f64::from(dpi) / 96.0;
        let mut frame = RECT {
            left: 0,
            top: 0,
            right: px(WIDTH, scale),
            bottom: px(HEIGHT, scale),
        };
        unsafe { AdjustWindowRectExForDpi(&mut frame, style, false, ex_style, dpi) }?;
        let (outer_w, outer_h) = (frame.right - frame.left, frame.bottom - frame.top);
        unsafe {
            SetWindowPos(
                hwnd,
                None,
                0,
                0,
                outer_w,
                outer_h,
                SWP_NOMOVE | SWP_NOZORDER,
            )
        }?;
        let fonts = Fonts {
            app: font(19, 600, w!("Segoe UI"), scale),
            title: font(26, 600, w!("Segoe UI"), scale),
            section: font(16, 600, w!("Segoe UI"), scale),
            body: font(14, 400, w!("Segoe UI"), scale),
            body_bold: font(14, 600, w!("Segoe UI"), scale),
            small: font(12, 400, w!("Segoe UI"), scale),
            big: font(30, 600, w!("Segoe UI"), scale),
            glyph: font(16, 400, w!("Segoe MDL2 Assets"), scale),
            glyph_large: font(20, 400, w!("Segoe MDL2 Assets"), scale),
        };
        let body = fonts.body;
        STATE.with_borrow_mut(|state| {
            *state = Some(State {
                scale,
                page: Page::Devices,
                user: String::new(),
                account: AccountState::LoggedOut,
                devices: Vec::new(),
                transfers: Vec::new(),
                help: None,
                fonts,
                version: version.to_string(),
            })
        });
        let make = |class: PCWSTR,
                    text: &str,
                    style: u32,
                    ex: WINDOW_EX_STYLE,
                    id: u16,
                    place: [i32; 4]| {
            let text: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
            let [x, y, width, height] = place.map(|v| px(v, scale));
            let control = unsafe {
                CreateWindowExW(
                    ex,
                    class,
                    PCWSTR(text.as_ptr()),
                    WINDOW_STYLE(WS_CHILD.0 | style),
                    x,
                    y,
                    width,
                    height,
                    Some(hwnd),
                    Some(HMENU(usize::from(id) as *mut c_void)),
                    Some(instance.into()),
                    None,
                )
            }?;
            unsafe {
                SendMessageW(
                    control,
                    WM_SETFONT,
                    Some(WPARAM(body.0 as usize)),
                    Some(LPARAM(1)),
                );
            }
            Ok::<HWND, windows::core::Error>(control)
        };
        let tab = WS_TABSTOP.0;
        let button = |text: &str, id: u16, place: [i32; 4], default: bool| {
            let style = tab | if default { BS_DEFPUSHBUTTON } else { 0 };
            make(w!("BUTTON"), text, style, WINDOW_EX_STYLE(0), id, place)
        };
        let mut controls = Vec::new();
        let mut place = |page: Page, hwnd: HWND| {
            controls.push(Placed { page, hwnd });
            hwnd
        };

        // Devices.
        let list_style =
            tab | LBS_NOTIFY | LBS_OWNERDRAWFIXED | LBS_NOINTEGRALHEIGHT | WS_VSCROLL.0;
        let devices = place(
            Page::Devices,
            make(
                w!("LISTBOX"),
                "",
                list_style,
                WINDOW_EX_STYLE(0),
                ID_DEVICES,
                [LEFT, 100, CONTENT, 372],
            )?,
        );
        let no_devices = place(
            Page::Devices,
            make(
                w!("STATIC"),
                tr("Log in to see your devices."),
                SS_NOPREFIX,
                WINDOW_EX_STYLE(0),
                0,
                [LEFT, 110, CONTENT, 60],
            )?,
        );
        let connect = place(
            Page::Devices,
            button(tr("Connect"), ID_CONNECT, [LEFT, 492, 120, 34], true)?,
        );
        let wake = place(
            Page::Devices,
            button(tr("Wake up"), ID_WAKE, [LEFT + 132, 492, 120, 34], false)?,
        );
        let more = place(
            Page::Devices,
            button(tr("More..."), ID_MORE, [LEFT + 264, 492, 120, 34], false)?,
        );

        // Help: this PC's card (its buttons inside), then helping someone.
        let help_open_button = place(
            Page::Help,
            button(
                tr("Let someone help me..."),
                crate::presence::MENU_HELP,
                [LEFT + 20, 212, 200, 34],
                false,
            )?,
        );
        let mut help_buttons = Vec::new();
        for (index, (text, id)) in [
            (tr("Copy invitation"), ID_HELP_COPY),
            (tr("New code"), crate::presence::ID_NEW_CODE),
            (tr("Stop"), crate::presence::ID_HELP_CLOSE),
        ]
        .into_iter()
        .enumerate()
        {
            let x = LEFT + 20 + index as i32 * 148;
            help_buttons.push(place(
                Page::Help,
                button(text, id, [x, 212, 136, 34], false)?,
            ));
        }
        let fixed_button = place(
            Page::Help,
            button(
                tr("Fixed help code..."),
                crate::presence::MENU_FIXED_CODE,
                [LEFT + CONTENT - 200, 212, 180, 34],
                false,
            )?,
        );
        let edit_style = tab | ES_AUTOHSCROLL;
        let help_id = place(
            Page::Help,
            make(
                w!("EDIT"),
                "",
                edit_style,
                WS_EX_CLIENTEDGE,
                ID_HELP_ID,
                [LEFT, 356, 220, 30],
            )?,
        );
        let help_code = place(
            Page::Help,
            make(
                w!("EDIT"),
                "",
                edit_style,
                WS_EX_CLIENTEDGE,
                ID_HELP_CODE,
                [LEFT + 236, 356, 220, 30],
            )?,
        );
        place(
            Page::Help,
            button(tr("Connect"), ID_HELP_GO, [LEFT + 472, 354, 136, 34], false)?,
        );

        // Transfers.
        let transfers = place(
            Page::Transfers,
            make(
                w!("LISTBOX"),
                "",
                list_style,
                WINDOW_EX_STYLE(0),
                ID_TRANSFERS,
                [LEFT, 100, CONTENT, 372],
            )?,
        );
        let no_transfers = place(
            Page::Transfers,
            make(
                w!("STATIC"),
                tr(
                    "Nothing yet: files sent in a session (drag them onto its window) and files received appear here.",
                ),
                SS_NOPREFIX,
                WINDOW_EX_STYLE(0),
                0,
                [LEFT, 110, CONTENT, 60],
            )?,
        );
        place(
            Page::Transfers,
            button(
                tr("Open received files"),
                ID_OPEN_RECEIVED,
                [LEFT, 492, 180, 34],
                false,
            )?,
        );
        place(
            Page::Transfers,
            button(
                tr("Clear the list"),
                ID_CLEAR_TRANSFERS,
                [LEFT + 192, 492, 140, 34],
                false,
            )?,
        );

        // Settings: the tray menu's items, in two groups.
        let mut checks = Vec::new();
        let this_pc = [
            (
                crate::presence::MENU_ALLOW,
                tr("This account's devices may control this PC"),
            ),
            (crate::presence::MENU_ASK, tr("Ask before each session")),
            (crate::presence::MENU_CLIPBOARD, tr("Share the clipboard")),
            (crate::presence::MENU_FILES, tr("Receive files")),
            (crate::presence::MENU_TERMINAL, tr("Allow command lines")),
            (
                crate::presence::MENU_LOCK_AFTER,
                tr("Lock this PC after a session"),
            ),
        ];
        for (index, (id, text)) in this_pc.into_iter().enumerate() {
            let y = 130 + index as i32 * 30;
            let hwnd = make(
                w!("BUTTON"),
                text,
                tab | BS_CHECKBOX,
                WINDOW_EX_STYLE(0),
                id,
                [LEFT + 4, y, 300, 26],
            )?;
            checks.push((id, place(Page::Settings, hwnd)));
        }
        // Remote help: how someone of another account may start (UU's three ways), and files while they help.
        let ways = [
            (
                crate::presence::MENU_HELP_BOTH,
                tr("The code, then I allow it"),
            ),
            (crate::presence::MENU_HELP_CODE_ONLY, tr("The code alone")),
            (
                crate::presence::MENU_HELP_ASK_ONLY,
                tr("No code: I allow each request"),
            ),
        ];
        for (index, (id, text)) in ways.into_iter().enumerate() {
            let y = 154 + index as i32 * 30;
            let hwnd = make(
                w!("BUTTON"),
                text,
                tab | BS_RADIOBUTTON,
                WINDOW_EX_STYLE(0),
                id,
                [LEFT + 320, y, CONTENT - 324, 26],
            )?;
            checks.push((id, place(Page::Settings, hwnd)));
        }
        let files_while_helping = make(
            w!("BUTTON"),
            tr("Allow files while someone helps"),
            tab | BS_CHECKBOX,
            WINDOW_EX_STYLE(0),
            crate::presence::MENU_HELP_FILES,
            [LEFT + 320, 250, CONTENT - 324, 26],
        )?;
        checks.push((
            crate::presence::MENU_HELP_FILES,
            place(Page::Settings, files_while_helping),
        ));
        let program = [
            (crate::presence::MENU_AUTOSTART, tr("Start with Windows")),
            (
                crate::presence::MENU_AUTO_UPDATE,
                tr("Install updates automatically"),
            ),
        ];
        for (index, (id, text)) in program.into_iter().enumerate() {
            let y = 362 + index as i32 * 30;
            let hwnd = make(
                w!("BUTTON"),
                text,
                tab | BS_CHECKBOX,
                WINDOW_EX_STYLE(0),
                id,
                [LEFT + 4, y, 300, 26],
            )?;
            checks.push((id, place(Page::Settings, hwnd)));
        }
        place(
            Page::Settings,
            button(
                tr("Check for updates"),
                crate::presence::MENU_UPDATE_CHECK,
                [LEFT + 320, 390, 170, 30],
                false,
            )?,
        );
        let password_button = place(
            Page::Settings,
            button(
                tr("Lock settings with a password..."),
                crate::presence::MENU_PASSWORD,
                [LEFT, 440, 260, 34],
                false,
            )?,
        );
        place(
            Page::Settings,
            button(
                tr("Wake-on-LAN check..."),
                crate::presence::MENU_WOL_CHECK,
                [LEFT + 272, 440, 200, 34],
                false,
            )?,
        );

        let window = MainWindow {
            hwnd,
            controls,
            devices,
            no_devices,
            transfers,
            no_transfers,
            help_id,
            help_code,
            help_buttons,
            help_open_button,
            fixed_button,
            password_button,
            checks,
            connect,
            wake,
            more,
        };
        window.arrange();
        Ok(window)
    }

    /// Brings the window up (restored, in front).
    pub(crate) fn show(&self) {
        unsafe {
            if IsIconic(self.hwnd).as_bool() {
                let _ = ShowWindow(self.hwnd, SW_RESTORE);
            } else {
                let _ = ShowWindow(self.hwnd, SW_SHOW);
            }
            let _ = SetForegroundWindow(self.hwnd);
        }
    }

    pub(crate) fn hide(&self) {
        let _ = unsafe { ShowWindow(self.hwnd, SW_HIDE) };
    }

    pub(crate) fn visible(&self) -> bool {
        unsafe { IsWindowVisible(self.hwnd) }.as_bool()
    }

    pub(crate) fn set_page(&mut self, page: Page) {
        STATE.with_borrow_mut(|state| {
            if let Some(state) = state {
                state.page = page;
            }
        });
        self.arrange();
        if page == Page::Help {
            let _ = unsafe { SetFocus(Some(self.help_id)) };
        }
    }

    pub(crate) fn set_account(&mut self, user: &str, account: AccountState) {
        STATE.with_borrow_mut(|state| {
            if let Some(state) = state {
                state.user = user.to_string();
                state.account = account;
            }
        });
        self.arrange();
    }

    pub(crate) fn set_devices(&mut self, devices: Vec<MainDevice>) {
        let selected = self.selected_device();
        let count = devices.len();
        let index = selected.and_then(|id| devices.iter().position(|device| device.id == id));
        STATE.with_borrow_mut(|state| {
            if let Some(state) = state {
                state.devices = devices;
            }
        });
        refill(self.devices, count, index.or((count > 0).then_some(0)));
        self.arrange();
    }

    pub(crate) fn set_transfers(&mut self, transfers: Vec<TransferRow>) {
        let count = transfers.len();
        STATE.with_borrow_mut(|state| {
            if let Some(state) = state {
                state.transfers = transfers;
            }
        });
        refill(self.transfers, count, None);
        self.arrange();
    }

    /// This PC's help ID and code while help is open (None: closed).
    pub(crate) fn set_help(&mut self, help: Option<(String, String)>) {
        STATE.with_borrow_mut(|state| {
            if let Some(state) = state {
                state.help = help;
            }
        });
        self.arrange();
    }

    pub(crate) fn set_settings(&mut self, shown: Shown) {
        for (id, hwnd) in &self.checks {
            let on = match *id {
                crate::presence::MENU_ALLOW => shown.allow,
                crate::presence::MENU_ASK => shown.ask,
                crate::presence::MENU_CLIPBOARD => shown.clipboard,
                crate::presence::MENU_FILES => shown.files,
                crate::presence::MENU_TERMINAL => shown.terminal,
                crate::presence::MENU_LOCK_AFTER => shown.lock_after,
                crate::presence::MENU_AUTOSTART => shown.autostart,
                crate::presence::MENU_AUTO_UPDATE => shown.auto_update,
                crate::presence::MENU_HELP_BOTH => {
                    shown.help_mode == crate::presence::HelpMode::CodeAndAsk
                }
                crate::presence::MENU_HELP_CODE_ONLY => {
                    shown.help_mode == crate::presence::HelpMode::CodeOnly
                }
                crate::presence::MENU_HELP_ASK_ONLY => {
                    shown.help_mode == crate::presence::HelpMode::AskOnly
                }
                crate::presence::MENU_HELP_FILES => shown.help_files,
                _ => false,
            };
            unsafe { SendMessageW(*hwnd, BM_SETCHECK, Some(WPARAM(usize::from(on))), None) };
        }
        crate::forms::set_text(
            self.password_button,
            if shown.password {
                tr("Remove the settings password")
            } else {
                tr("Lock settings with a password...")
            },
        );
        crate::forms::set_text(
            self.fixed_button,
            if shown.fixed_code {
                tr("Remove the fixed help code")
            } else {
                tr("Fixed help code...")
            },
        );
    }

    /// What the person did, if anything (the thread's messages are pumped by Presence).
    pub(crate) fn take(&mut self) -> Option<MainAction> {
        while let Some(command) = crate::forms::take_action(self.hwnd) {
            if let Some(action) = self.action_for(command) {
                return Some(action);
            }
        }
        None
    }

    fn action_for(&mut self, command: u16) -> Option<MainAction> {
        match command {
            ID_CLOSE => {
                self.hide();
                Some(MainAction::Closed)
            }
            id if (ID_NAV..ID_NAV + 4).contains(&id) => {
                self.set_page(Page::ALL[usize::from(id - ID_NAV)]);
                None
            }
            ID_ACCOUNT => Some(MainAction::Account),
            // Enter on the Devices page (the default button), a double click, or Connect.
            ID_CONNECT => self.selected_device().map(MainAction::Connect),
            ID_WAKE => self.selected_device().map(MainAction::Wake),
            ID_MORE => self.selected_device().map(MainAction::More),
            ID_HELP_GO => {
                let help_id: String = crate::forms::text_of(self.help_id)
                    .chars()
                    .filter(char::is_ascii_digit)
                    .collect();
                let code = crate::forms::normalize_code(&crate::forms::text_of(self.help_code));
                if help_id.is_empty() || code.is_empty() {
                    crate::forms::show_error(
                        tr("Queqiao"),
                        tr("Type the help ID and the code you were given."),
                    );
                    return None;
                }
                Some(MainAction::HelpSomeone { help_id, code })
            }
            // An invitation pasted into either field fills both.
            ID_HELP_ID | ID_HELP_CODE => {
                let typed = crate::forms::text_of(if command == ID_HELP_ID {
                    self.help_id
                } else {
                    self.help_code
                });
                if let Some((help_id, code)) = queqiao_core::invitation::parse(&typed) {
                    crate::forms::set_text(self.help_id, &help_id);
                    crate::forms::set_text(self.help_code, &code);
                }
                None
            }
            ID_HELP_COPY => Some(MainAction::CopyInvitation),
            ID_OPEN_RECEIVED => Some(MainAction::OpenReceived),
            ID_CLEAR_TRANSFERS => Some(MainAction::ClearTransfers),
            other => Some(MainAction::Choose(other)),
        }
    }

    fn selected_device(&self) -> Option<String> {
        let index = unsafe { SendMessageW(self.devices, LB_GETCURSEL, None, None) }.0;
        let index = usize::try_from(index).ok()?;
        STATE.with_borrow(|state| {
            let device = state.as_ref()?.devices.get(index)?;
            (!device.this_pc).then(|| device.id.clone())
        })
    }

    /// Shows the current page's controls (and hides the others), with what fits the state.
    fn arrange(&self) {
        let (page, devices, transfers, help, logged_in) = STATE.with_borrow(|state| {
            state
                .as_ref()
                .map_or((Page::Devices, 0, 0, false, false), |state| {
                    (
                        state.page,
                        state.devices.len(),
                        state.transfers.len(),
                        state.help.is_some(),
                        state.account != AccountState::LoggedOut,
                    )
                })
        });
        for placed in &self.controls {
            let mut shown = placed.page == page;
            if placed.hwnd == self.devices {
                shown &= devices > 0;
            } else if placed.hwnd == self.no_devices {
                shown &= devices == 0;
            } else if placed.hwnd == self.transfers {
                shown &= transfers > 0;
            } else if placed.hwnd == self.no_transfers {
                shown &= transfers == 0;
            } else if placed.hwnd == self.help_open_button {
                shown &= !help;
            } else if self.help_buttons.contains(&placed.hwnd) {
                shown &= help;
            } else if placed.hwnd == self.fixed_button {
                shown &= !help;
            }
            let _ = unsafe { ShowWindow(placed.hwnd, if shown { SW_SHOW } else { SW_HIDE }) };
        }
        crate::forms::set_text(
            self.no_devices,
            if logged_in {
                tr("No devices yet: log in to this account on another PC, and it appears here.")
            } else {
                tr("Log in to see your devices.")
            },
        );
        for button in [self.connect, self.wake, self.more] {
            let _ = unsafe { EnableWindow(button, devices > 0) };
        }
        let _ = unsafe { InvalidateRect(Some(self.hwnd), None, false) };
    }

    /// A picture of each page (tests): main_devices.bmp, main_help.bmp ... in `folder`.
    pub(crate) fn save_pages(&mut self, folder: &Path) -> std::io::Result<()> {
        let back =
            STATE.with_borrow(|state| state.as_ref().map_or(Page::Devices, |state| state.page));
        for page in Page::ALL {
            self.set_page(page);
            unsafe {
                let _ = UpdateWindow(self.hwnd);
            }
            crate::forms::pump(50);
            let name = format!("main_{}.bmp", format!("{page:?}").to_lowercase());
            crate::forms::save_window_picture(self.hwnd, &folder.join(name))?;
        }
        self.set_page(back);
        Ok(())
    }
}

impl Drop for MainWindow {
    fn drop(&mut self) {
        let _ = unsafe { DestroyWindow(self.hwnd) };
        STATE.with_borrow_mut(|state| {
            if let Some(state) = state.take() {
                let f = state.fonts;
                for font in [
                    f.app,
                    f.title,
                    f.section,
                    f.body,
                    f.body_bold,
                    f.small,
                    f.big,
                    f.glyph,
                    f.glyph_large,
                ] {
                    let _ = unsafe { DeleteObject(font.into()) };
                }
            }
        });
    }
}

/// `count` owner-drawn rows in `list` (their data is in State), `selected` chosen.
fn refill(list: HWND, count: usize, selected: Option<usize>) {
    unsafe {
        SendMessageW(list, LB_RESETCONTENT, None, None);
        for index in 0..count {
            SendMessageW(list, LB_ADDSTRING, None, Some(LPARAM(index as isize)));
        }
        if let Some(index) = selected {
            SendMessageW(list, LB_SETCURSEL, Some(WPARAM(index)), None);
        }
    }
}

fn px(value: i32, scale: f64) -> i32 {
    (f64::from(value) * scale).round() as i32
}

fn font(size: i32, weight: i32, face: PCWSTR, scale: f64) -> HFONT {
    unsafe {
        CreateFontW(
            -px(size, scale),
            0,
            0,
            0,
            weight,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            CLEARTYPE_QUALITY,
            FF_DONTCARE.0 as u32,
            face,
        )
    }
}

fn rect(x: i32, y: i32, width: i32, height: i32, scale: f64) -> RECT {
    RECT {
        left: px(x, scale),
        top: px(y, scale),
        right: px(x + width, scale),
        bottom: px(y + height, scale),
    }
}

fn fill(dc: HDC, area: &RECT, color: u32) {
    unsafe {
        let brush = CreateSolidBrush(COLORREF(color));
        FillRect(dc, area, brush);
        let _ = DeleteObject(brush.into());
    }
}

/// A rounded rectangle (or a circle, with `round` as large as it) filled with `color`.
fn rounded(dc: HDC, area: &RECT, round: i32, color: u32) {
    unsafe {
        let brush = CreateSolidBrush(COLORREF(color));
        let old_brush = SelectObject(dc, brush.into());
        let old_pen = SelectObject(dc, GetStockObject(NULL_PEN));
        let _ = RoundRect(
            dc,
            area.left,
            area.top,
            area.right + 1,
            area.bottom + 1,
            round,
            round,
        );
        SelectObject(dc, old_pen);
        SelectObject(dc, old_brush);
        let _ = DeleteObject(brush.into());
    }
}

fn dot(dc: HDC, area: &RECT, color: u32) {
    unsafe {
        let brush = CreateSolidBrush(COLORREF(color));
        let old_brush = SelectObject(dc, brush.into());
        let pen = CreatePen(PS_SOLID, 2, COLORREF(PAGE_COLOR));
        let old_pen = SelectObject(dc, pen.into());
        let _ = Ellipse(dc, area.left, area.top, area.right, area.bottom);
        SelectObject(dc, old_pen);
        SelectObject(dc, old_brush);
        let _ = DeleteObject(pen.into());
        let _ = DeleteObject(brush.into());
    }
}

fn text(dc: HDC, value: &str, area: RECT, font: HFONT, color: u32, flags: u32) {
    let mut value: Vec<u16> = value.encode_utf16().collect();
    let mut area = area;
    unsafe {
        let old = SelectObject(dc, font.into());
        SetBkMode(dc, TRANSPARENT);
        SetTextColor(dc, COLORREF(color));
        DrawTextW(
            dc,
            &mut value,
            &mut area,
            windows::Win32::Graphics::Gdi::DRAW_TEXT_FORMAT(flags | DT_NOPREFIX.0),
        );
        SelectObject(dc, old);
    }
}

const LINE: u32 = DT_LEFT.0 | DT_SINGLELINE.0 | DT_VCENTER.0 | DT_END_ELLIPSIS.0;

/// Where the account line is (a click logs in or out), in 96-DPI units.
const ACCOUNT_AREA: [i32; 4] = [12, HEIGHT - 76, SIDEBAR - 24, 60];

fn paint(hwnd: HWND, dc: HDC, state: &State) {
    let s = state.scale;
    let mut area = RECT::default();
    let _ = unsafe { GetClientRect(hwnd, &mut area) };
    fill(dc, &area, PAGE_COLOR);
    fill(dc, &rect(0, 0, SIDEBAR, HEIGHT, s), SIDEBAR_COLOR);
    let f = &state.fonts;

    // The sidebar: the name, the pages, the account.
    text(
        dc,
        tr("Queqiao"),
        rect(24, 22, SIDEBAR - 40, 36, s),
        f.app,
        TEXT_COLOR,
        LINE,
    );
    for (index, page) in Page::ALL.iter().enumerate() {
        let y = NAV_TOP + index as i32 * NAV_STEP;
        if *page == state.page {
            rounded(
                dc,
                &rect(10, y, SIDEBAR - 20, NAV_HEIGHT, s),
                px(8, s),
                NAV_SELECTED,
            );
            rounded(
                dc,
                &rect(10, y + 11, 3, NAV_HEIGHT - 22, s),
                px(3, s),
                ACCENT,
            );
        }
        text(
            dc,
            page.glyph(),
            rect(26, y, 24, NAV_HEIGHT, s),
            f.glyph,
            TEXT_COLOR,
            LINE,
        );
        text(
            dc,
            page.title(),
            rect(58, y, SIDEBAR - 76, NAV_HEIGHT, s),
            f.body,
            TEXT_COLOR,
            LINE,
        );
    }
    let [ax, ay, aw, _] = ACCOUNT_AREA;
    fill(dc, &rect(ax, ay - 8, aw, 1, s), DIVIDER);
    let initial = state
        .user
        .chars()
        .next()
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_default();
    let avatar = rect(ax + 4, ay + 12, 36, 36, s);
    rounded(
        dc,
        &avatar,
        px(36, s),
        if state.user.is_empty() {
            OFFLINE
        } else {
            ACCENT
        },
    );
    text(
        dc,
        if initial.is_empty() {
            "\u{E77B}"
        } else {
            &initial
        },
        avatar,
        if initial.is_empty() {
            f.glyph
        } else {
            f.body_bold
        },
        PAGE_COLOR,
        DT_CENTER.0 | DT_SINGLELINE.0 | DT_VCENTER.0,
    );
    let (status, color, link) = match state.account {
        AccountState::Online => (tr("Online"), ONLINE, tr("Log out")),
        AccountState::Offline => (tr("Cannot reach the server"), OFFLINE, tr("Log out")),
        AccountState::LoggedOut => (tr("Not logged in"), OFFLINE, tr("Log in")),
    };
    let name = if state.user.is_empty() {
        tr("Queqiao")
    } else {
        &state.user
    };
    text(
        dc,
        name,
        rect(ax + 50, ay + 6, aw - 54, 20, s),
        f.body_bold,
        TEXT_COLOR,
        LINE,
    );
    rounded(dc, &rect(ax + 50, ay + 31, 8, 8, s), px(8, s), color);
    text(
        dc,
        status,
        rect(ax + 62, ay + 26, aw - 66, 18, s),
        f.small,
        SECONDARY,
        LINE,
    );
    text(
        dc,
        link,
        rect(ax + 50, ay + 44, aw - 54, 16, s),
        f.small,
        ACCENT,
        LINE,
    );

    // The page: its title and a line about it.
    text(
        dc,
        state.page.title(),
        rect(LEFT, 24, CONTENT, 40, s),
        f.title,
        TEXT_COLOR,
        LINE,
    );
    text(
        dc,
        state.page.subtitle(),
        rect(LEFT, 64, CONTENT, 22, s),
        f.small,
        SECONDARY,
        LINE,
    );
    match state.page {
        Page::Help => {
            text(
                dc,
                tr("Let someone help this PC"),
                rect(LEFT, 98, CONTENT, 24, s),
                f.section,
                TEXT_COLOR,
                LINE,
            );
            rounded(dc, &rect(LEFT, 128, CONTENT, 132, s), px(10, s), CARD);
            match &state.help {
                Some((help_id, code)) => {
                    text(
                        dc,
                        tr("Help ID"),
                        rect(LEFT + 20, 138, 260, 18, s),
                        f.small,
                        SECONDARY,
                        LINE,
                    );
                    text(
                        dc,
                        &queqiao_core::invitation::spaced_id(help_id),
                        rect(LEFT + 20, 156, 280, 44, s),
                        f.big,
                        TEXT_COLOR,
                        LINE,
                    );
                    text(
                        dc,
                        tr("Code"),
                        rect(LEFT + 320, 138, 260, 18, s),
                        f.small,
                        SECONDARY,
                        LINE,
                    );
                    text(
                        dc,
                        &queqiao_core::invitation::halved_code(code),
                        rect(LEFT + 320, 156, 280, 44, s),
                        f.big,
                        ACCENT,
                        LINE,
                    );
                }
                None => text(
                    dc,
                    tr(
                        "Someone of another account can control this PC with its help ID and code, after you allow it each time. Command lines, files, restarting and the sign-in screen stay closed to them.",
                    ),
                    rect(LEFT + 20, 140, CONTENT - 40, 64, s),
                    f.body,
                    SECONDARY,
                    DT_LEFT.0 | DT_WORDBREAK.0,
                ),
            }
            text(
                dc,
                tr("Help someone"),
                rect(LEFT, 290, CONTENT, 24, s),
                f.section,
                TEXT_COLOR,
                LINE,
            );
            text(
                dc,
                tr("Help ID"),
                rect(LEFT, 334, 220, 18, s),
                f.small,
                SECONDARY,
                LINE,
            );
            text(
                dc,
                tr("Code"),
                rect(LEFT + 236, 334, 220, 18, s),
                f.small,
                SECONDARY,
                LINE,
            );
            text(
                dc,
                tr(
                    "Pasting an invitation into either field fills in both. The person there is asked before you see their screen.",
                ),
                rect(LEFT, 396, CONTENT, 40, s),
                f.small,
                SECONDARY,
                DT_LEFT.0 | DT_WORDBREAK.0,
            );
        }
        Page::Settings => {
            text(
                dc,
                tr("This PC"),
                rect(LEFT, 98, CONTENT, 24, s),
                f.section,
                TEXT_COLOR,
                LINE,
            );
            text(
                dc,
                tr("Remote help"),
                rect(LEFT + 320, 98, CONTENT - 320, 24, s),
                f.section,
                TEXT_COLOR,
                LINE,
            );
            text(
                dc,
                tr("Someone of another account starts helping with:"),
                rect(LEFT + 320, 126, CONTENT - 320, 22, s),
                f.small,
                SECONDARY,
                LINE,
            );
            text(
                dc,
                tr("Queqiao"),
                rect(LEFT, 330, CONTENT, 24, s),
                f.section,
                TEXT_COLOR,
                LINE,
            );
            let version = trf("Version {version}", &[("version", &state.version)]);
            text(
                dc,
                &version,
                rect(LEFT, 500, CONTENT, 20, s),
                f.small,
                SECONDARY,
                LINE,
            );
        }
        Page::Devices | Page::Transfers => {}
    }
}

fn draw_device(item: &DRAWITEMSTRUCT, state: &State) {
    let s = state.scale;
    let dc = item.hDC;
    let area = item.rcItem;
    let selected = item.itemState.0 & ODS_SELECTED != 0;
    fill(dc, &area, if selected { ROW_SELECTED } else { PAGE_COLOR });
    let Some(device) = state.devices.get(item.itemID as usize) else {
        return;
    };
    let f = &state.fonts;
    let (x, y) = (area.left, area.top);
    let icon = RECT {
        left: x + px(12, s),
        top: y + px(12, s),
        right: x + px(48, s),
        bottom: y + px(48, s),
    };
    rounded(dc, &icon, px(10, s), CARD);
    text(
        dc,
        GLYPH_DEVICES,
        icon,
        f.glyph_large,
        SECONDARY,
        DT_CENTER.0 | DT_SINGLELINE.0 | DT_VCENTER.0,
    );
    let status = RECT {
        left: icon.right - px(11, s),
        top: icon.bottom - px(11, s),
        right: icon.right + px(3, s),
        bottom: icon.bottom + px(3, s),
    };
    dot(dc, &status, if device.online { ONLINE } else { OFFLINE });
    let name = RECT {
        left: x + px(64, s),
        top: y + px(10, s),
        right: area.right - px(12, s),
        bottom: y + px(32, s),
    };
    text(dc, &device.name, name, f.body_bold, TEXT_COLOR, LINE);
    let detail = match (device.this_pc, device.online) {
        (true, _) => tr("This PC"),
        (false, true) => tr("Online"),
        (false, false) => tr("Offline"),
    };
    let second = RECT {
        top: y + px(32, s),
        bottom: y + px(50, s),
        ..name
    };
    text(dc, detail, second, f.small, SECONDARY, LINE);
    fill(
        dc,
        &RECT {
            left: x + px(64, s),
            top: area.bottom - 1,
            right: area.right,
            bottom: area.bottom,
        },
        DIVIDER,
    );
}

fn draw_transfer(item: &DRAWITEMSTRUCT, state: &State) {
    let s = state.scale;
    let dc = item.hDC;
    let area = item.rcItem;
    fill(dc, &area, PAGE_COLOR);
    let Some(row) = state.transfers.get(item.itemID as usize) else {
        return;
    };
    let f = &state.fonts;
    let (x, y) = (area.left, area.top);
    let icon = RECT {
        left: x + px(12, s),
        top: y + px(10, s),
        right: x + px(44, s),
        bottom: y + px(42, s),
    };
    rounded(dc, &icon, px(8, s), CARD);
    let glyph = if row.received {
        GLYPH_RECEIVED
    } else {
        GLYPH_SENT
    };
    text(
        dc,
        glyph,
        icon,
        f.glyph,
        ACCENT,
        DT_CENTER.0 | DT_SINGLELINE.0 | DT_VCENTER.0,
    );
    let name = RECT {
        left: x + px(58, s),
        top: y + px(7, s),
        right: area.right - px(12, s),
        bottom: y + px(28, s),
    };
    text(dc, &row.name, name, f.body, TEXT_COLOR, LINE);
    let second = RECT {
        top: y + px(28, s),
        bottom: y + px(46, s),
        ..name
    };
    text(dc, &row.detail, second, f.small, SECONDARY, LINE);
}

extern "system" fn main_proc(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let push = |command: u16| crate::forms::push_action(hwnd, command);
    match message {
        WM_PAINT => {
            let mut paint_info = PAINTSTRUCT::default();
            let dc = unsafe { BeginPaint(hwnd, &mut paint_info) };
            let mut area = RECT::default();
            let _ = unsafe { GetClientRect(hwnd, &mut area) };
            // Drawn off screen first, then copied: no flicker.
            let memory = unsafe { CreateCompatibleDC(Some(dc)) };
            let bitmap = unsafe { CreateCompatibleBitmap(dc, area.right, area.bottom) };
            let old = unsafe { SelectObject(memory, bitmap.into()) };
            STATE.with(|state| {
                if let Ok(state) = state.try_borrow()
                    && let Some(state) = state.as_ref()
                {
                    paint(hwnd, memory, state);
                }
            });
            unsafe {
                let _ = BitBlt(
                    dc,
                    0,
                    0,
                    area.right,
                    area.bottom,
                    Some(memory),
                    0,
                    0,
                    SRCCOPY,
                );
                SelectObject(memory, old);
                let _ = DeleteObject(bitmap.into());
                let _ = DeleteDC(memory);
                let _ = EndPaint(hwnd, &paint_info);
            }
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_LBUTTONUP => {
            let (x, y) = (
                (lparam.0 & 0xFFFF) as i16 as i32,
                ((lparam.0 >> 16) & 0xFFFF) as i16 as i32,
            );
            let scale = STATE.with_borrow(|state| state.as_ref().map_or(1.0, |state| state.scale));
            let (x, y) = ((f64::from(x) / scale) as i32, (f64::from(y) / scale) as i32);
            if x < SIDEBAR {
                for index in 0..4 {
                    let top = NAV_TOP + index * NAV_STEP;
                    if (top..top + NAV_HEIGHT).contains(&y) {
                        push(ID_NAV + index as u16);
                    }
                }
                let [ax, ay, aw, ah] = ACCOUNT_AREA;
                if (ax..ax + aw).contains(&x) && (ay..ay + ah).contains(&y) {
                    push(ID_ACCOUNT);
                }
            }
            LRESULT(0)
        }
        WM_SETCURSOR => {
            // The hand over what can be clicked in the sidebar.
            let mut point = windows::Win32::Foundation::POINT::default();
            unsafe {
                let _ = GetCursorPos(&mut point);
                let _ = windows::Win32::Graphics::Gdi::ScreenToClient(hwnd, &mut point);
            }
            let scale = STATE.with_borrow(|state| state.as_ref().map_or(1.0, |state| state.scale));
            let (x, y) = (
                (f64::from(point.x) / scale) as i32,
                (f64::from(point.y) / scale) as i32,
            );
            let [ax, ay, aw, ah] = ACCOUNT_AREA;
            let over_nav =
                x < SIDEBAR && (NAV_TOP..NAV_TOP + 3 * NAV_STEP + NAV_HEIGHT).contains(&y);
            let over_account = (ax..ax + aw).contains(&x) && (ay..ay + ah).contains(&y);
            if wparam.0 == hwnd.0 as usize && (over_nav || over_account) {
                unsafe { SetCursor(LoadCursorW(None, IDC_HAND).ok()) };
                return LRESULT(1);
            }
            unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
        }
        WM_MEASUREITEM => {
            let item = unsafe { &mut *(lparam.0 as *mut MEASUREITEMSTRUCT) };
            let scale = STATE.with_borrow(|state| state.as_ref().map_or(1.0, |state| state.scale));
            let row = if item.CtlID == u32::from(ID_TRANSFERS) {
                TRANSFER_ROW
            } else {
                DEVICE_ROW
            };
            item.itemHeight = px(row, scale) as u32;
            LRESULT(1)
        }
        WM_DRAWITEM => {
            let item = unsafe { &*(lparam.0 as *const DRAWITEMSTRUCT) };
            STATE.with(|state| {
                if let Ok(state) = state.try_borrow()
                    && let Some(state) = state.as_ref()
                {
                    if item.CtlID == u32::from(ID_DEVICES) {
                        draw_device(item, state);
                    } else if item.CtlID == u32::from(ID_TRANSFERS) {
                        draw_transfer(item, state);
                    }
                }
            });
            LRESULT(1)
        }
        WM_COMMAND => {
            let id = (wparam.0 & 0xFFFF) as u16;
            let code = ((wparam.0 >> 16) & 0xFFFF) as u16;
            match (id, code) {
                (ID_DEVICES, LBN_DBLCLK) => push(ID_CONNECT),
                (ID_HELP_ID | ID_HELP_CODE, EN_CHANGE) => push(id),
                (ID_DEVICES | ID_TRANSFERS | ID_HELP_ID | ID_HELP_CODE, _) => {}
                // Enter: the page's default action.
                (1, _) => {
                    let page = STATE.with_borrow(|state| state.as_ref().map(|state| state.page));
                    match page {
                        Some(Page::Devices) => push(ID_CONNECT),
                        Some(Page::Help) => push(ID_HELP_GO),
                        _ => {}
                    }
                }
                (2, _) => push(ID_CLOSE),
                (id, BN_CLICKED) if id != 0 => push(id),
                _ => {}
            }
            LRESULT(0)
        }
        WM_CTLCOLORSTATIC | WM_CTLCOLORBTN => unsafe {
            // Labels and check boxes on the page's white.
            let dc = HDC(wparam.0 as *mut c_void);
            SetBkMode(dc, TRANSPARENT);
            SetTextColor(dc, COLORREF(SECONDARY));
            LRESULT(page_brush().0 as isize)
        },
        WM_CTLCOLORLISTBOX => LRESULT(page_brush().0 as isize),
        WM_CLOSE => {
            push(ID_CLOSE);
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

/// The page's white, made once (it lives as long as the program).
fn page_brush() -> HBRUSH {
    use std::sync::OnceLock;
    static BRUSH: OnceLock<usize> = OnceLock::new();
    HBRUSH(
        *BRUSH.get_or_init(|| unsafe { CreateSolidBrush(COLORREF(PAGE_COLOR)) }.0 as usize)
            as *mut c_void,
    )
}
