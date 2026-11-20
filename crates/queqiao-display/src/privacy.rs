//! The privacy screen on the controlled PC: a dark window over all its monitors that the person at the PC sees,
//! but screen capture does not (display affinity WDA_EXCLUDEFROMCAPTURE, Windows 10 version 2004 and later),
//! so the controlling person still sees the desktop. Clicks and keys go through it (layered and transparent
//! to input), so remote input reaches the programs underneath.

use std::sync::Once;

use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, CreateFontW, CreateSolidBrush,
    DEFAULT_CHARSET, DT_CENTER, DT_VCENTER, DT_WORDBREAK, DeleteObject, DrawTextW, EndPaint,
    FF_DONTCARE, FW_NORMAL, FillRect, HFONT, OUT_DEFAULT_PRECIS, PAINTSTRUCT, SelectObject,
    SetBkMode, SetTextColor, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetThreadDpiAwarenessContext,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, Result, w};

const CLASS: PCWSTR = w!("QueqiaoPrivacyScreen");
static REGISTER: Once = Once::new();

/// What the window shows: a fill colour (0x00BBGGRR) and a text in its middle.
struct Look {
    color: u32,
    text: Vec<u16>,
    font: HFONT,
}

pub struct PrivacyScreen {
    hwnd: HWND,
}

impl PrivacyScreen {
    /// Covers all monitors in near-black with `text`. With `area` (screen pixels), covers only that rectangle in
    /// `color` instead: tests use a small square of an unusual colour and look for it in captured pictures.
    /// Fails where Windows cannot hide a window from capture (before Windows 10 version 2004): a dark screen
    /// that the controlling person also saw would be no use.
    pub fn show(text: &str, area: Option<RECT>, color: u32) -> Result<PrivacyScreen> {
        let instance = unsafe { GetModuleHandleW(None) }?;
        REGISTER.call_once(|| unsafe {
            let class = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(privacy_proc),
                hInstance: instance.into(),
                hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                lpszClassName: CLASS,
                ..Default::default()
            };
            RegisterClassExW(&class);
        });
        // Physical pixels, whatever the program's own DPI setting: the window must cover the screens exactly.
        let previous =
            unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
        let rect = area.unwrap_or_else(|| unsafe {
            let (left, top) = (
                GetSystemMetrics(SM_XVIRTUALSCREEN),
                GetSystemMetrics(SM_YVIRTUALSCREEN),
            );
            RECT {
                left,
                top,
                right: left + GetSystemMetrics(SM_CXVIRTUALSCREEN),
                bottom: top + GetSystemMetrics(SM_CYVIRTUALSCREEN),
            }
        });
        let font = unsafe {
            CreateFontW(
                -32,
                0,
                0,
                0,
                FW_NORMAL.0 as i32,
                0,
                0,
                0,
                DEFAULT_CHARSET,
                OUT_DEFAULT_PRECIS,
                CLIP_DEFAULT_PRECIS,
                CLEARTYPE_QUALITY,
                FF_DONTCARE.0 as u32,
                w!("Segoe UI"),
            )
        };
        let look = Box::new(Look {
            color,
            text: text.encode_utf16().collect(),
            font,
        });
        let created = unsafe {
            CreateWindowExW(
                WS_EX_TOPMOST
                    | WS_EX_TOOLWINDOW
                    | WS_EX_NOACTIVATE
                    | WS_EX_LAYERED
                    | WS_EX_TRANSPARENT,
                CLASS,
                w!("Queqiao privacy screen"),
                WS_POPUP,
                rect.left,
                rect.top,
                rect.right - rect.left,
                rect.bottom - rect.top,
                None,
                None,
                Some(instance.into()),
                None,
            )
        };
        if !previous.is_invalid() {
            unsafe { SetThreadDpiAwarenessContext(previous) };
        }
        let hwnd = match created {
            Ok(hwnd) => hwnd,
            Err(error) => {
                let _ = unsafe { DeleteObject(font.into()) };
                return Err(error);
            }
        };
        unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(look) as isize) };
        let screen = PrivacyScreen { hwnd };
        unsafe { SetLayeredWindowAttributes(hwnd, COLORREF(0), 255, LWA_ALPHA) }?;
        screen.set_excluded(true)?;
        let _ = unsafe { ShowWindow(hwnd, SW_SHOWNOACTIVATE) };
        Ok(screen)
    }

    /// Hidden from screen capture (true, normal) or not (tests: to see that the check would notice).
    pub fn set_excluded(&self, excluded: bool) -> Result<()> {
        let affinity = if excluded {
            WDA_EXCLUDEFROMCAPTURE
        } else {
            WDA_NONE
        };
        unsafe { SetWindowDisplayAffinity(self.hwnd, affinity) }
    }

    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }
}

impl Drop for PrivacyScreen {
    fn drop(&mut self) {
        let look = unsafe { SetWindowLongPtrW(self.hwnd, GWLP_USERDATA, 0) } as *mut Look;
        let _ = unsafe { DestroyWindow(self.hwnd) };
        if !look.is_null() {
            let look = unsafe { Box::from_raw(look) };
            let _ = unsafe { DeleteObject(look.font.into()) };
        }
    }
}

extern "system" fn privacy_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_PAINT => {
            let mut paint = PAINTSTRUCT::default();
            let dc = unsafe { BeginPaint(hwnd, &mut paint) };
            let look = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const Look;
            if let Some(look) = unsafe { look.as_ref() } {
                let mut area = RECT::default();
                let _ = unsafe { GetClientRect(hwnd, &mut area) };
                let brush = unsafe { CreateSolidBrush(COLORREF(look.color)) };
                unsafe { FillRect(dc, &area, brush) };
                let _ = unsafe { DeleteObject(brush.into()) };
                if !look.text.is_empty() {
                    let mut text = look.text.clone();
                    unsafe {
                        let previous = SelectObject(dc, look.font.into());
                        SetBkMode(dc, TRANSPARENT);
                        SetTextColor(dc, COLORREF(0x00C8C8C8));
                        let mut middle = area;
                        middle.top = area.bottom / 2 - 60;
                        middle.bottom = area.bottom / 2 + 60;
                        DrawTextW(
                            dc,
                            &mut text,
                            &mut middle,
                            DT_CENTER | DT_VCENTER | DT_WORDBREAK,
                        );
                        SelectObject(dc, previous);
                    }
                }
            }
            let _ = unsafe { EndPaint(hwnd, &paint) };
            LRESULT(0)
        }
        // Never takes the focus or a click.
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize),
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}
