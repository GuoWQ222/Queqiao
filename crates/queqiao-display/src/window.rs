//! A plain Win32 window. Messages are handled with `pump`, called from the viewer's own loop, so the
//! loop never blocks on the message queue. Mouse and keyboard input in the window is collected as
//! [`WindowEvent`]s for the viewer to send to the remote PC.

use std::cell::RefCell;
use std::sync::Once;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, Result, w};

static REGISTER: Once = Once::new();
const CLASS: PCWSTR = w!("QueqiaoViewer");

/// Input in the window. Positions are in pixels of the window's drawing area.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowEvent {
    MouseMove {
        x: i32,
        y: i32,
    },
    /// 0 left, 1 right, 2 middle, 3 back (X1), 4 forward (X2).
    MouseButton {
        button: u8,
        down: bool,
    },
    /// 120 = one notch.
    Wheel {
        delta: i16,
        horizontal: bool,
    },
    /// Hardware scancode (set 1) and the "extended" flag, as Windows reports them.
    Key {
        scancode: u16,
        extended: bool,
        down: bool,
    },
    /// The window lost the keyboard: keys still held must be released on the remote side.
    FocusLost,
}

thread_local! {
    static EVENTS: RefCell<Vec<WindowEvent>> = const { RefCell::new(Vec::new()) };
}

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

    /// The input collected since the last call (filled by `pump`).
    pub fn take_events(&mut self) -> Vec<WindowEvent> {
        EVENTS.with_borrow_mut(std::mem::take)
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
        EVENTS.with_borrow_mut(Vec::clear);
    }
}

fn push(event: WindowEvent) {
    EVENTS.with_borrow_mut(|events| events.push(event));
}

extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let low = (lparam.0 & 0xFFFF) as u16 as i16 as i32;
    let high = ((lparam.0 >> 16) & 0xFFFF) as u16 as i16 as i32;
    let button = |button: u8, down: bool| {
        // Keep receiving the mouse while a button is held, also outside the window (drags).
        unsafe {
            if down {
                SetCapture(hwnd);
            } else {
                let _ = ReleaseCapture();
            }
        }
        push(WindowEvent::MouseButton { button, down });
        LRESULT(0)
    };
    match message {
        WM_DESTROY => {
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            push(WindowEvent::MouseMove { x: low, y: high });
            LRESULT(0)
        }
        WM_LBUTTONDOWN => button(0, true),
        WM_LBUTTONUP => button(0, false),
        WM_RBUTTONDOWN => button(1, true),
        WM_RBUTTONUP => button(1, false),
        WM_MBUTTONDOWN => button(2, true),
        WM_MBUTTONUP => button(2, false),
        WM_XBUTTONDOWN | WM_XBUTTONUP => {
            let which = if (wparam.0 >> 16) & 0xFFFF == 2 { 4 } else { 3 };
            button(which, message == WM_XBUTTONDOWN);
            LRESULT(1)
        }
        WM_MOUSEWHEEL | WM_MOUSEHWHEEL => {
            let delta = ((wparam.0 >> 16) & 0xFFFF) as u16 as i16;
            push(WindowEvent::Wheel {
                delta,
                horizontal: message == WM_MOUSEHWHEEL,
            });
            LRESULT(0)
        }
        WM_KEYDOWN | WM_SYSKEYDOWN | WM_KEYUP | WM_SYSKEYUP => {
            push(WindowEvent::Key {
                scancode: ((lparam.0 >> 16) & 0xFF) as u16,
                extended: (lparam.0 >> 24) & 1 == 1,
                down: message == WM_KEYDOWN || message == WM_SYSKEYDOWN,
            });
            // Not passed on: Alt and F10 would otherwise open this window's system menu.
            LRESULT(0)
        }
        WM_KILLFOCUS => {
            push(WindowEvent::FocusLost);
            unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
        }
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}
