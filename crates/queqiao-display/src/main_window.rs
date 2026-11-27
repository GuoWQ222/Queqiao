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

use queqiao_core::text::tr;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{DWMWINDOWATTRIBUTE, DwmSetWindowAttribute};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, CreateCompatibleBitmap,
    CreateCompatibleDC, CreateFontW, CreatePen, CreateRoundRectRgn, CreateSolidBrush,
    DEFAULT_CHARSET, DT_CENTER, DT_END_ELLIPSIS, DT_LEFT, DT_NOPREFIX, DT_RIGHT, DT_SINGLELINE,
    DT_VCENTER, DT_WORDBREAK, DeleteDC, DeleteObject, DrawTextW, Ellipse, EndPaint, FF_DONTCARE,
    FillRect, GRADIENT_FILL_RECT_V, GRADIENT_RECT, GetDC, GetStockObject, GradientFill, HBRUSH,
    HDC, HFONT, IntersectClipRect, InvalidateRect, NULL_PEN, OUT_DEFAULT_PRECIS, PAINTSTRUCT,
    PS_SOLID, Polygon, ReleaseDC, RoundRect, SRCCOPY, SelectClipRgn, SelectObject, SetBkMode,
    SetTextColor, TRANSPARENT, TRIVERTEX, UpdateWindow,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::{DRAWITEMSTRUCT, MEASUREITEMSTRUCT};
use windows::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForWindow};
use windows::Win32::UI::Input::KeyboardAndMouse::SetFocus;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, Result, w};

use crate::model::AccountState;
use crate::model::{ConnectionRow, MainDevice, Page, TransferRow};
use crate::settings::{Control, Row, Shown, Tab};

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
pub(crate) const WHITE: u32 = 0x00FF_FFFF;
const PAGE_COLOR: u32 = WHITE;
pub(crate) const TEXT_COLOR: u32 = 0x0040_211B;
pub(crate) const SECONDARY: u32 = 0x008C_6B64;
const DISABLED: u32 = 0x00CC_B9B4;
pub(crate) const DIVIDER: u32 = 0x00F2_E9E6;
const CARD: u32 = 0x00FA_F6F5;
pub(crate) const ACCENT: u32 = 0x00AE_5F3D;
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
/// The "being controlled" banner's Disconnect (ends the session another device has with that PC).
const ID_END_CONTROL: u16 = 340;
const ID_CLOSE: u16 = 0xFFFF;

// Windows 11's title bar colours (dwmapi.h); earlier Windows ignore them.
const DWMWA_USE_IMMERSIVE_DARK_MODE: DWMWINDOWATTRIBUTE = DWMWINDOWATTRIBUTE(20);
const DWMWA_BORDER_COLOR: DWMWINDOWATTRIBUTE = DWMWINDOWATTRIBUTE(34);
const DWMWA_CAPTION_COLOR: DWMWINDOWATTRIBUTE = DWMWINDOWATTRIBUTE(35);
const DWMWA_TEXT_COLOR: DWMWINDOWATTRIBUTE = DWMWINDOWATTRIBUTE(36);

/// The glyph (Segoe MDL2 Assets) of each page in the navigation.
fn glyph(page: Page) -> &'static str {
    match page {
        Page::Devices => GLYPH_DEVICES,
        Page::Help => GLYPH_HELP,
        Page::Transfers => GLYPH_TRANSFERS,
        Page::Settings => GLYPH_SETTINGS,
    }
}

/// What the person did in the main window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum MainAction {
    /// A setting or a menu command (a tray menu id: `Presence::choose`).
    Choose(u16),
    Connect(String),
    /// Connect, taking control from the device that has it (the person said Take Control).
    TakeOver(String),
    /// The banner's Disconnect: the session another device has with that PC ends.
    EndControl(String),
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
    icon: HICON,
    /// The magpie with its wings spread, for the card's bridge (None in a program without it).
    bird: Option<HICON>,
    /// The Settings page: what it shows, its tab, how far its cards are scrolled (96-DPI units).
    settings: Shown,
    tab: Tab,
    scroll: i32,
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
    small_glyph: HFONT,
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
}

impl MainWindow {
    /// The window, hidden (`show` brings it up).
    pub(crate) fn new() -> Result<MainWindow> {
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
            small_glyph: font(11, 400, w!("Segoe MDL2 Assets"), scale),
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
                icon: crate::icon::app_icon(px(28, scale)),
                bird: crate::icon::spread_magpie(px(BIRD, scale)),
                settings: Shown::default(),
                tab: Tab::default(),
                scroll: 0,
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
                crate::tray::MENU_HELP,
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
                crate::tray::MENU_FIXED_CODE,
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

        // Settings: painted (tabs and cards, settings.rs), no controls of their own.

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
        crate::forms::set_text(
            self.fixed_button,
            if shown.fixed_code {
                tr("Remove the fixed help code")
            } else {
                tr("Fixed help code...")
            },
        );
        STATE.with_borrow_mut(|state| {
            if let Some(state) = state {
                state.settings = shown;
            }
        });
        let _ = unsafe { InvalidateRect(Some(self.hwnd), None, false) };
    }

    /// Tests: on each tab, whether a click at the middle of each control gives that control's command.
    pub(crate) fn check_settings_clicks(&mut self) -> String {
        let mut checked = 0;
        let mut wrong = Vec::new();
        for tab in Tab::ALL {
            self.set_tab(tab);
            STATE.with_borrow(|state| {
                let Some(state) = state.as_ref() else {
                    return;
                };
                for (place, id) in cards(state).into_iter().flat_map(|card| card.hits) {
                    let (x, y) = (place[0] + place[2] / 2, place[1] + place[3] / 2);
                    if (CARDS_TOP..CARDS_BOTTOM).contains(&y) {
                        checked += 1;
                        if settings_hit(state, x, y).map(|(hit, _)| hit) != Some(id) {
                            wrong.push(id);
                        }
                    }
                }
            });
        }
        self.set_tab(Tab::General);
        let verdict = if wrong.is_empty() {
            "yes".to_string()
        } else {
            format!("no ({wrong:?})")
        };
        format!("{checked} controls answer clicks where they are drawn: {verdict}")
    }

    /// Tests: a click on control `id` (its tab chosen first), as the mouse gives it to the window; false if no tab
    /// has it.
    pub(crate) fn click_setting_for_test(&mut self, id: u16) -> bool {
        self.set_page(Page::Settings);
        for tab in Tab::ALL {
            self.set_tab(tab);
            let point = STATE.with_borrow(|state| {
                let state = state.as_ref()?;
                let place = cards(state)
                    .into_iter()
                    .flat_map(|card| card.hits)
                    .find(|(_, hit)| *hit == id)
                    .map(|(place, _)| place)?;
                let x = px(place[0] + place[2] / 2, state.scale);
                let y = px(place[1] + place[3] / 2, state.scale);
                Some(((y as u32 & 0xFFFF) << 16) | (x as u32 & 0xFFFF))
            });
            if let Some(point) = point {
                unsafe {
                    SendMessageW(self.hwnd, WM_LBUTTONUP, None, Some(LPARAM(point as isize)));
                }
                return true;
            }
        }
        false
    }

    /// The Settings page's tab (tests: a picture of each).
    pub(crate) fn set_tab(&mut self, tab: Tab) {
        STATE.with_borrow_mut(|state| {
            if let Some(state) = state {
                state.tab = tab;
                state.scroll = 0;
            }
        });
        let _ = unsafe { InvalidateRect(Some(self.hwnd), None, false) };
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
            id if (ID_TAB..ID_TAB + Tab::ALL.len() as u16).contains(&id) => {
                self.set_tab(Tab::ALL[usize::from(id - ID_TAB)]);
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
            // Enter on the Devices page, a double click in the list, or the card's button. One device controls a PC at a
            // time (as UU Remote): with another device in control, the person is asked first.
            ID_CONNECT => {
                let id = self.selected_device()?;
                match self.selected_controller() {
                    Some(controller) => {
                        crate::forms::confirm_take_over_owned(Some(self.hwnd), &controller)
                            .then_some(MainAction::TakeOver(id))
                    }
                    None => Some(MainAction::Connect(id)),
                }
            }
            ID_END_CONTROL => STATE.with_borrow(|state| {
                let state = state.as_ref()?;
                let device = state.devices.get(state.selected?)?;
                (!device.controlled_by.is_empty())
                    .then(|| MainAction::EndControl(device.id.clone()))
            }),
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

    /// The device controlling the chosen one now, if any.
    fn selected_controller(&self) -> Option<String> {
        STATE.with_borrow(|state| {
            let state = state.as_ref()?;
            let device = state.devices.get(state.selected?)?;
            (!device.controlled_by.is_empty()).then(|| device.controlled_by.clone())
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
        // Each further tab of Settings: main_settings_security.bmp ... (main_settings.bmp is its first).
        self.set_page(Page::Settings);
        for tab in &Tab::ALL[1..] {
            self.set_tab(*tab);
            unsafe {
                let _ = UpdateWindow(self.hwnd);
            }
            crate::forms::pump(50);
            let name = format!("main_settings_{}.bmp", format!("{tab:?}").to_lowercase());
            crate::forms::save_window_picture(self.hwnd, &folder.join(name))?;
        }
        self.set_tab(Tab::General);
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
                    f.small_glyph,
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

pub(crate) fn px(value: i32, scale: f64) -> i32 {
    (f64::from(value) * scale).round() as i32
}

pub(crate) fn font(size: i32, weight: i32, face: PCWSTR, scale: f64) -> HFONT {
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

pub(crate) fn rect(x: i32, y: i32, width: i32, height: i32, scale: f64) -> RECT {
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

pub(crate) fn fill(dc: HDC, area: &RECT, color: u32) {
    unsafe {
        let brush = CreateSolidBrush(COLORREF(color));
        FillRect(dc, area, brush);
        let _ = DeleteObject(brush.into());
    }
}

/// A rounded rectangle (or a circle, with `round` as large as it) filled with `color`.
pub(crate) fn rounded(dc: HDC, area: &RECT, round: i32, color: u32) {
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
pub(crate) fn outlined(dc: HDC, area: &RECT, round: i32, color: u32, border: u32) {
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

pub(crate) fn text(dc: HDC, value: &str, area: RECT, font: HFONT, color: u32, flags: u32) {
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
pub(crate) fn text_width(dc: HDC, value: &str, font: HFONT) -> i32 {
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

/// How tall `value` is in `font`, wrapped to `width` pixels.
pub(crate) fn wrapped_height(dc: HDC, value: &str, font: HFONT, width: i32) -> i32 {
    if value.is_empty() {
        return 0;
    }
    let mut value: Vec<u16> = value.encode_utf16().collect();
    let mut area = RECT {
        right: width,
        ..RECT::default()
    };
    unsafe {
        let old = SelectObject(dc, font.into());
        DrawTextW(
            dc,
            &mut value,
            &mut area,
            windows::Win32::Graphics::Gdi::DRAW_TEXT_FORMAT(
                DT_CALCRECT_FLAG | DT_WORDBREAK.0 | DT_LEFT.0 | DT_NOPREFIX.0,
            ),
        );
        SelectObject(dc, old);
    }
    area.bottom - area.top
}

const DT_CALCRECT_FLAG: u32 = 0x400;
pub(crate) const LINE: u32 = DT_LEFT.0 | DT_SINGLELINE.0 | DT_VCENTER.0 | DT_END_ELLIPSIS.0;
pub(crate) const CENTRED: u32 = DT_CENTER.0 | DT_SINGLELINE.0 | DT_VCENTER.0 | DT_END_ELLIPSIS.0;

/// The chosen device, from the state.
fn chosen(state: &State) -> Option<&MainDevice> {
    state.devices.get(state.selected?)
}

/// Under the heading while a device of the account controls the chosen one (UU Remote's "被控中", the user,
/// 2026-11-26): the banner, its Disconnect, and how far it moves the page's parts below it down.
const BANNER: [i32; 4] = [LEFT, 90, CONTENT, 46];
const BANNER_END: [i32; 4] = [LEFT + CONTENT - 114, 99, 102, 28];
const BANNER_SPACE: i32 = 58;
/// A pale blue band, its words in the accent blue (0x00BBGGRR).
const BANNER_BACK: u32 = 0x00FE_F2EA;
const BANNER_EDGE: u32 = 0x00F6_DCC6;
/// Segoe MDL2 Assets' screen.
const GLYPH_CONTROLLED: &str = "\u{E7F4}";

/// The chosen device, when another device controls it.
fn controlled(state: &State) -> Option<&MainDevice> {
    if state.account == AccountState::LoggedOut {
        return None;
    }
    chosen(state).filter(|device| !device.controlled_by.is_empty())
}

fn below_banner(state: &State) -> i32 {
    if controlled(state).is_some() {
        BANNER_SPACE
    } else {
        0
    }
}

/// The card, the tools under it and the recent connections: lower while the banner shows.
fn hero(state: &State) -> [i32; 4] {
    [HERO[0], HERO[1] + below_banner(state), HERO[2], HERO[3]]
}

fn actions(state: &State) -> [i32; 4] {
    [
        ACTIONS[0],
        ACTIONS[1] + below_banner(state),
        ACTIONS[2],
        ACTIONS[3],
    ]
}

fn recent_y(state: &State) -> i32 {
    RECENT_Y + below_banner(state)
}

/// The "being controlled" banner: a screen, the label, a divider, the sentence, Disconnect on the right.
fn paint_banner(dc: HDC, state: &State) {
    if controlled(state).is_none() {
        return;
    }
    let s = state.scale;
    let f = &state.fonts;
    let band = rect_of(BANNER, s);
    outlined(dc, &band, px(10, s), BANNER_BACK, BANNER_EDGE);
    let (label, words) = crate::model::controlled_banner();
    text(
        dc,
        GLYPH_CONTROLLED,
        rect(LEFT + 16, BANNER[1], 22, BANNER[3], s),
        f.glyph,
        ACCENT,
        LINE,
    );
    let label_left = px(LEFT + 46, s);
    let label_right = label_left + text_width(dc, label, f.body_bold);
    text(
        dc,
        label,
        RECT {
            left: label_left,
            right: label_right,
            ..band
        },
        f.body_bold,
        ACCENT,
        LINE,
    );
    fill(
        dc,
        &RECT {
            left: label_right + px(12, s),
            top: band.top + px(14, s),
            right: label_right + px(12, s) + px(1, s).max(1),
            bottom: band.bottom - px(14, s),
        },
        BANNER_EDGE,
    );
    let end = rect_of(BANNER_END, s);
    // The sentence on one line where it fits (Chinese), else two, centred in the band.
    let (left, right) = (label_right + px(26, s), end.left - px(10, s));
    let height = wrapped_height(dc, words, f.small, right - left).min(band.bottom - band.top);
    let top = band.top + ((band.bottom - band.top) - height) / 2;
    text(
        dc,
        words,
        RECT {
            left,
            top,
            right,
            bottom: top + height,
        },
        f.small,
        ACCENT,
        DT_LEFT.0 | DT_WORDBREAK.0 | DT_END_ELLIPSIS.0,
    );
    outlined(dc, &end, px(6, s), WHITE, BANNER_EDGE);
    text(dc, tr("Disconnect"), end, f.small, TEXT_COLOR, CENTRED);
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
    let hero_at = hero(state);
    let place = [
        hero_at[0] + hero_at[2] / 2 - 70,
        hero_at[1] + hero_at[3] - 54,
        140,
        38,
    ];
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
    let row = actions(state);
    let width = row[2] / 4;
    let cell = |index: i32| [row[0] + index * width, row[1], width, row[3]];
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
        Page::Settings => {
            text(
                dc,
                Page::Settings.title(),
                rect(LEFT, 20, CONTENT, 40, s),
                state.fonts.title,
                TEXT_COLOR,
                LINE,
            );
            paint_settings(dc, state);
        }
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
            glyph(*page),
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

    paint_banner(dc, state);
    paint_hero(dc, state, device);

    // The tools.
    outlined(dc, &rect_of(actions(state), s), px(12, s), WHITE, DIVIDER);
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
    let recent_top = recent_y(state);
    text(
        dc,
        tr("Recent connections"),
        rect(LEFT, recent_top, CONTENT, 22, s),
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
            rect(LEFT, recent_top + 28, CONTENT, 22, s),
            f.small,
            SECONDARY,
            LINE,
        );
    }
    for (index, row) in recent.into_iter().enumerate() {
        let y = recent_top + 30 + index as i32 * RECENT_ROW;
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
    // Under the banner while another device controls it.
    let hero_at = hero(state);
    let card = rect_of(hero_at, s);
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
            rect(x - 40, hero_at[1] + 34, 80, 40, s),
            f.glyph_hero,
            glyph_color,
            CENTRED,
        );
        text(
            dc,
            name,
            rect(x - 80, hero_at[1] + 76, 160, 20, s),
            f.body_bold,
            name_color,
            CENTRED,
        );
        text(
            dc,
            under,
            rect(x - 80, hero_at[1] + 96, 160, 18, s),
            f.small,
            HERO_MUTED,
            CENTRED,
        );
    };
    if device.this_pc {
        end(
            hero_at[0] + hero_at[2] / 2,
            WHITE,
            this_name,
            tr("This PC"),
            WHITE,
        );
        text(
            dc,
            tr("Your other devices connect to it from their own Queqiao."),
            rect(
                hero_at[0] + 20,
                hero_at[1] + hero_at[3] - 44,
                hero_at[2] - 40,
                22,
                s,
            ),
            f.small,
            HERO_MUTED,
            CENTRED,
        );
        return;
    }
    let (left_x, right_x) = (hero_at[0] + 92, hero_at[0] + hero_at[2] - 92);
    end(left_x, WHITE, this_name, tr("This PC"), WHITE);
    let (device_color, status) = if device.online {
        (WHITE, tr("Online"))
    } else {
        (HERO_FAINT, tr("Offline"))
    };
    end(right_x, device_color, &device.name, status, device_color);
    // The arch: dotted, from beside one PC to beside the other, its top near the card's top.
    let (x0, y0) = (f64::from(left_x + 52), f64::from(hero_at[1] + 60));
    let (x2, y2) = (f64::from(right_x - 52), y0);
    let (x1, y1) = ((x0 + x2) / 2.0, f64::from(hero_at[1]) + 6.0);
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
        Page::Devices | Page::Transfers | Page::Settings => {}
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
    if !device.controlled_by.is_empty() {
        // Being controlled (as UU Remote's list): a screen and the one device controlling it.
        let chip = RECT {
            left: name_right - px(32, s),
            top: y + px(9, s),
            right: name_right,
            bottom: y + px(27, s),
        };
        rounded(
            dc,
            &chip,
            px(8, s),
            if picked { SIDE_BG } else { SIDE_CHIP },
        );
        text(
            dc,
            GLYPH_CONTROLLED,
            RECT {
                right: chip.left + px(19, s),
                ..chip
            },
            f.small_glyph,
            GOLD,
            CENTRED,
        );
        text(
            dc,
            "1",
            RECT {
                left: chip.left + px(16, s),
                ..chip
            },
            f.small,
            SIDE_TEXT,
            CENTRED,
        );
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
    if state.page == Page::Settings {
        return settings_hit(state, x, y).map(|(id, _)| id);
    }
    if state.page != Page::Devices {
        return None;
    }
    if controlled(state).is_some() && inside(BANNER_END, x, y) {
        return Some(ID_END_CONTROL);
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
            match command {
                // A card's choice: its menu, then the option chosen.
                Some(command) if (ID_CHOICE..ID_CHOICE + 32).contains(&command) => {
                    let chosen = STATE.with_borrow(|state| {
                        let state = state.as_ref()?;
                        let (x, y) = scaled_point(lparam, state.scale);
                        let (_, place) = settings_hit(state, x, y)?;
                        choose_from(hwnd, state, usize::from(command - ID_CHOICE), place)
                    });
                    if let Some(chosen) = chosen {
                        push(chosen);
                    }
                }
                Some(command) => push(command),
                None => {}
            }
            LRESULT(0)
        }
        // The Settings page's cards scroll.
        WM_MOUSEWHEEL => {
            let delta = i32::from(((wparam.0 >> 16) & 0xFFFF) as u16 as i16);
            let scrolled = STATE.with_borrow_mut(|state| {
                let state = state.as_mut()?;
                if state.page != Page::Settings {
                    return None;
                }
                let most = max_scroll(state);
                let scroll = (state.scroll - delta * 56 / 120).clamp(0, most);
                (scroll != state.scroll).then(|| state.scroll = scroll)
            });
            if scrolled.is_some() {
                let _ = unsafe { InvalidateRect(Some(hwnd), None, false) };
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

// The Settings page (96-DPI units): the tabs under the title, then the cards, scrolled between CARDS_TOP and
// CARDS_BOTTOM.
const TABS_Y: i32 = 66;
const TAB_GAP: i32 = 30;
const CARDS_TOP: i32 = 112;
const CARDS_BOTTOM: i32 = HEIGHT - 12;
const CARD_GAP: i32 = 10;
/// A tab (+ its index).
const ID_TAB: u16 = 340;
/// A card's choice (+ the card's index): its menu opens.
const ID_CHOICE: u16 = 350;
const SWITCH_ON: u32 = ACCENT;
const SWITCH_OFF: u32 = 0x00C9_BBB6;
const BUTTON_BORDER: u32 = 0x00DA_CFCB;
const KEY_CHIP: (u32, u32) = (0x00FB_EDE8, ACCENT);

/// One card on the page: its row, its place (as scrolled), and what a click on each of its controls sends.
struct Card {
    row: Row,
    top: i32,
    height: i32,
    hits: Vec<([i32; 4], u16)>,
}

/// Each tab's place along the tab line: (tab, x, width).
fn tab_places(state: &State) -> Vec<(Tab, i32, i32)> {
    let dc = unsafe { GetDC(None) };
    let mut x = LEFT;
    let places = Tab::ALL
        .iter()
        .map(|tab| {
            let width = unscaled(
                text_width(dc, tab.title(), state.fonts.section),
                state.scale,
            );
            let place = (*tab, x, width);
            x += width + TAB_GAP;
            place
        })
        .collect();
    unsafe { ReleaseDC(None, dc) };
    places
}

/// A width measured on the screen, in 96-DPI units.
fn unscaled(width: i32, scale: f64) -> i32 {
    (f64::from(width) / scale).ceil() as i32
}

/// The cards of the chosen tab, placed as scrolled.
fn cards(state: &State) -> Vec<Card> {
    let rows = crate::settings::rows(state.tab, &state.settings);
    let dc = unsafe { GetDC(None) };
    let right = LEFT + CONTENT - 18;
    let mut y = CARDS_TOP - state.scroll;
    let mut cards = Vec::new();
    for (index, row) in rows.into_iter().enumerate() {
        if index > 0 && !row.sub {
            y += CARD_GAP;
        }
        let height = if row.detail.is_empty() { 52 } else { 68 };
        let middle = y + height / 2;
        let mut hits = Vec::new();
        match &row.control {
            // The switch and its "On" / "Off".
            Control::Switch { id, .. } => hits.push(([right - 80, middle - 13, 80, 26], *id)),
            Control::Choice { .. } => {
                hits.push((
                    [right - 200, middle - 16, 200, 32],
                    ID_CHOICE + index as u16,
                ));
            }
            Control::Buttons(buttons) => {
                let mut x = right;
                for (id, label) in buttons.iter().rev() {
                    let width = unscaled(text_width(dc, label, state.fonts.body), state.scale) + 32;
                    x -= width;
                    hits.push(([x, middle - 16, width, 32], *id));
                    x -= 10;
                }
            }
            Control::Keys { id, .. } => hits.push(([right - 30, middle - 15, 30, 30], *id)),
        }
        cards.push(Card {
            row,
            top: y,
            height,
            hits,
        });
        y += height;
    }
    unsafe { ReleaseDC(None, dc) };
    cards
}

/// How far the cards can be scrolled.
fn max_scroll(state: &State) -> i32 {
    let cards = cards(state);
    let bottom = cards
        .last()
        .map_or(0, |card| card.top + card.height + state.scroll);
    (bottom - CARDS_BOTTOM + 8).max(0)
}

/// What a click at (x, y) on the Settings page sends, and the place clicked.
fn settings_hit(state: &State, x: i32, y: i32) -> Option<(u16, [i32; 4])> {
    if (TABS_Y..TABS_Y + 36).contains(&y) {
        return tab_places(state)
            .into_iter()
            .enumerate()
            .find(|(_, (_, left, width))| (*left - 8..*left + width + 8).contains(&x))
            .map(|(index, (_, left, width))| (ID_TAB + index as u16, [left, TABS_Y, width, 36]));
    }
    if !(CARDS_TOP..CARDS_BOTTOM).contains(&y) {
        return None;
    }
    cards(state)
        .into_iter()
        .flat_map(|card| card.hits)
        .find(|(place, _)| inside(*place, x, y))
        .map(|(place, id)| (id, place))
}

fn paint_settings(dc: HDC, state: &State) {
    let s = state.scale;
    let f = &state.fonts;
    // The tabs, the chosen one dark with a short gold line under it.
    for (tab, x, width) in tab_places(state) {
        let chosen = tab == state.tab;
        text(
            dc,
            tab.title(),
            rect(x, TABS_Y, width + 4, 30, s),
            f.section,
            if chosen { TEXT_COLOR } else { SECONDARY },
            LINE,
        );
        if chosen {
            rounded(
                dc,
                &rect(x + width / 2 - 12, TABS_Y + 32, 24, 4, s),
                px(4, s),
                GOLD,
            );
        }
    }
    fill(dc, &rect(LEFT, CARDS_TOP - 6, CONTENT, 1, s), DIVIDER);
    let cards = cards(state);
    unsafe {
        let _ = IntersectClipRect(
            dc,
            px(LEFT - 2, s),
            px(CARDS_TOP - 2, s),
            px(LEFT + CONTENT + 2, s),
            px(CARDS_BOTTOM, s),
        );
    }
    // Each card with the ones that belong to it, as one rounded panel; a line between them.
    let mut index = 0;
    while index < cards.len() {
        let mut end = index + 1;
        while end < cards.len() && cards[end].row.sub {
            end += 1;
        }
        let top = cards[index].top;
        let bottom = cards[end - 1].top + cards[end - 1].height;
        outlined(
            dc,
            &rect(LEFT, top, CONTENT, bottom - top, s),
            px(12, s),
            WHITE,
            DIVIDER,
        );
        for card in &cards[index..end] {
            if card.row.sub {
                fill(dc, &rect(LEFT + 56, card.top, CONTENT - 74, 1, s), DIVIDER);
            }
            paint_card(dc, state, card);
        }
        index = end;
    }
    unsafe {
        let _ = SelectClipRgn(dc, None);
    }
    // A thin bar on the right while there is more to scroll to.
    let most = max_scroll(state);
    if most > 0 {
        let track = CARDS_BOTTOM - CARDS_TOP;
        let thumb = (track * track / (track + most)).max(24);
        let at = CARDS_TOP + (track - thumb) * state.scroll / most;
        rounded(
            dc,
            &rect(LEFT + CONTENT + 8, at, 4, thumb, s),
            px(4, s),
            SWITCH_OFF,
        );
    }
}

fn paint_card(dc: HDC, state: &State, card: &Card) {
    let s = state.scale;
    let f = &state.fonts;
    let (top, height) = (card.top, card.height);
    let middle = top + height / 2;
    let right = LEFT + CONTENT - 18;
    // Where the control begins: the title and its line stop before it.
    let controls_left = card
        .hits
        .iter()
        .map(|(place, _)| place[0])
        .min()
        .unwrap_or(right - 140);
    if !card.row.glyph.is_empty() {
        text(
            dc,
            card.row.glyph,
            rect(LEFT + 18, top, 24, height, s),
            f.glyph,
            TEXT_COLOR,
            DT_CENTER.0 | DT_SINGLELINE.0 | DT_VCENTER.0,
        );
    }
    let title_width = (controls_left - 16 - (LEFT + 56)).max(80);
    if card.row.detail.is_empty() {
        text(
            dc,
            &card.row.title,
            rect(LEFT + 56, top, title_width, height, s),
            f.body,
            TEXT_COLOR,
            LINE,
        );
    } else {
        text(
            dc,
            &card.row.title,
            rect(LEFT + 56, top + 13, title_width, 22, s),
            f.body,
            TEXT_COLOR,
            LINE,
        );
        text(
            dc,
            &card.row.detail,
            rect(LEFT + 56, top + 36, title_width, 20, s),
            f.small,
            SECONDARY,
            LINE,
        );
    }
    match &card.row.control {
        Control::Switch { on, .. } => {
            text(
                dc,
                if *on { tr("On") } else { tr("Off") },
                rect(right - 84, middle - 10, 32, 20, s),
                f.small,
                SECONDARY,
                DT_RIGHT.0 | DT_SINGLELINE.0 | DT_VCENTER.0,
            );
            let track = rect(right - 44, middle - 12, 44, 24, s);
            if *on {
                rounded(dc, &track, px(24, s), SWITCH_ON);
            } else {
                outlined(dc, &track, px(24, s), WHITE, SWITCH_OFF);
            }
            let knob_x = if *on { right - 22 } else { right - 40 };
            rounded(
                dc,
                &rect(knob_x, middle - 8, 16, 16, s),
                px(16, s),
                if *on { WHITE } else { SWITCH_OFF },
            );
        }
        Control::Choice { value, .. } => {
            let [x, y, width, box_height] = card.hits[0].0;
            outlined(
                dc,
                &rect(x, y, width, box_height, s),
                px(8, s),
                WHITE,
                BUTTON_BORDER,
            );
            text(
                dc,
                value,
                rect(x + 12, y, width - 40, box_height, s),
                f.body,
                TEXT_COLOR,
                LINE,
            );
            text(
                dc,
                "\u{E70D}",
                rect(x + width - 30, y, 20, box_height, s),
                f.small_glyph,
                SECONDARY,
                DT_CENTER.0 | DT_SINGLELINE.0 | DT_VCENTER.0,
            );
        }
        Control::Buttons(buttons) => {
            for ((place, _), (_, label)) in card.hits.iter().zip(buttons.iter().rev()) {
                let [x, y, width, button_height] = *place;
                outlined(
                    dc,
                    &rect(x, y, width, button_height, s),
                    px(8, s),
                    WHITE,
                    BUTTON_BORDER,
                );
                text(
                    dc,
                    label,
                    rect(x, y, width, button_height, s),
                    f.body,
                    TEXT_COLOR,
                    DT_CENTER.0 | DT_SINGLELINE.0 | DT_VCENTER.0,
                );
            }
        }
        Control::Keys { keys, .. } => {
            text(
                dc,
                "\u{E70F}",
                rect(right - 30, middle - 15, 30, 30, s),
                f.glyph,
                SECONDARY,
                DT_CENTER.0 | DT_SINGLELINE.0 | DT_VCENTER.0,
            );
            if keys.is_empty() {
                text(
                    dc,
                    tr("Not set"),
                    rect(right - 160, middle - 12, 120, 24, s),
                    f.small,
                    SECONDARY,
                    DT_RIGHT.0 | DT_SINGLELINE.0 | DT_VCENTER.0,
                );
            }
            let mut x = right - 38;
            for key in keys.iter().rev() {
                let width = (unscaled(text_width(dc, key, f.small), s) + 18).max(30);
                x -= width;
                rounded(
                    dc,
                    &rect(x, middle - 12, width, 24, s),
                    px(6, s),
                    KEY_CHIP.0,
                );
                text(
                    dc,
                    key,
                    rect(x, middle - 12, width, 24, s),
                    f.small,
                    KEY_CHIP.1,
                    DT_CENTER.0 | DT_SINGLELINE.0 | DT_VCENTER.0,
                );
                x -= 6;
            }
        }
    }
}

/// The menu of a card's choice, under its box: the option chosen (its id), if any.
fn choose_from(hwnd: HWND, state: &State, index: usize, place: [i32; 4]) -> Option<u16> {
    let rows = crate::settings::rows(state.tab, &state.settings);
    let Control::Choice { options, .. } = &rows.get(index)?.control else {
        return None;
    };
    unsafe {
        let menu = CreatePopupMenu().ok()?;
        for (id, label, chosen) in options {
            let label: Vec<u16> = label.encode_utf16().chain(Some(0)).collect();
            let flags = if *chosen {
                MF_STRING | MF_CHECKED
            } else {
                MF_STRING
            };
            let _ = AppendMenuW(menu, flags, usize::from(*id), PCWSTR(label.as_ptr()));
        }
        let mut corner = POINT {
            x: px(place[0], state.scale),
            y: px(place[1] + place[3] + 2, state.scale),
        };
        let _ = windows::Win32::Graphics::Gdi::ClientToScreen(hwnd, &mut corner);
        let chosen = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_LEFTALIGN | TPM_TOPALIGN,
            corner.x,
            corner.y,
            None,
            hwnd,
            None,
        );
        let _ = DestroyMenu(menu);
        u16::try_from(chosen.0).ok().filter(|id| *id != 0)
    }
}
