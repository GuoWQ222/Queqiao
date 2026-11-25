//! Input on Linux under X11 (docs/LINUX.md): the viewer's mouse and keyboard through the XTEST extension, with the
//! same API as Windows' `SendInput` injector. Positions arrive relative to the captured monitor (0..=65535) and land
//! on its place on the X screen; keys arrive as scancodes and become X key codes (`scancode.rs`), so this PC's
//! keyboard layout decides the characters.

use std::sync::Mutex;

use queqiao_core::protocol::{InputEvent, MouseButton};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{self, ConnectionExt as _};
use x11rb::protocol::xtest::ConnectionExt as _;
use x11rb::rust_connection::RustConnection;

use crate::ScreenRect;
use crate::scancode::x11_keycode;

/// X button numbers.
const LEFT: u8 = 1;
const MIDDLE: u8 = 2;
const RIGHT: u8 = 3;
const WHEEL_UP: u8 = 4;
const WHEEL_DOWN: u8 = 5;
const WHEEL_LEFT: u8 = 6;
const WHEEL_RIGHT: u8 = 7;
const BACK: u8 = 8;
const FORWARD: u8 = 9;
/// Windows' wheel units per notch.
const NOTCH: i32 = 120;

struct State {
    connection: RustConnection,
    root: u32,
    /// Wheel movement not yet a whole notch (vertical, horizontal), so smooth scrolling is not lost.
    wheel: (i32, i32),
}

/// Injects input into the X display named by $DISPLAY, or in a Wayland session into the compositor (wayland.rs),
/// where XTEST would reach only XWayland's windows.
pub struct Injector {
    screen: ScreenRect,
    state: Mutex<Option<State>>,
    /// The Wayland injector, or why there is none, in a Wayland session.
    wayland: Option<Mutex<Result<crate::wayland::WaylandInput, String>>>,
}

impl Injector {
    pub fn new(screen: ScreenRect) -> Self {
        if crate::wayland::wayland_session() {
            return Injector {
                screen,
                state: Mutex::new(None),
                wayland: Some(Mutex::new(crate::wayland::WaylandInput::new(screen))),
            };
        }
        let state = x11rb::connect(None).ok().and_then(|(connection, number)| {
            let root = connection.setup().roots.get(number)?.root;
            connection.xtest_get_version(2, 2).ok()?.reply().ok()?;
            Some(State {
                connection,
                root,
                wheel: (0, 0),
            })
        });
        Injector {
            screen,
            state: Mutex::new(state),
            wayland: None,
        }
    }

    /// The screen positions map onto.
    pub fn screen(&self) -> ScreenRect {
        self.screen
    }

    /// Injects one event; an error if the X server has no XTEST or refused it.
    pub fn inject(&self, event: &InputEvent) -> Result<(), String> {
        if let Some(wayland) = &self.wayland {
            let mut wayland = wayland
                .lock()
                .map_err(|_| "the injector broke".to_string())?;
            return match wayland.as_mut() {
                Ok(input) => input.inject(self.screen, event),
                Err(error) => Err(error.clone()),
            };
        }
        let mut guard = self
            .state
            .lock()
            .map_err(|_| "the injector broke".to_string())?;
        let state = guard
            .as_mut()
            .ok_or("no X display with XTEST to inject into")?;
        let fake = |state: &State, kind: u8, detail: u8, x: i16, y: i16| {
            state
                .connection
                .xtest_fake_input(kind, detail, x11rb::CURRENT_TIME, state.root, x, y, 0)
                .map(|_| ())
                .map_err(|e| format!("XTEST: {e}"))
        };
        match *event {
            InputEvent::MouseMove { x, y } => {
                let (x, y) = self.screen.desktop_point(x, y);
                fake(state, xproto::MOTION_NOTIFY_EVENT, 0, x as i16, y as i16)?;
            }
            InputEvent::MouseDelta { dx, dy } => {
                // Relative motion (detail 1).
                fake(state, xproto::MOTION_NOTIFY_EVENT, 1, dx, dy)?;
            }
            InputEvent::MouseButton { button, down } => {
                let number = match button {
                    MouseButton::Left => LEFT,
                    MouseButton::Middle => MIDDLE,
                    MouseButton::Right => RIGHT,
                    MouseButton::X1 => BACK,
                    MouseButton::X2 => FORWARD,
                };
                let kind = if down {
                    xproto::BUTTON_PRESS_EVENT
                } else {
                    xproto::BUTTON_RELEASE_EVENT
                };
                fake(state, kind, number, 0, 0)?;
            }
            InputEvent::Wheel { delta, horizontal } => {
                let pending = if horizontal {
                    &mut state.wheel.1
                } else {
                    &mut state.wheel.0
                };
                *pending += i32::from(delta);
                let notches = *pending / NOTCH;
                *pending -= notches * NOTCH;
                // Positive is away from the person (up) or to the right.
                let button = match (horizontal, notches > 0) {
                    (false, true) => WHEEL_UP,
                    (false, false) => WHEEL_DOWN,
                    (true, true) => WHEEL_RIGHT,
                    (true, false) => WHEEL_LEFT,
                };
                for _ in 0..notches.unsigned_abs() {
                    fake(state, xproto::BUTTON_PRESS_EVENT, button, 0, 0)?;
                    fake(state, xproto::BUTTON_RELEASE_EVENT, button, 0, 0)?;
                }
            }
            InputEvent::Key {
                scancode,
                extended,
                down,
            } => {
                let Some(code) = x11_keycode(scancode, extended) else {
                    return Err(format!(
                        "no key for scancode {scancode:#x}{} here",
                        if extended { " (E0)" } else { "" }
                    ));
                };
                let kind = if down {
                    xproto::KEY_PRESS_EVENT
                } else {
                    xproto::KEY_RELEASE_EVENT
                };
                fake(state, kind, code, 0, 0)?;
            }
        }
        state.connection.flush().map_err(|e| format!("XTEST: {e}"))
    }
}

/// The pointer's position on the X screen.
pub fn cursor_position() -> (i32, i32) {
    let Ok((connection, number)) = x11rb::connect(None) else {
        return (0, 0);
    };
    let root = connection.setup().roots[number].root;
    connection
        .query_pointer(root)
        .ok()
        .and_then(|cookie| cookie.reply().ok())
        .map_or((0, 0), |reply| {
            (i32::from(reply.root_x), i32::from(reply.root_y))
        })
}

/// Moves the pointer (to put it back after an input test).
pub fn set_cursor_position(x: i32, y: i32) {
    if let Ok((connection, number)) = x11rb::connect(None) {
        let root = connection.setup().roots[number].root;
        let _ = connection.warp_pointer(x11rb::NONE, root, 0, 0, 0, 0, x as i16, y as i16);
        let _ = connection.flush();
    }
}

/// The scancode of a Windows virtual-key code, for the keys the tests ask about: F1-F24, letters, digits.
fn scancode_of_virtual_key(virtual_key: u16) -> Option<(u16, bool)> {
    const LETTERS: [u16; 26] = [
        0x1E, 0x30, 0x2E, 0x20, 0x12, 0x21, 0x22, 0x23, 0x17, 0x24, 0x25, 0x26, 0x32, 0x31, 0x18,
        0x19, 0x10, 0x13, 0x1F, 0x14, 0x16, 0x2F, 0x11, 0x2D, 0x15, 0x2C,
    ];
    const FUNCTION: [u16; 24] = [
        0x3B, 0x3C, 0x3D, 0x3E, 0x3F, 0x40, 0x41, 0x42, 0x43, 0x44, 0x57, 0x58, 0x64, 0x65, 0x66,
        0x67, 0x68, 0x69, 0x6A, 0x6B, 0x6C, 0x6D, 0x6E, 0x76,
    ];
    let scancode = match virtual_key {
        0x30 => 0x0B,
        0x31..=0x39 => virtual_key - 0x31 + 0x02,
        0x41..=0x5A => LETTERS[usize::from(virtual_key - 0x41)],
        0x70..=0x87 => FUNCTION[usize::from(virtual_key - 0x70)],
        _ => return None,
    };
    Some((scancode, false))
}

/// True while the key with this Windows virtual-key code is down (injected keys count), as on Windows: the
/// agent's input test asks about F24 (0x87).
pub fn key_is_down(virtual_key: u16) -> bool {
    let Some((scancode, extended)) = scancode_of_virtual_key(virtual_key) else {
        return false;
    };
    let Some(code) = x11_keycode(scancode, extended) else {
        return false;
    };
    let Ok((connection, _)) = x11rb::connect(None) else {
        return false;
    };
    connection
        .query_keymap()
        .ok()
        .and_then(|cookie| cookie.reply().ok())
        .is_some_and(|keymap| keymap.keys[usize::from(code / 8)] & (1 << (code % 8)) != 0)
}
