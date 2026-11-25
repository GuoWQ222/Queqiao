package com.queqiao.app

import android.content.Context
import android.os.Build
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import org.json.JSONObject
import kotlin.concurrent.thread

/** One device of the account, as the list shows it. */
data class Device(val id: String, val name: String, val online: Boolean, val controllable: Boolean)

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
    val message: String = "",
    val passwordNeeded: Boolean = false,
)

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

    fun connect(device: Device) {
        _session.value = SessionState()
        _screen.value = Screen.Session(device)
        command(JSONObject().put("connect", JSONObject().put("device", device.id)))
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

    private fun onEvent(event: JSONObject) {
        when {
            event.has("logged_in") -> {
                ownDeviceId = event.getJSONObject("logged_in").optString("device")
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
                    Device(d.getString("id"), d.getString("name"), d.optBoolean("online"), d.optBoolean("controllable"))
                }
            }
            event.has("error") -> {
                val error = event.getJSONObject("error")
                _busy.value = false
                _error.value = error.optString("message")
                if (error.optString("what") == "server" && _screen.value is Screen.Devices) {
                    _error.value = error.optString("message")
                }
            }
            event.has("session") -> {
                val s = event.getJSONObject("session")
                val state = _session.value
                _session.value = when (s.optString("state")) {
                    "path" -> state.copy(relayed = s.optBoolean("relayed"), path = s.optString("path"))
                    "failed" -> state.copy(stage = "failed", message = s.optString("message"))
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
            event.has("clipboard") -> _clipboard.value = event.optString("clipboard")
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
