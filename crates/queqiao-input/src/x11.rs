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
    wayland: Option<Mutex<Result<crate::wayland::WaylandInjector, String>>>,
}

impl Injector {
    pub fn new(screen: ScreenRect) -> Self {
        if crate::wayland::wayland_session() {
            return Injector {
                screen,
                state: Mutex::new(None),
                wayland: Some(Mutex::new(crate::wayland::WaylandInjector::new(screen))),
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

    /// How input goes (XTEST, or on Wayland the virtual devices or the portal), or why it cannot.
    pub fn describe(&self) -> String {
        if let Some(wayland) = &self.wayland {
            return match wayland.lock().as_deref() {
                Ok(Ok(injector)) => injector.describe(),
                Ok(Err(error)) => format!("none ({error})"),
                Err(_) => "none (the injector broke)".into(),
            };
        }
        match self.state.lock().as_deref() {
            Ok(Some(_)) => "XTEST".into(),
            _ => "none (no X display with XTEST)".into(),
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

    /// Types `text` as characters whatever the keyboard layout. X11: each character's keysym is put on a key that has
    /// none (as xdotool does), pressed and let go, and the key is emptied again at the end; Wayland: the portal's
    /// keysyms. A line break as Return, a tab as Tab.
    pub fn type_text(&self, text: &str) -> Result<(), String> {
        if let Some(wayland) = &self.wayland {
            let mut wayland = wayland
                .lock()
                .map_err(|_| "the injector broke".to_string())?;
            return match wayland.as_mut() {
                Ok(input) => input.type_text(text),
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
        let error = |e: &dyn std::fmt::Display| format!("typing text: {e}");
        let (first, last) = {
            let setup = state.connection.setup();
            (setup.min_keycode, setup.max_keycode)
        };
        let mapping = state
            .connection
            .get_keyboard_mapping(first, last - first + 1)
            .map_err(|e| error(&e))?
            .reply()
            .map_err(|e| error(&e))?;
        let per = usize::from(mapping.keysyms_per_keycode).max(1);
        // Keys with no keysym at all (the highest first), up to 16. Programs reread the keyboard lazily (Xlib fetches
        // the new map when it next looks a key up, after MappingNotify), so a key must keep its character until the
        // program has looked at the key event: each character goes on the next spare key in turn, a key is used
        // again only after a pause, and they are all emptied after one at the end (one key for everything lost
        // characters: found 2026-11-26).
        let spares: Vec<u8> = (0..=usize::from(last - first))
            .rev()
            .filter(|&i| {
                mapping
                    .keysyms
                    .get(i * per..(i + 1) * per)
                    .is_some_and(|syms| syms.iter().all(|&k| k == 0))
            })
            .map(|i| first + i as u8)
            .take(16)
            .collect();
        if spares.is_empty() {
            return Err("typing text: no free key on this keyboard to type with".into());
        }
        let sync = |state: &State| {
            state
                .connection
                .get_input_focus()
                .map_err(|e| error(&e))?
                .reply()
                .map(|_| ())
                .map_err(|e| error(&e))
        };
        // What each spare key holds now (0: nothing yet), and the next one to take.
        let mut holding = vec![0u32; spares.len()];
        let mut next = 0;
        let mut result = Ok(());
        for c in text.chars() {
            let Some(symbol) = keysym(c) else { continue };
            let slot = match holding.iter().position(|&held| held == symbol) {
                Some(slot) => slot,
                None => {
                    let slot = next % spares.len();
                    if next >= spares.len() {
                        // This key typed a character a moment ago: let the program read it first.
                        std::thread::sleep(std::time::Duration::from_millis(20));
                    }
                    next += 1;
                    // Both levels (a single alphabetic keysym would type its lower case without Shift).
                    let changed = state
                        .connection
                        .change_keyboard_mapping(1, spares[slot], 2, &[symbol, symbol])
                        .map_err(|e| error(&e))
                        .and_then(|_| sync(state));
                    if let Err(e) = changed {
                        result = Err(e);
                        break;
                    }
                    holding[slot] = symbol;
                    slot
                }
            };
            for kind in [xproto::KEY_PRESS_EVENT, xproto::KEY_RELEASE_EVENT] {
                if let Err(e) = state.connection.xtest_fake_input(
                    kind,
                    spares[slot],
                    x11rb::CURRENT_TIME,
                    state.root,
                    0,
                    0,
                    0,
                ) {
                    result = Err(error(&e));
                }
            }
            if let Err(e) = sync(state) {
                result = Err(e);
                break;
            }
        }
        // The keys emptied again, once the programs have read the last characters.
        std::thread::sleep(std::time::Duration::from_millis(60));
        for (slot, &held) in holding.iter().enumerate() {
            if held != 0 {
                let _ = state
                    .connection
                    .change_keyboard_mapping(1, spares[slot], 2, &[0, 0]);
            }
        }
        state.connection.flush().map_err(|e| error(&e))?;
        result
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

/// The X keysym that types `c`: Latin-1 as itself, anything else as 0x01000000 + its code point; a line break is
/// Return, a tab Tab; None for a carriage return (the line break comes as '\n').
pub(crate) fn keysym(c: char) -> Option<u32> {
    match c {
        '\n' => Some(0xff0d),
        '\t' => Some(0xff09),
        '\r' => None,
        c if (c as u32) >= 0x20 && (c as u32) < 0x7f => Some(c as u32),
        c if (c as u32) >= 0xa0 && (c as u32) <= 0xff => Some(c as u32),
        c if (c as u32) < 0x20 || (c as u32) == 0x7f => None,
        c => Some(0x0100_0000 | c as u32),
    }
}

#[cfg(test)]
mod keysym_tests {
    use super::keysym;

    #[test]
    fn characters_become_keysyms() {
        assert_eq!(keysym('a'), Some(0x61));
        assert_eq!(keysym('é'), Some(0xe9));
        assert_eq!(keysym('中'), Some(0x0100_4e2d));
        assert_eq!(keysym('🙂'), Some(0x0101_f642));
        assert_eq!(keysym('\n'), Some(0xff0d));
        assert_eq!(keysym('\r'), None);
    }
}
