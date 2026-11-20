//! queqiao-viewer: the controlling side. Shows the remote screen in a window and sends mouse and
//! keyboard input made in that window.
//!
//! With an account (M3), started without options: the login form (once; the login is then saved,
//! DPAPI-encrypted), then the account's devices; double-click one to control it.
//! For tests and scripts:
//!   queqiao-viewer [--server <host:port> --pin <fingerprint> --user <name> --password <password>]
//!                  [--device <name or id> | --list | --logout | --ui-test <name or id>]
//!   --device connects without any window but the session's; --list prints the devices; --logout ends the
//!   saved login; --ui-test goes through the login form and the device list as a user would (filled and
//!   pressed by the program) and saves pictures of both next to the report.
//! By IP address (M2): queqiao-viewer --listen <ip:port> | --connect <agent-ip:port>
//! Common options: [--seconds N] [--width N] [--report <file>] [--input-test] [--force <switches>]
//! --force sets test switches as QUEQIAO_FORCE does (for example network:relay).
//! --input-test sends a fixed sequence instead of the window's input: five pointer moves and a press of
//! F24 (a key no program uses); the agent run with --verify-input checks them. --version prints the
//! version.

#![windows_subsystem = "windows"]

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use queqiao_account::{
    Account, AccountEvent, ClientMessage, Device, DeviceIdentity, Role as AccountRole, SavedLogin,
    new_session_id,
};
use queqiao_codec::{DecodedFrame, DecoderChoice, H264Decoder};
use queqiao_core::protocol::{
    Command, InputEvent, MAX_CLIPBOARD_TEXT, Message, MouseButton, Quality,
};
use queqiao_core::text::{tr, trf};
use queqiao_display::{HelpAction, HelpForm, normalize_code};

/// The session window's menu items for the controlled PC (system-menu ids: multiples of 16).
const MENU_CTRL_ALT_DEL: u16 = 0x1000;
const MENU_LOCK: u16 = 0x1010;
const MENU_RESTART: u16 = 0x1020;
const MENU_SOUND: u16 = 0x1030;
const MENU_FULL_SCREEN: u16 = 0x1040;
const MENU_PRIVACY: u16 = 0x1050;
const MENU_TERMINAL: u16 = 0x1060;
/// Picture settings: frame rate (at most 30, 60, or what that screen shows) and quality.
const MENU_FPS: [(u16, u8, &str); 3] = [
    (0x1100, 30, "30 frames per second"),
    (0x1110, 60, "60 frames per second"),
    (0x1120, 0, "As many as that screen shows"),
    // (translated where the menu is made)
];
const MENU_QUALITY: [(u16, Quality, &str); 3] = [
    (
        0x1200,
        Quality::Smooth,
        "Smooth (fewer bits, slow networks)",
    ),
    (0x1210, Quality::Balanced, "Balanced"),
    (0x1220, Quality::Sharp, "Sharp (twice the bits, for text)"),
];
/// A new command line's window size until its window reports its own.
const TERMINAL_SIZE: (u16, u16) = (120, 30);
/// Scancodes of the full-screen key, Ctrl+Alt+Enter (handled here, not sent): Enter, Ctrl, Alt.
const KEY_ENTER: u16 = 0x1C;
const KEY_CTRL: u16 = 0x1D;
const KEY_ALT: u16 = 0x38;
use queqiao_audio::{Player, TEST_TONE_HZ};
use queqiao_files::{Outcome, Transfers, downloads_folder};
use queqiao_input::Clipboard;

/// The start of the clipboard text the tests send (each side checks it got the other's).
const CLIPBOARD_TEST_TEXT: &str = "Queqiao clipboard test";
use queqiao_display::{
    DeviceList, DeviceRow, ListAction, LoginAction, LoginFields, LoginForm, Presenter, Window,
    WindowEvent, fit, show_error,
};
use queqiao_terminal::{TerminalMessage, TerminalWindow, WindowEvent as TerminalEvent};
use queqiao_transport::{Peer, PeerEvent, Role, Signal, Signaling, Stream};
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE, D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP,
};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::{IDXGIAdapter, IDXGIDevice};
use windows::core::Interface;

/// The input test: pointer positions as fractions of the remote screen, then F24 down and up.
const TEST_POINTS: [(f64, f64); 5] = [
    (0.25, 0.25),
    (0.75, 0.25),
    (0.75, 0.75),
    (0.25, 0.75),
    (0.5, 0.5),
];
const TEST_SCANCODE: u16 = 0x76;
/// A session that is not connected after this long is given up.
const CONNECT_LIMIT: Duration = Duration::from_secs(30);

#[derive(Default)]
struct Options {
    /// A device whose new key the person confirmed (its old pin is dropped).
    trust_new_key: Option<String>,
    connect: Option<String>,
    listen: Option<String>,
    server: Option<String>,
    pin: Option<String>,
    user: Option<String>,
    password: Option<String>,
    device: Option<String>,
    list: bool,
    logout: bool,
    ui_test: Option<String>,
    seconds: Option<u64>,
    width: Option<i32>,
    report: Option<PathBuf>,
    input_test: bool,
    /// For tests: send a clipboard text once the data channel is open.
    clipboard_test: bool,
    /// Files to send once the session's files channel is open (tests; people drop files on the window).
    send_files: Vec<PathBuf>,
    /// Commands to send once the session's control channel is open (tests; people use the window menu).
    commands: Vec<Command>,
    /// Play the other PC's sound on this render device (part of its name) instead of the default one.
    sound_device: Option<String>,
    /// Check the received sound for the agent's test tone and report it.
    sound_test: bool,
    /// Start with the other PC's sound off.
    mute: bool,
    /// Start in full screen.
    full_screen: bool,
    /// Darken that PC's screen from the start.
    privacy: bool,
    /// Open a command line on that PC and check it (its window closes by itself).
    terminal_test: bool,
    /// Picture settings to send at the start: frames per second at most (0: the screen's) and quality.
    picture: Option<(u8, Quality)>,
    /// Help someone of another account: their help ID and code (tests; people use "Help someone...").
    help_id: Option<String>,
    help_code: Option<String>,
}

impl Options {
    fn direct(&self) -> bool {
        self.connect.is_some() || self.listen.is_some()
    }

    /// Started by a person (no test or script options): errors go into a message box.
    fn interactive(&self) -> bool {
        !self.direct()
            && self.device.is_none()
            && !self.list
            && !self.logout
            && self.ui_test.is_none()
            && self.report.is_none()
    }
}

#[derive(Default)]
struct Stats {
    received: u32,
    lost: u32,
    decoded: u32,
    shown: u32,
    bytes: usize,
    decode_ms: Vec<f64>,
    /// Arrival of the frame -> on screen, per frame time.
    viewer_ms: HashMap<u64, f64>,
    /// Screen change -> sent, reported by the agent per frame time.
    agent_ms: HashMap<u64, f64>,
    rtt_ms: Vec<f64>,
    inputs_sent: u32,
    /// Where the viewer's loop spends its time: handling window messages, frame arrival -> picked up by
    /// the loop, putting a picture on screen.
    pump_ms: Vec<f64>,
    pickup_ms: Vec<f64>,
    show_ms: Vec<f64>,
}

/// The session's state in the window title, once a second: delay (round trip), frames shown per second,
/// data received and whether the session is direct or relayed. A notice from the controlled PC stays there
/// for a few seconds.
struct TitleStatus {
    agent: String,
    relayed: bool,
    title: String,
    notice: Option<(String, Instant)>,
    /// Counters at the last update, for the rates.
    last: (Instant, u32, usize),
}

const NOTICE_SHOWN: Duration = Duration::from_secs(6);

impl TitleStatus {
    fn new(agent: &str) -> TitleStatus {
        TitleStatus {
            agent: agent.to_string(),
            relayed: false,
            title: String::new(),
            notice: None,
            last: (Instant::now(), 0, 0),
        }
    }

    fn notice(&mut self, text: &str) {
        let short: String = text.chars().take(120).collect();
        self.notice = Some((short, Instant::now() + NOTICE_SHOWN));
        // Shown at once rather than at the next update.
        self.last.0 -= Duration::from_secs(1);
    }

    fn update(&mut self, window: &Window, stats: &Stats) {
        let now = Instant::now();
        let (at, shown, bytes) = self.last;
        let seconds = now.duration_since(at).as_secs_f64();
        if seconds < 1.0 {
            return;
        }
        let fps = f64::from(stats.shown.saturating_sub(shown)) / seconds;
        let kbit = stats.bytes.saturating_sub(bytes) as f64 * 8.0 / seconds / 1e3;
        // A still screen sends little: kbit/s then, so it does not read as 0.0.
        let rate = if kbit < 1000.0 {
            format!("{kbit:.0} kbit/s")
        } else {
            format!("{:.1} Mbit/s", kbit / 1e3)
        };
        let delay = match stats.rtt_ms.last() {
            Some(rtt) => format!("{rtt:.0} ms"),
            None => "- ms".into(),
        };
        let path = if self.relayed { "relayed" } else { "direct" };
        // English for the report (tests read it); the window shows it in the user's language.
        let line = |name: &str, path: &str| {
            format!(
                "{name} - {} | {delay} | {fps:.0} fps | {rate} | {path}",
                self.agent
            )
        };
        let (mut title, mut shown) = (line("Queqiao", path), line(tr("Queqiao"), tr(path)));
        if let Some((text, until)) = &self.notice {
            if now < *until {
                title = format!("{title} | {text}");
                shown = format!("{shown} | {text}");
            } else {
                self.notice = None;
            }
        }
        if title != self.title {
            window.set_title(&shown);
            self.title = title;
        }
        self.last = (now, stats.shown, stats.bytes);
    }
}

fn main() {
    // A terminal window (started by a session of this viewer): it runs until its command line ends.
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("--terminal-pipe")
        && let Some(pipe) = args.get(2)
    {
        let title = match args.iter().position(|arg| arg == "--terminal-title") {
            Some(at) => args.get(at + 1).cloned().unwrap_or_default(),
            None => String::new(),
        };
        let test = args.iter().any(|arg| arg == "--terminal-test");
        std::process::exit(queqiao_terminal::run_helper(pipe, &title, test));
    }
    let banner = queqiao_core::banner(env!("CARGO_PKG_NAME"));
    let Some(options) = parse(std::env::args().skip(1)) else {
        println!("{banner}");
        println!(
            "usage: queqiao-viewer [--server <host:port> --pin <fingerprint> --user <name> --password <password>]"
        );
        println!(
            "                      [--device <name or id> | --list | --logout | --ui-test <name or id>]"
        );
        println!("       queqiao-viewer --listen <ip:port> | --connect <agent-ip:port>");
        println!("       common: [--seconds N] [--width N] [--report <file>] [--input-test]");
        return;
    };
    let mut lines = vec![banner];
    let mut failure = None;
    let passed = match run(&options, &mut lines) {
        Ok(passed) => passed,
        Err(error) => {
            lines.push(format!("ERROR {error}"));
            failure = Some(error);
            false
        }
    };
    lines.push(format!("RESULT {}", if passed { "PASS" } else { "FAIL" }));
    match &options.report {
        Some(path) => {
            let _ = std::fs::write(path, lines.join("\r\n") + "\r\n");
        }
        None => lines.iter().for_each(|line| println!("{line}")),
    }
    if let Some(error) = failure
        && options.interactive()
    {
        show_error(tr("Queqiao"), &error);
    }
    std::process::exit(if passed { 0 } else { 1 });
}

fn parse(mut args: impl Iterator<Item = String>) -> Option<Options> {
    let mut options = Options::default();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--connect" => options.connect = args.next(),
            "--listen" => options.listen = args.next(),
            "--server" => options.server = args.next(),
            "--pin" => options.pin = args.next(),
            "--user" => options.user = args.next(),
            "--password" => options.password = args.next(),
            "--device" => options.device = args.next(),
            "--list" => options.list = true,
            "--logout" => options.logout = true,
            "--ui-test" => options.ui_test = args.next(),
            "--seconds" => options.seconds = args.next().and_then(|v| v.parse().ok()),
            "--width" => options.width = args.next().and_then(|v| v.parse().ok()),
            "--report" => options.report = args.next().map(PathBuf::from),
            "--input-test" => options.input_test = true,
            "--clipboard-test" => options.clipboard_test = true,
            "--send-file" => options.send_files.push(PathBuf::from(args.next()?)),
            "--sound-device" => options.sound_device = Some(args.next()?),
            "--sound-test" => options.sound_test = true,
            "--mute" => options.mute = true,
            "--full-screen" => options.full_screen = true,
            "--privacy" => options.privacy = true,
            "--terminal-test" => options.terminal_test = true,
            "--help-id" => options.help_id = Some(args.next()?),
            "--help-code" => options.help_code = Some(normalize_code(&args.next()?)),
            "--picture" => {
                let value = args.next()?;
                let (fps, quality) = value.split_once(',')?;
                let quality = match quality {
                    "smooth" => Quality::Smooth,
                    "balanced" => Quality::Balanced,
                    "sharp" => Quality::Sharp,
                    _ => return None,
                };
                options.picture = Some((fps.parse().ok()?, quality));
            }
            "--command" => options.commands.push(match args.next()?.as_str() {
                "lock" => Command::Lock,
                "restart" => Command::Restart,
                "ctrl-alt-del" => Command::CtrlAltDel,
                _ => return None,
            }),
            "--trust-new-key" => options.trust_new_key = args.next(),
            "--force" => queqiao_core::set_forced(&args.next()?),
            _ => return None,
        }
    }
    Some(options)
}

fn run(options: &Options, lines: &mut Vec<String>) -> Result<bool, String> {
    lines.extend(queqiao_core::platform::prepare_process());
    if !options.direct() {
        return run_account(options, lines);
    }
    let (device, context, gpu) = create_device()?;
    lines.push(format!("device: {gpu}"));
    let mut signal = match (&options.connect, &options.listen) {
        (Some(address), _) => Signal::connect(address, Duration::from_secs(60)),
        (None, Some(address)) => Signal::listen(address, Duration::from_secs(120)),
        (None, None) => unreachable!("direct() checked it"),
    }
    .map_err(|e| e.to_string())?;
    signal.hello("viewer").map_err(|e| e.to_string())?;
    let agent = signal.peer_address().map_err(|e| e.to_string())?;
    session(
        options,
        &device,
        &context,
        &mut signal,
        &agent.to_string(),
        lines,
    )
}

fn run_account(options: &Options, lines: &mut Vec<String>) -> Result<bool, String> {
    let identity = DeviceIdentity::load_or_create().map_err(|e| e.to_string())?;
    let mut login = SavedLogin::load(AccountRole::Viewer).unwrap_or_default();
    if let Some(server) = &options.server {
        if login.server != *server {
            login.refresh_token = None;
        }
        login.server = server.clone();
    }
    if let Some(pin) = &options.pin {
        login.pin = pin.clone();
    }
    let pictures = options.ui_test.as_ref().map(|_| {
        options
            .report
            .as_ref()
            .and_then(|report| report.parent())
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
    });
    let Some(mut account) = log_in(options, &mut login, &identity, pictures.as_deref(), lines)?
    else {
        lines.push("the login form was closed".into());
        return Ok(true);
    };
    login.save(AccountRole::Viewer).map_err(|e| e.to_string())?;
    if let Some(device) = &options.trust_new_key {
        account.trust_new_key(device).map_err(|e| e.to_string())?;
        lines.push(format!("the key of device {device} will be pinned anew"));
    }
    lines.push(format!(
        "logged in as {} at {}; this device: {} ({})",
        account.user, login.server, identity.name, identity.id
    ));
    if options.logout {
        account.send(ClientMessage::Logout);
        login.refresh_token = None;
        login.save(AccountRole::Viewer).map_err(|e| e.to_string())?;
        lines.push("logged out: the saved login is deleted here and revoked on the server".into());
        return Ok(true);
    }
    if options.list {
        for device in &account.devices {
            let state = if device.controllable {
                "online"
            } else {
                "offline"
            };
            let own = if device.id == account.device_id {
                ", this device"
            } else {
                ""
            };
            lines.push(format!(
                "device {} ({}): {state}{own}",
                device.name, device.id
            ));
        }
        return Ok(true);
    }
    let (device, context, gpu) = create_device()?;
    lines.push(format!("device: {gpu}"));
    if let Some(key) = &options.device {
        let target = wait_for_device(&mut account, key, Duration::from_secs(30))?;
        return connect_to(&mut account, &target, options, &device, &context, lines);
    }
    if let (Some(help_id), Some(code)) = (&options.help_id, &options.help_code) {
        let target = account
            .request_help(help_id, code)
            .map_err(|e| e.to_string())?;
        lines.push(format!("help granted: {} ({})", target.name, target.id));
        return connect_to(&mut account, &target, options, &device, &context, lines);
    }
    device_list(
        options,
        &mut account,
        &mut login,
        &identity,
        &device,
        &context,
        pictures.as_deref(),
        lines,
    )
}

/// The account connection: with the password from the options, the saved login, or the login form.
/// None when the person closed the form.
fn log_in(
    options: &Options,
    login: &mut SavedLogin,
    identity: &DeviceIdentity,
    pictures: Option<&Path>,
    lines: &mut Vec<String>,
) -> Result<Option<Account>, String> {
    let given = options.user.as_deref().zip(options.password.as_deref());
    if options.ui_test.is_none() {
        if let Some(password) = given {
            let account = Account::connect(login, identity, AccountRole::Viewer, Some(password));
            return account.map(Some).map_err(|e| e.to_string());
        }
        if login.refresh_token.is_some() && options.user.is_none() {
            match Account::connect(login, identity, AccountRole::Viewer, None) {
                Ok(account) => return Ok(Some(account)),
                Err(error) if !options.interactive() => return Err(error.0),
                Err(error) => {
                    lines.push(format!("the saved login was refused: {error}"));
                    login.refresh_token = None;
                }
            }
        }
        if !options.interactive() {
            return Err("not logged in: give --server, --pin, --user and --password once".into());
        }
    }

    let defaults = LoginFields {
        server: login.server.clone(),
        pin: login.pin.clone(),
        user: options.user.clone().unwrap_or_default(),
        password: String::new(),
    };
    let mut form = LoginForm::new(tr("Queqiao - log in"), &defaults)
        .map_err(|e| format!("login form: {}", describe(e)))?;
    if options.ui_test.is_some() {
        form.set_fields(&LoginFields {
            password: options.password.clone().unwrap_or_default(),
            ..defaults.clone()
        });
        settle(|| form.next(20).is_some());
        if let Some(folder) = pictures {
            form.save_picture(&folder.join("login.bmp"))
                .map_err(|e| format!("login picture: {e}"))?;
        }
        form.submit();
    }
    loop {
        match form.next(15) {
            Some(LoginAction::Cancel) => return Ok(None),
            Some(LoginAction::Submit(fields)) => {
                if [&fields.server, &fields.pin, &fields.user, &fields.password]
                    .iter()
                    .any(|value| value.is_empty())
                {
                    form.set_status(tr("Please fill in all four fields."));
                    continue;
                }
                if login.server != fields.server {
                    login.refresh_token = None;
                }
                login.server = fields.server.clone();
                login.pin = fields.pin.clone();
                form.set_busy(true, "Logging in...");
                // In the background, so the form keeps responding.
                let (mut attempt, identity) = (login.clone(), identity.clone());
                let worker = std::thread::spawn(move || {
                    let result = Account::connect(
                        &mut attempt,
                        &identity,
                        AccountRole::Viewer,
                        Some((&fields.user, &fields.password)),
                    );
                    (result, attempt)
                });
                while !worker.is_finished() {
                    if let Some(LoginAction::Cancel) = form.next(15) {
                        return Ok(None);
                    }
                }
                let (result, attempt) = worker
                    .join()
                    .map_err(|_| "the login thread failed".to_string())?;
                match result {
                    Ok(account) => {
                        *login = attempt;
                        return Ok(Some(account));
                    }
                    Err(error) if options.ui_test.is_some() => return Err(error.0),
                    Err(error) => form.set_busy(false, &error.0),
                }
            }
            None => {}
        }
    }
}

/// The device list: double-click (or select and press Connect) to control a device.
#[allow(clippy::too_many_arguments)]
fn device_list(
    options: &Options,
    account: &mut Account,
    login: &mut SavedLogin,
    identity: &DeviceIdentity,
    device: &ID3D11Device,
    context: &ID3D11DeviceContext,
    pictures: Option<&Path>,
    lines: &mut Vec<String>,
) -> Result<bool, String> {
    let heading = trf(
        "Signed in as {user}. Your devices:",
        &[("user", &account.user)],
    );
    let mut list = DeviceList::new(tr("Queqiao"), &heading)
        .map_err(|e| format!("device list: {}", describe(e)))?;
    list.set_rows(rows(&account.devices, &account.device_id));
    let (mut sessions, mut passed) = (0u32, true);
    let mut reconnect_at: Option<Instant> = None;
    let mut test_picked = false;
    let test_deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match list.next(15) {
            Some(ListAction::Closed) => break,
            Some(ListAction::Help) => {
                list.set_enabled(false);
                if let Some(target) = ask_for_help_target(account, lines) {
                    sessions += 1;
                    match connect_to(account, &target, options, device, context, lines) {
                        Ok(ok) => {
                            passed &= ok;
                            list.set_status(&trf(
                                "The session with {name} has ended.",
                                &[("name", &target.name)],
                            ));
                        }
                        Err(error) => {
                            passed = false;
                            list.set_status(&trf(
                                "Could not connect to {name}: {error}",
                                &[("name", &target.name), ("error", &error)],
                            ));
                        }
                    }
                }
                list.set_enabled(true);
            }
            Some(ListAction::LogOut) => {
                account.send(ClientMessage::Logout);
                login.refresh_token = None;
                login.save(AccountRole::Viewer).map_err(|e| e.to_string())?;
                lines.push("logged out".into());
                break;
            }
            Some(ListAction::Connect(id)) => {
                match account.devices.iter().find(|d| d.id == id).cloned() {
                    Some(target) if target.controllable => {
                        list.set_enabled(false);
                        list.set_status(&trf("Connected to {name}.", &[("name", &target.name)]));
                        let result = connect_to(account, &target, options, device, context, lines);
                        list.set_enabled(true);
                        sessions += 1;
                        match result {
                            Ok(ok) => {
                                passed &= ok;
                                list.set_status(&trf(
                                    "The session with {name} has ended.",
                                    &[("name", &target.name)],
                                ));
                            }
                            Err(error) => {
                                passed = false;
                                lines.push(format!("session failed: {error}"));
                                list.set_status(&trf(
                                    "Could not connect to {name}: {error}",
                                    &[("name", &target.name), ("error", &error)],
                                ));
                            }
                        }
                        if options.ui_test.is_some() {
                            break;
                        }
                    }
                    Some(target) => {
                        list.set_status(&trf(
                            "{name} is offline: Queqiao is not running there.",
                            &[("name", &target.name)],
                        ));
                    }
                    None => {}
                }
            }
            None => {}
        }
        if reconnect_at.is_none() {
            while let Some(event) = account.next_event(Duration::ZERO) {
                match event {
                    AccountEvent::Devices(devices) => {
                        list.set_rows(rows(&devices, &account.device_id))
                    }
                    AccountEvent::Closed(reason) => {
                        list.set_status(&trf(
                            "Lost the connection to the server ({reason}). Trying again...",
                            &[("reason", &reason)],
                        ));
                        reconnect_at = Some(Instant::now() + Duration::from_secs(2));
                        break;
                    }
                    AccountEvent::Error(message) | AccountEvent::Notice(message) => {
                        list.set_status(&message)
                    }
                    AccountEvent::Signal { .. }
                    | AccountEvent::IceServers { .. }
                    | AccountEvent::PasswordChanged(_)
                    | AccountEvent::HelpOpen(_)
                    | AccountEvent::HelpClosed(_)
                    | AccountEvent::HelpRequested { .. }
                    | AccountEvent::HelpGranted(_) => {}
                }
            }
        }
        if reconnect_at.is_some_and(|at| Instant::now() >= at) {
            match Account::connect(login, identity, AccountRole::Viewer, None) {
                Ok(new) => {
                    *account = new;
                    list.set_rows(rows(&account.devices, &account.device_id));
                    list.set_status(tr("Connected to the server again."));
                    reconnect_at = None;
                }
                Err(error) => {
                    list.set_status(&trf(
                        "The server cannot be reached ({error}). Trying again...",
                        &[("error", &error.to_string())],
                    ));
                    reconnect_at = Some(Instant::now() + Duration::from_secs(5));
                }
            }
        }
        // The UI test: select the device once it is online and press Connect, as a person would.
        if let Some(key) = &options.ui_test
            && !test_picked
        {
            let online = account
                .devices
                .iter()
                .any(|d| (d.id == *key || d.name.eq_ignore_ascii_case(key)) && d.controllable);
            if online && list.select(key) {
                settle(|| list.next(20).is_some());
                if let Some(folder) = pictures {
                    list.save_picture(&folder.join("devices.bmp"))
                        .map_err(|e| format!("device list picture: {e}"))?;
                }
                list.press_connect();
                test_picked = true;
            } else if Instant::now() > test_deadline {
                return Err(format!("{key} did not come online within 30 s"));
            }
        }
    }
    lines.push(format!("{sessions} sessions from the device list"));
    Ok(passed && (options.ui_test.is_none() || sessions == 1))
}

/// Lets a form paint for about 200 ms before its picture is taken; `step` handles messages.
fn settle(mut step: impl FnMut() -> bool) {
    for _ in 0..10 {
        let _ = step();
    }
}

/// The device list's rows: devices that can be controlled first, then by name.
fn rows(devices: &[Device], own: &str) -> Vec<DeviceRow> {
    let mut sorted: Vec<&Device> = devices.iter().collect();
    sorted.sort_by_key(|d| (!d.controllable, d.name.to_lowercase()));
    sorted
        .into_iter()
        .map(|d| DeviceRow {
            id: d.id.clone(),
            name: d.name.clone(),
            status: format!(
                "{}{}",
                tr(if d.controllable { "Online" } else { "Offline" }),
                if d.id == own {
                    format!(" - {}", tr("this PC"))
                } else {
                    String::new()
                }
            ),
        })
        .collect()
}

/// Waits until the device (id or name, any case) can be controlled.
fn wait_for_device(account: &mut Account, key: &str, timeout: Duration) -> Result<Device, String> {
    let deadline = Instant::now() + timeout;
    loop {
        let found = account
            .devices
            .iter()
            .find(|d| (d.id == key || d.name.eq_ignore_ascii_case(key)) && d.controllable);
        if let Some(device) = found {
            return Ok(device.clone());
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            let known: Vec<&str> = account.devices.iter().map(|d| d.name.as_str()).collect();
            return Err(format!(
                "{key} is not online (devices on the account: {})",
                known.join(", ")
            ));
        }
        if let Some(AccountEvent::Closed(reason)) =
            account.next_event(left.min(Duration::from_millis(200)))
        {
            return Err(reason);
        }
    }
}

/// A session with the agent on `target`, set up through the server.
/// The "Help someone..." dialog: the help ID and code, until the server grants them (or the person gives up).
fn ask_for_help_target(account: &mut Account, lines: &mut Vec<String>) -> Option<Device> {
    let mut form = HelpForm::new().ok()?;
    loop {
        match form.next(50) {
            Some(HelpAction::Submit { help_id, code }) => {
                match account.request_help(&help_id, &code) {
                    Ok(target) => {
                        lines.push(format!("help granted: {} ({})", target.name, target.id));
                        return Some(target);
                    }
                    Err(error) => form.set_status(&error.0),
                }
            }
            Some(HelpAction::Cancel) => return None,
            None => {}
        }
    }
}

fn connect_to(
    account: &mut Account,
    target: &Device,
    options: &Options,
    device: &ID3D11Device,
    context: &ID3D11DeviceContext,
    lines: &mut Vec<String>,
) -> Result<bool, String> {
    let id = new_session_id();
    let mut signal = account.session(&target.id, AccountRole::Agent, &id);
    signal.hello("viewer").map_err(|e| e.to_string())?;
    session(options, device, context, &mut signal, &target.name, lines)
}

/// One session: the agent's screen in a window, this window's input to the agent.
fn session(
    options: &Options,
    device: &ID3D11Device,
    context: &ID3D11DeviceContext,
    signal: &mut dyn Signaling,
    agent: &str,
    lines: &mut Vec<String>,
) -> Result<bool, String> {
    lines.push(format!("agent: {agent}"));
    let width = options.width.unwrap_or(1280);
    let mut window = Window::new(&format!("Queqiao - {agent}"), width, width * 9 / 16)
        .map_err(|e| format!("window: {}", describe(e)))?;
    let mut presenter = Presenter::new(device, context, &window)
        .map_err(|e| format!("presenter: {}", describe(e)))?;
    let mut decoder = H264Decoder::new(device, context, DecoderChoice::from_env())
        .map_err(|e| format!("decoder: {}", describe(e)))?;
    lines.push(format!("decoder: {}", decoder.name()));

    let started = Instant::now();
    let peer = Peer::start(Role::Viewer, signal).map_err(|e| e.to_string())?;
    lines.extend(peer.candidates().iter().cloned());
    let end = options.seconds.map(|s| started + Duration::from_secs(s));
    let mut stats = Stats::default();
    let (mut connected_after, mut first_picture_after) = (None, None);
    let mut channel_open = false;
    // The clipboard both ways (text); the last text from the agent is neither set again nor sent back.
    let clipboard = Clipboard::start()
        .map_err(|error| lines.push(format!("clipboard: not shared ({error})")))
        .ok();
    let (mut clipboard_in, mut clipboard_out) = (0u32, 0u32);
    let mut clipboard_last_in: Option<String> = None;
    let mut clipboard_test_received = false;
    let mut clipboard_test_at: Option<Instant> = None;
    let mut picture_size = (0u32, 0u32);
    let mut held_keys: HashSet<(u16, bool)> = HashSet::new();
    let mut next_ping = Instant::now();
    let mut ping_id = 0u32;
    let mut test_step = 0usize;
    let mut next_test_step: Option<Instant> = None;
    let mut decoded: Vec<DecodedFrame> = Vec::new();
    // Files both ways: dropped on the window, or from the agent into Downloads\Queqiao.
    let mut transfers = Transfers::new(downloads_folder(), true);
    for path in &options.send_files {
        let _ = transfers.send(path);
    }
    let mut status = TitleStatus::new(agent);
    // The other PC's sound; "Play that PC's sound" in the window menu turns it off there (no data then).
    let player = Player::start(options.sound_device.clone(), options.sound_test);
    let mut sound_on = !options.mute;
    player.set_muted(options.mute);
    window.add_menu_items(&[
        (MENU_CTRL_ALT_DEL, tr("Send Ctrl+Alt+Del")),
        (MENU_LOCK, tr("Lock that PC")),
        (MENU_RESTART, tr("Restart that PC...")),
        (MENU_SOUND, tr("Play that PC's sound")),
        (MENU_PRIVACY, tr("Darken that PC's screen")),
        (MENU_TERMINAL, tr("Open a command line on that PC")),
        (MENU_FULL_SCREEN, tr("Full screen (Ctrl+Alt+Enter)")),
    ]);
    let mut privacy_on = options.privacy;
    let fps_items: Vec<(u16, &str)> = MENU_FPS
        .iter()
        .map(|(id, _, text)| (*id, tr(text)))
        .collect();
    let quality_items: Vec<(u16, &str)> = MENU_QUALITY
        .iter()
        .map(|(id, _, text)| (*id, tr(text)))
        .collect();
    window.add_submenu(tr("Frame rate"), &fps_items);
    window.add_submenu(tr("Picture quality"), &quality_items);
    let (mut fps_cap, mut quality) = options.picture.unwrap_or((60, Quality::Balanced));
    let show_picture_choice = |window: &Window, fps_cap: u8, quality: Quality| {
        for (id, fps, _) in MENU_FPS {
            window.check_menu_item(id, fps == fps_cap);
        }
        for (id, level, _) in MENU_QUALITY {
            window.check_menu_item(id, level == quality);
        }
    };
    show_picture_choice(&window, fps_cap, quality);
    let mut terminals = TerminalWindows::default();
    window.check_menu_item(MENU_PRIVACY, privacy_on);
    window.set_full_screen(options.full_screen);
    window.check_menu_item(MENU_SOUND, sound_on);
    let mut closed = None;

    'session: loop {
        status.update(&window, &stats);
        if end.is_some_and(|end| Instant::now() > end) {
            break;
        }
        if connected_after.is_none() && started.elapsed() > CONNECT_LIMIT {
            closed = Some(format!(
                "could not connect within {} s",
                CONNECT_LIMIT.as_secs()
            ));
            break;
        }
        let pump_start = Instant::now();
        let open = window.pump();
        stats.pump_ms.push(pump_start.elapsed().as_secs_f64() * 1e3);
        for slow in window.take_slow_messages() {
            lines.push(format!(
                "slow at {} ms: {slow}",
                started.elapsed().as_millis()
            ));
        }
        if !open {
            closed = Some("the window was closed".to_string());
            break;
        }
        for event in window.take_events() {
            if let WindowEvent::Menu(id) = event {
                let command = match id {
                    MENU_CTRL_ALT_DEL => Some(Command::CtrlAltDel),
                    MENU_LOCK => Some(Command::Lock),
                    id if MENU_FPS.iter().any(|(item, _, _)| *item == id) => {
                        fps_cap = MENU_FPS.iter().find(|(item, _, _)| *item == id).map_or(60, |(_, fps, _)| *fps);
                        show_picture_choice(&window, fps_cap, quality);
                        Some(Command::Picture { fps: fps_cap, quality })
                    }
                    id if MENU_QUALITY.iter().any(|(item, _, _)| *item == id) => {
                        quality = MENU_QUALITY
                            .iter()
                            .find(|(item, _, _)| *item == id)
                            .map_or(Quality::Balanced, |(_, level, _)| *level);
                        show_picture_choice(&window, fps_cap, quality);
                        Some(Command::Picture { fps: fps_cap, quality })
                    }
                    MENU_TERMINAL => {
                        terminals.open(&peer, agent, false, lines);
                        None
                    }
                    MENU_PRIVACY => {
                        privacy_on = !privacy_on;
                        window.check_menu_item(MENU_PRIVACY, privacy_on);
                        Some(if privacy_on { Command::PrivacyOn } else { Command::PrivacyOff })
                    }
                    MENU_FULL_SCREEN => {
                        let on = !window.is_full_screen();
                        window.set_full_screen(on);
                        None
                    }
                    MENU_SOUND => {
                        sound_on = !sound_on;
                        player.set_muted(!sound_on);
                        window.check_menu_item(MENU_SOUND, sound_on);
                        Some(if sound_on { Command::SoundOn } else { Command::SoundOff })
                    }
                    MENU_RESTART
                        if window.confirm(
                            "Queqiao",
                            &format!("Restart {agent}? Programs there may ask to save their work; unsaved work can be lost."),
                        ) =>
                    {
                        Some(Command::Restart)
                    }
                    _ => None,
                };
                if let Some(command) = command {
                    peer.send_data(Message::Command(command).encode());
                    lines.push(format!("sent command: {command:?}"));
                }
                continue;
            }
            // Ctrl+Alt+Enter: full screen on or off (every other key goes to that PC).
            if let WindowEvent::Key {
                scancode: KEY_ENTER,
                down: true,
                ..
            } = event
                && held_keys.iter().any(|(code, _)| *code == KEY_CTRL)
                && held_keys.iter().any(|(code, _)| *code == KEY_ALT)
            {
                let on = !window.is_full_screen();
                window.set_full_screen(on);
                continue;
            }
            if let WindowEvent::FilesDropped(paths) = &event {
                for path in paths {
                    match transfers.send(path) {
                        Ok(()) => lines.push(format!("sending {}", path.display())),
                        Err(error) => lines.push(format!("not sent: {error}")),
                    }
                }
                continue;
            }
            if options.input_test {
                continue;
            }
            for message in translate(event, picture_size, window.client_size(), &mut held_keys) {
                peer.send_data(message.encode());
                stats.inputs_sent += 1;
            }
        }
        if options.input_test
            && channel_open
            && first_picture_after.is_some()
            && let Some(due) =
                next_test_step.or_else(|| Some(Instant::now() + Duration::from_secs(1)))
        {
            next_test_step = Some(due);
            if Instant::now() >= due && test_step < TEST_POINTS.len() + 2 {
                let event = match test_step {
                    i if i < TEST_POINTS.len() => {
                        let (x, y) = TEST_POINTS[i];
                        InputEvent::MouseMove {
                            x: (x * 65535.0) as u16,
                            y: (y * 65535.0) as u16,
                        }
                    }
                    i => InputEvent::Key {
                        scancode: TEST_SCANCODE,
                        extended: false,
                        down: i == TEST_POINTS.len(),
                    },
                };
                peer.send_data(Message::Input(event).encode());
                stats.inputs_sent += 1;
                test_step += 1;
                next_test_step = Some(due + Duration::from_millis(300));
            }
        }
        if channel_open {
            if let Some(clipboard) = &clipboard
                && let Some(text) = clipboard.changed()
                && clipboard_last_in.as_ref() != Some(&text)
                && text.len() <= MAX_CLIPBOARD_TEXT
            {
                peer.send_data(Message::ClipboardText(text).encode());
                clipboard_out += 1;
            }
            let due = *clipboard_test_at.get_or_insert(Instant::now() + Duration::from_secs(1));
            if options.clipboard_test && clipboard_test_at.is_some() && Instant::now() >= due {
                let text = format!("{CLIPBOARD_TEST_TEXT} (from the viewer)");
                peer.send_data(Message::ClipboardText(text).encode());
                clipboard_out += 1;
                // Sent once: far in the future from now on.
                clipboard_test_at = Some(Instant::now() + Duration::from_secs(1 << 30));
            }
        }
        if channel_open && Instant::now() >= next_ping {
            let sent_us = started.elapsed().as_micros() as u64;
            peer.send_data(
                Message::Ping {
                    id: ping_id,
                    sent_us,
                }
                .encode(),
            );
            ping_id += 1;
            next_ping = Instant::now() + Duration::from_millis(500);
        }

        for message in transfers.take(peer.room(Stream::Files)) {
            peer.send_stream(Stream::Files, message);
        }
        terminals.poll(&peer, lines);
        report_files(transfers.outcomes(), lines);

        let mut event = peer.wait_event(Duration::from_millis(2));
        while let Some(current) = event.take() {
            match current {
                PeerEvent::Connected => connected_after = Some(started.elapsed()),
                PeerEvent::ChannelOpen => {
                    channel_open = true;
                    if !sound_on {
                        peer.send_data(Message::Command(Command::SoundOff).encode());
                    }
                    if privacy_on {
                        peer.send_data(Message::Command(Command::PrivacyOn).encode());
                    }
                    if options.picture.is_some() {
                        peer.send_data(
                            Message::Command(Command::Picture {
                                fps: fps_cap,
                                quality,
                            })
                            .encode(),
                        );
                    }
                    for command in &options.commands {
                        peer.send_data(Message::Command(*command).encode());
                        lines.push(format!("sent command: {command:?}"));
                    }
                }
                PeerEvent::StreamOpen(Stream::Files) => report_files(transfers.opened(), lines),
                PeerEvent::StreamOpen(Stream::Terminal) => {
                    if options.terminal_test {
                        terminals.open(&peer, agent, true, lines);
                    }
                }
                PeerEvent::StreamData(Stream::Terminal, bytes) => {
                    if let Some(message) = TerminalMessage::decode(&bytes) {
                        terminals.on_message(message, &peer, lines);
                    }
                }
                PeerEvent::StreamData(Stream::Files, bytes) => {
                    for reply in transfers.on_message(&bytes) {
                        peer.send_stream(Stream::Files, reply);
                    }
                }
                PeerEvent::Video {
                    data,
                    time_90khz,
                    contiguous,
                    received,
                } => {
                    stats.pickup_ms.push(received.elapsed().as_secs_f64() * 1e3);
                    stats.received += 1;
                    stats.lost += u32::from(!contiguous);
                    stats.bytes += data.len();
                    let decode_start = Instant::now();
                    decoder
                        .decode(&data, time_90khz as i64, &mut decoded)
                        .map_err(|e| format!("decode: {}", describe(e)))?;
                    stats
                        .decode_ms
                        .push(decode_start.elapsed().as_secs_f64() * 1e3);
                    for picture in decoded.drain(..) {
                        stats.decoded += 1;
                        picture_size = (picture.width, picture.height);
                        let show_start = Instant::now();
                        presenter
                            .show(&picture.texture, picture_size, &window)
                            .map_err(|e| format!("show: {}", describe(e)))?;
                        stats.show_ms.push(show_start.elapsed().as_secs_f64() * 1e3);
                        stats.shown += 1;
                        first_picture_after.get_or_insert(started.elapsed());
                        let viewer_ms = received.elapsed().as_secs_f64() * 1e3;
                        stats.viewer_ms.insert(picture.time as u64, viewer_ms);
                    }
                }
                PeerEvent::Data(bytes) => match Message::decode(&bytes) {
                    Some(Message::Pong { sent_us, .. }) => {
                        let now_us = started.elapsed().as_micros() as u64;
                        stats
                            .rtt_ms
                            .push(now_us.saturating_sub(sent_us) as f64 / 1e3);
                    }
                    Some(Message::FrameInfo {
                        time_90khz,
                        agent_ms,
                    }) => {
                        stats.agent_ms.insert(time_90khz, f64::from(agent_ms));
                    }
                    Some(Message::Leave) => {
                        closed = Some("the agent ended the session".to_string());
                        break 'session;
                    }
                    Some(Message::Notice(text)) => {
                        lines.push(format!("notice from that PC: {text}"));
                        status.notice(&text);
                    }
                    Some(Message::ClipboardText(text)) => {
                        clipboard_in += 1;
                        clipboard_test_received |= text.starts_with(CLIPBOARD_TEST_TEXT);
                        if let Some(clipboard) = &clipboard
                            && clipboard_last_in.as_ref() != Some(&text)
                        {
                            clipboard.set_text(text.clone());
                            clipboard_last_in = Some(text);
                        }
                    }
                    _ => {}
                },
                PeerEvent::KeyframeRequested => {}
                PeerEvent::Path(path) => {
                    status.relayed = path.starts_with("relayed");
                    lines.push(format!(
                        "path after {} ms: {path}",
                        started.elapsed().as_millis()
                    ))
                }
                PeerEvent::Note(note) => {
                    lines.push(format!("{} ms: {note}", started.elapsed().as_millis()))
                }
                PeerEvent::Bitrate(_) => {}
                PeerEvent::Audio { data, time_48khz } => player.push(data, time_48khz),
                PeerEvent::Closed(reason) => {
                    closed = Some(reason);
                    break 'session;
                }
            }
            event = peer.try_event();
        }
    }
    // Keys still held would stay down on the remote PC.
    for (scancode, extended) in held_keys.drain() {
        let up = InputEvent::Key {
            scancode,
            extended,
            down: false,
        };
        peer.send_data(Message::Input(up).encode());
    }
    // Tell the agent at once, unless it ended the session itself.
    if closed.as_deref() != Some("the agent ended the session") {
        peer.send_data(Message::Leave.encode());
    }
    std::thread::sleep(Duration::from_millis(100));
    drop(peer);

    let elapsed = started.elapsed().as_secs_f64();
    let after =
        |d: Option<Duration>| d.map_or("never".to_string(), |d| format!("{} ms", d.as_millis()));
    lines.push(format!("present: {}", presenter.description()));
    lines.push(format!(
        "connected after {}, first picture after {}",
        after(connected_after),
        after(first_picture_after)
    ));
    lines.push(format!(
        "{elapsed:.1} s: {} frames received ({} after a loss, {:.2} MB), {} decoded, {} shown",
        stats.received,
        stats.lost,
        stats.bytes as f64 / 1e6,
        stats.decoded,
        stats.shown
    ));
    lines.push(format!("decode, ms: {}", summary(&mut stats.decode_ms)));
    lines.push(format!(
        "network round trip, ms: {}",
        summary(&mut stats.rtt_ms.clone())
    ));
    let half_rtt = median(&mut stats.rtt_ms) / 2.0;
    let mut viewer: Vec<f64> = stats.viewer_ms.values().copied().collect();
    lines.push(format!("arrival -> shown, ms: {}", summary(&mut viewer)));
    let mut agent: Vec<f64> = stats.agent_ms.values().copied().collect();
    lines.push(format!(
        "agent: screen change -> sent, ms: {}",
        summary(&mut agent)
    ));
    let mut total: Vec<f64> = stats
        .viewer_ms
        .iter()
        .filter_map(|(time, viewer_ms)| {
            stats
                .agent_ms
                .get(time)
                .map(|agent_ms| agent_ms + half_rtt + viewer_ms)
        })
        .collect();
    lines.push(format!(
        "screen change -> shown (agent + half round trip + viewer), ms: {}",
        summary(&mut total)
    ));
    lines.push(format!("input events sent: {}", stats.inputs_sent));
    lines.push(format!("window title: {}", status.title));
    if options.terminal_test {
        lines.push(terminals.test_result());
    }
    drop(terminals);
    if window.is_full_screen()
        && let Some(area) = window.monitor_rect()
    {
        let (width, height) = window.client_size();
        lines.push(format!(
            "window: full screen, drawing area {width}x{height}, monitor {}x{}",
            area.right - area.left,
            area.bottom - area.top
        ));
    }
    lines.extend(player.notes());
    let sound = player.stats();
    lines.push(format!(
        "sound: to {}: {} packets ({:.0} kB) received, {} frames played, {} concealed, {} dropped",
        if sound.device.is_empty() {
            "no device"
        } else {
            &sound.device
        },
        sound.received,
        sound.bytes as f64 / 1e3,
        sound.played,
        sound.concealed,
        sound.dropped
    ));
    if options.sound_test {
        // At least a second of sound, and nearly all of it the tone.
        let passed = sound.loud_frames >= 100 && sound.tone_frames * 10 >= sound.loud_frames * 9;
        lines.push(format!(
            "sound test: {} (the {TEST_TONE_HZ} Hz tone in {} of {} frames with sound)",
            if passed { "PASS" } else { "FAIL" },
            sound.tone_frames,
            sound.loud_frames
        ));
    }
    if transfers.busy() {
        lines.push("files: a transfer was cut off when the session ended".into());
    }
    lines.push(format!(
        "clipboard: {clipboard_in} received, {clipboard_out} sent; test text from the other side received: {}",
        if clipboard_test_received { "yes" } else { "no" }
    ));
    for (stage, values) in [
        ("window messages", &mut stats.pump_ms),
        ("frame arrival -> picked up", &mut stats.pickup_ms),
        ("present", &mut stats.show_ms),
    ] {
        let slow = values.iter().filter(|&&v| v > 50.0).count();
        lines.push(format!(
            "loop, {stage}, ms: {} ({slow} over 50 ms)",
            summary(values)
        ));
    }
    lines.push(format!(
        "ended: {}",
        closed.unwrap_or_else(|| "time is up".into())
    ));
    let test_done = !options.input_test || test_step == TEST_POINTS.len() + 2;
    Ok(connected_after.is_some()
        && first_picture_after.is_some()
        && stats.shown == stats.decoded
        && test_done)
}

/// Window input -> protocol messages. Pointer positions are mapped from the picture's place in the window
/// to the remote screen (0..=65535); keys are tracked so they can be released when focus is lost.
fn translate(
    event: WindowEvent,
    picture: (u32, u32),
    window: (u32, u32),
    held: &mut HashSet<(u16, bool)>,
) -> Vec<Message> {
    let input = match event {
        WindowEvent::MouseMove { x, y } => {
            if picture.0 == 0 || picture.1 == 0 {
                return Vec::new();
            }
            let area = fit(picture, window);
            let scale = |value: i32, start: i32, end: i32| {
                let span = i64::from((end - start - 1).max(1));
                (i64::from(value - start) * 65535 / span).clamp(0, 65535) as u16
            };
            InputEvent::MouseMove {
                x: scale(x, area.left, area.right),
                y: scale(y, area.top, area.bottom),
            }
        }
        WindowEvent::MouseButton { button, down } => {
            let button = match button {
                0 => MouseButton::Left,
                1 => MouseButton::Right,
                2 => MouseButton::Middle,
                3 => MouseButton::X1,
                _ => MouseButton::X2,
            };
            InputEvent::MouseButton { button, down }
        }
        WindowEvent::Wheel { delta, horizontal } => InputEvent::Wheel { delta, horizontal },
        WindowEvent::Key {
            scancode,
            extended,
            down,
        } => {
            if down {
                held.insert((scancode, extended));
            } else {
                held.remove(&(scancode, extended));
            }
            InputEvent::Key {
                scancode,
                extended,
                down,
            }
        }
        WindowEvent::FilesDropped(_) | WindowEvent::Menu(_) => return Vec::new(),
        WindowEvent::FocusLost => {
            return held
                .drain()
                .map(|(scancode, extended)| {
                    Message::Input(InputEvent::Key {
                        scancode,
                        extended,
                        down: false,
                    })
                })
                .collect();
        }
    };
    vec![Message::Input(input)]
}

/// The command lines open on that PC, each in a console window here.
#[derive(Default)]
struct TerminalWindows {
    windows: HashMap<u32, TerminalWindow>,
    next_id: u32,
    /// Test: (the shell, its output so far, the test text seen, the exit code, the window size).
    test: Option<TerminalTest>,
}

#[derive(Default)]
struct TerminalTest {
    shell: String,
    output: Vec<u8>,
    seen: bool,
    exit_code: Option<u32>,
    size: Option<(u16, u16)>,
}

/// What the terminal test types, by shell; its output differs from what is typed, so seeing it means the
/// command ran.
fn terminal_test_input(shell: &str) -> &'static str {
    if shell == "PowerShell" {
        "Write-Output ('queqiao' + '-terminal-ok')\r"
    } else {
        "echo queqiao^-terminal-ok\r"
    }
}

impl TerminalWindows {
    fn open(&mut self, peer: &Peer, agent: &str, test: bool, lines: &mut Vec<String>) {
        self.next_id += 1;
        let id = self.next_id;
        let program = match std::env::current_exe() {
            Ok(program) => program,
            Err(error) => {
                lines.push(format!("command line: no window ({error})"));
                return;
            }
        };
        match TerminalWindow::open(&program, agent, test) {
            Ok(window) => {
                self.windows.insert(id, window);
                if test {
                    self.test = Some(TerminalTest::default());
                }
                let (cols, rows) = TERMINAL_SIZE;
                let open = TerminalMessage::Open { id, cols, rows };
                peer.send_stream(Stream::Terminal, open.encode());
            }
            Err(error) => lines.push(format!("command line: no window ({error})")),
        }
    }

    fn on_message(&mut self, message: TerminalMessage, peer: &Peer, lines: &mut Vec<String>) {
        match message {
            TerminalMessage::Opened { id, shell } => {
                lines.push(format!("command line {id} opened ({shell})"));
                if let Some(test) = &mut self.test {
                    test.shell = shell.clone();
                    let input = terminal_test_input(&shell).as_bytes().to_vec();
                    peer.send_stream(
                        Stream::Terminal,
                        TerminalMessage::Input { id, data: input }.encode(),
                    );
                }
            }
            TerminalMessage::Output { id, data } => {
                if let Some(window) = self.windows.get(&id) {
                    window.output(&data);
                }
                if let Some(test) = &mut self.test {
                    test.output.extend_from_slice(&data);
                    if !test.seen
                        && String::from_utf8_lossy(&test.output).contains("queqiao-terminal-ok")
                    {
                        test.seen = true;
                        let exit = TerminalMessage::Input {
                            id,
                            data: b"exit\r".to_vec(),
                        };
                        peer.send_stream(Stream::Terminal, exit.encode());
                    }
                }
            }
            TerminalMessage::Exited { id, code } => {
                lines.push(format!("command line {id} ended with exit code {code}"));
                if let Some(window) = self.windows.get_mut(&id) {
                    window.end(&trf(
                        "The command line ended (exit code {code}).",
                        &[("code", &code.to_string())],
                    ));
                }
                if let Some(test) = &mut self.test {
                    test.exit_code = Some(code);
                }
            }
            TerminalMessage::Failed { id, reason } => {
                lines.push(format!("command line {id} failed: {reason}"));
                if let Some(window) = self.windows.get_mut(&id) {
                    window.end(&reason);
                }
            }
            _ => {}
        }
    }

    /// Keys and size changes from the windows; a closed window ends its command line.
    fn poll(&mut self, peer: &Peer, lines: &mut Vec<String>) {
        let mut closed = Vec::new();
        for (&id, window) in &self.windows {
            while let Some(event) = window.try_event() {
                let message = match event {
                    TerminalEvent::Input(data) => TerminalMessage::Input { id, data },
                    TerminalEvent::Resize(cols, rows) => {
                        if let Some(test) = &mut self.test {
                            test.size = Some((cols, rows));
                        }
                        TerminalMessage::Resize { id, cols, rows }
                    }
                    TerminalEvent::Closed => {
                        closed.push(id);
                        TerminalMessage::Close { id }
                    }
                };
                peer.send_stream(Stream::Terminal, message.encode());
            }
        }
        for id in closed {
            // In the test the window stays known, to read its helper's exit code at the end.
            if self.test.is_none() {
                self.windows.remove(&id);
                lines.push(format!("command line {id}: window closed"));
            }
        }
    }

    /// "terminal test: PASS (...)": the command ran, ended with 0, and the window process came up, got the
    /// output and closed by itself with 0.
    fn test_result(&self) -> String {
        let Some(test) = &self.test else {
            return "terminal test: FAIL (no command line was opened)".into();
        };
        let helper = self.windows.values().next().and_then(|window| {
            let until = Instant::now() + Duration::from_secs(5);
            loop {
                if let Some(code) = window.helper_exit_code() {
                    return Some(code);
                }
                if Instant::now() > until {
                    return None;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        });
        let passed =
            test.seen && test.exit_code == Some(0) && test.size.is_some() && helper == Some(0);
        format!(
            "terminal test: {} (shell {}, test output seen: {}, exit code {:?}, window size {:?}, window process exit code {:?})",
            if passed { "PASS" } else { "FAIL" },
            test.shell,
            test.seen,
            test.exit_code,
            test.size,
            helper
        )
    }
}

/// One report line per finished or failed file.
fn report_files(outcomes: Vec<Outcome>, lines: &mut Vec<String>) {
    for outcome in outcomes {
        lines.push(match outcome {
            Outcome::Sent { name, size } => {
                format!("file sent: {name} ({size} bytes, checked by the agent)")
            }
            Outcome::Received { path, size } => {
                format!(
                    "file received: {} ({size} bytes, SHA-256 checked)",
                    path.display()
                )
            }
            Outcome::Failed { name, reason } => format!("file failed: {name}: {reason}"),
        });
    }
}

/// The GPU's D3D11 device with video support; Microsoft's software adapter (WARP) without a usable GPU
/// or when forced (QUEQIAO_FORCE=adapter:warp).
fn create_device() -> Result<(ID3D11Device, ID3D11DeviceContext, String), String> {
    let forced_warp = queqiao_core::forced("adapter").as_deref() == Some("warp");
    let video = D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT;
    let attempts: &[(D3D_DRIVER_TYPE, D3D11_CREATE_DEVICE_FLAG)] = if forced_warp {
        &[(D3D_DRIVER_TYPE_WARP, D3D11_CREATE_DEVICE_BGRA_SUPPORT)]
    } else {
        &[
            (D3D_DRIVER_TYPE_HARDWARE, video),
            (D3D_DRIVER_TYPE_WARP, D3D11_CREATE_DEVICE_BGRA_SUPPORT),
        ]
    };
    let mut failures = Vec::new();
    for &(driver, flags) in attempts {
        let mut device = None;
        let mut context = None;
        let created = unsafe {
            D3D11CreateDevice(
                None::<&IDXGIAdapter>,
                driver,
                HMODULE::default(),
                flags,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )
        };
        match (created, device, context) {
            (Ok(()), Some(device), Some(context)) => {
                let name = adapter_name(&device);
                return Ok((device, context, name));
            }
            (Err(error), _, _) => failures.push(describe(error)),
            _ => failures.push("nothing returned".into()),
        }
    }
    Err(format!("no D3D11 device: {}", failures.join("; ")))
}

fn adapter_name(device: &ID3D11Device) -> String {
    device
        .cast::<IDXGIDevice>()
        .and_then(|d| unsafe { d.GetAdapter() })
        .and_then(|a| unsafe { a.GetDesc() })
        .map(|desc| {
            let text = &desc.Description;
            let len = text.iter().position(|&c| c == 0).unwrap_or(text.len());
            String::from_utf16_lossy(&text[..len])
        })
        .unwrap_or_else(|_| "unknown".into())
}

fn median(values: &mut [f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(f64::total_cmp);
    values[values.len() / 2]
}

fn summary(values: &mut [f64]) -> String {
    values.sort_by(f64::total_cmp);
    let at = |q: f64| {
        values
            .get(((values.len().max(1) - 1) as f64 * q).round() as usize)
            .copied()
            .unwrap_or(f64::NAN)
    };
    format!(
        "median {:.2}, p95 {:.2}, max {:.2} (n={})",
        at(0.5),
        at(0.95),
        at(1.0),
        values.len()
    )
}

fn describe(error: windows::core::Error) -> String {
    format!("0x{:08X} {}", error.code().0, error.message())
}
