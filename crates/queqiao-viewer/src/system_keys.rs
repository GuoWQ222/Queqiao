//! A session key as the system takes it for a key that works with the window hidden (the boss key): Windows'
//! RegisterHotKey, an X key grab on the root window. The keys themselves are queqiao_core::hotkeys.

use queqiao_core::hotkeys::Hotkey;

pub(crate) trait SystemKey {
    /// Windows: the modifiers and virtual key for RegisterHotKey. Linux: the X modifier bits (Control, Mod1 for Alt,
    /// Shift) and the X keycode (0 for a key the evdev table does not know).
    fn system(&self) -> (u32, u32);
}

impl SystemKey for Hotkey {
    #[cfg(windows)]
    fn system(&self) -> (u32, u32) {
        use windows::Win32::UI::Input::KeyboardAndMouse::{MAPVK_VSC_TO_VK_EX, MapVirtualKeyW};
        const MOD_ALT: u32 = 0x1;
        const MOD_CONTROL: u32 = 0x2;
        const MOD_SHIFT: u32 = 0x4;
        const MOD_NOREPEAT: u32 = 0x4000;
        let modifiers = MOD_NOREPEAT
            | if self.alt { MOD_ALT } else { 0 }
            | if self.ctrl { MOD_CONTROL } else { 0 }
            | if self.shift { MOD_SHIFT } else { 0 };
        let code = u32::from(self.scancode) | if self.extended { 0xE000 } else { 0 };
        let vk = unsafe { MapVirtualKeyW(code, MAPVK_VSC_TO_VK_EX) };
        (modifiers, vk)
    }

    #[cfg(target_os = "linux")]
    fn system(&self) -> (u32, u32) {
        const SHIFT: u32 = 1;
        const CONTROL: u32 = 4;
        const MOD1: u32 = 8;
        let modifiers = if self.alt { MOD1 } else { 0 }
            | if self.ctrl { CONTROL } else { 0 }
            | if self.shift { SHIFT } else { 0 };
        let keycode =
            queqiao_input::scancode::x11_keycode(self.scancode, self.extended).unwrap_or(0);
        (modifiers, u32::from(keycode))
    }
}
