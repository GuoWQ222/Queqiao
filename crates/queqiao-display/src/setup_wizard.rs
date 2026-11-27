//! The installer's window on Windows, a wizard (the user, 2026-11-27: "redesign the windows installer, make it like other
//! software's installer and make it beautiful ... next page ... previous page ... installation location", then UU
//! Remote's installer as the reference: "follow this design and redesign the installation package, making it different
//! and beautiful"). UU's steps (welcome, where to install, installing with its steps, done), drawn as Queqiao's other
//! windows: a blue side panel with the magpie, Queqiao's name and the steps, white pages, rounded buttons. Polled with
//! `next` as the other forms; Tab, Enter and Esc as in a dialog.

use std::cell::RefCell;
use std::ffi::c_void;
use std::path::Path;
use std::sync::Once;

use queqiao_core::text::{tr, trf};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, COLOR_WINDOW, CreateCompatibleBitmap, CreateCompatibleDC, CreatePen,
    DeleteDC, DeleteObject, DrawFocusRect, EndPaint, GRADIENT_FILL_RECT_V, GRADIENT_RECT,
    GetMonitorInfoW, GetStockObject, GetSysColorBrush, GradientFill, HDC, HFONT, HGDIOBJ,
    InvalidateRect, LineTo, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow, MoveToEx,
    PAINTSTRUCT, PS_SOLID, SRCCOPY, SelectObject, SetBkColor, SetTextColor, TRIVERTEX, WHITE_BRUSH,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::DRAWITEMSTRUCT;
use windows::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForWindow};
use windows::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, SetFocus};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, Result, w};

use crate::forms::{
    CLOSED, ID_CANCEL, push_action, save_window_picture, set_text, take_action, text_of,
};
use crate::main_window::{
    ACCENT, CENTRED, DIVIDER, LINE, SECONDARY, TEXT_COLOR, WHITE, fill, font, outlined, px, rect,
    rounded, text, text_width,
};

const CLASS: PCWSTR = w!("QueqiaoSetupWizard");
static REGISTER: Once = Once::new();

/// The drawing area at 96 DPI: the side panel, the page's column, the button row.
const WIDTH: i32 = 640;
const HEIGHT: i32 = 420;
const PANEL: i32 = 196;
const LEFT: i32 = 228;
const COLUMN: i32 = 384;
const FOOT: i32 = 356;
const BUTTONS: i32 = 368;

/// Command ids: IDOK (Next, Install, Finish) is what Enter sends, IDCANCEL what Esc sends.
const ID_NEXT: u16 = 1;
const ID_BACK: u16 = 20;
const ID_BROWSE: u16 = 21;
const ID_FOLDER: u16 = 22;
const ID_DESKTOP: u16 = 23;
const ID_OPEN: u16 = 25;
/// Pushed when the folder's text changed (typed, or chosen with Browse).
const FOLDER_CHANGED: u16 = 26;

/// Colours (0x00BBGGRR): the side panel's gradient, its text, the steps to come; a button that cannot be pressed,
/// a frame, the service note's tint, an error, success, the log's box.
const PANEL_TOP: u32 = 0x00D9_883F;
const PANEL_BOTTOM: u32 = 0x008A_3A1E;
const PANEL_TEXT: u32 = 0x00FF_FFFF;
const PANEL_DIM: u32 = 0x00E8_C9AE;
const BUTTON_OFF: u32 = 0x00D8_CFC8;
const FRAME: u32 = 0x00D9_D0CB;
const NOTE: u32 = 0x00FE_EFE3;
const ERROR: u32 = 0x003A_3AD9;
const SUCCESS: u32 = 0x0050_AF4C;
const LOG_BOX: u32 = 0x00F8_F5F3;

// Control styles and notifications (winuser.h) as plain numbers, as in forms.rs.
const ES_AUTOHSCROLL: u32 = 0x80;
const BS_OWNERDRAW: u32 = 0xB;
const EN_SETFOCUS: u16 = 0x0100;
const EN_KILLFOCUS: u16 = 0x0200;
const EN_CHANGE: u16 = 0x0300;
const ODS_DISABLED: u32 = 0x4;
const ODS_FOCUS: u32 = 0x10;
/// Windows hides focus rectangles until the keyboard is used (as its own buttons do).
const ODS_NOFOCUSRECT: u32 = 0x200;
/// Wrapped text as an edit control breaks it (Chinese, which has no spaces, too).
const WRAP: u32 = 0x0010 | 0x2000; // DT_WORDBREAK | DT_EDITCONTROL (DT_LEFT is 0)

/// The pages, in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WizardPage {
    Welcome,
    Location,
    Installing,
    Finished,
}

/// What the person did.
#[derive(Debug, PartialEq, Eq)]
pub enum WizardAction {
    /// "Install" on the location page: the folder as typed (to be checked) and the shortcuts wanted.
    Install { folder: String, desktop: bool },
    /// The folder's text changed (its free room may differ).
    FolderChanged,
    /// "Finish" (or "Close" after a failure); `open`: open Queqiao now.
    Finish { open: bool },
    /// Cancel, Esc or the window closed (not while installing).
    Cancel,
}

/// What the wizard starts with.
pub struct WizardSetup<'a> {
    pub version: &'a str,
    /// The folder suggested (or the installed one).
    pub folder: &'a str,
    /// Queqiao is installed already: its folder, which an update keeps (as UU Remote's installer does).
    pub installed: bool,
    pub desktop: bool,
}

struct Fonts {
    title: HFONT,
    body: HFONT,
    bold: HFONT,
    small: HFONT,
    name: HFONT,
}

/// What the window draws, kept for its window procedure (one wizard per thread).
struct State {
    scale: f64,
    page: WizardPage,
    version: String,
    installed: bool,
    /// "Space needed ... available ...", the location page's error, the install's percent and step, its log.
    space: String,
    error: String,
    percent: u32,
    step: String,
    log: Vec<String>,
    /// How the install ended (the finish page).
    result: Option<std::result::Result<(), String>>,
    desktop: bool,
    open: bool,
    folder_focus: bool,
    fonts: Fonts,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

fn with_state<R>(default: R, f: impl FnOnce(&mut State) -> R) -> R {
    STATE.with_borrow_mut(|state| state.as_mut().map_or(default, f))
}

pub struct SetupWizard {
    hwnd: HWND,
    folder: HWND,
    browse: HWND,
    back: HWND,
    next: HWND,
    cancel: HWND,
    desktop: HWND,
    open: HWND,
}

impl SetupWizard {
    pub fn new(setup: &WizardSetup) -> Result<SetupWizard> {
        let instance = unsafe { GetModuleHandleW(None) }?;
        REGISTER.call_once(|| unsafe {
            let class = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(wizard_proc),
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
        let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX | WS_CLIPCHILDREN;
        let ex_style = WS_EX_CONTROLPARENT;
        let title: Vec<u16> = tr("Queqiao Setup").encode_utf16().chain(Some(0)).collect();
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
            title: font(22, 700, w!("Segoe UI"), scale),
            body: font(14, 400, w!("Segoe UI"), scale),
            bold: font(14, 600, w!("Segoe UI"), scale),
            small: font(12, 400, w!("Segoe UI"), scale),
            name: font(20, 700, w!("Segoe UI"), scale),
        };
        let body = fonts.body;
        STATE.with_borrow_mut(|state| {
            *state = Some(State {
                scale,
                page: WizardPage::Welcome,
                version: setup.version.to_string(),
                installed: setup.installed,
                space: String::new(),
                error: String::new(),
                percent: 0,
                step: String::new(),
                log: Vec::new(),
                result: None,
                desktop: setup.desktop,
                open: true,
                folder_focus: false,
                fonts,
            })
        });
        let child = |class: PCWSTR, label: &str, style: u32, id: u16| -> Result<HWND> {
            let label: Vec<u16> = label.encode_utf16().chain(Some(0)).collect();
            let control = unsafe {
                CreateWindowExW(
                    WINDOW_EX_STYLE::default(),
                    class,
                    PCWSTR(label.as_ptr()),
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
                    Some(WPARAM(body.0 as usize)),
                    Some(LPARAM(1)),
                )
            };
            Ok(control)
        };
        let button = WS_TABSTOP.0 | BS_OWNERDRAW;
        let wizard = SetupWizard {
            hwnd,
            folder: child(
                w!("EDIT"),
                setup.folder,
                WS_TABSTOP.0 | ES_AUTOHSCROLL,
                ID_FOLDER,
            )?,
            browse: child(w!("BUTTON"), tr("Browse..."), button, ID_BROWSE)?,
            back: child(w!("BUTTON"), tr("< Back"), button, ID_BACK)?,
            next: child(w!("BUTTON"), tr("Next >"), button, ID_NEXT)?,
            cancel: child(w!("BUTTON"), tr("Cancel"), button, ID_CANCEL)?,
            desktop: child(
                w!("BUTTON"),
                tr("Create a desktop shortcut"),
                button,
                ID_DESKTOP,
            )?,
            open: child(w!("BUTTON"), tr("Open Queqiao now"), button, ID_OPEN)?,
        };
        if setup.installed {
            // An update keeps its folder.
            unsafe {
                let _ = EnableWindow(wizard.folder, false);
                let _ = EnableWindow(wizard.browse, false);
            }
        }
        wizard.place();
        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOWNORMAL);
            let _ = SetForegroundWindow(hwnd);
            let _ = SetFocus(Some(wizard.next));
        }
        Ok(wizard)
    }

    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }

    pub fn page(&self) -> WizardPage {
        with_state(WizardPage::Welcome, |state| state.page)
    }

    /// The folder as typed.
    pub fn folder(&self) -> String {
        text_of(self.folder).trim().to_string()
    }

    /// The location page's line about room ("Space needed: 26 MB · available: 450.6 GB").
    pub fn set_space(&self, line: &str) {
        with_state((), |state| state.space = line.to_string());
        self.redraw();
    }

    /// Why the folder cannot be used (empty: none); the location page shows it in red.
    pub fn set_error(&self, error: &str) {
        with_state((), |state| state.error = error.to_string());
        self.redraw();
    }

    /// Goes to `page` (the installer: the installing and finish pages; tests: any, for its picture).
    pub fn show_page(&self, page: WizardPage) {
        with_state((), |state| state.page = page);
        self.place();
        let focus = match page {
            WizardPage::Location if !with_state(false, |state| state.installed) => self.folder,
            WizardPage::Installing => self.hwnd,
            _ => self.next,
        };
        let _ = unsafe { SetFocus(Some(focus)) };
    }

    /// The install started: its page, nothing to press until it ends.
    pub fn start_installing(&self) {
        with_state((), |state| {
            state.percent = 0;
            state.step = tr("Preparing...").to_string();
            state.log.clear();
        });
        self.show_page(WizardPage::Installing);
    }

    /// A step of the install: how far (0 to 100) and what it does (a new line in its list when it changed).
    pub fn progress(&self, percent: u32, step: &str) {
        with_state((), |state| {
            state.percent = percent.min(100);
            if state.step != step {
                state.step = step.to_string();
                state.log.push(step.to_string());
                if state.log.len() > 200 {
                    state.log.remove(0);
                }
            }
        });
        self.redraw();
    }

    /// The install ended: the finish page, with what happened.
    pub fn finished(&self, result: std::result::Result<(), String>) {
        let ok = result.is_ok();
        with_state((), |state| {
            if ok {
                state.percent = 100;
            }
            state.result = Some(result);
        });
        set_text(self.next, if ok { tr("Finish") } else { tr("Close") });
        self.show_page(WizardPage::Finished);
    }

    /// Handles the window's messages for `wait_ms` (it draws), without taking what the person did.
    pub fn pump(&self, wait_ms: u32) {
        crate::forms::pump_for(wait_ms, Some(self.hwnd));
    }

    pub fn save_picture(&self, path: &Path) -> std::io::Result<()> {
        save_window_picture(self.hwnd, path)
    }

    /// Handles messages for up to `wait_ms` and returns what the person did, if anything. Back and Next between the
    /// first pages, Browse and the check boxes are handled here.
    pub fn next(&mut self, wait_ms: u32) -> Option<WizardAction> {
        let command = next_command(self.hwnd, wait_ms)?;
        let page = self.page();
        match (command, page) {
            (ID_NEXT, WizardPage::Welcome) => {
                self.show_page(WizardPage::Location);
                Some(WizardAction::FolderChanged)
            }
            (ID_NEXT, WizardPage::Location) => Some(WizardAction::Install {
                folder: self.folder(),
                desktop: with_state(true, |state| state.desktop),
            }),
            (ID_NEXT, WizardPage::Finished) => Some(WizardAction::Finish {
                open: with_state(false, |state| {
                    state.open && state.result.as_ref().is_some_and(|r| r.is_ok())
                }),
            }),
            (ID_BACK, WizardPage::Location) => {
                self.set_error("");
                self.show_page(WizardPage::Welcome);
                None
            }
            (ID_BROWSE, WizardPage::Location) => {
                let chosen = pick_folder(self.hwnd, &self.folder())?;
                set_text(self.folder, &chosen);
                Some(WizardAction::FolderChanged)
            }
            (FOLDER_CHANGED, _) => Some(WizardAction::FolderChanged),
            (ID_DESKTOP | ID_OPEN, _) => {
                with_state((), |state| match command {
                    ID_DESKTOP => state.desktop = !state.desktop,
                    _ => state.open = !state.open,
                });
                for control in [self.desktop, self.open] {
                    let _ = unsafe { InvalidateRect(Some(control), None, false) };
                }
                None
            }
            // Nothing stops an install half way (it puts the earlier version back by itself when a step fails).
            (ID_CANCEL | CLOSED, WizardPage::Installing) => None,
            (ID_CANCEL | CLOSED, WizardPage::Finished) => {
                Some(WizardAction::Finish { open: false })
            }
            (ID_CANCEL | CLOSED, _) => Some(WizardAction::Cancel),
            _ => None,
        }
    }

    /// Puts the controls where the page wants them, the others hidden.
    fn place(&self) {
        let (scale, page) = with_state((1.0, WizardPage::Welcome), |state| {
            (state.scale, state.page)
        });
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
        let location = page == WizardPage::Location;
        let finished = page == WizardPage::Finished;
        let ok = with_state(false, |state| {
            state.result.as_ref().is_some_and(|r| r.is_ok())
        });
        put(self.folder, [LEFT + 10, 143, COLUMN - 116, 20], location);
        put(self.browse, [LEFT + COLUMN - 92, 136, 92, 34], location);
        put(self.desktop, [LEFT, 224, COLUMN, 26], location);
        put(self.open, [LEFT, 212, COLUMN, 26], finished && ok);
        put(self.back, [LEFT + COLUMN - 286, BUTTONS, 90, 34], !finished);
        put(self.next, [LEFT + COLUMN - 188, BUTTONS, 92, 34], !finished);
        put(
            self.cancel,
            [LEFT + COLUMN - 88, BUTTONS, 88, 34],
            !finished,
        );
        if finished {
            put(self.next, [LEFT + COLUMN - 92, BUTTONS, 92, 34], true);
        }
        let label = match page {
            WizardPage::Location => tr("Install"),
            WizardPage::Finished if ok => tr("Finish"),
            WizardPage::Finished => tr("Close"),
            _ => tr("Next >"),
        };
        set_text(self.next, label);
        unsafe {
            let installing = page == WizardPage::Installing;
            let _ = EnableWindow(self.back, page == WizardPage::Location);
            let _ = EnableWindow(self.next, !installing);
            let _ = EnableWindow(self.cancel, !installing);
        }
        self.redraw();
        for control in [self.back, self.next, self.cancel] {
            let _ = unsafe { InvalidateRect(Some(control), None, false) };
        }
    }

    fn redraw(&self) {
        let _ = unsafe { InvalidateRect(Some(self.hwnd), None, false) };
    }
}

impl Drop for SetupWizard {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
        if let Some(state) = STATE.with_borrow_mut(Option::take) {
            let f = state.fonts;
            for font in [f.title, f.body, f.bold, f.small, f.name] {
                let _ = unsafe { DeleteObject(HGDIOBJ(font.0)) };
            }
        }
        let hwnd = self.hwnd.0 as usize;
        while take_action(HWND(hwnd as *mut c_void)).is_some() {}
    }
}

/// Windows' folder dialog, starting at the folder typed (or the nearest one that exists); the folder chosen, with
/// "Queqiao" added unless it is called that (a folder of the person's holds other files: Queqiao gets its own in it).
fn pick_folder(owner: HWND, current: &str) -> Option<String> {
    use windows::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
        CoTaskMemFree,
    };
    use windows::Win32::UI::Shell::{
        FOS_FORCEFILESYSTEM, FOS_PICKFOLDERS, FileOpenDialog, IFileOpenDialog, IShellItem,
        SHCreateItemFromParsingName, SIGDN_FILESYSPATH,
    };
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let dialog: IFileOpenDialog =
            CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let options = dialog.GetOptions().ok()?;
        dialog
            .SetOptions(options | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM)
            .ok()?;
        let title: Vec<u16> = tr("Choose a folder for Queqiao")
            .encode_utf16()
            .chain(Some(0))
            .collect();
        let _ = dialog.SetTitle(PCWSTR(title.as_ptr()));
        if let Some(start) = Path::new(current).ancestors().find(|path| path.is_dir()) {
            let start: Vec<u16> = start
                .display()
                .to_string()
                .encode_utf16()
                .chain(Some(0))
                .collect();
            if let Ok(item) =
                SHCreateItemFromParsingName::<_, _, IShellItem>(PCWSTR(start.as_ptr()), None)
            {
                let _ = dialog.SetFolder(&item);
            }
        }
        dialog.Show(Some(owner)).ok()?;
        let item = dialog.GetResult().ok()?;
        let name = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let chosen = name.to_string().ok();
        CoTaskMemFree(Some(name.0 as *const c_void));
        let chosen = std::path::PathBuf::from(chosen?);
        let named = chosen
            .file_name()
            .is_some_and(|name| name.to_string_lossy().eq_ignore_ascii_case("Queqiao"));
        Some(
            if named {
                chosen
            } else {
                chosen.join("Queqiao")
            }
            .display()
            .to_string(),
        )
    }
}

/// The next command for the window: queued ones first, then messages for up to `wait_ms`, with Tab, Enter and Esc
/// handled as in a dialog.
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

extern "system" fn wizard_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_COMMAND => {
            let id = (wparam.0 & 0xFFFF) as u16;
            let code = ((wparam.0 >> 16) & 0xFFFF) as u16;
            match (id, code) {
                (ID_FOLDER, EN_SETFOCUS | EN_KILLFOCUS) => {
                    with_state((), |state| state.folder_focus = code == EN_SETFOCUS);
                    let _ = unsafe { InvalidateRect(Some(hwnd), None, false) };
                }
                (ID_FOLDER, EN_CHANGE) => push_action(hwnd, FOLDER_CHANGED),
                (ID_FOLDER, _) => {}
                (id, 0 | 1) if id != 0 => push_action(hwnd, id),
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
            let mut client = RECT::default();
            let _ = unsafe { GetClientRect(hwnd, &mut client) };
            // Drawn off screen first, then copied: the progress bar moves without flicker.
            unsafe {
                let memory = CreateCompatibleDC(Some(dc));
                let bitmap = CreateCompatibleBitmap(dc, client.right, client.bottom);
                let old = SelectObject(memory, bitmap.into());
                STATE.with_borrow(|state| {
                    if let Some(state) = state {
                        paint_window(memory, &client, state);
                    }
                });
                let _ = BitBlt(
                    dc,
                    0,
                    0,
                    client.right,
                    client.bottom,
                    Some(memory),
                    0,
                    0,
                    SRCCOPY,
                );
                SelectObject(memory, old);
                let _ = DeleteObject(bitmap.into());
                let _ = DeleteDC(memory);
                let _ = EndPaint(hwnd, &paint);
            }
            LRESULT(0)
        }
        // The folder's edit (and the same one disabled, for an update) sits on white.
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

/// A vertical gradient from `top` to `bottom` over `area`.
fn gradient(dc: HDC, area: &RECT, top: u32, bottom: u32) {
    let channel = |color: u32, shift: u32| (((color >> shift) & 0xFF) as u16) << 8;
    let vertex = |x: i32, y: i32, color: u32| TRIVERTEX {
        x,
        y,
        Red: channel(color, 0),
        Green: channel(color, 8),
        Blue: channel(color, 16),
        Alpha: 0,
    };
    let vertices = [
        vertex(area.left, area.top, top),
        vertex(area.right, area.bottom, bottom),
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
}

/// A check mark in `area`, `width` pixels thick.
fn check_mark(dc: HDC, area: &RECT, color: u32, width: i32) {
    let (w, h) = (area.right - area.left, area.bottom - area.top);
    unsafe {
        let pen = CreatePen(PS_SOLID, width.max(1), COLORREF(color));
        let old = SelectObject(dc, pen.into());
        let _ = MoveToEx(
            dc,
            area.left + w * 22 / 100,
            area.top + h * 52 / 100,
            None::<*mut POINT>,
        );
        let _ = LineTo(dc, area.left + w * 42 / 100, area.top + h * 72 / 100);
        let _ = LineTo(dc, area.left + w * 78 / 100, area.top + h * 32 / 100);
        SelectObject(dc, old);
        let _ = DeleteObject(pen.into());
    }
}

/// Wrapped text from `top` in the page's column; how tall it was.
fn paragraph(dc: HDC, value: &str, top: i32, font: HFONT, color: u32, s: f64) -> i32 {
    let width = px(COLUMN, s);
    let height = crate::main_window::wrapped_height(dc, value, font, width) + px(4, s);
    let area = RECT {
        left: px(LEFT, s),
        top,
        right: px(LEFT, s) + width,
        bottom: top + height,
    };
    text(dc, value, area, font, color, WRAP);
    height
}

/// The side panel, then the page.
fn paint_window(dc: HDC, client: &RECT, state: &State) {
    let s = state.scale;
    let f = &state.fonts;
    fill(dc, client, WHITE);
    paint_panel(dc, client, state);
    let (title, subtitle) = match state.page {
        WizardPage::Welcome => (
            tr("Welcome to Queqiao"),
            tr("This wizard installs Queqiao on this PC."),
        ),
        WizardPage::Location => (
            tr("Choose where to install"),
            tr("Queqiao's programs go into this folder."),
        ),
        WizardPage::Installing => (
            tr("Installing"),
            tr("Please wait while Queqiao is installed."),
        ),
        WizardPage::Finished => match &state.result {
            Some(Ok(())) => (
                tr("Queqiao is installed"),
                tr("Find it in the Start menu or on the desktop."),
            ),
            _ => (
                tr("Queqiao was not installed"),
                tr("Nothing of it was left half done."),
            ),
        },
    };
    text(
        dc,
        title,
        rect(LEFT, 30, COLUMN, 32, s),
        f.title,
        TEXT_COLOR,
        LINE,
    );
    text(
        dc,
        subtitle,
        rect(LEFT, 64, COLUMN, 20, s),
        f.body,
        SECONDARY,
        LINE,
    );
    match state.page {
        WizardPage::Welcome => paint_welcome(dc, state),
        WizardPage::Location => paint_location(dc, state),
        WizardPage::Installing => paint_installing(dc, state),
        WizardPage::Finished => paint_finished(dc, state),
    }
    // The line above the buttons.
    fill(
        dc,
        &RECT {
            left: px(PANEL, s),
            top: px(FOOT, s),
            right: client.right,
            bottom: px(FOOT, s) + px(1, s).max(1),
        },
        DIVIDER,
    );
}

/// The blue panel: the icon and Queqiao's name, the steps (done, this one, to come), the icon large, the version.
fn paint_panel(dc: HDC, client: &RECT, state: &State) {
    let s = state.scale;
    let f = &state.fonts;
    let panel = RECT {
        right: px(PANEL, s),
        ..*client
    };
    gradient(dc, &panel, PANEL_TOP, PANEL_BOTTOM);
    let icon = px(40, s);
    let _ = unsafe {
        DrawIconEx(
            dc,
            px(26, s),
            px(30, s),
            crate::icon::app_icon(64),
            icon,
            icon,
            0,
            None,
            DI_NORMAL,
        )
    };
    text(
        dc,
        tr("Queqiao"),
        rect(76, 28, PANEL - 84, 26, s),
        f.name,
        PANEL_TEXT,
        LINE,
    );
    text(
        dc,
        tr("Setup"),
        rect(76, 52, PANEL - 84, 18, s),
        f.small,
        PANEL_DIM,
        LINE,
    );
    let current = match state.page {
        WizardPage::Welcome => 0,
        WizardPage::Location => 1,
        WizardPage::Installing => 2,
        WizardPage::Finished => 3,
    };
    for (index, name) in [tr("Welcome"), tr("Location"), tr("Install"), tr("Finish")]
        .into_iter()
        .enumerate()
    {
        let top = 112 + 38 * index as i32;
        let circle = rect(28, top, 22, 22, s);
        let label = rect(62, top, PANEL - 70, 22, s);
        if index < current {
            rounded(dc, &circle, px(22, s), PANEL_DIM);
            check_mark(dc, &circle, PANEL_BOTTOM, px(2, s));
            text(dc, name, label, f.body, PANEL_DIM, LINE);
        } else if index == current {
            rounded(dc, &circle, px(22, s), PANEL_TEXT);
            text(
                dc,
                &(index + 1).to_string(),
                circle,
                f.bold,
                PANEL_BOTTOM,
                CENTRED,
            );
            text(dc, name, label, f.bold, PANEL_TEXT, LINE);
        } else {
            outlined(dc, &circle, px(22, s), PANEL_BOTTOM, PANEL_DIM);
            text(
                dc,
                &(index + 1).to_string(),
                circle,
                f.small,
                PANEL_DIM,
                CENTRED,
            );
            text(dc, name, label, f.body, PANEL_DIM, LINE);
        }
        // A thin line down to the next step.
        if index < 3 {
            fill(dc, &rect(38, top + 24, 2, 12, s), PANEL_DIM);
        }
    }
    // Queqiao's own icon, large (the user, 2026-11-27: the bird icon instead of the magpie with its wings spread).
    let size = px(88, s);
    let _ = unsafe {
        DrawIconEx(
            dc,
            (px(PANEL, s) - size) / 2,
            px(270, s),
            crate::icon::app_icon(size),
            size,
            size,
            0,
            None,
            DI_NORMAL,
        )
    };
    text(
        dc,
        &trf("Version {version}", &[("version", &state.version)]),
        rect(0, HEIGHT - 30, PANEL, 18, s),
        f.small,
        PANEL_DIM,
        CENTRED,
    );
}

fn paint_welcome(dc: HDC, state: &State) {
    let s = state.scale;
    let f = &state.fonts;
    let mut top = px(110, s);
    for line in [
        tr(
            "Queqiao lets you use your own computers from anywhere, and lets someone you trust help you.",
        ),
        tr(
            "It installs the Queqiao service too, so that the sign-in, lock and UAC screens can be shown in a session. Windows will ask once for an administrator's permission.",
        ),
    ] {
        top += paragraph(dc, line, top, f.body, TEXT_COLOR, s) + px(12, s);
    }
    paragraph(
        dc,
        tr("Click Next to continue."),
        top + px(8, s),
        f.body,
        SECONDARY,
        s,
    );
}

fn paint_location(dc: HDC, state: &State) {
    let s = state.scale;
    let f = &state.fonts;
    text(
        dc,
        tr("Install to"),
        rect(LEFT, 112, COLUMN, 18, s),
        f.small,
        SECONDARY,
        LINE,
    );
    // The edit's frame (the accent colour while it has the keyboard).
    let border = if state.folder_focus { ACCENT } else { FRAME };
    outlined(
        dc,
        &rect(LEFT, 136, COLUMN - 100, 34, s),
        px(8, s),
        WHITE,
        border,
    );
    let mut top = 178;
    if state.installed {
        text(
            dc,
            tr("Queqiao is installed here already: an update keeps its folder."),
            rect(LEFT, top, COLUMN, 18, s),
            f.small,
            SECONDARY,
            LINE,
        );
        top += 20;
    }
    text(
        dc,
        &state.space,
        rect(LEFT, top, COLUMN, 18, s),
        f.small,
        SECONDARY,
        LINE,
    );
    // The service, which every installation has.
    let note = rect(LEFT, 288, COLUMN, 50, s);
    rounded(dc, &note, px(10, s), NOTE);
    let inner = RECT {
        left: note.left + px(12, s),
        top: note.top + px(7, s),
        right: note.right - px(12, s),
        bottom: note.bottom - px(4, s),
    };
    text(
        dc,
        tr(
            "The Queqiao service is installed too (sign-in, lock and UAC screens in a session). Windows asks once for an administrator's permission.",
        ),
        inner,
        f.small,
        TEXT_COLOR,
        WRAP,
    );
    if !state.error.is_empty() {
        text(
            dc,
            &state.error,
            rect(LEFT, 340, COLUMN, 16, s),
            f.small,
            ERROR,
            LINE,
        );
    }
}

fn paint_installing(dc: HDC, state: &State) {
    let s = state.scale;
    let f = &state.fonts;
    text(
        dc,
        &state.step,
        rect(LEFT, 112, COLUMN - 50, 20, s),
        f.body,
        TEXT_COLOR,
        LINE,
    );
    text(
        dc,
        &format!("{}%", state.percent),
        rect(LEFT + COLUMN - 50, 112, 50, 20, s),
        f.bold,
        ACCENT,
        LINE | 0x2, // DT_RIGHT
    );
    // The bar: a pale track, the accent part done.
    let track = rect(LEFT, 140, COLUMN, 8, s);
    rounded(dc, &track, px(8, s), DIVIDER);
    let done = (track.right - track.left) * state.percent as i32 / 100;
    if done > 0 {
        rounded(
            dc,
            &RECT {
                right: track.left + done.max(px(8, s)),
                ..track
            },
            px(8, s),
            ACCENT,
        );
    }
    // The steps so far, the newest at the foot.
    let box_area = rect(LEFT, 164, COLUMN, 178, s);
    rounded(dc, &box_area, px(10, s), LOG_BOX);
    let row = px(19, s);
    let rows = ((box_area.bottom - box_area.top - px(16, s)) / row).max(1) as usize;
    let shown = &state.log[state.log.len().saturating_sub(rows)..];
    for (index, line) in shown.iter().enumerate() {
        let top = box_area.top + px(8, s) + row * index as i32;
        let area = RECT {
            left: box_area.left + px(14, s),
            top,
            right: box_area.right - px(14, s),
            bottom: top + row,
        };
        let last = index + 1 == shown.len();
        text(
            dc,
            line,
            area,
            f.small,
            if last { TEXT_COLOR } else { SECONDARY },
            LINE,
        );
    }
}

fn paint_finished(dc: HDC, state: &State) {
    let s = state.scale;
    let f = &state.fonts;
    match &state.result {
        Some(Ok(())) => {
            let circle = rect(LEFT, 112, 44, 44, s);
            rounded(dc, &circle, px(44, s), SUCCESS);
            let inner = RECT {
                left: circle.left + px(8, s),
                top: circle.top + px(8, s),
                right: circle.right - px(8, s),
                bottom: circle.bottom - px(8, s),
            };
            check_mark(dc, &inner, WHITE, px(3, s));
            text(
                dc,
                tr("Queqiao and its service are running, and start with Windows."),
                rect(LEFT + 58, 112, COLUMN - 58, 44, s),
                f.body,
                TEXT_COLOR,
                WRAP | 0x4, // DT_VCENTER has no effect on wrapped text: placed by the rectangle
            );
        }
        Some(Err(error)) => {
            let note = rect(LEFT, 110, COLUMN, 160, s);
            rounded(dc, &note, px(10, s), 0x00EE_EEFD);
            let inner = RECT {
                left: note.left + px(14, s),
                top: note.top + px(12, s),
                right: note.right - px(14, s),
                bottom: note.bottom - px(12, s),
            };
            text(dc, error, inner, f.body, TEXT_COLOR, WRAP);
        }
        None => {}
    }
}

/// The drawn buttons: Next/Install/Finish (filled in the accent colour, grey while it cannot be pressed), Back, Cancel
/// and Browse (outlined), and the check boxes (a rounded box, ticked in the accent colour, and their label).
fn draw_button(item: &DRAWITEMSTRUCT, state: &State) {
    let s = state.scale;
    let f = &state.fonts;
    let dc = item.hDC;
    let area = item.rcItem;
    let label = text_of(item.hwndItem);
    let enabled = item.itemState.0 & ODS_DISABLED == 0;
    fill(dc, &area, WHITE);
    match item.CtlID as u16 {
        ID_NEXT => {
            rounded(
                dc,
                &area,
                px(8, s),
                if enabled { ACCENT } else { BUTTON_OFF },
            );
            text(dc, &label, area, f.bold, WHITE, CENTRED);
        }
        id @ (ID_DESKTOP | ID_OPEN) => {
            let checked = match id {
                ID_DESKTOP => state.desktop,
                _ => state.open,
            };
            let size = px(18, s);
            let middle = (area.top + area.bottom) / 2;
            let tick = RECT {
                left: area.left,
                top: middle - size / 2,
                right: area.left + size,
                bottom: middle - size / 2 + size,
            };
            if checked {
                rounded(dc, &tick, px(6, s), ACCENT);
                check_mark(dc, &tick, WHITE, px(2, s));
            } else {
                outlined(dc, &tick, px(6, s), WHITE, FRAME);
            }
            let words = RECT {
                left: tick.right + px(10, s),
                ..area
            };
            text(dc, &label, words, f.body, TEXT_COLOR, LINE);
            if item.itemState.0 & ODS_FOCUS != 0 && item.itemState.0 & ODS_NOFOCUSRECT == 0 {
                let width = text_width(dc, &label, f.body);
                let focus = RECT {
                    left: words.left - px(2, s),
                    right: (words.left + width + px(2, s)).min(area.right),
                    ..area
                };
                let _ = unsafe { DrawFocusRect(dc, &focus) };
            }
            return;
        }
        _ => {
            outlined(
                dc,
                &area,
                px(8, s),
                WHITE,
                if enabled { FRAME } else { DIVIDER },
            );
            text(
                dc,
                &label,
                area,
                f.body,
                if enabled { TEXT_COLOR } else { BUTTON_OFF },
                CENTRED,
            );
        }
    }
    if item.itemState.0 & ODS_FOCUS != 0 && item.itemState.0 & ODS_NOFOCUSRECT == 0 {
        let inner = RECT {
            left: area.left + px(4, s),
            top: area.top + px(4, s),
            right: area.right - px(4, s),
            bottom: area.bottom - px(4, s),
        };
        let _ = unsafe { DrawFocusRect(dc, &inner) };
    }
}
