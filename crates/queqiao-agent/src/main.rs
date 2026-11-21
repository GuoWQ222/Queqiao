//! queqiao-agent: the controlled side. Captures the screen, encodes it and streams it to the viewer, and
//! injects the viewer's mouse and keyboard input. Runs in the logged-on user's desktop session.
//!
//! With an account (M3): the agent logs in to queqiao-server, stays online and serves each viewer of the
//! same account that connects, one at a time.
//!   queqiao-agent --server <host:port> --pin <fingerprint> --user <name> --password <password>
//!       logs in and saves the login (the refresh token, DPAPI-encrypted); later starts need no options:
//!   queqiao-agent [--sessions N] [--seconds N]
//! By IP address (M2, for tests):
//!   queqiao-agent --connect <viewer-ip:port> | --listen <ip:port>
//! Common options: [--seconds N] [--fps N] [--report <file>] [--verify-input] [--force <switches>]
//! --seconds limits the whole run; --sessions N stops after N sessions. --trust-new-key <device id> accepts
//! that device's new key (after a reinstall; sessions with a device whose key changed are refused otherwise). --verify-input checks every injected
//! pointer move and the test key (F24) and puts the pointer back at the end. --version prints the version.

#![windows_subsystem = "windows"]

mod autostart;
mod power;
mod service_link;
mod ui;
mod update;
mod wake;
mod wol_check;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use queqiao_account::{
    Account, AccountEvent, ClientMessage, DeviceIdentity, Role as AccountRole, SavedLogin,
};
use queqiao_audio::{Capture, TEST_TONE_HZ, Tone};
use queqiao_capture::{BgraImage, Duplicator, OutputInfo, needs_restart, qpc_frequency, qpc_now};
use queqiao_codec::ScreenEncoder;
use queqiao_core::protocol::MAX_CLIPBOARD_TEXT;
use queqiao_core::protocol::{Command, InputEvent, Message};
use queqiao_core::text::trf;
use queqiao_display::{
    AccountState, LoginAction, LoginFields, LoginForm, PresenceEvent, normalize_code,
};
use queqiao_files::{ImageReceiver, ImageSender, image_fingerprint, test_picture};
use queqiao_files::{Outcome, Transfers, downloads_folder};
use queqiao_input::{
    ClipContent, Clipboard, Injector, ScreenRect, cursor_position, key_is_down, set_cursor_position,
};
use queqiao_local::FromHelper;
use queqiao_terminal::{TerminalMessage, Terminals};
use queqiao_transport::{
    Addresses, Peer, PeerEvent, Role, START_BITRATE, Signal, Signaling, Stream, check_hello,
    hello_body,
};
use service_link::Links;
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};

/// The test key: F24 exists on no normal keyboard and no program reacts to it.
const TEST_SCANCODE: u16 = 0x76;
const VK_F24: u16 = 0x87;
/// Adaptive bitrate: the encoder never goes below this, changes at most this often, and runs at this share of
/// the estimated bandwidth (the rest is headroom for retransmissions and the pacer).
const MIN_BITRATE: u32 = 500_000;
const BITRATE_CHANGE_GAP: Duration = Duration::from_millis(250);
const BANDWIDTH_SHARE: f64 = 0.85;

/// The encoder bitrate for a bandwidth estimate.
fn adapted_bitrate(estimate: u64, max: u32) -> u32 {
    ((estimate as f64 * BANDWIDTH_SHARE) as u64)
        .clamp(u64::from(MIN_BITRATE), u64::from(max.max(MIN_BITRATE))) as u32
}

/// How long the person at the PC has to allow a session (the viewer waits 30 s for the answer).
const CONSENT_SECONDS: u32 = 25;

/// A session that is not connected after this long is given up.
const CONNECT_LIMIT: Duration = Duration::from_secs(30);

#[derive(Default)]
struct Options {
    /// A device whose new key the person confirmed (its old pin is dropped).
    trust_new_key: Option<String>,
    /// Ask the person at the PC before each session in this run (whatever the setting says).
    ask: bool,
    /// For tests: answer the consent prompt (true = Allow) as a click would, after 1 s.
    answer: Option<bool>,
    /// For tests: press the banner's End button this many seconds into a session.
    end_after: Option<u64>,
    /// For tests: save pictures of the banner and the consent prompt in this folder.
    ui_pictures: Option<PathBuf>,
    /// For tests: send a clipboard text once the session's data channel is open.
    clipboard_test: bool,
    /// Files to send to the viewer once the session's files channel is open (tests).
    send_files: Vec<PathBuf>,
    /// Report the viewer's commands (lock, restart) instead of carrying them out (tests).
    dry_run_commands: bool,
    /// Capture the sound of this render device (part of its name) instead of the default one.
    sound_device: Option<String>,
    /// Play a test tone into the sound device during the session (tests: use a virtual device).
    sound_test: bool,
    /// The privacy screen as a small square of a test colour, checked against what capture sees (tests).
    privacy_test: bool,
    /// Take help requests from the start with this code (tests; people use the tray menu).
    help_code: Option<String>,
    /// Write the help ID into this file once the server gave it (tests).
    help_id_file: Option<PathBuf>,
    /// A fixed help code for this run (tests; people set it in the tray).
    fixed_help_code: Option<String>,
    /// The Wake-on-LAN set-up check into the report, then end.
    wol_check: bool,
    /// A throwaway device with this name instead of this PC's own (tests with two accounts on one PC).
    test_device: Option<String>,
    /// Lock this PC when a session ends, for this run only (tests: with --dry-run-commands it is only reported).
    lock_after_session: bool,
    /// In a helper's session, choose a tray setting as a click would (tests: it must be refused).
    test_press_setting: bool,
    /// Start not logged in and log in through the tray's "Log in..." and its form, filled with --server, --pin,
    /// --user and --password (tests).
    test_tray_login: bool,
    connect: Option<String>,
    listen: Option<String>,
    server: Option<String>,
    pin: Option<String>,
    user: Option<String>,
    password: Option<String>,
    sessions: Option<u32>,
    seconds: Option<u64>,
    fps: Option<u32>,
    report: Option<PathBuf>,
    verify_input: bool,
    /// The channels the Queqiao service hands over when it starts the agent (`<read>,<write>` handles).
    service_channel: Option<String>,
    /// The agent for the sign-in screen (the service starts it while nobody is signed in): who it is comes from the
    /// service, in memory; it shows nothing itself and keeps nothing.
    logon: bool,
    desktop_channel: Option<String>,
}

#[derive(Default)]
struct Stats {
    captured: u32,
    sent: u32,
    keyframes: u32,
    bytes: usize,
    keyframe_requests: u32,
    inputs: u32,
    inject_failures: u32,
    last_inject_error: String,
    moves_checked: u32,
    moves_ok: u32,
    /// Game mouse: relative moves checked, and those that moved the pointer the right way.
    deltas_checked: u32,
    deltas_ok: u32,
    keys_checked: u32,
    keys_ok: u32,
    agent_ms: Vec<f64>,
}

fn main() {
    queqiao_core::platform::install_crash_log("queqiao-agent");
    // Tests: a crash on purpose, to see the report written.
    if std::env::args().any(|arg| arg == "--crash-test") {
        panic!("crash test (asked for with --crash-test)");
    }
    let banner = queqiao_core::banner(env!("CARGO_PKG_NAME"));
    let Some(options) = parse(std::env::args().skip(1)) else {
        println!("{banner}");
        println!(
            "usage: queqiao-agent [--server <host:port> --pin <fingerprint> --user <name> --password <password>] [--sessions N]"
        );
        println!("       queqiao-agent --connect <viewer-ip:port> | --listen <ip:port>");
        println!("       common: [--seconds N] [--fps N] [--report <file>] [--verify-input]");
        return;
    };
    let mut lines = vec![banner];
    let mut not_logged_in = false;
    let mut already_running = false;
    let passed = match run(&options, &mut lines) {
        Ok(passed) => passed,
        Err(error) => {
            not_logged_in = error.starts_with("not logged in");
            already_running = error == autostart::ALREADY_RUNNING;
            lines.push(format!("ERROR {error}"));
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
    // Started by the service: 3 tells it there is nothing to do until the next sign-in. 4: another agent runs in this
    // session (the installer does not take that for a failed start).
    let code = if passed {
        0
    } else if not_logged_in && options.service_channel.is_some() {
        3
    } else if already_running {
        4
    } else {
        1
    };
    std::process::exit(code);
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
            "--sessions" => options.sessions = args.next().and_then(|v| v.parse().ok()),
            "--seconds" => options.seconds = args.next().and_then(|v| v.parse().ok()),
            "--fps" => options.fps = args.next().and_then(|v| v.parse().ok()),
            "--report" => options.report = args.next().map(PathBuf::from),
            "--verify-input" => options.verify_input = true,
            "--trust-new-key" => options.trust_new_key = args.next(),
            "--ask" => options.ask = true,
            "--answer" => options.answer = Some(args.next()? == "allow"),
            "--end-after" => options.end_after = args.next().and_then(|v| v.parse().ok()),
            "--ui-pictures" => options.ui_pictures = args.next().map(PathBuf::from),
            "--clipboard-test" => options.clipboard_test = true,
            "--send-file" => options.send_files.push(PathBuf::from(args.next()?)),
            "--dry-run-commands" => options.dry_run_commands = true,
            "--sound-device" => options.sound_device = Some(args.next()?),
            "--sound-test" => options.sound_test = true,
            "--privacy-test" => options.privacy_test = true,
            "--help-code" => options.help_code = Some(normalize_code(&args.next()?)),
            "--help-id-file" => options.help_id_file = Some(PathBuf::from(args.next()?)),
            "--fixed-help-code" => options.fixed_help_code = Some(normalize_code(&args.next()?)),
            "--wol-check" => options.wol_check = true,
            "--test-device" => options.test_device = Some(args.next()?),
            "--lock-after-session" => options.lock_after_session = true,
            "--test-press-setting" => options.test_press_setting = true,
            "--test-tray-login" => options.test_tray_login = true,
            "--service-channel" => options.service_channel = Some(args.next()?),
            "--logon" => options.logon = true,
            "--desktop-channel" => options.desktop_channel = Some(args.next()?),
            "--force" => queqiao_core::set_forced(&args.next()?),
            _ => return None,
        }
    }
    Some(options)
}

fn run(options: &Options, lines: &mut Vec<String>) -> Result<bool, String> {
    lines.extend(queqiao_core::platform::prepare_process());
    if options.wol_check {
        let check = wol_check::check();
        lines.extend(wol_check::report(&check));
        lines.push("--- the advice shown in the tray:".into());
        lines.extend(wol_check::advice(&check).lines().map(str::to_string));
        return Ok(true);
    }
    let mut links = Links::open(
        options.service_channel.as_deref(),
        options.desktop_channel.as_deref(),
    )?;
    if links.service.is_some() {
        lines.push(
            "started by the Queqiao service: the sign-in, lock, Ctrl+Alt+Del and UAC screens come through its desktop helper"
                .into(),
        );
    }
    let handed = if options.logon {
        let service = links
            .service
            .as_mut()
            .ok_or("--logon is for the Queqiao service only")?;
        let identity = service.identity(Duration::from_secs(10))?;
        lines.push(format!(
            "the agent for the sign-in screen, as {} ({})",
            identity.device_name, identity.device_id
        ));
        Some(identity)
    } else {
        None
    };
    // Started without options (at sign-in, or by hand): one agent per user session.
    if !options.logon
        && options.server.is_none()
        && options.connect.is_none()
        && options.listen.is_none()
    {
        // With the Queqiao service running, Queqiao for the person signed in is the one it starts (the Run key's
        // copy, started at sign-in as well, leaves).
        if options.service_channel.is_none() && autostart::service_runs() {
            lines.push("the Queqiao service runs Queqiao on this PC: this copy leaves".into());
            return Ok(true);
        }
        autostart::claim_single_instance()?;
    }
    let end = options
        .seconds
        .map(|s| Instant::now() + Duration::from_secs(s));
    // Nothing happens unseen: the tray icon whenever the agent runs, a banner during every session.
    // At the sign-in screen: only what the person allowed (sessions from the account's devices without asking),
    // nothing that needs someone signed in (clipboard, files, command lines).
    let settings = match &handed {
        Some(handed) => ui::Settings {
            allow_account_control: handed.allow_account_control,
            ask_before_session: false,
            share_clipboard: false,
            receive_files: false,
            allow_terminal: false,
            ..ui::Settings::default()
        },
        None => ui::Settings::load(),
    };
    let mut shown = settings.clone();
    shown.ask_before_session |= options.ask;
    let ui = if options.logon {
        ui::Ui::headless()?
    } else {
        ui::Ui::start(&shown, options.ui_pictures.clone())?
    };
    lines.push(format!(
        "tray icon {}; this account's devices may control this PC: {}; ask first: {}",
        if ui.tray_shown {
            "shown"
        } else {
            "not shown (no taskbar yet)"
        },
        if settings.allow_account_control {
            "yes"
        } else {
            "no"
        },
        if shown.ask_before_session {
            "yes"
        } else {
            "no"
        }
    ));
    if options.connect.is_none() && options.listen.is_none() {
        return run_account(options, end, &ui, settings, &mut links, handed, lines);
    }
    let mut signal = match (&options.connect, &options.listen) {
        (Some(address), _) => Signal::connect(address, Duration::from_secs(60)),
        (None, Some(address)) => Signal::listen(address, Duration::from_secs(120)),
        (None, None) => unreachable!("checked above"),
    }
    .map_err(|e| e.to_string())?;
    signal.hello("agent").map_err(|e| e.to_string())?;
    let viewer = signal.peer_address().map_err(|e| e.to_string())?;
    let mut deferred = Vec::new();
    let outcome = session(
        options,
        &mut signal,
        &viewer.to_string(),
        "direct",
        end,
        &ui,
        Permissions {
            clipboard: settings.share_clipboard,
            files: settings.receive_files,
            terminal: settings.allow_terminal,
            helper: false,
        },
        &mut links,
        &mut deferred,
        lines,
    )?;
    Ok(outcome.passed)
}

/// Logged in to queqiao-server: waits for viewers of the account and serves them one after another.
/// When the server connection drops, it reconnects with the saved login (waiting 1 s, 2 s, ... 30 s).
fn run_account(
    options: &Options,
    end: Option<Instant>,
    ui: &ui::Ui,
    mut settings: ui::Settings,
    links: &mut Links,
    handed: Option<queqiao_local::LogonIdentity>,
    lines: &mut Vec<String>,
) -> Result<bool, String> {
    // Tests run two accounts on one PC: a throwaway device (own key, pins in memory) for the second. At the sign-in
    // screen the device and its login are the person's, handed over by the service (in memory only).
    let identity = match (&handed, &options.test_device) {
        (Some(handed), _) => DeviceIdentity::handed_over(
            &handed.device_id,
            &handed.device_name,
            &handed.key_pkcs8,
            handed.pins.clone(),
        ),
        (None, Some(label)) => DeviceIdentity::in_memory(label),
        (None, None) => DeviceIdentity::load_or_create(),
    }
    .map_err(|e| e.to_string())?;
    let mut login = match &handed {
        Some(handed) => SavedLogin {
            server: handed.server.clone(),
            pin: handed.pin.clone(),
            refresh_token: Some(handed.refresh_token.clone()),
        },
        None => SavedLogin::load(AccountRole::Agent).unwrap_or_default(),
    };
    if let Some(server) = &options.server {
        if login.server != *server {
            login.refresh_token = None;
        }
        login.server = server.clone();
    }
    if let Some(pin) = &options.pin {
        login.pin = pin.clone();
    }
    // The person's own agent (started at sign-in, by the installer or from the Start menu) stays in the tray when
    // it cannot go online: not logged in (the tray offers "Log in..."), or the server out of reach (it tries again
    // after 1 s, 2 s, ... 30 s, in the background). Started for a test or for the sign-in screen, it ends with the
    // error instead.
    let tolerant = handed.is_none()
        && (options.test_tray_login
            || (options.test_device.is_none()
                && options.server.is_none()
                && options.user.is_none()
                && options.password.is_none()
                && options.trust_new_key.is_none()));
    // A login is kept for this PC's own device only (not for a throwaway test device, nor the sign-in screen's).
    let keeps_login = handed.is_none() && options.test_device.is_none();
    let mut state = AccountState::Online;
    let mut account = if options.test_tray_login || login.server.is_empty() {
        if !tolerant {
            return Err("not logged in: start once with --server <host:port> --pin <fingerprint> --user <name> --password <password>".into());
        }
        state = AccountState::LoggedOut;
        lines.push("not logged in: Log in... in the tray".into());
        None
    } else {
        let password = options.user.as_deref().zip(options.password.as_deref());
        match Account::connect(&mut login, &identity, AccountRole::Agent, password) {
            Ok(account) => Some(account),
            Err(error) if !tolerant => return Err(error.0),
            Err(error) => {
                state = if needs_new_login(&error.0) {
                    AccountState::LoggedOut
                } else {
                    AccountState::Offline
                };
                lines.push(format!("not online: {error}"));
                None
            }
        }
    };
    ui.set_account_state(state);

    // With the service: this device for its agent at the sign-in screen while the person lets the account's devices
    // in without asking (otherwise the service forgets it). Offered once online.
    let mut offer_logon = false;
    let (mut sessions, mut passed) = (0u32, true);
    let mut retry = Duration::from_secs(1);
    let mut next_try = Instant::now() + retry;
    // A connection attempt in the background while offline: (the result, the login as the attempt left it).
    type Attempt = std::thread::JoinHandle<(Result<Account, String>, SavedLogin)>;
    let mut attempt: Option<Attempt> = None;
    let mut deferred: Vec<PresenceEvent> = Vec::new();
    // This PC's network cards, so another device of the account can wake it later.
    let macs = wake::macs();
    // Help for someone of another account: the code while help requests are open (a fixed one keeps them open), and
    // whether the help window shows it (only when the person asked, or a test gave a code).
    let mut fixed_code: Option<String> = options
        .fixed_help_code
        .clone()
        .or_else(|| settings.fixed_help_code());
    let mut help_code: Option<String> = options.help_code.clone().or_else(|| fixed_code.clone());
    let mut help_window = options.help_code.is_some();
    // After five wrong codes the server closes help: a fixed code opens it again after a while (guessing stays slow).
    let mut fixed_reopen_at: Option<Instant> = None;
    // A helper the person allowed when it asked without a code (its next session request needs no prompt).
    let mut helper_allowed: Option<String> = None;
    let mut trust_new_key = options.trust_new_key.as_deref();
    if let Some(online) = account.as_mut() {
        went_online(
            online,
            &login,
            &identity,
            keeps_login,
            trust_new_key.take(),
            &macs,
            help_code.as_deref(),
            lines,
        )?;
        offer_logon = keeps_login;
    }
    if options.test_tray_login {
        ui.press_menu(queqiao_display::TEST_MENU_LOGIN);
    }
    // Updates, for the person's own agent of an installed Queqiao: put there by queqiao-setup, or started by the
    // Queqiao service (which updates its own copy).
    let updater = (tolerant
        && !options.test_tray_login
        && (update::installed_here() || links.service.is_some()))
    .then(|| update::Updater::start(lines));
    loop {
        if end.is_some_and(|end| Instant::now() > end)
            || options.sessions.is_some_and(|n| sessions >= n)
        {
            break;
        }
        // What the person did in the tray (also during the last session).
        deferred.extend(std::iter::from_fn(|| ui.try_event()));
        let mut quit = false;
        for event in deferred.drain(..) {
            match event {
                PresenceEvent::Quit => quit = true,
                PresenceEvent::SetAllow(on) => {
                    settings.allow_account_control = on;
                    let _ = settings.save();
                    offer_logon = handed.is_none() && options.test_device.is_none();
                    lines.push(format!(
                        "the person at this PC set: this account's devices may control it: {on}"
                    ));
                }
                PresenceEvent::SetAutostart(on) => match autostart::set(on) {
                    Ok(()) => lines.push(format!(
                        "the person at this PC set: start with Windows: {on}"
                    )),
                    Err(error) => {
                        lines.push(format!("start with Windows could not be set: {error}"))
                    }
                },
                PresenceEvent::SetLockAfter(on) => {
                    settings.lock_after_session = on;
                    let _ = settings.save();
                    lines.push(format!(
                        "the person at this PC set: lock this PC when a session ends: {on}"
                    ));
                }
                PresenceEvent::SetTerminal(on) => {
                    settings.allow_terminal = on;
                    let _ = settings.save();
                    lines.push(format!(
                        "the person at this PC set: allow remote command lines: {on}"
                    ));
                }
                PresenceEvent::SetFiles(on) => {
                    settings.receive_files = on;
                    let _ = settings.save();
                    lines.push(format!("the person at this PC set: receive files: {on}"));
                }
                PresenceEvent::SetClipboard(on) => {
                    settings.share_clipboard = on;
                    let _ = settings.save();
                    lines.push(format!(
                        "the person at this PC set: share the clipboard: {on}"
                    ));
                }
                PresenceEvent::SetAsk(on) => {
                    settings.ask_before_session = on;
                    let _ = settings.save();
                    offer_logon = handed.is_none() && options.test_device.is_none();
                    lines.push(format!(
                        "the person at this PC set: ask before each session: {on}"
                    ));
                }
                wanted @ (PresenceEvent::OpenHelp | PresenceEvent::HelpNewCode) => match account.as_mut() {
                    // The window shows the fixed code when there is one; New code: a one-time code instead.
                    Some(online) => {
                        let code = match (&fixed_code, matches!(wanted, PresenceEvent::OpenHelp)) {
                            (Some(fixed), true) => fixed.clone(),
                            _ => new_help_code(),
                        };
                        online.open_help(&code);
                        help_code = Some(code);
                        help_window = true;
                    }
                    None => {
                        lines.push("help: not online".into());
                        // Not on this thread: it keeps trying to go online meanwhile.
                        std::thread::spawn(|| {
                            queqiao_display::show_error(
                                queqiao_core::text::tr("Queqiao"),
                                queqiao_core::text::tr("This PC is not online, so nobody can be let in to help. Log in from the tray icon, or check the network."),
                            )
                        });
                    }
                },
                PresenceEvent::HelpClose => {
                    help_window = false;
                    match (account.as_mut(), &fixed_code) {
                        // Back to the fixed code, without the window.
                        (Some(online), Some(fixed)) => {
                            online.open_help(fixed);
                            help_code = Some(fixed.clone());
                            lines.push("help: the window closed; open again with the fixed code".into());
                        }
                        (online, _) => {
                            if let Some(online) = online {
                                online.close_help();
                            }
                            help_code = None;
                            lines.push("help: closed by the person at this PC".into());
                        }
                    }
                }
                PresenceEvent::SetFixedHelpCode(code) => {
                    match settings.set_fixed_help_code(code.as_deref()) {
                        Ok(()) => lines.push(format!(
                            "the person at this PC {} a fixed help code",
                            if code.is_some() { "set" } else { "removed" }
                        )),
                        Err(error) => lines.push(format!("the fixed help code was not kept: {error}")),
                    }
                    fixed_code = code;
                    ui.set_fixed_code(fixed_code.is_some());
                    if !help_window && let Some(online) = account.as_mut() {
                        match &fixed_code {
                            Some(fixed) => {
                                online.open_help(fixed);
                                help_code = Some(fixed.clone());
                            }
                            None => {
                                online.close_help();
                                help_code = None;
                            }
                        }
                    }
                }
                PresenceEvent::LogIn if account.is_none() && tolerant => {
                    if let Some((mut new, tried)) = tray_log_in(options, &login, &identity, lines) {
                        login = tried;
                        went_online(&mut new, &login, &identity, keeps_login, trust_new_key.take(), &macs, help_code.as_deref(), lines)?;
                        lines.push("logged in from the tray".into());
                        account = Some(new);
                        // An attempt with the old login still running is let go (its connection closes when it ends).
                        attempt = None;
                        retry = Duration::from_secs(1);
                        state = AccountState::Online;
                        ui.set_account_state(state);
                        offer_logon = keeps_login;
                    }
                }
                PresenceEvent::LogIn => {}
                PresenceEvent::CheckWakeOnLan => {
                    // powercfg takes a moment: not on this thread.
                    std::thread::spawn(|| {
                        let advice = wol_check::advice(&wol_check::check());
                        queqiao_display::show_info(queqiao_core::text::tr("Queqiao"), &advice);
                    });
                }
                PresenceEvent::CheckUpdates => match &updater {
                    Some(updater) => {
                        lines.push("updates: the person asked for a look".into());
                        updater.request();
                    }
                    None => notify(
                        false,
                        queqiao_core::text::tr(
                            "Updates come with an installed Queqiao (installed with queqiao-setup).",
                        )
                        .to_string(),
                    ),
                },
                PresenceEvent::SetAutoUpdate(on) => {
                    settings.auto_update = on;
                    let _ = settings.save();
                    lines.push(format!(
                        "the person at this PC set: install updates automatically: {on}"
                    ));
                }
                PresenceEvent::SettingsLocked => {
                    lines.push("a setting was chosen while someone of another account controls this PC: refused".into())
                }
                PresenceEvent::SetPassword(password) => {
                    settings.settings_password = password.as_deref().map(queqiao_account::hash_local_password).unwrap_or_default();
                    let _ = settings.save();
                    let hash = (!settings.settings_password.is_empty()).then(|| settings.settings_password.clone());
                    ui.set_password_hash(hash);
                    lines.push(format!(
                        "the person at this PC {} the settings password",
                        if password.is_some() { "set" } else { "removed" }
                    ));
                }
                PresenceEvent::EndSession | PresenceEvent::Consent(_) => {}
            }
        }
        if quit {
            lines.push("quit from the tray icon".into());
            break;
        }
        // An update found: installed between sessions, by itself if the person lets it, or when they asked and agree.
        if let Some(found) = updater.as_ref().and_then(update::Updater::try_result) {
            match found {
                update::Found::UpToDate { by_hand } => {
                    lines.push(format!("updates: {} is the latest", queqiao_core::VERSION));
                    if by_hand {
                        notify(
                            false,
                            trf(
                                "Queqiao is up to date (version {version}).",
                                &[("version", queqiao_core::VERSION)],
                            ),
                        );
                    }
                }
                update::Found::Skipped { version, by_hand } => {
                    lines.push(format!(
                        "updates: {version} did not keep running here before: skipped"
                    ));
                    if by_hand {
                        notify(
                            false,
                            trf(
                                "Queqiao {version} did not start on this PC and was put back. Queqiao waits for a later version.",
                                &[("version", &version)],
                            ),
                        );
                    }
                }
                update::Found::Failed { error, by_hand } => {
                    lines.push(format!("updates: {error}"));
                    if by_hand {
                        notify(
                            true,
                            trf("The update check failed: {error}", &[("error", &error)]),
                        );
                    }
                }
                update::Found::Ready {
                    version,
                    notes,
                    installer,
                    manifest,
                    signature,
                    by_hand,
                } => {
                    lines.push(format!("updates: {version} downloaded and checked"));
                    let install = if by_hand {
                        queqiao_display::confirm(
                            windows::Win32::Foundation::HWND::default(),
                            queqiao_core::text::tr("Queqiao"),
                            &trf(
                                "Queqiao {version} is ready to install. Queqiao closes for a moment and starts again.\n\n{notes}\n\nInstall it now?",
                                &[("version", &version), ("notes", &notes)],
                            ),
                        )
                    } else {
                        settings.auto_update
                    };
                    if install {
                        match links.service.as_mut() {
                            // The service replaces its programs and starts this agent again.
                            Some(service) => {
                                if let Err(error) = update::apply_through_service(
                                    service, &installer, &version, &manifest, &signature, lines,
                                ) {
                                    lines.push(format!("updates: {error}"));
                                }
                            }
                            None => match update::apply(&installer, &version) {
                                Ok(()) => {
                                    lines.push(format!(
                                        "updates: installing {version}; this agent leaves"
                                    ));
                                    break;
                                }
                                Err(error) => lines
                                    .push(format!("updates: the installer did not start: {error}")),
                            },
                        }
                    }
                }
            }
        }
        // Offline or not logged in: the tray, and while offline an attempt in the background now and then.
        let Some(online) = account.as_mut() else {
            if let Some(worker) = attempt.take_if(|worker| worker.is_finished()) {
                let outcome = worker
                    .join()
                    .map_err(|_| "the connection attempt failed".to_string());
                match outcome {
                    Ok((Ok(mut new), tried)) => {
                        login = tried;
                        went_online(
                            &mut new,
                            &login,
                            &identity,
                            keeps_login,
                            trust_new_key.take(),
                            &macs,
                            help_code.as_deref(),
                            lines,
                        )?;
                        lines.push("reconnected".into());
                        account = Some(new);
                        retry = Duration::from_secs(1);
                        state = AccountState::Online;
                        ui.set_account_state(state);
                        offer_logon = keeps_login;
                    }
                    Ok((Err(error), _)) if needs_new_login(&error) => {
                        if !tolerant {
                            return Err(error);
                        }
                        lines.push(format!("not online: {error}"));
                        state = AccountState::LoggedOut;
                        ui.set_account_state(state);
                    }
                    Ok((Err(_), _)) | Err(_) => {
                        retry = (retry * 2).min(Duration::from_secs(30));
                        next_try = Instant::now() + retry;
                    }
                }
            } else if attempt.is_none()
                && state == AccountState::Offline
                && Instant::now() >= next_try
            {
                if !tolerant && end.is_some_and(|end| next_try + retry > end) {
                    return Err("the server connection was lost".into());
                }
                let (mut tried, identity) = (login.clone(), identity.clone());
                attempt = Some(std::thread::spawn(move || {
                    let result = Account::connect(&mut tried, &identity, AccountRole::Agent, None);
                    (result.map_err(|error| error.0), tried)
                }));
            }
            if let Some(event) = ui.wait_event(Duration::from_millis(100)) {
                deferred.push(event);
            }
            continue;
        };
        if fixed_reopen_at.is_some_and(|at| Instant::now() >= at)
            && let Some(fixed) = &fixed_code
        {
            fixed_reopen_at = None;
            online.open_help(fixed);
            help_code = Some(fixed.clone());
            lines.push("help: open again with the fixed code".into());
        }
        if offer_logon && let Some(service) = links.service.as_mut() {
            offer_logon = false;
            let allowed = settings.allow_account_control && !settings.ask_before_session;
            let offer = allowed
                .then(|| logon_identity(&identity, &login, online))
                .flatten();
            let handing = offer.is_some();
            match service.offer_logon(offer) {
                Ok(()) if handing => lines.push(
                    "the sign-in screen: this device handed to the Queqiao service for it".into(),
                ),
                Ok(()) => lines.push(
                    "the sign-in screen: not offered (control without asking is off here)".into(),
                ),
                Err(error) => lines.push(format!("the sign-in screen: {error}")),
            }
        }
        match online.next_event(Duration::from_millis(200)) {
            Some(AccountEvent::Signal {
                from,
                from_name,
                from_role: AccountRole::Viewer,
                session: id,
                kind,
                body,
                signature,
            }) if kind == "hello" => {
                // Only a device of this account whose key matches its pin, signing what it sends.
                if let Err(error) =
                    online.check_signal(&from, AccountRole::Viewer, &id, &kind, &body, &signature)
                {
                    lines.push(format!("refused a session request: {error}"));
                    continue;
                }
                // A device of another account, joined by this PC's help code.
                let helper = online.helper_account(&from).map(str::to_string);
                let shown_name = match &helper {
                    Some(user) => trf(
                        "{viewer} (account {user}) is helping on this PC",
                        &[("viewer", &from_name), ("user", user)],
                    ),
                    None => from_name.clone(),
                };
                let mut signal = online.session(&from, AccountRole::Viewer, &id);
                if let Err(error) = check_hello(&body, "agent") {
                    let _ = signal.send("error", &error.0);
                    lines.push(format!("refused viewer {from_name}: {error}"));
                    continue;
                }
                if helper.is_none() && !settings.allow_account_control {
                    let _ = signal.send(
                        "error",
                        "control by this account's devices is turned off on that PC",
                    );
                    lines.push(format!(
                        "refused viewer {from_name}: control by this account's devices is off"
                    ));
                    continue;
                }
                // Someone of another account is always asked about, whatever the settings.
                let asked_already =
                    helper.is_some() && helper_allowed.as_deref() == Some(from.as_str());
                if asked_already {
                    helper_allowed = None;
                } else if helper.is_some() || options.ask || settings.ask_before_session {
                    let question = match &helper {
                        Some(user) => trf(
                            "{viewer} (account {user}) wants to control this PC to help you.",
                            &[("viewer", &from_name), ("user", user)],
                        ),
                        None => trf(
                            "{viewer}, a device of your account, wants to control this PC.",
                            &[("viewer", &from_name)],
                        ),
                    };
                    match ask_consent(ui, &question, options.answer) {
                        Ok(true) => {
                            lines.push(format!("the person at this PC allowed {from_name}"))
                        }
                        Ok(false) => {
                            let _ = signal.send("error", "the person at that PC declined");
                            lines.push(format!("the person at this PC declined {from_name}"));
                            continue;
                        }
                        Err(error) => {
                            let _ = signal
                                .send("error", "the consent prompt could not be shown on that PC");
                            lines.push(format!(
                                "refused viewer {from_name}: consent prompt: {error}"
                            ));
                            continue;
                        }
                    }
                }
                signal
                    .send("hello", &hello_body("agent"))
                    .map_err(|e| e.to_string())?;
                sessions += 1;
                lines.push(format!("session {sessions}: viewer {from_name} ({from})"));
                if let Some(user) = &helper {
                    lines.push(format!(
                        "helper session: {from_name} of account {user}; file transfer, command lines, the privacy screen and restarting are off"
                    ));
                    // The helper holds the mouse: this PC's settings are locked until the session ends.
                    ui.lock_settings(true);
                    if options.test_press_setting {
                        ui.press_menu(queqiao_display::TEST_MENU_TERMINAL);
                    }
                }
                let started_at = std::time::SystemTime::now();
                match session(
                    options,
                    &mut signal,
                    &shown_name,
                    &from,
                    end,
                    ui,
                    Permissions {
                        clipboard: settings.share_clipboard,
                        files: settings.receive_files && helper.is_none(),
                        terminal: settings.allow_terminal && helper.is_none(),
                        helper: helper.is_some(),
                    },
                    links,
                    &mut deferred,
                    lines,
                ) {
                    Ok(outcome) => {
                        passed &= outcome.passed;
                        ui::record_session(&from_name, &from, started_at, &outcome.ended);
                        lock_after(options, &settings, lines);
                        ui.lock_settings(false);
                    }
                    Err(error) => {
                        ui.lock_settings(false);
                        lock_after(options, &settings, lines);
                        lines.push(format!("session {sessions} failed: {error}"));
                        ui::record_session(
                            &from_name,
                            &from,
                            started_at,
                            &format!("failed: {error}"),
                        );
                        passed = false;
                    }
                }
            }
            Some(AccountEvent::Closed(reason)) => {
                lines.push(format!("server connection lost: {reason}"));
                if !tolerant && end.is_some_and(|end| Instant::now() + retry > end) {
                    return Err("the server connection was lost".into());
                }
                // Tried again in the background (above), the tray responding meanwhile.
                account = None;
                retry = Duration::from_secs(1);
                next_try = Instant::now() + retry;
                state = AccountState::Offline;
                ui.set_account_state(state);
            }
            Some(AccountEvent::Error(message)) => lines.push(format!("server: {message}")),
            Some(AccountEvent::Notice(message)) => lines.push(format!("notice: {message}")),
            Some(AccountEvent::HelpOpen(help_id)) => {
                lines.push(format!("help: open, help ID {help_id}"));
                if let Some(path) = &options.help_id_file {
                    let _ = std::fs::write(path, &help_id);
                }
                if help_window && let Some(code) = &help_code {
                    ui.show_help(Some((&help_id, code)));
                }
            }
            Some(AccountEvent::WakeRequest { name, macs }) => match wake::send(&macs) {
                Ok(sent) => lines.push(format!("wake: sent {sent} wake-up packets for {name}")),
                Err(error) => lines.push(format!("wake: could not wake {name}: {error}")),
            },
            Some(AccountEvent::HelpAsked {
                request,
                device,
                user,
            }) => {
                // Only while this PC takes help requests; the person decides.
                let allowed = help_code.is_some() && {
                    let question = trf(
                        "{viewer} (account {user}) asks to help you on this PC. Allow it?",
                        &[("viewer", &device.name), ("user", &user)],
                    );
                    matches!(ask_consent(ui, &question, options.answer), Ok(true))
                };
                online.answer_help(request, allowed);
                lines.push(format!(
                    "help: {} of account {user} asked without a code: {}",
                    device.name,
                    if allowed { "allowed" } else { "declined" }
                ));
                if allowed {
                    lines.push(format!("the person at this PC allowed {}", device.name));
                    // Its session request that follows is not asked about again.
                    helper_allowed = Some(device.id.clone());
                }
            }
            Some(AccountEvent::HelpClosed(reason)) => {
                lines.push(format!("help: closed: {reason}"));
                help_code = None;
                help_window = false;
                ui.show_help(None);
                if fixed_code.is_some() {
                    fixed_reopen_at = Some(Instant::now() + FIXED_CODE_PAUSE);
                    lines.push(format!(
                        "help: the fixed code takes requests again in {} minutes",
                        FIXED_CODE_PAUSE.as_secs() / 60
                    ));
                }
            }
            Some(AccountEvent::HelpRequested { device, user }) => {
                lines.push(format!(
                    "help: {} of account {user} was let in (code given or ask allowed)",
                    device.name
                ));
                // The code is used up: the fixed one again, or a new one while the window is open.
                if help_code.is_some() {
                    let code = match &fixed_code {
                        Some(fixed) if !help_window => fixed.clone(),
                        _ => new_help_code(),
                    };
                    online.open_help(&code);
                    if fixed_code.as_ref() == Some(&code) {
                        lines.push("help: open again with the fixed code".into());
                    }
                    help_code = Some(code);
                }
            }
            Some(
                AccountEvent::Devices(_)
                | AccountEvent::Signal { .. }
                | AccountEvent::IceServers { .. }
                | AccountEvent::PasswordChanged(_)
                | AccountEvent::HelpGranted(_),
            )
            | None => {}
        }
    }
    lines.push(format!("{sessions} sessions"));
    Ok(passed && options.sessions.is_none_or(|n| sessions >= n))
}

/// A message for the person at this PC, on a thread of its own (the agent goes on meanwhile).
fn notify(error: bool, text: String) {
    std::thread::spawn(move || {
        let title = queqiao_core::text::tr("Queqiao");
        if error {
            queqiao_display::show_error(title, &text);
        } else {
            queqiao_display::show_info(title, &text);
        }
    });
}

/// Whether the server wants the password again (the saved login expired, was revoked, or the password changed).
fn needs_new_login(error: &str) -> bool {
    error.contains("log in again")
}

/// Just online: the login kept (for this PC's own device), a new key pinned if asked, this PC's network cards sent
/// (so another device of the account can wake it), and help requests open again if they were.
#[allow(clippy::too_many_arguments)]
fn went_online(
    account: &mut Account,
    login: &SavedLogin,
    identity: &DeviceIdentity,
    keep_login: bool,
    trust_new_key: Option<&str>,
    macs: &[String],
    help_code: Option<&str>,
    lines: &mut Vec<String>,
) -> Result<(), String> {
    if keep_login {
        login.save(AccountRole::Agent).map_err(|e| e.to_string())?;
    }
    if let Some(device) = trust_new_key {
        account.trust_new_key(device).map_err(|e| e.to_string())?;
        lines.push(format!("the key of device {device} will be pinned anew"));
    }
    lines.push(format!(
        "logged in as {} at {}; this device: {} ({}); {} devices on the account",
        account.user,
        login.server,
        identity.name,
        identity.id,
        account.devices.len()
    ));
    if !macs.is_empty() {
        account.send(ClientMessage::DeviceInfo {
            macs: macs.to_vec(),
        });
    }
    if let Some(code) = help_code {
        account.open_help(code);
    }
    Ok(())
}

/// "Log in..." in the tray: the login form (the server, its certificate fingerprint, the user name and the
/// password), the login itself in the background so the form keeps responding. The account and the login as it
/// now is, or None when the person closed the form.
fn tray_log_in(
    options: &Options,
    login: &SavedLogin,
    identity: &DeviceIdentity,
    lines: &mut Vec<String>,
) -> Option<(Account, SavedLogin)> {
    let defaults = LoginFields {
        server: login.server.clone(),
        pin: login.pin.clone(),
        user: String::new(),
        password: String::new(),
    };
    let mut form = match LoginForm::new(
        queqiao_core::text::tr("Queqiao - log in this PC"),
        &defaults,
    ) {
        Ok(form) => form,
        Err(error) => {
            lines.push(format!("login form: {}", error.message()));
            return None;
        }
    };
    if options.test_tray_login {
        form.set_fields(&LoginFields {
            server: options.server.clone().unwrap_or_default(),
            pin: options.pin.clone().unwrap_or_default(),
            user: options.user.clone().unwrap_or_default(),
            password: options.password.clone().unwrap_or_default(),
        });
        let drawn = Instant::now() + Duration::from_millis(500);
        while Instant::now() < drawn {
            let _ = form.next(20);
        }
        if let Some(folder) = &options.ui_pictures {
            let _ = form.save_picture(&folder.join("agent_login.bmp"));
        }
        form.submit();
    }
    loop {
        match form.next(15) {
            Some(LoginAction::Cancel) => {
                lines.push("login: the form was closed".into());
                return None;
            }
            Some(LoginAction::Submit(fields)) => {
                if [&fields.server, &fields.pin, &fields.user, &fields.password]
                    .iter()
                    .any(|value| value.is_empty())
                {
                    form.set_status(queqiao_core::text::tr("Please fill in all four fields."));
                    continue;
                }
                let mut tried = login.clone();
                if tried.server != fields.server {
                    tried.refresh_token = None;
                }
                tried.server = fields.server.clone();
                tried.pin = fields.pin.clone();
                form.set_busy(true, queqiao_core::text::tr("Logging in..."));
                let identity = identity.clone();
                let worker = std::thread::spawn(move || {
                    let result = Account::connect(
                        &mut tried,
                        &identity,
                        AccountRole::Agent,
                        Some((&fields.user, &fields.password)),
                    );
                    (result.map_err(|error| error.0), tried)
                });
                while !worker.is_finished() {
                    if let Some(LoginAction::Cancel) = form.next(15) {
                        lines.push("login: the form was closed".into());
                        return None;
                    }
                }
                match worker.join() {
                    Ok((Ok(account), tried)) => return Some((account, tried)),
                    Ok((Err(error), _)) => {
                        lines.push(format!("login: {error}"));
                        if options.test_tray_login {
                            return None;
                        }
                        form.set_busy(false, &error);
                    }
                    Err(_) => return None,
                }
            }
            None => {}
        }
    }
}

/// Asks the person at the PC whether `viewer` may control it; false when denied or unanswered.
fn ask_consent(ui: &ui::Ui, question: &str, answer: Option<bool>) -> Result<bool, String> {
    ui.ask(question, CONSENT_SECONDS)?;
    if let Some(answer) = answer {
        std::thread::sleep(Duration::from_secs(1));
        ui.press_consent(answer);
    }
    let deadline = Instant::now() + Duration::from_secs(u64::from(CONSENT_SECONDS) + 2);
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Ok(false);
        }
        if let Some(PresenceEvent::Consent(allowed)) = ui.wait_event(left) {
            return Ok(allowed);
        }
    }
}

/// The local keyboard and mouse back (the privacy screen went, or Windows' own screen came up).
fn end_local_block(block: &mut Option<queqiao_input::LocalInputBlock>, lines: &mut Vec<String>) {
    if let Some(block) = block.take() {
        lines.push(format!(
            "privacy: the local keyboard and mouse work again ({} local events were held back)",
            block.swallowed()
        ));
    }
}

/// The privacy test's square: 320 x 200 pixels, 100 pixels in from the captured monitor's top left corner.
const PRIVACY_TEST_SIZE: (i32, i32) = (320, 200);
const PRIVACY_TEST_OFFSET: i32 = 100;
/// Magenta (0x00BBGGRR), rare on a desktop.
const PRIVACY_TEST_COLOR: u32 = 0x00FF_00FF;

fn privacy_test_area(left: i32, top: i32) -> RECT {
    RECT {
        left: left + PRIVACY_TEST_OFFSET,
        top: top + PRIVACY_TEST_OFFSET,
        right: left + PRIVACY_TEST_OFFSET + PRIVACY_TEST_SIZE.0,
        bottom: top + PRIVACY_TEST_OFFSET + PRIVACY_TEST_SIZE.1,
    }
}

/// Pixels of the test colour inside the test square of a captured picture (monitor coordinates).
fn privacy_test_pixels(image: &BgraImage) -> usize {
    let (width, height) = (image.width as usize, image.height as usize);
    let start = PRIVACY_TEST_OFFSET as usize;
    let (right, bottom) = (
        (start + PRIVACY_TEST_SIZE.0 as usize).min(width),
        (start + PRIVACY_TEST_SIZE.1 as usize).min(height),
    );
    (start..bottom)
        .flat_map(|y| (start..right).map(move |x| (y * width + x) * 4))
        .filter(|&at| image.pixels[at..at + 3] == [0xFF, 0x00, 0xFF])
        .count()
}

/// The privacy test (the square is up): capture must not see it; with the exclusion turned off it must, so the
/// check itself is shown to work. Through GDI (a screenshot now) and desktop duplication (frames as they come).
fn privacy_check(duplicator: &mut Duplicator, ui: &ui::Ui) -> String {
    let look = |duplicator: &mut Duplicator| {
        std::thread::sleep(Duration::from_millis(400));
        let snapshot = duplicator.snapshot_image().ok();
        let gdi = snapshot.as_ref().map_or(usize::MAX, privacy_test_pixels);
        let other = snapshot.as_ref().map(privacy_other_pixels);
        let (mut frames, mut most) = (0, 0);
        let until = Instant::now() + Duration::from_millis(800);
        while Instant::now() < until {
            if let Ok(Some(frame)) = duplicator.acquire(50)
                && let Ok(image) = duplicator.copy_to_cpu(&frame)
            {
                frames += 1;
                most = most.max(privacy_test_pixels(&image));
            }
        }
        (gdi, frames, most, other)
    };
    let hidden = look(duplicator);
    let shown = match ui.set_privacy_excluded(false) {
        Ok(()) => look(duplicator),
        Err(error) => return format!("privacy test: FAIL (turning the exclusion off: {error})"),
    };
    let _ = ui.set_privacy_excluded(true);
    let square = (PRIVACY_TEST_SIZE.0 * PRIVACY_TEST_SIZE.1) as usize;
    // All but a few pixels (the square is plain; a pointer or a window above it may cover a little).
    let seen = |pixels: usize| pixels * 100 >= square * 99;
    let passed = hidden.0 == 0 && hidden.2 == 0 && seen(shown.0) && (shown.1 == 0 || seen(shown.2));
    format!(
        "privacy test: {} (test colour pixels of {square}: hidden from capture: GDI {}, duplication {} in {} frames; \
         not hidden: GDI {}, duplication {} in {} frames; other colours there: {})",
        if passed { "PASS" } else { "FAIL" },
        hidden.0,
        hidden.2,
        hidden.1,
        shown.0,
        shown.2,
        shown.1,
        shown.3.flatten().unwrap_or_else(|| "none".into())
    )
}

/// Where in the test square the picture is not the test colour, and what is there instead (the first such
/// pixel): "x a..b, y c..d, first [B, G, R] at (x, y)" in square coordinates.
fn privacy_other_pixels(image: &BgraImage) -> Option<String> {
    let width = image.width as usize;
    let start = PRIVACY_TEST_OFFSET as usize;
    let (mut x_range, mut y_range) = ((usize::MAX, 0), (usize::MAX, 0));
    let mut first: Option<([u8; 3], usize, usize)> = None;
    for y in 0..PRIVACY_TEST_SIZE.1 as usize {
        for x in 0..PRIVACY_TEST_SIZE.0 as usize {
            let at = ((start + y) * width + start + x) * 4;
            let Some(pixel) = image.pixels.get(at..at + 3) else {
                continue;
            };
            if pixel != [0xFF, 0x00, 0xFF] {
                x_range = (x_range.0.min(x), x_range.1.max(x));
                y_range = (y_range.0.min(y), y_range.1.max(y));
                first.get_or_insert(([pixel[0], pixel[1], pixel[2]], x, y));
            }
        }
    }
    first.map(|(colour, x, y)| {
        format!(
            "x {}..{}, y {}..{}, first {colour:?} at ({x}, {y})",
            x_range.0, x_range.1, y_range.0, y_range.1
        )
    })
}

/// Starts capturing this PC's sound for the session (None, with a report line, when it cannot).
fn start_sound(peer: &Peer, options: &Options, lines: &mut Vec<String>) -> Option<Capture> {
    if options.logon {
        lines.push("sound: none at the sign-in screen".into());
        return None;
    }
    match peer.audio_sender() {
        Ok(sender) => Some(Capture::start(
            options.sound_device.clone(),
            Box::new(move |packet, time| sender.send(packet, time)),
        )),
        Err(error) => {
            lines.push(format!("sound: not sent ({error})"));
            None
        }
    }
}

fn sound_line(capture: &Capture) -> String {
    let stats = capture.stats();
    format!(
        "sound: from {}: {} packets ({:.0} kB) sent, {} silent frames left out",
        if stats.device.is_empty() {
            "no device"
        } else {
            &stats.device
        },
        stats.packets,
        stats.bytes as f64 / 1e3,
        stats.silent
    )
}

/// Carries out a command from the viewer; returns what to tell the person controlling.
fn run_command(command: Command, dry_run: bool) -> String {
    match command {
        // SendSAS works only for a service running as SYSTEM (M5).
        Command::CtrlAltDel => {
            "Ctrl+Alt+Del needs the Queqiao service on that PC, which is not running".into()
        }
        Command::Lock if dry_run => "the PC would be locked now (test run)".into(),
        Command::Lock => match power::lock() {
            Ok(()) => "the PC is locked".into(),
            Err(error) => format!("locking failed: {error}"),
        },
        Command::Restart if dry_run => format!(
            "the PC would restart in {} s (test run)",
            power::RESTART_DELAY
        ),
        Command::Restart => match power::restart() {
            Ok(()) => format!("the PC restarts in {} s", power::RESTART_DELAY),
            Err(error) => format!("restarting failed: {error}"),
        },
        // Handled by the session (they change its state).
        Command::SoundOn
        | Command::SoundOff
        | Command::PrivacyOn
        | Command::PrivacyOff
        | Command::MuteSpeakers(_)
        | Command::Resolution(_)
        | Command::Screen(_)
        | Command::MaxBitrate(_)
        | Command::RestartQueqiao
        | Command::Picture { .. } => "changed".into(),
    }
}

/// Everything that belongs to the screen being shown.
struct OpenedScreen {
    duplicator: Duplicator,
    info: OutputInfo,
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    latest: ID3D11Texture2D,
    screen: (u32, u32),
    encoder: ScreenEncoder,
    injector: Injector,
}

/// Opens screen `index`: capture (on the GPU that drives it), a first picture, an encoder for its size (within
/// `limit`) and input mapped to it.
fn open_screen(
    index: usize,
    fps: u32,
    fixed_fps: bool,
    limit: Option<(u32, u32)>,
) -> Result<OpenedScreen, String> {
    let duplicator = Duplicator::new(index).map_err(describe)?;
    let info = duplicator.info().clone();
    let (device, context) = (duplicator.device().clone(), duplicator.context().clone());
    let snapshot = duplicator.snapshot_image().map_err(describe)?;
    let latest = upload(&device, &snapshot)?;
    let screen = (snapshot.width, snapshot.height);
    let encoder = ScreenEncoder::with_limit(&device, &context, screen, fps, fixed_fps, limit)
        .map_err(|e| format!("encoder: {}", describe(e)))?;
    let injector = Injector::new(ScreenRect {
        left: info.left,
        top: info.top,
        width: info.width,
        height: info.height,
    });
    Ok(OpenedScreen {
        duplicator,
        info,
        device,
        context,
        latest,
        screen,
        encoder,
        injector,
    })
}

/// Locks the PC after a session when the person chose so (or a test asked).
fn lock_after(options: &Options, settings: &ui::Settings, lines: &mut Vec<String>) {
    if !(settings.lock_after_session || options.lock_after_session) {
        return;
    }
    if options.dry_run_commands {
        lines.push("after the session: the PC would be locked now (test run)".into());
        return;
    }
    match power::lock() {
        Ok(()) => lines.push("after the session: the PC is locked".into()),
        Err(error) => lines.push(format!("after the session: locking failed: {error}")),
    }
}

/// What a session may do: the person's settings, and less for someone helping from another account.
struct Permissions {
    clipboard: bool,
    files: bool,
    terminal: bool,
    /// Another account's device, joined by a help code: no privacy screen, no restart.
    helper: bool,
}

/// A help code: 8 of 32 letters and digits without look-alikes (no 0, O, 1 or I), 40 bits.
/// How long help stays closed after five wrong codes before a fixed code takes requests again.
const FIXED_CODE_PAUSE: Duration = Duration::from_secs(15 * 60);

fn new_help_code() -> String {
    const ALPHABET: &[u8; 32] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    let random = queqiao_account::new_session_id();
    random
        .as_bytes()
        .chunks(2)
        .filter_map(|pair| u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok())
        .map(|byte| char::from(ALPHABET[usize::from(byte % 32)]))
        .collect()
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

/// One report line per finished or failed file.
fn report_files(outcomes: Vec<Outcome>, lines: &mut Vec<String>) {
    for outcome in outcomes {
        lines.push(match outcome {
            Outcome::Sent { name, size } => {
                format!("file sent: {name} ({size} bytes, checked by the viewer)")
            }
            Outcome::Received { path, size, .. } => {
                format!(
                    "file received: {} ({size} bytes, SHA-256 checked)",
                    path.display()
                )
            }
            Outcome::Failed { name, reason } => format!("file failed: {name}: {reason}"),
        });
    }
}

/// How a session went.
/// The start of the clipboard text the tests send (each side checks it got the other's).
pub(crate) const CLIPBOARD_TEST_TEXT: &str = "Queqiao clipboard test";

/// Clipboard texts in a session.
#[derive(Default)]
struct ClipboardStats {
    received: u32,
    sent: u32,
    test_received: bool,
    test_sent: bool,
    /// The last text from the other side (not sent back, not set again).
    last_in: Option<String>,
}

impl ClipboardStats {
    fn received(&mut self, text: &str) {
        self.received += 1;
        self.test_received |= text.starts_with(CLIPBOARD_TEST_TEXT);
    }

    fn line(&self) -> String {
        format!(
            "clipboard: {} received, {} sent; test text from the other side received: {}",
            self.received,
            self.sent,
            if self.test_received { "yes" } else { "no" }
        )
    }
}

struct SessionOutcome {
    passed: bool,
    ended: String,
}

/// Takes the banner down however the session ends.
struct BannerGuard<'a>(&'a ui::Ui);

impl Drop for BannerGuard<'_> {
    fn drop(&mut self) {
        self.0.session_ended();
    }
}

/// One session with one viewer: capture, encode and send the screen, inject the viewer's input. The banner
/// shows it on this PC the whole time; tray and banner actions that are not about the session are put in
/// `deferred`.
#[allow(clippy::too_many_arguments)]
fn session(
    options: &Options,
    signal: &mut dyn Signaling,
    viewer: &str,
    viewer_id: &str,
    end: Option<Instant>,
    ui: &ui::Ui,
    permissions: Permissions,
    links: &mut Links,
    deferred: &mut Vec<PresenceEvent>,
    lines: &mut Vec<String>,
) -> Result<SessionOutcome, String> {
    let _ = viewer_id;
    let Permissions {
        clipboard: share_clipboard,
        files: receive_files,
        terminal: allow_terminal,
        helper,
    } = permissions;
    ui.session_started(viewer)
        .map_err(|e| format!("the banner could not be shown, so the session is refused: {e}"))?;
    let _banner = BannerGuard(ui);
    let frequency = qpc_frequency() as f64;
    let ms = |ticks: i64| ticks as f64 * 1000.0 / frequency;

    // This PC's own capture; none while another desktop takes input (the sign-in, lock, Ctrl+Alt+Del or UAC
    // screen), whose pictures come from the service's desktop helper.
    let (mut secure_switches, mut secure_frames, mut secure_inputs) = (0u32, 0u32, 0u32);
    if let Some(link) = links.desktop.as_mut() {
        for name in link.poll_desktop() {
            lines.push(format!("input desktop: {name}"));
        }
        link.session(true);
        link.banner(Some(viewer));
    }
    let (mut duplicator, mut info, mut device, mut context, snapshot) = match Duplicator::primary()
    {
        Ok(own) => {
            let snapshot = own.snapshot_image().map_err(describe)?;
            let (info, device, context) = (
                own.info().clone(),
                own.device().clone(),
                own.context().clone(),
            );
            (Some(own), info, device, context, snapshot)
        }
        Err(error) => {
            let Some(link) = links.desktop.as_mut() else {
                return Err(describe(error));
            };
            // No screens to see from here (the agent at the sign-in screen runs in session 0): a device on the first
            // GPU, and the screen's place as the helper says.
            let opened = queqiao_capture::screen_device(None)
                .or_else(|_| queqiao_capture::first_device())
                .map_err(describe)?;
            link.capture(opened.index);
            let snapshot = first_helper_picture(link)?;
            let mut info = opened.info;
            match link.screen {
                Some(screen) => {
                    (info.left, info.top) = (screen.left, screen.top);
                    (info.width, info.height) = (screen.width, screen.height);
                }
                None => (info.width, info.height) = (snapshot.width, snapshot.height),
            }
            secure_switches += 1;
            lines.push(format!(
                "this PC's own desktop does not take input now ({}): pictures and input through the desktop helper",
                describe(error)
            ));
            (None, info, opened.device, opened.context, snapshot)
        }
    };
    let mut latest = upload(&device, &snapshot)?;
    let fps = options
        .fps
        .unwrap_or((info.refresh_hz.round() as u32).clamp(1, 60));
    let mut screen = (snapshot.width, snapshot.height);
    let mut encoder = ScreenEncoder::new(&device, &context, screen, fps, options.fps.is_some())
        .map_err(|e| format!("encoder: {}", describe(e)))?;
    let config = encoder.config().clone();
    lines.push(format!(
        "screen {}x{} at {:.0} Hz on {}",
        info.width, info.height, info.refresh_hz, info.adapter
    ));
    lines.push(format!(
        "encoder: {} ({}) at {}x{}, {} fps, {:.1} Mbit/s; convert: {}",
        encoder.encoder().name(),
        if encoder.encoder().is_hardware() {
            "hardware"
        } else {
            "software"
        },
        config.width,
        config.height,
        config.fps,
        f64::from(config.bitrate) / 1e6,
        encoder.converter_description()
    ));
    let mut injector = Injector::new(ScreenRect {
        left: info.left,
        top: info.top,
        width: info.width,
        height: info.height,
    });
    // The screen shown (counted as queqiao_capture::screens does) and the picture size the viewer chose.
    let mut screen_index = queqiao_capture::screens()
        .ok()
        .and_then(|list| list.iter().position(|s| s.primary))
        .unwrap_or(0);
    let mut resolution_limit: Option<(u32, u32)> = None;

    lines.push(format!("viewer: {viewer}"));
    let started = Instant::now();
    // Someone helping from another account learns only the address the server connection uses.
    let addresses = if helper {
        Addresses::RouteOnly
    } else {
        Addresses::All
    };
    let peer = Peer::start_with(Role::Agent, signal, addresses).map_err(|e| e.to_string())?;
    lines.extend(peer.candidates().iter().cloned());
    // This PC's sound, captured and sent from its own thread (the viewer can turn it off).
    let mut sound = start_sound(&peer, options, lines);
    let _tone = if options.sound_test {
        Tone::start(options.sound_device.clone(), TEST_TONE_HZ)
            .map_err(|error| lines.push(format!("sound test: no tone ({error})")))
            .ok()
    } else {
        None
    };
    let mut sound_report = Vec::new();
    let mut privacy_check_due = false;
    // The local keyboard and mouse off while the privacy screen is up.
    let mut local_block: Option<queqiao_input::LocalInputBlock> = None;
    // Muted speakers: whether they were muted before, to put back when the session ends.
    let mut speakers_before: Option<bool> = None;
    // Adaptive bitrate: start at the transport's starting estimate, then follow the estimate (85 % of it),
    // never above the size-based bitrate the encoder was opened with.
    // The viewer's picture settings change the highest bitrate (quality) and the frame interval (frame rate).
    let mut size_bitrate = config.bitrate;
    // The quality the viewer chose (a share of the size-based bitrate), kept when the size changes.
    let mut quality_factor = 1.0f64;
    // A bitrate ceiling the viewer typed in (bit/s), instead of the preset's share of the size-based one.
    let mut custom_bitrate: Option<u32> = None;
    let mut max_bitrate = size_bitrate;
    let mut last_estimate = START_BITRATE;
    peer.set_bitrate_range(u64::from(MIN_BITRATE), u64::from(max_bitrate));
    let start_bitrate = adapted_bitrate(START_BITRATE, max_bitrate);
    if start_bitrate != encoder.bitrate()
        && let Err(error) = encoder.set_bitrate(start_bitrate)
    {
        lines.push(format!(
            "the encoder keeps {:.1} Mbit/s: {}",
            f64::from(encoder.bitrate()) / 1e6,
            describe(error)
        ));
    }
    let mut bitrate_changes = 0u32;
    let mut lowest_bitrate = encoder.bitrate();
    let mut last_bitrate_change: Option<Instant> = None;

    let pointer_start = cursor_position();
    let mut interval = Duration::from_secs_f64(1.0 / f64::from(config.fps));
    let screen_fps = (info.refresh_hz.round() as u32).clamp(1, 240);
    let mut stats = Stats::default();
    let mut connected_after: Option<Duration> = None;
    // The GDI snapshot is the first picture to send; its screen time is unknown (0).
    let mut waiting: Option<(i64, i64)> = Some((0, qpc_now()));
    let mut force_keyframe = false;
    let mut last_encode: Option<Instant> = None;
    let mut closed = None;

    // The clipboard both ways, while the person at this PC allows it.
    let clipboard = if share_clipboard {
        Clipboard::start()
            .map_err(|error| lines.push(format!("clipboard: not shared ({error})")))
            .ok()
    } else {
        lines.push("clipboard: not shared (turned off on this PC)".into());
        None
    };
    let mut clipboard_stats = ClipboardStats::default();
    let mut channel_open_at: Option<Instant> = None;
    // Files both ways: from the viewer into Downloads\Queqiao (while the person at this PC allows it).
    let mut transfers = Transfers::new(downloads_folder(), receive_files);
    // Clipboard images both ways, on their own stream.
    let (mut images, mut image_in) = (ImageSender::default(), ImageReceiver::default());
    let (mut images_received, mut images_sent, mut image_test_received) = (0u32, 0u32, false);
    // The image last received: not put on the clipboard again, nor sent back (as for text).
    let mut last_image_in: Option<u64> = None;
    // Files from the other side's clipboard copies, by copy; they go on this clipboard as they arrive.
    let mut clip_files: std::collections::HashMap<u32, Vec<PathBuf>> =
        std::collections::HashMap::new();
    let mut clip_files_set = 0u32;
    // Remote command lines (while the person at this PC allows them).
    let mut terminals = Terminals::new(allow_terminal);
    for path in &options.send_files {
        let _ = transfers.send(path);
    }
    let mut end_pressed = false;
    let mut ended_here = false;
    let mut last_own_try = Instant::now();
    let mut size_noted = false;
    // A newly opened screen of this PC (after a desktop or display change) replaces the old one.
    macro_rules! adopt {
        ($opened:expr) => {{
            let opened = $opened;
            duplicator = Some(opened.duplicator);
            info = opened.info;
            (device, context) = (opened.device, opened.context);
            latest = opened.latest;
            screen = opened.screen;
            encoder = opened.encoder;
            injector = opened.injector;
            size_bitrate = encoder.config().bitrate;
            max_bitrate = custom_bitrate
                .unwrap_or(((f64::from(size_bitrate) * quality_factor) as u32).max(MIN_BITRATE));
            peer.set_bitrate_range(u64::from(MIN_BITRATE), u64::from(max_bitrate));
            let _ = encoder.set_bitrate(adapted_bitrate(last_estimate, max_bitrate));
            force_keyframe = true;
            waiting = Some((0, qpc_now()));
        }};
    }
    'session: loop {
        if end.is_some_and(|end| Instant::now() > end) {
            break;
        }
        if !end_pressed
            && options
                .end_after
                .is_some_and(|seconds| started.elapsed() >= Duration::from_secs(seconds))
        {
            ui.press_end();
            end_pressed = true;
        }
        while let Some(event) = ui.try_event() {
            match event {
                PresenceEvent::EndSession => {
                    closed = Some("ended by the person at this PC".to_string());
                    ended_here = true;
                    break 'session;
                }
                PresenceEvent::Quit => {
                    closed = Some("ended by the person at this PC (quit)".to_string());
                    ended_here = true;
                    deferred.push(PresenceEvent::Quit);
                    break 'session;
                }
                PresenceEvent::SettingsLocked => lines.push(
                    "a setting was chosen while someone of another account controls this PC: refused".into(),
                ),
                other => deferred.push(other),
            }
        }
        // End pressed on the helper's banner (on the sign-in, lock or UAC screen).
        if links
            .desktop
            .as_mut()
            .is_some_and(|link| std::mem::take(&mut link.end_pressed))
        {
            closed = Some("ended by the person at this PC".to_string());
            ended_here = true;
            break 'session;
        }
        if connected_after.is_none() && started.elapsed() > CONNECT_LIMIT {
            closed = Some(format!(
                "could not connect within {} s",
                CONNECT_LIMIT.as_secs()
            ));
            break;
        }
        while let Some(event) = peer.try_event() {
            match event {
                PeerEvent::Connected => connected_after = Some(started.elapsed()),
                PeerEvent::KeyframeRequested => {
                    stats.keyframe_requests += 1;
                    force_keyframe = true;
                    waiting.get_or_insert((0, qpc_now()));
                }
                PeerEvent::Data(bytes) => match Message::decode(&bytes) {
                    Some(Message::Input(event)) => {
                        stats.inputs += 1;
                        // On another desktop (sign-in, lock, Ctrl+Alt+Del, UAC) the desktop helper injects.
                        if duplicator.is_none()
                            && let Some(link) = links.desktop.as_mut()
                        {
                            link.input(event, injector.screen());
                            secure_inputs += 1;
                            continue;
                        }
                        let before = cursor_position();
                        match injector.inject(&event) {
                            Ok(())
                                if options.verify_input
                                    && matches!(event, InputEvent::MouseDelta { .. }) =>
                            {
                                verify_delta(&event, before, &mut stats)
                            }
                            Ok(()) if options.verify_input => verify(&injector, &event, &mut stats),
                            Ok(()) => {}
                            Err(error) => {
                                stats.inject_failures += 1;
                                stats.last_inject_error = error;
                            }
                        }
                    }
                    Some(Message::Ping { id, sent_us }) => {
                        peer.send_data(Message::Pong { id, sent_us }.encode());
                    }
                    Some(Message::Leave) => {
                        closed = Some("the viewer left".to_string());
                        break 'session;
                    }
                    Some(Message::Command(
                        Command::PrivacyOn | Command::Restart | Command::RestartQueqiao,
                    )) if helper => {
                        let notice =
                            "someone helping from another account cannot do that on this PC"
                                .to_string();
                        lines.push(format!("command from the helper refused: {notice}"));
                        peer.send_data(Message::Notice(notice).encode());
                    }
                    Some(Message::Command(Command::PrivacyOn)) => {
                        let look = if options.privacy_test {
                            ui::PrivacyLook {
                                area: Some(privacy_test_area(info.left, info.top)),
                                color: PRIVACY_TEST_COLOR,
                            }
                        } else {
                            ui::PrivacyLook {
                                area: None,
                                color: 0x0010_1010,
                            }
                        };
                        let notice = match ui.set_privacy(Some(look)) {
                            Ok(banner_above) => {
                                privacy_check_due = options.privacy_test;
                                lines.push(format!(
                                    "privacy: the session banner is above the dark screen: {}",
                                    if banner_above { "yes" } else { "no" }
                                ));
                                // Nobody types or clicks unseen there meanwhile: its own keyboard and mouse are
                                // off (Ctrl+Alt+Del gives them back).
                                match queqiao_input::LocalInputBlock::start() {
                                    Ok(block) => {
                                        local_block = Some(block);
                                        lines.push(
                                            "privacy: the local keyboard and mouse are off".into(),
                                        );
                                        "that PC's screen is dark and its keyboard and mouse are off: only you see and use it".to_string()
                                    }
                                    Err(error) => {
                                        lines.push(format!("privacy: {error}"));
                                        "that PC's screen is dark: only you see it (its keyboard and mouse still work)".to_string()
                                    }
                                }
                            }
                            Err(error) => format!("that PC's screen could not be hidden: {error}"),
                        };
                        lines.push(format!("command from the viewer: PrivacyOn: {notice}"));
                        peer.send_data(Message::Notice(notice).encode());
                    }
                    Some(Message::Command(Command::PrivacyOff)) => {
                        let _ = ui.set_privacy(None);
                        end_local_block(&mut local_block, lines);
                        let notice = "that PC's screen is shown again".to_string();
                        lines.push(format!("command from the viewer: PrivacyOff: {notice}"));
                        peer.send_data(Message::Notice(notice).encode());
                    }
                    Some(Message::Command(Command::Picture { fps, quality })) => {
                        // At most what the screen shows; 0 asks for exactly that.
                        let frames = if fps == 0 {
                            screen_fps
                        } else {
                            u32::from(fps).min(screen_fps)
                        };
                        interval = Duration::from_secs_f64(1.0 / f64::from(frames));
                        quality_factor = quality.bitrate_factor();
                        max_bitrate = custom_bitrate.unwrap_or(
                            ((f64::from(size_bitrate) * quality_factor) as u32).max(MIN_BITRATE),
                        );
                        peer.set_bitrate_range(u64::from(MIN_BITRATE), u64::from(max_bitrate));
                        let target = adapted_bitrate(last_estimate, max_bitrate);
                        let changed = encoder.set_bitrate(target).is_ok();
                        let notice = format!(
                            "picture: up to {frames} fps, quality {quality:?}: up to {:.1} Mbit/s{}",
                            f64::from(max_bitrate) / 1e6,
                            if changed {
                                ""
                            } else {
                                " (the encoder keeps its bitrate)"
                            }
                        );
                        lines.push(format!("command from the viewer: {notice}"));
                        peer.send_data(Message::Notice(notice).encode());
                    }
                    Some(Message::Command(Command::MuteSpeakers(on))) => {
                        let device = options.sound_device.as_deref();
                        let notice = if on {
                            match queqiao_audio::set_mute(device, true) {
                                Ok(before) => {
                                    speakers_before.get_or_insert(before);
                                    "that PC's speakers are muted (you still hear its sound)"
                                        .to_string()
                                }
                                Err(error) => {
                                    format!("that PC's speakers could not be muted: {error}")
                                }
                            }
                        } else {
                            let before = speakers_before.take().unwrap_or(false);
                            let _ = queqiao_audio::set_mute(device, before);
                            "that PC's speakers play again".to_string()
                        };
                        lines.push(format!("command from the viewer: {notice}"));
                        peer.send_data(Message::Notice(notice).encode());
                    }
                    Some(Message::Command(Command::Resolution(short))) => {
                        // A new encoder for the new size (the screen is still captured whole and scaled).
                        let limit =
                            (short > 0).then(|| (u32::from(short) * 16 / 9, u32::from(short)));
                        resolution_limit = limit;
                        let notice = match ScreenEncoder::with_limit(
                            &device,
                            &context,
                            screen,
                            fps,
                            options.fps.is_some(),
                            limit,
                        ) {
                            Ok(new) => {
                                encoder = new;
                                force_keyframe = true;
                                size_bitrate = encoder.config().bitrate;
                                max_bitrate = custom_bitrate.unwrap_or(
                                    ((f64::from(size_bitrate) * quality_factor) as u32)
                                        .max(MIN_BITRATE),
                                );
                                peer.set_bitrate_range(
                                    u64::from(MIN_BITRATE),
                                    u64::from(max_bitrate),
                                );
                                let _ = encoder
                                    .set_bitrate(adapted_bitrate(last_estimate, max_bitrate));
                                format!(
                                    "picture size {}x{}",
                                    encoder.config().width,
                                    encoder.config().height
                                )
                            }
                            Err(error) => format!("the picture size stays: {}", describe(error)),
                        };
                        lines.push(format!("command from the viewer: {notice}"));
                        peer.send_data(Message::Notice(notice).encode());
                    }
                    Some(Message::Command(Command::Screen(_))) if duplicator.is_none() => {
                        let notice =
                            "the screen can be changed once this PC's own desktop takes input again"
                                .to_string();
                        lines.push(format!("command from the viewer: {notice}"));
                        peer.send_data(Message::Notice(notice).encode());
                    }
                    Some(Message::Command(Command::Screen(index))) => {
                        let fixed = options.fps.is_some();
                        // DXGI lets a process duplicate a screen once: the current duplication goes first. If the
                        // new screen cannot be opened, the one before is opened again (or the session ends).
                        drop(duplicator.take());
                        let (opened, shown) = match open_screen(
                            usize::from(index),
                            fps,
                            fixed,
                            resolution_limit,
                        ) {
                            Ok(opened) => (opened, true),
                            Err(error) => {
                                lines.push(format!(
                                    "screen {} cannot be shown: {error}",
                                    usize::from(index) + 1
                                ));
                                let back = open_screen(screen_index, fps, fixed, resolution_limit)
                                    .map_err(|again| format!("the screen before cannot be shown again either: {again}"))?;
                                (back, false)
                            }
                        };
                        duplicator = Some(opened.duplicator);
                        info = opened.info;
                        (device, context) = (opened.device, opened.context);
                        latest = opened.latest;
                        screen = opened.screen;
                        encoder = opened.encoder;
                        injector = opened.injector;
                        if shown {
                            screen_index = usize::from(index);
                        }
                        size_bitrate = encoder.config().bitrate;
                        max_bitrate = custom_bitrate.unwrap_or(
                            ((f64::from(size_bitrate) * quality_factor) as u32).max(MIN_BITRATE),
                        );
                        peer.set_bitrate_range(u64::from(MIN_BITRATE), u64::from(max_bitrate));
                        let _ = encoder.set_bitrate(adapted_bitrate(last_estimate, max_bitrate));
                        // The screen's first picture goes at once.
                        force_keyframe = true;
                        waiting = Some((0, qpc_now()));
                        let notice = if shown {
                            format!(
                                "screen {} ({}x{}), picture {}x{}",
                                usize::from(index) + 1,
                                info.width,
                                info.height,
                                encoder.config().width,
                                encoder.config().height
                            )
                        } else {
                            format!(
                                "screen {} cannot be shown; screen {} again",
                                usize::from(index) + 1,
                                screen_index + 1
                            )
                        };
                        lines.push(format!("command from the viewer: {notice}"));
                        peer.send_data(Message::Notice(notice).encode());
                    }
                    Some(Message::Command(Command::MaxBitrate(kbit))) => {
                        // 0.5 to 200 Mbit/s; 0 goes back to the preset.
                        custom_bitrate = (kbit > 0).then(|| kbit.clamp(500, 200_000) * 1000);
                        max_bitrate = custom_bitrate.unwrap_or(
                            ((f64::from(size_bitrate) * quality_factor) as u32).max(MIN_BITRATE),
                        );
                        peer.set_bitrate_range(u64::from(MIN_BITRATE), u64::from(max_bitrate));
                        let changed = encoder
                            .set_bitrate(adapted_bitrate(last_estimate, max_bitrate))
                            .is_ok();
                        let notice = format!(
                            "bitrate: up to {:.1} Mbit/s{}",
                            f64::from(max_bitrate) / 1e6,
                            if changed {
                                ""
                            } else {
                                " (the encoder keeps its bitrate)"
                            }
                        );
                        lines.push(format!("command from the viewer: {notice}"));
                        peer.send_data(Message::Notice(notice).encode());
                    }
                    Some(Message::Command(Command::SoundOff)) => {
                        if let Some(capture) = sound.take() {
                            sound_report.push(sound_line(&capture));
                        }
                        lines.push("the viewer turned this PC's sound off".into());
                    }
                    Some(Message::Command(Command::SoundOn)) => {
                        if sound.is_none() {
                            sound = start_sound(&peer, options, lines);
                        }
                        lines.push("the viewer turned this PC's sound on".into());
                    }
                    Some(Message::Command(Command::CtrlAltDel)) if links.service.is_some() => {
                        let notice = links
                            .service
                            .as_mut()
                            .map(|service| service.send_sas())
                            .unwrap_or_default();
                        lines.push(format!("command from the viewer: CtrlAltDel: {notice}"));
                        peer.send_data(Message::Notice(notice).encode());
                    }
                    Some(Message::Command(Command::RestartQueqiao)) => {
                        let notice = match &links.service {
                            Some(_) if options.dry_run_commands => {
                                "Queqiao would restart on that PC now (test run)"
                            }
                            Some(_) => "Queqiao restarts on that PC: connect again in a few seconds",
                            None => "restarting Queqiao needs the Queqiao service on that PC, which is not running",
                        }
                        .to_string();
                        lines.push(format!("command from the viewer: RestartQueqiao: {notice}"));
                        peer.send_data(Message::Notice(notice).encode());
                        if !options.dry_run_commands
                            && let Some(service) = links.service.as_mut()
                        {
                            // The notice first; then the service ends this agent.
                            std::thread::sleep(Duration::from_millis(300));
                            if let Err(error) = service.restart() {
                                lines.push(format!("restarting Queqiao failed: {error}"));
                            }
                        }
                    }
                    Some(Message::Command(command)) => {
                        let notice = run_command(command, options.dry_run_commands);
                        lines.push(format!("command from the viewer: {command:?}: {notice}"));
                        peer.send_data(Message::Notice(notice).encode());
                    }
                    Some(Message::ClipboardText(text)) => {
                        clipboard_stats.received(&text);
                        if let Some(clipboard) = &clipboard
                            && clipboard_stats.last_in.as_ref() != Some(&text)
                        {
                            clipboard.set_text(text.clone());
                            clipboard_stats.last_in = Some(text);
                        }
                    }
                    _ => {}
                },
                PeerEvent::Closed(reason) => {
                    closed = Some(reason);
                    break 'session;
                }
                PeerEvent::Path(path) => lines.push(format!(
                    "path after {} ms: {path}",
                    started.elapsed().as_millis()
                )),
                PeerEvent::Note(note) => {
                    lines.push(format!("{} ms: {note}", started.elapsed().as_millis()))
                }
                PeerEvent::Bitrate(estimate) => {
                    last_estimate = estimate;
                    let target = adapted_bitrate(estimate, max_bitrate);
                    let current = encoder.bitrate();
                    let big_enough = target.abs_diff(current) > current / 20;
                    let spaced =
                        last_bitrate_change.is_none_or(|at| at.elapsed() >= BITRATE_CHANGE_GAP);
                    if big_enough && spaced && encoder.set_bitrate(target).is_ok() {
                        bitrate_changes += 1;
                        lowest_bitrate = lowest_bitrate.min(target);
                        last_bitrate_change = Some(Instant::now());
                    }
                }
                PeerEvent::ChannelOpen => {
                    channel_open_at = Some(Instant::now());
                    // The viewer can offer the other screens in its menu.
                    if let Ok(list) = queqiao_capture::screens() {
                        let sizes = list
                            .iter()
                            .map(|s| (s.width.min(65535) as u16, s.height.min(65535) as u16))
                            .collect();
                        let current = screen_index.min(255) as u8;
                        peer.send_data(Message::Screens { current, sizes }.encode());
                    }
                }
                PeerEvent::StreamOpen(Stream::Files) => report_files(transfers.opened(), lines),
                PeerEvent::StreamOpen(Stream::Clipboard) => {}
                PeerEvent::StreamData(Stream::Clipboard, bytes) => {
                    if let Some(result) = image_in.handle(&bytes) {
                        match result {
                            Ok(image) => {
                                images_received += 1;
                                image_test_received |= image == test_picture(1);
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
                PeerEvent::StreamOpen(Stream::Terminal) => {}
                PeerEvent::StreamData(Stream::Terminal, bytes) => {
                    if let Some(message) = TerminalMessage::decode(&bytes) {
                        for reply in terminals.handle(message) {
                            match &reply {
                                TerminalMessage::Opened { id, shell } => {
                                    lines.push(format!("command line {id} opened ({shell})"))
                                }
                                TerminalMessage::Failed { id, reason } => {
                                    lines.push(format!("command line {id} failed: {reason}"))
                                }
                                _ => {}
                            }
                            peer.send_stream(Stream::Terminal, reply.encode());
                        }
                    }
                }
                PeerEvent::StreamData(Stream::Files, bytes) => {
                    for reply in transfers.on_message(&bytes) {
                        peer.send_stream(Stream::Files, reply);
                    }
                }
                PeerEvent::Video { .. } | PeerEvent::Audio { .. } => {}
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
        report_files(outcomes, lines);
        for message in images.take(peer.room(Stream::Clipboard)) {
            peer.send_stream(Stream::Clipboard, message);
        }
        for message in terminals.poll(peer.room(Stream::Terminal)) {
            if let TerminalMessage::Exited { id, code } = &message {
                lines.push(format!("command line {id} ended with exit code {code}"));
            }
            peer.send_stream(Stream::Terminal, message.encode());
        }

        if privacy_check_due {
            privacy_check_due = false;
            if let Some(own) = duplicator.as_mut() {
                lines.push(privacy_check(own, ui));
            }
        }
        if let Some(capture) = &sound {
            for note in capture.notes() {
                lines.push(format!("{} ms: {note}", started.elapsed().as_millis()));
            }
        }
        if let Some(opened) = channel_open_at {
            match clipboard.as_ref().and_then(Clipboard::changed) {
                Some(ClipContent::Text(text))
                    if clipboard_stats.last_in.as_ref() != Some(&text)
                        && text.len() <= MAX_CLIPBOARD_TEXT =>
                {
                    peer.send_data(Message::ClipboardText(text).encode());
                    clipboard_stats.sent += 1;
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
            if options.clipboard_test
                && !clipboard_stats.test_sent
                && opened.elapsed() >= Duration::from_secs(1)
            {
                let text = format!("{CLIPBOARD_TEST_TEXT} (from the agent)");
                peer.send_data(Message::ClipboardText(text).encode());
                clipboard_stats.sent += 1;
                if images.send(&test_picture(2)).is_ok() {
                    images_sent += 1;
                }
                clipboard_stats.test_sent = true;
            }
        }

        // Capture: wait for a screen change, at most until the next frame is due and at most 5 ms, so
        // input from the viewer is handled quickly.
        let now = Instant::now();
        let due = last_encode.map_or(now, |last| last + interval);
        let wait = if waiting.is_some() {
            due.saturating_duration_since(now).as_millis().min(5) as u32
        } else {
            5
        };
        let fixed = options.fps.is_some();
        let acquired = duplicator.as_mut().map(|own| own.acquire(wait));
        match acquired {
            Some(Ok(Some(frame))) if frame.present_qpc != 0 => {
                stats.captured += 1;
                unsafe { context.CopyResource(&latest, &frame.texture) };
                waiting = Some((frame.present_qpc, frame.acquired_qpc));
            }
            Some(Ok(_)) => {}
            Some(Err(error)) if needs_restart(&error) => {
                // The desktop switched (sign-in, lock, Ctrl+Alt+Del or UAC screen) or the display changed.
                duplicator = None;
                match open_screen(screen_index, fps, fixed, resolution_limit) {
                    Ok(opened) => {
                        adopt!(opened);
                        lines.push(format!(
                            "{} ms: the display changed: captured again ({}x{})",
                            started.elapsed().as_millis(),
                            info.width,
                            info.height
                        ));
                    }
                    Err(error) => match links.desktop.as_mut() {
                        Some(link) if !link.closed => {
                            link.capture(screen_index);
                            secure_switches += 1;
                            last_own_try = Instant::now();
                            lines.push(format!(
                                "{} ms: another desktop takes input ({error}): pictures and input through the desktop helper",
                                started.elapsed().as_millis()
                            ));
                            // Windows' own screen came up (Ctrl+Alt+Del, the lock screen, a UAC prompt): the dark
                            // screen and the local block end, so the person there is never shut out.
                            if local_block.is_some() {
                                let _ = ui.set_privacy(None);
                                end_local_block(&mut local_block, lines);
                                peer.send_data(
                                    Message::Notice(
                                        "Windows' own screen came up on that PC (Ctrl+Alt+Del, lock or UAC): its screen is shown again and its keyboard and mouse work"
                                            .into(),
                                    )
                                    .encode(),
                                );
                            }
                        }
                        _ => {
                            return Err(format!(
                                "the desktop switched (sign-in, lock or UAC screen, or a display change) and cannot be captured: {error}"
                            ));
                        }
                    },
                }
            }
            Some(Err(error)) => return Err(describe(error)),
            None => {
                // Pictures from the desktop helper until this PC's own desktop takes input again.
                let Some(link) = links.desktop.as_mut() else {
                    return Err("nothing captures this PC's screen".into());
                };
                let gap = last_own_try.elapsed();
                let back = ((link.on_own_desktop() || link.closed)
                    && gap >= Duration::from_millis(250))
                    || gap >= Duration::from_secs(3);
                if back {
                    last_own_try = Instant::now();
                    if let Ok(opened) = open_screen(screen_index, fps, fixed, resolution_limit) {
                        link.stop_capture();
                        adopt!(opened);
                        lines.push(format!(
                            "{} ms: this PC's own desktop takes input again: captured here",
                            started.elapsed().as_millis()
                        ));
                    }
                }
                if let Some(link) = links.desktop.as_mut().filter(|_| duplicator.is_none()) {
                    if link.closed {
                        return Err(
                            "the desktop helper is gone and this PC's own desktop cannot be captured"
                                .into(),
                        );
                    }
                    match link.next(Duration::from_millis(u64::from(wait))) {
                        Some(FromHelper::Frame {
                            width,
                            height,
                            pixels,
                        }) => {
                            if (width, height) == screen {
                                latest = upload(
                                    &device,
                                    &BgraImage {
                                        width,
                                        height,
                                        pixels,
                                    },
                                )?;
                                stats.captured += 1;
                                secure_frames += 1;
                                waiting = Some((0, qpc_now()));
                            } else if !size_noted {
                                size_noted = true;
                                lines.push(format!(
                                    "a {width}x{height} picture from the desktop helper does not fit the {}x{} screen: left out",
                                    screen.0, screen.1
                                ));
                            }
                        }
                        Some(FromHelper::Failed(error)) => lines.push(format!(
                            "{} ms: desktop helper: {error}",
                            started.elapsed().as_millis()
                        )),
                        Some(FromHelper::Desktop(name)) => lines.push(format!(
                            "{} ms: input desktop: {name}",
                            started.elapsed().as_millis()
                        )),
                        Some(FromHelper::EndPressed | FromHelper::Screen { .. }) | None => {}
                    }
                }
            }
        }
        // While capturing here: the helper's word on desktop changes (and no pictures).
        if duplicator.is_some()
            && let Some(link) = links.desktop.as_mut()
        {
            for name in link.poll_desktop() {
                lines.push(format!(
                    "{} ms: input desktop: {name}",
                    started.elapsed().as_millis()
                ));
            }
        }

        // Encode as soon as there is a change and the frame time has passed.
        if connected_after.is_some()
            && Instant::now() >= due
            && let Some((screen_time, captured_time)) = waiting.take()
        {
            let frames = encoder
                .encode(&latest, force_keyframe)
                .map_err(|e| format!("encode: {}", describe(e)))?;
            force_keyframe = false;
            last_encode = Some(Instant::now());
            for frame in frames {
                let time_90khz = (frame.time.max(0) as u64) * 9 / 1000;
                stats.sent += 1;
                stats.bytes += frame.data.len();
                stats.keyframes += u32::from(frame.keyframe);
                peer.send_video(frame.data, time_90khz);
                let origin = if screen_time != 0 {
                    screen_time
                } else {
                    captured_time
                };
                let agent_ms = ms(qpc_now() - origin);
                // Pictures without a screen time (the start-up screenshot, keyframes sent again on
                // request) say nothing about the delay of a screen change.
                if screen_time != 0 {
                    stats.agent_ms.push(agent_ms);
                }
                let info = Message::FrameInfo {
                    time_90khz,
                    agent_ms: agent_ms as f32,
                };
                peer.send_data(info.encode());
            }
        }
    }
    if let Some(link) = links.desktop.as_mut() {
        link.banner(None);
        link.session(false);
        lines.push(format!(
            "desktop helper: {secure_switches} switches to another desktop, {secure_frames} pictures from it, {secure_inputs} inputs passed to it"
        ));
    }
    if closed.is_none() || ended_here {
        // Our end (time is up, or the person here ended it): tell the viewer at once.
        peer.send_data(Message::Leave.encode());
        std::thread::sleep(Duration::from_millis(100));
    }
    drop(peer);
    if options.verify_input {
        set_cursor_position(pointer_start.0, pointer_start.1);
    }

    let elapsed = started.elapsed().as_secs_f64();
    lines.push(format!(
        "connected after {}",
        connected_after.map_or("never".into(), |d| format!("{} ms", d.as_millis()))
    ));
    lines.push(format!(
        "{elapsed:.1} s: {} screen changes, {} frames sent ({} keyframes, {:.2} MB, {:.1} Mbit/s), {} keyframe requests",
        stats.captured,
        stats.sent,
        stats.keyframes,
        stats.bytes as f64 / 1e6,
        stats.bytes as f64 * 8.0 / elapsed / 1e6,
        stats.keyframe_requests
    ));
    lines.push(format!(
        "screen change -> sent, ms: {}",
        summary(&mut stats.agent_ms)
    ));
    lines.push(clipboard_stats.line());
    if clip_files_set > 0 {
        lines.push(format!(
            "clipboard files: {clip_files_set} put on the clipboard from the other side"
        ));
        // What the clipboard holds now (the watcher puts them there on its own thread).
        std::thread::sleep(Duration::from_millis(300));
        let held = queqiao_input::clipboard_files(None).unwrap_or_default();
        let names: Vec<String> = held.iter().map(|path| path.display().to_string()).collect();
        lines.push(format!("clipboard now holds: {}", names.join(", ")));
    }
    lines.push(format!(
        "clipboard images: {images_received} received, {images_sent} sent; test image from the other side received: {}",
        if image_test_received { "yes" } else { "no" }
    ));
    // The speakers go back to how the person left them, however the session ended.
    if let Some(before) = speakers_before.take() {
        let restored = queqiao_audio::set_mute(options.sound_device.as_deref(), before);
        lines.push(format!(
            "speakers: put back (muted: {before}){}",
            if restored.is_ok() { "" } else { " - failed" }
        ));
    }
    if !terminals.is_empty() {
        lines.push(format!(
            "{} command lines closed with the session",
            terminals.len()
        ));
    }
    drop(terminals);
    sound_report.extend(sound.as_ref().map(sound_line));
    lines.extend(sound_report);
    if transfers.busy() {
        lines.push("files: a transfer was cut off when the session ended".into());
    }
    lines.push(format!(
        "bitrate: started at {:.1}, lowest {:.1}, last {:.1}, highest allowed {:.1} Mbit/s; {bitrate_changes} changes",
        f64::from(start_bitrate) / 1e6,
        f64::from(lowest_bitrate) / 1e6,
        f64::from(encoder.bitrate()) / 1e6,
        f64::from(max_bitrate) / 1e6
    ));
    lines.push(format!(
        "input: {} events, {} refused{}",
        stats.inputs,
        stats.inject_failures,
        if stats.inject_failures > 0 {
            format!(" ({})", stats.last_inject_error)
        } else {
            String::new()
        }
    ));
    if options.verify_input {
        lines.push(format!(
            "input check: pointer at the expected place {}/{}, test key state right {}/{}{}",
            stats.moves_ok,
            stats.moves_checked,
            stats.keys_ok,
            stats.keys_checked,
            if stats.deltas_checked > 0 {
                format!(
                    ", relative moves right {}/{}",
                    stats.deltas_ok, stats.deltas_checked
                )
            } else {
                String::new()
            }
        ));
    }
    let ended = closed.unwrap_or_else(|| "time is up".into());
    lines.push(format!("ended: {ended}"));
    let input_ok = !options.verify_input
        || (stats.moves_checked > 0
            && stats.moves_ok == stats.moves_checked
            && stats.keys_checked == 2
            && stats.keys_ok == 2
            && stats.deltas_ok == stats.deltas_checked);
    let passed =
        connected_after.is_some() && stats.sent > 0 && stats.inject_failures == 0 && input_ok;
    Ok(SessionOutcome { passed, ended })
}

/// After an injected event: is the pointer where it should be, is the test key in the right state?
/// A relative move must move the pointer the same way (the distance follows the PC's pointer speed), unless it
/// was already at that edge of the desktop.
fn verify_delta(event: &InputEvent, before: (i32, i32), stats: &mut Stats) {
    let InputEvent::MouseDelta { dx, dy } = *event else {
        return;
    };
    let after = cursor_position();
    let right_way =
        |moved: i32, wanted: i16| wanted == 0 || moved.signum() == i32::from(wanted.signum());
    stats.deltas_checked += 1;
    if right_way(after.0 - before.0, dx) && right_way(after.1 - before.1, dy) {
        stats.deltas_ok += 1;
    }
}

fn verify(injector: &Injector, event: &InputEvent, stats: &mut Stats) {
    match *event {
        InputEvent::MouseMove { x, y } => {
            let expected = injector.screen().desktop_point(x, y);
            let actual = cursor_position();
            stats.moves_checked += 1;
            if (expected.0 - actual.0).abs() <= 2 && (expected.1 - actual.1).abs() <= 2 {
                stats.moves_ok += 1;
            }
        }
        InputEvent::Key { scancode, down, .. } if scancode == TEST_SCANCODE => {
            stats.keys_checked += 1;
            if key_is_down(VK_F24) == down {
                stats.keys_ok += 1;
            }
        }
        _ => {}
    }
}

/// This device as the service's agent at the sign-in screen is to be (None without a key or a saved login).
fn logon_identity(
    identity: &DeviceIdentity,
    login: &SavedLogin,
    account: &Account,
) -> Option<queqiao_local::LogonIdentity> {
    Some(queqiao_local::LogonIdentity {
        device_id: identity.id.clone(),
        device_name: identity.name.clone(),
        key_pkcs8: identity.key.as_ref()?.pkcs8().to_vec(),
        server: login.server.clone(),
        pin: login.pin.clone(),
        refresh_token: login.refresh_token.clone()?,
        allow_account_control: true,
        pins: account.pins(),
    })
}

/// The desktop helper's first picture (a session that starts while another desktop takes input).
fn first_helper_picture(link: &mut service_link::DesktopLink) -> Result<BgraImage, String> {
    let until = Instant::now() + Duration::from_secs(3);
    let mut failure = String::new();
    while let Some(left) = until.checked_duration_since(Instant::now()) {
        match link.next(left) {
            Some(FromHelper::Frame {
                width,
                height,
                pixels,
            }) => {
                return Ok(BgraImage {
                    width,
                    height,
                    pixels,
                });
            }
            Some(FromHelper::Failed(error)) => failure = error,
            Some(FromHelper::Desktop(_) | FromHelper::EndPressed | FromHelper::Screen { .. }) => {}
            None if link.closed => break,
            None => {}
        }
    }
    Err(format!(
        "the desktop helper sent no picture within 3 s{}",
        if failure.is_empty() {
            String::new()
        } else {
            format!(" ({failure})")
        }
    ))
}

fn upload(device: &ID3D11Device, image: &BgraImage) -> Result<ID3D11Texture2D, String> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: image.width,
        Height: image.height,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let data = D3D11_SUBRESOURCE_DATA {
        pSysMem: image.pixels.as_ptr().cast(),
        SysMemPitch: image.width * 4,
        SysMemSlicePitch: 0,
    };
    let mut out = None;
    unsafe { device.CreateTexture2D(&desc, Some(&data), Some(&mut out)) }.map_err(describe)?;
    out.ok_or_else(|| "CreateTexture2D returned nothing".to_string())
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
