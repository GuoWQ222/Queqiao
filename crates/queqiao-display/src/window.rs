//! A plain Win32 window. Messages are handled with `pump`, called from the viewer's own loop, so the
//! loop never blocks on the message queue. Mouse and keyboard input in the window is collected as
//! [`WindowEvent`]s for the viewer to send to the remote PC.

use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::Once;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::POINT;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    ClientToScreen, GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture};
use windows::Win32::UI::Input::{
    GetRawInputData, HRAWINPUT, MOUSE_MOVE_ABSOLUTE, RAWINPUT, RAWINPUTDEVICE,
    RAWINPUTDEVICE_FLAGS, RAWINPUTHEADER, RID_INPUT, RIDEV_REMOVE, RIM_TYPEMOUSE,
    RegisterRawInputDevices,
};
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
    /// Game mouse: the mouse moved by this much (raw counts), while the pointer is held in the window.
    MouseDelta {
        dx: i32,
        dy: i32,
    },
    /// A hotkey registered with [`Window::register_hotkey`] was pressed (anywhere).
    Hotkey(i32),
}

thread_local! {
    static EVENTS: RefCell<Vec<WindowEvent>> = const { RefCell::new(Vec::new()) };
    /// That PC's pointer shape as this window's cursor (0: the arrow), and whether that pointer is hidden.
    static REMOTE_CURSOR: std::cell::Cell<isize> = const { std::cell::Cell::new(0) };
    static POINTER_HIDDEN: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

pub struct Window {
    hwnd: HWND,
    closed: bool,
    slow: Vec<String>,
    /// While full screen: the style and placement to go back to.
    restore: Option<(isize, WINDOWPLACEMENT)>,
    /// That PC's pointer shape, while this window's cursor takes it.
    remote_cursor: Option<crate::cursor::RemoteCursor>,
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
                // The session window shows Queqiao in the taskbar and Alt+Tab.
                hIcon: crate::icon::app_icon(32),
                hIconSm: crate::icon::app_icon(16),
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
            restore: None,
            remote_cursor: None,
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

    /// Adds a submenu `title` with `items` to the window's system menu (ids as for [`Window::add_menu_items`]).
    pub fn add_submenu(&self, title: &str, items: &[(u16, &str)]) {
        let menu = unsafe { GetSystemMenu(self.hwnd, false) };
        let Ok(submenu) = (unsafe { CreatePopupMenu() }) else {
            return;
        };
        if menu.is_invalid() {
            return;
        }
        for (id, text) in items {
            let text: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
            let _ =
                unsafe { AppendMenuW(submenu, MF_STRING, usize::from(*id), PCWSTR(text.as_ptr())) };
        }
        let title: Vec<u16> = title.encode_utf16().chain(Some(0)).collect();
        let _ = unsafe { AppendMenuW(menu, MF_POPUP, submenu.0 as usize, PCWSTR(title.as_ptr())) };
    }

    /// That PC's pointer shape as the cursor over the picture (None: the arrow again).
    pub fn set_remote_cursor(&mut self, cursor: Option<crate::cursor::RemoteCursor>) {
        REMOTE_CURSOR.set(
            cursor
                .as_ref()
                .map_or(0, |cursor| cursor.handle().0 as isize),
        );
        // The old one goes only after the window no longer names it.
        self.remote_cursor = cursor;
        self.refresh_cursor();
    }

    /// That PC's pointer is hidden (a game, a video): so is the cursor over the picture.
    pub fn set_pointer_hidden(&self, hidden: bool) {
        POINTER_HIDDEN.set(hidden);
        self.refresh_cursor();
    }

    /// Applies the cursor now if the pointer is over this window (otherwise the next WM_SETCURSOR does).
    fn refresh_cursor(&self) {
        let mut at = POINT::default();
        if unsafe { GetCursorPos(&mut at) }.is_ok() && unsafe { WindowFromPoint(at) } == self.hwnd {
            let _ = unsafe {
                PostMessageW(
                    Some(self.hwnd),
                    WM_SETCURSOR,
                    WPARAM(self.hwnd.0 as usize),
                    LPARAM(HTCLIENT as isize),
                )
            };
        }
    }

    /// New text for a menu item added with [`Window::add_menu_items`].
    pub fn rename_menu_item(&self, id: u16, text: &str) {
        let menu = unsafe { GetSystemMenu(self.hwnd, false) };
        let text: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
        let _ = unsafe {
            ModifyMenuW(
                menu,
                u32::from(id),
                MF_BYCOMMAND | MF_STRING,
                usize::from(id),
                PCWSTR(text.as_ptr()),
            )
        };
    }

    /// A system-wide hotkey for this window ([`WindowEvent::Hotkey`] when pressed, even with the window hidden);
    /// false if Windows refused it (another program has that key).
    pub fn register_hotkey(&self, id: i32, modifiers: u32, vk: u32) -> bool {
        unsafe {
            windows::Win32::UI::Input::KeyboardAndMouse::RegisterHotKey(
                Some(self.hwnd),
                id,
                windows::Win32::UI::Input::KeyboardAndMouse::HOT_KEY_MODIFIERS(modifiers),
                vk,
            )
        }
        .is_ok()
    }

    pub fn unregister_hotkey(&self, id: i32) {
        let _ = unsafe {
            windows::Win32::UI::Input::KeyboardAndMouse::UnregisterHotKey(Some(self.hwnd), id)
        };
    }

    /// Tests: as if the hotkey were pressed.
    pub fn press_hotkey(&self, id: i32) {
        let _ = unsafe { PostMessageW(Some(self.hwnd), WM_HOTKEY, WPARAM(id as usize), LPARAM(0)) };
    }

    /// Shown (in front) or hidden.
    pub fn set_visible(&self, visible: bool) {
        unsafe {
            let _ = ShowWindow(self.hwnd, if visible { SW_SHOW } else { SW_HIDE });
            if visible {
                let _ = SetForegroundWindow(self.hwnd);
            }
        }
    }

    pub fn is_visible(&self) -> bool {
        unsafe { IsWindowVisible(self.hwnd) }.as_bool()
    }

    /// Puts a check mark on a menu item added with [`Window::add_menu_items`], or takes it off.
    pub fn check_menu_item(&self, id: u16, checked: bool) {
        let menu = unsafe { GetSystemMenu(self.hwnd, false) };
        let flags = MF_BYCOMMAND | if checked { MF_CHECKED } else { MF_UNCHECKED };
        let _ = unsafe { CheckMenuItem(menu, u32::from(id), flags.0) };
    }

    /// Game mouse: the pointer is hidden and held inside the window, and the mouse's movement comes as
    /// [`WindowEvent::MouseDelta`] (raw input) instead of positions. Off gives the pointer back.
    pub fn set_game_mouse(&mut self, on: bool) {
        GAME_MOUSE.set(on);
        let device = RAWINPUTDEVICE {
            usUsagePage: 1,
            usUsage: 2,
            dwFlags: if on {
                RAWINPUTDEVICE_FLAGS(0)
            } else {
                RIDEV_REMOVE
            },
            hwndTarget: if on { self.hwnd } else { HWND::default() },
        };
        let _ = unsafe { RegisterRawInputDevices(&[device], size_of::<RAWINPUTDEVICE>() as u32) };
        if on {
            clip_to_client(self.hwnd);
        } else {
            let _ = unsafe { ClipCursor(None) };
        }
    }

    /// Windows shortcuts (Win, Alt+Tab, Alt+Esc, Ctrl+Esc) go to the other PC while this window is in front.
    pub fn set_send_shortcuts(&mut self, on: bool) {
        crate::shortcuts::set(self.hwnd, on);
    }

    pub fn is_sending_shortcuts(&self) -> bool {
        crate::shortcuts::is_on()
    }

    pub fn is_game_mouse(&self) -> bool {
        GAME_MOUSE.get()
    }

    /// Above all other windows (true) or not.
    pub fn set_always_on_top(&self, on: bool) {
        let after = if on { HWND_TOPMOST } else { HWND_NOTOPMOST };
        let _ = unsafe {
            SetWindowPos(
                self.hwnd,
                Some(after),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            )
        };
    }

    pub fn is_full_screen(&self) -> bool {
        self.restore.is_some()
    }

    /// The window's opacity in percent (20 to 100; 100: opaque, no longer layered).
    pub fn set_opacity(&self, percent: u8) {
        let percent = percent.clamp(20, 100);
        let layered = WS_EX_LAYERED.0 as isize;
        unsafe {
            let style = GetWindowLongPtrW(self.hwnd, GWL_EXSTYLE);
            if percent >= 100 {
                SetWindowLongPtrW(self.hwnd, GWL_EXSTYLE, style & !layered);
            } else {
                SetWindowLongPtrW(self.hwnd, GWL_EXSTYLE, style | layered);
                let alpha = (u32::from(percent) * 255 / 100) as u8;
                let _ = SetLayeredWindowAttributes(
                    self.hwnd,
                    windows::Win32::Foundation::COLORREF(0),
                    alpha,
                    LWA_ALPHA,
                );
            }
        }
    }

    /// The opacity in percent as Windows has it (100 when the window is not layered).
    pub fn opacity(&self) -> u8 {
        let mut alpha = 255u8;
        let mut flags = LAYERED_WINDOW_ATTRIBUTES_FLAGS::default();
        let layered =
            unsafe { GetWindowLongPtrW(self.hwnd, GWL_EXSTYLE) } & WS_EX_LAYERED.0 as isize != 0;
        if layered
            && unsafe {
                GetLayeredWindowAttributes(self.hwnd, None, Some(&mut alpha), Some(&mut flags))
            }
            .is_ok()
            && flags.0 & LWA_ALPHA.0 != 0
        {
            (u32::from(alpha) * 100).div_ceil(255) as u8
        } else {
            100
        }
    }

    /// The picture at its own size: the client area `width` x `height`, at most the monitor's work area (not in full
    /// screen).
    pub fn set_client_size(&self, width: u32, height: u32) {
        if self.restore.is_some() || width == 0 || height == 0 {
            return;
        }
        unsafe {
            let style = WINDOW_STYLE(GetWindowLongPtrW(self.hwnd, GWL_STYLE) as u32);
            let ex = WINDOW_EX_STYLE(GetWindowLongPtrW(self.hwnd, GWL_EXSTYLE) as u32);
            let mut outer = RECT {
                left: 0,
                top: 0,
                right: width as i32,
                bottom: height as i32,
            };
            let has_menu = !GetMenu(self.hwnd).0.is_null();
            if AdjustWindowRectEx(&mut outer, style, has_menu, ex).is_err() {
                return;
            }
            let (mut w, mut h) = (outer.right - outer.left, outer.bottom - outer.top);
            let monitor = MonitorFromWindow(self.hwnd, MONITOR_DEFAULTTONEAREST);
            let mut info = MONITORINFO {
                cbSize: size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            let mut at = (0, 0);
            if GetMonitorInfoW(monitor, &mut info).as_bool() {
                let work = info.rcWork;
                w = w.min(work.right - work.left);
                h = h.min(work.bottom - work.top);
                let mut current = RECT::default();
                let _ = GetWindowRect(self.hwnd, &mut current);
                // Kept on the monitor.
                at = (
                    current.left.clamp(work.left, work.right - w),
                    current.top.clamp(work.top, work.bottom - h),
                );
            }
            let _ = SetWindowPos(
                self.hwnd,
                None,
                at.0,
                at.1,
                w,
                h,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
    }

    /// Full screen on the window's monitor (no frame), or back to the window it was.
    pub fn set_full_screen(&mut self, on: bool) {
        if on == self.restore.is_some() {
            return;
        }
        if on {
            let style = unsafe { GetWindowLongPtrW(self.hwnd, GWL_STYLE) };
            let mut placement = WINDOWPLACEMENT {
                length: size_of::<WINDOWPLACEMENT>() as u32,
                ..Default::default()
            };
            if unsafe { GetWindowPlacement(self.hwnd, &mut placement) }.is_err() {
                return;
            }
            let Some(area) = self.monitor_rect() else {
                return;
            };
            let framed = WS_OVERLAPPEDWINDOW.0 as isize;
            unsafe {
                SetWindowLongPtrW(
                    self.hwnd,
                    GWL_STYLE,
                    (style & !framed) | WS_POPUP.0 as isize,
                )
            };
            let _ = unsafe {
                SetWindowPos(
                    self.hwnd,
                    Some(HWND_TOP),
                    area.left,
                    area.top,
                    area.right - area.left,
                    area.bottom - area.top,
                    SWP_NOOWNERZORDER | SWP_FRAMECHANGED,
                )
            };
            self.restore = Some((style, placement));
        } else if let Some((style, placement)) = self.restore.take() {
            unsafe { SetWindowLongPtrW(self.hwnd, GWL_STYLE, style) };
            let _ = unsafe { SetWindowPlacement(self.hwnd, &placement) };
            let _ = unsafe {
                SetWindowPos(
                    self.hwnd,
                    None,
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOOWNERZORDER | SWP_FRAMECHANGED,
                )
            };
        }
    }

    /// The whole monitor the window is (mostly) on.
    pub fn monitor_rect(&self) -> Option<RECT> {
        let monitor = unsafe { MonitorFromWindow(self.hwnd, MONITOR_DEFAULTTONEAREST) };
        let mut info = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        unsafe { GetMonitorInfoW(monitor, &mut info) }
            .as_bool()
            .then_some(info.rcMonitor)
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
        // The keyboard hook goes with the window.
        crate::shortcuts::set(self.hwnd, false);
        let _ = unsafe { DestroyWindow(self.hwnd) };
        self.pump();
        EVENTS.with_borrow_mut(Vec::clear);
    }
}

thread_local! {
    /// Game mouse is on (the window procedure hides the pointer and reports raw movement).
    static GAME_MOUSE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Holds the pointer inside the window's drawing area.
fn clip_to_client(hwnd: HWND) {
    let mut area = RECT::default();
    if unsafe { GetClientRect(hwnd, &mut area) }.is_err() {
        return;
    }
    let mut corners = [
        POINT {
            x: area.left,
            y: area.top,
        },
        POINT {
            x: area.right,
            y: area.bottom,
        },
    ];
    for corner in &mut corners {
        let _ = unsafe { ClientToScreen(hwnd, corner) };
    }
    let screen = RECT {
        left: corners[0].x,
        top: corners[0].y,
        right: corners[1].x,
        bottom: corners[1].y,
    };
    let _ = unsafe { ClipCursor(Some(&screen)) };
}

/// For the keyboard hook (same thread): a key event for the window.
pub(crate) fn push_event(event: WindowEvent) {
    push(event);
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
            // With game mouse on, movement comes from raw input instead.
            if !GAME_MOUSE.get() {
                push(WindowEvent::MouseMove { x: low, y: high });
            }
            LRESULT(0)
        }
        WM_INPUT => {
            let mut size = 0u32;
            let header = size_of::<RAWINPUTHEADER>() as u32;
            let handle = HRAWINPUT(lparam.0 as *mut _);
            unsafe { GetRawInputData(handle, RID_INPUT, None, &mut size, header) };
            let mut buffer = vec![0u8; size as usize];
            let read = unsafe {
                GetRawInputData(
                    handle,
                    RID_INPUT,
                    Some(buffer.as_mut_ptr().cast()),
                    &mut size,
                    header,
                )
            };
            if GAME_MOUSE.get()
                && read as usize >= size_of::<RAWINPUTHEADER>()
                && buffer.len() >= size_of::<RAWINPUT>()
            {
                let raw = unsafe { &*buffer.as_ptr().cast::<RAWINPUT>() };
                if raw.header.dwType == RIM_TYPEMOUSE.0 {
                    let mouse = unsafe { raw.data.mouse };
                    let relative = mouse.usFlags.0 & MOUSE_MOVE_ABSOLUTE.0 == 0;
                    if relative && (mouse.lLastX != 0 || mouse.lLastY != 0) {
                        push(WindowEvent::MouseDelta {
                            dx: mouse.lLastX,
                            dy: mouse.lLastY,
                        });
                    }
                }
            }
            unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
        }
        WM_SETCURSOR if GAME_MOUSE.get() && (lparam.0 & 0xFFFF) as u32 == HTCLIENT => {
            unsafe { SetCursor(None) };
            LRESULT(1)
        }
        WM_SIZE | WM_MOVE if GAME_MOUSE.get() => {
            clip_to_client(hwnd);
            unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
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
        // That PC's pointer: its shape over the picture, or none while it is hidden.
        WM_SETCURSOR
            if (lparam.0 & 0xFFFF) as u32 == HTCLIENT
                && !GAME_MOUSE.get()
                && (POINTER_HIDDEN.get() || REMOTE_CURSOR.get() != 0) =>
        {
            let cursor = if POINTER_HIDDEN.get() {
                None
            } else {
                Some(HCURSOR(REMOTE_CURSOR.get() as *mut _))
            };
            unsafe { SetCursor(cursor) };
            LRESULT(1)
        }
        WM_HOTKEY => {
            push(WindowEvent::Hotkey(wparam.0 as i32));
            LRESULT(0)
        }
        WM_KILLFOCUS => {
            push(WindowEvent::FocusLost);
            unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
        }
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}
