import UIKit

/// Keys by their set 1 scancodes (as the protocol sends them: the PC's keyboard layout decides the character).
enum Keys {
    static let shift = 0x2A
    static let ctrl = 0x1D
    static let alt = 0x38
    static let win = 0x5B
    static let esc = 0x01
    static let tab = 0x0F
    static let enter = 0x1C
    static let backspace = 0x0E
    static let space = 0x39
    static let delete = 0x53
    static let up = 0x48
    static let down = 0x50
    static let left = 0x4B
    static let right = 0x4D
    static let home = 0x47
    static let end = 0x4F
    static let pageUp = 0x49
    static let pageDown = 0x51
    static let f: [Int] = [0x3B, 0x3C, 0x3D, 0x3E, 0x3F, 0x40, 0x41, 0x42, 0x43, 0x44, 0x57, 0x58]
    static let v = 0x2F
    static let c = 0x2E
    static let z = 0x2C
    static let d = 0x20

    private static let extendedKeys: Set<Int> = [win, delete, up, down, left, right, home, end, pageUp, pageDown]

    static func extended(_ code: Int) -> Bool { extendedKeys.contains(code) }

    private static let rows: [(String, String, [Int])] = [
        ("1234567890-=", "!@#$%^&*()_+", [0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D]),
        ("qwertyuiop[]", "QWERTYUIOP{}", [0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1A, 0x1B]),
        ("asdfghjkl;'`", "ASDFGHJKL:\"~", [0x1E, 0x1F, 0x20, 0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27, 0x28, 0x29]),
        ("\\zxcvbnm,./", "|ZXCVBNM<>?", [0x2B, 0x2C, 0x2D, 0x2E, 0x2F, 0x30, 0x31, 0x32, 0x33, 0x34, 0x35]),
    ]

    /// The key and whether Shift is held for `c` on a US keyboard, or nil.
    static func forCharacter(_ c: Character) -> (Int, Bool)? {
        switch c {
        case " ": return (space, false)
        case "\n", "\r": return (enter, false)
        case "\t": return (tab, false)
        default: break
        }
        for (plain, shifted, codes) in rows {
            if let i = plain.firstIndex(of: c) { return (codes[plain.distance(from: plain.startIndex, to: i)], false) }
            if let i = shifted.firstIndex(of: c) {
                return (codes[shifted.distance(from: shifted.startIndex, to: i)], true)
            }
        }
        return nil
    }

    /// The scancode of a hardware keyboard's key (its HID usage), or nil.
    static func forHID(_ usage: Int) -> Int? {
        switch usage {
        case 0x04...0x1D:
            // a..z in HID order.
            let letters: [Int] = [0x1E, 0x30, 0x2E, 0x20, 0x12, 0x21, 0x22, 0x23, 0x17, 0x24, 0x25, 0x26, 0x32,
                                  0x31, 0x18, 0x19, 0x10, 0x13, 0x1F, 0x14, 0x16, 0x2F, 0x11, 0x2D, 0x15, 0x2C]
            return letters[usage - 0x04]
        case 0x1E...0x27: return 0x02 + (usage - 0x1E)
        case 0x28: return enter
        case 0x29: return esc
        case 0x2A: return backspace
        case 0x2B: return tab
        case 0x2C: return space
        case 0x2D: return 0x0C
        case 0x2E: return 0x0D
        case 0x2F: return 0x1A
        case 0x30: return 0x1B
        case 0x31: return 0x2B
        case 0x33: return 0x27
        case 0x34: return 0x28
        case 0x35: return 0x29
        case 0x36: return 0x33
        case 0x37: return 0x34
        case 0x38: return 0x35
        case 0x39: return 0x3A
        case 0x3A...0x45: return f[usage - 0x3A]
        case 0x4A: return home
        case 0x4B: return pageUp
        case 0x4C: return delete
        case 0x4D: return end
        case 0x4E: return pageDown
        case 0x4F: return right
        case 0x50: return left
        case 0x51: return down
        case 0x52: return up
        case 0xE0, 0xE4: return ctrl
        case 0xE1, 0xE5: return shift
        case 0xE2, 0xE6: return alt
        case 0xE3, 0xE7: return win
        default: return nil
        }
    }
}
