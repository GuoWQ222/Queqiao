//! Windows shortcuts for the controlled PC: while the session window is in front, a low-level keyboard hook
//! takes the keys that Windows itself would act on here (the Windows keys, Alt+Tab, Alt+Esc, Ctrl+Esc,
//! Ctrl+Shift+Esc) and gives them to the window as ordinary key events, so they go to the other PC. Keys
//! Windows never lets a program see (Ctrl+Alt+Del, Win+L) still act here.
//!
//! The hook runs on the window's thread (it pumps messages often), and only while it is turned on.

use std::cell::Cell;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_CONTROL};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetForegroundWindow, HHOOK, KBDLLHOOKSTRUCT, LLKHF_ALTDOWN, LLKHF_EXTENDED,
    LLKHF_UP, SetWindowsHookExW, UnhookWindowsHookEx, WH_KEYBOARD_LL,
};

use crate::window::WindowEvent;

const VK_TAB: u32 = 0x09;
const VK_ESCAPE: u32 = 0x1B;
const VK_LWIN: u32 = 0x5B;
const VK_RWIN: u32 = 0x5C;

thread_local! {
    /// The session window the keys belong to, and the hook while it is set.
    static TARGET: Cell<isize> = const { Cell::new(0) };
    static HOOK: Cell<isize> = const { Cell::new(0) };
}

/// Whether a key goes to the other PC instead of to Windows here: the Windows keys always, Tab with Alt, Esc
/// with Alt or Ctrl (Ctrl+Shift+Esc too).
pub(crate) fn taken(vk: u32, alt: bool, ctrl: bool) -> bool {
    match vk {
        VK_LWIN | VK_RWIN => true,
        VK_TAB => alt,
        VK_ESCAPE => alt || ctrl,
        _ => false,
    }
}

/// Turns the hook on for `window` (or off).
pub(crate) fn set(window: HWND, on: bool) {
    let current = HOOK.get();
    if on && current == 0 {
        let instance = unsafe { GetModuleHandleW(None) }.ok();
        if let Ok(hook) = unsafe {
            SetWindowsHookExW(
                WH_KEYBOARD_LL,
                Some(keyboard_proc),
                instance.map(Into::into),
                0,
            )
        } {
            HOOK.set(hook.0 as isize);
            TARGET.set(window.0 as isize);
        }
    } else if !on && current != 0 {
        let _ = unsafe { UnhookWindowsHookEx(HHOOK(current as *mut _)) };
        HOOK.set(0);
        TARGET.set(0);
    }
}

pub(crate) fn is_on() -> bool {
    HOOK.get() != 0
}

extern "system" fn keyboard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 {
        let target = TARGET.get();
        let in_front = target != 0 && unsafe { GetForegroundWindow() }.0 as isize == target;
        let key = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        let alt = key.flags.0 & LLKHF_ALTDOWN.0 != 0;
        let ctrl = unsafe { GetAsyncKeyState(i32::from(VK_CONTROL.0)) } < 0;
        if in_front && taken(key.vkCode, alt, ctrl) {
            crate::window::push_event(WindowEvent::Key {
                scancode: key.scanCode as u16,
                extended: key.flags.0 & LLKHF_EXTENDED.0 != 0,
                down: key.flags.0 & LLKHF_UP.0 == 0,
            });
            // Taken: Windows here does not see it.
            return LRESULT(1);
        }
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_shortcuts_windows_acts_on_are_taken() {
        assert!(taken(VK_LWIN, false, false));
        assert!(taken(VK_RWIN, false, false));
        assert!(taken(VK_TAB, true, false), "Alt+Tab");
        assert!(
            !taken(VK_TAB, false, false),
            "Tab alone stays an ordinary key"
        );
        assert!(taken(VK_ESCAPE, true, false), "Alt+Esc");
        assert!(taken(VK_ESCAPE, false, true), "Ctrl+Esc and Ctrl+Shift+Esc");
        assert!(
            !taken(VK_ESCAPE, false, false),
            "Esc alone stays an ordinary key"
        );
        assert!(!taken(0x41, true, true), "letters are never taken");
        assert!(!taken(0x73, true, false), "Alt+F4 closes the window here");
    }
}
