package com.queqiao.app

import android.os.Bundle
import android.view.WindowManager
import androidx.activity.ComponentActivity
import androidx.activity.compose.BackHandler
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        val engine = Engine.get(this)
        setContent {
            QueqiaoTheme {
                val screen by engine.screen.collectAsState()
                when (val s = screen) {
                    Screen.Login -> LoginScreen(engine)
                    Screen.Devices -> DevicesScreen(engine)
                    is Screen.Session -> {
                        // The phone stays awake while it shows the PC.
                        window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
                        SessionScreen(engine, s.device) {
                            window.clearFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
                        }
                    }
                }
            }
        }
    }
}

/** The night-blue band with the magpie and the name, as the desktop sidebar's top. */
@Composable
fun Header(subtitle: String, trailing: @Composable () -> Unit = {}) {
    Box(
        Modifier
            .fillMaxWidth()
            .background(Brush.verticalGradient(listOf(Q.HeroTop, Q.Night)))
            .statusBarsPadding()
            .padding(horizontal = 20.dp, vertical = 16.dp),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Image(painterResource(R.drawable.magpie), null, Modifier.size(36.dp))
            Spacer(Modifier.width(10.dp))
            Column(Modifier.weight(1f)) {
                Text(stringResource(R.string.app_name), color = Q.NightText, fontSize = 20.sp, fontWeight = FontWeight.Bold)
                Text(subtitle, color = Q.NightMuted, fontSize = 13.sp)
            }
            trailing()
        }
    }
}

@Composable
fun GoldButton(text: String, modifier: Modifier = Modifier, enabled: Boolean = true, onClick: () -> Unit) {
    Button(
        onClick = onClick,
        enabled = enabled,
        modifier = modifier.height(46.dp),
        shape = RoundedCornerShape(8.dp),
        colors = ButtonDefaults.buttonColors(
            containerColor = Q.Gold,
            contentColor = Q.OnGold,
            disabledContainerColor = Q.Disabled,
        ),
    ) {
        Text(text, fontWeight = FontWeight.Bold, fontSize = 16.sp)
    }
}

@Composable
fun LoginScreen(engine: Engine) {
    val busy by engine.busy.collectAsState()
    val error by engine.error.collectAsState()
    var server by rememberSaveable { mutableStateOf(BuildConfig.DEFAULT_SERVER) }
    var pin by rememberSaveable { mutableStateOf(BuildConfig.DEFAULT_PIN) }
    var user by rememberSaveable { mutableStateOf("") }
    var password by rememberSaveable { mutableStateOf("") }
    var showServer by rememberSaveable { mutableStateOf(BuildConfig.DEFAULT_SERVER.isEmpty()) }
    Column(Modifier.fillMaxSize().background(androidx.compose.ui.graphics.Color.White).imePadding()) {
        Header(stringResource(R.string.login_subtitle))
        Column(
            Modifier
                .fillMaxWidth()
                .verticalScroll(rememberScrollState())
                .padding(24.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            Text(stringResource(R.string.login_title), color = Q.Text, fontSize = 22.sp, fontWeight = FontWeight.Bold)
            Text(stringResource(R.string.login_hint), color = Q.Secondary, fontSize = 14.sp)
            OutlinedTextField(
                user, { user = it }, Modifier.fillMaxWidth().testTag("user"),
                label = { Text(stringResource(R.string.user)) }, singleLine = true,
            )
            OutlinedTextField(
                password, { password = it }, Modifier.fillMaxWidth().testTag("password"),
                label = { Text(stringResource(R.string.password)) }, singleLine = true,
                visualTransformation = PasswordVisualTransformation(),
                keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Password),
            )
            if (showServer) {
                OutlinedTextField(
                    server, { server = it }, Modifier.fillMaxWidth().testTag("server"),
                    label = { Text(stringResource(R.string.server)) }, singleLine = true,
                    keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Uri),
                )
                OutlinedTextField(
                    pin, { pin = it }, Modifier.fillMaxWidth().testTag("pin"),
                    label = { Text(stringResource(R.string.pin)) }, singleLine = true,
                )
            } else {
                Text(
                    stringResource(R.string.other_server), color = Q.Accent, fontSize = 14.sp,
                    modifier = Modifier.clickable { showServer = true },
                )
            }
            if (error.isNotEmpty()) Text(error, color = Q.Danger, fontSize = 14.sp, modifier = Modifier.testTag("error"))
            GoldButton(
                stringResource(R.string.log_in), Modifier.fillMaxWidth().testTag("login"),
                enabled = !busy && user.isNotBlank() && password.isNotEmpty() && server.isNotBlank(),
            ) { engine.logIn(server, pin, user, password) }
            if (busy) {
                Box(Modifier.fillMaxWidth(), contentAlignment = Alignment.Center) {
                    CircularProgressIndicator(color = Q.Accent, modifier = Modifier.size(28.dp))
                }
            }
        }
    }
}

@Composable
fun DevicesScreen(engine: Engine) {
    val devices by engine.devices.collectAsState()
    val error by engine.error.collectAsState()
    var menu by remember { mutableStateOf(false) }
    var removing by remember { mutableStateOf<Device?>(null) }
    Column(Modifier.fillMaxSize().background(androidx.compose.ui.graphics.Color.White)) {
        Header(stringResource(R.string.my_devices)) {
            Box {
                Text(
                    "⋮", color = Q.NightText, fontSize = 24.sp,
                    modifier = Modifier.clickable { menu = true }.padding(8.dp).testTag("menu"),
                )
                DropdownMenu(menu, { menu = false }) {
                    DropdownMenuItem({ Text(stringResource(R.string.log_out)) }, {
                        menu = false
                        engine.logOut()
                    })
                }
            }
        }
        if (error.isNotEmpty()) {
            Text(error, color = Q.Danger, fontSize = 13.sp, modifier = Modifier.padding(horizontal = 20.dp, vertical = 8.dp))
        }
        val others = devices.filter { it.id != engine.ownDeviceId }
        if (others.isEmpty()) {
            Box(Modifier.fillMaxSize().padding(32.dp), contentAlignment = Alignment.Center) {
                Text(stringResource(R.string.no_devices), color = Q.Secondary, fontSize = 15.sp)
            }
        } else {
            LazyColumn(
                Modifier.fillMaxSize().navigationBarsPadding(),
                contentPadding = androidx.compose.foundation.layout.PaddingValues(16.dp),
                verticalArrangement = Arrangement.spacedBy(12.dp),
            ) {
                items(others, key = { it.id }) { device ->
                    DeviceCard(device, onConnect = { engine.connect(device) }, onRemove = { removing = device })
                }
            }
        }
    }
    removing?.let { device ->
        AlertDialog(
            onDismissRequest = { removing = null },
            title = { Text(stringResource(R.string.remove_title, device.name)) },
            text = { Text(stringResource(R.string.remove_text)) },
            confirmButton = {
                TextButton({
                    engine.removeDevice(device)
                    removing = null
                }) { Text(stringResource(R.string.remove), color = Q.Danger) }
            },
            dismissButton = { TextButton({ removing = null }) { Text(stringResource(R.string.cancel)) } },
        )
    }
}

@Composable
fun DeviceCard(device: Device, onConnect: () -> Unit, onRemove: () -> Unit) {
    var more by remember { mutableStateOf(false) }
    Column(
        Modifier
            .fillMaxWidth()
            .clip(RoundedCornerShape(12.dp))
            .background(Brush.verticalGradient(listOf(Q.HeroTop, Q.HeroBottom)))
            .padding(16.dp)
            .testTag("device:${device.name}"),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Box(
                Modifier.size(10.dp).clip(CircleShape)
                    .background(if (device.online) Q.Online else Q.Offline),
            )
            Spacer(Modifier.width(8.dp))
            Text(
                device.name, color = androidx.compose.ui.graphics.Color.White, fontSize = 18.sp,
                fontWeight = FontWeight.Bold, maxLines = 1, overflow = TextOverflow.Ellipsis, modifier = Modifier.weight(1f),
            )
            Box {
                Text(
                    "⋯", color = Q.HeroMuted, fontSize = 22.sp,
                    modifier = Modifier.clickable { more = true }.padding(horizontal = 8.dp),
                )
                DropdownMenu(more, { more = false }) {
                    DropdownMenuItem({ Text(stringResource(R.string.remove_from_account)) }, {
                        more = false
                        onRemove()
                    })
                }
            }
        }
        Spacer(Modifier.height(4.dp))
        Text(
            stringResource(if (device.online) R.string.online else R.string.offline),
            color = Q.HeroMuted, fontSize = 13.sp,
        )
        Spacer(Modifier.height(14.dp))
        GoldButton(
            stringResource(R.string.connect), Modifier.fillMaxWidth().testTag("connect:${device.name}"),
            enabled = device.online && device.controllable, onClick = onConnect,
        )
    }
}

@Composable
fun SessionScreen(engine: Engine, device: Device, onLeave: () -> Unit) {
    val state by engine.session.collectAsState()
    var view by remember { mutableStateOf<SessionView?>(null) }
    var toolbar by remember { mutableStateOf(false) }
    var keyboard by remember { mutableStateOf(false) }
    var mode by rememberSaveable { mutableStateOf(SessionView.Mode.Touch) }
    var muted by rememberSaveable { mutableStateOf(false) }
    var password by remember { mutableStateOf("") }
    val audio = remember { AudioPlayer(engine.handle) }
    androidx.compose.runtime.DisposableEffect(Unit) {
        onDispose {
            audio.stop()
            onLeave()
        }
    }
    BackHandler { toolbar = !toolbar }
    Box(Modifier.fillMaxSize().background(androidx.compose.ui.graphics.Color.Black)) {
        androidx.compose.ui.viewinterop.AndroidView(
            factory = { context -> SessionView(context, engine).also { view = it } },
            modifier = Modifier.fillMaxSize().testTag("picture"),
            update = { it.mode = mode },
        )
        if (state.stage != "picture") {
            Column(
                Modifier.align(Alignment.Center).padding(24.dp),
                horizontalAlignment = Alignment.CenterHorizontally,
            ) {
                if (state.stage != "failed") CircularProgressIndicator(color = Q.Gold)
                Spacer(Modifier.height(12.dp))
                Text(
                    when (state.stage) {
                        "failed" -> stringResource(R.string.session_failed, state.message)
                        "connected" -> stringResource(R.string.session_waiting)
                        else -> stringResource(R.string.session_connecting, device.name)
                    },
                    color = Q.NightText, fontSize = 15.sp, modifier = Modifier.testTag("stage"),
                )
                if (state.stage == "failed") {
                    Spacer(Modifier.height(12.dp))
                    GoldButton(stringResource(R.string.back)) { engine.disconnect() }
                }
            }
        }
        // The floating ball (as UU Remote's): opens the session's tools.
        Box(
            Modifier
                .align(Alignment.CenterEnd)
                .padding(end = 10.dp)
                .size(44.dp)
                .clip(CircleShape)
                .background(Q.Night.copy(alpha = 0.78f))
                .border(1.5.dp, Q.Gold, CircleShape)
                .clickable { toolbar = !toolbar }
                .testTag("ball"),
            contentAlignment = Alignment.Center,
        ) {
            Image(painterResource(R.drawable.magpie), null, Modifier.size(26.dp))
        }
        if (toolbar) {
            Toolbar(
                state = state,
                mode = mode,
                muted = muted,
                keyboard = keyboard,
                onMode = { mode = it },
                onKeyboard = {
                    keyboard = !keyboard
                    view?.showKeyboard(keyboard)
                    toolbar = false
                },
                onMute = {
                    muted = !muted
                    audio.muted = muted
                },
                onKeys = { codes -> view?.combo(*codes) },
                onCommand = { engine.remote(it) },
                onClose = { toolbar = false },
                onDisconnect = { engine.disconnect() },
                modifier = Modifier.align(Alignment.BottomCenter),
            )
        }
    }
    if (state.passwordNeeded) {
        AlertDialog(
            onDismissRequest = {},
            title = { Text(stringResource(R.string.pc_password_title)) },
            text = {
                Column {
                    Text(stringResource(R.string.pc_password_text))
                    OutlinedTextField(
                        password, { password = it }, singleLine = true,
                        visualTransformation = PasswordVisualTransformation(),
                        modifier = Modifier.testTag("pc_password"),
                    )
                }
            },
            confirmButton = {
                TextButton({
                    engine.password(password)
                    password = ""
                }) { Text(stringResource(R.string.unlock)) }
            },
            dismissButton = { TextButton({ engine.disconnect() }) { Text(stringResource(R.string.disconnect)) } },
        )
    }
}

@Composable
fun Toolbar(
    state: SessionState,
    mode: SessionView.Mode,
    muted: Boolean,
    keyboard: Boolean,
    onMode: (SessionView.Mode) -> Unit,
    onKeyboard: () -> Unit,
    onMute: () -> Unit,
    onKeys: (IntArray) -> Unit,
    onCommand: (String) -> Unit,
    onClose: () -> Unit,
    onDisconnect: () -> Unit,
    modifier: Modifier = Modifier,
) {
    var keysOpen by remember { mutableStateOf(false) }
    Column(
        modifier
            .fillMaxWidth()
            .clip(RoundedCornerShape(topStart = 16.dp, topEnd = 16.dp))
            .background(Q.Night)
            .navigationBarsPadding()
            .padding(16.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text(
                if (state.relayed) stringResource(R.string.path_relayed) else stringResource(R.string.path_direct),
                color = Q.NightMuted, fontSize = 13.sp, modifier = Modifier.weight(1f),
            )
            Text("✕", color = Q.NightText, fontSize = 18.sp, modifier = Modifier.clickable(onClick = onClose).padding(4.dp))
        }
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            ToolChip(stringResource(R.string.mode_touch), mode == SessionView.Mode.Touch, Modifier.weight(1f)) {
                onMode(SessionView.Mode.Touch)
            }
            ToolChip(stringResource(R.string.mode_mouse), mode == SessionView.Mode.Mouse, Modifier.weight(1f)) {
                onMode(SessionView.Mode.Mouse)
            }
        }
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            ToolChip(stringResource(R.string.keyboard), keyboard, Modifier.weight(1f).testTag("keyboard"), onClick = onKeyboard)
            ToolChip(stringResource(R.string.keys), keysOpen, Modifier.weight(1f)) { keysOpen = !keysOpen }
            ToolChip(stringResource(if (muted) R.string.sound_off else R.string.sound_on), !muted, Modifier.weight(1f), onClick = onMute)
        }
        if (keysOpen) {
            val keys = listOf(
                "Esc" to intArrayOf(Keys.ESC), "Tab" to intArrayOf(Keys.TAB), "Win" to intArrayOf(Keys.WIN),
                "Ctrl+C" to intArrayOf(Keys.CTRL, 0x2E), "Ctrl+V" to intArrayOf(Keys.CTRL, 0x2F),
                "Ctrl+Z" to intArrayOf(Keys.CTRL, 0x2C), "Alt+Tab" to intArrayOf(Keys.ALT, Keys.TAB),
                "Alt+F4" to intArrayOf(Keys.ALT, Keys.F[3]), "Win+D" to intArrayOf(Keys.WIN, 0x20),
                "←" to intArrayOf(Keys.LEFT), "↑" to intArrayOf(Keys.UP), "↓" to intArrayOf(Keys.DOWN),
                "→" to intArrayOf(Keys.RIGHT), "Del" to intArrayOf(Keys.DELETE), "F5" to intArrayOf(Keys.F[4]),
            )
            keys.chunked(5).forEach { row ->
                Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                    row.forEach { (label, codes) ->
                        ToolChip(label, false, Modifier.weight(1f)) { onKeys(codes) }
                    }
                    repeat(5 - row.size) { Spacer(Modifier.weight(1f)) }
                }
            }
        }
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            ToolChip("Ctrl+Alt+Del", false, Modifier.weight(1f)) { onCommand("ctrl_alt_del") }
            ToolChip(stringResource(R.string.lock), false, Modifier.weight(1f)) { onCommand("lock") }
            ToolChip(stringResource(R.string.privacy_screen), false, Modifier.weight(1f)) { onCommand("privacy_on") }
        }
        Button(
            onClick = onDisconnect,
            modifier = Modifier.fillMaxWidth().height(44.dp).testTag("disconnect"),
            shape = RoundedCornerShape(8.dp),
            colors = ButtonDefaults.buttonColors(containerColor = Q.Danger, contentColor = androidx.compose.ui.graphics.Color.White),
        ) { Text(stringResource(R.string.disconnect), fontWeight = FontWeight.Bold) }
    }
}

@Composable
fun ToolChip(text: String, selected: Boolean, modifier: Modifier = Modifier, onClick: () -> Unit) {
    Box(
        modifier
            .height(40.dp)
            .clip(RoundedCornerShape(8.dp))
            .background(if (selected) Q.NightSelected else Q.NightChip)
            .border(1.dp, if (selected) Q.Gold else Q.NightChip, RoundedCornerShape(8.dp))
            .clickable(onClick = onClick),
        contentAlignment = Alignment.Center,
    ) {
        Text(text, color = if (selected) Q.Gold else Q.NightText, fontSize = 13.sp, maxLines = 1)
    }
}
