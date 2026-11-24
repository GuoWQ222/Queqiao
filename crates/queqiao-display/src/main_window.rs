//! Queqiao's main window, organised as UU Remote's, in Queqiao's own look: a night-blue title bar and sidebar (the
//! account's devices, then Help, Transfers and Settings, the account at the bottom) and a white page beside it. A
//! device chosen in the sidebar gets its page: whether it is online, a card joining this PC to it by a bridge of
//! magpies (Connect, or Wake up when it is offline), its tools (files, a command line, waking it, more) and the
//! latest connections to it from this PC. It lives on the agent's UI thread with the tray icon (presence.rs) and is
//! polled from there; its settings go through the same `Presence::choose` as the tray menu (so the settings password
//! and the lock while someone helps apply to both). Painted with GDI (the sidebar, the device page, the owner-drawn
//! lists), with standard controls for what is typed or ticked.

use std::cell::RefCell;
use std::ffi::c_void;
use std::path::Path;
use std::sync::Once;

use queqiao_core::text::{tr, trf};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{DWMWINDOWATTRIBUTE, DwmSetWindowAttribute};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, CreateCompatibleBitmap,
    CreateCompatibleDC, CreateFontW, CreatePen, CreateRoundRectRgn, CreateSolidBrush,
    DEFAULT_CHARSET, DT_CENTER, DT_END_ELLIPSIS, DT_LEFT, DT_NOPREFIX, DT_RIGHT, DT_SINGLELINE,
    DT_VCENTER, DT_WORDBREAK, DeleteDC, DeleteObject, DrawTextW, Ellipse, EndPaint, FF_DONTCARE,
    FillRect, GRADIENT_FILL_RECT_V, GRADIENT_RECT, GetStockObject, GradientFill, HBRUSH, HDC,
    HFONT, InvalidateRect, NULL_PEN, OUT_DEFAULT_PRECIS, PAINTSTRUCT, PS_SOLID, Polygon, RoundRect,
    SRCCOPY, SelectClipRgn, SelectObject, SetBkMode, SetTextColor, TRANSPARENT, TRIVERTEX,
    UpdateWindow,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::{DRAWITEMSTRUCT, MEASUREITEMSTRUCT};
use windows::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForWindow};
use windows::Win32::UI::Input::KeyboardAndMouse::SetFocus;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, Result, w};

use crate::model::AccountState;
use crate::model::{ConnectionRow, MainDevice, TransferRow};

const CLASS: PCWSTR = w!("QueqiaoMain");
static REGISTER: Once = Once::new();

// Layout, in 96-DPI units.
const WIDTH: i32 = 880;
const HEIGHT: i32 = 580;
const SIDEBAR: i32 = 216;
const LEFT: i32 = SIDEBAR + 32;
const CONTENT: i32 = WIDTH - LEFT - 32;
// The sidebar: the name, the devices, the pages, the account.
const DEVICES_LABEL_Y: i32 = 62;
const DEVICE_LIST: [i32; 4] = [10, 84, SIDEBAR - 20, 216];
const DEVICE_ROW: i32 = 36;
const NAV_TOP: i32 = 318;
const NAV_STEP: i32 = 40;
const NAV_HEIGHT: i32 = 36;
const TRANSFER_ROW: i32 = 52;
/// Where the account line is (a click logs in or out).
const ACCOUNT_AREA: [i32; 4] = [12, HEIGHT - 70, SIDEBAR - 24, 58];
// The device page.
const HERO: [i32; 4] = [LEFT, 92, CONTENT, 180];
const ACTIONS: [i32; 4] = [LEFT, 288, CONTENT, 52];
const RECENT_Y: i32 = 360;
const RECENT_ROW: i32 = 38;
const RECENT_ROWS: usize = 3;
/// The size of a magpie on the card's bridge.
const BIRD: i32 = 36;

// Colours (COLORREF: 0x00BBGGRR). Night blue and star gold around a white page.
const BADGE: u32 = 0x00F8_F1EE;
const SIDE_BG: u32 = 0x003B_1A14;
const SIDE_SELECTED: u32 = 0x0069_3126;
const SIDE_TEXT: u32 = 0x00EE_D7D2;
const SIDE_MUTED: u32 = 0x00B5_9088;
const SIDE_LINE: u32 = 0x005E_3129;
const SIDE_CHIP: u32 = 0x0074_382D;
const GOLD: u32 = 0x005A_B4E3;
const ON_GOLD: u32 = 0x003F_1F1A;
const WHITE: u32 = 0x00FF_FFFF;
const PAGE_COLOR: u32 = WHITE;
const TEXT_COLOR: u32 = 0x0040_211B;
const SECONDARY: u32 = 0x008C_6B64;
const DISABLED: u32 = 0x00CC_B9B4;
const DIVIDER: u32 = 0x00F2_E9E6;
const CARD: u32 = 0x00FA_F6F5;
const ACCENT: u32 = 0x00AE_5F3D;
const ONLINE: u32 = 0x0071_BF2F;
const OFFLINE: u32 = 0x00B8_A19A;
const PILL_ON: (u32, u32) = (0x00EC_F6E3, 0x004A_7817);
const PILL_OFF: (u32, u32) = (0x00F5_F0EE, 0x0080_645D);
const PILL_SELF: (u32, u32) = (0x00FB_EDE8, 0x00AE_5F3D);
const HERO_TOP: (u16, u16, u16) = (0x25, 0x30, 0x70);
const HERO_BOTTOM: (u16, u16, u16) = (0x0E, 0x13, 0x30);
const HERO_MUTED: u32 = 0x00D6_B1A9;
const HERO_FAINT: u32 = 0x008A_6A5A;

// Glyphs of Segoe MDL2 Assets (Windows 10 and later).
const GLYPH_DEVICES: &str = "\u{E7F4}";
const GLYPH_HELP: &str = "\u{E716}";
const GLYPH_TRANSFERS: &str = "\u{E896}";
const GLYPH_SETTINGS: &str = "\u{E713}";
const GLYPH_RECEIVED: &str = "\u{E896}";
const GLYPH_SENT: &str = "\u{E898}";
const GLYPH_FILES: &str = "\u{E8B7}";
const GLYPH_TERMINAL: &str = "\u{E756}";
const GLYPH_WAKE: &str = "\u{E7E8}";
const GLYPH_MORE: &str = "\u{E712}";
const GLYPH_RECENT: &str = "\u{E823}";

// Control styles (winuser.h) as plain numbers, as in forms.rs.
const ES_AUTOHSCROLL: u32 = 0x80;
const LBS_NOTIFY: u32 = 0x1;
const LBS_OWNERDRAWFIXED: u32 = 0x10;
const LBS_NOINTEGRALHEIGHT: u32 = 0x100;
const LBN_SELCHANGE: u16 = 1;
const LBN_DBLCLK: u16 = 2;
const EN_CHANGE: u16 = 0x0300;
const BN_CLICKED: u16 = 0;
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
const ID_FILES: u16 = 315;
const ID_TERMINAL: u16 = 316;
const ID_SELECT: u16 = 317;
const ID_HELP_ID: u16 = 320;
const ID_HELP_CODE: u16 = 321;
const ID_HELP_GO: u16 = 322;
const ID_HELP_COPY: u16 = 323;
const ID_NEW_HELP_ID: u16 = 324;
const ID_TRANSFERS: u16 = 330;
const ID_OPEN_RECEIVED: u16 = 331;
const ID_CLEAR_TRANSFERS: u16 = 332;
const ID_CLOSE: u16 = 0xFFFF;

// Windows 11's title bar colours (dwmapi.h); earlier Windows ignore them.
const DWMWA_USE_IMMERSIVE_DARK_MODE: DWMWINDOWATTRIBUTE = DWMWINDOWATTRIBUTE(20);
const DWMWA_BORDER_COLOR: DWMWINDOWATTRIBUTE = DWMWINDOWATTRIBUTE(34);
const DWMWA_CAPTION_COLOR: DWMWINDOWATTRIBUTE = DWMWINDOWATTRIBUTE(35);
const DWMWA_TEXT_COLOR: DWMWINDOWATTRIBUTE = DWMWINDOWATTRIBUTE(36);

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

/// The settings as the Settings page shows them (the tray menu's own copy is in Presence).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Shown {
    pub help_mode: crate::model::HelpMode,
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
    /// Connect, then send files to it (a file picker in the session).
    Files(String),
    /// Connect, then open a command line on it.
    Terminal(String),
    Wake(String),
    /// More... for that device: a menu (`more_menu`).
    More(String),
    HelpSomeone {
        help_id: String,
        code: String,
    },
    CopyInvitation,
    /// A new help ID (the old one stops working).
    NewHelpId,
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
    /// The device chosen in the sidebar (an index into `devices`).
    selected: Option<usize>,
    transfers: Vec<TransferRow>,
    connections: Vec<ConnectionRow>,
    help: Option<(String, String)>,
    fonts: Fonts,
    version: String,
    icon: HICON,
    /// The magpie with its wings spread, for the card's bridge (None in a program without it).
    bird: Option<HICON>,
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
    glyph_hero: HFONT,
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
    transfers: HWND,
    no_transfers: HWND,
    help_id: HWND,
    help_code: HWND,
    help_buttons: Vec<HWND>,
    help_open_button: HWND,
    fixed_button: HWND,
    password_button: HWND,
    checks: Vec<(u16, HWND)>,
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
        // The title bar in the sidebar's night blue, with white text (Windows 11; earlier Windows keep their own).
        unsafe {
            let dark: i32 = 1;
            let set = |attribute, value: &u32| {
                let _ = DwmSetWindowAttribute(
                    hwnd,
                    attribute,
                    std::ptr::from_ref(value).cast(),
                    size_of::<u32>() as u32,
                );
            };
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_USE_IMMERSIVE_DARK_MODE,
                std::ptr::from_ref(&dark).cast(),
                size_of::<i32>() as u32,
            );
            set(DWMWA_CAPTION_COLOR, &SIDE_BG);
            set(DWMWA_BORDER_COLOR, &SIDE_BG);
            set(DWMWA_TEXT_COLOR, &WHITE);
        }
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
            glyph_hero: font(30, 400, w!("Segoe MDL2 Assets"), scale),
        };
        let body = fonts.body;
        STATE.with_borrow_mut(|state| {
            *state = Some(State {
                scale,
                page: Page::Devices,
                user: String::new(),
                account: AccountState::LoggedOut,
                devices: Vec::new(),
                selected: None,
                transfers: Vec::new(),
                connections: Vec::new(),
                help: None,
                fonts,
                version: version.to_string(),
                icon: crate::icon::app_icon(px(28, scale)),
                bird: crate::icon::spread_magpie(px(BIRD, scale)),
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
        let button = |text: &str, id: u16, place: [i32; 4]| {
            make(w!("BUTTON"), text, tab, WINDOW_EX_STYLE(0), id, place)
        };
        let mut controls = Vec::new();
        let mut place = |page: Page, hwnd: HWND| {
            controls.push(Placed { page, hwnd });
            hwnd
        };

        // The account's devices, in the sidebar (on every page).
        let list_style =
            tab | LBS_NOTIFY | LBS_OWNERDRAWFIXED | LBS_NOINTEGRALHEIGHT | WS_VSCROLL.0;
        let devices = make(
            w!("LISTBOX"),
            "",
            list_style,
            WINDOW_EX_STYLE(0),
            ID_DEVICES,
            DEVICE_LIST,
        )?;

        // Help: this PC's card (its buttons inside), then helping someone.
        let help_open_button = place(
            Page::Help,
            button(
                tr("Let someone help me..."),
                crate::presence::MENU_HELP,
                [LEFT + 20, 212, 200, 34],
            )?,
        );
        let mut help_buttons = Vec::new();
        for (index, (text, id)) in [
            (tr("Copy invitation"), ID_HELP_COPY),
            (tr("New code"), crate::presence::ID_NEW_CODE),
            (tr("New help ID"), ID_NEW_HELP_ID),
            (tr("Stop"), crate::presence::ID_HELP_CLOSE),
        ]
        .into_iter()
        .enumerate()
        {
            let x = LEFT + 20 + index as i32 * 146;
            help_buttons.push(place(Page::Help, button(text, id, [x, 212, 134, 34])?));
        }
        let fixed_button = place(
            Page::Help,
            button(
                tr("Fixed help code..."),
                crate::presence::MENU_FIXED_CODE,
                [LEFT + CONTENT - 200, 212, 180, 34],
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
            button(tr("Connect"), ID_HELP_GO, [LEFT + 472, 354, 128, 34])?,
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
            )?,
        );
        place(
            Page::Transfers,
            button(
                tr("Clear the list"),
                ID_CLEAR_TRANSFERS,
                [LEFT + 192, 492, 140, 34],
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
            )?,
        );
        let password_button = place(
            Page::Settings,
            button(
                tr("Lock settings with a password..."),
                crate::presence::MENU_PASSWORD,
                [LEFT, 440, 260, 34],
            )?,
        );
        place(
            Page::Settings,
            button(
                tr("Wake-on-LAN check..."),
                crate::presence::MENU_WOL_CHECK,
                [LEFT + 272, 440, 200, 34],
            )?,
        );

        let window = MainWindow {
            hwnd,
            controls,
            devices,
            transfers,
            no_transfers,
            help_id,
            help_code,
            help_buttons,
            help_open_button,
            fixed_button,
            password_button,
            checks,
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
        // The same device stays chosen; else the first other device (this PC is listed first).
        let chosen = STATE.with_borrow(|state| {
            state
                .as_ref()
                .and_then(|state| state.selected.and_then(|at| state.devices.get(at)))
                .map(|device| device.id.clone())
        });
        let count = devices.len();
        let index = chosen
            .and_then(|id| devices.iter().position(|device| device.id == id))
            .or_else(|| devices.iter().position(|device| !device.this_pc))
            .or((count > 0).then_some(0));
        STATE.with_borrow_mut(|state| {
            if let Some(state) = state {
                state.devices = devices;
                state.selected = index;
            }
        });
        refill(self.devices, count, index);
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

    /// The sessions this PC started, newest first (each device's page shows its own).
    pub(crate) fn set_connections(&mut self, connections: Vec<ConnectionRow>) {
        STATE.with_borrow_mut(|state| {
            if let Some(state) = state {
                state.connections = connections;
            }
        });
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
                    shown.help_mode == crate::model::HelpMode::CodeAndAsk
                }
                crate::presence::MENU_HELP_CODE_ONLY => {
                    shown.help_mode == crate::model::HelpMode::CodeOnly
                }
                crate::presence::MENU_HELP_ASK_ONLY => {
                    shown.help_mode == crate::model::HelpMode::AskOnly
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
            ID_SELECT => {
                let index = unsafe { SendMessageW(self.devices, LB_GETCURSEL, None, None) }.0;
                let index = usize::try_from(index).ok();
                STATE.with_borrow_mut(|state| {
                    if let Some(state) = state {
                        state.selected = index;
                    }
                });
                self.set_page(Page::Devices);
                None
            }
            ID_ACCOUNT => Some(MainAction::Account),
            // Enter on the Devices page, a double click in the list, or the card's button.
            ID_CONNECT => self.selected_device().map(MainAction::Connect),
            ID_FILES => self.selected_device().map(MainAction::Files),
            ID_TERMINAL => self.selected_device().map(MainAction::Terminal),
            ID_WAKE => self.selected_device().map(MainAction::Wake),
            ID_MORE => self.selected_device().map(MainAction::More),
            ID_HELP_GO => {
                let help_id: String = crate::forms::text_of(self.help_id)
                    .chars()
                    .filter(char::is_ascii_digit)
                    .collect();
                let code = crate::model::normalize_code(&crate::forms::text_of(self.help_code));
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
            ID_NEW_HELP_ID => Some(MainAction::NewHelpId),
            ID_OPEN_RECEIVED => Some(MainAction::OpenReceived),
            ID_CLEAR_TRANSFERS => Some(MainAction::ClearTransfers),
            other => Some(MainAction::Choose(other)),
        }
    }

    /// The chosen device, if it is another device of the account (this PC is not connected to).
    fn selected_device(&self) -> Option<String> {
        STATE.with_borrow(|state| {
            let state = state.as_ref()?;
            let device = state.devices.get(state.selected?)?;
            (!device.this_pc).then(|| device.id.clone())
        })
    }

    /// Shows the current page's controls (and hides the others), with what fits the state.
    fn arrange(&self) {
        let (page, devices, transfers, help) = STATE.with_borrow(|state| {
            state
                .as_ref()
                .map_or((Page::Devices, 0, 0, false), |state| {
                    (
                        state.page,
                        state.devices.len(),
                        state.transfers.len(),
                        state.help.is_some(),
                    )
                })
        });
        for placed in &self.controls {
            let mut shown = placed.page == page;
            if placed.hwnd == self.transfers {
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
        let _ = unsafe { ShowWindow(self.devices, if devices > 0 { SW_SHOW } else { SW_HIDE }) };
        let _ = unsafe { InvalidateRect(Some(self.devices), None, false) };
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
        // The Devices page once more as it looks for a device that is online (its card with the magpies), when the
        // chosen one is offline: main_devices_online.bmp.
        let offline = STATE.with_borrow_mut(|state| {
            let state = state.as_mut()?;
            let device = state.devices.get_mut(state.selected?)?;
            (!device.online && !device.this_pc).then(|| {
                device.online = true;
            })
        });
        if offline.is_some() {
            self.set_page(Page::Devices);
            unsafe {
                let _ = UpdateWindow(self.hwnd);
            }
            crate::forms::pump(50);
            let picture = crate::forms::save_window_picture(
                self.hwnd,
                &folder.join("main_devices_online.bmp"),
            );
            STATE.with_borrow_mut(|state| {
                if let Some(state) = state.as_mut()
                    && let Some(at) = state.selected
                    && let Some(device) = state.devices.get_mut(at)
                {
                    device.online = false;
                }
            });
            picture?;
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
                    f.glyph_hero,
                ] {
                    let _ = unsafe { DeleteObject(font.into()) };
                }
                let _ = unsafe { DestroyIcon(state.icon) };
                if let Some(bird) = state.bird {
                    let _ = unsafe { DestroyIcon(bird) };
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

fn rect_of(place: [i32; 4], scale: f64) -> RECT {
    rect(place[0], place[1], place[2], place[3], scale)
}

fn inside(place: [i32; 4], x: i32, y: i32) -> bool {
    (place[0]..place[0] + place[2]).contains(&x) && (place[1]..place[1] + place[3]).contains(&y)
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

/// A rounded rectangle with a 1-pixel border.
fn outlined(dc: HDC, area: &RECT, round: i32, color: u32, border: u32) {
    rounded(dc, area, round, border);
    let inner = RECT {
        left: area.left + 1,
        top: area.top + 1,
        right: area.right - 1,
        bottom: area.bottom - 1,
    };
    rounded(dc, &inner, (round - 2).max(0), color);
}

fn dot(dc: HDC, area: &RECT, color: u32, ring: u32) {
    unsafe {
        let brush = CreateSolidBrush(COLORREF(color));
        let old_brush = SelectObject(dc, brush.into());
        let pen = CreatePen(PS_SOLID, 2, COLORREF(ring));
        let old_pen = SelectObject(dc, pen.into());
        let _ = Ellipse(dc, area.left, area.top, area.right, area.bottom);
        SelectObject(dc, old_pen);
        SelectObject(dc, old_brush);
        let _ = DeleteObject(pen.into());
        let _ = DeleteObject(brush.into());
    }
}

fn text(dc: HDC, value: &str, area: RECT, font: HFONT, color: u32, flags: u32) {
    // DrawTextW reads the text even when told it is 0 long: an empty Vec's dangling pointer crashes it.
    if value.is_empty() {
        return;
    }
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

/// How wide `value` is in `font`, in pixels.
fn text_width(dc: HDC, value: &str, font: HFONT) -> i32 {
    // DrawTextW reads the text even when told it is 0 long: an empty Vec's dangling pointer crashes it.
    if value.is_empty() {
        return 0;
    }
    let mut value: Vec<u16> = value.encode_utf16().collect();
    let mut area = RECT::default();
    unsafe {
        let old = SelectObject(dc, font.into());
        DrawTextW(
            dc,
            &mut value,
            &mut area,
            windows::Win32::Graphics::Gdi::DRAW_TEXT_FORMAT(
                DT_CALCRECT_FLAG | DT_SINGLELINE.0 | DT_NOPREFIX.0,
            ),
        );
        SelectObject(dc, old);
    }
    area.right - area.left
}

const DT_CALCRECT_FLAG: u32 = 0x400;
const LINE: u32 = DT_LEFT.0 | DT_SINGLELINE.0 | DT_VCENTER.0 | DT_END_ELLIPSIS.0;
const CENTRED: u32 = DT_CENTER.0 | DT_SINGLELINE.0 | DT_VCENTER.0 | DT_END_ELLIPSIS.0;

/// The chosen device, from the state.
fn chosen(state: &State) -> Option<&MainDevice> {
    state.devices.get(state.selected?)
}

/// The card's button on the Devices page: where it is and what it does (None: no button).
fn go_button(state: &State) -> Option<([i32; 4], u16, &'static str)> {
    if state.account == AccountState::LoggedOut {
        return Some(([LEFT, 182, 140, 38], ID_ACCOUNT, tr("Log in")));
    }
    let device = chosen(state)?;
    if device.this_pc {
        return None;
    }
    let place = [HERO[0] + HERO[2] / 2 - 70, HERO[1] + HERO[3] - 54, 140, 38];
    Some(if device.online {
        (place, ID_CONNECT, tr("Connect"))
    } else {
        (place, ID_WAKE, tr("Wake up"))
    })
}

/// The tools under the card: where, what, glyph, label, and whether they can be used now.
fn action_cells(state: &State) -> [([i32; 4], u16, &'static str, &'static str, bool); 4] {
    let (online, other) =
        chosen(state).map_or((false, false), |device| (device.online, !device.this_pc));
    let width = ACTIONS[2] / 4;
    let cell = |index: i32| [ACTIONS[0] + index * width, ACTIONS[1], width, ACTIONS[3]];
    [
        (
            cell(0),
            ID_FILES,
            GLYPH_FILES,
            tr("Send files"),
            other && online,
        ),
        (
            cell(1),
            ID_TERMINAL,
            GLYPH_TERMINAL,
            tr("Command line"),
            other && online,
        ),
        (
            cell(2),
            ID_WAKE,
            GLYPH_WAKE,
            tr("Wake up"),
            other && !online,
        ),
        (cell(3), ID_MORE, GLYPH_MORE, tr("More"), other),
    ]
}

fn paint(hwnd: HWND, dc: HDC, state: &State) {
    let s = state.scale;
    let mut area = RECT::default();
    let _ = unsafe { GetClientRect(hwnd, &mut area) };
    fill(dc, &area, PAGE_COLOR);
    fill(dc, &rect(0, 0, SIDEBAR, HEIGHT, s), SIDE_BG);
    paint_sidebar(dc, state);
    match state.page {
        Page::Devices => paint_device_page(dc, state),
        page => {
            text(
                dc,
                page.title(),
                rect(LEFT, 24, CONTENT, 40, s),
                state.fonts.title,
                TEXT_COLOR,
                LINE,
            );
            text(
                dc,
                page.subtitle(),
                rect(LEFT, 64, CONTENT, 22, s),
                state.fonts.small,
                SECONDARY,
                LINE,
            );
            paint_page(dc, state);
        }
    }
}

/// The sidebar: the magpie and the name, the devices, the pages, the account.
fn paint_sidebar(dc: HDC, state: &State) {
    let s = state.scale;
    let f = &state.fonts;
    rounded(dc, &rect(16, 10, 36, 36, s), px(12, s), BADGE);
    unsafe {
        let _ = DrawIconEx(
            dc,
            px(20, s),
            px(14, s),
            state.icon,
            px(28, s),
            px(28, s),
            0,
            None,
            DI_NORMAL,
        );
    }
    text(
        dc,
        tr("Queqiao"),
        rect(56, 12, SIDEBAR - 70, 32, s),
        f.app,
        WHITE,
        LINE,
    );
    // The devices: a label (a click shows the chosen device's page), the list below it.
    text(
        dc,
        tr("My devices"),
        rect(22, DEVICES_LABEL_Y, SIDEBAR - 70, 20, s),
        f.small,
        SIDE_MUTED,
        LINE,
    );
    if !state.devices.is_empty() {
        text(
            dc,
            &state.devices.len().to_string(),
            rect(SIDEBAR - 60, DEVICES_LABEL_Y, 38, 20, s),
            f.small,
            SIDE_MUTED,
            DT_RIGHT.0 | DT_SINGLELINE.0 | DT_VCENTER.0,
        );
    } else {
        let note = if state.account == AccountState::LoggedOut {
            tr("Log in to see your devices.")
        } else {
            tr("No other devices yet.")
        };
        text(
            dc,
            note,
            rect(22, DEVICE_LIST[1] + 6, SIDEBAR - 44, 40, s),
            f.small,
            SIDE_TEXT,
            DT_LEFT.0 | DT_WORDBREAK.0,
        );
    }
    fill(dc, &rect(18, NAV_TOP - 12, SIDEBAR - 36, 1, s), SIDE_LINE);
    // The other pages.
    for (index, page) in Page::ALL.iter().enumerate().skip(1) {
        let y = NAV_TOP + (index as i32 - 1) * NAV_STEP;
        if *page == state.page {
            rounded(
                dc,
                &rect(10, y, SIDEBAR - 20, NAV_HEIGHT, s),
                px(8, s),
                SIDE_SELECTED,
            );
            rounded(dc, &rect(10, y + 9, 3, NAV_HEIGHT - 18, s), px(3, s), GOLD);
        }
        let color = if *page == state.page {
            WHITE
        } else {
            SIDE_TEXT
        };
        text(
            dc,
            page.glyph(),
            rect(26, y, 24, NAV_HEIGHT, s),
            f.glyph,
            color,
            LINE,
        );
        text(
            dc,
            page.title(),
            rect(58, y, SIDEBAR - 76, NAV_HEIGHT, s),
            f.body,
            color,
            LINE,
        );
    }
    // The account.
    let [ax, ay, aw, _] = ACCOUNT_AREA;
    fill(dc, &rect(ax + 6, ay - 8, aw - 12, 1, s), SIDE_LINE);
    let initial = state
        .user
        .chars()
        .next()
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_default();
    let avatar = rect(ax + 6, ay + 10, 36, 36, s);
    rounded(
        dc,
        &avatar,
        px(36, s),
        if state.user.is_empty() {
            SIDE_CHIP
        } else {
            GOLD
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
        if initial.is_empty() {
            SIDE_TEXT
        } else {
            ON_GOLD
        },
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
        rect(ax + 52, ay + 4, aw - 56, 20, s),
        f.body_bold,
        WHITE,
        LINE,
    );
    rounded(dc, &rect(ax + 52, ay + 29, 8, 8, s), px(8, s), color);
    text(
        dc,
        status,
        rect(ax + 64, ay + 24, aw - 68, 18, s),
        f.small,
        SIDE_TEXT,
        LINE,
    );
    text(
        dc,
        link,
        rect(ax + 52, ay + 41, aw - 56, 16, s),
        f.small,
        GOLD,
        LINE,
    );
}

/// The Devices page: the chosen device, the card that joins this PC to it, its tools, the latest connections.
fn paint_device_page(dc: HDC, state: &State) {
    let s = state.scale;
    let f = &state.fonts;
    if state.account == AccountState::LoggedOut {
        text(
            dc,
            tr("Log in to see your devices."),
            rect(LEFT, 92, CONTENT, 40, s),
            f.title,
            TEXT_COLOR,
            LINE,
        );
        text(
            dc,
            tr(
                "Your devices appear here once this PC is logged in to your account, and you can control them from here.",
            ),
            rect(LEFT, 136, CONTENT, 40, s),
            f.body,
            SECONDARY,
            DT_LEFT.0 | DT_WORDBREAK.0,
        );
        paint_go_button(dc, state);
        return;
    }
    let Some(device) = chosen(state) else {
        text(
            dc,
            tr("Devices"),
            rect(LEFT, 24, CONTENT, 40, s),
            f.title,
            TEXT_COLOR,
            LINE,
        );
        text(
            dc,
            tr("No devices yet: log in to this account on another PC, and it appears here."),
            rect(LEFT, 70, CONTENT, 40, s),
            f.body,
            SECONDARY,
            DT_LEFT.0 | DT_WORDBREAK.0,
        );
        return;
    };
    // The heading: a status pill and the name, a line about what can be done.
    let (label, (back, front)) = if device.this_pc {
        (tr("This PC"), PILL_SELF)
    } else if device.online {
        (tr("Online"), PILL_ON)
    } else {
        (tr("Offline"), PILL_OFF)
    };
    let pill_width = text_width(dc, label, f.small) + px(30, s);
    let pill = RECT {
        left: px(LEFT, s),
        top: px(30, s),
        right: px(LEFT, s) + pill_width,
        bottom: px(54, s),
    };
    rounded(dc, &pill, px(24, s), back);
    let marker = RECT {
        left: pill.left + px(10, s),
        top: pill.top + px(8, s),
        right: pill.left + px(18, s),
        bottom: pill.top + px(16, s),
    };
    rounded(dc, &marker, px(8, s), front);
    text(
        dc,
        label,
        RECT {
            left: pill.left + px(22, s),
            ..pill
        },
        f.small,
        front,
        LINE,
    );
    text(
        dc,
        &device.name,
        RECT {
            left: pill.right + px(12, s),
            top: px(22, s),
            right: px(LEFT + CONTENT, s),
            bottom: px(62, s),
        },
        f.title,
        TEXT_COLOR,
        LINE,
    );
    let line = if device.this_pc {
        tr("This PC: your other devices can control it.")
    } else if device.online {
        tr("Online: you can connect now.")
    } else {
        tr("Offline: a device of yours on its network can wake it.")
    };
    text(
        dc,
        line,
        rect(LEFT, 62, CONTENT, 22, s),
        f.small,
        SECONDARY,
        LINE,
    );

    paint_hero(dc, state, device);

    // The tools.
    outlined(dc, &rect_of(ACTIONS, s), px(12, s), WHITE, DIVIDER);
    for (index, (place, _, glyph, label, enabled)) in action_cells(state).into_iter().enumerate() {
        if index > 0 {
            fill(
                dc,
                &rect(place[0], place[1] + 12, 1, place[3] - 24, s),
                DIVIDER,
            );
        }
        let color = if enabled { TEXT_COLOR } else { DISABLED };
        let label_width = text_width(dc, label, f.body) + px(26, s);
        let cell = rect_of(place, s);
        let start = cell.left + ((cell.right - cell.left) - label_width) / 2;
        text(
            dc,
            glyph,
            RECT {
                left: start,
                right: start + px(20, s),
                ..cell
            },
            f.glyph,
            if enabled { ACCENT } else { DISABLED },
            LINE,
        );
        text(
            dc,
            label,
            RECT {
                left: start + px(26, s),
                right: cell.right,
                ..cell
            },
            f.body,
            color,
            LINE,
        );
    }

    // The latest connections to it from this PC.
    text(
        dc,
        tr("Recent connections"),
        rect(LEFT, RECENT_Y, CONTENT, 22, s),
        f.section,
        TEXT_COLOR,
        LINE,
    );
    let recent: Vec<&ConnectionRow> = state
        .connections
        .iter()
        .filter(|row| row.device_id == device.id)
        .take(RECENT_ROWS)
        .collect();
    if recent.is_empty() {
        let none = if device.this_pc {
            tr("Sessions from this PC to your other devices are listed on their pages.")
        } else {
            tr("No connections from this PC yet.")
        };
        text(
            dc,
            none,
            rect(LEFT, RECENT_Y + 28, CONTENT, 22, s),
            f.small,
            SECONDARY,
            LINE,
        );
    }
    for (index, row) in recent.into_iter().enumerate() {
        let y = RECENT_Y + 30 + index as i32 * RECENT_ROW;
        let place = rect(LEFT, y, CONTENT, RECENT_ROW - 6, s);
        rounded(dc, &place, px(8, s), CARD);
        text(
            dc,
            GLYPH_RECENT,
            rect(LEFT + 12, y, 20, RECENT_ROW - 6, s),
            f.glyph,
            SECONDARY,
            LINE,
        );
        text(
            dc,
            &device.name,
            rect(LEFT + 40, y, 220, RECENT_ROW - 6, s),
            f.body,
            TEXT_COLOR,
            LINE,
        );
        text(
            dc,
            &row.when,
            rect(LEFT + 270, y, 180, RECENT_ROW - 6, s),
            f.small,
            SECONDARY,
            LINE,
        );
        text(
            dc,
            &row.length,
            rect(LEFT + CONTENT - 132, y, 120, RECENT_ROW - 6, s),
            f.small,
            SECONDARY,
            DT_RIGHT.0 | DT_SINGLELINE.0 | DT_VCENTER.0,
        );
    }
}

/// The card: night sky, this PC on the left, the device on the right, joined by a dotted gold arch with three
/// magpies on it; the button below (Connect, or Wake up). For this PC itself, the PC alone.
fn paint_hero(dc: HDC, state: &State, device: &MainDevice) {
    let s = state.scale;
    let f = &state.fonts;
    let card = rect_of(HERO, s);
    unsafe {
        let region = CreateRoundRectRgn(
            card.left,
            card.top,
            card.right + 1,
            card.bottom + 1,
            px(16, s),
            px(16, s),
        );
        SelectClipRgn(dc, Some(region));
        let vertices = [
            TRIVERTEX {
                x: card.left,
                y: card.top,
                Red: HERO_TOP.0 << 8,
                Green: HERO_TOP.1 << 8,
                Blue: HERO_TOP.2 << 8,
                Alpha: 0,
            },
            TRIVERTEX {
                x: card.right,
                y: card.bottom,
                Red: HERO_BOTTOM.0 << 8,
                Green: HERO_BOTTOM.1 << 8,
                Blue: HERO_BOTTOM.2 << 8,
                Alpha: 0,
            },
        ];
        let mesh = GRADIENT_RECT {
            UpperLeft: 0,
            LowerRight: 1,
        };
        let _ = GradientFill(
            dc,
            &vertices,
            std::ptr::from_ref(&mesh).cast(),
            1,
            GRADIENT_FILL_RECT_V,
        );
        // Stars: the same scatter every time.
        let mut seed: u32 = 2_166_136_261;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        };
        for _ in 0..46 {
            let x = card.left + (next() % (card.right - card.left).max(1) as u32) as i32;
            let y = card.top + (next() % (card.bottom - card.top).max(1) as u32) as i32;
            let size = 1 + (next() % 3 == 0) as i32;
            let color = [0x00F5_EEEA, 0x00D6_C6C0, 0x0095_7F74][(next() % 3) as usize];
            fill(
                dc,
                &RECT {
                    left: x,
                    top: y,
                    right: x + size,
                    bottom: y + size,
                },
                color,
            );
        }
        SelectClipRgn(dc, None);
        let _ = DeleteObject(region.into());
    }
    let this_name = state
        .devices
        .iter()
        .find(|device| device.this_pc)
        .map_or(tr("This PC"), |device| device.name.as_str());
    let end = |x: i32, glyph_color: u32, name: &str, under: &str, name_color: u32| {
        text(
            dc,
            GLYPH_DEVICES,
            rect(x - 40, HERO[1] + 34, 80, 40, s),
            f.glyph_hero,
            glyph_color,
            CENTRED,
        );
        text(
            dc,
            name,
            rect(x - 80, HERO[1] + 76, 160, 20, s),
            f.body_bold,
            name_color,
            CENTRED,
        );
        text(
            dc,
            under,
            rect(x - 80, HERO[1] + 96, 160, 18, s),
            f.small,
            HERO_MUTED,
            CENTRED,
        );
    };
    if device.this_pc {
        end(
            HERO[0] + HERO[2] / 2,
            WHITE,
            this_name,
            tr("This PC"),
            WHITE,
        );
        text(
            dc,
            tr("Your other devices connect to it from their own Queqiao."),
            rect(HERO[0] + 20, HERO[1] + HERO[3] - 44, HERO[2] - 40, 22, s),
            f.small,
            HERO_MUTED,
            CENTRED,
        );
        return;
    }
    let (left_x, right_x) = (HERO[0] + 92, HERO[0] + HERO[2] - 92);
    end(left_x, WHITE, this_name, tr("This PC"), WHITE);
    let (device_color, status) = if device.online {
        (WHITE, tr("Online"))
    } else {
        (HERO_FAINT, tr("Offline"))
    };
    end(right_x, device_color, &device.name, status, device_color);
    // The arch: dotted, from beside one PC to beside the other, its top near the card's top.
    let (x0, y0) = (f64::from(left_x + 52), f64::from(HERO[1] + 60));
    let (x2, y2) = (f64::from(right_x - 52), y0);
    let (x1, y1) = ((x0 + x2) / 2.0, f64::from(HERO[1]) + 6.0);
    let at = |t: f64| {
        let u = 1.0 - t;
        (
            u * u * x0 + 2.0 * u * t * x1 + t * t * x2,
            u * u * y0 + 2.0 * u * t * y1 + t * t * y2,
        )
    };
    let arch = if device.online { GOLD } else { HERO_FAINT };
    let dots = 34;
    for k in 0..=dots {
        let (x, y) = at(f64::from(k) / f64::from(dots));
        let (x, y) = ((x * s).round() as i32, (y * s).round() as i32);
        let r = px(2, s).max(2);
        rounded(
            dc,
            &RECT {
                left: x - r / 2,
                top: y - r / 2,
                right: x + r / 2 + 1,
                bottom: y + r / 2 + 1,
            },
            r,
            arch,
        );
    }
    // Three magpies on the arch, wings spread along it, while the device is online (the bridge forms).
    for t in [0.3, 0.5, 0.7] {
        if !device.online {
            break;
        }
        let (x, y) = at(t);
        if let Some(bird) = state.bird {
            let size = px(BIRD, s);
            unsafe {
                let _ = DrawIconEx(
                    dc,
                    (x * s).round() as i32 - size / 2,
                    (y * s).round() as i32 - size / 2 - px(4, s),
                    bird,
                    size,
                    size,
                    0,
                    None,
                    DI_NORMAL,
                );
            }
            continue;
        }
        let (xa, ya) = at(t - 0.01);
        let (xb, yb) = at(t + 0.01);
        let angle = (yb - ya).atan2(xb - xa);
        // A bird as seen from below: wings out, a short tail; drawn in units of 1, scaled.
        let shape: [(f64, f64); 9] = [
            (-9.0, -1.5),
            (-3.5, -2.5),
            (-1.2, -5.0),
            (1.2, -5.0),
            (3.5, -2.5),
            (9.0, -1.5),
            (2.0, 1.0),
            (0.0, 6.5),
            (-2.0, 1.0),
        ];
        let size = 1.6;
        let points: Vec<POINT> = shape
            .iter()
            .map(|&(dx, dy)| {
                let (rx, ry) = (
                    dx * angle.cos() - dy * angle.sin(),
                    dx * angle.sin() + dy * angle.cos(),
                );
                POINT {
                    x: ((x + rx * size) * s).round() as i32,
                    y: ((y - 2.0 + ry * size) * s).round() as i32,
                }
            })
            .collect();
        unsafe {
            let brush = CreateSolidBrush(COLORREF(arch));
            let old_brush = SelectObject(dc, brush.into());
            let old_pen = SelectObject(dc, GetStockObject(NULL_PEN));
            let _ = Polygon(dc, &points);
            SelectObject(dc, old_pen);
            SelectObject(dc, old_brush);
            let _ = DeleteObject(brush.into());
        }
    }
    paint_go_button(dc, state);
}

/// The gold button of the Devices page (Connect, Wake up, or Log in).
fn paint_go_button(dc: HDC, state: &State) {
    let s = state.scale;
    if let Some((place, _, label)) = go_button(state) {
        let area = rect_of(place, s);
        rounded(dc, &area, px(38, s), GOLD);
        text(dc, label, area, state.fonts.body_bold, ON_GOLD, CENTRED);
    }
}

/// The Help, Transfers and Settings pages' own drawing (their controls draw themselves).
fn paint_page(dc: HDC, state: &State) {
    let s = state.scale;
    let f = &state.fonts;
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
            outlined(
                dc,
                &rect(LEFT, 128, CONTENT, 132, s),
                px(12, s),
                CARD,
                DIVIDER,
            );
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

/// A row of the sidebar's device list: its online dot, its name, "This PC" for this one; the chosen one lit.
fn draw_device(item: &DRAWITEMSTRUCT, state: &State) {
    let s = state.scale;
    let dc = item.hDC;
    let area = item.rcItem;
    fill(dc, &area, SIDE_BG);
    let Some(device) = state.devices.get(item.itemID as usize) else {
        return;
    };
    let f = &state.fonts;
    let lit = item.itemState.0 & ODS_SELECTED != 0 && state.page == Page::Devices;
    let picked = item.itemState.0 & ODS_SELECTED != 0;
    let row = RECT {
        left: area.left,
        top: area.top + px(2, s),
        right: area.right,
        bottom: area.bottom - px(2, s),
    };
    if picked {
        rounded(
            dc,
            &row,
            px(8, s),
            if lit { SIDE_SELECTED } else { SIDE_CHIP },
        );
        if lit {
            rounded(
                dc,
                &RECT {
                    left: row.left,
                    top: row.top + px(8, s),
                    right: row.left + px(3, s),
                    bottom: row.bottom - px(8, s),
                },
                px(3, s),
                GOLD,
            );
        }
    }
    let (x, y) = (area.left, area.top);
    let marker = RECT {
        left: x + px(14, s),
        top: y + px(14, s),
        right: x + px(22, s),
        bottom: y + px(22, s),
    };
    dot(
        dc,
        &marker,
        if device.online || device.this_pc {
            ONLINE
        } else {
            OFFLINE
        },
        if picked {
            if lit { SIDE_SELECTED } else { SIDE_CHIP }
        } else {
            SIDE_BG
        },
    );
    let mut name_right = area.right - px(10, s);
    if device.this_pc {
        let label = tr("This PC");
        let chip_width = text_width(dc, label, f.small) + px(14, s);
        let chip = RECT {
            left: area.right - px(8, s) - chip_width,
            top: y + px(9, s),
            right: area.right - px(8, s),
            bottom: y + px(27, s),
        };
        rounded(
            dc,
            &chip,
            px(18, s),
            if picked { SIDE_BG } else { SIDE_CHIP },
        );
        text(dc, label, chip, f.small, SIDE_TEXT, CENTRED);
        name_right = chip.left - px(6, s);
    }
    text(
        dc,
        &device.name,
        RECT {
            left: x + px(32, s),
            top: area.top,
            right: name_right,
            bottom: area.bottom,
        },
        f.body,
        if picked { WHITE } else { SIDE_TEXT },
        LINE,
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
    fill(
        dc,
        &RECT {
            left: x + px(58, s),
            top: area.bottom - 1,
            right: area.right,
            bottom: area.bottom,
        },
        DIVIDER,
    );
}

/// What a click at (x, y) (96-DPI units) does, if anything (the hand cursor shows over the same places).
fn hit(state: &State, x: i32, y: i32) -> Option<u16> {
    if x < SIDEBAR {
        if (DEVICES_LABEL_Y..DEVICES_LABEL_Y + 20).contains(&y) && !state.devices.is_empty() {
            return Some(ID_NAV);
        }
        for index in 1..4 {
            let top = NAV_TOP + (index - 1) * NAV_STEP;
            if (top..top + NAV_HEIGHT).contains(&y) {
                return Some(ID_NAV + index as u16);
            }
        }
        if inside(ACCOUNT_AREA, x, y) {
            return Some(ID_ACCOUNT);
        }
        return None;
    }
    if state.page != Page::Devices {
        return None;
    }
    if let Some((place, command, _)) = go_button(state)
        && inside(place, x, y)
    {
        return Some(command);
    }
    if state.account != AccountState::LoggedOut && chosen(state).is_some() {
        for (place, command, _, _, enabled) in action_cells(state) {
            if enabled && inside(place, x, y) {
                return Some(command);
            }
        }
    }
    None
}

/// The mouse position in 96-DPI units.
fn scaled_point(lparam: LPARAM, scale: f64) -> (i32, i32) {
    let (x, y) = (
        (lparam.0 & 0xFFFF) as i16 as i32,
        ((lparam.0 >> 16) & 0xFFFF) as i16 as i32,
    );
    ((f64::from(x) / scale) as i32, (f64::from(y) / scale) as i32)
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
            let command = STATE.with_borrow(|state| {
                let state = state.as_ref()?;
                let (x, y) = scaled_point(lparam, state.scale);
                hit(state, x, y)
            });
            if let Some(command) = command {
                push(command);
            }
            LRESULT(0)
        }
        WM_SETCURSOR => {
            // The hand over what can be clicked.
            let mut point = POINT::default();
            unsafe {
                let _ = GetCursorPos(&mut point);
                let _ = windows::Win32::Graphics::Gdi::ScreenToClient(hwnd, &mut point);
            }
            let clickable = STATE.with_borrow(|state| {
                state.as_ref().is_some_and(|state| {
                    let (x, y) = (
                        (f64::from(point.x) / state.scale) as i32,
                        (f64::from(point.y) / state.scale) as i32,
                    );
                    hit(state, x, y).is_some()
                })
            });
            if wparam.0 == hwnd.0 as usize && clickable {
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
                (ID_DEVICES, LBN_SELCHANGE) => push(ID_SELECT),
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
            LRESULT(brush(PAGE_COLOR).0 as isize)
        },
        WM_CTLCOLORLISTBOX => {
            // The device list on the sidebar's blue, the transfers on the page's white.
            let list = HWND(lparam.0 as *mut c_void);
            let id = unsafe { GetDlgCtrlID(list) };
            let color = if id == i32::from(ID_DEVICES) {
                SIDE_BG
            } else {
                PAGE_COLOR
            };
            LRESULT(brush(color).0 as isize)
        }
        WM_CLOSE => {
            push(ID_CLOSE);
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

/// A brush of the page's white or the sidebar's blue, made once each (they live as long as the program).
fn brush(color: u32) -> HBRUSH {
    use std::sync::OnceLock;
    static PAGE: OnceLock<usize> = OnceLock::new();
    static SIDE: OnceLock<usize> = OnceLock::new();
    let slot = if color == SIDE_BG { &SIDE } else { &PAGE };
    HBRUSH(
        *slot.get_or_init(|| unsafe { CreateSolidBrush(COLORREF(color)) }.0 as usize)
            as *mut c_void,
    )
}
