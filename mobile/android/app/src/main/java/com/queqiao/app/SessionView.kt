package com.queqiao.app

import android.content.Context
import android.graphics.Canvas
import android.graphics.Color
import android.graphics.Paint
import android.graphics.Path
import android.os.SystemClock
import android.text.InputType
import android.view.KeyEvent
import android.view.MotionEvent
import android.view.SurfaceHolder
import android.view.SurfaceView
import android.view.View
import android.view.inputmethod.BaseInputConnection
import android.view.inputmethod.EditorInfo
import android.view.inputmethod.InputConnection
import android.view.inputmethod.InputMethodManager
import android.widget.FrameLayout
import kotlin.math.abs
import kotlin.math.hypot
import kotlin.math.max
import kotlin.math.min

/**
 * The remote screen: the decoded picture fitted into the view (zoomed and moved with two fingers), and the touches
 * and typing turned into the PC's mouse and keys. Two ways to use the mouse, as UU Remote has: touch (a finger is the
 * pointer: tap to click where it lands) and mouse (the screen is a touchpad moving a pointer drawn here).
 */
class SessionView(context: Context, private val engine: Engine) : FrameLayout(context) {
    enum class Mode { Touch, Mouse }

    var mode = Mode.Touch
        set(value) {
            field = value
            overlay.invalidate()
        }

    /** The picture's size (from the decoder), 0 until the first frame. */
    @Volatile var pictureWidth = 0
        private set
    @Volatile var pictureHeight = 0
        private set

    var onFramesShown: (Long) -> Unit = {}
    private val surface = SurfaceView(context)
    private val overlay = Overlay(context)
    private var decoder: VideoDecoder? = null

    // Zoom (1 = fitted) and the picture's offset in view pixels.
    private var zoom = 1f
    private var panX = 0f
    private var panY = 0f

    // The pointer in mouse mode, 0..1 on the remote screen.
    private var pointerX = 0.5f
    private var pointerY = 0.5f

    init {
        addView(surface, LayoutParams(LayoutParams.MATCH_PARENT, LayoutParams.MATCH_PARENT))
        addView(overlay, LayoutParams(LayoutParams.MATCH_PARENT, LayoutParams.MATCH_PARENT))
        isFocusable = true
        isFocusableInTouchMode = true
        surface.holder.addCallback(object : SurfaceHolder.Callback {
            override fun surfaceCreated(holder: SurfaceHolder) {
                decoder = VideoDecoder(engine.handle, holder.surface) { w, h ->
                    pictureWidth = w
                    pictureHeight = h
                    post { layoutPicture() }
                }
            }

            override fun surfaceChanged(holder: SurfaceHolder, format: Int, width: Int, height: Int) {}

            override fun surfaceDestroyed(holder: SurfaceHolder) {
                decoder?.stop()
                decoder = null
            }
        })
    }

    fun framesShown(): Long = decoder?.framesShown ?: 0

    override fun onSizeChanged(w: Int, h: Int, oldw: Int, oldh: Int) {
        super.onSizeChanged(w, h, oldw, oldh)
        layoutPicture()
    }

    /** The fitted picture's rectangle at zoom 1: left, top, width, height in view pixels. */
    private fun fitted(): FloatArray {
        val pw = if (pictureWidth > 0) pictureWidth.toFloat() else 16f
        val ph = if (pictureHeight > 0) pictureHeight.toFloat() else 9f
        val scale = min(width / pw, height / ph)
        val w = pw * scale
        val h = ph * scale
        return floatArrayOf((width - w) / 2, (height - h) / 2, w, h)
    }

    /** Where the picture is now (zoomed and moved). */
    private fun shown(): FloatArray {
        val f = fitted()
        val w = f[2] * zoom
        val h = f[3] * zoom
        return floatArrayOf(f[0] - (w - f[2]) / 2 + panX, f[1] - (h - f[3]) / 2 + panY, w, h)
    }

    private fun clampPan() {
        val f = fitted()
        val w = f[2] * zoom
        val h = f[3] * zoom
        val limitX = max(0f, (w - width) / 2)
        val limitY = max(0f, (h - height) / 2)
        panX = panX.coerceIn(-limitX, limitX)
        panY = panY.coerceIn(-limitY, limitY)
        if (w <= width) panX = 0f
        if (h <= height) panY = 0f
    }

    private fun layoutPicture() {
        if (width == 0 || height == 0) return
        clampPan()
        val s = shown()
        val params = surface.layoutParams as LayoutParams
        params.width = s[2].toInt()
        params.height = s[3].toInt()
        params.leftMargin = s[0].toInt()
        params.topMargin = s[1].toInt()
        surface.layoutParams = params
        overlay.invalidate()
    }

    /** A view point on the remote screen, 0..1. */
    private fun toRemote(x: Float, y: Float): Pair<Float, Float> {
        val s = shown()
        return ((x - s[0]) / s[2]).coerceIn(0f, 1f) to ((y - s[1]) / s[3]).coerceIn(0f, 1f)
    }

    // The gesture being made.
    private var downTime = 0L
    private var downX = 0f
    private var downY = 0f
    private var lastX = 0f
    private var lastY = 0f
    private var fingers = 0
    private var maxFingers = 0
    private var moved = false
    private var dragging = false
    private var leftDown = false
    private var pinchStart = 0f
    private var zoomStart = 1f
    private var twoStartX = 0f
    private var twoStartY = 0f
    private var panStartX = 0f
    private var panStartY = 0f
    private var wheelRest = 0f
    private var lastTapTime = 0L
    private val longPress = Runnable { onLongPress() }

    override fun onTouchEvent(event: MotionEvent): Boolean {
        val x = event.x
        val y = event.y
        when (event.actionMasked) {
            MotionEvent.ACTION_DOWN -> {
                requestFocus()
                downTime = SystemClock.uptimeMillis()
                downX = x
                downY = y
                lastX = x
                lastY = y
                fingers = 1
                maxFingers = 1
                moved = false
                dragging = false
                // Mouse mode: a second tap held right after a tap drags (the touchpad's double tap and drag).
                if (mode == Mode.Mouse && downTime - lastTapTime < 300) {
                    dragging = true
                    press(true)
                } else {
                    postDelayed(longPress, 500)
                }
                if (mode == Mode.Touch) {
                    val (rx, ry) = toRemote(x, y)
                    engine.move(rx, ry)
                }
            }
            MotionEvent.ACTION_POINTER_DOWN -> {
                removeCallbacks(longPress)
                fingers = event.pointerCount
                maxFingers = max(maxFingers, fingers)
                if (fingers == 2) {
                    if (leftDown && !dragging) press(false)
                    pinchStart = spread(event)
                    zoomStart = zoom
                    twoStartX = (event.getX(0) + event.getX(1)) / 2
                    twoStartY = (event.getY(0) + event.getY(1)) / 2
                    panStartX = panX
                    panStartY = panY
                    lastY = twoStartY
                    lastX = twoStartX
                    wheelRest = 0f
                }
            }
            MotionEvent.ACTION_MOVE -> {
                if (fingers >= 2 && event.pointerCount >= 2) {
                    twoFingers(event)
                    return true
                }
                if (fingers != 1) return true
                if (!moved && hypot(x - downX, y - downY) > touchSlop) {
                    moved = true
                    removeCallbacks(longPress)
                    if (mode == Mode.Touch && !leftDown) press(true)
                }
                if (moved) {
                    if (mode == Mode.Touch) {
                        val (rx, ry) = toRemote(x, y)
                        engine.move(rx, ry)
                    } else {
                        val s = shown()
                        // Touchpad: the pointer moves by the finger's movement, faster for fast moves.
                        val dx = (x - lastX) / s[2]
                        val dy = (y - lastY) / s[3]
                        val speed = hypot(x - lastX, y - lastY)
                        val gain = 1.2f + min(2f, speed / 25f)
                        pointerX = (pointerX + dx * gain).coerceIn(0f, 1f)
                        pointerY = (pointerY + dy * gain).coerceIn(0f, 1f)
                        engine.move(pointerX, pointerY)
                        followPointer()
                        overlay.invalidate()
                    }
                }
                lastX = x
                lastY = y
            }
            MotionEvent.ACTION_POINTER_UP -> {
                fingers = event.pointerCount - 1
            }
            MotionEvent.ACTION_UP, MotionEvent.ACTION_CANCEL -> {
                removeCallbacks(longPress)
                val quick = SystemClock.uptimeMillis() - downTime < 250
                if (maxFingers == 2 && quick && !moved) {
                    // Two-finger tap: right click.
                    click("right")
                } else if (maxFingers == 1) {
                    if (leftDown) {
                        press(false)
                    } else if (!moved) {
                        if (mode == Mode.Touch) {
                            val (rx, ry) = toRemote(x, y)
                            engine.move(rx, ry)
                        }
                        click("left")
                        lastTapTime = SystemClock.uptimeMillis()
                    }
                }
                if (leftDown) press(false)
                fingers = 0
                dragging = false
            }
        }
        return true
    }

    private val touchSlop = 12f * resources.displayMetrics.density

    private fun spread(event: MotionEvent): Float =
        hypot(event.getX(0) - event.getX(1), event.getY(0) - event.getY(1))

    /** Two fingers: pinch zooms, moving together scrolls (or moves the zoomed picture). */
    private fun twoFingers(event: MotionEvent) {
        val cx = (event.getX(0) + event.getX(1)) / 2
        val cy = (event.getY(0) + event.getY(1)) / 2
        val ratio = spread(event) / max(1f, pinchStart)
        if (abs(ratio - 1f) > 0.08f || zoom != zoomStart) {
            moved = true
            zoom = (zoomStart * ratio).coerceIn(1f, 5f)
            panX = panStartX + (cx - twoStartX)
            panY = panStartY + (cy - twoStartY)
            layoutPicture()
        } else if (zoom > 1f) {
            moved = true
            panX = panStartX + (cx - twoStartX)
            panY = panStartY + (cy - twoStartY)
            layoutPicture()
        } else {
            // Scroll: about one notch per 40 dp, in the direction of the fingers (as a phone's own lists scroll).
            val dy = cy - lastY
            if (abs(cy - twoStartY) > touchSlop) moved = true
            if (moved) {
                wheelRest += dy / (40f * resources.displayMetrics.density) * 120f
                val notches = wheelRest.toInt()
                if (notches != 0) {
                    engine.wheel(notches)
                    wheelRest -= notches
                }
            }
        }
        lastX = cx
        lastY = cy
    }

    /** Keeps the pointer in view while zoomed (mouse mode). */
    private fun followPointer() {
        if (zoom <= 1f) return
        val s = shown()
        val px = s[0] + pointerX * s[2]
        val py = s[1] + pointerY * s[3]
        val margin = 40f * resources.displayMetrics.density
        var changed = false
        if (px < margin) { panX += margin - px; changed = true }
        if (px > width - margin) { panX -= px - (width - margin); changed = true }
        if (py < margin) { panY += margin - py; changed = true }
        if (py > height - margin) { panY -= py - (height - margin); changed = true }
        if (changed) layoutPicture()
    }

    private fun onLongPress() {
        if (moved || fingers != 1) return
        performHapticFeedback(HAPTIC_FEEDBACK_ENABLED)
        if (mode == Mode.Touch) {
            // Hold: right click where the finger is.
            click("right")
            moved = true
        } else {
            // Hold: the left button down, drag with the finger.
            press(true)
            dragging = true
            moved = true
        }
    }

    private fun press(down: Boolean) {
        leftDown = down
        engine.button("left", down)
    }

    private fun click(button: String) {
        engine.button(button, true)
        engine.button(button, false)
    }

    // The keyboard: the phone's keyboard types into the PC.

    fun showKeyboard(show: Boolean) {
        val manager = context.getSystemService(Context.INPUT_METHOD_SERVICE) as InputMethodManager
        if (show) {
            requestFocus()
            manager.showSoftInput(this, InputMethodManager.SHOW_IMPLICIT)
        } else {
            manager.hideSoftInputFromWindow(windowToken, 0)
        }
    }

    override fun onCheckIsTextEditor(): Boolean = true

    override fun onCreateInputConnection(outAttrs: EditorInfo): InputConnection {
        outAttrs.inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_FLAG_NO_SUGGESTIONS or
            InputType.TYPE_TEXT_VARIATION_VISIBLE_PASSWORD
        outAttrs.imeOptions = EditorInfo.IME_FLAG_NO_FULLSCREEN or EditorInfo.IME_FLAG_NO_EXTRACT_UI or
            EditorInfo.IME_ACTION_NONE
        return object : BaseInputConnection(this, false) {
            override fun commitText(text: CharSequence, newCursorPosition: Int): Boolean {
                type(text.toString())
                return true
            }

            override fun deleteSurroundingText(beforeLength: Int, afterLength: Int): Boolean {
                repeat(max(1, beforeLength)) { tap(Keys.BACKSPACE) }
                repeat(afterLength) { tap(Keys.DELETE) }
                return true
            }

            override fun sendKeyEvent(event: KeyEvent): Boolean {
                handleKey(event)
                return true
            }

            override fun performEditorAction(actionCode: Int): Boolean {
                tap(Keys.ENTER)
                return true
            }
        }
    }

    override fun onKeyDown(keyCode: Int, event: KeyEvent): Boolean {
        if (keyCode == KeyEvent.KEYCODE_BACK) return super.onKeyDown(keyCode, event)
        return handleKey(event) || super.onKeyDown(keyCode, event)
    }

    override fun onKeyUp(keyCode: Int, event: KeyEvent): Boolean {
        if (keyCode == KeyEvent.KEYCODE_BACK) return super.onKeyUp(keyCode, event)
        return handleKey(event) || super.onKeyUp(keyCode, event)
    }

    /** A key from a hardware keyboard or the soft keyboard's own keys. */
    private fun handleKey(event: KeyEvent): Boolean {
        val down = event.action == KeyEvent.ACTION_DOWN
        val code = Keys.forKeyCode(event.keyCode)
        if (code != null) {
            engine.key(code, Keys.extended(code), down)
            return true
        }
        if (event.action == KeyEvent.ACTION_DOWN) {
            val c = event.unicodeChar
            if (c != 0) {
                type(String(Character.toChars(c)))
                return true
            }
        }
        return event.action == KeyEvent.ACTION_UP && event.unicodeChar != 0
    }

    /** Types [text] on the PC: keys of the US layout where there are any, the PC's clipboard and Ctrl+V otherwise. */
    fun type(text: String) {
        val other = StringBuilder()
        for (c in text) {
            val key = Keys.forChar(c)
            if (key == null) {
                other.append(c)
                continue
            }
            flush(other)
            val (code, shift) = key
            if (shift) engine.key(Keys.SHIFT, false, true)
            tap(code)
            if (shift) engine.key(Keys.SHIFT, false, false)
        }
        flush(other)
    }

    private fun flush(other: StringBuilder) {
        if (other.isEmpty()) return
        // Chinese and other characters no key types: through the PC's clipboard (as UU Remote's phone keyboard).
        engine.sendClipboard(other.toString())
        engine.key(Keys.CTRL, false, true)
        tap(0x2F)
        engine.key(Keys.CTRL, false, false)
        other.clear()
    }

    fun tap(code: Int) {
        engine.key(code, Keys.extended(code), true)
        engine.key(code, Keys.extended(code), false)
    }

    /** Key combinations: all keys down in order, then up in reverse. */
    fun combo(vararg codes: Int) {
        for (c in codes) engine.key(c, Keys.extended(c), true)
        for (c in codes.reversed()) engine.key(c, Keys.extended(c), false)
    }

    /** The pointer drawn in mouse mode (the PC's own pointer is not in the picture). */
    private inner class Overlay(context: Context) : View(context) {
        private val fill = Paint(Paint.ANTI_ALIAS_FLAG).apply { color = Color.WHITE }
        private val edge = Paint(Paint.ANTI_ALIAS_FLAG).apply {
            color = Color.BLACK
            style = Paint.Style.STROKE
            strokeWidth = 1.5f * resources.displayMetrics.density
        }
        private val arrow = Path()

        init {
            setWillNotDraw(false)
        }

        override fun onDraw(canvas: Canvas) {
            if (mode != Mode.Mouse) return
            val s = shown()
            val px = s[0] + pointerX * s[2]
            val py = s[1] + pointerY * s[3]
            val d = resources.displayMetrics.density
            arrow.reset()
            arrow.moveTo(px, py)
            arrow.lineTo(px, py + 18 * d)
            arrow.lineTo(px + 4.5f * d, py + 14 * d)
            arrow.lineTo(px + 8 * d, py + 21 * d)
            arrow.lineTo(px + 10.5f * d, py + 20 * d)
            arrow.lineTo(px + 7.2f * d, py + 13 * d)
            arrow.lineTo(px + 13 * d, py + 13 * d)
            arrow.close()
            canvas.drawPath(arrow, fill)
            canvas.drawPath(arrow, edge)
        }
    }
}
