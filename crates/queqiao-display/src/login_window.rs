//! The login window on Windows, drawn as UU Remote's (the user, 2026-11-26: "redesign the login screen, using UU Remote
//! as a visual reference. Omit the QR code and add a copyright notice at the bottom. Use a single username and password
//! form for both sign-in and registration"): a pale blue sky fading to white, the magpie and Queqiao's name, the user
//! name and password on underlines, one button (a name nobody has yet makes the account: Account::log_in_or_create),
//! the server behind "Use another server..." when one is built in, the copyright at the foot. The same API as the plain
//! form before it, polled with `next`; Tab, Enter and Esc as in a dialog.

use std::cell::RefCell;
use std::ffi::c_void;
use std::path::Path;
use std::sync::Once;

use queqiao_core::text::tr;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, COLOR_WINDOW, DeleteObject, DrawFocusRect, EndPaint, GRADIENT_FILL_RECT_V,
    GRADIENT_RECT, GetMonitorInfoW, GetStockObject, GetSysColorBrush, GradientFill, HDC, HFONT,
    HGDIOBJ, InvalidateRect, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow, PAINTSTRUCT,
    SetBkColor, SetTextColor, TRIVERTEX, WHITE_BRUSH,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::DRAWITEMSTRUCT;
use windows::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForWindow};
use windows::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, IsWindowEnabled, SetFocus};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, Result, w};

use crate::forms::{
    CLOSED, ID_CANCEL, push_action, save_window_picture, set_text, take_action, text_of,
};
use crate::main_window::{
    ACCENT, CENTRED, DIVIDER, SECONDARY, TEXT_COLOR, WHITE, fill, font, px, rect, rounded, text,
    text_width,
};
use crate::model::{LoginAction, LoginFields};

const CLASS: PCWSTR = w!("QueqiaoLogin");
static REGISTER: Once = Once::new();

/// The drawing area at 96 DPI and the fields' column.
const WIDTH: i32 = 420;
const HEIGHT: i32 = 490;
const LEFT: i32 = 60;
const FIELD: i32 = 300;
/// The sky's height: pale blue at the top, white from here down, above the first field (Windows' edit boxes are white:
/// on the sky they showed as white boxes, 2026-11-26).
const SKY_HEIGHT: i32 = 118;

/// Command ids: IDOK is what Enter sends.
const ID_OK: u16 = 1;
const ID_OTHER_SERVER: u16 = 3;
const ID_USER: u16 = 10;
const ID_PASSWORD: u16 = 11;
const ID_SERVER: u16 = 12;
const ID_PIN: u16 = 13;
const ID_STATUS: u16 = 14;

/// The sky's colour (0x00BBGGRR), the button's when it cannot be pressed, the copyright's.
const SKY: u32 = 0x00FE_EFE3;
const BUTTON_OFF: u32 = 0x00D8_CFC8;
const FOOT: u32 = 0x00B4_A9A4;

// Control styles and notifications (winuser.h) as plain numbers, as in forms.rs.
const ES_AUTOHSCROLL: u32 = 0x80;
const ES_PASSWORD: u32 = 0x20;
const BS_OWNERDRAW: u32 = 0xB;
const SS_NOPREFIX: u32 = 0x80;
const SS_CENTER: u32 = 0x1;
const EN_SETFOCUS: u16 = 0x0100;
const EN_KILLFOCUS: u16 = 0x0200;
const ODS_DISABLED: u32 = 0x4;
const ODS_FOCUS: u32 = 0x10;

struct Fonts {
    title: HFONT,
    body: HFONT,
    bold: HFONT,
    small: HFONT,
}

/// What the window draws, kept for its window procedure (one login window per thread).
struct State {
    scale: f64,
    show_server: bool,
    /// The edit with the keyboard (its underline in the accent colour), 0 for none.
    focus: u16,
    fonts: Fonts,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

/// Where each part goes (96-DPI units): each field's row (its label; the edit 20 below; the underline 46 below), the
/// link or the server's two rows, the button, the status.
struct Layout {
    user: i32,
    password: i32,
    server: i32,
    pin: i32,
    link: i32,
    button: i32,
    status: i32,
}

fn layout(show_server: bool) -> Layout {
    if show_server {
        Layout {
            user: 124,
            password: 182,
            server: 240,
            pin: 298,
            link: -100,
            button: 356,
            status: 404,
        }
    } else {
        Layout {
            user: 124,
            password: 182,
            server: -100,
            pin: -100,
            link: 240,
            button: 278,
            status: 326,
        }
    }
}

/// The fields drawn: (label, row, the edit's id).
fn rows(show_server: bool) -> Vec<(&'static str, i32, u16)> {
    let at = layout(show_server);
    let mut rows = vec![
        (tr("User name"), at.user, ID_USER),
        (tr("Password"), at.password, ID_PASSWORD),
    ];
    if show_server {
        rows.push((tr("Server (host:port)"), at.server, ID_SERVER));
        rows.push((tr("Server key (SHA-256)"), at.pin, ID_PIN));
    }
    rows
}

pub struct LoginForm {
    hwnd: HWND,
    server: HWND,
    pin: HWND,
    user: HWND,
    password: HWND,
    status: HWND,
    button: HWND,
    link: HWND,
}

impl LoginForm {
    pub fn new(title: &str, fields: &LoginFields) -> Result<LoginForm> {
        let instance = unsafe { GetModuleHandleW(None) }?;
        REGISTER.call_once(|| unsafe {
            let class = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(login_proc),
                hInstance: instance.into(),
                hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                hbrBackground: GetSysColorBrush(COLOR_WINDOW),
                hIcon: crate::icon::app_icon(32),
                hIconSm: crate::icon::app_icon(16),
                lpszClassName: CLASS,
                ..Default::default()
            };
            RegisterClassExW(&class);
        });
        let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX;
        let ex_style = WS_EX_CONTROLPARENT;
        let title_w: Vec<u16> = title.encode_utf16().chain(Some(0)).collect();
        let hwnd = unsafe {
            CreateWindowExW(
                ex_style,
                CLASS,
                PCWSTR(title_w.as_ptr()),
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
        // The drawing area at this DPI, the frame around it, centred on the monitor's work area.
        let mut frame = rect(0, 0, WIDTH, HEIGHT, scale);
        unsafe { AdjustWindowRectExForDpi(&mut frame, style, false, ex_style, dpi) }?;
        let (outer_w, outer_h) = (frame.right - frame.left, frame.bottom - frame.top);
        let mut monitor = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        let _ = unsafe {
            GetMonitorInfoW(
                MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST),
                &mut monitor,
            )
        };
        let work = monitor.rcWork;
        let x = work.left + ((work.right - work.left) - outer_w).max(0) / 2;
        let y = work.top + ((work.bottom - work.top) - outer_h).max(0) / 2;
        unsafe { SetWindowPos(hwnd, None, x, y, outer_w, outer_h, SWP_NOZORDER) }?;
        let fonts = Fonts {
            title: font(26, 700, w!("Segoe UI"), scale),
            body: font(15, 400, w!("Segoe UI"), scale),
            bold: font(15, 600, w!("Segoe UI"), scale),
            small: font(12, 400, w!("Segoe UI"), scale),
        };
        // Without a server built in (or one not filled in yet), its fields show at once.
        let show_server = fields.server.is_empty() || fields.pin.is_empty();
        let body = fonts.body;
        let small = fonts.small;
        STATE.with_borrow_mut(|state| {
            *state = Some(State {
                scale,
                show_server,
                focus: 0,
                fonts,
            })
        });
        let child = |class: PCWSTR, text: &str, style: u32, id: u16, font: HFONT| -> Result<HWND> {
            let text: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
            let control = unsafe {
                CreateWindowExW(
                    WINDOW_EX_STYLE::default(),
                    class,
                    PCWSTR(text.as_ptr()),
                    WINDOW_STYLE(WS_CHILD.0 | style),
                    0,
                    0,
                    10,
                    10,
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
                    Some(WPARAM(font.0 as usize)),
                    Some(LPARAM(1)),
                );
            }
            Ok(control)
        };
        let edit = WS_TABSTOP.0 | ES_AUTOHSCROLL;
        let user = child(w!("EDIT"), &fields.user, edit, ID_USER, body)?;
        let password = child(
            w!("EDIT"),
            &fields.password,
            edit | ES_PASSWORD,
            ID_PASSWORD,
            body,
        )?;
        let server = child(w!("EDIT"), &fields.server, edit, ID_SERVER, body)?;
        let pin = child(w!("EDIT"), &fields.pin, edit, ID_PIN, body)?;
        let link = child(
            w!("BUTTON"),
            tr("Use another server..."),
            WS_TABSTOP.0 | BS_OWNERDRAW,
            ID_OTHER_SERVER,
            small,
        )?;
        let button = child(
            w!("BUTTON"),
            tr("Log in"),
            WS_TABSTOP.0 | BS_OWNERDRAW,
            ID_OK,
            body,
        )?;
        let status = child(w!("STATIC"), "", SS_NOPREFIX | SS_CENTER, ID_STATUS, small)?;
        let form = LoginForm {
            hwnd,
            server,
            pin,
            user,
            password,
            status,
            button,
            link,
        };
        form.place();
        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOWNORMAL);
            let _ = SetForegroundWindow(hwnd);
        }
        // Start where something is missing.
        let first_empty = [(user, &fields.user)]
            .into_iter()
            .chain(
                show_server
                    .then_some([(server, &fields.server), (pin, &fields.pin)])
                    .into_iter()
                    .flatten(),
            )
            .find(|(_, value)| value.is_empty())
            .map_or(password, |(control, _)| control);
        let _ = unsafe { SetFocus(Some(first_empty)) };
        Ok(form)
    }

    /// Puts the controls where the layout says (the server's fields shown or not).
    fn place(&self) {
        let (scale, show_server) = STATE.with_borrow(|state| {
            state
                .as_ref()
                .map_or((1.0, true), |state| (state.scale, state.show_server))
        });
        let at = layout(show_server);
        let put = |control: HWND, place: [i32; 4], visible: bool| {
            let area = rect(place[0], place[1], place[2], place[3], scale);
            unsafe {
                let _ = SetWindowPos(
                    control,
                    None,
                    area.left,
                    area.top,
                    area.right - area.left,
                    area.bottom - area.top,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
                let _ = ShowWindow(control, if visible { SW_SHOW } else { SW_HIDE });
            }
        };
        put(self.user, [LEFT, at.user + 20, FIELD, 24], true);
        put(self.password, [LEFT, at.password + 20, FIELD, 24], true);
        put(self.server, [LEFT, at.server + 20, FIELD, 24], show_server);
        put(self.pin, [LEFT, at.pin + 20, FIELD, 24], show_server);
        put(self.link, [LEFT, at.link, FIELD, 22], !show_server);
        put(self.button, [LEFT, at.button, FIELD, 40], true);
        put(self.status, [LEFT - 20, at.status, FIELD + 40, 48], true);
        let _ = unsafe { InvalidateRect(Some(self.hwnd), None, true) };
    }

    fn show_server(&self) {
        STATE.with_borrow_mut(|state| {
            if let Some(state) = state {
                state.show_server = true;
            }
        });
        self.place();
        let _ = unsafe { SetFocus(Some(self.server)) };
    }

    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }

    pub fn fields(&self) -> LoginFields {
        LoginFields {
            server: text_of(self.server).trim().to_string(),
            pin: text_of(self.pin).trim().to_string(),
            user: text_of(self.user).trim().to_string(),
            password: text_of(self.password),
        }
    }

    /// Fills the form (for tests, which then call `submit`).
    pub fn set_fields(&self, fields: &LoginFields) {
        set_text(self.server, &fields.server);
        set_text(self.pin, &fields.pin);
        set_text(self.user, &fields.user);
        set_text(self.password, &fields.password);
    }

    /// Presses "Log in" (for tests).
    pub fn submit(&self) {
        push_action(self.hwnd, ID_OK);
    }

    pub fn set_status(&self, text: &str) {
        set_text(self.status, text);
    }

    /// While a login is under way: no second press, a note in the status line.
    pub fn set_busy(&self, busy: bool, status: &str) {
        let _ = unsafe { EnableWindow(self.button, !busy) };
        let _ = unsafe { InvalidateRect(Some(self.button), None, true) };
        self.set_status(status);
    }

    /// Handles messages for up to `wait_ms` and returns what the person did, if anything.
    pub fn next(&mut self, wait_ms: u32) -> Option<LoginAction> {
        match next_command(self.hwnd, wait_ms)? {
            ID_OK if unsafe { IsWindowEnabled(self.button) }.as_bool() => {
                Some(LoginAction::Submit(self.fields()))
            }
            ID_OTHER_SERVER => {
                self.show_server();
                None
            }
            ID_CANCEL | CLOSED => Some(LoginAction::Cancel),
            _ => None,
        }
    }

    pub fn save_picture(&self, path: &Path) -> std::io::Result<()> {
        save_window_picture(self.hwnd, path)
    }
}

impl Drop for LoginForm {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
        if let Some(state) = STATE.with_borrow_mut(Option::take) {
            for font in [
                state.fonts.title,
                state.fonts.body,
                state.fonts.bold,
                state.fonts.small,
            ] {
                let _ = unsafe { DeleteObject(HGDIOBJ(font.0)) };
            }
        }
        let hwnd = self.hwnd.0 as usize;
        while take_action(HWND(hwnd as *mut c_void)).is_some() {}
    }
}

/// The next command for the window (as forms.rs' forms): queued ones first, then messages for up to `wait_ms`, with
/// Tab, Enter and Esc handled as in a dialog.
fn next_command(hwnd: HWND, wait_ms: u32) -> Option<u16> {
    if let Some(command) = take_action(hwnd) {
        return Some(command);
    }
    unsafe { MsgWaitForMultipleObjects(None, false, wait_ms, QS_ALLINPUT) };
    let mut message = MSG::default();
    while unsafe { PeekMessageW(&mut message, None, 0, 0, PM_REMOVE) }.as_bool() {
        if unsafe { IsDialogMessageW(hwnd, &message) }.as_bool() {
            continue;
        }
        unsafe {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
    if !unsafe { IsWindow(Some(hwnd)) }.as_bool() {
        return Some(CLOSED);
    }
    take_action(hwnd)
}

extern "system" fn login_proc(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match message {
        WM_COMMAND => {
            let id = (wparam.0 & 0xFFFF) as u16;
            let code = ((wparam.0 >> 16) & 0xFFFF) as u16;
            match code {
                // The edit with the keyboard: its underline in the accent colour.
                EN_SETFOCUS | EN_KILLFOCUS if (ID_USER..=ID_PIN).contains(&id) => {
                    STATE.with_borrow_mut(|state| {
                        if let Some(state) = state {
                            state.focus = if code == EN_SETFOCUS { id } else { 0 };
                        }
                    });
                    let _ = unsafe { InvalidateRect(Some(hwnd), None, false) };
                }
                0 | 1 if id != 0 && !(ID_USER..=ID_STATUS).contains(&id) => push_action(hwnd, id),
                _ => {}
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            push_action(hwnd, CLOSED);
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            let mut paint = PAINTSTRUCT::default();
            let dc = unsafe { BeginPaint(hwnd, &mut paint) };
            STATE.with_borrow(|state| {
                if let Some(state) = state {
                    paint_window(hwnd, dc, state);
                }
            });
            let _ = unsafe { EndPaint(hwnd, &paint) };
            LRESULT(0)
        }
        // The fields and the status line sit on the white part.
        WM_CTLCOLOREDIT | WM_CTLCOLORSTATIC => unsafe {
            let dc = HDC(wparam.0 as *mut c_void);
            SetBkColor(dc, COLORREF(WHITE));
            SetTextColor(
                dc,
                COLORREF(if message == WM_CTLCOLOREDIT {
                    TEXT_COLOR
                } else {
                    SECONDARY
                }),
            );
            LRESULT(GetStockObject(WHITE_BRUSH).0 as isize)
        },
        WM_DRAWITEM => {
            let item = unsafe { &*(lparam.0 as *const DRAWITEMSTRUCT) };
            STATE.with_borrow(|state| {
                if let Some(state) = state {
                    draw_button(item, state);
                }
            });
            LRESULT(1)
        }
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

/// The sky (pale blue to white), the magpie and the name, the fields' labels and underlines, the copyright.
fn paint_window(hwnd: HWND, dc: HDC, state: &State) {
    let s = state.scale;
    let f = &state.fonts;
    let mut client = RECT::default();
    let _ = unsafe { GetClientRect(hwnd, &mut client) };
    fill(dc, &client, WHITE);
    let sky = RECT {
        bottom: px(SKY_HEIGHT, s),
        ..client
    };
    let channel = |color: u32, shift: u32| (((color >> shift) & 0xFF) as u16) << 8;
    let vertices = [
        TRIVERTEX {
            x: sky.left,
            y: sky.top,
            Red: channel(SKY, 0),
            Green: channel(SKY, 8),
            Blue: channel(SKY, 16),
            Alpha: 0,
        },
        TRIVERTEX {
            x: sky.right,
            y: sky.bottom,
            Red: 0xFF00,
            Green: 0xFF00,
            Blue: 0xFF00,
            Alpha: 0,
        },
    ];
    let mesh = GRADIENT_RECT {
        UpperLeft: 0,
        LowerRight: 1,
    };
    let _ = unsafe {
        GradientFill(
            dc,
            &vertices,
            std::ptr::from_ref(&mesh).cast(),
            1,
            GRADIENT_FILL_RECT_V,
        )
    };
    // The magpie and the name, side by side in the middle.
    let name = tr("Queqiao");
    let icon_size = px(44, s);
    let gap = px(12, s);
    let name_width = text_width(dc, name, f.title);
    let start = (client.right - (icon_size + gap + name_width)) / 2;
    let top = px(30, s);
    let _ = unsafe {
        DrawIconEx(
            dc,
            start,
            top,
            crate::icon::app_icon(64),
            icon_size,
            icon_size,
            0,
            None,
            DI_NORMAL,
        )
    };
    text(
        dc,
        name,
        RECT {
            left: start + icon_size + gap,
            top,
            right: start + icon_size + gap + name_width + px(4, s),
            bottom: top + icon_size,
        },
        f.title,
        TEXT_COLOR,
        CENTRED,
    );
    text(
        dc,
        tr("Log in, or sign up with a new user name"),
        rect(0, 84, WIDTH, 20, s),
        f.small,
        SECONDARY,
        CENTRED,
    );
    // Each field: its label, its underline (thicker in the accent colour while it has the keyboard).
    for (label, row, id) in rows(state.show_server) {
        text(
            dc,
            label,
            rect(LEFT, row, FIELD, 16, s),
            f.small,
            SECONDARY,
            crate::main_window::LINE,
        );
        let focused = state.focus == id;
        let thickness = if focused {
            px(2, s).max(2)
        } else {
            px(1, s).max(1)
        };
        let line = px(row + 46, s);
        fill(
            dc,
            &RECT {
                left: px(LEFT, s),
                top: line - thickness,
                right: px(LEFT + FIELD, s),
                bottom: line,
            },
            if focused { ACCENT } else { DIVIDER },
        );
    }
    text(
        dc,
        tr("© 2026 GuoWQ222. All rights reserved."),
        rect(0, HEIGHT - 32, WIDTH, 20, s),
        f.small,
        FOOT,
        CENTRED,
    );
}

/// The two drawn buttons: "Log in" (a wide rounded button in the accent colour, grey while it cannot be pressed) and
/// the link to the server's fields (accent text).
fn draw_button(item: &DRAWITEMSTRUCT, state: &State) {
    let s = state.scale;
    let f = &state.fonts;
    let dc = item.hDC;
    let area = item.rcItem;
    let label = crate::forms::text_of(item.hwndItem);
    fill(dc, &area, WHITE);
    match item.CtlID as u16 {
        ID_OK => {
            let enabled = item.itemState.0 & ODS_DISABLED == 0;
            rounded(
                dc,
                &area,
                px(8, s),
                if enabled { ACCENT } else { BUTTON_OFF },
            );
            text(dc, &label, area, f.bold, WHITE, CENTRED);
            if item.itemState.0 & ODS_FOCUS != 0 {
                let inner = RECT {
                    left: area.left + px(3, s),
                    top: area.top + px(3, s),
                    right: area.right - px(3, s),
                    bottom: area.bottom - px(3, s),
                };
                let _ = unsafe { DrawFocusRect(dc, &inner) };
            }
        }
        _ => {
            text(dc, &label, area, f.small, ACCENT, crate::main_window::LINE);
            if item.itemState.0 & ODS_FOCUS != 0 {
                let _ = unsafe { DrawFocusRect(dc, &area) };
            }
        }
    }
}
