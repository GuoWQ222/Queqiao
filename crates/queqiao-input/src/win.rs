//! Input on Windows (see lib.rs): the viewer's mouse and keyboard with `SendInput`.
//!
//! Input on the controlled PC: injects the viewer's mouse and keyboard events with `SendInput`.
//!
//! Pointer positions arrive relative to the captured screen (0..=65535 across its width and height) and
//! are mapped onto the whole Windows desktop, so any screen of a multi-monitor PC works. Keys arrive as
//! hardware scancodes, so the controlled PC's keyboard layout decides the characters. The process must
//! be per-monitor DPI aware (the agent is), so desktop positions are physical pixels.

use crate::ScreenRect;
use queqiao_core::protocol::{InputEvent, MouseButton};
use windows::Win32::Foundation::POINT;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::{
    GetCursorPos, GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
    SM_YVIRTUALSCREEN, SetCursorPos,
};

pub struct Injector {
    screen: ScreenRect,
}

impl Injector {
    pub fn new(screen: ScreenRect) -> Self {
        Self { screen }
    }

    pub fn screen(&self) -> ScreenRect {
        self.screen
    }

    /// Injects one event. Windows refuses injection into windows of higher integrity (for example an
    /// elevated program in front) unless the agent itself runs elevated; that shows up as an error.
    pub fn inject(&self, event: &InputEvent) -> Result<(), String> {
        let input = match *event {
            InputEvent::MouseMove { x, y } => {
                let (px, py) = self.screen.desktop_point(x, y);
                let (vx, vy, vw, vh) = virtual_desktop();
                let scale = |value: i32, origin: i32, size: i32| {
                    (i64::from(value - origin) * 65535 / i64::from((size - 1).max(1))) as i32
                };
                let flags = MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK;
                mouse(scale(px, vx, vw), scale(py, vy, vh), 0, flags)
            }
            InputEvent::MouseButton { button, down } => {
                let (flags, data) = match (button, down) {
                    (MouseButton::Left, true) => (MOUSEEVENTF_LEFTDOWN, 0),
                    (MouseButton::Left, false) => (MOUSEEVENTF_LEFTUP, 0),
                    (MouseButton::Right, true) => (MOUSEEVENTF_RIGHTDOWN, 0),
                    (MouseButton::Right, false) => (MOUSEEVENTF_RIGHTUP, 0),
                    (MouseButton::Middle, true) => (MOUSEEVENTF_MIDDLEDOWN, 0),
                    (MouseButton::Middle, false) => (MOUSEEVENTF_MIDDLEUP, 0),
                    (MouseButton::X1, true) => (MOUSEEVENTF_XDOWN, 1),
                    (MouseButton::X1, false) => (MOUSEEVENTF_XUP, 1),
                    (MouseButton::X2, true) => (MOUSEEVENTF_XDOWN, 2),
                    (MouseButton::X2, false) => (MOUSEEVENTF_XUP, 2),
                };
                mouse(0, 0, data, flags)
            }
            // Relative: Windows applies the PC's pointer speed (and acceleration) as for a real mouse.
            InputEvent::MouseDelta { dx, dy } => {
                mouse(i32::from(dx), i32::from(dy), 0, MOUSEEVENTF_MOVE)
            }
            InputEvent::Wheel { delta, horizontal } => {
                let flags = if horizontal {
                    MOUSEEVENTF_HWHEEL
                } else {
                    MOUSEEVENTF_WHEEL
                };
                mouse(0, 0, i32::from(delta), flags)
            }
            InputEvent::Key {
                scancode,
                extended,
                down,
            } => {
                let mut flags = KEYEVENTF_SCANCODE;
                if extended {
                    flags |= KEYEVENTF_EXTENDEDKEY;
                }
                if !down {
                    flags |= KEYEVENTF_KEYUP;
                }
                INPUT {
                    r#type: INPUT_KEYBOARD,
                    Anonymous: INPUT_0 {
                        ki: KEYBDINPUT {
                            wVk: VIRTUAL_KEY(0),
                            wScan: scancode,
                            dwFlags: flags,
                            time: 0,
                            dwExtraInfo: 0,
                        },
                    },
                }
            }
        };
        let sent = unsafe { SendInput(&[input], size_of::<INPUT>() as i32) };
        if sent == 1 {
            Ok(())
        } else {
            Err(format!(
                "SendInput refused the event: {}",
                windows::core::Error::from_thread().message()
            ))
        }
    }

    /// Types `text` as characters whatever the PC's keyboard layout (KEYEVENTF_UNICODE: each UTF-16 unit as a
    /// VK_PACKET key, which the window receives as WM_CHAR), a line break as Enter, a tab as Tab.
    pub fn type_text(&self, text: &str) -> Result<(), String> {
        let key = |vk: VIRTUAL_KEY, scan: u16, flags: KEYBD_EVENT_FLAGS| INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: vk,
                    wScan: scan,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        };
        let mut inputs = Vec::with_capacity(text.len() * 2);
        for c in text.chars() {
            let virtual_key = match c {
                '\n' => Some(VK_RETURN),
                '\t' => Some(VK_TAB),
                '\r' => continue,
                _ => None,
            };
            if let Some(vk) = virtual_key {
                inputs.push(key(vk, 0, KEYBD_EVENT_FLAGS(0)));
                inputs.push(key(vk, 0, KEYEVENTF_KEYUP));
                continue;
            }
            let mut units = [0u16; 2];
            for &unit in c.encode_utf16(&mut units).iter() {
                inputs.push(key(VIRTUAL_KEY(0), unit, KEYEVENTF_UNICODE));
                inputs.push(key(
                    VIRTUAL_KEY(0),
                    unit,
                    KEYEVENTF_UNICODE | KEYEVENTF_KEYUP,
                ));
            }
        }
        if inputs.is_empty() {
            return Ok(());
        }
        let sent = unsafe { SendInput(&inputs, size_of::<INPUT>() as i32) };
        if sent as usize == inputs.len() {
            Ok(())
        } else {
            Err(format!(
                "SendInput typed {sent} of {} key events: {}",
                inputs.len(),
                windows::core::Error::from_thread().message()
            ))
        }
    }
}

fn mouse(dx: i32, dy: i32, data: i32, flags: MOUSE_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx,
                dy,
                mouseData: data as u32,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

/// The whole Windows desktop (all screens): left, top, width, height in physical pixels.
fn virtual_desktop() -> (i32, i32, i32, i32) {
    unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
        )
    }
}

/// The mouse pointer's desktop position.
pub fn cursor_position() -> (i32, i32) {
    let mut point = POINT::default();
    let _ = unsafe { GetCursorPos(&mut point) };
    (point.x, point.y)
}

pub fn set_cursor_position(x: i32, y: i32) {
    let _ = unsafe { SetCursorPos(x, y) };
}

/// True while the key with this virtual-key code is down (injected keys count).
pub fn key_is_down(virtual_key: u16) -> bool {
    (unsafe { GetAsyncKeyState(i32::from(virtual_key)) } as u16 & 0x8000) != 0
}
