//! The file transfer window on Windows (Linux: linux/file_window.rs), laid out as UU Remote's: this PC's files on the
//! left and the other PC's on the right, each with back, up, refresh, a folder box (its list: the person's folders
//! and the drives) and the folder's entries with check boxes; "Send →" sends what is checked on the left into the
//! folder shown on the right, "← Send" the other way; files dropped from Explorer go to the right-hand folder. Below,
//! the transfer list: each thing sent or fetched with its state, size, from and to, and its buttons (show in folder,
//! pause or resume, remove), with pause all, start all, cancel all and clear finished above it.
//!
//! In Queqiao's colours (night-blue title bar, white cards on a pale page, gold and night-blue accents), with the
//! standard list and folder-box controls (the Explorer look, the system's file icons). It is polled from the session's
//! loop as the other forms are: what the person does comes from `next`; what it shows is set with `set_pane` and
//! `set_jobs`.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::Once;

use queqiao_core::text::{tr, trf};
use windows::Win32::Foundation::{
    COLORREF, FILETIME, HWND, LPARAM, LRESULT, POINT, RECT, SYSTEMTIME, WPARAM,
};
use windows::Win32::Graphics::Dwm::{DWMWINDOWATTRIBUTE, DwmSetWindowAttribute};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, CreateFontW, CreateSolidBrush,
    DEFAULT_CHARSET, DT_CENTER, DT_END_ELLIPSIS, DT_LEFT, DT_NOPREFIX, DT_RIGHT, DT_SINGLELINE,
    DT_VCENTER, DeleteObject, DrawTextW, EndPaint, FF_DONTCARE, FillRect, GetStockObject, HBRUSH,
    HDC, HFONT, InvalidateRect, NULL_PEN, OUT_DEFAULT_PRECIS, PAINTSTRUCT, RoundRect, SelectObject,
    SetBkMode, SetTextColor, TRANSPARENT,
};
use windows::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL, FILE_FLAGS_AND_ATTRIBUTES,
    FileTimeToLocalFileTime,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Time::FileTimeToSystemTime;
use windows::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForWindow};
use windows::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, VK_BACK, VK_F5, VK_RETURN};
use windows::Win32::UI::Shell::{
    DefSubclassProc, DragAcceptFiles, DragFinish, DragQueryFileW, HDROP, SHFILEINFOW,
    SHGFI_SMALLICON, SHGFI_SYSICONINDEX, SHGFI_USEFILEATTRIBUTES, SHGetFileInfoW,
    SetWindowSubclass,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, Result, w};

use crate::model::{
    FileItem, FileKind, FilePane, FileWindowEvent, JobCommand, JobRow, JobRowState, size_text,
};

const CLASS: PCWSTR = w!("QueqiaoFiles");
static REGISTER: Once = Once::new();

// Colours (COLORREF: 0x00BBGGRR), as the main window's.
const NIGHT: u32 = 0x003B_1A14;
const PAGE: u32 = 0x00F8_F5F4;
const CARD: u32 = 0x00FF_FFFF;
const CARD_LINE: u32 = 0x00EE_E6E3;
const TEXT_COLOR: u32 = 0x0040_211B;
const SECONDARY: u32 = 0x008C_6B64;
const DISABLED: u32 = 0x00CC_B9B4;
const PRESSED: u32 = 0x00F2_E9E6;
const GOLD: u32 = 0x005A_B4E3;
const ONLINE: u32 = 0x0047_9A2A;
const FAILED: u32 = 0x003C_4AC8;
const TRACK: u32 = 0x00EE_E6E3;
const ACCENT: u32 = 0x00AE_5F3D;
const CHIP_HERE: (u32, u32) = (0x00FB_EDE8, 0x00AE_5F3D);
const CHIP_THERE: (u32, u32) = (0x00EC_F6E3, 0x004A_7817);
const WHITE: u32 = 0x00FF_FFFF;

// Control ids.
const ID_LIST: [u16; 2] = [100, 101];
const ID_FOLDER: [u16; 2] = [110, 111];
const ID_BACK: [u16; 2] = [120, 121];
const ID_UP: [u16; 2] = [130, 131];
const ID_REFRESH: [u16; 2] = [140, 141];
const ID_SEND: [u16; 2] = [150, 151];
const ID_JOBS: u16 = 160;
const ID_PAUSE_ALL: u16 = 170;
const ID_RESUME_ALL: u16 = 171;
const ID_CANCEL_ALL: u16 = 172;
const ID_CLEAR: u16 = 173;
/// The folder box's Enter (sent by its edit field's subclass).
const ID_FOLDER_ENTER: [u16; 2] = [180, 181];

// List view and combo box messages and styles (commctrl.h).
const LVM_SETIMAGELIST: u32 = 0x1003;
const LVM_GETITEMCOUNT: u32 = 0x1004;
const LVM_DELETEALLITEMS: u32 = 0x1009;
const LVM_SETCOLUMNWIDTH: u32 = 0x101E;
const LVM_GETHEADER: u32 = 0x101F;
const LVM_GETITEMSTATE: u32 = 0x102C;
const LVM_SETEXTENDEDLISTVIEWSTYLE: u32 = 0x1036;
const LVM_SUBITEMHITTEST: u32 = 0x1039;
const LVM_INSERTITEMW: u32 = 0x104D;
const LVM_INSERTCOLUMNW: u32 = 0x1061;
const LVM_SETITEMTEXTW: u32 = 0x1074;
const LVM_GETITEMW: u32 = 0x104B;
const LVS_REPORT: u32 = 0x1;
const LVS_SINGLESEL: u32 = 0x4;
const LVS_SHOWSELALWAYS: u32 = 0x8;
const LVS_SHAREIMAGELISTS: u32 = 0x40;
const LVS_NOSORTHEADER: u32 = 0x8000;
const LVS_EX_CHECKBOXES: usize = 0x4;
const LVS_EX_FULLROWSELECT: usize = 0x20;
const LVS_EX_LABELTIP: usize = 0x4000;
const LVS_EX_DOUBLEBUFFER: usize = 0x10000;
const LVIF_TEXT: u32 = 0x1;
const LVIF_IMAGE: u32 = 0x2;
const LVIF_PARAM: u32 = 0x4;
const LVIS_SELECTED: u32 = 0x2;
const LVIS_STATEIMAGEMASK: u32 = 0xF000;
const LVCF_FMT: u32 = 0x1;
const LVCF_WIDTH: u32 = 0x2;
const LVCF_TEXT: u32 = 0x4;
const LVCFMT_RIGHT: i32 = 0x1;
const NM_CLICK: u32 = (-2i32) as u32;
const NM_DBLCLK: u32 = (-3i32) as u32;
const NM_RCLICK: u32 = (-5i32) as u32;
const NM_CUSTOMDRAW: u32 = (-12i32) as u32;
const LVN_ITEMCHANGING: u32 = (-100i32) as u32;
const LVN_ITEMCHANGED: u32 = (-101i32) as u32;
const LVN_KEYDOWN: u32 = (-155i32) as u32;
const CDDS_PREPAINT: u32 = 0x1;
const CDDS_ITEMPREPAINT: u32 = 0x10001;
const CDDS_SUBITEM: u32 = 0x20000;
const CDRF_DODEFAULT: isize = 0;
const CDRF_SKIPDEFAULT: isize = 0x4;
const CDRF_NOTIFYITEMDRAW: isize = 0x20;
const CDRF_NOTIFYSUBITEMDRAW: isize = 0x20;
const CBS_DROPDOWN: u32 = 0x2;
const CBS_AUTOHSCROLL: u32 = 0x40;
const CB_ADDSTRING: u32 = 0x143;
const CB_GETCURSEL: u32 = 0x147;
const CB_RESETCONTENT: u32 = 0x14B;
const CB_SETMINVISIBLE: u32 = 0x1701;
const CBN_SELCHANGE: u16 = 1;
const BS_OWNERDRAW: u32 = 0xB;
const ODS_SELECTED: u32 = 0x1;
const ODS_DISABLED: u32 = 0x4;
const LVSIL_SMALL: usize = 1;

// Window attributes (dwmapi.h) for the title bar.
const DWMWA_USE_IMMERSIVE_DARK_MODE: DWMWINDOWATTRIBUTE = DWMWINDOWATTRIBUTE(20);
const DWMWA_BORDER_COLOR: DWMWINDOWATTRIBUTE = DWMWINDOWATTRIBUTE(34);
const DWMWA_CAPTION_COLOR: DWMWINDOWATTRIBUTE = DWMWINDOWATTRIBUTE(35);
const DWMWA_TEXT_COLOR: DWMWINDOWATTRIBUTE = DWMWINDOWATTRIBUTE(36);

// Glyphs of Segoe MDL2 Assets.
const GLYPH_BACK: &str = "\u{E72B}";
const GLYPH_UP: &str = "\u{E74A}";
const GLYPH_REFRESH: &str = "\u{E72C}";
const GLYPH_PAUSE: &str = "\u{E769}";
const GLYPH_PLAY: &str = "\u{E768}";
const GLYPH_CANCEL: &str = "\u{E711}";
const GLYPH_CLEAR: &str = "\u{E74D}";
const GLYPH_FOLDER: &str = "\u{E838}";

// Layout in 96-DPI units.
const MARGIN: i32 = 16;
const HEADER: i32 = 44;
const TOOLBAR: i32 = 32;
const SEND_WIDTH: i32 = 96;
const ACTIONS_WIDTH: i32 = 72;

#[repr(C)]
struct NmHdr {
    hwnd_from: HWND,
    id_from: usize,
    code: u32,
}

#[repr(C)]
struct LvItem {
    mask: u32,
    item: i32,
    sub_item: i32,
    state: u32,
    state_mask: u32,
    text: *mut u16,
    text_max: i32,
    image: i32,
    param: isize,
    indent: i32,
    group_id: i32,
    columns: u32,
    column_list: *mut u32,
    column_formats: *mut i32,
    group: i32,
}

impl Default for LvItem {
    fn default() -> Self {
        LvItem {
            mask: 0,
            item: 0,
            sub_item: 0,
            state: 0,
            state_mask: 0,
            text: std::ptr::null_mut(),
            text_max: 0,
            image: 0,
            param: 0,
            indent: 0,
            group_id: 0,
            columns: 0,
            column_list: std::ptr::null_mut(),
            column_formats: std::ptr::null_mut(),
            group: 0,
        }
    }
}

#[repr(C)]
struct LvColumn {
    mask: u32,
    format: i32,
    width: i32,
    text: *mut u16,
    text_max: i32,
    sub_item: i32,
    image: i32,
    order: i32,
    min_width: i32,
    default_width: i32,
    ideal_width: i32,
}

#[repr(C)]
struct LvHitTest {
    point: POINT,
    flags: u32,
    item: i32,
    sub_item: i32,
    group: i32,
}

#[repr(C)]
struct NmCustomDraw {
    hdr: NmHdr,
    stage: u32,
    hdc: HDC,
    rect: RECT,
    item_spec: usize,
    item_state: u32,
    item_param: isize,
}

#[repr(C)]
struct NmLvCustomDraw {
    draw: NmCustomDraw,
    text_color: u32,
    text_back: u32,
    sub_item: i32,
}

#[repr(C)]
struct NmItemActivate {
    hdr: NmHdr,
    item: i32,
    sub_item: i32,
    new_state: u32,
    old_state: u32,
    changed: u32,
    point: POINT,
    param: isize,
    key_flags: u32,
}

#[repr(C)]
struct NmListView {
    hdr: NmHdr,
    item: i32,
    sub_item: i32,
    new_state: u32,
    old_state: u32,
    changed: u32,
    point: POINT,
    param: isize,
}

#[repr(C)]
struct NmLvKeyDown {
    hdr: NmHdr,
    key: u16,
}

#[repr(C)]
struct DrawItem {
    control_type: u32,
    control_id: u32,
    item_id: u32,
    action: u32,
    state: u32,
    hwnd: HWND,
    hdc: HDC,
    rect: RECT,
    data: usize,
}

/// How an owner-drawn button looks.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Look {
    /// Night-blue with white text (the Send buttons).
    Primary,
    /// A glyph alone (back, up, refresh).
    Glyph,
    /// A glyph and a label (the transfer list's buttons).
    Text,
}

struct Button {
    hwnd: HWND,
    look: Look,
    glyph: &'static str,
    label: String,
}

struct Fonts {
    text: HFONT,
    bold: HFONT,
    title: HFONT,
    glyph: HFONT,
    small: HFONT,
}

struct State {
    scale: f64,
    fonts: Fonts,
    names: [String; 2],
    lists: [HWND; 2],
    folders: [HWND; 2],
    jobs_list: HWND,
    buttons: HashMap<u16, Button>,
    panes: [FilePane; 2],
    jobs: Vec<JobRow>,
    events: Vec<FileWindowEvent>,
    /// System icon index by file extension (and the folder's, the drive's).
    icons: HashMap<String, i32>,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
    /// While a list is filled: its item changes are not the person's.
    static FILLING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

fn px(value: i32, scale: f64) -> i32 {
    (f64::from(value) * scale).round() as i32
}

fn brush(color: u32) -> HBRUSH {
    unsafe { CreateSolidBrush(COLORREF(color)) }
}

fn fill(dc: HDC, area: &RECT, color: u32) {
    let brush = brush(color);
    unsafe {
        FillRect(dc, area, brush);
        let _ = DeleteObject(brush.into());
    }
}

fn round_fill(dc: HDC, area: &RECT, radius: i32, color: u32, line: Option<u32>) {
    unsafe {
        let brush = brush(color);
        let pen = line.map(|line| {
            windows::Win32::Graphics::Gdi::CreatePen(
                windows::Win32::Graphics::Gdi::PS_SOLID,
                1,
                COLORREF(line),
            )
        });
        let old_brush = SelectObject(dc, brush.into());
        let old_pen = SelectObject(
            dc,
            match pen {
                Some(pen) => pen.into(),
                None => GetStockObject(NULL_PEN),
            },
        );
        let _ = RoundRect(
            dc,
            area.left,
            area.top,
            area.right,
            area.bottom,
            radius,
            radius,
        );
        SelectObject(dc, old_pen);
        SelectObject(dc, old_brush);
        let _ = DeleteObject(brush.into());
        if let Some(pen) = pen {
            let _ = DeleteObject(pen.into());
        }
    }
}

fn draw_text(dc: HDC, value: &str, area: RECT, font: HFONT, color: u32, align: u32) {
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
            windows::Win32::Graphics::Gdi::DRAW_TEXT_FORMAT(
                align | DT_SINGLELINE.0 | DT_VCENTER.0 | DT_END_ELLIPSIS.0 | DT_NOPREFIX.0,
            ),
        );
        SelectObject(dc, old);
    }
}

fn text_width(dc: HDC, value: &str, font: HFONT) -> i32 {
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
                0x400 | DT_SINGLELINE.0 | DT_NOPREFIX.0,
            ),
        );
        SelectObject(dc, old);
    }
    area.right - area.left
}

/// Seconds since 1970 as this PC's local date and time ("2026/06/30 17:55"); "" when unknown.
fn time_text(seconds: i64) -> String {
    if seconds <= 0 {
        return String::new();
    }
    let ticks = (seconds as u64 + 11_644_473_600) * 10_000_000;
    let utc = FILETIME {
        dwLowDateTime: ticks as u32,
        dwHighDateTime: (ticks >> 32) as u32,
    };
    let mut local = FILETIME::default();
    let mut time = SYSTEMTIME::default();
    if unsafe { FileTimeToLocalFileTime(&utc, &mut local) }.is_err()
        || unsafe { FileTimeToSystemTime(&local, &mut time) }.is_err()
    {
        return String::new();
    }
    format!(
        "{}/{:02}/{:02} {:02}:{:02}",
        time.wYear, time.wMonth, time.wDay, time.wHour, time.wMinute
    )
}

/// The entry's type as UU Remote shows it: the extension, or what it is.
fn type_text(item: &FileItem) -> String {
    match item.kind {
        FileKind::Folder => tr("Folder").to_string(),
        FileKind::Drive => tr("Drive").to_string(),
        FileKind::Place => tr("Folder").to_string(),
        FileKind::File => Path::new(&item.name)
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default(),
    }
}

/// Opens Explorer at `path` with it selected (a file or folder that arrived).
pub fn show_in_folder(path: &Path) {
    let _ = std::process::Command::new("explorer.exe")
        .arg(format!("/select,{}", path.display()))
        .spawn();
}

pub struct FileWindow {
    hwnd: HWND,
}

impl FileWindow {
    /// The window for a session between `this_pc` and `that_pc` (their names, shown above the panes).
    pub fn new(this_pc: &str, that_pc: &str) -> Result<FileWindow> {
        let instance = unsafe { GetModuleHandleW(None) }?;
        REGISTER.call_once(|| unsafe {
            let class = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(files_proc),
                hInstance: instance.into(),
                hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                hIcon: crate::icon::app_icon(32),
                hIconSm: crate::icon::app_icon(16),
                lpszClassName: CLASS,
                ..Default::default()
            };
            RegisterClassExW(&class);
        });
        let title = wide(&trf(
            "Queqiao - file transfer with {name}",
            &[("name", that_pc)],
        ));
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_CONTROLPARENT,
                CLASS,
                PCWSTR(title.as_ptr()),
                WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                1000,
                680,
                None,
                None,
                Some(instance.into()),
                None,
            )
        }?;
        let scale = f64::from(unsafe { GetDpiForWindow(hwnd) }.max(96)) / 96.0;
        // The title bar in Queqiao's night blue (Windows 11; earlier Windows keep their own).
        unsafe {
            let dark: i32 = 1;
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_USE_IMMERSIVE_DARK_MODE,
                std::ptr::from_ref(&dark).cast(),
                size_of::<i32>() as u32,
            );
            for (attribute, value) in [
                (DWMWA_CAPTION_COLOR, NIGHT),
                (DWMWA_BORDER_COLOR, NIGHT),
                (DWMWA_TEXT_COLOR, WHITE),
            ] {
                let _ = DwmSetWindowAttribute(
                    hwnd,
                    attribute,
                    std::ptr::from_ref(&value).cast(),
                    size_of::<u32>() as u32,
                );
            }
        }
        // 1000 x 680 at 96 DPI, centred on its monitor.
        let mut frame = RECT {
            left: 0,
            top: 0,
            right: px(1000, scale),
            bottom: px(680, scale),
        };
        let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96);
        let _ = unsafe {
            AdjustWindowRectExForDpi(
                &mut frame,
                WS_OVERLAPPEDWINDOW,
                false,
                WS_EX_CONTROLPARENT,
                dpi,
            )
        };
        let (width, height) = (frame.right - frame.left, frame.bottom - frame.top);
        let screen = (unsafe { GetSystemMetrics(SM_CXSCREEN) }, unsafe {
            GetSystemMetrics(SM_CYSCREEN)
        });
        let _ = unsafe {
            SetWindowPos(
                hwnd,
                None,
                ((screen.0 - width) / 2).max(0),
                ((screen.1 - height) / 2).max(0),
                width,
                height,
                SWP_NOZORDER,
            )
        };
        let font = |size: i32, weight: i32, face: PCWSTR| unsafe {
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
        };
        let fonts = Fonts {
            text: font(13, 400, w!("Segoe UI")),
            bold: font(14, 600, w!("Segoe UI")),
            title: font(18, 700, w!("Segoe UI")),
            glyph: font(14, 400, w!("Segoe MDL2 Assets")),
            small: font(12, 400, w!("Segoe UI")),
        };
        let child = |class: PCWSTR, text: &str, style: u32, id: u16| -> HWND {
            let text = wide(text);
            unsafe {
                CreateWindowExW(
                    WINDOW_EX_STYLE(0),
                    class,
                    PCWSTR(text.as_ptr()),
                    WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | style),
                    0,
                    0,
                    10,
                    10,
                    Some(hwnd),
                    Some(HMENU(usize::from(id) as *mut c_void)),
                    Some(instance.into()),
                    None,
                )
            }
            .unwrap_or_default()
        };
        let set_font = |control: HWND, font: HFONT| unsafe {
            SendMessageW(
                control,
                WM_SETFONT,
                Some(WPARAM(font.0 as usize)),
                Some(LPARAM(1)),
            );
        };
        // The system's small file icons for the lists.
        let mut info = SHFILEINFOW::default();
        let images = unsafe {
            SHGetFileInfoW(
                w!("C:\\"),
                FILE_FLAGS_AND_ATTRIBUTES(0),
                Some(&mut info),
                size_of::<SHFILEINFOW>() as u32,
                SHGFI_SYSICONINDEX | SHGFI_SMALLICON,
            )
        };
        let mut lists = [HWND::default(); 2];
        let mut folders = [HWND::default(); 2];
        let mut buttons = HashMap::new();
        for side in 0..2 {
            let list = child(
                w!("SysListView32"),
                "",
                LVS_REPORT | LVS_SHOWSELALWAYS | LVS_SHAREIMAGELISTS | LVS_NOSORTHEADER,
                ID_LIST[side],
            );
            unsafe {
                SendMessageW(
                    list,
                    LVM_SETEXTENDEDLISTVIEWSTYLE,
                    Some(WPARAM(0)),
                    Some(LPARAM(
                        (LVS_EX_CHECKBOXES
                            | LVS_EX_FULLROWSELECT
                            | LVS_EX_DOUBLEBUFFER
                            | LVS_EX_LABELTIP) as isize,
                    )),
                );
                SendMessageW(
                    list,
                    LVM_SETIMAGELIST,
                    Some(WPARAM(LVSIL_SMALL)),
                    Some(LPARAM(images as isize)),
                );
                let _ = windows::Win32::UI::Controls::SetWindowTheme(
                    list,
                    w!("Explorer"),
                    PCWSTR::null(),
                );
            }
            set_font(list, fonts.text);
            set_font(
                HWND(unsafe { SendMessageW(list, LVM_GETHEADER, None, None) }.0 as *mut c_void),
                fonts.text,
            );
            for (index, (title, width, right)) in [
                (tr("Name"), 200, false),
                (tr("Modified"), 124, false),
                (tr("Type"), 64, false),
                (tr("Size"), 76, true),
            ]
            .into_iter()
            .enumerate()
            {
                add_column(list, index, title, px(width, scale), right);
            }
            lists[side] = list;
            let folder = child(
                w!("COMBOBOX"),
                "",
                CBS_DROPDOWN | CBS_AUTOHSCROLL | WS_VSCROLL.0,
                ID_FOLDER[side],
            );
            set_font(folder, fonts.text);
            unsafe { SendMessageW(folder, CB_SETMINVISIBLE, Some(WPARAM(12)), None) };
            // Enter in its edit field opens the typed folder.
            if let Ok(edit) =
                unsafe { FindWindowExW(Some(folder), None, w!("Edit"), PCWSTR::null()) }
            {
                let _ = unsafe {
                    SetWindowSubclass(
                        edit,
                        Some(folder_edit_proc),
                        usize::from(ID_FOLDER_ENTER[side]),
                        hwnd.0 as usize,
                    )
                };
            }
            folders[side] = folder;
            for (id, look, glyph, label) in [
                (ID_BACK[side], Look::Glyph, GLYPH_BACK, tr("Back")),
                (ID_UP[side], Look::Glyph, GLYPH_UP, tr("Up")),
                (ID_REFRESH[side], Look::Glyph, GLYPH_REFRESH, tr("Refresh")),
                (
                    ID_SEND[side],
                    Look::Primary,
                    "",
                    if side == 0 {
                        tr("Send →")
                    } else {
                        tr("← Send")
                    },
                ),
            ] {
                let button = child(w!("BUTTON"), label, BS_OWNERDRAW, id);
                buttons.insert(
                    id,
                    Button {
                        hwnd: button,
                        look,
                        glyph,
                        label: label.to_string(),
                    },
                );
            }
        }
        let jobs_list = child(
            w!("SysListView32"),
            "",
            LVS_REPORT | LVS_SINGLESEL | LVS_SHAREIMAGELISTS | LVS_NOSORTHEADER,
            ID_JOBS,
        );
        unsafe {
            SendMessageW(
                jobs_list,
                LVM_SETEXTENDEDLISTVIEWSTYLE,
                Some(WPARAM(0)),
                Some(LPARAM(
                    (LVS_EX_FULLROWSELECT | LVS_EX_DOUBLEBUFFER | LVS_EX_LABELTIP) as isize,
                )),
            );
            SendMessageW(
                jobs_list,
                LVM_SETIMAGELIST,
                Some(WPARAM(LVSIL_SMALL)),
                Some(LPARAM(images as isize)),
            );
        }
        set_font(jobs_list, fonts.text);
        // The column headings in the rows' font (they keep the system's larger one otherwise).
        set_font(
            HWND(unsafe { SendMessageW(jobs_list, LVM_GETHEADER, None, None) }.0 as *mut c_void),
            fonts.text,
        );
        for (index, (title, width, right)) in [
            (tr("Name"), 210, false),
            (tr("Status"), 130, false),
            (tr("Size"), 120, true),
            (tr("From"), 190, false),
            (tr("To"), 190, false),
            ("", ACTIONS_WIDTH, false),
        ]
        .into_iter()
        .enumerate()
        {
            add_column(jobs_list, index, title, px(width, scale), right);
        }
        for (id, glyph, label) in [
            (ID_PAUSE_ALL, GLYPH_PAUSE, tr("Pause all")),
            (ID_RESUME_ALL, GLYPH_PLAY, tr("Start all")),
            (ID_CANCEL_ALL, GLYPH_CANCEL, tr("Cancel all")),
            (ID_CLEAR, GLYPH_CLEAR, tr("Clear finished")),
        ] {
            let button = child(w!("BUTTON"), label, BS_OWNERDRAW, id);
            buttons.insert(
                id,
                Button {
                    hwnd: button,
                    look: Look::Text,
                    glyph,
                    label: label.to_string(),
                },
            );
        }
        unsafe { DragAcceptFiles(hwnd, true) };
        STATE.with_borrow_mut(|state| {
            *state = Some(State {
                scale,
                fonts,
                names: [this_pc.to_string(), that_pc.to_string()],
                lists,
                folders,
                jobs_list,
                buttons,
                panes: [FilePane::default(), FilePane::default()],
                jobs: Vec::new(),
                events: Vec::new(),
                icons: HashMap::new(),
            })
        });
        layout(hwnd);
        refresh_buttons();
        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = SetForegroundWindow(hwnd);
        }
        Ok(FileWindow { hwnd })
    }

    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }

    /// Brings the window to the front (it was opened already).
    pub fn show(&self) {
        unsafe {
            if IsIconic(self.hwnd).as_bool() {
                let _ = ShowWindow(self.hwnd, SW_RESTORE);
            }
            let _ = SetForegroundWindow(self.hwnd);
        }
    }

    /// What a pane shows (`remote`: the other PC's, on the right); redrawn only when it changed.
    pub fn set_pane(&mut self, remote: bool, pane: &FilePane) {
        let side = usize::from(remote);
        let changed = STATE.with_borrow(|state| {
            state
                .as_ref()
                .is_some_and(|state| state.panes[side] != *pane)
        });
        if !changed {
            return;
        }
        let folder_changed = STATE.with_borrow(|state| {
            state.as_ref().is_some_and(|state| {
                state.panes[side].folder != pane.folder || state.panes[side].items != pane.items
            })
        });
        STATE.with_borrow_mut(|state| {
            if let Some(state) = state {
                state.panes[side] = pane.clone();
            }
        });
        if folder_changed {
            fill_pane(side);
        }
        fill_folder_box(side);
        refresh_buttons();
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, true);
        }
    }

    /// The transfer list's rows.
    pub fn set_jobs(&mut self, jobs: &[JobRow]) {
        let same =
            STATE.with_borrow(|state| state.as_ref().is_some_and(|state| state.jobs == jobs));
        if same {
            return;
        }
        STATE.with_borrow_mut(|state| {
            if let Some(state) = state {
                state.jobs = jobs.to_vec();
            }
        });
        fill_jobs();
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    /// What the person did since the last call (`FileWindowEvent::Closed` when they closed the window).
    pub fn next_event(&mut self) -> Option<FileWindowEvent> {
        STATE.with_borrow_mut(|state| {
            let state = state.as_mut()?;
            (!state.events.is_empty()).then(|| state.events.remove(0))
        })
    }

    /// Tests: a picture of the window (its own content, also where other windows cover it).
    pub fn save_picture(&self, path: &Path) -> std::io::Result<()> {
        crate::forms::save_window_picture(self.hwnd, path)
    }

    /// Tests: checks the entry `name` of a pane, as a click on its box would.
    pub fn check_for_test(&self, remote: bool, name: &str) -> bool {
        let side = usize::from(remote);
        let Some((list, index)) = STATE.with_borrow(|state| {
            let state = state.as_ref()?;
            let index = state.panes[side]
                .items
                .iter()
                .position(|item| item.name == name)?;
            Some((state.lists[side], index))
        }) else {
            return false;
        };
        set_checked(list, index, true);
        true
    }

    /// Tests: presses a pane's Send button.
    pub fn send_for_test(&self, remote: bool) {
        push(FileWindowEvent::Send {
            from_remote: remote,
            names: checked_names(usize::from(remote)),
        });
    }
}

impl Drop for FileWindow {
    fn drop(&mut self) {
        let state = STATE.with_borrow_mut(Option::take);
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
        if let Some(state) = state {
            for font in [
                state.fonts.text,
                state.fonts.bold,
                state.fonts.title,
                state.fonts.glyph,
                state.fonts.small,
            ] {
                let _ = unsafe { DeleteObject(font.into()) };
            }
        }
    }
}

fn push(event: FileWindowEvent) {
    STATE.with_borrow_mut(|state| {
        if let Some(state) = state {
            state.events.push(event);
        }
    });
}

fn add_column(list: HWND, index: usize, title: &str, width: i32, right: bool) {
    let mut text = wide(title);
    let column = LvColumn {
        mask: LVCF_FMT | LVCF_WIDTH | LVCF_TEXT,
        format: if right { LVCFMT_RIGHT } else { 0 },
        width,
        text: text.as_mut_ptr(),
        text_max: 0,
        sub_item: 0,
        image: 0,
        order: 0,
        min_width: 0,
        default_width: 0,
        ideal_width: 0,
    };
    unsafe {
        SendMessageW(
            list,
            LVM_INSERTCOLUMNW,
            Some(WPARAM(index)),
            Some(LPARAM(std::ptr::from_ref(&column) as isize)),
        );
    }
}

/// The system's small icon for an entry (looked up once per extension).
fn icon_for(icons: &mut HashMap<String, i32>, kind: FileKind, name: &str) -> i32 {
    let (key, probe, attributes) = match kind {
        FileKind::Folder | FileKind::Place => (
            "<folder>".to_string(),
            "folder".to_string(),
            FILE_ATTRIBUTE_DIRECTORY,
        ),
        FileKind::Drive => (
            "<drive>".to_string(),
            "C:\\".to_string(),
            FILE_ATTRIBUTE_DIRECTORY,
        ),
        FileKind::File => {
            let extension = Path::new(name)
                .extension()
                .map(|e| e.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            (
                format!(".{extension}"),
                format!("file.{extension}"),
                FILE_ATTRIBUTE_NORMAL,
            )
        }
    };
    if let Some(index) = icons.get(&key) {
        return *index;
    }
    let probe = wide(&probe);
    let mut info = SHFILEINFOW::default();
    // Drives by their own icon (this PC's C: stands for any); the rest by type alone, never touching a file.
    let flags = if kind == FileKind::Drive {
        SHGFI_SYSICONINDEX | SHGFI_SMALLICON
    } else {
        SHGFI_SYSICONINDEX | SHGFI_SMALLICON | SHGFI_USEFILEATTRIBUTES
    };
    unsafe {
        SHGetFileInfoW(
            PCWSTR(probe.as_ptr()),
            attributes,
            Some(&mut info),
            size_of::<SHFILEINFOW>() as u32,
            flags,
        )
    };
    icons.insert(key, info.iIcon);
    info.iIcon
}

fn insert_row(list: HWND, index: usize, texts: &[String], image: i32, param: isize) {
    let mut first = wide(texts.first().map_or("", String::as_str));
    let item = LvItem {
        mask: LVIF_TEXT | LVIF_IMAGE | LVIF_PARAM,
        item: index as i32,
        text: first.as_mut_ptr(),
        image,
        param,
        ..Default::default()
    };
    let at = unsafe {
        SendMessageW(
            list,
            LVM_INSERTITEMW,
            Some(WPARAM(0)),
            Some(LPARAM(std::ptr::from_ref(&item) as isize)),
        )
    }
    .0;
    for (column, text) in texts.iter().enumerate().skip(1) {
        set_cell(list, at as usize, column, text);
    }
}

fn set_cell(list: HWND, row: usize, column: usize, text: &str) {
    let mut text = wide(text);
    let item = LvItem {
        sub_item: column as i32,
        text: text.as_mut_ptr(),
        ..Default::default()
    };
    unsafe {
        SendMessageW(
            list,
            LVM_SETITEMTEXTW,
            Some(WPARAM(row)),
            Some(LPARAM(std::ptr::from_ref(&item) as isize)),
        );
    }
}

fn set_checked(list: HWND, row: usize, on: bool) {
    let item = LvItem {
        state_mask: LVIS_STATEIMAGEMASK,
        state: if on { 2 << 12 } else { 1 << 12 },
        ..Default::default()
    };
    unsafe {
        SendMessageW(
            list,
            0x102B, // LVM_SETITEMSTATE
            Some(WPARAM(row)),
            Some(LPARAM(std::ptr::from_ref(&item) as isize)),
        );
    }
}

fn row_count(list: HWND) -> usize {
    unsafe { SendMessageW(list, LVM_GETITEMCOUNT, None, None) }
        .0
        .max(0) as usize
}

/// The names of the checked entries of pane `side`, else of the selected ones.
fn checked_names(side: usize) -> Vec<String> {
    STATE.with_borrow(|state| {
        let Some(state) = state.as_ref() else {
            return Vec::new();
        };
        let list = state.lists[side];
        let items = &state.panes[side].items;
        let state_of = |row: usize, mask: u32| {
            unsafe {
                SendMessageW(
                    list,
                    LVM_GETITEMSTATE,
                    Some(WPARAM(row)),
                    Some(LPARAM(mask as isize)),
                )
            }
            .0 as u32
        };
        let checked: Vec<String> = (0..row_count(list))
            .filter(|row| state_of(*row, LVIS_STATEIMAGEMASK) >> 12 == 2)
            .filter_map(|row| items.get(row).map(|item| item.name.clone()))
            .collect();
        if !checked.is_empty() {
            return checked;
        }
        (0..row_count(list))
            .filter(|row| state_of(*row, LVIS_SELECTED) & LVIS_SELECTED != 0)
            .filter_map(|row| items.get(row).map(|item| item.name.clone()))
            .collect()
    })
}

/// Pane `side`'s list filled anew from its items.
fn fill_pane(side: usize) {
    // The rows first (the icons need the state); the list is filled after the state is let go, since the list's
    // notifications come back to the window procedure while it fills.
    let Some((list, rows)) = STATE.with_borrow_mut(|state| {
        let state = state.as_mut()?;
        let items = state.panes[side].items.clone();
        let rows: Vec<([String; 4], i32)> = items
            .iter()
            .map(|item| {
                let image = icon_for(&mut state.icons, item.kind, &item.name);
                let texts = [
                    item.label.clone(),
                    time_text(item.modified),
                    type_text(item),
                    if item.kind == FileKind::File {
                        size_text(item.size)
                    } else {
                        "--".to_string()
                    },
                ];
                (texts, image)
            })
            .collect();
        Some((state.lists[side], rows))
    }) else {
        return;
    };
    FILLING.set(true);
    unsafe {
        SendMessageW(list, WM_SETREDRAW, Some(WPARAM(0)), None);
        SendMessageW(list, LVM_DELETEALLITEMS, None, None);
    }
    for (index, (texts, image)) in rows.iter().enumerate() {
        insert_row(list, index, texts, *image, index as isize);
    }
    unsafe {
        SendMessageW(list, WM_SETREDRAW, Some(WPARAM(1)), None);
        let _ = InvalidateRect(Some(list), None, true);
    }
    FILLING.set(false);
}

/// Pane `side`'s folder box: the folder shown, and the places in its list.
fn fill_folder_box(side: usize) {
    STATE.with_borrow(|state| {
        let Some(state) = state.as_ref() else {
            return;
        };
        let combo = state.folders[side];
        let pane = &state.panes[side];
        unsafe {
            SendMessageW(combo, CB_RESETCONTENT, None, None);
            for (label, path) in &pane.places {
                let text = wide(&format!("{label}  —  {path}"));
                SendMessageW(
                    combo,
                    CB_ADDSTRING,
                    None,
                    Some(LPARAM(text.as_ptr() as isize)),
                );
            }
            let shown = if pane.folder.is_empty() {
                if side == 0 {
                    tr("This PC's places")
                } else {
                    tr("That PC's places")
                }
                .to_string()
            } else {
                pane.folder.clone()
            };
            let text = wide(&shown);
            let _ = SetWindowTextW(combo, PCWSTR(text.as_ptr()));
        }
    });
}

/// The transfer list filled anew (rows added or gone), or its texts set again.
fn fill_jobs() {
    // As fill_pane: the rows with the state, the list after it is let go.
    let Some((list, rows)) = STATE.with_borrow_mut(|state| {
        let state = state.as_mut()?;
        let jobs = state.jobs.clone();
        let rows: Vec<(u32, [String; 6], i32)> = jobs
            .iter()
            .map(|job| {
                let kind = if job.folder {
                    FileKind::Folder
                } else {
                    FileKind::File
                };
                let image = icon_for(&mut state.icons, kind, &job.name);
                let texts = [
                    job.name.clone(),
                    status_text(job),
                    job_size_text(job),
                    job.from.clone(),
                    job.to.clone(),
                    String::new(),
                ];
                (job.id, texts, image)
            })
            .collect();
        Some((state.jobs_list, rows))
    }) else {
        return;
    };
    let same_rows = row_count(list) == rows.len()
        && rows
            .iter()
            .enumerate()
            .all(|(row, (id, _, _))| row_param(list, row) == *id as isize);
    FILLING.set(true);
    if !same_rows {
        unsafe {
            SendMessageW(list, WM_SETREDRAW, Some(WPARAM(0)), None);
            SendMessageW(list, LVM_DELETEALLITEMS, None, None);
        }
    }
    for (row, (id, texts, image)) in rows.iter().enumerate() {
        if same_rows {
            set_cell(list, row, 0, &texts[0]);
            set_cell(list, row, 2, &texts[2]);
        } else {
            insert_row(list, row, texts, *image, *id as isize);
        }
    }
    unsafe {
        if !same_rows {
            SendMessageW(list, WM_SETREDRAW, Some(WPARAM(1)), None);
        }
        let _ = InvalidateRect(Some(list), None, false);
    }
    FILLING.set(false);
}

fn row_param(list: HWND, row: usize) -> isize {
    let mut item = LvItem {
        mask: LVIF_PARAM,
        item: row as i32,
        ..Default::default()
    };
    unsafe {
        SendMessageW(
            list,
            LVM_GETITEMW,
            Some(WPARAM(0)),
            Some(LPARAM(std::ptr::from_mut(&mut item) as isize)),
        );
    }
    item.param
}

fn status_text(job: &JobRow) -> String {
    let percent = job
        .bytes_done
        .saturating_mul(100)
        .checked_div(job.bytes)
        .unwrap_or(0);
    match &job.state {
        JobRowState::Waiting => tr("Waiting").to_string(),
        JobRowState::Going => format!("{percent} %"),
        JobRowState::Paused => format!("{} {percent} %", tr("Paused")),
        JobRowState::Done if job.sending => tr("Sent").to_string(),
        JobRowState::Done => tr("Received").to_string(),
        JobRowState::Failed(reason) => format!("{}: {reason}", tr("Failed")),
        JobRowState::Cancelled => tr("Cancelled").to_string(),
    }
}

fn job_size_text(job: &JobRow) -> String {
    match job.state {
        JobRowState::Going | JobRowState::Paused if job.bytes > 0 => {
            format!("{} / {}", size_text(job.bytes_done), size_text(job.bytes))
        }
        _ if job.bytes > 0 || matches!(job.state, JobRowState::Done) => size_text(job.bytes),
        _ => "--".to_string(),
    }
}

/// The buttons a row shows in its last column, left to right.
fn row_commands(job: &JobRow) -> Vec<(JobCommand, &'static str)> {
    let mut commands = Vec::new();
    if job.local_path.is_some() {
        commands.push((JobCommand::ShowInFolder, GLYPH_FOLDER));
    }
    match job.state {
        JobRowState::Going | JobRowState::Waiting if job.can_pause => {
            commands.push((JobCommand::Pause, GLYPH_PAUSE))
        }
        JobRowState::Paused => commands.push((JobCommand::Resume, GLYPH_PLAY)),
        _ => {}
    }
    commands.push((JobCommand::Remove, GLYPH_CLEAR));
    commands
}

/// Where every control goes for the window's size.
fn layout(hwnd: HWND) {
    let mut client = RECT::default();
    let _ = unsafe { GetClientRect(hwnd, &mut client) };
    STATE.with_borrow(|state| {
        let Some(state) = state.as_ref() else {
            return;
        };
        let s = state.scale;
        let (w, h) = (client.right, client.bottom);
        let place = |control: HWND, area: RECT| unsafe {
            let _ = MoveWindow(
                control,
                area.left,
                area.top,
                area.right - area.left,
                area.bottom - area.top,
                true,
            );
        };
        let areas = Areas::new(w, h, s);
        for side in 0..2 {
            let card = areas.panes[side];
            let row = card.top + px(8, s);
            let size = px(TOOLBAR, s);
            let mut x = card.left + px(8, s);
            for id in [ID_BACK[side], ID_UP[side], ID_REFRESH[side]] {
                if let Some(button) = state.buttons.get(&id) {
                    place(
                        button.hwnd,
                        RECT {
                            left: x,
                            top: row,
                            right: x + size,
                            bottom: row + size,
                        },
                    );
                }
                x += size + px(2, s);
            }
            // The folder box: its own height for one line, its list below.
            place(
                state.folders[side],
                RECT {
                    left: x + px(6, s),
                    top: row + px(2, s),
                    right: card.right - px(10, s),
                    bottom: row + px(300, s),
                },
            );
            place(
                state.lists[side],
                RECT {
                    left: card.left + px(1, s),
                    top: row + size + px(8, s),
                    right: card.right - px(1, s),
                    bottom: card.bottom - px(6, s),
                },
            );
            if let Some(button) = state.buttons.get(&ID_SEND[side]) {
                place(button.hwnd, areas.send[side]);
            }
        }
        let card = areas.jobs;
        let row = card.top + px(8, s);
        let mut right = card.right - px(10, s);
        for id in [ID_CLEAR, ID_CANCEL_ALL, ID_RESUME_ALL, ID_PAUSE_ALL] {
            if let Some(button) = state.buttons.get(&id) {
                let dc = unsafe { windows::Win32::Graphics::Gdi::GetDC(Some(hwnd)) };
                let width = text_width(dc, &button.label, state.fonts.text) + px(36, s);
                unsafe { windows::Win32::Graphics::Gdi::ReleaseDC(Some(hwnd), dc) };
                place(
                    button.hwnd,
                    RECT {
                        left: right - width,
                        top: row,
                        right,
                        bottom: row + px(28, s),
                    },
                );
                right -= width + px(4, s);
            }
        }
        place(
            state.jobs_list,
            RECT {
                left: card.left + px(1, s),
                top: row + px(36, s),
                right: card.right - px(1, s),
                bottom: card.bottom - px(6, s),
            },
        );
        // The name column takes what the others leave.
        let fixed: i32 = [130, 120, 190, 190, ACTIONS_WIDTH]
            .iter()
            .map(|v| px(*v, s))
            .sum();
        let name = (card.right - card.left - fixed - px(24, s)).max(px(120, s));
        unsafe {
            SendMessageW(
                state.jobs_list,
                LVM_SETCOLUMNWIDTH,
                Some(WPARAM(0)),
                Some(LPARAM(name as isize)),
            )
        };
        for side in 0..2 {
            let width = areas.panes[side].right - areas.panes[side].left;
            let others = px(124 + 64 + 76, s) + px(24, s);
            unsafe {
                SendMessageW(
                    state.lists[side],
                    LVM_SETCOLUMNWIDTH,
                    Some(WPARAM(0)),
                    Some(LPARAM((width - others).max(px(120, s)) as isize)),
                )
            };
        }
    });
}

/// The window's areas for a client area `w` x `h`.
struct Areas {
    /// The two header halves (names and Send buttons).
    headers: [RECT; 2],
    send: [RECT; 2],
    panes: [RECT; 2],
    jobs: RECT,
}

impl Areas {
    fn new(w: i32, h: i32, s: f64) -> Areas {
        let margin = px(MARGIN, s);
        let half = (w - 3 * margin) / 2;
        let header_top = px(10, s);
        let header_bottom = header_top + px(HEADER, s);
        let jobs_height = ((h - header_bottom) * 38 / 100).max(px(190, s));
        let jobs_top = h - margin - jobs_height;
        let lefts = [margin, 2 * margin + half];
        let headers = lefts.map(|left| RECT {
            left,
            top: header_top,
            right: left + half,
            bottom: header_bottom,
        });
        let send_top = header_top + (px(HEADER, s) - px(32, s)) / 2;
        let send = [
            RECT {
                left: lefts[0] + half - px(SEND_WIDTH, s),
                top: send_top,
                right: lefts[0] + half,
                bottom: send_top + px(32, s),
            },
            RECT {
                left: lefts[1],
                top: send_top,
                right: lefts[1] + px(SEND_WIDTH, s),
                bottom: send_top + px(32, s),
            },
        ];
        let panes = lefts.map(|left| RECT {
            left,
            top: header_bottom + px(6, s),
            right: left + half,
            bottom: jobs_top - margin,
        });
        Areas {
            headers,
            send,
            panes,
            jobs: RECT {
                left: margin,
                top: jobs_top,
                right: w - margin,
                bottom: h - margin,
            },
        }
    }
}

fn paint(hwnd: HWND) {
    let mut paint = PAINTSTRUCT::default();
    let dc = unsafe { BeginPaint(hwnd, &mut paint) };
    let mut client = RECT::default();
    let _ = unsafe { GetClientRect(hwnd, &mut client) };
    STATE.with_borrow(|state| {
        let Some(state) = state.as_ref() else {
            return;
        };
        let s = state.scale;
        fill(dc, &client, PAGE);
        let areas = Areas::new(client.right, client.bottom, s);
        for card in areas.panes.iter().chain(Some(&areas.jobs)) {
            round_fill(dc, card, px(12, s), CARD, Some(CARD_LINE));
        }
        // The names above the panes, each with its chip: this PC left, the other right.
        for side in 0..2 {
            let header = areas.headers[side];
            let (chip_text, chip) = if side == 0 {
                (tr("This PC"), CHIP_HERE)
            } else {
                (tr("That PC"), CHIP_THERE)
            };
            let name = &state.names[side];
            let chip_width = text_width(dc, chip_text, state.fonts.small) + px(16, s);
            let room = header.right - header.left - px(SEND_WIDTH, s) - chip_width - px(30, s);
            let name_width = text_width(dc, name, state.fonts.title).min(room.max(px(60, s)));
            let (name_left, chip_left) = if side == 0 {
                (
                    header.left + px(4, s),
                    header.left + px(4, s) + name_width + px(10, s),
                )
            } else {
                let name_left = header.right - px(4, s) - name_width;
                (name_left, name_left - px(10, s) - chip_width)
            };
            draw_text(
                dc,
                name,
                RECT {
                    left: name_left,
                    top: header.top,
                    right: name_left + name_width,
                    bottom: header.bottom,
                },
                state.fonts.title,
                TEXT_COLOR,
                DT_LEFT.0,
            );
            let chip_area = RECT {
                left: chip_left,
                top: (header.top + header.bottom) / 2 - px(10, s),
                right: chip_left + chip_width,
                bottom: (header.top + header.bottom) / 2 + px(10, s),
            };
            round_fill(dc, &chip_area, px(8, s), chip.0, None);
            draw_text(
                dc,
                chip_text,
                chip_area,
                state.fonts.small,
                chip.1,
                DT_CENTER.0,
            );
            // A pane's note (reading, or why its folder could not be read), at its bottom.
            let status = &state.panes[side].status;
            if !status.is_empty() {
                let card = areas.panes[side];
                draw_text(
                    dc,
                    status,
                    RECT {
                        left: card.left + px(14, s),
                        top: card.bottom - px(34, s),
                        right: card.right - px(14, s),
                        bottom: card.bottom - px(8, s),
                    },
                    state.fonts.small,
                    SECONDARY,
                    DT_LEFT.0,
                );
            }
        }
        // The transfer list's heading.
        let card = areas.jobs;
        let row = RECT {
            left: card.left + px(14, s),
            top: card.top + px(8, s),
            right: card.right,
            bottom: card.top + px(36, s),
        };
        draw_text(
            dc,
            tr("Transfers"),
            row,
            state.fonts.bold,
            TEXT_COLOR,
            DT_LEFT.0,
        );
        let title_width = text_width(dc, tr("Transfers"), state.fonts.bold);
        let done = state
            .jobs
            .iter()
            .filter(|job| job.state == JobRowState::Done)
            .count();
        let note = if state.jobs.is_empty() {
            tr("Nothing sent yet: check files and press Send, or drop files here.").to_string()
        } else {
            trf("{count} done", &[("count", &done.to_string())])
        };
        draw_text(
            dc,
            &note,
            RECT {
                left: row.left + title_width + px(10, s),
                ..row
            },
            state.fonts.small,
            SECONDARY,
            DT_LEFT.0,
        );
    });
    unsafe {
        let _ = EndPaint(hwnd, &paint);
    }
}

/// An owner-drawn button.
fn draw_button(item: &DrawItem) {
    STATE.with_borrow(|state| {
        let Some(state) = state.as_ref() else {
            return;
        };
        let Some(button) = state.buttons.get(&(item.control_id as u16)) else {
            return;
        };
        let s = state.scale;
        let dc = item.hdc;
        let area = item.rect;
        let pressed = item.state & ODS_SELECTED != 0;
        let disabled = item.state & ODS_DISABLED != 0;
        // The card's white behind it.
        fill(dc, &area, CARD);
        match button.look {
            Look::Primary => {
                let color = if disabled {
                    PRESSED
                } else if pressed {
                    ACCENT
                } else {
                    NIGHT
                };
                round_fill(dc, &area, px(8, s), color, None);
                draw_text(
                    dc,
                    &button.label,
                    area,
                    state.fonts.bold,
                    if disabled { DISABLED } else { WHITE },
                    DT_CENTER.0,
                );
            }
            Look::Glyph => {
                if pressed {
                    round_fill(dc, &area, px(6, s), PRESSED, None);
                }
                draw_text(
                    dc,
                    button.glyph,
                    area,
                    state.fonts.glyph,
                    if disabled { DISABLED } else { TEXT_COLOR },
                    DT_CENTER.0,
                );
            }
            Look::Text => {
                if pressed {
                    round_fill(dc, &area, px(6, s), PRESSED, None);
                }
                let color = if disabled { DISABLED } else { SECONDARY };
                draw_text(
                    dc,
                    button.glyph,
                    RECT {
                        left: area.left + px(8, s),
                        right: area.left + px(26, s),
                        ..area
                    },
                    state.fonts.glyph,
                    color,
                    DT_CENTER.0,
                );
                draw_text(
                    dc,
                    &button.label,
                    RECT {
                        left: area.left + px(28, s),
                        ..area
                    },
                    state.fonts.text,
                    color,
                    DT_LEFT.0,
                );
            }
        }
    });
}

/// Enables what can be used now: back and up by the pane's state, Send by what is checked or selected.
fn refresh_buttons() {
    let checked = [!checked_names(0).is_empty(), !checked_names(1).is_empty()];
    STATE.with_borrow(|state| {
        let Some(state) = state.as_ref() else {
            return;
        };
        let enable = |id: u16, on: bool| {
            if let Some(button) = state.buttons.get(&id) {
                let _ = unsafe { EnableWindow(button.hwnd, on) };
            }
        };
        for side in 0..2 {
            let pane = &state.panes[side];
            enable(ID_BACK[side], pane.can_go_back);
            enable(ID_UP[side], pane.can_go_up);
            // Into the other side's folder: never into the places list.
            enable(
                ID_SEND[side],
                checked[side] && !state.panes[1 - side].folder.is_empty(),
            );
        }
        let any = !state.jobs.is_empty();
        let running = state
            .jobs
            .iter()
            .any(|job| matches!(job.state, JobRowState::Going | JobRowState::Waiting));
        let paused = state
            .jobs
            .iter()
            .any(|job| job.state == JobRowState::Paused);
        enable(ID_PAUSE_ALL, running);
        enable(ID_RESUME_ALL, paused);
        enable(ID_CANCEL_ALL, running || paused);
        enable(ID_CLEAR, any);
    });
}

/// A cell of the transfer list drawn by hand: the state (with a bar while it goes), the buttons.
fn draw_job_cell(draw: &NmLvCustomDraw) -> bool {
    STATE.with_borrow(|state| {
        let Some(state) = state.as_ref() else {
            return false;
        };
        let row = draw.draw.item_spec;
        let Some(job) = state.jobs.get(row) else {
            return false;
        };
        let s = state.scale;
        let dc = draw.draw.hdc;
        let area = draw.draw.rect;
        match draw.sub_item {
            1 => {
                fill(dc, &area, CARD);
                let inner = RECT {
                    left: area.left + px(6, s),
                    right: area.right - px(6, s),
                    ..area
                };
                match &job.state {
                    JobRowState::Going | JobRowState::Paused => {
                        let percent = job
                            .bytes_done
                            .saturating_mul(100)
                            .checked_div(job.bytes)
                            .unwrap_or(0)
                            .min(100) as i32;
                        let bar_top = (area.top + area.bottom) / 2 - px(3, s);
                        let bar = RECT {
                            left: inner.left,
                            top: bar_top,
                            right: inner.right - px(44, s),
                            bottom: bar_top + px(6, s),
                        };
                        round_fill(dc, &bar, px(6, s), TRACK, None);
                        let filled = RECT {
                            right: bar.left + (bar.right - bar.left) * percent / 100,
                            ..bar
                        };
                        if filled.right > filled.left {
                            round_fill(
                                dc,
                                &filled,
                                px(6, s),
                                if job.state == JobRowState::Paused {
                                    DISABLED
                                } else {
                                    GOLD
                                },
                                None,
                            );
                        }
                        draw_text(
                            dc,
                            &format!("{percent} %"),
                            inner,
                            state.fonts.small,
                            SECONDARY,
                            DT_RIGHT.0,
                        );
                    }
                    JobRowState::Done => {
                        draw_text(
                            dc,
                            "\u{E73E}",
                            RECT {
                                right: inner.left + px(18, s),
                                ..inner
                            },
                            state.fonts.glyph,
                            ONLINE,
                            DT_LEFT.0,
                        );
                        draw_text(
                            dc,
                            &status_text(job),
                            RECT {
                                left: inner.left + px(20, s),
                                ..inner
                            },
                            state.fonts.text,
                            ONLINE,
                            DT_LEFT.0,
                        );
                    }
                    JobRowState::Failed(_) => {
                        draw_text(
                            dc,
                            &status_text(job),
                            inner,
                            state.fonts.text,
                            FAILED,
                            DT_LEFT.0,
                        );
                    }
                    _ => draw_text(
                        dc,
                        &status_text(job),
                        inner,
                        state.fonts.text,
                        SECONDARY,
                        DT_LEFT.0,
                    ),
                }
                true
            }
            3 | 4 => {
                fill(dc, &area, CARD);
                // A chip for the side, then the path ("" : that side's Queqiao downloads folder).
                let here = (draw.sub_item == 3) == job.sending;
                let (label, chip) = if here {
                    (tr("This PC"), CHIP_HERE)
                } else {
                    (tr("That PC"), CHIP_THERE)
                };
                let path = if draw.sub_item == 3 {
                    &job.from
                } else {
                    &job.to
                };
                let width = text_width(dc, label, state.fonts.small) + px(10, s);
                let chip_area = RECT {
                    left: area.left + px(6, s),
                    top: (area.top + area.bottom) / 2 - px(9, s),
                    right: area.left + px(6, s) + width,
                    bottom: (area.top + area.bottom) / 2 + px(9, s),
                };
                round_fill(dc, &chip_area, px(6, s), chip.0, None);
                draw_text(dc, label, chip_area, state.fonts.small, chip.1, DT_CENTER.0);
                let shown = if path.is_empty() {
                    tr("Downloads\\Queqiao").to_string()
                } else {
                    path.clone()
                };
                draw_text(
                    dc,
                    &shown,
                    RECT {
                        left: chip_area.right + px(6, s),
                        right: area.right - px(4, s),
                        ..area
                    },
                    state.fonts.text,
                    TEXT_COLOR,
                    DT_LEFT.0,
                );
                true
            }
            5 => {
                fill(dc, &area, CARD);
                for (index, (_, glyph)) in row_commands(job).iter().enumerate() {
                    let left = area.left + px(4, s) + index as i32 * px(22, s);
                    draw_text(
                        dc,
                        glyph,
                        RECT {
                            left,
                            right: left + px(22, s),
                            ..area
                        },
                        state.fonts.glyph,
                        SECONDARY,
                        DT_CENTER.0,
                    );
                }
                true
            }
            _ => false,
        }
    })
}

/// The transfer list's row and column at a point of the list (client coordinates).
fn job_hit(list: HWND, point: POINT) -> Option<(usize, i32, i32)> {
    let mut hit = LvHitTest {
        point,
        flags: 0,
        item: -1,
        sub_item: 0,
        group: 0,
    };
    unsafe {
        SendMessageW(
            list,
            LVM_SUBITEMHITTEST,
            Some(WPARAM(0)),
            Some(LPARAM(std::ptr::from_mut(&mut hit) as isize)),
        );
    }
    (hit.item >= 0).then_some((hit.item as usize, hit.sub_item, point.x))
}

/// The left edge of column `column` in the list (client coordinates).
fn column_left(list: HWND, column: i32) -> i32 {
    (0..column)
        .map(|index| unsafe { SendMessageW(list, 0x101D, Some(WPARAM(index as usize)), None) }.0 as i32)
        .sum()
}

fn job_command_at(row: usize, column: i32, x: i32) -> Option<(u32, JobCommand)> {
    if column != 5 {
        return None;
    }
    STATE.with_borrow(|state| {
        let state = state.as_ref()?;
        let job = state.jobs.get(row)?;
        let offset = x - column_left(state.jobs_list, 5) - px(4, state.scale);
        let index = usize::try_from(offset / px(22, state.scale)).ok()?;
        row_commands(job)
            .get(index)
            .map(|(command, _)| (job.id, *command))
    })
}

/// The right-click menu of a row of the transfer list.
fn job_menu(hwnd: HWND, row: usize) {
    let Some(job) = STATE.with_borrow(|state| {
        state
            .as_ref()
            .and_then(|state| state.jobs.get(row).cloned())
    }) else {
        return;
    };
    let mut entries: Vec<(JobCommand, &str)> = Vec::new();
    if job.local_path.is_some() {
        entries.push((JobCommand::ShowInFolder, tr("Show in folder")));
    }
    match job.state {
        JobRowState::Going | JobRowState::Waiting if job.can_pause => {
            entries.push((JobCommand::Pause, tr("Pause")))
        }
        JobRowState::Paused => entries.push((JobCommand::Resume, tr("Resume"))),
        _ => {}
    }
    if matches!(
        job.state,
        JobRowState::Going | JobRowState::Waiting | JobRowState::Paused
    ) {
        entries.push((JobCommand::Cancel, tr("Cancel")));
    }
    entries.push((JobCommand::Remove, tr("Remove from the list")));
    let Ok(menu) = (unsafe { CreatePopupMenu() }) else {
        return;
    };
    for (index, (_, label)) in entries.iter().enumerate() {
        let text = wide(label);
        let _ = unsafe { AppendMenuW(menu, MF_STRING, index + 1, PCWSTR(text.as_ptr())) };
    }
    let mut cursor = POINT::default();
    let _ = unsafe { GetCursorPos(&mut cursor) };
    let chosen = unsafe {
        TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_RIGHTBUTTON,
            cursor.x,
            cursor.y,
            None,
            hwnd,
            None,
        )
    };
    let _ = unsafe { DestroyMenu(menu) };
    if let Some((command, _)) = usize::try_from(chosen.0)
        .ok()
        .and_then(|at| entries.get(at.wrapping_sub(1)))
    {
        push(FileWindowEvent::Job {
            id: job.id,
            command: *command,
        });
    }
}

fn notify(hwnd: HWND, header: &NmHdr, lparam: LPARAM) -> Option<LRESULT> {
    let id = header.id_from as u16;
    let side = ID_LIST.iter().position(|list| *list == id);
    match (header.code, side) {
        (NM_DBLCLK, Some(side)) => {
            let activate = unsafe { &*(lparam.0 as *const NmItemActivate) };
            let name = STATE.with_borrow(|state| {
                let state = state.as_ref()?;
                let item = state.panes[side]
                    .items
                    .get(usize::try_from(activate.item).ok()?)?;
                item.kind.ne(&FileKind::File).then(|| item.name.clone())
            });
            if let Some(name) = name {
                push(FileWindowEvent::OpenItem {
                    remote: side == 1,
                    name,
                });
            }
            Some(LRESULT(0))
        }
        (LVN_ITEMCHANGED, Some(_)) => {
            if !FILLING.get() {
                refresh_buttons();
            }
            Some(LRESULT(0))
        }
        (LVN_KEYDOWN, Some(side)) => {
            let key = unsafe { &*(lparam.0 as *const NmLvKeyDown) }.key;
            let remote = side == 1;
            if key == VK_BACK.0 {
                push(FileWindowEvent::Back { remote });
            } else if key == VK_F5.0 {
                push(FileWindowEvent::Refresh { remote });
            } else if key == VK_RETURN.0 {
                let names = checked_names(side);
                let folder = STATE.with_borrow(|state| {
                    let state = state.as_ref()?;
                    let name = names.first()?;
                    state.panes[side]
                        .items
                        .iter()
                        .find(|item| &item.name == name && item.kind != FileKind::File)
                        .map(|item| item.name.clone())
                });
                if let Some(name) = folder {
                    push(FileWindowEvent::OpenItem { remote, name });
                }
            }
            Some(LRESULT(0))
        }
        (code, None) if id == ID_JOBS => match code {
            // The transfer list's rows are not selected: their buttons and menu work instead.
            LVN_ITEMCHANGING => {
                let change = unsafe { &*(lparam.0 as *const NmListView) };
                Some(LRESULT(isize::from(change.new_state & LVIS_SELECTED != 0)))
            }
            NM_CUSTOMDRAW => {
                let draw = unsafe { &*(lparam.0 as *const NmLvCustomDraw) };
                Some(LRESULT(match draw.draw.stage {
                    CDDS_PREPAINT => CDRF_NOTIFYITEMDRAW,
                    CDDS_ITEMPREPAINT => CDRF_NOTIFYSUBITEMDRAW,
                    stage if stage == CDDS_ITEMPREPAINT | CDDS_SUBITEM => {
                        if draw_job_cell(draw) {
                            CDRF_SKIPDEFAULT
                        } else {
                            CDRF_DODEFAULT
                        }
                    }
                    _ => CDRF_DODEFAULT,
                }))
            }
            NM_CLICK | NM_DBLCLK => {
                let activate = unsafe { &*(lparam.0 as *const NmItemActivate) };
                if let Some((row, column, x)) = job_hit(header.hwnd_from, activate.point) {
                    if let Some((job, command)) = job_command_at(row, column, x) {
                        push(FileWindowEvent::Job { id: job, command });
                    } else if code == NM_DBLCLK {
                        let job = STATE.with_borrow(|state| {
                            state.as_ref().and_then(|state| {
                                state
                                    .jobs
                                    .get(row)
                                    .filter(|job| job.local_path.is_some())
                                    .map(|job| job.id)
                            })
                        });
                        if let Some(id) = job {
                            push(FileWindowEvent::Job {
                                id,
                                command: JobCommand::ShowInFolder,
                            });
                        }
                    }
                }
                Some(LRESULT(0))
            }
            NM_RCLICK => {
                let activate = unsafe { &*(lparam.0 as *const NmItemActivate) };
                if let Some((row, _, _)) = job_hit(header.hwnd_from, activate.point) {
                    job_menu(hwnd, row);
                }
                Some(LRESULT(0))
            }
            _ => None,
        },
        _ => None,
    }
}

fn command(id: u16, code: u16) {
    for side in 0..2 {
        let remote = side == 1;
        let event = match id {
            i if i == ID_BACK[side] => Some(FileWindowEvent::Back { remote }),
            i if i == ID_UP[side] => Some(FileWindowEvent::Up { remote }),
            i if i == ID_REFRESH[side] => Some(FileWindowEvent::Refresh { remote }),
            i if i == ID_SEND[side] => {
                let names = checked_names(side);
                (!names.is_empty()).then_some(FileWindowEvent::Send {
                    from_remote: remote,
                    names,
                })
            }
            i if i == ID_FOLDER[side] && code == CBN_SELCHANGE => STATE.with_borrow(|state| {
                let state = state.as_ref()?;
                let index =
                    unsafe { SendMessageW(state.folders[side], CB_GETCURSEL, None, None) }.0;
                let (_, path) = state.panes[side].places.get(usize::try_from(index).ok()?)?;
                Some(FileWindowEvent::Open {
                    remote,
                    path: path.clone(),
                })
            }),
            i if i == ID_FOLDER_ENTER[side] => STATE.with_borrow(|state| {
                let state = state.as_ref()?;
                let mut buffer = vec![0u16; 1024];
                let length =
                    unsafe { GetWindowTextW(state.folders[side], &mut buffer) }.max(0) as usize;
                let typed = String::from_utf16_lossy(&buffer[..length])
                    .trim()
                    .to_string();
                (!typed.is_empty()).then_some(FileWindowEvent::Open {
                    remote,
                    path: typed,
                })
            }),
            _ => None,
        };
        if let Some(event) = event {
            push(event);
            return;
        }
    }
    let event = match id {
        ID_PAUSE_ALL => Some(FileWindowEvent::PauseAll),
        ID_RESUME_ALL => Some(FileWindowEvent::ResumeAll),
        ID_CANCEL_ALL => Some(FileWindowEvent::CancelAll),
        ID_CLEAR => Some(FileWindowEvent::ClearFinished),
        _ => None,
    };
    if let Some(event) = event {
        push(event);
    }
}

extern "system" fn folder_edit_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    id: usize,
    window: usize,
) -> LRESULT {
    if message == WM_KEYDOWN && wparam.0 as u16 == VK_RETURN.0 {
        let _ = unsafe {
            PostMessageW(
                Some(HWND(window as *mut c_void)),
                WM_COMMAND,
                WPARAM(id),
                LPARAM(0),
            )
        };
        return LRESULT(0);
    }
    if message == WM_CHAR && wparam.0 == 13 {
        return LRESULT(0);
    }
    unsafe { DefSubclassProc(hwnd, message, wparam, lparam) }
}

extern "system" fn files_proc(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match message {
        WM_SIZE => {
            layout(hwnd);
            unsafe {
                let _ = InvalidateRect(Some(hwnd), None, true);
            }
            LRESULT(0)
        }
        WM_GETMINMAXINFO => {
            let scale = f64::from(unsafe { GetDpiForWindow(hwnd) }.max(96)) / 96.0;
            let info = unsafe { &mut *(lparam.0 as *mut MINMAXINFO) };
            info.ptMinTrackSize = POINT {
                x: px(780, scale),
                y: px(540, scale),
            };
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            paint(hwnd);
            LRESULT(0)
        }
        WM_DRAWITEM => {
            draw_button(unsafe { &*(lparam.0 as *const DrawItem) });
            LRESULT(1)
        }
        WM_NOTIFY => {
            let header = unsafe { &*(lparam.0 as *const NmHdr) };
            match notify(hwnd, header, lparam) {
                Some(result) => result,
                None => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
            }
        }
        WM_COMMAND => {
            command(
                (wparam.0 & 0xFFFF) as u16,
                ((wparam.0 >> 16) & 0xFFFF) as u16,
            );
            LRESULT(0)
        }
        WM_CTLCOLORSTATIC | WM_CTLCOLOREDIT => unsafe {
            DefWindowProcW(hwnd, message, wparam, lparam)
        },
        WM_DROPFILES => {
            let drop = HDROP(wparam.0 as *mut _);
            let count = unsafe { DragQueryFileW(drop, u32::MAX, None) };
            let mut paths = Vec::new();
            for index in 0..count {
                let length = unsafe { DragQueryFileW(drop, index, None) } as usize;
                let mut buffer = vec![0u16; length + 1];
                let copied = unsafe { DragQueryFileW(drop, index, Some(&mut buffer)) } as usize;
                paths.push(PathBuf::from(String::from_utf16_lossy(
                    &buffer[..copied.min(length)],
                )));
            }
            unsafe { DragFinish(drop) };
            push(FileWindowEvent::Dropped(paths));
            LRESULT(0)
        }
        WM_CLOSE => {
            push(FileWindowEvent::Closed);
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn sizes_read_as_uu_shows_them() {
        assert_eq!(super::size_text(512), "512 B");
        assert_eq!(super::size_text(1331), "1.30 KB");
        assert_eq!(super::size_text(9_772_000), "9.32 MB");
    }
}
