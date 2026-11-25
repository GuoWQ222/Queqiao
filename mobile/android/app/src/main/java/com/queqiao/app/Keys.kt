package com.queqiao.app

import android.view.KeyEvent

/**
 * Keys by their set 1 scancodes (as the protocol sends them: the PC's keyboard layout decides the character). The
 * phone's keyboard gives characters; these are the US layout keys that make them.
 */
object Keys {
    const val SHIFT = 0x2A
    const val CTRL = 0x1D
    const val ALT = 0x38
    const val WIN = 0x5B // extended
    const val ESC = 0x01
    const val TAB = 0x0F
    const val ENTER = 0x1C
    const val BACKSPACE = 0x0E
    const val SPACE = 0x39
    const val DELETE = 0x53 // extended
    const val UP = 0x48 // extended
    const val DOWN = 0x50 // extended
    const val LEFT = 0x4B // extended
    const val RIGHT = 0x4D // extended
    const val HOME = 0x47 // extended
    const val END = 0x4F // extended
    const val PAGE_UP = 0x49 // extended
    const val PAGE_DOWN = 0x51 // extended

    /** F1..F12. */
    val F = intArrayOf(0x3B, 0x3C, 0x3D, 0x3E, 0x3F, 0x40, 0x41, 0x42, 0x43, 0x44, 0x57, 0x58)

    /** Which keys are extended (E0 prefix). */
    fun extended(scancode: Int): Boolean =
        scancode in setOf(WIN, DELETE, UP, DOWN, LEFT, RIGHT, HOME, END, PAGE_UP, PAGE_DOWN)

    private const val ROW1 = "1234567890-="
    private const val ROW1_SHIFTED = "!@#$%^&*()_+"
    private const val ROW2 = "qwertyuiop[]"
    private const val ROW2_SHIFTED = "QWERTYUIOP{}"
    private const val ROW3 = "asdfghjkl;'`"
    private const val ROW3_SHIFTED = "ASDFGHJKL:\"~"
    private const val ROW4 = "\\zxcvbnm,./"
    private const val ROW4_SHIFTED = "|ZXCVBNM<>?"

    // The scancodes of each row's keys, in the order of the strings above.
    private val ROW1_CODES = intArrayOf(0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D)
    private val ROW2_CODES = intArrayOf(0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1A, 0x1B)
    private val ROW3_CODES = intArrayOf(0x1E, 0x1F, 0x20, 0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27, 0x28, 0x29)
    private val ROW4_CODES = intArrayOf(0x2B, 0x2C, 0x2D, 0x2E, 0x2F, 0x30, 0x31, 0x32, 0x33, 0x34, 0x35)

    /** The key and whether Shift is held for [c], or null if no key of the US layout types it. */
    fun forChar(c: Char): Pair<Int, Boolean>? {
        when (c) {
            ' ' -> return SPACE to false
            '\n' -> return ENTER to false
            '\t' -> return TAB to false
        }
        for ((plain, shifted, codes) in listOf(
            Triple(ROW1, ROW1_SHIFTED, ROW1_CODES),
            Triple(ROW2, ROW2_SHIFTED, ROW2_CODES),
            Triple(ROW3, ROW3_SHIFTED, ROW3_CODES),
            Triple(ROW4, ROW4_SHIFTED, ROW4_CODES),
        )) {
            val i = plain.indexOf(c)
            if (i >= 0) return codes[i] to false
            val j = shifted.indexOf(c)
            if (j >= 0) return codes[j] to true
        }
        return null
    }

    /** The scancode of an Android key code (a hardware keyboard, or the soft keyboard's own keys), or null. */
    fun forKeyCode(code: Int): Int? = when (code) {
        KeyEvent.KEYCODE_DEL -> BACKSPACE
        KeyEvent.KEYCODE_FORWARD_DEL -> DELETE
        KeyEvent.KEYCODE_ENTER, KeyEvent.KEYCODE_NUMPAD_ENTER -> ENTER
        KeyEvent.KEYCODE_TAB -> TAB
        KeyEvent.KEYCODE_ESCAPE -> ESC
        KeyEvent.KEYCODE_SPACE -> SPACE
        KeyEvent.KEYCODE_DPAD_UP -> UP
        KeyEvent.KEYCODE_DPAD_DOWN -> DOWN
        KeyEvent.KEYCODE_DPAD_LEFT -> LEFT
        KeyEvent.KEYCODE_DPAD_RIGHT -> RIGHT
        KeyEvent.KEYCODE_MOVE_HOME -> HOME
        KeyEvent.KEYCODE_MOVE_END -> END
        KeyEvent.KEYCODE_PAGE_UP -> PAGE_UP
        KeyEvent.KEYCODE_PAGE_DOWN -> PAGE_DOWN
        KeyEvent.KEYCODE_SHIFT_LEFT, KeyEvent.KEYCODE_SHIFT_RIGHT -> SHIFT
        KeyEvent.KEYCODE_CTRL_LEFT, KeyEvent.KEYCODE_CTRL_RIGHT -> CTRL
        KeyEvent.KEYCODE_ALT_LEFT, KeyEvent.KEYCODE_ALT_RIGHT -> ALT
        KeyEvent.KEYCODE_META_LEFT, KeyEvent.KEYCODE_META_RIGHT -> WIN
        in KeyEvent.KEYCODE_F1..KeyEvent.KEYCODE_F12 -> F[code - KeyEvent.KEYCODE_F1]
        else -> null
    }
}
