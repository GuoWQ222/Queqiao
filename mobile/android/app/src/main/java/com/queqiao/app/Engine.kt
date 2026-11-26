package com.queqiao.app

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.os.Handler
import android.os.Looper
import android.os.Build
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import org.json.JSONObject
import kotlin.concurrent.thread

/** One device of the account, as the list shows it. */
data class Device(
    val id: String,
    val name: String,
    val online: Boolean,
    val controllable: Boolean,
    /** Another device of the account controlling it now (UU Remote's "being controlled"), empty for none. */
    val controlledBy: String = "",
)

/** Where the app is: logging in, the device list, a session. */
sealed class Screen {
    data object Login : Screen()
    data object Devices : Screen()
    data class Session(val device: Device) : Screen()
}

/** What a session shows over its picture. */
data class SessionState(
    val stage: String = "connecting",
    val relayed: Boolean = false,
    val path: String = "",
    /** The relay's place ("Shanghai"; `relayZh` "上海"), empty when direct or unnamed. */
    val relay: String = "",
    val relayZh: String = "",
    val message: String = "",
    val passwordNeeded: Boolean = false,
    /** "busy": another device controls the PC (`controller`; empty: someone of another account helping). */
    val controller: String = "",
    /** The person may take control from `controller` (a device of the account, not asked to already). */
    val mayTakeOver: Boolean = false,
)

/** The session's figures, once a second (as the desktop's card). */
data class Figures(val fps: Double = 0.0, val mbps: Double = 0.0, val rttMs: Double? = null, val relayed: Boolean = false)

/** The PC's screens: how many and which one is shown. */
data class Screens(val count: Int = 1, val current: Int = 0)

/**
 * The app's side of the core: starts it, turns its events into state the screens observe, and sends commands. One
 * per app (the core keeps the account and the session).
 */
class Engine private constructor(context: Context) {
    val handle: Long = Core.start(context.filesDir.absolutePath, deviceName())
    private val _screen = MutableStateFlow<Screen>(Screen.Login)
    val screen: StateFlow<Screen> = _screen
    private val _devices = MutableStateFlow<List<Device>>(emptyList())
    val devices: StateFlow<List<Device>> = _devices
    private val _error = MutableStateFlow("")
    val error: StateFlow<String> = _error
    private val _busy = MutableStateFlow(true)
    val busy: StateFlow<Boolean> = _busy
    private val _session = MutableStateFlow(SessionState())
    val session: StateFlow<SessionState> = _session
    private val _clipboard = MutableStateFlow("")
    val clipboard: StateFlow<String> = _clipboard
    private val _figures = MutableStateFlow(Figures())
    val figures: StateFlow<Figures> = _figures
    private val _screens = MutableStateFlow(Screens())
    val screens: StateFlow<Screens> = _screens
    private val _helping = MutableStateFlow(false)
    /** A help request is waiting for its answer. */
    val helping: StateFlow<Boolean> = _helping
    private val _created = MutableStateFlow("")
    /** The user name of an account the login just made (one form for logging in and signing up), else empty. */
    val created: StateFlow<String> = _created
    private val app = context.applicationContext
    var ownDeviceId = ""
        private set

    init {
        thread(name = "queqiao-events", isDaemon = true) {
            while (true) {
                val event = Core.nextEvent(handle, 500) ?: continue
                try {
                    onEvent(JSONObject(event))
                } catch (_: Exception) {
                }
            }
        }
        // The saved login, if there is one.
        command(JSONObject().put("resume", JSONObject()))
    }

    fun command(json: JSONObject): Boolean = Core.command(handle, json.toString())

    fun logIn(server: String, pin: String, user: String, password: String) {
        _busy.value = true
        _error.value = ""
        command(
            JSONObject().put(
                "login",
                JSONObject().put("server", server.trim()).put("pin", pin.trim().lowercase())
                    .put("user", user.trim()).put("password", password),
            ),
        )
    }

    fun logOut() {
        command(JSONObject().put("log_out", JSONObject()))
    }

    /** A session with [device]; [takeOver]: from the device of the account controlling it (the person confirmed). */
    fun connect(device: Device, takeOver: Boolean = false) {
        _session.value = SessionState()
        _figures.value = Figures()
        _screen.value = Screen.Session(device)
        command(JSONObject().put("connect", JSONObject().put("device", device.id).put("take_over", takeOver)))
    }

    /** Ends another device's session with [device] (the being-controlled banner's Disconnect). */
    fun endControl(device: Device) = command(JSONObject().put("end_control", device.id))

    /** The account this login made, shown once (a name typed wrong makes one too), then cleared. */
    fun createdSeen() {
        _created.value = ""
    }

    fun disconnect() {
        command(JSONObject().put("disconnect", JSONObject()))
        _screen.value = Screen.Devices
    }

    fun removeDevice(device: Device) {
        command(JSONObject().put("remove_device", device.id))
    }

    // Input: the picture's 0..1 coordinates, buttons, the wheel, keys by scancode.
    fun move(x: Float, y: Float) =
        command(JSONObject().put("move", JSONObject().put("x", x.toDouble()).put("y", y.toDouble())))

    fun button(name: String, down: Boolean) =
        command(JSONObject().put("button", JSONObject().put("button", name).put("down", down)))

    fun wheel(delta: Int, horizontal: Boolean = false) =
        command(JSONObject().put("wheel", JSONObject().put("delta", delta).put("horizontal", horizontal)))

    fun key(scancode: Int, extended: Boolean, down: Boolean) =
        command(
            JSONObject().put(
                "key",
                JSONObject().put("scancode", scancode).put("extended", extended).put("down", down),
            ),
        )

    fun remote(command: String) = command(JSONObject().put("command", command))

    fun password(text: String) = command(JSONObject().put("password", text))

    fun sendClipboard(text: String) = command(JSONObject().put("clipboard", text))

    /** Text typed on the PC as characters (any language). */
    fun typeText(text: String) = command(JSONObject().put("text", text))

    /** Helping someone: their help ID and code (empty: the person there is asked). */
    fun help(id: String, code: String) {
        _helping.value = true
        _error.value = ""
        command(JSONObject().put("help", JSONObject().put("id", id.trim()).put("code", code.trim())))
    }

    fun wake(device: Device) = command(JSONObject().put("wake", device.id))

    fun picture(fps: Int, quality: String) =
        command(JSONObject().put("picture", JSONObject().put("fps", fps).put("quality", quality)))

    fun showScreen(n: Int) = command(JSONObject().put("screen", n))

    fun muteSpeakers(mute: Boolean) = command(JSONObject().put("mute_speakers", mute))

    /** The phone's clipboard text to the PC's clipboard. */
    fun sendPhoneClipboard(): Boolean {
        val manager = app.getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
        val text = manager.primaryClip?.takeIf { it.itemCount > 0 }?.getItemAt(0)?.coerceToText(app)?.toString()
        if (text.isNullOrEmpty()) return false
        sendClipboard(text)
        return true
    }

    private fun onEvent(event: JSONObject) {
        when {
            event.has("logged_in") -> {
                val login = event.getJSONObject("logged_in")
                ownDeviceId = login.optString("device")
                if (login.optBoolean("created")) _created.value = login.optString("user")
                _busy.value = false
                _error.value = ""
                if (_screen.value is Screen.Login) _screen.value = Screen.Devices
            }
            event.has("logged_out") -> {
                _busy.value = false
                _screen.value = Screen.Login
            }
            event.has("devices") -> {
                val list = event.getJSONArray("devices")
                _devices.value = (0 until list.length()).map {
                    val d = list.getJSONObject(it)
                    Device(
                        d.getString("id"), d.getString("name"), d.optBoolean("online"), d.optBoolean("controllable"),
                        d.optString("controlled_by"),
                    )
                }
            }
            event.has("error") -> {
                val error = event.getJSONObject("error")
                _busy.value = false
                _helping.value = false
                _error.value = error.optString("message")
                if (error.optString("what") == "server" && _screen.value is Screen.Devices) {
                    _error.value = error.optString("message")
                }
            }
            event.has("session") -> {
                val s = event.getJSONObject("session")
                val state = _session.value
                _session.value = when (s.optString("state")) {
                    "path" -> state.copy(
                        relayed = s.optBoolean("relayed"),
                        path = s.optString("path"),
                        relay = s.optString("relay"),
                        relayZh = s.optString("relay_zh"),
                    )
                    "failed" -> state.copy(stage = "failed", message = s.optString("message"))
                    // Another device has the PC: the session screen asks whether to take control.
                    "busy" -> state.copy(
                        stage = "busy",
                        controller = s.optString("controller"),
                        mayTakeOver = s.optBoolean("may_take_over"),
                    )
                    "ended" -> {
                        if (_screen.value is Screen.Session) _screen.value = Screen.Devices
                        state.copy(stage = "ended", message = s.optString("why"))
                    }
                    else -> state.copy(stage = s.optString("state"))
                }
            }
            event.has("password_needed") -> _session.value = _session.value.copy(passwordNeeded = true)
            event.has("password_result") -> {
                if (event.getJSONObject("password_result").optBoolean("ok")) {
                    _session.value = _session.value.copy(passwordNeeded = false)
                }
            }
            event.has("clipboard") -> {
                // The PC's clipboard on the phone (as UU Remote shares it).
                val text = event.optString("clipboard")
                _clipboard.value = text
                Handler(Looper.getMainLooper()).post {
                    val manager = app.getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
                    manager.setPrimaryClip(ClipData.newPlainText("Queqiao", text))
                }
            }
            event.has("stats") -> {
                val s = event.getJSONObject("stats")
                _figures.value = Figures(
                    fps = s.optDouble("fps", 0.0),
                    mbps = s.optDouble("mbps", 0.0),
                    rttMs = if (s.isNull("rtt_ms")) null else s.optDouble("rtt_ms"),
                    relayed = s.optBoolean("relayed"),
                )
            }
            event.has("screens") -> {
                val s = event.getJSONObject("screens")
                _screens.value = Screens(s.optJSONArray("sizes")?.length() ?: 1, s.optInt("current"))
            }
            event.has("help_granted") -> {
                val g = event.getJSONObject("help_granted")
                _helping.value = false
                _session.value = SessionState()
                _figures.value = Figures()
                _screen.value = Screen.Session(Device(g.optString("id"), g.optString("name"), true, true))
            }
        }
    }

    companion object {
        @Volatile private var instance: Engine? = null

        fun get(context: Context): Engine =
            instance ?: synchronized(this) {
                instance ?: Engine(context.applicationContext).also { instance = it }
            }

        private fun deviceName(): String {
            val model = Build.MODEL ?: "Android"
            val maker = Build.MANUFACTURER ?: ""
            return if (model.startsWith(maker, ignoreCase = true)) model else "$maker $model".trim()
        }
    }
}
