//! A plain Win32 window. Messages are handled with `pump`, called from the viewer's own loop, so the
//! loop never blocks on the message queue. Mouse and keyboard input in the window is collected as
//! [`WindowEvent`]s for the viewer to send to the remote PC.

use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::Once;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture};
use windows::Win32::UI::Shell::{DragAcceptFiles, DragFinish, DragQueryFileW, HDROP};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, Result, w};

static REGISTER: Once = Once::new();
/// A message handled for longer than this is reported (`take_slow_messages`).
const SLOW: Duration = Duration::from_millis(50);
const CLASS: PCWSTR = w!("QueqiaoViewer");

/// Input in the window. Positions are in pixels of the window's drawing area.
#[derive(Clone, Debug, PartialEq, Eq)]
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
    /// Files (or folders) dropped on the window from Explorer: to send to the other PC.
    FilesDropped(Vec<PathBuf>),
    /// An item added with [`Window::add_menu_items`] was chosen.
    Menu(u16),
}

thread_local! {
    static EVENTS: RefCell<Vec<WindowEvent>> = const { RefCell::new(Vec::new()) };
}

pub struct Window {
    hwnd: HWND,
    closed: bool,
    slow: Vec<String>,
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
        // Files dropped from Explorer come as WM_DROPFILES.
        unsafe { DragAcceptFiles(hwnd, true) };
        Ok(Self {
            hwnd,
            closed: false,
            slow: Vec::new(),
        })
    }

    /// Adds items to the window's system menu (the title-bar menu, also on Alt+Space); choosing one gives
    /// [`WindowEvent::Menu`]. Ids must be multiples of 16 below 0xF000: Windows uses the low 4 bits itself.
    pub fn add_menu_items(&self, items: &[(u16, &str)]) {
        let menu = unsafe { GetSystemMenu(self.hwnd, false) };
        if menu.is_invalid() {
            return;
        }
        let _ = unsafe { AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null()) };
        for (id, text) in items {
            let text: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
            let _ =
                unsafe { AppendMenuW(menu, MF_STRING, usize::from(*id), PCWSTR(text.as_ptr())) };
        }
    }

    pub fn set_title(&self, title: &str) {
        let title: Vec<u16> = title.encode_utf16().chain(Some(0)).collect();
        let _ = unsafe { SetWindowTextW(self.hwnd, PCWSTR(title.as_ptr())) };
    }

    /// Asks a yes/no question in a message box owned by this window; No is the default button.
    pub fn confirm(&self, title: &str, text: &str) -> bool {
        let title: Vec<u16> = title.encode_utf16().chain(Some(0)).collect();
        let text: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
        let answer = unsafe {
            MessageBoxW(
                Some(self.hwnd),
                PCWSTR(text.as_ptr()),
                PCWSTR(title.as_ptr()),
                MB_YESNO | MB_ICONWARNING | MB_DEFBUTTON2,
            )
        };
        answer == IDYES
    }

    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }

    /// Handles pending window messages without waiting. False once the window has been closed.
    pub fn pump(&mut self) -> bool {
        let mut message = MSG::default();
        loop {
            // PeekMessage also runs messages sent from other threads or programs: timed separately.
            let peek_start = Instant::now();
            let got = unsafe { PeekMessageW(&mut message, None, 0, 0, PM_REMOVE) }.as_bool();
            let peek = peek_start.elapsed();
            if peek > SLOW {
                self.slow
                    .push(format!("PeekMessage {} ms", peek.as_millis()));
            }
            if !got {
                break;
            }
            let dispatch_start = Instant::now();
            unsafe {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
            let took = dispatch_start.elapsed();
            if took > SLOW {
                let owner = if message.hwnd == self.hwnd {
                    "this window"
                } else {
                    "another window"
                };
                self.slow.push(format!(
                    "message 0x{:04X} to {owner} {} ms",
                    message.message,
                    took.as_millis()
                ));
            }
        }
        // Each window tracks its own end (closing it does not end the program, which may have other
        // windows, such as the device list).
        self.closed |= !unsafe { IsWindow(Some(self.hwnd)) }.as_bool();
        !self.closed
    }

    /// Messages that took longer than 50 ms to handle since the last call, described.
    pub fn take_slow_messages(&mut self) -> Vec<String> {
        std::mem::take(&mut self.slow)
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
        // Our own system-menu items (Windows' own commands are 0xF000 and above).
        WM_SYSCOMMAND if (wparam.0 & 0xFFF0) < 0xF000 => {
            push(WindowEvent::Menu((wparam.0 & 0xFFF0) as u16));
            LRESULT(0)
        }
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
            push(WindowEvent::FilesDropped(paths));
            LRESULT(0)
        }
        WM_KILLFOCUS => {
            push(WindowEvent::FocusLost);
            unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
        }
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}
