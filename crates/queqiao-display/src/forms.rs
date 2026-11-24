//! Small forms made of standard Windows controls: the login form and the device list. They use the
//! system's message font at the window's DPI, keyboard navigation (Tab, Enter, Esc) and, with the
//! common-controls manifest the viewer embeds, the current Windows look.
//!
//! Each form is polled from the program's own loop (`next`), like [`crate::Window`].

use std::cell::RefCell;
use std::ffi::c_void;
use std::path::Path;
use std::sync::Once;

use queqiao_core::text::tr;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, COLOR_WINDOW, CreateCompatibleBitmap, CreateCompatibleDC,
    CreateFontIndirectW, DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDC, GetDIBits, GetMonitorInfoW,
    GetSysColor, GetSysColorBrush, HDC, HFONT, HGDIOBJ, MONITOR_DEFAULTTONEAREST, MONITORINFO,
    MonitorFromWindow, ReleaseDC, SelectObject, SetBkColor,
};
use windows::Win32::Storage::Xps::{PRINT_WINDOW_FLAGS, PrintWindow};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{
    AdjustWindowRectExForDpi, GetDpiForWindow, SystemParametersInfoForDpi,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, IsWindowEnabled, SetFocus};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, Result, w};

use crate::model::{DeviceRow, HelpAction, ListAction, LoginAction, LoginFields, normalize_code};

static REGISTER: Once = Once::new();
const CLASS: PCWSTR = w!("QueqiaoForm");

// Control styles (winuser.h); the windows crate types them inconsistently, so they are plain numbers here.
const ES_AUTOHSCROLL: u32 = 0x80;
const ES_PASSWORD: u32 = 0x20;
const LBS_NOTIFY: u32 = 0x1;
const LBS_USETABSTOPS: u32 = 0x80;
const LBS_NOINTEGRALHEIGHT: u32 = 0x100;
const BS_DEFPUSHBUTTON: u32 = 0x1;
const BS_AUTOCHECKBOX: u32 = 0x3;
const BM_GETCHECK: u32 = 0x00F0;
const BM_SETCHECK: u32 = 0x00F1;
const BST_CHECKED: usize = 1;
const SS_NOPREFIX: u32 = 0x80;
const SS_ENDELLIPSIS: u32 = 0x4000;
const LBN_DBLCLK: u16 = 2;
const BN_CLICKED: u16 = 0;
const PW_RENDERFULLCONTENT: u32 = 2;

/// Command ids. IDOK and IDCANCEL are what Enter and Esc send.
const ID_OK: u16 = 1;
pub(crate) const ID_CANCEL: u16 = 2;
const ID_LIST: u16 = 100;
const ID_LOGOUT: u16 = 101;
const ID_HELP: u16 = 102;
const ID_WAKE: u16 = 103;
const ID_MORE: u16 = 104;
const ID_AUTOSTART: u16 = 105;
const ID_SERVICE: u16 = 106;
const ID_SIGNUP: u16 = 107;
/// The window was closed (not a control id).
pub(crate) const CLOSED: u16 = 0xFFFF;

thread_local! {
    /// (window, command) pairs collected by the window procedure, read by the forms' `next`.
    static ACTIONS: RefCell<Vec<(usize, u16)>> = const { RefCell::new(Vec::new()) };
}

/// A top-level window with child controls laid out in 96-DPI units.
/// How a form's window looks and where it goes.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    /// A normal window with a caption, centred.
    Dialog,
    /// A dialog that stays on top and comes to the front (a question the person must see).
    Prompt,
    /// No caption, always on top, never takes the keyboard, at the top centre of the screen.
    Banner,
}

pub(crate) struct Form {
    pub(crate) hwnd: HWND,
    font: HFONT,
    scale: f64,
    kind: Kind,
}

impl Form {
    /// A fixed-size window whose drawing area is `width` x `height` at 96 DPI, centred on its monitor.
    fn new(title: &str, width: i32, height: i32) -> Result<Form> {
        Form::create(title, width, height, Kind::Dialog)
    }

    /// A window whose drawing area is `width` x `height` at 96 DPI.
    pub(crate) fn create(title: &str, width: i32, height: i32, kind: Kind) -> Result<Form> {
        let instance = unsafe { GetModuleHandleW(None) }?;
        REGISTER.call_once(|| unsafe {
            let class = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(form_proc),
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
        let (style, ex_style) = match kind {
            Kind::Dialog => (
                WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX,
                WS_EX_CONTROLPARENT,
            ),
            Kind::Prompt => (
                WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU,
                WS_EX_CONTROLPARENT | WS_EX_TOPMOST,
            ),
            Kind::Banner => (
                WS_POPUP | WS_BORDER,
                WS_EX_CONTROLPARENT | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            ),
        };
        let title = wide(title);
        let hwnd = unsafe {
            CreateWindowExW(
                ex_style,
                CLASS,
                PCWSTR(title.as_ptr()),
                style,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                width,
                height,
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
            right: (f64::from(width) * scale).round() as i32,
            bottom: (f64::from(height) * scale).round() as i32,
        };
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
        let y = match kind {
            Kind::Banner => work.top + (8.0 * scale).round() as i32,
            _ => work.top + ((work.bottom - work.top) - outer_h).max(0) / 2,
        };
        unsafe { SetWindowPos(hwnd, None, x, y, outer_w, outer_h, SWP_NOZORDER) }?;

        // The system's message font (Segoe UI on current Windows) at this window's DPI.
        let mut metrics = NONCLIENTMETRICSW {
            cbSize: size_of::<NONCLIENTMETRICSW>() as u32,
            ..Default::default()
        };
        let font = unsafe {
            SystemParametersInfoForDpi(
                SPI_GETNONCLIENTMETRICS.0,
                metrics.cbSize,
                Some((&raw mut metrics).cast::<c_void>()),
                0,
                dpi,
            )
            .map(|()| CreateFontIndirectW(&metrics.lfMessageFont))
            .unwrap_or_default()
        };
        Ok(Form {
            hwnd,
            font,
            scale,
            kind,
        })
    }

    pub(crate) fn show(&self) {
        unsafe {
            if self.kind == Kind::Banner {
                let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
            } else {
                let _ = ShowWindow(self.hwnd, SW_SHOWNORMAL);
                let _ = SetForegroundWindow(self.hwnd);
            }
        }
    }

    /// A child control at (x, y, w, h) in 96-DPI units.
    pub(crate) fn control(
        &self,
        class: PCWSTR,
        text: &str,
        style: u32,
        ex: WINDOW_EX_STYLE,
        id: u16,
        place: [i32; 4],
    ) -> Result<HWND> {
        let instance = unsafe { GetModuleHandleW(None) }?;
        let text = wide(text);
        let [x, y, width, height] = place.map(|v| (f64::from(v) * self.scale).round() as i32);
        let hwnd = unsafe {
            CreateWindowExW(
                ex,
                class,
                PCWSTR(text.as_ptr()),
                WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | style),
                x,
                y,
                width,
                height,
                Some(self.hwnd),
                Some(HMENU(usize::from(id) as *mut c_void)),
                Some(instance.into()),
                None,
            )
        }?;
        unsafe {
            SendMessageW(
                hwnd,
                WM_SETFONT,
                Some(WPARAM(self.font.0 as usize)),
                Some(LPARAM(1)),
            );
        }
        Ok(hwnd)
    }

    /// Text that wraps onto as many lines as `place` holds (a label cuts long text with an ellipsis instead).
    pub(crate) fn text(&self, text: &str, place: [i32; 4]) -> Result<HWND> {
        self.control(
            w!("STATIC"),
            text,
            SS_NOPREFIX,
            WINDOW_EX_STYLE::default(),
            0,
            place,
        )
    }

    pub(crate) fn label(&self, text: &str, place: [i32; 4]) -> Result<HWND> {
        self.control(
            w!("STATIC"),
            text,
            SS_NOPREFIX | SS_ENDELLIPSIS,
            WINDOW_EX_STYLE::default(),
            0,
            place,
        )
    }

    fn edit(&self, text: &str, password: bool, place: [i32; 4]) -> Result<HWND> {
        let style = WS_TABSTOP.0 | ES_AUTOHSCROLL | if password { ES_PASSWORD } else { 0 };
        self.control(w!("EDIT"), text, style, WS_EX_CLIENTEDGE, 0, place)
    }

    pub(crate) fn button(
        &self,
        text: &str,
        id: u16,
        default: bool,
        place: [i32; 4],
    ) -> Result<HWND> {
        let style = WS_TABSTOP.0 | if default { BS_DEFPUSHBUTTON } else { 0 };
        self.control(
            w!("BUTTON"),
            text,
            style,
            WINDOW_EX_STYLE::default(),
            id,
            place,
        )
    }

    /// Handles window messages, waiting up to `wait_ms` for one, and returns the next command for this
    /// form.
    pub(crate) fn next_command(&self, wait_ms: u32) -> Option<u16> {
        if let Some(command) = take_action(self.hwnd) {
            return Some(command);
        }
        unsafe { MsgWaitForMultipleObjects(None, false, wait_ms, QS_ALLINPUT) };
        let mut message = MSG::default();
        while unsafe { PeekMessageW(&mut message, None, 0, 0, PM_REMOVE) }.as_bool() {
            // Tab, Enter and Esc for this form; everything else as usual.
            if unsafe { IsDialogMessageW(self.hwnd, &message) }.as_bool() {
                continue;
            }
            unsafe {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
        if !unsafe { IsWindow(Some(self.hwnd)) }.as_bool() {
            return Some(CLOSED);
        }
        take_action(self.hwnd)
    }

    pub(crate) fn post(&self, command: u16) {
        ACTIONS.with_borrow_mut(|actions| actions.push((self.hwnd.0 as usize, command)));
    }

    fn set_enabled(&self, enabled: bool) {
        let _ = unsafe { EnableWindow(self.hwnd, enabled) };
    }
}

impl Drop for Form {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyWindow(self.hwnd);
            let _ = DeleteObject(HGDIOBJ(self.font.0));
        }
        let hwnd = self.hwnd.0 as usize;
        ACTIONS.with_borrow_mut(|actions| actions.retain(|(window, _)| *window != hwnd));
    }
}

/// Queues `command` for the window `hwnd` (read with `take_action`).
pub(crate) fn push_action(hwnd: HWND, command: u16) {
    ACTIONS.with_borrow_mut(|actions| actions.push((hwnd.0 as usize, command)));
}

/// Handles this thread's window messages, waiting up to `wait_ms` for one (no keyboard navigation).
pub(crate) fn pump(wait_ms: u32) {
    unsafe { MsgWaitForMultipleObjects(None, false, wait_ms, QS_ALLINPUT) };
    let mut message = MSG::default();
    while unsafe { PeekMessageW(&mut message, None, 0, 0, PM_REMOVE) }.as_bool() {
        unsafe {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
}

/// As `pump`, with keyboard navigation (Tab, Enter, Esc) for `window` and its controls.
pub(crate) fn pump_for(wait_ms: u32, window: Option<HWND>) {
    let Some(window) = window else {
        return pump(wait_ms);
    };
    unsafe { MsgWaitForMultipleObjects(None, false, wait_ms, QS_ALLINPUT) };
    let mut message = MSG::default();
    while unsafe { PeekMessageW(&mut message, None, 0, 0, PM_REMOVE) }.as_bool() {
        unsafe {
            if IsDialogMessageW(window, &message).as_bool() {
                continue;
            }
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
}

pub(crate) fn take_action(hwnd: HWND) -> Option<u16> {
    ACTIONS.with_borrow_mut(|actions| {
        let index = actions
            .iter()
            .position(|(window, _)| *window == hwnd.0 as usize)?;
        Some(actions.remove(index).1)
    })
}

extern "system" fn form_proc(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let push =
        |command: u16| ACTIONS.with_borrow_mut(|actions| actions.push((hwnd.0 as usize, command)));
    match message {
        WM_COMMAND => {
            let id = (wparam.0 & 0xFFFF) as u16;
            let code = ((wparam.0 >> 16) & 0xFFFF) as u16;
            if id == ID_LIST && code == LBN_DBLCLK {
                push(ID_OK);
            } else if code == BN_CLICKED && id != ID_LIST && id != 0 {
                push(id);
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            push(CLOSED);
            LRESULT(0)
        }
        WM_CTLCOLORSTATIC => {
            // Labels on the window's own background colour.
            unsafe {
                SetBkColor(
                    HDC(wparam.0 as *mut c_void),
                    COLORREF(GetSysColor(COLOR_WINDOW)),
                );
                LRESULT(GetSysColorBrush(COLOR_WINDOW).0 as isize)
            }
        }
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

pub(crate) fn text_of(hwnd: HWND) -> String {
    let mut buffer = vec![0u16; 1024];
    let length = unsafe { GetWindowTextW(hwnd, &mut buffer) }.max(0) as usize;
    String::from_utf16_lossy(&buffer[..length])
}

pub(crate) fn set_text(hwnd: HWND, text: &str) {
    let text = wide(text);
    let _ = unsafe { SetWindowTextW(hwnd, PCWSTR(text.as_ptr())) };
}

/// A password asked for while something else goes on (a session): polled with `next`.
pub struct PasswordForm {
    form: Form,
    edit: HWND,
    status: HWND,
}

/// What the person did in the password form.
pub enum PasswordAction {
    Submit(String),
    Cancel,
}

impl PasswordForm {
    pub fn new(title: &str, text: &str) -> Result<PasswordForm> {
        let form = Form::create(title, 420, 210, Kind::Prompt)?;
        form.text(text, [20, 14, 380, 62])?;
        let edit = form.edit("", true, [20, 84, 380, 26])?;
        let status = form.label("", [20, 118, 380, 22])?;
        form.button(tr("OK"), ID_OK, true, [196, 160, 96, 30])?;
        form.button(tr("Cancel"), ID_CANCEL, false, [304, 160, 96, 30])?;
        form.show();
        let _ = unsafe { SetFocus(Some(edit)) };
        Ok(PasswordForm { form, edit, status })
    }

    /// A line under the field (the field is emptied).
    pub fn set_status(&self, text: &str) {
        set_text(self.status, text);
        set_text(self.edit, "");
    }

    pub fn next(&mut self, wait_ms: u32) -> Option<PasswordAction> {
        match self.form.next_command(wait_ms)? {
            ID_OK => Some(PasswordAction::Submit(text_of(self.edit))),
            ID_CANCEL | CLOSED => Some(PasswordAction::Cancel),
            _ => None,
        }
    }
}

/// A menu at the mouse pointer, owned by `owner`; the chosen item's id.
pub fn popup_menu(owner: HWND, items: &[(u16, &str, bool)]) -> Option<u16> {
    let menu = unsafe { CreatePopupMenu() }.ok()?;
    for (id, text, checked) in items {
        let text: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
        let flags = if *checked {
            MF_STRING | MF_CHECKED
        } else {
            MF_STRING
        };
        let _ = unsafe { AppendMenuW(menu, flags, usize::from(*id), PCWSTR(text.as_ptr())) };
    }
    let mut at = windows::Win32::Foundation::POINT::default();
    let _ = unsafe { GetCursorPos(&mut at) };
    let chosen = unsafe {
        TrackPopupMenu(
            menu,
            TPM_RIGHTBUTTON | TPM_RETURNCMD,
            at.x,
            at.y,
            None,
            owner,
            None,
        )
    }
    .0;
    let _ = unsafe { DestroyMenu(menu) };
    u16::try_from(chosen).ok().filter(|id| *id != 0)
}

/// Asks for one line of text (`initial` filled in); None when cancelled.
pub fn ask_text(title: &str, prompt: &str, initial: &str) -> Option<String> {
    let form = Form::new(title, 380, 170).ok()?;
    form.label(prompt, [20, 16, 340, 20]).ok()?;
    let field = form.edit(initial, false, [20, 40, 340, 24]).ok()?;
    form.button(tr("OK"), ID_OK, true, [260, 90, 100, 30])
        .ok()?;
    form.show();
    let _ = unsafe { SetFocus(Some(field)) };
    loop {
        match form.next_command(50) {
            Some(ID_OK) => return Some(text_of(field)),
            Some(ID_CANCEL | CLOSED) => return None,
            _ => {}
        }
    }
}

/// Asks for a password (not shown as typed); None when cancelled.
pub fn ask_secret(title: &str, prompt: &str) -> Option<String> {
    let form = Form::new(title, 380, 170).ok()?;
    form.label(prompt, [20, 16, 340, 20]).ok()?;
    let field = form.edit("", true, [20, 40, 340, 24]).ok()?;
    form.button(tr("OK"), ID_OK, true, [260, 90, 100, 30])
        .ok()?;
    form.show();
    let _ = unsafe { SetFocus(Some(field)) };
    loop {
        match form.next_command(50) {
            Some(ID_OK) => return Some(text_of(field)),
            Some(ID_CANCEL | CLOSED) => return None,
            _ => {}
        }
    }
}

/// A yes/no question (No is the default).
pub fn confirm(owner: HWND, title: &str, text: &str) -> bool {
    let title: Vec<u16> = title.encode_utf16().chain(Some(0)).collect();
    let text: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
    let answer = unsafe {
        MessageBoxW(
            Some(owner),
            PCWSTR(text.as_ptr()),
            PCWSTR(title.as_ptr()),
            MB_YESNO | MB_ICONWARNING | MB_DEFBUTTON2,
        )
    };
    answer == IDYES
}

/// A small window with a list and buttons (the transfer list); each press comes with the selected row.
pub struct PickList {
    form: Form,
    list: HWND,
    items: Vec<String>,
}

impl PickList {
    pub fn new(title: &str, heading: &str, buttons: &[(u16, &str)]) -> Result<PickList> {
        let form = Form::new(title, 520, 320)?;
        form.label(heading, [20, 16, 480, 20])?;
        let list = form.control(
            w!("LISTBOX"),
            "",
            WS_TABSTOP.0 | WS_VSCROLL.0 | LBS_NOTIFY | LBS_NOINTEGRALHEIGHT,
            WS_EX_CLIENTEDGE,
            ID_LIST,
            [20, 44, 480, 180],
        )?;
        let mut left = 20;
        for (id, text) in buttons {
            form.button(text, *id, false, [left, 240, 140, 30])?;
            left += 150;
        }
        form.show();
        Ok(PickList {
            form,
            list,
            items: Vec::new(),
        })
    }

    /// Shows `items`, keeping the selected row where it was.
    pub fn set_items(&mut self, items: Vec<String>) {
        if items == self.items {
            return;
        }
        let selected = self.selected();
        unsafe { SendMessageW(self.list, LB_RESETCONTENT, None, None) };
        for item in &items {
            let text = wide(item);
            unsafe {
                SendMessageW(
                    self.list,
                    LB_ADDSTRING,
                    None,
                    Some(LPARAM(text.as_ptr() as isize)),
                )
            };
        }
        self.items = items;
        if let Some(index) = selected.filter(|index| *index < self.items.len()) {
            unsafe { SendMessageW(self.list, LB_SETCURSEL, Some(WPARAM(index)), None) };
        }
    }

    pub fn selected(&self) -> Option<usize> {
        let index = unsafe { SendMessageW(self.list, LB_GETCURSEL, None, None) }.0;
        usize::try_from(index)
            .ok()
            .filter(|index| *index < self.items.len())
    }

    /// A button pressed (with the selected row), or None; Some((CLOSED, _)) when the window was closed.
    pub fn next(&mut self, wait_ms: u32) -> Option<(u16, Option<usize>)> {
        let command = self.form.next_command(wait_ms)?;
        Some((
            if command == ID_CANCEL {
                CLOSED
            } else {
                command
            },
            self.selected(),
        ))
    }
}

/// What a closed PickList reports.
pub const PICK_CLOSED: u16 = CLOSED;

/// Asks for the help ID and code of someone to help (another account's device).
pub struct HelpForm {
    form: Form,
    help_id: HWND,
    code: HWND,
    status: HWND,
}

impl HelpForm {
    pub fn new() -> Result<HelpForm> {
        let form = Form::new(tr("Queqiao - help someone"), 380, 260)?;
        form.text(
            tr("Ask the person you help for the help ID and code that Queqiao shows them (tray menu: Let someone help me...)."),
            [20, 14, 340, 44],
        )?;
        form.label(tr("Help ID"), [20, 64, 340, 18])?;
        let help_id = form.edit("", false, [20, 84, 340, 24])?;
        form.label(
            tr("Code (leave empty to ask the person to allow it)"),
            [20, 118, 340, 18],
        )?;
        let code = form.edit("", false, [20, 138, 340, 24])?;
        let status = form.text("", [20, 170, 340, 36])?;
        form.button(tr("Connect"), ID_OK, true, [260, 214, 100, 30])?;
        form.show();
        let _ = unsafe { SetFocus(Some(help_id)) };
        Ok(HelpForm {
            form,
            help_id,
            code,
            status,
        })
    }

    pub fn hwnd(&self) -> HWND {
        self.form.hwnd
    }

    /// Fills the form and presses Connect (tests).
    pub fn submit_with(&self, help_id: &str, code: &str) {
        set_text(self.help_id, help_id);
        set_text(self.code, code);
        self.form.post(ID_OK);
    }

    /// Fills the form (from an invitation), leaving Connect to the person.
    pub fn fill(&self, help_id: &str, code: &str) {
        set_text(self.help_id, &queqiao_core::invitation::spaced_id(help_id));
        set_text(self.code, &queqiao_core::invitation::halved_code(code));
    }

    /// Presses Connect as a click would (tests).
    pub fn press_connect(&self) {
        self.form.post(ID_OK);
    }

    pub fn save_picture(&self, path: &Path) -> std::io::Result<()> {
        save_window_picture(self.form.hwnd, path)
    }

    pub fn set_status(&self, text: &str) {
        set_text(self.status, text);
    }

    pub fn next(&mut self, wait_ms: u32) -> Option<HelpAction> {
        match self.form.next_command(wait_ms)? {
            ID_OK => Some(HelpAction::Submit {
                help_id: text_of(self.help_id)
                    .chars()
                    .filter(char::is_ascii_digit)
                    .collect(),
                code: normalize_code(&text_of(self.code)),
            }),
            ID_CANCEL | CLOSED => Some(HelpAction::Cancel),
            _ => None,
        }
    }
}

pub struct LoginForm {
    form: Form,
    server: HWND,
    pin: HWND,
    user: HWND,
    password: HWND,
    status: HWND,
    button: HWND,
    signup: HWND,
}

impl LoginForm {
    pub fn new(title: &str, fields: &LoginFields) -> Result<LoginForm> {
        let form = Form::new(title, 380, 344)?;
        let (left, width) = (20, 340);
        form.label(tr("Server (host:port)"), [left, 16, width, 18])?;
        let server = form.edit(&fields.server, false, [left, 36, width, 24])?;
        form.label(
            tr("Server certificate fingerprint (SHA-256)"),
            [left, 70, width, 18],
        )?;
        let pin = form.edit(&fields.pin, false, [left, 90, width, 24])?;
        form.label(tr("User name"), [left, 124, width, 18])?;
        let user = form.edit(&fields.user, false, [left, 144, width, 24])?;
        form.label(tr("Password"), [left, 178, width, 18])?;
        let password = form.edit(&fields.password, true, [left, 198, width, 24])?;
        // Three lines that wrap: an error is read whole (a label cut it off with an ellipsis, 2026-11-23).
        let status = form.text("", [left, 232, width, 52])?;
        let button = form.button(
            tr("Log in"),
            ID_OK,
            true,
            [left + width - 100, 294, 100, 30],
        )?;
        let signup = form.button(
            tr("Create an account"),
            ID_SIGNUP,
            false,
            [left, 294, 160, 30],
        )?;
        form.show();
        // Start where something is missing.
        let first_empty = [
            (server, &fields.server),
            (pin, &fields.pin),
            (user, &fields.user),
        ]
        .into_iter()
        .find(|(_, value)| value.is_empty())
        .map_or(password, |(control, _)| control);
        let _ = unsafe { SetFocus(Some(first_empty)) };
        Ok(LoginForm {
            form,
            server,
            pin,
            user,
            password,
            status,
            button,
            signup,
        })
    }

    pub fn hwnd(&self) -> HWND {
        self.form.hwnd
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
        self.form.post(ID_OK);
    }

    pub fn set_status(&self, text: &str) {
        set_text(self.status, text);
    }

    /// While a login is under way: no second press, a note in the status line.
    pub fn set_busy(&self, busy: bool, status: &str) {
        let _ = unsafe { EnableWindow(self.button, !busy) };
        let _ = unsafe { EnableWindow(self.signup, !busy) };
        self.set_status(status);
    }

    /// Presses "Create an account" as a click would (tests).
    pub fn press_signup(&self) {
        self.form.post(ID_SIGNUP);
    }

    /// Handles messages for up to `wait_ms` and returns what the user did, if anything.
    pub fn next(&mut self, wait_ms: u32) -> Option<LoginAction> {
        match self.form.next_command(wait_ms)? {
            ID_OK if unsafe { IsWindowEnabled(self.button) }.as_bool() => {
                Some(LoginAction::Submit(self.fields()))
            }
            ID_SIGNUP if unsafe { IsWindowEnabled(self.signup) }.as_bool() => {
                Some(LoginAction::Register(self.fields()))
            }
            ID_CANCEL | CLOSED => Some(LoginAction::Cancel),
            _ => None,
        }
    }

    pub fn save_picture(&self, path: &Path) -> std::io::Result<()> {
        save_window_picture(self.form.hwnd, path)
    }
}

pub struct DeviceList {
    form: Form,
    list: HWND,
    status: HWND,
    rows: Vec<DeviceRow>,
}

impl DeviceList {
    pub fn new(title: &str, heading: &str) -> Result<DeviceList> {
        let form = Form::new(title, 540, 400)?;
        form.label(heading, [20, 16, 500, 20])?;
        let list = form.control(
            w!("LISTBOX"),
            "",
            WS_TABSTOP.0 | WS_VSCROLL.0 | LBS_NOTIFY | LBS_USETABSTOPS | LBS_NOINTEGRALHEIGHT,
            WS_EX_CLIENTEDGE,
            ID_LIST,
            [20, 44, 500, 250],
        )?;
        // One tab stop (in dialog units) between the name and the status column.
        let stops = [150i32];
        unsafe {
            SendMessageW(
                list,
                LB_SETTABSTOPS,
                Some(WPARAM(1)),
                Some(LPARAM(stops.as_ptr() as isize)),
            );
        }
        let status = form.text(
            tr("Double-click a device to connect to it."),
            [20, 304, 500, 36],
        )?;
        form.button(tr("Log out"), ID_LOGOUT, false, [20, 352, 90, 30])?;
        form.button(tr("Wake up"), ID_WAKE, false, [116, 352, 94, 30])?;
        form.button(tr("Help someone..."), ID_HELP, false, [216, 352, 100, 30])?;
        form.button(tr("More..."), ID_MORE, false, [322, 352, 90, 30])?;
        form.button(tr("Connect"), ID_OK, true, [422, 352, 98, 30])?;
        form.show();
        let _ = unsafe { SetFocus(Some(list)) };
        Ok(DeviceList {
            form,
            list,
            status,
            rows: Vec::new(),
        })
    }

    pub fn hwnd(&self) -> HWND {
        self.form.hwnd
    }

    /// Replaces the rows; the selected device stays selected.
    pub fn set_rows(&mut self, rows: Vec<DeviceRow>) {
        if rows == self.rows {
            return;
        }
        let selected = self.selected().map(|row| row.id.clone());
        unsafe { SendMessageW(self.list, LB_RESETCONTENT, None, None) };
        for row in &rows {
            let text = wide(&format!("{}\t{}", row.name, row.status));
            unsafe {
                SendMessageW(
                    self.list,
                    LB_ADDSTRING,
                    None,
                    Some(LPARAM(text.as_ptr() as isize)),
                );
            }
        }
        self.rows = rows;
        if let Some(id) = selected {
            self.select(&id);
        }
    }

    fn selected(&self) -> Option<&DeviceRow> {
        let index = unsafe { SendMessageW(self.list, LB_GETCURSEL, None, None) }.0;
        usize::try_from(index)
            .ok()
            .and_then(|index| self.rows.get(index))
    }

    /// Selects the device whose id or name (any case) is `key`; false if there is none.
    pub fn select(&self, key: &str) -> bool {
        let Some(index) = self
            .rows
            .iter()
            .position(|row| row.id == key || row.name.eq_ignore_ascii_case(key))
        else {
            return false;
        };
        unsafe { SendMessageW(self.list, LB_SETCURSEL, Some(WPARAM(index)), None) };
        true
    }

    /// Presses "Connect" (for tests).
    pub fn press_connect(&self) {
        self.form.post(ID_OK);
    }

    pub fn set_status(&self, text: &str) {
        set_text(self.status, text);
    }

    /// Greys the window out while a session runs.
    pub fn set_enabled(&self, enabled: bool) {
        self.form.set_enabled(enabled);
    }

    /// Handles messages for up to `wait_ms` and returns what the user did, if anything.
    pub fn next(&mut self, wait_ms: u32) -> Option<ListAction> {
        match self.form.next_command(wait_ms)? {
            ID_OK => match self.selected() {
                Some(row) => Some(ListAction::Connect(row.id.clone())),
                None => {
                    self.set_status(tr("Select a device first."));
                    None
                }
            },
            ID_LOGOUT => Some(ListAction::LogOut),
            ID_HELP => Some(ListAction::Help),
            ID_MORE => match self.selected() {
                Some(row) => Some(ListAction::More(row.id.clone())),
                None => {
                    self.set_status(tr("Select a device first."));
                    None
                }
            },
            ID_WAKE => match self.selected() {
                Some(row) => Some(ListAction::Wake(row.id.clone())),
                None => {
                    self.set_status(tr("Select a device first."));
                    None
                }
            },
            CLOSED => Some(ListAction::Closed),
            _ => None,
        }
    }

    pub fn save_picture(&self, path: &Path) -> std::io::Result<()> {
        save_window_picture(self.form.hwnd, path)
    }
}

/// The standard Open dialog for one or more files, owned by `owner`: the paths chosen (none if it was cancelled).
/// Run it on a thread of its own when the caller must keep going meanwhile (a session keeps its picture).
/// The window a dialog belongs to, from its number as it was sent to another thread (an HWND is not Send).
pub fn owner_from_raw(raw: isize) -> HWND {
    HWND(raw as *mut std::ffi::c_void)
}

pub fn pick_files(owner: Option<HWND>, title: &str) -> Vec<std::path::PathBuf> {
    use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
    use windows::Win32::UI::Controls::Dialogs::{
        GetOpenFileNameW, OFN_ALLOWMULTISELECT, OFN_EXPLORER, OFN_FILEMUSTEXIST, OFN_HIDEREADONLY,
        OFN_PATHMUSTEXIST, OPENFILENAMEW,
    };
    // The dialog uses COM; this thread may not have it yet.
    let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
    let mut buffer = vec![0u16; 64 * 1024];
    let title: Vec<u16> = title.encode_utf16().chain(Some(0)).collect();
    let filter: Vec<u16> = format!("{}\0*.*\0\0", tr("All files"))
        .encode_utf16()
        .collect();
    let mut dialog = OPENFILENAMEW {
        lStructSize: size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: owner.unwrap_or_default(),
        lpstrFilter: PCWSTR(filter.as_ptr()),
        lpstrFile: windows::core::PWSTR(buffer.as_mut_ptr()),
        nMaxFile: buffer.len() as u32,
        lpstrTitle: PCWSTR(title.as_ptr()),
        Flags: OFN_ALLOWMULTISELECT
            | OFN_EXPLORER
            | OFN_FILEMUSTEXIST
            | OFN_PATHMUSTEXIST
            | OFN_HIDEREADONLY,
        ..Default::default()
    };
    if !unsafe { GetOpenFileNameW(&mut dialog) }.as_bool() {
        return Vec::new();
    }
    // One file: its full path. Several: the folder, then each name; NUL-separated, two NULs at the end.
    let parts: Vec<String> = buffer
        .split(|&unit| unit == 0)
        .take_while(|part| !part.is_empty())
        .map(String::from_utf16_lossy)
        .collect();
    match parts.as_slice() {
        [] => Vec::new(),
        [one] => vec![std::path::PathBuf::from(one)],
        [folder, names @ ..] => names
            .iter()
            .map(|name| std::path::Path::new(folder).join(name))
            .collect(),
    }
}

/// Saves a window as it looks now (also when covered) as a 32-bit BMP file.
pub fn save_window_picture(hwnd: HWND, path: &Path) -> std::io::Result<()> {
    save_picture(hwnd, path, false)
}

/// As [`save_window_picture`], but copied from the screen: with what lies on top of the window (a session window's
/// bar and card of figures, which are windows of their own).
pub fn save_screen_picture(hwnd: HWND, path: &Path) -> std::io::Result<()> {
    save_picture(hwnd, path, true)
}

fn save_picture(hwnd: HWND, path: &Path, from_screen: bool) -> std::io::Result<()> {
    let mut area = RECT::default();
    unsafe { GetWindowRect(hwnd, &mut area) }.map_err(std::io::Error::other)?;
    let (width, height) = (area.right - area.left, area.bottom - area.top);
    if width <= 0 || height <= 0 {
        return Err(std::io::Error::other("the window has no size"));
    }
    let mut pixels = vec![0u8; (width * height * 4) as usize];
    unsafe {
        let screen = GetDC(None);
        let memory = CreateCompatibleDC(Some(screen));
        let bitmap = CreateCompatibleBitmap(screen, width, height);
        let old = SelectObject(memory, HGDIOBJ(bitmap.0));
        if from_screen {
            // CAPTUREBLT: layered windows on top are copied too.
            let _ = windows::Win32::Graphics::Gdi::BitBlt(
                memory,
                0,
                0,
                width,
                height,
                Some(screen),
                area.left,
                area.top,
                windows::Win32::Graphics::Gdi::ROP_CODE(
                    windows::Win32::Graphics::Gdi::SRCCOPY.0
                        | windows::Win32::Graphics::Gdi::CAPTUREBLT.0,
                ),
            );
        } else {
            let _ = PrintWindow(hwnd, memory, PRINT_WINDOW_FLAGS(PW_RENDERFULLCONTENT));
        }
        SelectObject(memory, old);
        let mut info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        GetDIBits(
            memory,
            bitmap,
            0,
            height as u32,
            Some(pixels.as_mut_ptr().cast()),
            &mut info,
            DIB_RGB_COLORS,
        );
        let _ = DeleteObject(HGDIOBJ(bitmap.0));
        let _ = DeleteDC(memory);
        ReleaseDC(None, screen);
    }
    // BMP: file header, info header (top-down rows), BGRA pixels.
    let mut file = Vec::with_capacity(54 + pixels.len());
    file.extend_from_slice(b"BM");
    file.extend_from_slice(&(54 + pixels.len() as u32).to_le_bytes());
    file.extend_from_slice(&[0; 4]);
    file.extend_from_slice(&54u32.to_le_bytes());
    file.extend_from_slice(&40u32.to_le_bytes());
    file.extend_from_slice(&width.to_le_bytes());
    file.extend_from_slice(&(-height).to_le_bytes());
    file.extend_from_slice(&1u16.to_le_bytes());
    file.extend_from_slice(&32u16.to_le_bytes());
    file.extend_from_slice(&[0; 24]);
    file.extend_from_slice(&pixels);
    std::fs::write(path, file)
}

/// An error box (for programs started by double-click, which have no console).
pub fn show_error(title: &str, text: &str) {
    let (title, text) = (wide(title), wide(text));
    unsafe {
        MessageBoxW(
            None,
            PCWSTR(text.as_ptr()),
            PCWSTR(title.as_ptr()),
            MB_ICONERROR | MB_OK,
        )
    };
}

/// Puts `text` on the clipboard; whether it worked.
pub fn copy_text(owner: HWND, text: &str) -> bool {
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
    };
    use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
    const CF_UNICODETEXT: u32 = 13;
    let wide: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
    unsafe {
        if OpenClipboard(Some(owner)).is_err() {
            return false;
        }
        let done = (|| -> Option<()> {
            EmptyClipboard().ok()?;
            let memory = GlobalAlloc(GMEM_MOVEABLE, wide.len() * 2).ok()?;
            let target = GlobalLock(memory).cast::<u16>();
            if target.is_null() {
                return None;
            }
            std::ptr::copy_nonoverlapping(wide.as_ptr(), target, wide.len());
            let _ = GlobalUnlock(memory);
            // The clipboard owns the memory from here.
            SetClipboardData(CF_UNICODETEXT, Some(HANDLE(memory.0))).ok()?;
            Some(())
        })()
        .is_some();
        let _ = CloseClipboard();
        done
    }
}

/// A message box with an information icon.
pub fn show_info(title: &str, text: &str) {
    let (title, text) = (wide(title), wide(text));
    unsafe {
        MessageBoxW(
            None,
            PCWSTR(text.as_ptr()),
            PCWSTR(title.as_ptr()),
            MB_ICONINFORMATION | MB_OK,
        )
    };
}

/// What the person chose in the installer.
pub enum SetupAction {
    Install { autostart: bool, service: bool },
    Cancel,
}

/// The installer's window: what is installed where, two choices (start with Windows; the service for the
/// sign-in, lock and UAC screens), Install and Cancel, and a status line.
pub struct SetupForm {
    form: Form,
    autostart: HWND,
    service: HWND,
    status: HWND,
}

impl SetupForm {
    pub fn new(text: &str, autostart: bool, service: bool) -> Result<SetupForm> {
        let form = Form::new(tr("Install Queqiao"), 420, 250)?;
        form.text(text, [20, 14, 380, 62])?;
        let check = |label: &str, id: u16, top: i32, on: bool| -> Result<HWND> {
            let hwnd = form.control(
                w!("BUTTON"),
                label,
                WS_TABSTOP.0 | BS_AUTOCHECKBOX,
                WINDOW_EX_STYLE::default(),
                id,
                [20, top, 380, 36],
            )?;
            unsafe { SendMessageW(hwnd, BM_SETCHECK, Some(WPARAM(usize::from(on))), None) };
            Ok(hwnd)
        };
        let autostart = check(tr("Start with Windows"), ID_AUTOSTART, 82, autostart)?;
        let service = check(
            tr(
                "Also at the sign-in, lock and UAC screens (installs the Queqiao service; needs an administrator)",
            ),
            ID_SERVICE,
            118,
            service,
        )?;
        let status = form.label("", [20, 162, 380, 20])?;
        form.button(tr("Cancel"), ID_CANCEL, false, [190, 200, 100, 30])?;
        form.button(tr("Install"), ID_OK, true, [300, 200, 100, 30])?;
        form.show();
        Ok(SetupForm {
            form,
            autostart,
            service,
            status,
        })
    }

    pub fn hwnd(&self) -> HWND {
        self.form.hwnd
    }

    pub fn set_status(&self, text: &str) {
        set_text(self.status, text);
    }

    /// While installing: nothing to press.
    pub fn set_busy(&self, busy: bool) {
        self.form.set_enabled(!busy);
    }

    pub fn next(&mut self, wait_ms: u32) -> Option<SetupAction> {
        let checked = |hwnd: HWND| {
            unsafe { SendMessageW(hwnd, BM_GETCHECK, None, None) }.0 as usize == BST_CHECKED
        };
        match self.form.next_command(wait_ms)? {
            ID_OK => Some(SetupAction::Install {
                autostart: checked(self.autostart),
                service: checked(self.service),
            }),
            ID_CANCEL | CLOSED => Some(SetupAction::Cancel),
            _ => None,
        }
    }
}
