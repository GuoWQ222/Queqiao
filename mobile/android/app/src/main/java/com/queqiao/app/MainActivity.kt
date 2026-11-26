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
import androidx.compose.foundation.text.KeyboardActions
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
import androidx.compose.ui.platform.LocalConfiguration
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.ImeAction
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
    val ready = !busy && user.isNotBlank() && password.isNotEmpty() && server.isNotBlank()
    val logIn = { if (ready) engine.logIn(server, pin, user, password) }
    // The keyboard's Go key on the last field logs in (the button may sit under the keyboard).
    val go = KeyboardActions(onGo = { logIn() })
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
                keyboardOptions = KeyboardOptions(
                    keyboardType = KeyboardType.Password,
                    imeAction = if (showServer) ImeAction.Next else ImeAction.Go,
                ),
                keyboardActions = go,
            )
            if (showServer) {
                OutlinedTextField(
                    server, { server = it }, Modifier.fillMaxWidth().testTag("server"),
                    label = { Text(stringResource(R.string.server)) }, singleLine = true,
                    keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Uri, imeAction = ImeAction.Next),
                )
                OutlinedTextField(
                    pin, { pin = it }, Modifier.fillMaxWidth().testTag("pin"),
                    label = { Text(stringResource(R.string.pin)) }, singleLine = true,
                    keyboardOptions = KeyboardOptions(imeAction = ImeAction.Go),
                    keyboardActions = go,
                )
            } else {
                Text(
                    stringResource(R.string.other_server), color = Q.Accent, fontSize = 14.sp,
                    modifier = Modifier.clickable { showServer = true },
                )
            }
            if (error.isNotEmpty()) Text(error, color = Q.Danger, fontSize = 14.sp, modifier = Modifier.testTag("error"))
            GoldButton(
                stringResource(R.string.log_in), Modifier.fillMaxWidth().testTag("login"), enabled = ready,
            ) { logIn() }
            if (busy) {
                Box(Modifier.fillMaxWidth(), contentAlignment = Alignment.Center) {
                    CircularProgressIndicator(color = Q.Accent, modifier = Modifier.size(28.dp))
                }
            }
        }
        Spacer(Modifier.weight(1f))
        Text(
            stringResource(R.string.copyright), color = Q.Secondary, fontSize = 12.sp,
            modifier = Modifier.fillMaxWidth().navigationBarsPadding().padding(bottom = 12.dp),
            textAlign = androidx.compose.ui.text.style.TextAlign.Center,
        )
    }
}

@Composable
fun DevicesScreen(engine: Engine) {
    val devices by engine.devices.collectAsState()
    val error by engine.error.collectAsState()
    val created by engine.created.collectAsState()
    var menu by remember { mutableStateOf(false) }
    var removing by remember { mutableStateOf<Device?>(null) }
    // Connect on a PC another device of the account controls: asked first (as UU Remote's Take Control).
    var takingOver by remember { mutableStateOf<Device?>(null) }
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
        LazyColumn(
            Modifier.fillMaxSize().navigationBarsPadding().imePadding(),
            contentPadding = androidx.compose.foundation.layout.PaddingValues(16.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            if (others.isEmpty()) {
                item {
                    Text(
                        stringResource(R.string.no_devices), color = Q.Secondary, fontSize = 15.sp,
                        modifier = Modifier.padding(vertical = 24.dp, horizontal = 8.dp),
                    )
                }
            }
            items(others, key = { it.id }) { device ->
                DeviceCard(
                    device,
                    onConnect = {
                        if (device.controlledBy.isNotEmpty()) takingOver = device else engine.connect(device)
                    },
                    onWake = { engine.wake(device) },
                    onRemove = { removing = device },
                    onEndControl = { engine.endControl(device) },
                )
            }
            item { HelpCard(engine) }
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
    takingOver?.let { device -> TakeOverDialog(device.controlledBy, { takingOver = null }) { engine.connect(device, true) } }
    if (created.isNotEmpty()) {
        AlertDialog(
            onDismissRequest = { engine.createdSeen() },
            title = { Text(stringResource(R.string.created_title)) },
            text = { Text(stringResource(R.string.created_text, created)) },
            confirmButton = { TextButton({ engine.createdSeen() }) { Text(stringResource(R.string.ok)) } },
            modifier = Modifier.testTag("created"),
        )
    }
}

/** "Taking control will disconnect {controller}. Do you want to continue?" with Take Control and Cancel. */
@Composable
fun TakeOverDialog(controller: String, onCancel: () -> Unit, onTakeOver: () -> Unit) {
    AlertDialog(
        onDismissRequest = onCancel,
        title = { Text(stringResource(R.string.take_control)) },
        text = { Text(stringResource(R.string.take_over_text, controller)) },
        confirmButton = {
            TextButton({
                onCancel()
                onTakeOver()
            }, Modifier.testTag("take_control")) { Text(stringResource(R.string.take_control), color = Q.Accent) }
        },
        dismissButton = { TextButton(onCancel) { Text(stringResource(R.string.cancel)) } },
    )
}

@Composable
fun DeviceCard(
    device: Device,
    onConnect: () -> Unit,
    onWake: () -> Unit,
    onRemove: () -> Unit,
    onEndControl: () -> Unit,
) {
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
        if (device.controlledBy.isNotEmpty()) {
            // The being-controlled banner (as UU Remote's): who controls it, and Disconnect.
            Spacer(Modifier.height(10.dp))
            Row(
                Modifier
                    .fillMaxWidth()
                    .clip(RoundedCornerShape(8.dp))
                    .background(Q.BannerBack)
                    .padding(horizontal = 12.dp, vertical = 8.dp)
                    .testTag("controlled:${device.name}"),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Column(Modifier.weight(1f)) {
                    Text(
                        stringResource(R.string.being_controlled), color = Q.Accent, fontSize = 14.sp,
                        fontWeight = FontWeight.Bold,
                    )
                    Text(
                        stringResource(R.string.controlled_by, device.controlledBy), color = Q.Text, fontSize = 13.sp,
                        maxLines = 2, overflow = TextOverflow.Ellipsis,
                    )
                }
                TextButton(onEndControl, Modifier.testTag("end_control:${device.name}")) {
                    Text(stringResource(R.string.disconnect), color = Q.Accent, fontSize = 13.sp)
                }
            }
        }
        Spacer(Modifier.height(14.dp))
        if (device.online) {
            GoldButton(
                stringResource(R.string.connect), Modifier.fillMaxWidth().testTag("connect:${device.name}"),
                enabled = device.controllable, onClick = onConnect,
            )
        } else {
            // Offline: woken up through another device of the account on its network (Wake-on-LAN).
            var asked by remember { mutableStateOf(false) }
            GoldButton(
                stringResource(if (asked) R.string.wake_sent else R.string.wake_up),
                Modifier.fillMaxWidth().testTag("wake:${device.name}"), enabled = !asked,
            ) {
                asked = true
                onWake()
            }
        }
    }
}

/** Help someone (as UU Remote's): the help ID and code the other PC shows. */
@Composable
fun HelpCard(engine: Engine) {
    val helping by engine.helping.collectAsState()
    var id by rememberSaveable { mutableStateOf("") }
    var code by rememberSaveable { mutableStateOf("") }
    Column(
        Modifier
            .fillMaxWidth()
            .clip(RoundedCornerShape(12.dp))
            .background(Q.Card)
            .padding(16.dp),
        verticalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        Text(stringResource(R.string.help_someone), color = Q.Text, fontSize = 17.sp, fontWeight = FontWeight.Bold)
        Text(stringResource(R.string.help_hint), color = Q.Secondary, fontSize = 13.sp)
        OutlinedTextField(
            id, { id = it }, Modifier.fillMaxWidth().testTag("help_id"),
            label = { Text(stringResource(R.string.help_id)) }, singleLine = true,
            keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number),
        )
        OutlinedTextField(
            code, { code = it }, Modifier.fillMaxWidth().testTag("help_code"),
            label = { Text(stringResource(R.string.help_code)) }, singleLine = true,
        )
        GoldButton(
            stringResource(if (helping) R.string.help_waiting else R.string.connect),
            Modifier.fillMaxWidth().testTag("help_connect"), enabled = !helping && id.isNotBlank(),
        ) { engine.help(id, code) }
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
                val stopped = state.stage == "failed" || state.stage == "busy"
                if (!stopped) CircularProgressIndicator(color = Q.Gold)
                Spacer(Modifier.height(12.dp))
                Text(
                    when (state.stage) {
                        "failed" -> stringResource(R.string.session_failed, state.message)
                        "busy" -> if (state.controller.isEmpty()) {
                            stringResource(R.string.busy_helper, device.name)
                        } else {
                            stringResource(R.string.busy_controller, state.controller, device.name)
                        }
                        "connected" -> stringResource(R.string.session_waiting)
                        else -> stringResource(R.string.session_connecting, device.name)
                    },
                    color = Q.NightText, fontSize = 15.sp, modifier = Modifier.testTag("stage"),
                )
                // Another device of the account has it: asked whether to take control (Cancel: it keeps it).
                if (state.stage == "busy" && state.mayTakeOver) {
                    TakeOverDialog(state.controller, { engine.disconnect() }) { engine.connect(device, true) }
                }
                if (stopped) {
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
                engine = engine,
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
    engine: Engine,
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
    var pictureOpen by remember { mutableStateOf(false) }
    val figures by engine.figures.collectAsState()
    val screens by engine.screens.collectAsState()
    var quality by rememberSaveable { mutableStateOf("balanced") }
    var fps by rememberSaveable { mutableStateOf(60) }
    var speakersMuted by rememberSaveable { mutableStateOf(false) }
    val context = androidx.compose.ui.platform.LocalContext.current
    Column(
        modifier
            .fillMaxWidth()
            .clip(RoundedCornerShape(topStart = 16.dp, topEnd = 16.dp))
            .background(Q.Night)
            .navigationBarsPadding()
            .verticalScroll(rememberScrollState())
            .padding(16.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            // The figures, as the desktop's card: the way, the delay, frames a second, data.
            val relayed = figures.relayed || state.relayed
            // The relay's place after the way (the user, 2026-11-26: "add the relay's name"), in the phone's language.
            val chinese = LocalConfiguration.current.locales[0].language == "zh"
            val place = if (chinese) state.relayZh else state.relay
            val way = if (relayed) {
                stringResource(R.string.path_relayed) + if (place.isNotEmpty()) " · $place" else ""
            } else {
                stringResource(R.string.path_direct)
            }
            val delay = figures.rttMs?.let { " · %.0f ms".format(it) } ?: ""
            Text(
                way + delay + " · %.0f fps · %.1f Mbit/s".format(figures.fps, figures.mbps),
                color = Q.NightMuted, fontSize = 13.sp, modifier = Modifier.weight(1f).testTag("figures"),
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
            ToolChip(stringResource(R.string.picture), pictureOpen, Modifier.weight(1f)) { pictureOpen = !pictureOpen }
            ToolChip(stringResource(R.string.send_clipboard), false, Modifier.weight(1f)) {
                val sent = engine.sendPhoneClipboard()
                android.widget.Toast.makeText(
                    context, if (sent) R.string.clipboard_sent else R.string.clipboard_empty, android.widget.Toast.LENGTH_SHORT,
                ).show()
            }
            ToolChip(stringResource(R.string.pc_speakers_off), speakersMuted, Modifier.weight(1f)) {
                speakersMuted = !speakersMuted
                engine.muteSpeakers(speakersMuted)
            }
        }
        if (pictureOpen) {
            // Quality and frame rate (as the desktop's session bar), and the PC's screens.
            Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                for ((key, label) in listOf("smooth" to R.string.smooth, "balanced" to R.string.balanced, "sharp" to R.string.sharp)) {
                    ToolChip(stringResource(label), quality == key, Modifier.weight(1f)) {
                        quality = key
                        engine.picture(fps, quality)
                    }
                }
            }
            Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                for (rate in listOf(30, 60)) {
                    ToolChip("$rate fps", fps == rate, Modifier.weight(1f)) {
                        fps = rate
                        engine.picture(fps, quality)
                    }
                }
            }
            if (screens.count > 1) {
                Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                    for (n in 0 until screens.count) {
                        ToolChip(stringResource(R.string.screen_n, n + 1), screens.current == n, Modifier.weight(1f)) {
                            engine.showScreen(n)
                        }
                    }
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
