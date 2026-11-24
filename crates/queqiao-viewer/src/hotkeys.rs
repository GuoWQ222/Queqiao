//! The session window's own keys, which the person can change ("Hotkeys..." in the session menu): full screen, game
//! mouse, and the boss key (hides the session window and its sound at once, from anywhere; again to bring it back).
//! A key is Ctrl and/or Alt (Shift may be added) with one other key, written as "Ctrl+Alt+Enter"; it is never sent to
//! the other PC. Kept in `%APPDATA%\Queqiao\viewer_settings.json`.

use std::collections::HashSet;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Scancodes (set 1) of Ctrl, Alt and Shift, left and right (right Ctrl and Alt carry the extended flag).
const CTRL: u16 = 0x1D;
const ALT: u16 = 0x38;
const LEFT_SHIFT: u16 = 0x2A;
const RIGHT_SHIFT: u16 = 0x36;

/// Keys by name: (name, scancode, extended).
const KEYS: &[(&str, u16, bool)] = &[
    ("Enter", 0x1C, false),
    ("Space", 0x39, false),
    ("Tab", 0x0F, false),
    ("Backspace", 0x0E, false),
    ("Insert", 0x52, true),
    ("Delete", 0x53, true),
    ("Home", 0x47, true),
    ("End", 0x4F, true),
    ("PageUp", 0x49, true),
    ("PageDown", 0x51, true),
    ("Up", 0x48, true),
    ("Down", 0x50, true),
    ("Left", 0x4B, true),
    ("Right", 0x4D, true),
    ("F1", 0x3B, false),
    ("F2", 0x3C, false),
    ("F3", 0x3D, false),
    ("F4", 0x3E, false),
    ("F5", 0x3F, false),
    ("F6", 0x40, false),
    ("F7", 0x41, false),
    ("F8", 0x42, false),
    ("F9", 0x43, false),
    ("F10", 0x44, false),
    ("F11", 0x57, false),
    ("F12", 0x58, false),
    ("1", 0x02, false),
    ("2", 0x03, false),
    ("3", 0x04, false),
    ("4", 0x05, false),
    ("5", 0x06, false),
    ("6", 0x07, false),
    ("7", 0x08, false),
    ("8", 0x09, false),
    ("9", 0x0A, false),
    ("0", 0x0B, false),
    ("Q", 0x10, false),
    ("W", 0x11, false),
    ("E", 0x12, false),
    ("R", 0x13, false),
    ("T", 0x14, false),
    ("Y", 0x15, false),
    ("U", 0x16, false),
    ("I", 0x17, false),
    ("O", 0x18, false),
    ("P", 0x19, false),
    ("A", 0x1E, false),
    ("S", 0x1F, false),
    ("D", 0x20, false),
    ("F", 0x21, false),
    ("G", 0x22, false),
    ("H", 0x23, false),
    ("J", 0x24, false),
    ("K", 0x25, false),
    ("L", 0x26, false),
    ("Z", 0x2C, false),
    ("X", 0x2D, false),
    ("C", 0x2E, false),
    ("V", 0x2F, false),
    ("B", 0x30, false),
    ("N", 0x31, false),
    ("M", 0x32, false),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hotkey {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub scancode: u16,
    pub extended: bool,
}

impl Hotkey {
    /// "Ctrl+Alt+Enter" and the like (case does not matter); None unless it has Ctrl or Alt and one known key.
    pub fn parse(text: &str) -> Option<Hotkey> {
        let (mut ctrl, mut alt, mut shift) = (false, false, false);
        let mut key: Option<(u16, bool)> = None;
        for part in text.split('+').map(str::trim) {
            match part.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => ctrl = true,
                "alt" => alt = true,
                "shift" => shift = true,
                name => {
                    let found = KEYS
                        .iter()
                        .find(|(known, _, _)| known.eq_ignore_ascii_case(name))?;
                    if key.replace((found.1, found.2)).is_some() {
                        return None;
                    }
                }
            }
        }
        let (scancode, extended) = key?;
        (ctrl || alt).then_some(Hotkey {
            ctrl,
            alt,
            shift,
            scancode,
            extended,
        })
    }

    pub fn text(&self) -> String {
        let name = KEYS
            .iter()
            .find(|(_, scancode, extended)| {
                *scancode == self.scancode && *extended == self.extended
            })
            .map_or("?", |(name, _, _)| name);
        let mut parts = Vec::new();
        if self.ctrl {
            parts.push("Ctrl");
        }
        if self.alt {
            parts.push("Alt");
        }
        if self.shift {
            parts.push("Shift");
        }
        parts.push(name);
        parts.join("+")
    }

    /// Whether this key going down (with these keys held) is this hotkey. Enter is either Enter.
    pub fn matches(&self, scancode: u16, extended: bool, held: &HashSet<(u16, bool)>) -> bool {
        let held_code = |code: u16| held.iter().any(|(held, _)| *held == code);
        let same_key = scancode == self.scancode && (extended == self.extended || scancode == 0x1C);
        same_key
            && held_code(CTRL) == self.ctrl
            && held_code(ALT) == self.alt
            && (held_code(LEFT_SHIFT) || held_code(RIGHT_SHIFT)) == self.shift
    }

    /// The modifiers and virtual key for RegisterHotKey (a boss key works with the window hidden).
    #[cfg(windows)]
    pub fn system(&self) -> (u32, u32) {
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

    /// The X modifier bits (Control, Mod1 for Alt, Shift) and X keycode for a key grab on the root window (a boss
    /// key works with the window hidden); keycode 0 for a key the evdev table does not know.
    #[cfg(target_os = "linux")]
    pub fn system(&self) -> (u32, u32) {
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

/// The keys as kept: names, so the file reads as the menu does. An empty boss key: none.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Keys {
    #[serde(default = "default_full_screen")]
    pub full_screen: String,
    #[serde(default = "default_game_mouse")]
    pub game_mouse: String,
    #[serde(default)]
    pub boss_key: String,
}

fn default_full_screen() -> String {
    "Ctrl+Alt+Enter".into()
}

fn default_game_mouse() -> String {
    "Ctrl+Alt+M".into()
}

impl Default for Keys {
    fn default() -> Keys {
        Keys {
            full_screen: default_full_screen(),
            game_mouse: default_game_mouse(),
            boss_key: String::new(),
        }
    }
}

#[derive(Default, Serialize, Deserialize)]
struct Stored {
    #[serde(default)]
    keys: Keys,
}

fn file() -> Option<PathBuf> {
    queqiao_account::data_folder()
        .ok()
        .map(|folder| folder.join("viewer_settings.json"))
}

impl Keys {
    pub fn load() -> Keys {
        file()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .and_then(|text| serde_json::from_str::<Stored>(&text).ok())
            .map(|stored| stored.keys)
            .unwrap_or_default()
    }

    pub fn save(&self) -> Result<(), String> {
        let path = file().ok_or("no settings folder")?;
        let text = serde_json::to_string_pretty(&Stored { keys: self.clone() })
            .map_err(|e| e.to_string())?;
        std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))
    }

    pub fn full_screen(&self) -> Hotkey {
        Hotkey::parse(&self.full_screen)
            .or_else(|| Hotkey::parse(&default_full_screen()))
            .expect("the default parses")
    }

    pub fn game_mouse(&self) -> Hotkey {
        Hotkey::parse(&self.game_mouse)
            .or_else(|| Hotkey::parse(&default_game_mouse()))
            .expect("the default parses")
    }

    pub fn boss_key(&self) -> Option<Hotkey> {
        Hotkey::parse(&self.boss_key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_read_back() {
        for text in [
            "Ctrl+Alt+Enter",
            "Ctrl+Alt+M",
            "Ctrl+Shift+F12",
            "Alt+Home",
            "Ctrl+Alt+Shift+9",
        ] {
            assert_eq!(Hotkey::parse(text).unwrap().text(), text);
        }
        assert_eq!(
            Hotkey::parse(" ctrl + alt + m ").unwrap().text(),
            "Ctrl+Alt+M"
        );
        let home = Hotkey::parse("Alt+Home").unwrap();
        assert!(home.extended && home.scancode == 0x47);
    }

    #[test]
    fn plain_or_unknown_keys_are_refused() {
        for text in [
            "",
            "M",
            "Shift+M",
            "Ctrl+Alt",
            "Ctrl+Alt+M+N",
            "Ctrl+Alt+Foo",
            "Ctrl+Win+M",
        ] {
            assert_eq!(Hotkey::parse(text), None, "{text:?}");
        }
    }

    #[test]
    fn matches_with_exactly_its_modifiers() {
        let key = Hotkey::parse("Ctrl+Alt+M").unwrap();
        let held = |codes: &[(u16, bool)]| codes.iter().copied().collect::<HashSet<_>>();
        assert!(key.matches(0x32, false, &held(&[(CTRL, false), (ALT, false)])));
        // Right Ctrl and right Alt count too.
        assert!(key.matches(0x32, false, &held(&[(CTRL, true), (ALT, true)])));
        assert!(!key.matches(0x32, false, &held(&[(CTRL, false)])));
        assert!(!key.matches(
            0x32,
            false,
            &held(&[(CTRL, false), (ALT, false), (LEFT_SHIFT, false)])
        ));
        assert!(!key.matches(0x31, false, &held(&[(CTRL, false), (ALT, false)])));
        // Either Enter.
        let enter = Hotkey::parse("Ctrl+Alt+Enter").unwrap();
        assert!(enter.matches(0x1C, true, &held(&[(CTRL, false), (ALT, false)])));
    }

    #[test]
    fn missing_or_broken_settings_fall_back() {
        let keys: Keys = serde_json::from_str("{}").unwrap();
        assert_eq!(keys, Keys::default());
        let broken = Keys {
            full_screen: "nonsense".into(),
            game_mouse: "M".into(),
            boss_key: "also nonsense".into(),
        };
        assert_eq!(broken.full_screen().text(), "Ctrl+Alt+Enter");
        assert_eq!(broken.game_mouse().text(), "Ctrl+Alt+M");
        assert_eq!(broken.boss_key(), None);
    }
}
