//! Keys between the protocol and Linux (docs/LINUX.md): the protocol carries Windows' hardware scancodes (set 1,
//! with the E0 "extended" flag), so the controlled PC's keyboard layout decides the characters on both systems.
//! Linux names keys by evdev code (`KEY_*` in linux/input-event-codes.h): for the keys of the original XT keyboard
//! (scancodes 0x01-0x58 without E0) the evdev code is the scancode itself; the extended keys and the later ones
//! (F13-F24, the 102nd key, the Japanese and Korean keys) go through the table below. X11's key codes are evdev + 8.

/// (scancode, extended, evdev code) for every key that does not map to itself.
const TABLE: &[(u16, bool, u16)] = &[
    // Extended (E0) keys.
    (0x1C, true, 96),  // keypad Enter: KEY_KPENTER
    (0x1D, true, 97),  // right Ctrl: KEY_RIGHTCTRL
    (0x35, true, 98),  // keypad /: KEY_KPSLASH
    (0x37, true, 99),  // Print Screen: KEY_SYSRQ
    (0x38, true, 100), // right Alt (AltGr): KEY_RIGHTALT
    (0x47, true, 102), // Home
    (0x48, true, 103), // Up
    (0x49, true, 104), // Page Up
    (0x4B, true, 105), // Left
    (0x4D, true, 106), // Right
    (0x4F, true, 107), // End
    (0x50, true, 108), // Down
    (0x51, true, 109), // Page Down
    (0x52, true, 110), // Insert
    (0x53, true, 111), // Delete
    (0x5B, true, 125), // left Windows: KEY_LEFTMETA
    (0x5C, true, 126), // right Windows: KEY_RIGHTMETA
    (0x5D, true, 127), // Menu: KEY_COMPOSE
    (0x5E, true, 116), // Power
    (0x5F, true, 142), // Sleep
    (0x63, true, 143), // Wake
    (0x20, true, 113), // Mute
    (0x2E, true, 114), // Volume Down
    (0x30, true, 115), // Volume Up
    (0x22, true, 164), // Play/Pause
    (0x24, true, 166), // Stop
    (0x10, true, 165), // Previous track
    (0x19, true, 163), // Next track
    (0x45, false, 69), // Num Lock (Pause is E1 1D 45; Windows sends Num Lock as 0x45)
    // Later keys without E0.
    (0x46, true, 119), // Pause/Break as Windows sends it (E0 46, Ctrl+Pause): KEY_PAUSE
    (0x56, false, 86), // the 102nd key (< > | on European keyboards): KEY_102ND
    (0x57, false, 87), // F11
    (0x58, false, 88), // F12
    (0x64, false, 183), // F13
    (0x65, false, 184), // F14
    (0x66, false, 185), // F15
    (0x67, false, 186), // F16
    (0x68, false, 187), // F17
    (0x69, false, 188), // F18
    (0x6A, false, 189), // F19
    (0x6B, false, 190), // F20
    (0x6C, false, 191), // F21
    (0x6D, false, 192), // F22
    (0x6E, false, 193), // F23
    (0x76, false, 194), // F24 (the agent's input test key)
    (0x70, false, 93), // Katakana/Hiragana: KEY_KATAKANAHIRAGANA
    (0x73, false, 89), // Ro: KEY_RO
    (0x79, false, 92), // Henkan: KEY_HENKAN
    (0x7B, false, 94), // Muhenkan: KEY_MUHENKAN
    (0x7D, false, 124), // Yen: KEY_YEN
    (0x72, false, 122), // Hangul/English (Korean): KEY_HANGEUL
    (0x71, false, 123), // Hanja (Korean): KEY_HANJA
];

/// The evdev key code for a scancode, if Linux has the key.
pub fn evdev_from_scancode(scancode: u16, extended: bool) -> Option<u16> {
    if let Some((_, _, code)) = TABLE
        .iter()
        .find(|(s, e, _)| *s == scancode && *e == extended)
    {
        return Some(*code);
    }
    // The original XT keys: the evdev code is the scancode (Esc 1 ... F10 68, keypad 71-83).
    (!extended && (0x01..=0x53).contains(&scancode)).then_some(scancode)
}

/// The scancode (and E0 flag) for an evdev key code, if the protocol has the key.
pub fn scancode_from_evdev(code: u16) -> Option<(u16, bool)> {
    if let Some((scancode, extended, _)) = TABLE.iter().find(|(_, _, c)| *c == code) {
        return Some((*scancode, *extended));
    }
    (0x01..=0x53).contains(&code).then_some((code, false))
}

/// X11's key code for a scancode (evdev + 8).
pub fn x11_keycode(scancode: u16, extended: bool) -> Option<u8> {
    evdev_from_scancode(scancode, extended).and_then(|code| u8::try_from(code + 8).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn common_keys_map_to_themselves() {
        assert_eq!(evdev_from_scancode(0x01, false), Some(1)); // Esc
        assert_eq!(evdev_from_scancode(0x1E, false), Some(30)); // A
        assert_eq!(evdev_from_scancode(0x1C, false), Some(28)); // Enter
        assert_eq!(evdev_from_scancode(0x1D, false), Some(29)); // left Ctrl
        assert_eq!(evdev_from_scancode(0x44, false), Some(68)); // F10
        assert_eq!(x11_keycode(0x1E, false), Some(38)); // A is X key code 38
    }

    #[test]
    fn extended_keys_differ_from_their_keypad_twins() {
        // The arrows and the keypad's 8/4/6/2 share scancodes; E0 tells them apart.
        assert_eq!(evdev_from_scancode(0x48, true), Some(103));
        assert_eq!(evdev_from_scancode(0x48, false), Some(0x48));
        assert_eq!(evdev_from_scancode(0x1D, true), Some(97));
        assert_eq!(evdev_from_scancode(0x5B, true), Some(125));
        assert_eq!(evdev_from_scancode(0x76, false), Some(194)); // F24
        // An extended scancode Linux does not have.
        assert_eq!(evdev_from_scancode(0x01, true), None);
    }

    #[test]
    fn both_ways_agree() {
        for code in 1..=255u16 {
            if let Some((scancode, extended)) = scancode_from_evdev(code) {
                assert_eq!(
                    evdev_from_scancode(scancode, extended),
                    Some(code),
                    "{code}"
                );
            }
        }
        for scancode in 1..=0x7Fu16 {
            for extended in [false, true] {
                if let Some(code) = evdev_from_scancode(scancode, extended) {
                    assert_eq!(
                        scancode_from_evdev(code),
                        Some((scancode, extended)),
                        "{scancode:#x} {extended}"
                    );
                }
            }
        }
    }
}
