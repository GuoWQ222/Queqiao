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

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use queqiao_account::{
    Account, AccountEvent, ClientMessage, Device, DeviceIdentity, DeviceNotes, Role as AccountRole,
    SavedLogin, new_session_id,
};
use queqiao_codec::{DecodedFrame, DecoderChoice, H264Decoder};
use queqiao_core::protocol::{
    Command, InputEvent, MAX_CLIPBOARD_TEXT, Message, MouseButton, Quality,
};
use queqiao_core::text::{tr, trf};
use queqiao_display::{
    HelpAction, HelpForm, PICK_CLOSED, PickList, ask_secret, ask_text, confirm, popup_menu,
};

/// The session window's menu items for the controlled PC (system-menu ids: multiples of 16).
const MENU_CTRL_ALT_DEL: u16 = 0x1000;
const MENU_LOCK: u16 = 0x1010;
const MENU_RESTART: u16 = 0x1020;
const MENU_SOUND: u16 = 0x1030;
const MENU_FULL_SCREEN: u16 = 0x1040;
const MENU_PRIVACY: u16 = 0x1050;
const MENU_TERMINAL: u16 = 0x1060;
const MENU_GAME_MOUSE: u16 = 0x1070;
const MENU_MUTE_SPEAKERS: u16 = 0x1080;
const MENU_SHORTCUTS: u16 = 0x1090;
const MENU_ON_TOP: u16 = 0x10A0;
const MENU_CUSTOM_BITRATE: u16 = 0x10B0;
const MENU_TRANSFERS: u16 = 0x10C0;
const MENU_RESTART_QUEQIAO: u16 = 0x10D0;
const MENU_COMMAND_LINES: u16 = 0x10E0;
const MENU_SEND_FILES: u16 = 0x10F0;
// System-menu command ids are multiples of 16: Windows reports them masked with 0xFFF0.
const MENU_ACTUAL_SIZE: u16 = 0x1640;
const MENU_HOTKEYS: u16 = 0x1650;
const MENU_POINTER_SHAPE: u16 = 0x1660;
const MENU_MICROPHONE: u16 = 0x16D0;
/// The card of the session's figures at the bottom right (also on the bar).
const MENU_FIGURES: u16 = 0x16E0;
/// The picture's codec: (menu id, preference, label).
const MENU_CODEC: [(u16, queqiao_core::protocol::CodecPreference, &str); 3] = [
    (
        0x16A0,
        queqiao_core::protocol::CodecPreference::Automatic,
        "Automatic",
    ),
    (
        0x16B0,
        queqiao_core::protocol::CodecPreference::H264,
        "H.264",
    ),
    (
        0x16C0,
        queqiao_core::protocol::CodecPreference::Av1,
        "AV1 (both PCs need it)",
    ),
];
/// The game mouse's speed: (menu id, percent, label).
const MENU_MOUSE_SPEED: [(u16, u32, &str); 3] = [
    (0x1670, 50, "Slow (x0.5)"),
    (0x1680, 100, "Normal"),
    (0x1690, 200, "Fast (x2)"),
];
/// The window's opacity: (menu id, percent, label).
const MENU_OPACITY: [(u16, u8, &str); 4] = [
    (0x1600, 100, "Opaque"),
    (0x1610, 80, "80 %"),
    (0x1620, 60, "60 %"),
    (0x1630, 40, "40 %"),
];
/// The boss key's id with Windows.
const BOSS_HOTKEY: i32 = 1;
/// The transfer list's buttons.
const TRANSFER_PAUSE: u16 = 301;
const TRANSFER_CANCEL: u16 = 302;
/// The other PC's screens: menu ids from here, 16 apart.
const MENU_SCREENS: u16 = 0x1400;
/// Resolution: that screen's own, or its short side at most 1080 or 720 pixels.
const MENU_RESOLUTION: [(u16, u16, &str); 3] = [
    (0x1300, 0, "As that screen"),
    (0x1310, 1080, "1080p"),
    (0x1320, 720, "720p"),
];
/// The game-mouse test's relative moves (mouse counts).
const TEST_DELTAS: [(i16, i16); 4] = [(40, 0), (0, 40), (-40, 0), (0, -40)];
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
use queqiao_audio::{Player, TEST_TONE_HZ};
use queqiao_files::{ImageReceiver, ImageSender, image_fingerprint, test_picture};
use queqiao_files::{Outcome, TransferRow, Transfers, downloads_folder};
use queqiao_input::{ClipContent, Clipboard};

mod hotkeys;

/// The start of the clipboard text the tests send (each side checks it got the other's).
const CLIPBOARD_TEST_TEXT: &str = "Queqiao clipboard test";
use queqiao_display::{
    DeviceList, DeviceRow, ListAction, LoginAction, LoginFields, LoginForm, Presenter, Window,
    WindowEvent, fit, show_error,
};
use queqiao_terminal::{TerminalMessage, TerminalWindow, WindowEvent as TerminalEvent};
use queqiao_transport::{Addresses, Peer, PeerEvent, Role, Signal, Signaling, Stream};
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

/// What the main window asks the session to do as soon as it is up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Then {
    /// The file picker: the files chosen go to that PC.
    Files,
    /// A command line on that PC.
    Terminal,
}

#[derive(Default, Clone)]
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
    /// With --open: what to do as soon as the session is up (the main window's "Send files" and "Command line").
    then: Option<Then>,
    /// With --device: a second device of the account at the same time (tests: several sessions at once).
    also_device: Option<String>,
    list: bool,
    logout: bool,
    ui_test: Option<String>,
    seconds: Option<u64>,
    width: Option<i32>,
    report: Option<PathBuf>,
    input_test: bool,
    /// Tests: the card of figures on and the bar kept out from the start.
    figures: bool,
    /// Tests: a picture of the session window with its bar and card (viewer_session.bmp) into this folder.
    ui_pictures: Option<PathBuf>,
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
    /// After the input test, relative moves (game mouse) that the agent checks.
    game_mouse_test: bool,
    /// Mute that PC's speakers from the start.
    mute_speakers: bool,
    /// Cancel a file this side sends once its first piece went out (tests).
    cancel_test: bool,
    /// Tests of the sign-in screens through the service: Ctrl+Alt+Del 3 s after the data channel opens, Escape
    /// (which closes that screen) 2 s later; the pictures shown meanwhile and after are reported.
    secure_test: bool,
    /// Send this file as a clipboard copy (tests: the other side must put it on its clipboard).
    clipboard_files_test: Option<PathBuf>,
    /// The picture's short side at most this (720, 1080) from the start; 0 or none: the screen's own.
    resolution: Option<u16>,
    /// Show that PC's screen n (from 0) from the start.
    screen: Option<u8>,
    /// A bitrate ceiling (kbit/s) to send at the start (tests; people use "Custom bitrate...").
    max_bitrate: Option<u32>,
    /// Picture settings to send at the start: frames per second at most (0: the screen's) and quality.
    picture: Option<(u8, Quality)>,
    /// Help someone of another account: their help ID and code (tests; people use "Help someone...").
    help_id: Option<String>,
    help_code: Option<String>,
    /// A help invitation link (queqiao://help/<help ID>/<code>): the help form, filled in from it.
    invite: Option<String>,
    /// --help-connect: the help form, filled in from the link, connects at once (the main window's Help page).
    help_go: bool,
    /// Tests: take this device (name or id) out of the account, then leave.
    remove_device: Option<String>,
    /// Tests: the passwords given (in turn) when the PC asks for its Windows password.
    test_passwords: Vec<String>,
    /// Press Connect in the help form once it is drawn (tests).
    test_submit_help: bool,
    /// With --user and --password: a new account (on a server that takes them) instead of a login.
    signup: bool,
    /// A throwaway device with this name instead of this PC's own (tests); its login is not kept.
    test_device: Option<String>,
    /// The picture's codec to ask for (as the Picture codec menu does): auto, h264 or av1.
    codec: Option<queqiao_core::protocol::CodecPreference>,
    /// Send a test tone as this PC's microphone from the start (tests).
    mic_test: bool,
    /// The session window's opacity in percent (as the Opacity menu sets it).
    opacity: Option<u8>,
    /// Boss key test: Ctrl+Alt+F11 for this run, pressed (as Windows would report it) to hide and show the window.
    boss_key_test: bool,
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
    /// Frames before the first picture that were not keyframes (each asks the agent for one).
    waiting_for_keyframe: u32,
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

    /// Once a second: the title anew; the frames per second and the data rate, for the card of figures.
    fn update(&mut self, window: &Window, stats: &Stats) -> Option<(f64, String)> {
        let now = Instant::now();
        let (at, shown, bytes) = self.last;
        let seconds = now.duration_since(at).as_secs_f64();
        if seconds < 1.0 {
            return None;
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
        Some((fps, rate))
    }
}

/// Delay samples kept for a session's report at most (a long session, or a side that floods them).
const MAX_SAMPLES: usize = 200_000;

fn main() {
    queqiao_core::platform::install_crash_log("queqiao-viewer");
    #[cfg(feature = "test-switches")]
    if std::env::args().any(|arg| arg == "--crash-test") {
        panic!("crash test (asked for with --crash-test)");
    }
    // A terminal window (started by a session of this viewer): it runs until its command line ends.
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("--terminal-pipe")
        && let Some(pipe) = args.get(2)
    {
        let title = match args.iter().position(|arg| arg == "--terminal-title") {
            Some(at) => args.get(at + 1).cloned().unwrap_or_default(),
            None => String::new(),
        };
        let test =
            cfg!(feature = "test-switches") && args.iter().any(|arg| arg == "--terminal-test");
        std::process::exit(queqiao_terminal::run_helper(pipe, &title, test));
    }
    let banner = queqiao_core::banner(env!("CARGO_PKG_NAME"));
    let parsed = match args.get(1).map(String::as_str) {
        Some("--invite") => Some(invitation_options(&args[2..])),
        _ => parse(args.iter().skip(1).cloned()),
    };
    let Some(options) = parsed else {
        println!("{banner}");
        if cfg!(feature = "test-switches") {
            println!(
                "usage: queqiao-viewer [--server <host:port> --pin <fingerprint> --user <name> --password <password>]"
            );
            println!(
                "                      [--device <name or id> | --list | --logout | --ui-test <name or id>]"
            );
            println!("       queqiao-viewer --listen <ip:port> | --connect <agent-ip:port>");
            println!("       common: [--seconds N] [--width N] [--report <file>] [--input-test]");
        } else {
            println!(
                "usage: queqiao-viewer (the device list) | --invite <queqiao: link> | --open <device> | --help-connect <link>"
            );
        }
        // --version is answered by the banner; any other option is refused.
        let asked_version = args.get(1).map(String::as_str) == Some("--version");
        std::process::exit(if asked_version { 0 } else { 2 });
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

/// A queqiao: link: Windows starts the viewer with `--invite "<link>"` (the handler the installer registers). Only
/// the one value after `--invite` is read, nothing else: a link with a `"` in it cannot add options of its own (the
/// security review of 2026-11-22 found links that could otherwise log in to another server, send files or write
/// files). Anything else is kept as an invalid link, so the person is told it is not a Queqiao help link.
fn invitation_options(rest: &[String]) -> Options {
    let link = match rest {
        [link] => link.clone(),
        _ => rest.join(" "),
    };
    Options {
        invite: Some(link),
        ..Options::default()
    }
}

/// The command line. A shipping build reads only `--force`, which only chooses among the media paths (the device
/// list needs no options, a queqiao: link goes through `invitation_options`). Every other option is for the tests
/// and exists only in builds with the `test-switches` feature (`cargo build-test`).
fn parse(mut args: impl Iterator<Item = String>) -> Option<Options> {
    // Only the test options change `options`.
    #[cfg_attr(not(feature = "test-switches"), allow(unused_mut))]
    let mut options = Options::default();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--force" => queqiao_core::set_forced(&args.next()?),
            // The main window (the agent's): a device of the account, or helping someone with their help ID and
            // code (as a link). Each does what a click there does.
            "--open" => options.device = Some(args.next()?),
            "--then" => {
                options.then = Some(match args.next()?.as_str() {
                    "files" => Then::Files,
                    "terminal" => Then::Terminal,
                    _ => return None,
                })
            }
            "--help-connect" => {
                options.invite = Some(args.next()?);
                options.help_go = true;
            }
            _ => {
                #[cfg(feature = "test-switches")]
                if test_switch(&mut options, &arg, &mut args)? {
                    continue;
                }
                return None;
            }
        }
    }
    Some(options)
}

/// The options for the tests (builds with the `test-switches` feature only): whether `arg` is one.
#[cfg(feature = "test-switches")]
fn test_switch(
    options: &mut Options,
    arg: &str,
    args: &mut impl Iterator<Item = String>,
) -> Option<bool> {
    use queqiao_display::normalize_code;
    match arg {
        "--connect" => options.connect = args.next(),
        "--listen" => options.listen = args.next(),
        "--server" => options.server = args.next(),
        "--pin" => options.pin = args.next(),
        "--user" => options.user = args.next(),
        "--password" => options.password = args.next(),
        "--device" => options.device = args.next(),
        "--also-device" => options.also_device = args.next(),
        "--list" => options.list = true,
        "--logout" => options.logout = true,
        "--ui-test" => options.ui_test = args.next(),
        "--seconds" => options.seconds = args.next().and_then(|v| v.parse().ok()),
        "--width" => options.width = args.next().and_then(|v| v.parse().ok()),
        "--report" => options.report = args.next().map(PathBuf::from),
        "--input-test" => options.input_test = true,
        "--figures" => options.figures = true,
        "--ui-pictures" => options.ui_pictures = Some(PathBuf::from(args.next()?)),
        "--clipboard-test" => options.clipboard_test = true,
        "--send-file" => options.send_files.push(PathBuf::from(args.next()?)),
        "--sound-device" => options.sound_device = Some(args.next()?),
        "--sound-test" => options.sound_test = true,
        "--mic-test" => options.mic_test = true,
        "--mute" => options.mute = true,
        "--codec" => {
            use queqiao_core::protocol::CodecPreference;
            options.codec = Some(match args.next()?.as_str() {
                "h264" => CodecPreference::H264,
                "av1" => CodecPreference::Av1,
                _ => CodecPreference::Automatic,
            });
        }
        "--full-screen" => options.full_screen = true,
        "--privacy" => options.privacy = true,
        "--terminal-test" => options.terminal_test = true,
        "--game-mouse-test" => options.game_mouse_test = true,
        "--mute-speakers" => options.mute_speakers = true,
        "--cancel-test" => options.cancel_test = true,
        "--secure-test" => options.secure_test = true,
        "--clipboard-files-test" => {
            options.clipboard_files_test = Some(PathBuf::from(args.next()?))
        }
        "--resolution" => options.resolution = Some(args.next()?.parse().ok()?),
        "--screen" => options.screen = Some(args.next()?.parse().ok()?),
        "--max-bitrate" => options.max_bitrate = Some(args.next()?.parse().ok()?),
        "--help-id" => options.help_id = Some(args.next()?),
        "--help-code" => options.help_code = Some(normalize_code(&args.next()?)),
        "--invite" => options.invite = Some(args.next()?),
        "--test-submit-help" => options.test_submit_help = true,
        "--signup" => options.signup = true,
        "--test-device" => options.test_device = Some(args.next()?),
        "--opacity" => options.opacity = args.next().and_then(|v| v.parse().ok()),
        "--boss-key-test" => options.boss_key_test = true,
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
            "restart-queqiao" => Command::RestartQueqiao,
            _ => return None,
        }),
        "--trust-new-key" => options.trust_new_key = args.next(),
        "--remove-device" => options.remove_device = Some(args.next()?),
        "--test-password" => {
            options.test_passwords = args.next()?.split(',').map(str::to_string).collect()
        }
        _ => return Some(false),
    }
    Some(true)
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
        Addresses::All,
        lines,
    )
}

fn run_account(options: &Options, lines: &mut Vec<String>) -> Result<bool, String> {
    let identity = match &options.test_device {
        Some(label) => DeviceIdentity::in_memory(label),
        None => DeviceIdentity::load_or_create(),
    }
    .map_err(|e| e.to_string())?;
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
    let pictures = (options.ui_test.is_some() || options.test_submit_help).then(|| {
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
    if options.test_device.is_none() {
        login.save(AccountRole::Viewer).map_err(|e| e.to_string())?;
    }
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
        if options.test_device.is_none() {
            login.save(AccountRole::Viewer).map_err(|e| e.to_string())?;
        }
        lines.push("logged out: the saved login is deleted here and revoked on the server".into());
        return Ok(true);
    }
    if let Some(key) = &options.remove_device {
        let found = account
            .devices
            .iter()
            .find(|d| d.id == *key || d.name.eq_ignore_ascii_case(key))
            .map(|d| d.id.clone())
            .ok_or_else(|| format!("{key} is not on the account"))?;
        account.remove_device(&found);
        std::thread::sleep(Duration::from_millis(800));
        lines.push(format!("removed {found} from the account"));
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
    if let (Some(first), Some(second)) = (&options.device, &options.also_device) {
        // Several sessions at once: each on a thread of its own, as from the device list.
        let mut running = Vec::new();
        for key in [first, second] {
            let target = wait_for_device(&mut account, key, Duration::from_secs(30))?;
            let (peer, started) = start_peer(&mut account, &target, Addresses::All)?;
            running.push((
                target.name.clone(),
                session_thread(
                    options,
                    peer,
                    started,
                    target.id.clone(),
                    target.name.clone(),
                ),
            ));
        }
        let mut passed = true;
        for (at, (name, thread)) in running.into_iter().enumerate() {
            let (result, session_lines) = thread
                .join()
                .unwrap_or_else(|_| (Err("the session's thread failed".into()), Vec::new()));
            lines.extend(
                session_lines
                    .into_iter()
                    .map(|line| format!("{}| {line}", at + 1)),
            );
            match result {
                Ok(ok) => passed &= ok,
                Err(error) => {
                    passed = false;
                    lines.push(format!("{}| session with {name} failed: {error}", at + 1));
                }
            }
        }
        lines.push(format!("sessions at once: 2, all passed: {passed}"));
        return Ok(passed);
    }
    if let Some(key) = &options.device {
        let target = wait_for_device(&mut account, key, Duration::from_secs(30))?;
        return connect_to(
            &mut account,
            &target,
            options,
            &device,
            &context,
            Addresses::All,
            lines,
        );
    }
    if let Some(invite) = &options.invite {
        let prefill = queqiao_core::invitation::parse(invite)
            .ok_or_else(|| format!("not a Queqiao help link: {invite}"))?;
        lines.push(format!(
            "help: from the invitation link, help ID {}",
            prefill.0
        ));
        let Some(target) = ask_for_help_target(
            &mut account,
            Some(prefill),
            options,
            pictures.as_deref(),
            lines,
        ) else {
            lines.push("help: the form was closed".into());
            return Ok(!options.test_submit_help);
        };
        // A PC of another account: this PC's other addresses stay unknown to it.
        return connect_to(
            &mut account,
            &target,
            options,
            &device,
            &context,
            Addresses::RouteOnly,
            lines,
        );
    }
    if let Some(help_id) = &options.help_id {
        // Without --help-code: ask the person there to allow it.
        let code = options.help_code.clone().unwrap_or_default();
        let target = account
            .request_help(help_id, &code)
            .map_err(|e| e.to_string())?;
        lines.push(format!("help granted: {} ({})", target.name, target.id));
        // A PC of another account: this PC's other addresses stay unknown to it.
        return connect_to(
            &mut account,
            &target,
            options,
            &device,
            &context,
            Addresses::RouteOnly,
            lines,
        );
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
        if let Some((user, password)) = given {
            let account = if options.signup {
                Account::register(login, identity, AccountRole::Viewer, user, password)
            } else {
                Account::connect(login, identity, AccountRole::Viewer, Some((user, password)))
            };
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
            Some(action @ (LoginAction::Submit(_) | LoginAction::Register(_))) => {
                let register = matches!(action, LoginAction::Register(_));
                let (LoginAction::Submit(fields) | LoginAction::Register(fields)) = action else {
                    continue;
                };
                if [&fields.server, &fields.pin, &fields.user, &fields.password]
                    .iter()
                    .any(|value| value.is_empty())
                {
                    form.set_status(tr("Please fill in all four fields."));
                    continue;
                }
                // A new account: the password typed twice.
                if register
                    && options.ui_test.is_none()
                    && ask_secret(tr("Queqiao"), tr("The same password again:")).as_deref()
                        != Some(fields.password.as_str())
                {
                    form.set_status(tr("The two passwords are not the same."));
                    continue;
                }
                if login.server != fields.server {
                    login.refresh_token = None;
                }
                login.server = fields.server.clone();
                login.pin = fields.pin.clone();
                form.set_busy(
                    true,
                    if register {
                        tr("Creating the account...")
                    } else {
                        tr("Logging in...")
                    },
                );
                // In the background, so the form keeps responding.
                let (mut attempt, identity) = (login.clone(), identity.clone());
                let worker = std::thread::spawn(move || {
                    let result = if register {
                        Account::register(
                            &mut attempt,
                            &identity,
                            AccountRole::Viewer,
                            &fields.user,
                            &fields.password,
                        )
                    } else {
                        Account::connect(
                            &mut attempt,
                            &identity,
                            AccountRole::Viewer,
                            Some((&fields.user, &fields.password)),
                        )
                    };
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
    // The person's own names and favourites for the devices (kept on this PC only).
    let mut notes = DeviceNotes::load();
    list.set_rows(rows(&account.devices, &account.device_id, &notes));
    let (mut sessions, mut passed) = (0u32, true);
    let mut reconnect_at: Option<Instant> = None;
    let mut test_picked = false;
    // Sessions on their own threads: (the device's name, the thread).
    type Running = (
        String,
        std::thread::JoinHandle<(Result<bool, String>, Vec<String>)>,
    );
    let mut running: Vec<Running> = Vec::new();
    let test_deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match list.next(15) {
            Some(ListAction::Closed) => break,
            Some(ListAction::More(id)) => {
                let name = account
                    .devices
                    .iter()
                    .find(|d| d.id == id)
                    .map_or(id.clone(), |d| d.name.clone());
                let favourite = notes.get(&id).favourite;
                let items = [
                    (1u16, tr("Rename..."), false),
                    (2, tr("Favourite"), favourite),
                    (3, tr("Remove from the account..."), false),
                ];
                match popup_menu(list.hwnd(), &items) {
                    Some(1) => {
                        let current = notes.get(&id).remark;
                        if let Some(remark) = ask_text(
                            tr("Queqiao"),
                            &trf("A name of your own for {name}:", &[("name", &name)]),
                            &current,
                        ) {
                            let _ = notes.set_remark(&id, &remark);
                        }
                    }
                    Some(2) => {
                        let _ = notes.set_favourite(&id, !favourite);
                    }
                    Some(3) => {
                        let question = trf(
                            "Remove {name} from the account? Its saved login ends; it needs the password again to come back.",
                            &[("name", &name)],
                        );
                        if confirm(list.hwnd(), tr("Queqiao"), &question) {
                            account.remove_device(&id);
                        }
                    }
                    _ => {}
                }
                list.set_rows(rows(&account.devices, &account.device_id, &notes));
            }
            Some(ListAction::Wake(id)) => {
                // The server's answer (who sends the packet, or why not) comes as a notice or error.
                account.send(ClientMessage::Wake { device_id: id });
            }
            Some(ListAction::Help) => {
                list.set_enabled(false);
                if let Some(target) = ask_for_help_target(account, None, options, pictures, lines) {
                    sessions += 1;
                    match connect_to(
                        account,
                        &target,
                        options,
                        device,
                        context,
                        Addresses::RouteOnly,
                        lines,
                    ) {
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
                if options.test_device.is_none() {
                    login.save(AccountRole::Viewer).map_err(|e| e.to_string())?;
                }
                lines.push("logged out".into());
                break;
            }
            Some(ListAction::Connect(id)) => {
                match account.devices.iter().find(|d| d.id == id).cloned() {
                    Some(target) if target.controllable && options.ui_test.is_none() => {
                        // On a thread of its own: the list stays usable, for more sessions at once.
                        match start_peer(account, &target, Addresses::All) {
                            Ok((peer, started)) => {
                                list.set_status(&trf(
                                    "Connected to {name}.",
                                    &[("name", &target.name)],
                                ));
                                running.push((
                                    target.name.clone(),
                                    session_thread(
                                        options,
                                        peer,
                                        started,
                                        target.id.clone(),
                                        target.name.clone(),
                                    ),
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
                    }
                    Some(target) if target.controllable => {
                        list.set_enabled(false);
                        list.set_status(&trf("Connected to {name}.", &[("name", &target.name)]));
                        let result = connect_to(
                            account,
                            &target,
                            options,
                            device,
                            context,
                            Addresses::All,
                            lines,
                        );
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
        // Sessions that ended.
        let (ended, still): (Vec<Running>, Vec<Running>) = running
            .drain(..)
            .partition(|(_, thread)| thread.is_finished());
        running = still;
        for (name, thread) in ended {
            let (result, session_lines) = thread
                .join()
                .unwrap_or_else(|_| (Err("the session's thread failed".into()), Vec::new()));
            lines.extend(session_lines);
            sessions += 1;
            match result {
                Ok(ok) => {
                    passed &= ok;
                    list.set_status(&trf(
                        "The session with {name} has ended.",
                        &[("name", &name)],
                    ));
                }
                Err(error) => {
                    passed = false;
                    lines.push(format!("session failed: {error}"));
                    list.set_status(&trf(
                        "Could not connect to {name}: {error}",
                        &[("name", &name), ("error", &error)],
                    ));
                }
            }
        }
        if reconnect_at.is_none() {
            while let Some(event) = account.next_event(Duration::ZERO) {
                match event {
                    AccountEvent::Devices(devices) => {
                        list.set_rows(rows(&devices, &account.device_id, &notes))
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
                    AccountEvent::DeviceJoined(name) => list.set_status(&trf(
                        "A new device, {name}, logged in to your account. If that was not you, change your password and take it out of the account.",
                        &[("name", &name)],
                    )),
                    AccountEvent::DeviceKeyChanged(name) => list.set_status(&trf(
                        "{name} logged in again with a new device key. If that was not you, change your password and take it out of the account.",
                        &[("name", &name)],
                    )),
                    AccountEvent::Signal { .. }
                    | AccountEvent::IceServers { .. }
                    | AccountEvent::PasswordChanged(_)
                    | AccountEvent::HelpOpen(_)
                    | AccountEvent::HelpClosed(_)
                    | AccountEvent::HelpRequested { .. }
                    | AccountEvent::HelpGranted(_)
                    | AccountEvent::HelpAsked { .. }
                    | AccountEvent::WakeRequest { .. } => {}
                }
            }
        }
        if reconnect_at.is_some_and(|at| Instant::now() >= at) {
            match Account::connect(login, identity, AccountRole::Viewer, None) {
                Ok(new) => {
                    *account = new;
                    list.set_rows(rows(&account.devices, &account.device_id, &notes));
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
    // The list was closed: sessions still open end when their windows do.
    for (name, thread) in running {
        let (result, session_lines) = thread
            .join()
            .unwrap_or_else(|_| (Err("the session's thread failed".into()), Vec::new()));
        lines.extend(session_lines);
        sessions += 1;
        if let Err(error) = &result {
            lines.push(format!("session with {name} failed: {error}"));
        }
        passed &= result.unwrap_or(false);
    }
    Ok(passed && (options.ui_test.is_none() || sessions == 1))
}

/// Lets a form paint for about 200 ms before its picture is taken; `step` handles messages.
fn settle(mut step: impl FnMut() -> bool) {
    for _ in 0..10 {
        let _ = step();
    }
}

/// The device list's rows: devices that can be controlled first, then by name.
/// The device list's rows: favourites first, then those that can be controlled, then by name.
fn rows(devices: &[Device], own: &str, notes: &DeviceNotes) -> Vec<DeviceRow> {
    let mut sorted: Vec<&Device> = devices.iter().collect();
    sorted.sort_by_key(|d| {
        (
            !notes.get(&d.id).favourite,
            !d.controllable,
            notes.label(&d.id, &d.name).to_lowercase(),
        )
    });
    sorted
        .into_iter()
        .map(|d| DeviceRow {
            id: d.id.clone(),
            name: notes.label(&d.id, &d.name),
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
/// Filled in from an invitation link, or else from an invitation on the clipboard; Connect stays the person's click.
fn ask_for_help_target(
    account: &mut Account,
    prefill: Option<(String, String)>,
    options: &Options,
    pictures: Option<&Path>,
    lines: &mut Vec<String>,
) -> Option<Device> {
    let mut form = HelpForm::new().ok()?;
    let clipboard = || {
        queqiao_input::clipboard_text(None)
            .as_deref()
            .and_then(queqiao_core::invitation::parse)
    };
    match prefill {
        Some((help_id, code)) => {
            form.fill(&help_id, &code);
            form.set_status(tr(
                "Filled in from the invitation link: check it and press Connect.",
            ));
        }
        None => {
            if let Some((help_id, code)) = clipboard() {
                form.fill(&help_id, &code);
                form.set_status(tr(
                    "Filled in from the clipboard: check it and press Connect.",
                ));
                lines.push(format!(
                    "help: filled in from the clipboard, help ID {help_id}"
                ));
            }
        }
    }
    if options.test_submit_help {
        settle(|| form.next(20).is_some());
        if let Some(folder) = pictures {
            let _ = form.save_picture(&folder.join("help_form.bmp"));
        }
        form.press_connect();
    } else if options.help_go {
        settle(|| form.next(20).is_some());
        form.press_connect();
    }
    loop {
        match form.next(50) {
            Some(HelpAction::Submit { help_id, code }) => {
                match account.request_help(&help_id, &code) {
                    Ok(target) => {
                        lines.push(format!("help granted: {} ({})", target.name, target.id));
                        return Some(target);
                    }
                    Err(error) if options.test_submit_help => {
                        lines.push(format!("help refused: {error}"));
                        return None;
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
    addresses: Addresses,
    lines: &mut Vec<String>,
) -> Result<bool, String> {
    let id = new_session_id();
    let mut signal = account.session(&target.id, AccountRole::Agent, &id);
    signal.hello("viewer").map_err(|e| e.to_string())?;
    let (began, since) = (queqiao_account::connections::local_now(), Instant::now());
    let result = session(
        options,
        device,
        context,
        &mut signal,
        &target.name,
        addresses,
        lines,
    );
    if result.is_ok() {
        remember(&target.id, &target.name, began, since);
    }
    result
}

/// Notes a session this PC started, once it ended (the main window lists them as "Recent connections").
fn remember(device_id: &str, device_name: &str, began: String, since: Instant) {
    queqiao_account::connections::record(&queqiao_account::connections::Connection {
        when: began,
        seconds: since.elapsed().as_secs(),
        device_id: device_id.to_string(),
        device_name: device_name.to_string(),
    });
}

/// One session: the agent's screen in a window, this window's input to the agent.
fn session(
    options: &Options,
    device: &ID3D11Device,
    context: &ID3D11DeviceContext,
    signal: &mut dyn Signaling,
    agent: &str,
    addresses: Addresses,
    lines: &mut Vec<String>,
) -> Result<bool, String> {
    let started = Instant::now();
    let peer = Peer::start_with(Role::Viewer, signal, addresses).map_err(|e| e.to_string())?;
    run_session(options, device, context, peer, started, agent, lines)
}

/// Connects to `target` through the server: the hellos, offer, answer and candidates (signalling needs the account,
/// so this is on its thread). The peer and when it started.
fn start_peer(
    account: &mut Account,
    target: &Device,
    addresses: Addresses,
) -> Result<(Peer, Instant), String> {
    let id = new_session_id();
    let mut signal = account.session(&target.id, AccountRole::Agent, &id);
    signal.hello("viewer").map_err(|e| e.to_string())?;
    let started = Instant::now();
    let peer = Peer::start_with(Role::Viewer, &mut signal, addresses).map_err(|e| e.to_string())?;
    Ok((peer, started))
}

/// A session on a thread of its own, with a graphics device of its own (several run at once): its outcome and
/// report lines when it ends.
fn session_thread(
    options: &Options,
    peer: Peer,
    started: Instant,
    device_id: String,
    agent: String,
) -> std::thread::JoinHandle<(Result<bool, String>, Vec<String>)> {
    let options = options.clone();
    std::thread::spawn(move || {
        let mut lines = Vec::new();
        let began = queqiao_account::connections::local_now();
        let result = create_device().and_then(|(device, context, _)| {
            run_session(
                &options, &device, &context, peer, started, &agent, &mut lines,
            )
        });
        if result.is_ok() {
            remember(&device_id, &agent, began, started);
        }
        (result, lines)
    })
}

/// A session once its peer is set up: the window, the picture, input, sound, files, command lines.
fn run_session(
    options: &Options,
    device: &ID3D11Device,
    context: &ID3D11DeviceContext,
    peer: Peer,
    started: Instant,
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
    // The codec the frames come in (the decoder is made anew when it changes), what to ask for, what this PC can
    // decode, and the frames of each.
    let mut decoder_codec = queqiao_core::protocol::VideoCodec::H264;
    let mut codec_choice = options.codec.unwrap_or_default();
    // AV1 at all (asked for by hand), and on the GPU (what "Automatic" needs: the CPU is too slow for a big screen).
    let can_decode_av1 = queqiao_codec::can_decode(queqiao_core::protocol::VideoCodec::Av1);
    let av1_on_gpu = can_decode_av1 && queqiao_codec::av1_decodes_on_gpu(device);
    let av1_for = |choice: queqiao_core::protocol::CodecPreference| match choice {
        queqiao_core::protocol::CodecPreference::Automatic => av1_on_gpu,
        _ => can_decode_av1,
    };
    // A decoder for the other codec, made before it is needed: making Windows' AV1 decoder takes a while, and doing
    // it when the first AV1 frame comes would hold this window (and its keyboard hook) just after the first picture.
    let mut spare_decoder: Option<(queqiao_core::protocol::VideoCodec, H264Decoder)> = None;
    if codec_choice != queqiao_core::protocol::CodecPreference::H264 && av1_for(codec_choice) {
        let making = Instant::now();
        match H264Decoder::with_codec(
            queqiao_core::protocol::VideoCodec::Av1,
            device,
            context,
            DecoderChoice::from_env(),
        ) {
            Ok(made) => {
                lines.push(format!(
                    "decoder for AV1 ready: {} (made in {} ms)",
                    made.name(),
                    making.elapsed().as_millis()
                ));
                spare_decoder = Some((queqiao_core::protocol::VideoCodec::Av1, made));
            }
            Err(error) => lines.push(format!("decoder for AV1: {}", describe(error))),
        }
    }
    let mut frames_by_codec = [0u32; 2];
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
    // Clipboard images both ways, on their own stream.
    let (mut images, mut image_in) = (ImageSender::default(), ImageReceiver::default());
    let (mut images_received, mut images_sent, mut image_test_received) = (0u32, 0u32, false);
    // The image last received: not put on the clipboard again, nor sent back (as for text).
    let mut last_image_in: Option<u64> = None;
    // Files from the other side's clipboard copies, by copy; they go on this clipboard as they arrive.
    let mut clip_files: HashMap<u32, Vec<PathBuf>> = HashMap::new();
    let mut clip_files_set = 0u32;
    if let Some(path) = &options.clipboard_files_test {
        let _ = transfers.send_clipboard(std::slice::from_ref(path));
    }
    for path in &options.send_files {
        let _ = transfers.send(path);
    }
    let mut status = TitleStatus::new(agent);
    // The other PC's sound; "Play that PC's sound" in the window menu turns it off there (no data then).
    let player = Player::start(options.sound_device.clone(), options.sound_test);
    let mut sound_on = !options.mute;
    // The window's own keys (changed with Hotkeys...).
    let mut keys = hotkeys::Keys::load();
    if options.boss_key_test {
        keys.boss_key = "Ctrl+Alt+F11".into();
    }
    let full_screen_label = trf(
        "Full screen ({key})",
        &[("key", &keys.full_screen().text())],
    );
    let game_mouse_label = trf("Game mouse ({key})", &[("key", &keys.game_mouse().text())]);
    player.set_muted(options.mute);
    window.add_menu_items(&[
        (MENU_CTRL_ALT_DEL, tr("Send Ctrl+Alt+Del")),
        (MENU_LOCK, tr("Lock that PC")),
        (MENU_RESTART, tr("Restart that PC...")),
        (MENU_RESTART_QUEQIAO, tr("Restart Queqiao on that PC")),
        (MENU_SOUND, tr("Play that PC's sound")),
        (MENU_MUTE_SPEAKERS, tr("Mute that PC's speakers")),
        (MENU_PRIVACY, tr("Darken that PC's screen")),
        (MENU_TERMINAL, tr("Open a command line on that PC")),
        (MENU_COMMAND_LINES, tr("Command lines...")),
        (MENU_FULL_SCREEN, full_screen_label.as_str()),
        (MENU_ON_TOP, tr("Always on top")),
        (MENU_SEND_FILES, tr("Send files...")),
        (MENU_FIGURES, tr("Session figures")),
        (MENU_TRANSFERS, tr("Transfers...")),
        (MENU_CUSTOM_BITRATE, tr("Custom bitrate...")),
        (MENU_GAME_MOUSE, game_mouse_label.as_str()),
        (
            MENU_SHORTCUTS,
            tr("Send Windows shortcuts to that PC (Win, Alt+Tab)"),
        ),
    ]);
    let mut privacy_on = options.privacy;
    // Windows shortcuts go to that PC while the window is in front (on unless turned off).
    window.set_send_shortcuts(true);
    window.check_menu_item(MENU_SHORTCUTS, window.is_sending_shortcuts());
    let mut on_top = false;
    // The boss key: registered with Windows (it works with the window hidden); hidden: the window and its sound.
    let register_boss = |window: &Window, keys: &hotkeys::Keys, lines: &mut Vec<String>| -> bool {
        window.unregister_hotkey(BOSS_HOTKEY);
        let Some(boss) = keys.boss_key() else {
            return false;
        };
        let (modifiers, vk) = boss.system();
        let registered = window.register_hotkey(BOSS_HOTKEY, modifiers, vk);
        lines.push(format!(
            "boss key {}: {}",
            boss.text(),
            if registered {
                "registered"
            } else {
                "not registered (another program has it)"
            }
        ));
        registered
    };
    let boss_registered = register_boss(&window, &keys, lines);
    let mut boss_hidden = false;
    let mut boss_test: Option<(Instant, u8, bool)> = options
        .boss_key_test
        .then(|| (Instant::now() + Duration::from_secs(1), 0, false));
    // The transfer list window, while open.
    let mut transfer_list: Option<PickList> = None;
    // That PC's Windows password, while it asks for it (locked); the tests' passwords instead.
    let mut password_form: Option<queqiao_display::PasswordForm> = None;
    let mut test_passwords: std::collections::VecDeque<String> =
        options.test_passwords.iter().cloned().collect();
    let mut cancel_test_done = false;
    // The secure-desktop test: when it started, its step, and the pictures shown when that step began.
    let mut secure_test: Option<(Instant, u8, u64)> = None;
    let mut speakers_muted = options.mute_speakers;
    window.check_menu_item(MENU_MUTE_SPEAKERS, speakers_muted);
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
    let resolution_items: Vec<(u16, &str)> = MENU_RESOLUTION
        .iter()
        .map(|(id, _, text)| (*id, tr(text)))
        .collect();
    window.add_submenu(tr("Resolution"), &resolution_items);
    let opacity_items: Vec<(u16, &str)> = MENU_OPACITY
        .iter()
        .map(|(id, _, text)| (*id, tr(text)))
        .collect();
    window.add_submenu(tr("Opacity"), &opacity_items);
    window.add_menu_items(&[
        (MENU_ACTUAL_SIZE, tr("Actual size (1:1)")),
        (MENU_HOTKEYS, tr("Hotkeys...")),
        (MENU_POINTER_SHAPE, tr("That PC's pointer shape")),
        (MENU_MICROPHONE, tr("Send my microphone")),
    ]);
    // This PC's microphone to that PC (off until the person turns it on).
    let mut microphone: Option<queqiao_audio::Capture> = None;
    let speed_items: Vec<(u16, &str)> = MENU_MOUSE_SPEED
        .iter()
        .map(|(id, _, text)| (*id, tr(text)))
        .collect();
    window.add_submenu(tr("Game mouse speed"), &speed_items);
    let codec_items: Vec<(u16, &str)> = MENU_CODEC
        .iter()
        .map(|(id, _, text)| (*id, tr(text)))
        .collect();
    window.add_submenu(tr("Picture codec"), &codec_items);
    for (item, choice, _) in MENU_CODEC {
        window.check_menu_item(item, choice == options.codec.unwrap_or_default());
    }
    // That PC's pointer shape as this window's cursor (the last one, to show again when turned back on).
    let mut pointer_shape_on = true;
    window.check_menu_item(MENU_POINTER_SHAPE, true);
    let mut last_pointer: Option<queqiao_core::protocol::PointerShape> = None;
    let mut pointer_hidden = false;
    let mut pointer_shapes = 0u32;
    // The game mouse's speed in percent, and the part of a count not sent yet (so slow loses nothing).
    let mut mouse_speed = 100u32;
    let mut mouse_rest = (0i32, 0i32);
    let show_speed = |window: &Window, speed: u32| {
        for (id, value, _) in MENU_MOUSE_SPEED {
            window.check_menu_item(id, value == speed);
        }
    };
    show_speed(&window, mouse_speed);
    let show_opacity = |window: &Window, percent: u8| {
        for (id, value, _) in MENU_OPACITY {
            window.check_menu_item(id, value == percent);
        }
    };
    let opacity = options.opacity.unwrap_or(100).clamp(20, 100);
    window.set_opacity(opacity);
    show_opacity(&window, opacity);
    if options.opacity.is_some() {
        lines.push(format!(
            "window: opacity {opacity} % asked, {} % as Windows has it",
            window.opacity()
        ));
    }
    let mut resolution = options.resolution.unwrap_or(0);
    let show_resolution = |window: &Window, resolution: u16| {
        for (id, short, _) in MENU_RESOLUTION {
            window.check_menu_item(id, short == resolution);
        }
    };
    show_resolution(&window, resolution);
    // That PC's screens, once it has told them (a submenu then).
    let mut screen_count = 0u8;
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
    // The file picker runs on a thread of its own (the session goes on meanwhile); the files chosen come back here.
    let (picked_files, picked) = std::sync::mpsc::channel::<Vec<PathBuf>>();
    let pick_title = trf("Send files to {name}", &[("name", agent)]);
    let owner = window.hwnd().0 as isize;
    let start_picker = || {
        let (title, back) = (pick_title.clone(), picked_files.clone());
        std::thread::spawn(move || {
            let owner = windows::Win32::Foundation::HWND(owner as *mut std::ffi::c_void);
            let _ = back.send(queqiao_display::pick_files(Some(owner), &title));
        });
    };
    // What the main window asked for, done once the session is up.
    let mut then = options.then;
    // Queqiao's bar at the top (the session's commands, tucked away when the pointer leaves it) and the card of
    // figures at the bottom right; the menu stays as it was.
    use queqiao_display::{BAR_END, BAR_MORE, BarButton, Figures, SessionOverlay};
    let mut overlay = SessionOverlay::new(
        window.hwnd(),
        agent,
        vec![
            BarButton {
                glyph: "\u{E740}",
                label: tr("Full screen").to_string(),
                id: MENU_FULL_SCREEN,
            },
            BarButton {
                glyph: "\u{E765}",
                label: "Ctrl+Alt+Del".to_string(),
                id: MENU_CTRL_ALT_DEL,
            },
            BarButton {
                glyph: "\u{E898}",
                label: tr("Send files").to_string(),
                id: MENU_SEND_FILES,
            },
            BarButton {
                glyph: "\u{E756}",
                label: tr("Command line").to_string(),
                id: MENU_TERMINAL,
            },
            BarButton {
                glyph: "\u{E9D9}",
                label: tr("Figures").to_string(),
                id: MENU_FIGURES,
            },
            BarButton {
                glyph: "\u{E712}",
                label: String::new(),
                id: BAR_MORE,
            },
            BarButton {
                glyph: "\u{E711}",
                label: tr("End").to_string(),
                id: BAR_END,
            },
        ],
    )
    .map_err(|e| lines.push(format!("overlay: {}", describe(e))))
    .ok();
    if options.figures
        && let Some(overlay) = overlay.as_mut()
    {
        overlay.set_card(true);
        overlay.keep_bar_out(true);
        // "Send files" as under the pointer: its highlight (and its tip on a bar without labels) in the picture.
        overlay.hover_for_test(Some(2));
        window.check_menu_item(MENU_FIGURES, true);
    }
    // Tests: the picture of the window with its overlays, taken once, a few seconds after the first picture.
    let mut overlay_picture = options.ui_pictures.clone();
    // Then one with the bar let go, tucked into its tab (unless the pointer is at the top of the window).
    let mut tucked_picture: Option<(PathBuf, Instant)> = None;
    window.check_menu_item(MENU_PRIVACY, privacy_on);
    window.set_full_screen(options.full_screen);
    window.check_menu_item(MENU_SOUND, sound_on);
    let mut closed = None;

    'session: loop {
        if let Some(overlay) = overlay.as_mut() {
            overlay.tick();
        }
        if let Some(shown) = first_picture_after
            && started.elapsed() > shown + Duration::from_secs(3)
            && let Some(folder) = overlay_picture.take()
        {
            let path = folder.join("viewer_session.bmp");
            match queqiao_display::save_screen_picture(window.hwnd(), &path) {
                Ok(()) => lines.push(format!("session picture: {}", path.display())),
                Err(error) => lines.push(format!("session picture: {error}")),
            }
            if let Some(overlay) = overlay.as_mut() {
                for _ in 0..2 {
                    if !overlay.click_for_test(MENU_FIGURES) {
                        lines.push("bar: no Figures button to click".to_string());
                    }
                }
                overlay.keep_bar_out(false);
                overlay.hover_for_test(None);
            }
            tucked_picture = Some((folder, Instant::now()));
        }
        if let Some((folder, since)) = &tucked_picture
            && since.elapsed() > Duration::from_millis(2500)
        {
            let path = folder.join("viewer_session_tucked.bmp");
            match queqiao_display::save_screen_picture(window.hwnd(), &path) {
                Ok(()) => lines.push(format!("session picture: {}", path.display())),
                Err(error) => lines.push(format!("session picture: {error}")),
            }
            tucked_picture = None;
        }
        // Files from the picker go as dropped ones do.
        while let Ok(paths) = picked.try_recv() {
            for path in &paths {
                match transfers.send(path) {
                    Ok(()) => lines.push(format!("sending {}", path.display())),
                    Err(error) => lines.push(format!("not sent: {error}")),
                }
            }
        }
        if let Some((fps, rate)) = status.update(&window, &stats)
            && let Some(overlay) = overlay.as_mut()
        {
            let codec = match decoder_codec {
                queqiao_core::protocol::VideoCodec::Av1 => "AV1",
                _ => "H.264",
            };
            let place = if decoder.name().contains("D3D11") {
                "GPU"
            } else {
                "CPU"
            };
            overlay.set_figures(Figures {
                relayed: status.relayed,
                delay_ms: stats.rtt_ms.last().copied(),
                fps,
                rate,
                codec: format!("{codec} · {place}"),
                picture: picture_size,
                elapsed: started.elapsed(),
                delays: stats.rtt_ms.iter().rev().take(40).rev().copied().collect(),
            });
        }
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
                    MENU_RESTART_QUEQIAO => Some(Command::RestartQueqiao),
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
                    id if id >= MENU_SCREENS && id < MENU_SCREENS + 16 * u16::from(screen_count) => {
                        let index = ((id - MENU_SCREENS) / 16) as u8;
                        for other in 0..screen_count {
                            window.check_menu_item(MENU_SCREENS + 16 * u16::from(other), other == index);
                        }
                        Some(Command::Screen(index))
                    }
                    id if MENU_RESOLUTION.iter().any(|(item, _, _)| *item == id) => {
                        resolution = MENU_RESOLUTION.iter().find(|(item, _, _)| *item == id).map_or(0, |(_, short, _)| *short);
                        show_resolution(&window, resolution);
                        Some(Command::Resolution(resolution))
                    }
                    MENU_TERMINAL => {
                        terminals.open(&peer, agent, false, lines);
                        None
                    }
                    MENU_SEND_FILES => {
                        start_picker();
                        None
                    }
                    MENU_FIGURES => {
                        if let Some(overlay) = overlay.as_mut() {
                            let on = !overlay.card_shown();
                            overlay.set_card(on);
                            window.check_menu_item(MENU_FIGURES, on);
                            lines.push(format!("card of figures: {}", if on { "on" } else { "off" }));
                        }
                        None
                    }
                    MENU_COMMAND_LINES => {
                        // Each command line: show (or detach), rename, end.
                        let open = terminals.list();
                        let labels: Vec<(u16, String)> = open
                            .iter()
                            .enumerate()
                            .flat_map(|(at, (_, name, shown))| {
                                let base = 0x2000 + (at as u16) * 4;
                                [
                                    if *shown {
                                        (base + 1, trf("{name}: detach (it keeps running)", &[("name", name)]))
                                    } else {
                                        (base, trf("{name}: show", &[("name", name)]))
                                    },
                                    (base + 2, trf("{name}: rename...", &[("name", name)])),
                                    (base + 3, trf("{name}: end", &[("name", name)])),
                                ]
                            })
                            .collect();
                        let items: Vec<(u16, &str, bool)> =
                            labels.iter().map(|(id, label)| (*id, label.as_str(), false)).collect();
                        if items.is_empty() {
                            std::thread::spawn(|| {
                                queqiao_display::show_info(tr("Queqiao"), tr("No command lines are open."))
                            });
                        } else if let Some(chosen) = popup_menu(window.hwnd(), &items) {
                            let at = usize::from((chosen - 0x2000) / 4);
                            let action = match (chosen - 0x2000) % 4 {
                                0 => LineAction::Show,
                                1 => LineAction::Detach,
                                2 => LineAction::Rename,
                                _ => LineAction::End,
                            };
                            if let Some((id, _, _)) = open.get(at) {
                                terminals.act(*id, action, &peer, lines);
                            }
                        }
                        None
                    }
                    MENU_PRIVACY => {
                        privacy_on = !privacy_on;
                        window.check_menu_item(MENU_PRIVACY, privacy_on);
                        Some(if privacy_on { Command::PrivacyOn } else { Command::PrivacyOff })
                    }
                    MENU_TRANSFERS => {
                        if transfer_list.is_none() {
                            transfer_list = PickList::new(
                                tr("Queqiao - transfers"),
                                tr("Files on their way (sent and received):"),
                                &[(TRANSFER_PAUSE, tr("Pause / resume")), (TRANSFER_CANCEL, tr("Cancel"))],
                            )
                            .ok();
                        }
                        None
                    }
                    id if MENU_OPACITY.iter().any(|(item, _, _)| *item == id) => {
                        let percent = MENU_OPACITY.iter().find(|(item, _, _)| *item == id).map_or(100, |(_, percent, _)| *percent);
                        window.set_opacity(percent);
                        show_opacity(&window, percent);
                        None
                    }
                    MENU_ACTUAL_SIZE => {
                        window.set_client_size(picture_size.0, picture_size.1);
                        None
                    }
                    MENU_MICROPHONE => {
                        if microphone.take().is_some() {
                            window.check_menu_item(MENU_MICROPHONE, false);
                            Some(Command::Microphone(false))
                        } else {
                            match peer.audio_sender() {
                                Ok(sender) => {
                                    microphone = Some(queqiao_audio::Capture::from_source(
                                        queqiao_audio::Source::Microphone(None),
                                        Box::new(move |packet, time| sender.send(packet, time)),
                                    ));
                                    window.check_menu_item(MENU_MICROPHONE, true);
                                    Some(Command::Microphone(true))
                                }
                                Err(error) => {
                                    lines.push(format!("microphone: not sent ({error})"));
                                    None
                                }
                            }
                        }
                    }
                    MENU_POINTER_SHAPE => {
                        pointer_shape_on = !pointer_shape_on;
                        window.check_menu_item(MENU_POINTER_SHAPE, pointer_shape_on);
                        window.set_remote_cursor(
                            last_pointer
                                .as_ref()
                                .filter(|_| pointer_shape_on)
                                .and_then(queqiao_display::RemoteCursor::from_shape),
                        );
                        window.set_pointer_hidden(pointer_shape_on && pointer_hidden);
                        None
                    }
                    id if MENU_CODEC.iter().any(|(item, _, _)| *item == id) => {
                        codec_choice = MENU_CODEC.iter().find(|(item, _, _)| *item == id).map_or_else(Default::default, |(_, choice, _)| *choice);
                        for (item, choice, _) in MENU_CODEC {
                            window.check_menu_item(item, choice == codec_choice);
                        }
                        Some(Command::VideoCodec {
                            prefer: codec_choice,
                            can_decode_av1: av1_for(codec_choice),
                        })
                    }
                    id if MENU_MOUSE_SPEED.iter().any(|(item, _, _)| *item == id) => {
                        mouse_speed = MENU_MOUSE_SPEED.iter().find(|(item, _, _)| *item == id).map_or(100, |(_, speed, _)| *speed);
                        show_speed(&window, mouse_speed);
                        mouse_rest = (0, 0);
                        None
                    }
                    MENU_HOTKEYS => {
                        // Each key in turn (Cancel keeps it); an empty boss key turns it off.
                        let mut changed = keys.clone();
                        let asks: [(&str, &mut String, bool); 3] = [
                            (tr("Full screen key (for example Ctrl+Alt+Enter):"), &mut changed.full_screen, false),
                            (tr("Game mouse key (for example Ctrl+Alt+M):"), &mut changed.game_mouse, false),
                            (tr("Boss key, hides this window at once (for example Ctrl+Alt+H; empty: none):"), &mut changed.boss_key, true),
                        ];
                        for (prompt, value, may_be_empty) in asks {
                            if let Some(typed) = ask_text(tr("Queqiao"), prompt, value) {
                                let typed = typed.trim().to_string();
                                match hotkeys::Hotkey::parse(&typed) {
                                    Some(key) => *value = key.text(),
                                    None if typed.is_empty() && may_be_empty => value.clear(),
                                    None => show_error(
                                        tr("Queqiao"),
                                        &trf("Not a key Queqiao can use: {key}. A key is Ctrl and/or Alt (Shift may be added) with one letter, digit, F1 to F12 or Enter, Space, Home and the like.", &[("key", &typed)]),
                                    ),
                                }
                            }
                        }
                        if changed != keys {
                            keys = changed;
                            if let Err(error) = keys.save() {
                                lines.push(format!("hotkeys: not kept: {error}"));
                            }
                            window.rename_menu_item(MENU_FULL_SCREEN, &trf("Full screen ({key})", &[("key", &keys.full_screen().text())]));
                            window.rename_menu_item(MENU_GAME_MOUSE, &trf("Game mouse ({key})", &[("key", &keys.game_mouse().text())]));
                            register_boss(&window, &keys, lines);
                            lines.push("hotkeys changed".into());
                        }
                        None
                    }
                    MENU_ON_TOP => {
                        on_top = !on_top;
                        window.set_always_on_top(on_top);
                        window.check_menu_item(MENU_ON_TOP, on_top);
                        None
                    }
                    MENU_CUSTOM_BITRATE => {
                        let typed = ask_text(tr("Queqiao"), tr("Highest bitrate in Mbit/s (0: as the quality setting says):"), "");
                        typed
                            .and_then(|text| text.trim().replace(',', ".").parse::<f64>().ok())
                            .filter(|mbit| mbit.is_finite() && *mbit >= 0.0)
                            .map(|mbit| Command::MaxBitrate((mbit * 1000.0).min(f64::from(u32::MAX)) as u32))
                    }
                    MENU_SHORTCUTS => {
                        let on = !window.is_sending_shortcuts();
                        window.set_send_shortcuts(on);
                        window.check_menu_item(MENU_SHORTCUTS, window.is_sending_shortcuts());
                        None
                    }
                    MENU_MUTE_SPEAKERS => {
                        speakers_muted = !speakers_muted;
                        window.check_menu_item(MENU_MUTE_SPEAKERS, speakers_muted);
                        Some(Command::MuteSpeakers(speakers_muted))
                    }
                    MENU_GAME_MOUSE => {
                        let on = !window.is_game_mouse();
                        window.set_game_mouse(on);
                        window.check_menu_item(MENU_GAME_MOUSE, on);
                        None
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
            // The boss key: the window and its sound hidden at once, or back.
            if event == WindowEvent::Hotkey(BOSS_HOTKEY) {
                boss_hidden = !boss_hidden;
                if boss_hidden && window.is_game_mouse() {
                    window.set_game_mouse(false);
                    window.check_menu_item(MENU_GAME_MOUSE, false);
                }
                window.set_visible(!boss_hidden);
                if sound_on {
                    player.set_muted(boss_hidden);
                    let command = if boss_hidden {
                        Command::SoundOff
                    } else {
                        Command::SoundOn
                    };
                    peer.send_data(Message::Command(command).encode());
                }
                lines.push(format!(
                    "boss key: window {}",
                    if boss_hidden { "hidden" } else { "shown" }
                ));
                continue;
            }
            // The full-screen key (Ctrl+Alt+Enter unless changed): full screen on or off. Every other key goes to that PC.
            if let WindowEvent::Key {
                scancode,
                extended,
                down: true,
            } = event
                && keys.full_screen().matches(scancode, extended, &held_keys)
            {
                let on = !window.is_full_screen();
                window.set_full_screen(on);
                continue;
            }
            // The game-mouse key (Ctrl+Alt+M unless changed).
            if let WindowEvent::Key {
                scancode,
                extended,
                down: true,
            } = event
                && keys.game_mouse().matches(scancode, extended, &held_keys)
            {
                let on = !window.is_game_mouse();
                window.set_game_mouse(on);
                window.check_menu_item(MENU_GAME_MOUSE, on);
                continue;
            }
            // Leaving the window gives the pointer back.
            if event == WindowEvent::FocusLost && window.is_game_mouse() {
                window.set_game_mouse(false);
                window.check_menu_item(MENU_GAME_MOUSE, false);
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
            // The game mouse's speed (the part of a count left over is carried to the next move).
            let event = match event {
                WindowEvent::MouseDelta { dx, dy } if mouse_speed != 100 => {
                    WindowEvent::MouseDelta {
                        dx: scaled_move(dx, mouse_speed, &mut mouse_rest.0),
                        dy: scaled_move(dy, mouse_speed, &mut mouse_rest.1),
                    }
                }
                other => other,
            };
            for message in translate(event, picture_size, window.client_size(), &mut held_keys) {
                peer.send_data(message.encode());
                stats.inputs_sent += 1;
            }
        }
        // Boss key test: pressed once (hidden?), again (shown?).
        if let Some((due, step, hidden_seen)) = boss_test
            && first_picture_after.is_some()
            && Instant::now() >= due
        {
            let next = Instant::now() + Duration::from_millis(700);
            boss_test = match step {
                0 => {
                    window.press_hotkey(BOSS_HOTKEY);
                    Some((next, 1, false))
                }
                1 => {
                    let hidden = !window.is_visible();
                    window.press_hotkey(BOSS_HOTKEY);
                    Some((next, 2, hidden))
                }
                _ => {
                    let shown = window.is_visible();
                    let passed = boss_registered && hidden_seen && shown;
                    lines.push(format!(
                        "boss key test: {} (registered: {boss_registered}, hidden: {hidden_seen}, shown again: {shown})",
                        if passed { "PASS" } else { "FAIL" }
                    ));
                    None
                }
            };
        }
        if options.input_test
            && channel_open
            && first_picture_after.is_some()
            && let Some(due) =
                next_test_step.or_else(|| Some(Instant::now() + Duration::from_secs(1)))
        {
            next_test_step = Some(due);
            let deltas = if options.game_mouse_test {
                TEST_DELTAS.len()
            } else {
                0
            };
            if Instant::now() >= due && test_step < TEST_POINTS.len() + 2 + deltas {
                let event = match test_step {
                    i if i < TEST_POINTS.len() => {
                        let (x, y) = TEST_POINTS[i];
                        InputEvent::MouseMove {
                            x: (x * 65535.0) as u16,
                            y: (y * 65535.0) as u16,
                        }
                    }
                    i if i >= TEST_POINTS.len() + 2 => {
                        let (dx, dy) = TEST_DELTAS[i - TEST_POINTS.len() - 2];
                        InputEvent::MouseDelta { dx, dy }
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
            match clipboard.as_ref().and_then(Clipboard::changed) {
                Some(ClipContent::Text(text))
                    if clipboard_last_in.as_ref() != Some(&text)
                        && text.len() <= MAX_CLIPBOARD_TEXT =>
                {
                    peer.send_data(Message::ClipboardText(text).encode());
                    clipboard_out += 1;
                }
                // Files copied here go over at once (not those that came from the other side).
                Some(ClipContent::Files(paths)) => {
                    let own = downloads_folder();
                    let paths: Vec<PathBuf> = paths
                        .into_iter()
                        .filter(|path| !path.starts_with(&own))
                        .collect();
                    if !paths.is_empty() && clipboard_size(&paths) <= CLIPBOARD_FILES_LIMIT {
                        let _ = transfers.send_clipboard(&paths);
                        lines.push(format!("clipboard files: sending {}", paths.len()));
                    }
                }
                // Queued (compressed) to go on the clipboard stream.
                Some(ClipContent::Image(image))
                    if last_image_in != Some(image_fingerprint(&image))
                        && images.send(&image).is_ok() =>
                {
                    images_sent += 1
                }
                _ => {}
            }
            let due = *clipboard_test_at.get_or_insert(Instant::now() + Duration::from_secs(1));
            if options.clipboard_test && clipboard_test_at.is_some() && Instant::now() >= due {
                let text = format!("{CLIPBOARD_TEST_TEXT} (from the viewer)");
                peer.send_data(Message::ClipboardText(text).encode());
                clipboard_out += 1;
                if images.send(&test_picture(1)).is_ok() {
                    images_sent += 1;
                }
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

        // Test: Ctrl+Alt+Del, then Escape, then a look at what came back.
        if let Some((began, step, shown_before)) = secure_test {
            let at = began.elapsed();
            let shown_now = u64::from(stats.shown);
            if step == 0 && at >= Duration::from_secs(3) {
                peer.send_data(Message::Command(Command::CtrlAltDel).encode());
                lines.push("secure test: sent Ctrl+Alt+Del".into());
                secure_test = Some((began, 1, shown_now));
            } else if step == 1 && at >= Duration::from_secs(5) {
                lines.push(format!(
                    "secure test: {} pictures shown while the Ctrl+Alt+Del screen was up",
                    shown_now - shown_before
                ));
                for down in [true, false] {
                    let key = InputEvent::Key {
                        scancode: 0x01,
                        extended: false,
                        down,
                    };
                    peer.send_data(Message::Input(key).encode());
                }
                lines.push("secure test: sent Escape".into());
                secure_test = Some((began, 2, shown_now));
            } else if step == 2 && at >= Duration::from_secs(9) {
                lines.push(format!(
                    "secure test: {} pictures shown after Escape",
                    shown_now - shown_before
                ));
                secure_test = Some((began, 3, shown_now));
            }
        }
        // Test: a file this side sends is cancelled once its first piece went out.
        if options.cancel_test
            && !cancel_test_done
            && let Some(row) = transfers
                .rows()
                .into_iter()
                .find(|row| row.sending && row.done > 0)
        {
            transfers.cancel(true, row.id);
            lines.push(format!("cancelled {} after {} bytes", row.name, row.done));
            cancel_test_done = true;
        }
        // The password form: what the person typed goes to that PC (encrypted), Windows checks it there.
        if let Some(form) = &mut password_form {
            match form.next(0) {
                Some(queqiao_display::PasswordAction::Submit(password)) if !password.is_empty() => {
                    peer.send_data(Message::Password(password).encode());
                    form.set_status(tr("Checking..."));
                }
                Some(queqiao_display::PasswordAction::Cancel) => password_form = None,
                _ => {}
            }
        }
        // The transfer list: progress, and what the person pressed.
        if let Some(list) = &mut transfer_list {
            let transfer_rows: Vec<TransferRow> = transfers.rows();
            let items: Vec<String> = transfer_rows
                .iter()
                .map(|row| {
                    let percent = row
                        .done
                        .saturating_mul(100)
                        .checked_div(row.size)
                        .unwrap_or(100);
                    let way = if row.sending {
                        tr("sending")
                    } else {
                        tr("receiving")
                    };
                    let state = if row.paused {
                        format!(" ({})", tr("paused"))
                    } else {
                        String::new()
                    };
                    format!("{way}  {}  {percent}%{state}", row.name)
                })
                .collect();
            list.set_items(items);
            match list.next(0) {
                Some((PICK_CLOSED, _)) => transfer_list = None,
                Some((TRANSFER_PAUSE, Some(index))) => {
                    if let Some(row) = transfer_rows.get(index) {
                        transfers.pause(row.id, !row.paused);
                    }
                }
                Some((TRANSFER_CANCEL, Some(index))) => {
                    if let Some(row) = transfer_rows.get(index).cloned() {
                        for message in transfers.cancel(row.sending, row.id) {
                            peer.send_stream(Stream::Files, message);
                        }
                    }
                }
                _ => {}
            }
        }
        for message in transfers.take(peer.room(Stream::Files)) {
            peer.send_stream(Stream::Files, message);
        }
        let outcomes = transfers.outcomes();
        for outcome in &outcomes {
            if let Outcome::Received { batch, top, .. } = outcome
                && *batch > 0
            {
                let files = clip_files.entry(*batch).or_default();
                if !files.contains(top) {
                    files.push(top.clone());
                }
                clip_files_set += 1;
                if let Some(clipboard) = &clipboard {
                    clipboard.set(ClipContent::Files(files.clone()));
                }
            }
        }
        report_files(outcomes, agent, lines, started.elapsed());
        for message in images.take(peer.room(Stream::Clipboard)) {
            peer.send_stream(Stream::Clipboard, message);
        }
        terminals.poll(&peer, lines);

        let mut event = peer.wait_event(Duration::from_millis(2));
        while let Some(current) = event.take() {
            match current {
                PeerEvent::Connected => connected_after = Some(started.elapsed()),
                PeerEvent::ChannelOpen => {
                    channel_open = true;
                    match then.take() {
                        Some(Then::Terminal) => terminals.open(&peer, agent, false, lines),
                        Some(Then::Files) => start_picker(),
                        None => {}
                    }
                    if !sound_on {
                        peer.send_data(Message::Command(Command::SoundOff).encode());
                    }
                    if privacy_on {
                        peer.send_data(Message::Command(Command::PrivacyOn).encode());
                    }
                    // Tests: a test tone as this PC's microphone.
                    if options.mic_test
                        && microphone.is_none()
                        && let Ok(sender) = peer.audio_sender()
                    {
                        microphone = Some(queqiao_audio::Capture::from_source(
                            queqiao_audio::Source::Tone,
                            Box::new(move |packet, time| sender.send(packet, time)),
                        ));
                        peer.send_data(Message::Command(Command::Microphone(true)).encode());
                    }
                    // Which codec to send (and whether this PC can decode AV1).
                    peer.send_data(
                        Message::Command(Command::VideoCodec {
                            prefer: codec_choice,
                            can_decode_av1: av1_for(codec_choice),
                        })
                        .encode(),
                    );
                    if speakers_muted {
                        peer.send_data(Message::Command(Command::MuteSpeakers(true)).encode());
                    }
                    if resolution != 0 {
                        peer.send_data(Message::Command(Command::Resolution(resolution)).encode());
                    }
                    if let Some(index) = options.screen {
                        peer.send_data(Message::Command(Command::Screen(index)).encode());
                    }
                    if let Some(kbit) = options.max_bitrate {
                        peer.send_data(Message::Command(Command::MaxBitrate(kbit)).encode());
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
                    if options.secure_test && secure_test.is_none() {
                        secure_test = Some((Instant::now(), 0, u64::from(stats.shown)));
                    }
                }
                PeerEvent::StreamOpen(Stream::Files) => {
                    report_files(transfers.opened(), agent, lines, started.elapsed())
                }
                PeerEvent::StreamOpen(Stream::Clipboard) => {}
                PeerEvent::StreamData(Stream::Clipboard, bytes) => {
                    if let Some(result) = image_in.handle(&bytes) {
                        match result {
                            Ok(image) => {
                                images_received += 1;
                                image_test_received |= image == test_picture(2);
                                let print = image_fingerprint(&image);
                                if last_image_in != Some(print) {
                                    last_image_in = Some(print);
                                    if let Some(clipboard) = &clipboard {
                                        clipboard.set(ClipContent::Image(image));
                                    }
                                }
                            }
                            Err(error) => lines.push(format!("clipboard image dropped: {error}")),
                        }
                    }
                }
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
                    codec,
                } => {
                    // Another codec (the agent switched): a decoder for it.
                    if codec != decoder_codec {
                        let ready = spare_decoder.take_if(|(spare, _)| *spare == codec);
                        let next = match ready {
                            Some((_, made)) => made,
                            None => H264Decoder::with_codec(
                                codec,
                                device,
                                context,
                                DecoderChoice::from_env(),
                            )
                            .map_err(|e| {
                                format!("decoder for {}: {}", codec.name(), describe(e))
                            })?,
                        };
                        // The one in use stays ready, for a switch back.
                        let previous = std::mem::replace(&mut decoder, next);
                        spare_decoder = Some((decoder_codec, previous));
                        decoder_codec = codec;
                        lines.push(format!(
                            "decoder: {} (the picture is {} now)",
                            decoder.name(),
                            codec.name()
                        ));
                    }
                    frames_by_codec
                        [usize::from(codec == queqiao_core::protocol::VideoCodec::Av1)] += 1;
                    stats.pickup_ms.push(received.elapsed().as_secs_f64() * 1e3);
                    stats.received += 1;
                    stats.lost += u32::from(!contiguous);
                    stats.bytes += data.len();
                    // Until the first picture a frame that is not a keyframe shows nothing, and no loss tells the agent:
                    // over the internet it finished connecting seconds before this side, and its first keyframe was
                    // dropped here (2026-11-21: the first picture after 19 s). Ask for one (the transport spaces the
                    // requests 600 ms apart).
                    if stats.shown == 0 && !queqiao_codec::is_keyframe(codec, &data) {
                        stats.waiting_for_keyframe += 1;
                        peer.request_keyframe();
                    }
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
                        if stats.viewer_ms.len() < MAX_SAMPLES {
                            stats.viewer_ms.insert(picture.time as u64, viewer_ms);
                        }
                    }
                }
                PeerEvent::Data(bytes) => match Message::decode(&bytes) {
                    // Kept for the report, never more than MAX_SAMPLES: the other side sends these as often as it likes.
                    Some(Message::Pong { sent_us, .. }) if stats.rtt_ms.len() < MAX_SAMPLES => {
                        let now_us = started.elapsed().as_micros() as u64;
                        stats
                            .rtt_ms
                            .push(now_us.saturating_sub(sent_us) as f64 / 1e3);
                    }
                    Some(Message::FrameInfo {
                        time_90khz,
                        agent_ms,
                    }) if stats.agent_ms.len() < MAX_SAMPLES => {
                        stats.agent_ms.insert(time_90khz, f64::from(agent_ms));
                    }
                    Some(Message::Pong { .. } | Message::FrameInfo { .. }) => {}
                    Some(Message::Leave) => {
                        closed = Some("the agent ended the session".to_string());
                        break 'session;
                    }
                    Some(Message::Screens { current, sizes })
                        if screen_count == 0 && !sizes.is_empty() =>
                    {
                        let names: Vec<String> = sizes
                            .iter()
                            .enumerate()
                            .map(|(index, (width, height))| {
                                format!(
                                    "{} ({width}x{height})",
                                    trf("Screen {n}", &[("n", &(index + 1).to_string())])
                                )
                            })
                            .collect();
                        let items: Vec<(u16, &str)> = names
                            .iter()
                            .enumerate()
                            .map(|(index, name)| (MENU_SCREENS + 16 * index as u16, name.as_str()))
                            .collect();
                        window.add_submenu(tr("Screens"), &items);
                        screen_count = sizes.len() as u8;
                        window.check_menu_item(MENU_SCREENS + 16 * u16::from(current), true);
                        let english: Vec<String> = sizes
                            .iter()
                            .enumerate()
                            .map(|(index, (width, height))| {
                                format!("Screen {} ({width}x{height})", index + 1)
                            })
                            .collect();
                        lines.push(format!("screens of that PC: {}", english.join(", ")));
                    }
                    Some(Message::Notice(text)) => {
                        lines.push(format!("notice from that PC: {text}"));
                        status.notice(&text);
                    }
                    Some(Message::PasswordNeeded { reason, tries_left }) => {
                        use queqiao_core::protocol::PasswordReason;
                        lines.push(format!("password: asked ({reason:?}, {tries_left} tries)"));
                        let text = match reason {
                            PasswordReason::Locked => tr(
                                "That PC is locked. Type its Windows password to show that it is you (this does not unlock it: unlock it on its own screen then).",
                            ),
                            PasswordReason::SignedOut => tr(
                                "Nobody is signed in on that PC. Type the password of its Windows account to show that it is you.",
                            ),
                            PasswordReason::OnlyTheOwner => {
                                tr("That PC is locked: only the person there can unlock it.")
                            }
                        };
                        if let Some(next) = test_passwords.pop_front() {
                            peer.send_data(Message::Password(next).encode());
                        } else if tries_left > 0 {
                            let title = trf("Queqiao - {device} is locked", &[("device", agent)]);
                            password_form = queqiao_display::PasswordForm::new(&title, text).ok();
                        } else {
                            status.notice(text);
                        }
                    }
                    Some(Message::PasswordResult { ok, tries_left }) => {
                        lines.push(format!(
                            "password: {} ({tries_left} tries left)",
                            if ok { "right" } else { "wrong" }
                        ));
                        if ok {
                            password_form = None;
                        } else if let Some(next) = test_passwords.pop_front() {
                            peer.send_data(Message::Password(next).encode());
                        } else if let Some(form) = &password_form {
                            let tries = tries_left.to_string();
                            form.set_status(&trf(
                                "Wrong password: {tries} tries left.",
                                &[("tries", &tries)],
                            ));
                        }
                    }
                    Some(Message::PointerShape(shape)) => {
                        pointer_shapes += 1;
                        if pointer_shape_on {
                            window.set_remote_cursor(queqiao_display::RemoteCursor::from_shape(
                                &shape,
                            ));
                        }
                        last_pointer = Some(shape);
                    }
                    Some(Message::PointerVisible(visible)) => {
                        pointer_hidden = !visible;
                        window.set_pointer_hidden(pointer_shape_on && pointer_hidden);
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
    if stats.waiting_for_keyframe > 0 {
        lines.push(format!(
            "before the first picture: {} frames without a keyframe (a keyframe asked for)",
            stats.waiting_for_keyframe
        ));
    }
    lines.push(format!("decode, ms: {}", summary(&mut stats.decode_ms)));
    // In order, one every half second: how the round trip went while files or clipboard images were moving.
    let course: Vec<String> = stats
        .rtt_ms
        .iter()
        .take(120)
        .map(|ms| format!("{ms:.0}"))
        .collect();
    lines.push(format!(
        "network round trips, first minute, ms: {}",
        course.join(" ")
    ));
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
    if let Some(microphone) = &microphone {
        let sent = microphone.stats();
        lines.push(format!(
            "microphone to that PC: {} packets from {}",
            sent.packets, sent.device
        ));
    }
    lines.push(format!(
        "frames by codec: H.264 {}, AV1 {} (this PC decodes AV1: {can_decode_av1}, on its GPU: {av1_on_gpu})",
        frames_by_codec[0], frames_by_codec[1]
    ));
    lines.push(format!(
        "pointer: {pointer_shapes} shapes from that PC{}",
        last_pointer.as_ref().map_or(String::new(), |shape| format!(
            " (the last {}x{}, {:?})",
            shape.width, shape.height, shape.kind
        ))
    ));
    lines.push(format!("window title: {}", status.title));
    lines.push(format!(
        "last picture size: {}x{}",
        picture_size.0, picture_size.1
    ));
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
    lines.push(format!(
        "clipboard images: {images_received} received, {images_sent} sent; test image from the other side received: {}",
        if image_test_received { "yes" } else { "no" }
    ));
    if clip_files_set > 0 {
        lines.push(format!(
            "clipboard files: {clip_files_set} put on the clipboard from the other side"
        ));
    }
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
    let test_steps = TEST_POINTS.len()
        + 2
        + if options.game_mouse_test {
            TEST_DELTAS.len()
        } else {
            0
        };
    let test_done = !options.input_test || test_step == test_steps;
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
        WindowEvent::MouseDelta { dx, dy } => InputEvent::MouseDelta {
            dx: dx.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16,
            dy: dy.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16,
        },
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
        WindowEvent::FilesDropped(_) | WindowEvent::Menu(_) | WindowEvent::Hotkey(_) => {
            return Vec::new();
        }
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

/// The command lines open on that PC, each in a console window here, or detached: its window closed, the command
/// line still running there, its output kept (the newest [`BACKLOG`] bytes) until it is shown again. None outlives
/// the session.
#[derive(Default)]
struct TerminalWindows {
    lines: BTreeMap<u32, CommandLine>,
    next_id: u32,
    /// Test: (the shell, its output so far, the test text seen, the exit code, the window size).
    test: Option<TerminalTest>,
}

struct CommandLine {
    name: String,
    window: Option<TerminalWindow>,
    /// Output that came while detached.
    backlog: Vec<u8>,
    /// How it ended, while detached.
    ended: Option<String>,
}

/// The most output kept for a detached command line.
const BACKLOG: usize = 256 * 1024;

/// What can be done with a command line from the session menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LineAction {
    Show,
    Detach,
    Rename,
    End,
}

#[derive(Default)]
struct TerminalTest {
    shell: String,
    output: Vec<u8>,
    seen: bool,
    exit_code: Option<u32>,
    size: Option<(u16, u16)>,
    /// The test detaches the window right after the start and shows it again once the test output came.
    detached: bool,
    shown_again: bool,
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
        let name = format!("{agent} #{id}");
        match open_terminal_window(&name, test) {
            Ok(window) => {
                self.lines.insert(
                    id,
                    CommandLine {
                        name,
                        window: Some(window),
                        backlog: Vec::new(),
                        ended: None,
                    },
                );
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

    /// The command lines for the session menu: (id, name, shown).
    fn list(&self) -> Vec<(u32, String, bool)> {
        self.lines
            .iter()
            .map(|(&id, line)| (id, line.name.clone(), line.window.is_some()))
            .collect()
    }

    fn act(&mut self, id: u32, action: LineAction, peer: &Peer, lines: &mut Vec<String>) {
        let Some(line) = self.lines.get_mut(&id) else {
            return;
        };
        match action {
            LineAction::Detach => {
                if let Some(window) = line.window.take() {
                    window.detach();
                    lines.push(format!(
                        "command line {id}: detached (it keeps running there)"
                    ));
                }
            }
            LineAction::Show => {
                if line.window.is_some() {
                    return;
                }
                match open_terminal_window(&line.name, self.test.is_some()) {
                    Ok(mut window) => {
                        window.output(&line.backlog);
                        lines.push(format!(
                            "command line {id}: shown again ({} bytes kept meanwhile)",
                            line.backlog.len()
                        ));
                        line.backlog.clear();
                        if let Some(ended) = line.ended.take() {
                            window.end(&ended);
                        }
                        line.window = Some(window);
                    }
                    Err(error) => lines.push(format!("command line {id}: no window ({error})")),
                }
            }
            LineAction::Rename => {
                if let Some(name) = ask_text(
                    tr("Queqiao"),
                    tr("A name for this command line:"),
                    &line.name,
                )
                .map(|name| name.trim().to_string())
                .filter(|name| !name.is_empty())
                {
                    if let Some(window) = &line.window {
                        window.set_title(&name);
                    }
                    lines.push(format!("command line {id}: renamed"));
                    line.name = name;
                }
            }
            LineAction::End => {
                peer.send_stream(Stream::Terminal, TerminalMessage::Close { id }.encode());
                self.lines.remove(&id);
                lines.push(format!("command line {id}: ended from the session menu"));
            }
        }
    }

    fn on_message(&mut self, message: TerminalMessage, peer: &Peer, lines: &mut Vec<String>) {
        match message {
            TerminalMessage::Opened { id, shell } => {
                lines.push(format!("command line {id} opened ({shell})"));
                if let Some(test) = &mut self.test {
                    test.shell = shell.clone();
                    // Detached before anything is typed: the output must wait for the window.
                    test.detached = true;
                    self.act(id, LineAction::Detach, peer, lines);
                    let input = terminal_test_input(&shell).as_bytes().to_vec();
                    peer.send_stream(
                        Stream::Terminal,
                        TerminalMessage::Input { id, data: input }.encode(),
                    );
                }
            }
            TerminalMessage::Output { id, data } => {
                if let Some(line) = self.lines.get_mut(&id) {
                    match &line.window {
                        Some(window) => window.output(&data),
                        None => {
                            line.backlog.extend_from_slice(&data);
                            let over = line.backlog.len().saturating_sub(BACKLOG);
                            line.backlog.drain(..over);
                        }
                    }
                }
                let mut show_again = false;
                if let Some(test) = &mut self.test {
                    test.output.extend_from_slice(&data);
                    if !test.seen
                        && String::from_utf8_lossy(&test.output).contains("queqiao-terminal-ok")
                    {
                        test.seen = true;
                        show_again = test.detached;
                        let exit = TerminalMessage::Input {
                            id,
                            data: b"exit\r".to_vec(),
                        };
                        peer.send_stream(Stream::Terminal, exit.encode());
                    }
                }
                if show_again {
                    self.act(id, LineAction::Show, peer, lines);
                    if let Some(test) = &mut self.test {
                        test.shown_again = self
                            .lines
                            .get(&id)
                            .is_some_and(|line| line.window.is_some());
                    }
                }
            }
            TerminalMessage::Exited { id, code } => {
                lines.push(format!("command line {id} ended with exit code {code}"));
                let ended = trf(
                    "The command line ended (exit code {code}).",
                    &[("code", &code.to_string())],
                );
                self.ended(id, ended);
                if let Some(test) = &mut self.test {
                    test.exit_code = Some(code);
                }
            }
            TerminalMessage::Failed { id, reason } => {
                lines.push(format!("command line {id} failed: {reason}"));
                self.ended(id, reason);
            }
            _ => {}
        }
    }

    fn ended(&mut self, id: u32, text: String) {
        if let Some(line) = self.lines.get_mut(&id) {
            match line.window.as_mut() {
                Some(window) => window.end(&text),
                None => line.ended = Some(text),
            }
        }
    }

    /// Keys and size changes from the windows; a closed window ends its command line.
    fn poll(&mut self, peer: &Peer, lines: &mut Vec<String>) {
        let mut closed = Vec::new();
        for (&id, line) in &self.lines {
            let Some(window) = &line.window else {
                continue;
            };
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
                self.lines.remove(&id);
                lines.push(format!("command line {id}: window closed"));
            }
        }
    }

    /// "terminal test: PASS (...)": the command ran while the window was detached, the window came back with the
    /// output kept meanwhile, the command line ended with 0, and the window process closed by itself with 0.
    fn test_result(&self) -> String {
        let Some(test) = &self.test else {
            return "terminal test: FAIL (no command line was opened)".into();
        };
        let helper = self
            .lines
            .values()
            .next()
            .and_then(|line| line.window.as_ref())
            .and_then(|window| {
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
        let passed = test.seen
            && test.exit_code == Some(0)
            && test.size.is_some()
            && test.detached
            && test.shown_again
            && helper == Some(0);
        format!(
            "terminal test: {} (shell {}, test output seen: {}, detached and shown again: {}, exit code {:?}, window size {:?}, window process exit code {:?})",
            if passed { "PASS" } else { "FAIL" },
            test.shell,
            test.seen,
            test.detached && test.shown_again,
            test.exit_code,
            test.size,
            helper
        )
    }
}

fn open_terminal_window(title: &str, test: bool) -> Result<TerminalWindow, String> {
    let program = std::env::current_exe().map_err(|e| e.to_string())?;
    TerminalWindow::open(&program, title, test)
}

/// Files copied to the clipboard go over at once up to this many bytes in all.
const CLIPBOARD_FILES_LIMIT: u64 = 512 << 20;

/// The size of files and folders (folders counted with everything in them, links not followed).
fn clipboard_size(paths: &[PathBuf]) -> u64 {
    let mut total = 0u64;
    let mut pending: Vec<PathBuf> = paths.to_vec();
    while let Some(path) = pending.pop() {
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&path) {
                pending.extend(entries.flatten().map(|entry| entry.path()));
            }
        } else if meta.is_file() {
            total = total.saturating_add(meta.len());
        }
        if total > CLIPBOARD_FILES_LIMIT {
            break;
        }
    }
    total
}

/// One report line per finished or failed file; finished ones go into the list of transfers (main window).
/// `at`: how long the session has been on (the report says when each file was done).
fn report_files(outcomes: Vec<Outcome>, peer: &str, lines: &mut Vec<String>, at: Duration) {
    let at = at.as_secs_f64();
    for outcome in outcomes {
        queqiao_files::history::record(&outcome, peer);
        lines.push(match outcome {
            Outcome::Sent { name, size } => {
                format!("file sent: {name} ({size} bytes, checked by the agent) after {at:.1} s")
            }
            Outcome::Received { path, size, .. } => {
                format!(
                    "file received: {} ({size} bytes, SHA-256 checked) after {at:.1} s",
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

/// `value` mouse counts at `percent` speed; what is left of a count is kept in `rest` (hundredths) for the next.
fn scaled_move(value: i32, percent: u32, rest: &mut i32) -> i32 {
    let total = value * percent as i32 + *rest;
    let out = total / 100;
    *rest = total - out * 100;
    out
}

fn describe(error: windows::core::Error) -> String {
    format!("0x{:08X} {}", error.code().0, error.message())
}

#[cfg(test)]
mod option_tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_link_cannot_add_options() {
        // As Windows passes a link with a quote in it: "%1" ends early and the rest become options.
        let injected = args(&[
            "queqiao://help/123456789/ABCDEFGH\"",
            "--connect",
            "198.51.100.7:9",
            "--send-file",
            "C:\\Users",
            "--report",
            "x.cmd",
        ]);
        let options = invitation_options(&injected);
        assert!(
            options.connect.is_none() && options.report.is_none() && options.send_files.is_empty()
        );
        // The text after the quote is no option; as with a pasted invitation, only the help ID and code are read
        // from it, to fill in the help form.
        let link = options.invite.unwrap();
        assert_eq!(
            queqiao_core::invitation::parse(&link),
            Some(("123456789".to_string(), "ABCDEFGH".to_string()))
        );
        // A plain link is taken as it is.
        let plain = invitation_options(&args(&["queqiao://help/123456789/ABCDEFGH"]));
        assert_eq!(
            plain.invite.as_deref(),
            Some("queqiao://help/123456789/ABCDEFGH")
        );
        assert!(
            invitation_options(&[])
                .invite
                .is_some_and(|link| link.is_empty())
        );
    }

    #[cfg(not(feature = "test-switches"))]
    #[test]
    fn a_shipping_build_refuses_the_test_options() {
        for option in [
            "--connect",
            "--listen",
            "--server",
            "--pin",
            "--user",
            "--password",
            "--report",
            "--send-file",
            "--invite",
            "--test-submit-help",
            "--trust-new-key",
            "--device",
            "--command",
            "--signup",
        ] {
            assert!(
                parse(args(&[option, "x"]).into_iter()).is_none(),
                "{option}"
            );
        }
        assert!(parse(std::iter::empty()).is_some());
    }

    #[cfg(feature = "test-switches")]
    #[test]
    fn a_test_build_reads_the_test_options() {
        let options =
            parse(args(&["--connect", "127.0.0.1:1", "--report", "r.txt"]).into_iter()).unwrap();
        assert_eq!(options.connect.as_deref(), Some("127.0.0.1:1"));
        assert!(options.report.is_some());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Windows reports system-menu commands masked with 0xFFF0: every id must be a multiple of 16, and all different
    /// (an id that is not came back as another item: the opacity items once all acted as "Opaque").
    #[test]
    fn system_menu_ids_survive_windows_masking() {
        let mut ids = vec![
            MENU_CTRL_ALT_DEL,
            MENU_LOCK,
            MENU_RESTART,
            MENU_SOUND,
            MENU_FULL_SCREEN,
            MENU_PRIVACY,
            MENU_TERMINAL,
            MENU_GAME_MOUSE,
            MENU_MUTE_SPEAKERS,
            MENU_SHORTCUTS,
            MENU_ON_TOP,
            MENU_CUSTOM_BITRATE,
            MENU_TRANSFERS,
            MENU_RESTART_QUEQIAO,
            MENU_COMMAND_LINES,
            MENU_ACTUAL_SIZE,
            MENU_HOTKEYS,
            MENU_POINTER_SHAPE,
            MENU_MICROPHONE,
        ];
        ids.extend(MENU_MOUSE_SPEED.iter().map(|(id, _, _)| *id));
        ids.extend(MENU_CODEC.iter().map(|(id, _, _)| *id));
        ids.extend(MENU_FPS.iter().map(|(id, _, _)| *id));
        ids.extend(MENU_QUALITY.iter().map(|(id, _, _)| *id));
        ids.extend(MENU_RESOLUTION.iter().map(|(id, _, _)| *id));
        ids.extend(MENU_OPACITY.iter().map(|(id, _, _)| *id));
        // Up to 16 screens.
        ids.extend((0..16u16).map(|index| MENU_SCREENS + 16 * index));
        for id in &ids {
            assert_eq!(id & 0xF, 0, "menu id {id:#x} is not a multiple of 16");
            assert!(*id < 0xF000, "menu id {id:#x} is in Windows' own range");
        }
        let unique: HashSet<u16> = ids.iter().copied().collect();
        assert_eq!(unique.len(), ids.len(), "two menu items share an id");
    }

    #[test]
    fn slow_game_mouse_loses_no_counts() {
        let mut rest = 0;
        let moved: i32 = (0..10).map(|_| scaled_move(1, 50, &mut rest)).sum();
        assert_eq!(moved, 5);
        let mut rest = 0;
        let moved: i32 = (0..10).map(|_| scaled_move(-3, 50, &mut rest)).sum();
        assert_eq!(moved, -15);
        let mut rest = 0;
        assert_eq!(scaled_move(7, 200, &mut rest), 14);
        assert_eq!(rest, 0);
    }
}
