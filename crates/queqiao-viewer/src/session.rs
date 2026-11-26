//! The viewer's session with one agent, on every system (docs/LINUX.md step 3): the agent's screen in a window,
//! this window's input to the agent, the clipboard, files, sound, command lines and the session menu. What differs
//! per system comes through `crate::platform` (Windows: win.rs; Linux: linux.rs) and the platform layers of the
//! crates; moved from win.rs.

use crate::hotkeys;
use crate::system_keys::SystemKey;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use queqiao_audio::{Player, TEST_TONE_HZ};
use queqiao_capture::{Context, Device};
use queqiao_codec::{DecodedFrame, DecoderChoice, H264Decoder};
use queqiao_core::protocol::{
    Command, InputEvent, MAX_CLIPBOARD_TEXT, Message, MouseButton, Quality,
};
use queqiao_core::text::{tr, trf};
use queqiao_display::{Presenter, Window, WindowEvent, ask_text, fit, popup_menu, show_error};
use queqiao_files::{ImageReceiver, ImageSender, image_fingerprint, test_picture};
use queqiao_files::{Outcome, Transfers, downloads_folder};
use queqiao_input::{ClipContent, Clipboard};
use queqiao_terminal::{TerminalMessage, TerminalWindow, WindowEvent as TerminalEvent};
use queqiao_transport::{Addresses, Peer, PeerEvent, Role, Signaling, Stream};

use crate::options::{Options, Then};
use crate::platform::describe;

/// The session window's menu items for the controlled PC (system-menu ids: multiples of 16).
pub(crate) const MENU_CTRL_ALT_DEL: u16 = 0x1000;

pub(crate) const MENU_LOCK: u16 = 0x1010;

pub(crate) const MENU_RESTART: u16 = 0x1020;

pub(crate) const MENU_SOUND: u16 = 0x1030;

pub(crate) const MENU_FULL_SCREEN: u16 = 0x1040;

pub(crate) const MENU_PRIVACY: u16 = 0x1050;

pub(crate) const MENU_TERMINAL: u16 = 0x1060;

pub(crate) const MENU_GAME_MOUSE: u16 = 0x1070;

pub(crate) const MENU_MUTE_SPEAKERS: u16 = 0x1080;

pub(crate) const MENU_SHORTCUTS: u16 = 0x1090;

pub(crate) const MENU_ON_TOP: u16 = 0x10A0;

pub(crate) const MENU_CUSTOM_BITRATE: u16 = 0x10B0;

pub(crate) const MENU_TRANSFERS: u16 = 0x10C0;

pub(crate) const MENU_RESTART_QUEQIAO: u16 = 0x10D0;

pub(crate) const MENU_COMMAND_LINES: u16 = 0x10E0;

pub(crate) const MENU_SEND_FILES: u16 = 0x10F0;

// System-menu command ids are multiples of 16: Windows reports them masked with 0xFFF0.
pub(crate) const MENU_ACTUAL_SIZE: u16 = 0x1640;

pub(crate) const MENU_HOTKEYS: u16 = 0x1650;

pub(crate) const MENU_POINTER_SHAPE: u16 = 0x1660;

pub(crate) const MENU_MICROPHONE: u16 = 0x16D0;

/// The card of the session's figures at the bottom right.
pub(crate) const MENU_FIGURES: u16 = 0x16E0;

/// The file transfer window (the bar's first button; the control center's Tools).
pub(crate) const MENU_FILE_TRANSFER: u16 = 0x1720;

/// The control center's volume slider (WindowEvent::Value).
pub(crate) const MENU_VOLUME: u16 = 0x1730;

/// That PC's next screen (the control center).
pub(crate) const MENU_NEXT_SCREEN: u16 = 0x1740;

/// End the session (the control center's last item).
pub(crate) const MENU_END: u16 = 0x1750;

/// Lock that PC when the session ends (the control center's Security).
pub(crate) const MENU_LOCK_AFTER: u16 = 0x1760;

/// The picture's codec: (menu id, preference, label).
pub(crate) const MENU_CODEC: [(u16, queqiao_core::protocol::CodecPreference, &str); 4] = [
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
    (
        0x16F0,
        queqiao_core::protocol::CodecPreference::H265,
        "H.265 (both PCs need it)",
    ),
];

/// H.265's 4:4:4 colour (sharp coloured text) and 10 bits (smooth gradients), as UU Remote offers them: toggles in
/// the Picture codec menu, used when the picture is H.265 and both PCs can.
pub(crate) const MENU_CHROMA444: u16 = 0x1700;

pub(crate) const MENU_TEN_BIT: u16 = 0x1710;

/// The game mouse's speed: (menu id, percent, label).
pub(crate) const MENU_MOUSE_SPEED: [(u16, u32, &str); 3] = [
    (0x1670, 50, "Slow (x0.5)"),
    (0x1680, 100, "Normal"),
    (0x1690, 200, "Fast (x2)"),
];

/// The window's opacity: (menu id, percent, label).
pub(crate) const MENU_OPACITY: [(u16, u8, &str); 4] = [
    (0x1600, 100, "Opaque"),
    (0x1610, 80, "80 %"),
    (0x1620, 60, "60 %"),
    (0x1630, 40, "40 %"),
];

/// The boss key's id with Windows.
pub(crate) const BOSS_HOTKEY: i32 = 1;

/// The other PC's screens: menu ids from here, 16 apart.
pub(crate) const MENU_SCREENS: u16 = 0x1400;

/// Resolution: that screen's own, or its short side at most 1080 or 720 pixels.
pub(crate) const MENU_RESOLUTION: [(u16, u16, &str); 3] = [
    (0x1300, 0, "As that screen"),
    (0x1310, 1080, "1080p"),
    (0x1320, 720, "720p"),
];

/// The game-mouse test's relative moves (mouse counts).
pub(crate) const TEST_DELTAS: [(i16, i16); 4] = [(40, 0), (0, 40), (-40, 0), (0, -40)];

/// Picture settings: frame rate (at most 30, 60, or what that screen shows) and quality.
pub(crate) const MENU_FPS: [(u16, u8, &str); 3] = [
    (0x1100, 30, "30 frames per second"),
    (0x1110, 60, "60 frames per second"),
    (0x1120, 0, "As many as that screen shows"),
    // (translated where the menu is made)
];

pub(crate) const MENU_QUALITY: [(u16, Quality, &str); 3] = [
    (
        0x1200,
        Quality::Smooth,
        "Smooth (fewer bits, slow networks)",
    ),
    (0x1210, Quality::Balanced, "Balanced"),
    (0x1220, Quality::Sharp, "Sharp (twice the bits, for text)"),
];

/// A new command line's window size until its window reports its own.
pub(crate) const TERMINAL_SIZE: (u16, u16) = (120, 30);

/// The start of the clipboard text the tests send (each side checks it got the other's).
pub(crate) const CLIPBOARD_TEST_TEXT: &str = "Queqiao clipboard test";

/// The input test: pointer positions as fractions of the remote screen, then F24 down and up.
pub(crate) const TEST_POINTS: [(f64, f64); 5] = [
    (0.25, 0.25),
    (0.75, 0.25),
    (0.75, 0.75),
    (0.25, 0.75),
    (0.5, 0.5),
];

pub(crate) const TEST_SCANCODE: u16 = 0x76;

/// The input test's events in order: the pointer to each test point, a left click there with --input-test-click
/// (where the desktop gives the keyboard to the window clicked), the test key down and up, the game mouse's moves.
pub(crate) fn input_test_steps(options: &Options) -> Vec<InputEvent> {
    let mut steps: Vec<InputEvent> = TEST_POINTS
        .iter()
        .map(|(x, y)| InputEvent::MouseMove {
            x: (x * 65535.0) as u16,
            y: (y * 65535.0) as u16,
        })
        .collect();
    if options.input_test_click {
        for down in [true, false] {
            steps.push(InputEvent::MouseButton {
                button: MouseButton::Left,
                down,
            });
        }
    }
    for down in [true, false] {
        steps.push(InputEvent::Key {
            scancode: TEST_SCANCODE,
            extended: false,
            down,
        });
    }
    if options.game_mouse_test {
        steps.extend(
            TEST_DELTAS
                .iter()
                .map(|(dx, dy)| InputEvent::MouseDelta { dx: *dx, dy: *dy }),
        );
    }
    steps
}

/// Tests: the control center opened by the bar's button: when, the step, the button's corner on the screen.
type PanelTest = (Instant, u8, Option<(i32, i32)>);

/// Tests: how many steps the window test (--window-test) takes.
const WINDOW_TEST_STEPS: u8 = 14;

/// A notice the agent sent at most this long before ending the session explains why it ended.
const NOTICE_BEFORE_LEAVE: Duration = Duration::from_secs(3);

/// A session that is not connected after this long is given up.
pub(crate) const CONNECT_LIMIT: Duration = Duration::from_secs(30);

#[derive(Default)]
pub(crate) struct Stats {
    pub(crate) received: u32,
    pub(crate) lost: u32,
    /// After a loss: how often the last good picture was kept, and the frames held back until a keyframe.
    pub(crate) holds: u32,
    pub(crate) held_back: u32,
    /// Frames before the first picture that were not keyframes (each asks the agent for one).
    pub(crate) waiting_for_keyframe: u32,
    pub(crate) decoded: u32,
    pub(crate) shown: u32,
    pub(crate) bytes: usize,
    pub(crate) decode_ms: Vec<f64>,
    /// Arrival of the frame -> on screen, per frame time.
    pub(crate) viewer_ms: HashMap<u64, f64>,
    /// Screen change -> sent, reported by the agent per frame time.
    pub(crate) agent_ms: HashMap<u64, f64>,
    pub(crate) rtt_ms: Vec<f64>,
    pub(crate) inputs_sent: u32,
    /// Where the viewer's loop spends its time: handling window messages, frame arrival -> picked up by
    /// the loop, putting a picture on screen.
    pub(crate) pump_ms: Vec<f64>,
    pub(crate) pickup_ms: Vec<f64>,
    pub(crate) show_ms: Vec<f64>,
}

/// The session's state in the window title, once a second: delay (round trip), frames shown per second,
/// data received and whether the session is direct or relayed. A notice from the controlled PC stays there
/// for a few seconds.
pub(crate) struct TitleStatus {
    pub(crate) agent: String,
    pub(crate) relayed: bool,
    pub(crate) title: String,
    pub(crate) notice: Option<(String, Instant)>,
    /// Counters at the last update, for the rates.
    pub(crate) last: (Instant, u32, usize),
}

pub(crate) const NOTICE_SHOWN: Duration = Duration::from_secs(6);

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
pub(crate) const MAX_SAMPLES: usize = 200_000;

/// One session: the agent's screen in a window, this window's input to the agent.
pub(crate) fn session(
    options: &Options,
    device: &Device,
    context: &Context,
    signal: &mut dyn Signaling,
    agent: &str,
    addresses: Addresses,
    lines: &mut Vec<String>,
) -> Result<bool, String> {
    let started = Instant::now();
    let peer = Peer::start_with(Role::Viewer, signal, addresses).map_err(|e| e.to_string())?;
    run_session(options, device, context, peer, started, agent, lines)
}

/// A session once its peer is set up: the window, the picture, input, sound, files, command lines.
pub(crate) fn run_session(
    options: &Options,
    device: &Device,
    context: &Context,
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
    // Linux: the picture last shown, kept (moved, not copied) to show again where the window lost it.
    #[cfg(target_os = "linux")]
    let mut last_shown: Option<DecodedFrame> = None;
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
    // H.265: what this PC decodes (Intel's decoder: 4:4:4 and 10-bit too), and whether 4:4:4 and 10 bits are wanted.
    let hevc_caps = queqiao_codec::hevc_decode_caps(device, context);
    lines.push(format!(
        "H.265 this PC decodes: {}",
        queqiao_core::protocol::hevc::describe(hevc_caps)
    ));
    let (mut chroma444, mut ten_bit) = (options.chroma444, options.ten_bit);
    // A decoder for the other codec, made before it is needed: making Windows' AV1 decoder takes a while, and doing
    // it when the first AV1 frame comes would hold this window (and its keyboard hook) just after the first picture.
    let mut spare_decoder: Option<(queqiao_core::protocol::VideoCodec, H264Decoder)> = None;
    let spare_codec = match codec_choice {
        queqiao_core::protocol::CodecPreference::H265
            if hevc_caps & queqiao_core::protocol::hevc::MAIN != 0 =>
        {
            Some(queqiao_core::protocol::VideoCodec::H265)
        }
        queqiao_core::protocol::CodecPreference::H264
        | queqiao_core::protocol::CodecPreference::H265 => None,
        choice if av1_for(choice) => Some(queqiao_core::protocol::VideoCodec::Av1),
        _ => None,
    };
    if let Some(spare) = spare_codec {
        let making = Instant::now();
        match H264Decoder::with_codec(spare, device, context, DecoderChoice::from_env()) {
            Ok(made) => {
                lines.push(format!(
                    "decoder for {} ready: {} (made in {} ms)",
                    spare.name(),
                    made.name(),
                    making.elapsed().as_millis()
                ));
                spare_decoder = Some((spare, made));
            }
            Err(error) => lines.push(format!("decoder for {}: {}", spare.name(), describe(error))),
        }
    }
    // Frames of each codec: H.264, AV1, H.265.
    let mut frames_by_codec = [0u32; 3];
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
    // The file transfer window (UU Remote's two panes and transfer list), while open.
    let mut files_ui = crate::files_ui::FilesUi::new(agent);
    if let Some(folder) = &options.browse_test {
        files_ui.test = Some(crate::files_ui::BrowseTest::new(folder));
    }
    // Tests: the file window's picture is taken once its test is over.
    let mut files_picture_due = options.browse_test.is_some() && options.ui_pictures.is_some();
    let mut files_done_at: Option<Instant> = None;
    // That PC's Windows password, while it asks for it (locked); the tests' passwords instead.
    let mut password_form: Option<queqiao_display::PasswordForm> = None;
    let mut test_passwords: std::collections::VecDeque<String> =
        options.test_passwords.iter().cloned().collect();
    let mut cancel_test_done = false;
    // The secure-desktop test: when it started, its step, and the pictures shown when that step began.
    let mut secure_test: Option<(Instant, u8, u64)> = None;
    // The typing test (Linux sign-in and lock screens): when the first picture came, and the step.
    let mut type_test: Option<(Instant, u8)> = None;
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
        .chain([
            (MENU_CHROMA444, tr("4:4:4 colour (H.265)")),
            (MENU_TEN_BIT, tr("10-bit colour (H.265)")),
        ])
        .collect();
    window.add_submenu(tr("Picture codec"), &codec_items);
    for (item, choice, _) in MENU_CODEC {
        window.check_menu_item(item, choice == options.codec.unwrap_or_default());
    }
    window.check_menu_item(MENU_CHROMA444, options.chroma444);
    window.check_menu_item(MENU_TEN_BIT, options.ten_bit);
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
    let mut current_screen = 0u8;
    // The control center (the bar's "Control center"): UU Remote's groups with Queqiao's choices; built again when
    // that PC's screens or the keys change. Its check marks follow the menu's.
    let mut screen_names: Vec<String> = Vec::new();
    let mut volume = 100u8;
    let build_panel = |window: &Window, screens: &[String], keys: &hotkeys::Keys, volume: u8| {
        window.set_panel(crate::control::items(
            screens,
            &keys.full_screen().text(),
            &keys.game_mouse().text(),
            volume,
        ));
    };
    build_panel(&window, &screen_names, &keys, volume);
    let mut lock_after = false;
    window.check_menu_item(MENU_LOCK_AFTER, false);
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
            let owner = queqiao_display::owner_from_raw(owner);
            let _ = back.send(queqiao_display::pick_files(Some(owner), &title));
        });
    };
    // What the main window asked for, done once the session is up.
    let mut then = options.then;
    // Queqiao's bar at the top (tucked away when the pointer leaves it): the other PC's name with the delay's dot and
    // the session's time, file transfer, Ctrl+Alt+Del, full screen, the control center (everything else, grouped as
    // UU Remote does) and End; the card of figures at the bottom right. The window's menu (Alt+Space) stays.
    use queqiao_display::{BAR_END, BAR_MORE, BarButton, Figures, SessionOverlay};
    let mut overlay = SessionOverlay::new(
        window.hwnd(),
        agent,
        vec![
            BarButton {
                glyph: "\u{E8B7}",
                label: tr("File transfer").to_string(),
                id: MENU_FILE_TRANSFER,
            },
            BarButton {
                glyph: "\u{E765}",
                label: "Ctrl+Alt+Del".to_string(),
                id: MENU_CTRL_ALT_DEL,
            },
            BarButton {
                glyph: "\u{E740}",
                label: tr("Full screen").to_string(),
                id: MENU_FULL_SCREEN,
            },
            BarButton {
                glyph: "\u{E713}",
                label: tr("Control center").to_string(),
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
    // Keys go to that PC for its own input method: this PC's never composes in the session window.
    lines.push(format!(
        "this PC's input method in the session window: {}",
        if window.has_input_method() {
            "on"
        } else {
            "off"
        }
    ));
    // The card of figures (the network's state) shows from the start; the person drags it about or closes it.
    if let Some(overlay) = overlay.as_mut() {
        overlay.set_card(true);
        window.check_menu_item(MENU_FIGURES, true);
    }
    if options.figures
        && let Some(overlay) = overlay.as_mut()
    {
        overlay.keep_bar_out(true);
        // "Full screen" as under the pointer: its highlight (and its tip on a bar without labels) in the picture.
        overlay.hover_for_test(Some(2));
        window.check_menu_item(MENU_FIGURES, true);
    }
    // Tests: the picture of the window with its overlays, taken once, a few seconds after the first picture.
    let mut overlay_picture = options.ui_pictures.clone();
    let mut course: Vec<String> = Vec::new();
    let mut course_at = (Instant::now(), 0usize, 0u32, 0u32);
    // After a loss: the last good picture kept until a keyframe comes.
    let mut held_after_loss = false;
    // Tests: pictures after a loss and after a keyframe (taken, and when due).
    let mut moment_pictures = (0u32, 0u32);
    let mut due_pictures: Vec<(Instant, String)> = Vec::new();
    // Then one with the bar let go, tucked into its tab (unless the pointer is at the top of the window).
    let mut tucked_picture: Option<(PathBuf, Instant)> = None;
    // Tests: the control center opened by the bar's button (when, the step, the button's corner on the screen).
    let mut panel_test: Option<PanelTest> = None;
    // Tests: the window test's next step.
    let mut window_test_step: u8 = 0;
    window.check_menu_item(MENU_PRIVACY, privacy_on);
    window.set_full_screen(options.full_screen);
    window.check_menu_item(MENU_SOUND, sound_on);
    let mut closed = None;
    // The agent's last notice and when it came (shown if the agent ends the session right after it).
    let mut last_notice: Option<(String, Instant)> = None;

    'session: loop {
        if let Some(overlay) = overlay.as_mut() {
            overlay.tick();
            if overlay.take_card_closed() {
                window.check_menu_item(MENU_FIGURES, false);
                lines.push("card of figures: closed with its button".to_string());
            }
        }
        if let Some(shown) = first_picture_after
            && started.elapsed() > shown + Duration::from_secs(options.picture_at.unwrap_or(3))
            && let Some(folder) = overlay_picture.take()
        {
            let path = folder.join("viewer_session.bmp");
            match queqiao_display::save_screen_picture(window.hwnd(), &path) {
                Ok(()) => lines.push(format!("session picture: {}", path.display())),
                Err(error) => lines.push(format!("session picture: {error}")),
            }
            if let Some(overlay) = overlay.as_mut() {
                // The bar's "Control center" as a click on it gives it; then (panel_test) whether it opened, the
                // Picture group open beside it in a picture, closed again.
                if overlay.click_for_test(BAR_MORE) {
                    panel_test = Some((Instant::now(), 0, overlay.anchor_of(BAR_MORE)));
                } else {
                    lines.push("bar: no Control center button to click".to_string());
                }
                overlay.keep_bar_out(false);
                overlay.hover_for_test(None);
            }
            tucked_picture = Some((folder, Instant::now() + Duration::from_millis(2000)));
        }
        if let Some((since, step, anchor)) = panel_test
            && let Some(folder) = &options.ui_pictures
        {
            let at = since.elapsed();
            if step == 0 && at > Duration::from_millis(400) {
                lines.push(format!(
                    "control center open after the click: {}",
                    window.panel_open()
                ));
                // Closed, then opened again with the Picture group beside it.
                if window.panel_open() {
                    window.open_panel(anchor.unwrap_or((0, 0)), None);
                }
                window.open_panel(
                    anchor.unwrap_or((0, 0)),
                    Some(crate::control::GROUP_PICTURE),
                );
                panel_test = Some((since, 1, anchor));
            } else if step == 1 && at > Duration::from_millis(1300) {
                let path = folder.join("viewer_panel.bmp");
                match window.save_panel_picture(&path) {
                    Ok(()) => lines.push(format!("control center picture: {}", path.display())),
                    Err(error) => lines.push(format!("control center picture: {error}")),
                }
                panel_test = Some((since, 2, anchor));
            } else if step == 2 && at > Duration::from_millis(1700) {
                if window.panel_open() {
                    window.open_panel(anchor.unwrap_or((0, 0)), None);
                }
                lines.push(format!("control center closed: {}", !window.panel_open()));
                panel_test = None;
            }
        }
        // Window test (--window-test): from 2 s after the first picture, a step every 1.2 s. As the person does it:
        // the pointer at the top brings the bar out (it is not kept out), full screen on and off, maximised, minimised.
        if options.window_test
            && window_test_step < WINDOW_TEST_STEPS
            && let Some(shown) = first_picture_after
            && let Some(overlay) = overlay.as_mut()
            && started.elapsed()
                > shown + Duration::from_millis(2000 + 1200 * u64::from(window_test_step))
        {
            let ((bar_shown, bar_front), (card_shown, card_front)) = overlay.shown_for_test();
            let (bar_seen, bar_place) = overlay.bar_seen_for_test();
            let state = format!(
                "bar shown {bar_shown}, in front {bar_front}, seen {bar_seen}; card shown {card_shown}, in front {card_front} ({bar_place})"
            );
            match window_test_step {
                0 => {
                    overlay.set_card(true);
                    window.set_full_screen(true);
                }
                1 | 3 | 6 | 8 | 11 => overlay.point_at_top_for_test(),
                2 => {
                    lines.push(format!("window test: in full screen: {state}"));
                    window.set_full_screen(false);
                }
                4 => {
                    lines.push(format!("window test: after full screen: {state}"));
                    window.set_maximized(true);
                }
                5 => window.set_full_screen(true),
                7 => {
                    lines.push(format!("window test: maximized, in full screen: {state}"));
                    window.set_full_screen(false);
                }
                9 => {
                    lines.push(format!(
                        "window test: maximized, after full screen: {state}"
                    ));
                    window.set_maximized(false);
                    window.set_minimized(true);
                }
                // Figures came (once a second) while minimised.
                10 => {
                    lines.push(format!("window test: minimized: {state}"));
                    window.set_minimized(false);
                }
                12 => {
                    lines.push(format!("window test: restored: {state}"));
                    // Dragged far out to the top left, then to the bottom right: it stays inside the window.
                    let first = overlay.drag_card_for_test(-5000, -5000);
                    let second = overlay.drag_card_for_test(10000, 10000);
                    lines.push(format!(
                        "window test: card inside the window after dragging it out: {}",
                        first && second
                    ));
                }
                _ => {
                    overlay.close_card_for_test();
                    let ((_, _), (card_shown, _)) = overlay.shown_for_test();
                    lines.push(format!(
                        "window test: card closed by its button: {}",
                        !overlay.card_shown() && !card_shown
                    ));
                    lines.push("window test: done".to_string());
                }
            }
            window_test_step += 1;
        }
        if let Some(folder) = &options.ui_pictures {
            let now = Instant::now();
            for (_, name) in due_pictures.iter().filter(|(at, _)| *at <= now) {
                let path = folder.join(name);
                match queqiao_display::save_screen_picture(window.hwnd(), &path) {
                    Ok(()) => lines.push(format!(
                        "{} ms: picture {}",
                        started.elapsed().as_millis(),
                        path.display()
                    )),
                    Err(error) => lines.push(format!("picture {name}: {error}")),
                }
            }
            due_pictures.retain(|(at, _)| *at > now);
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
        // The report: once a second for the first two minutes, what arrived (Mbit/s), frames shown, L: frames after a
        // loss among them.
        if course.len() < 120 && course_at.0.elapsed() >= Duration::from_secs(1) {
            let seconds = course_at.0.elapsed().as_secs_f64();
            course.push(format!(
                "{:.1}@{:.0}{}",
                (stats.bytes - course_at.1) as f64 * 8.0 / seconds / 1e6,
                f64::from(stats.shown - course_at.2) / seconds,
                if stats.lost > course_at.3 { "L" } else { "" }
            ));
            course_at = (Instant::now(), stats.bytes, stats.shown, stats.lost);
        }
        if let Some((fps, rate)) = status.update(&window, &stats)
            && let Some(overlay) = overlay.as_mut()
        {
            let codec = match decoder_codec {
                queqiao_core::protocol::VideoCodec::Av1 => "AV1",
                queqiao_core::protocol::VideoCodec::H265 => "H.265",
                queqiao_core::protocol::VideoCodec::H264 => "H.264",
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
        // Linux: the last picture again where the window lost it (the remote screen may send no new one).
        #[cfg(target_os = "linux")]
        if window.needs_repaint()
            && let Some(picture) = &last_shown
        {
            let _ = presenter.show(&picture.texture, (picture.width, picture.height), &window);
        }
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
            // The control center's volume slider.
            if let WindowEvent::Value { id, value } = event {
                if id == MENU_VOLUME {
                    volume = value;
                    player.set_volume(value);
                }
                continue;
            }
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
                        current_screen = index;
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
                    MENU_TRANSFERS | MENU_FILE_TRANSFER => {
                        files_ui.open(&mut transfers, lines);
                        None
                    }
                    MENU_NEXT_SCREEN if screen_count > 1 => {
                        current_screen = (current_screen + 1) % screen_count;
                        for other in 0..screen_count {
                            window.check_menu_item(MENU_SCREENS + 16 * u16::from(other), other == current_screen);
                        }
                        Some(Command::Screen(current_screen))
                    }
                    MENU_LOCK_AFTER => {
                        lock_after = !lock_after;
                        window.check_menu_item(MENU_LOCK_AFTER, lock_after);
                        Some(Command::LockAfter(lock_after))
                    }
                    MENU_END => {
                        closed = Some("ended here (control center)".to_string());
                        break 'session;
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
                            can_decode_hevc: hevc_caps,
                            chroma444,
                            ten_bit,
                        })
                    }
                    MENU_CHROMA444 | MENU_TEN_BIT => {
                        if id == MENU_CHROMA444 {
                            chroma444 = !chroma444;
                        } else {
                            ten_bit = !ten_bit;
                        }
                        window.check_menu_item(MENU_CHROMA444, chroma444);
                        window.check_menu_item(MENU_TEN_BIT, ten_bit);
                        Some(Command::VideoCodec {
                            prefer: codec_choice,
                            can_decode_av1: av1_for(codec_choice),
                            can_decode_hevc: hevc_caps,
                            chroma444,
                            ten_bit,
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
                            build_panel(&window, &screen_names, &keys, volume);
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
            && let Some(due) = next_test_step.or_else(|| {
                Some(Instant::now() + Duration::from_secs(options.input_test_at.unwrap_or(1)))
            })
        {
            next_test_step = Some(due);
            let steps = input_test_steps(options);
            if Instant::now() >= due && test_step < steps.len() {
                let event = steps[test_step];
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

        // Test: Enter, then a text and Enter, on a Linux PC's sign-in or lock screen.
        if let Some(text) = &options.type_test {
            if type_test.is_none() && stats.shown > 0 {
                type_test = Some((Instant::now(), 0));
            }
            if let Some((began, step)) = type_test {
                let press = |peer: &Peer, scancode: u16, shift: bool| {
                    let mut keys = Vec::new();
                    if shift {
                        keys.push((0x2A, true));
                    }
                    keys.extend([(scancode, true), (scancode, false)]);
                    if shift {
                        keys.push((0x2A, false));
                    }
                    for (scancode, down) in keys {
                        let key = InputEvent::Key {
                            scancode,
                            extended: false,
                            down,
                        };
                        peer.send_data(Message::Input(key).encode());
                        std::thread::sleep(Duration::from_millis(30));
                    }
                };
                if step == 0 && began.elapsed() >= Duration::from_secs(3) {
                    press(&peer, 0x1C, false);
                    lines.push("type test: sent Enter".into());
                    type_test = Some((began, 1));
                } else if step == 1 && began.elapsed() >= Duration::from_secs(5) {
                    let rows = ["1234567890-", "qwertyuiop", "asdfghjkl", "zxcvbnm"];
                    let starts = [0x02u16, 0x10, 0x1E, 0x2C];
                    for c in text.chars() {
                        let lower = c.to_ascii_lowercase();
                        let scancode = match lower {
                            ' ' => Some(0x39),
                            '.' => Some(0x34),
                            _ => rows.iter().zip(starts).find_map(|(row, start)| {
                                row.find(lower).map(|at| start + at as u16)
                            }),
                        };
                        if let Some(scancode) = scancode {
                            press(&peer, scancode, c.is_ascii_uppercase());
                        }
                    }
                    press(&peer, 0x1C, false);
                    lines.push(format!(
                        "type test: sent {} characters and Enter",
                        text.chars().count()
                    ));
                    type_test = Some((began, 2));
                }
            }
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
        // The file transfer window: listings that came, what the person did, the transfer list. Tests: its test
        // (--browse-test) once the session's data channel is open and the session window's own pictures are taken
        // (the file window would cover it), and its picture when that is over.
        if options.browse_test.is_some()
            && channel_open
            && first_picture_after.is_some()
            && overlay_picture.is_none()
            && panel_test.is_none()
            && tucked_picture.is_none()
            && !files_ui.is_open()
            && !files_ui.test_done()
        {
            files_ui.open(&mut transfers, lines);
        }
        files_ui.poll(&mut transfers, lines);
        // The picture a second after the test: the transfer list shows the jobs done by then.
        if files_picture_due && files_ui.test_done() && files_done_at.is_none() {
            files_done_at = Some(Instant::now());
        }
        if files_picture_due
            && files_done_at.is_some_and(|at| at.elapsed() > Duration::from_millis(1200))
            && let Some(folder) = &options.ui_pictures
        {
            let path = folder.join("viewer_files.bmp");
            match files_ui.save_picture(&path) {
                Ok(()) => lines.push(format!("file transfer picture: {}", path.display())),
                Err(error) => lines.push(format!("file transfer picture: {error}")),
            }
            files_picture_due = false;
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
        if outcomes
            .iter()
            .any(|o| matches!(o, Outcome::Sent { .. } | Outcome::Received { .. }))
        {
            lines.extend(transfers.take_courses());
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
                        Some(Then::Files) => files_ui.open(&mut transfers, lines),
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
                            can_decode_hevc: hevc_caps,
                            chroma444,
                            ten_bit,
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
                    frames_by_codec[match codec {
                        queqiao_core::protocol::VideoCodec::H264 => 0,
                        queqiao_core::protocol::VideoCodec::Av1 => 1,
                        queqiao_core::protocol::VideoCodec::H265 => 2,
                    }] += 1;
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
                    // Tests: a picture of the window just after the first two frames after a loss and the first
                    // two keyframes after the first picture (what the person saw then).
                    if options.ui_pictures.is_some() && stats.shown > 0 {
                        let soon = Instant::now() + Duration::from_millis(30);
                        if !contiguous && moment_pictures.0 < 2 {
                            moment_pictures.0 += 1;
                            due_pictures
                                .push((soon, format!("after_loss_{}.bmp", moment_pictures.0)));
                        }
                        if queqiao_codec::is_keyframe(codec, &data) && moment_pictures.1 < 2 {
                            moment_pictures.1 += 1;
                            due_pictures
                                .push((soon, format!("after_keyframe_{}.bmp", moment_pictures.1)));
                        }
                    }
                    // After a loss the frames that follow are made from a picture this side does not have:
                    // decoded, they showed blocks out of place and doubled text (measured over the PC -> sunshine
                    // path, 2026-11-23: str0m gives up on missing packets after about three round trips, and that
                    // path stalls for up to a second). The last good picture stays until a keyframe comes, asked
                    // for again while waiting (the transport spaces the requests 600 ms apart).
                    if !contiguous && stats.shown > 0 && !held_after_loss {
                        held_after_loss = true;
                        stats.holds += 1;
                    }
                    if held_after_loss && queqiao_codec::is_keyframe(codec, &data) {
                        held_after_loss = false;
                    }
                    'frame: {
                        if held_after_loss {
                            stats.held_back += 1;
                            peer.request_keyframe();
                            break 'frame;
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
                            #[cfg(target_os = "linux")]
                            {
                                last_shown = Some(picture);
                            }
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
                        // Ended there with a word for this side (another device of the account took control, or ended
                        // it from its device list): the person is shown it, as the window closes.
                        match last_notice
                            .take()
                            .filter(|(_, at)| at.elapsed() < NOTICE_BEFORE_LEAVE)
                        {
                            Some((notice, _)) => {
                                closed = Some(format!("the agent ended the session: {notice}"));
                                if options.may_ask() {
                                    window.set_visible(false);
                                    queqiao_display::show_info(tr("Queqiao"), &notice);
                                }
                            }
                            None => closed = Some("the agent ended the session".to_string()),
                        }
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
                        current_screen = current;
                        screen_names = names.clone();
                        build_panel(&window, &screen_names, &keys, volume);
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
                        last_notice = Some((text, Instant::now()));
                    }
                    Some(Message::PasswordNeeded { reason, tries_left }) => {
                        use queqiao_core::protocol::PasswordReason;
                        lines.push(format!("password: asked ({reason:?}, {tries_left} tries)"));
                        let text = match reason {
                            PasswordReason::Locked => tr(
                                "That PC is locked. Type the password of its account to show that it is you (this does not unlock it: unlock it on its own screen then).",
                            ),
                            PasswordReason::SignedOut => tr(
                                "Nobody is signed in on that PC. Type the password of the account that set up Queqiao there to show that it is you.",
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
    lines.push(format!(
        "arrival course, every second (Mbit/s@frames shown, L after a loss): {}",
        course.join(" ")
    ));
    lines.push(format!(
        "after a loss: the last good picture kept {} times, {} frames held back until a keyframe",
        stats.holds, stats.held_back
    ));
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
        "frames by codec: H.264 {}, AV1 {}, H.265 {} (this PC decodes AV1: {can_decode_av1}, on its GPU: {av1_on_gpu})",
        frames_by_codec[0], frames_by_codec[1], frames_by_codec[2]
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
    let test_steps = input_test_steps(options).len();
    let test_done = !options.input_test || test_step == test_steps;
    Ok(connected_after.is_some()
        && first_picture_after.is_some()
        && stats.shown == stats.decoded
        && test_done)
}

/// Window input -> protocol messages. Pointer positions are mapped from the picture's place in the window
/// to the remote screen (0..=65535); keys are tracked so they can be released when focus is lost.
pub(crate) fn translate(
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
        WindowEvent::FilesDropped(_)
        | WindowEvent::Menu(_)
        | WindowEvent::Hotkey(_)
        | WindowEvent::Value { .. } => {
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
pub(crate) struct TerminalWindows {
    pub(crate) lines: BTreeMap<u32, CommandLine>,
    pub(crate) next_id: u32,
    /// Test: (the shell, its output so far, the test text seen, the exit code, the window size).
    pub(crate) test: Option<TerminalTest>,
}

pub(crate) struct CommandLine {
    pub(crate) name: String,
    pub(crate) window: Option<TerminalWindow>,
    /// Output that came while detached.
    pub(crate) backlog: Vec<u8>,
    /// How it ended, while detached.
    pub(crate) ended: Option<String>,
}

/// The most output kept for a detached command line.
pub(crate) const BACKLOG: usize = 256 * 1024;

/// What can be done with a command line from the session menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LineAction {
    Show,
    Detach,
    Rename,
    End,
}

#[derive(Default)]
pub(crate) struct TerminalTest {
    pub(crate) shell: String,
    pub(crate) output: Vec<u8>,
    pub(crate) seen: bool,
    pub(crate) exit_code: Option<u32>,
    pub(crate) size: Option<(u16, u16)>,
    /// The test detaches the window right after the start and shows it again once the test output came.
    pub(crate) detached: bool,
    pub(crate) shown_again: bool,
}

/// What the terminal test types, by shell; its output differs from what is typed, so seeing it means the
/// command ran.
pub(crate) fn terminal_test_input(shell: &str) -> &'static str {
    match shell {
        "PowerShell" => "Write-Output ('queqiao' + '-terminal-ok')\r",
        "cmd" => "echo queqiao^-terminal-ok\r",
        // bash, zsh, sh (Linux): two quotes end the word and start it again, as the caret does for cmd.
        _ => "echo queqiao''-terminal-ok\r",
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

pub(crate) fn open_terminal_window(title: &str, test: bool) -> Result<TerminalWindow, String> {
    let program = std::env::current_exe().map_err(|e| e.to_string())?;
    TerminalWindow::open(&program, title, test)
}

/// Files copied to the clipboard go over at once up to this many bytes in all.
pub(crate) const CLIPBOARD_FILES_LIMIT: u64 = 512 << 20;

/// The size of files and folders (folders counted with everything in them, links not followed).
pub(crate) fn clipboard_size(paths: &[PathBuf]) -> u64 {
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
pub(crate) fn report_files(
    outcomes: Vec<Outcome>,
    peer: &str,
    lines: &mut Vec<String>,
    at: Duration,
) {
    let at = at.as_secs_f64();
    for outcome in outcomes {
        queqiao_files::history::record(&outcome, peer);
        lines.push(match outcome {
            Outcome::Sent { name, size, .. } => {
                format!("file sent: {name} ({size} bytes, checked by the agent) after {at:.1} s")
            }
            Outcome::Received { path, size, .. } => {
                format!(
                    "file received: {} ({size} bytes, SHA-256 checked) after {at:.1} s",
                    path.display()
                )
            }
            Outcome::Failed { name, reason, .. } => format!("file failed: {name}: {reason}"),
        });
    }
}

pub(crate) fn median(values: &mut [f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(f64::total_cmp);
    values[values.len() / 2]
}

pub(crate) fn summary(values: &mut [f64]) -> String {
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
pub(crate) fn scaled_move(value: i32, percent: u32, rest: &mut i32) -> i32 {
    let total = value * percent as i32 + *rest;
    let out = total / 100;
    *rest = total - out * 100;
    out
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
            MENU_FIGURES,
            MENU_FILE_TRANSFER,
            MENU_VOLUME,
            MENU_NEXT_SCREEN,
            MENU_END,
            MENU_LOCK_AFTER,
        ];
        ids.extend(MENU_MOUSE_SPEED.iter().map(|(id, _, _)| *id));
        ids.extend(MENU_CODEC.iter().map(|(id, _, _)| *id));
        ids.extend([MENU_CHROMA444, MENU_TEN_BIT]);
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
