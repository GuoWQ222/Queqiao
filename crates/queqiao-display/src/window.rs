//! A plain Win32 window. Messages are handled with `pump`, called from the viewer's own loop, so the
//! loop never blocks on the message queue.

use std::sync::Once;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, Result, w};

static REGISTER: Once = Once::new();
const CLASS: PCWSTR = w!("QueqiaoViewer");

pub struct Window {
    hwnd: HWND,
    closed: bool,
}

impl Window {
    /// A window whose drawing area is `width` x `height`. It is shown without taking the keyboard focus
    /// from whatever the user is doing.
    pub fn new(title: &str, width: i32, height: i32) -> Result<Self> {
        let instance = unsafe { GetModuleHandleW(None) }?;
        REGISTER.call_once(|| unsafe {
            let class = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(window_proc),
                hInstance: instance.into(),
                hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                lpszClassName: CLASS,
                ..Default::default()
            };
            RegisterClassExW(&class);
        });
        let mut frame = RECT {
            left: 0,
            top: 0,
            right: width,
            bottom: height,
        };
        unsafe { AdjustWindowRect(&mut frame, WS_OVERLAPPEDWINDOW, false) }?;
        let title: Vec<u16> = title.encode_utf16().chain(Some(0)).collect();
        let hwnd = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                CLASS,
                PCWSTR(title.as_ptr()),
                WS_OVERLAPPEDWINDOW,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                frame.right - frame.left,
                frame.bottom - frame.top,
                None,
                None,
                Some(instance.into()),
                None,
            )
        }?;
        let _ = unsafe { ShowWindow(hwnd, SW_SHOWNOACTIVATE) };
        Ok(Self {
            hwnd,
            closed: false,
        })
    }

    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }

    /// Handles pending window messages without waiting. False once the window has been closed.
    pub fn pump(&mut self) -> bool {
        let mut message = MSG::default();
        while unsafe { PeekMessageW(&mut message, None, 0, 0, PM_REMOVE) }.as_bool() {
            if message.message == WM_QUIT {
                self.closed = true;
            }
            unsafe {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
        !self.closed
    }

    /// Size of the drawing area in pixels.
    pub fn client_size(&self) -> (u32, u32) {
        let mut area = RECT::default();
        let _ = unsafe { GetClientRect(self.hwnd, &mut area) };
        (
            (area.right - area.left).max(1) as u32,
            (area.bottom - area.top).max(1) as u32,
        )
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        // Fails harmlessly when the user already closed the window.
        let _ = unsafe { DestroyWindow(self.hwnd) };
        self.pump();
    }
}

extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_DESTROY {
        unsafe { PostQuitMessage(0) };
        return LRESULT(0);
    }
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}
