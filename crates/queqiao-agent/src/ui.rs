//! The agent's UI thread ("queqiao-ui"): the tray icon, the banner during a session and the consent prompt
//! (queqiao_display::Presence), driven by commands from the agent's main thread. Showing the banner and
//! asking for consent wait for the UI thread's answer: a session whose banner cannot be shown is refused,
//! since nothing may happen unseen on the controlled PC.
//!
//! Also the agent's settings (`agent_settings.json`) and the session history (`sessions.log`) in
//! `%APPDATA%\Queqiao`.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use queqiao_display::{
    AccountState, ConnectionRow, MainDevice, Page, Presence, PresenceEvent, PrivacyScreen,
    TransferRow,
};
use serde::{Deserialize, Serialize};
#[cfg(target_os = "linux")]
type Area = queqiao_display::Rect;
#[cfg(windows)]
type Area = windows::Win32::Foundation::RECT;

enum Command {
    SessionStarted(String, Sender<Result<(), String>>),
    SessionEnded,
    Ask(String, u32, Sender<Result<(), String>>),
    /// For tests: the same path as a click.
    PressConsent(bool),
    PressEnd,
    /// For tests: a tray menu item chosen, as by a click.
    PressMenu(u16),
    /// Settings locked (someone of another account controls the PC) or not.
    LockSettings(bool),
    /// The settings password's stored form (None: no password).
    PasswordHash(Option<String>),
    /// The help window with this help ID and code, or none.
    Help(Option<(String, String)>),
    /// The privacy screen on (with how it looks) or off.
    Privacy(Option<PrivacyLook>, Sender<Result<bool, String>>),
    /// Tests: the privacy screen hidden from capture or not (Windows: Linux has no privacy screen yet).
    #[cfg_attr(target_os = "linux", allow(dead_code))]
    PrivacyExcluded(bool, Sender<Result<(), String>>),
    /// Online, offline (trying again) or not logged in.
    Account(AccountState),
    /// A fixed help code is set or not.
    FixedCode(bool),
    /// The main window, on that page if given.
    ShowMain(Option<Page>),
    /// The account's devices and name, for the main window.
    Devices(Vec<MainDevice>),
    User(String),
    /// The files sent and received, for the main window.
    Transfers(Vec<TransferRow>),
    Connections(Vec<ConnectionRow>),
    /// Tests: pictures of the main window's pages into the folder.
    MainPictures(PathBuf, Sender<Result<(), String>>),
    /// Tests: the Settings page's clicks and language (Presence::test_settings).
    TestSettings(Sender<Vec<String>>),
    /// A notification from the tray icon.
    Notify(String),
}

/// The privacy screen over all monitors (`area` None), or for tests a square of `color` (0x00BBGGRR).
pub struct PrivacyLook {
    pub area: Option<Area>,
    pub color: u32,
}

/// What the person at this PC reads on the privacy screen.
const PRIVACY_TEXT: &str = "This PC is being used remotely with Queqiao. Its screen stays dark and its keyboard and mouse stay off until the session ends. Ctrl+Alt+Del gives them back.";

pub struct Ui {
    commands: Sender<Command>,
    events: Receiver<PresenceEvent>,
    thread: Option<JoinHandle<()>>,
    pub tray_shown: bool,
}

impl Ui {
    /// With `pictures`, the banner and the prompt are saved there (banner.bmp, prompt.bmp) once drawn.
    pub fn start(settings: &Settings, pictures: Option<PathBuf>) -> Result<Ui, String> {
        let (command_sender, commands) = mpsc::channel();
        let (event_sender, events) = mpsc::channel();
        let (ready_sender, ready) = mpsc::channel();
        let settings = settings.clone();
        let thread = std::thread::Builder::new()
            .name("queqiao-ui".into())
            .spawn(move || {
                run(
                    &settings,
                    pictures.as_deref(),
                    &commands,
                    &event_sender,
                    &ready_sender,
                )
            })
            .map_err(|e| e.to_string())?;
        let tray_shown = ready
            .recv()
            .map_err(|_| "the UI thread ended".to_string())??;
        Ok(Ui {
            commands: command_sender,
            events,
            thread: Some(thread),
            tray_shown,
        })
    }

    /// No windows at all, for the agent at the sign-in screen: it runs where nothing can be shown, and the service's
    /// desktop helper shows the session banner on the screen. Sessions start, nobody can be asked (asking fails, so
    /// a session that needs it is refused), there is no privacy screen.
    pub fn headless() -> Result<Ui, String> {
        let (command_sender, commands) = mpsc::channel::<Command>();
        let (_no_events, events) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("queqiao-ui".into())
            .spawn(move || {
                while let Ok(command) = commands.recv() {
                    match command {
                        Command::SessionStarted(_, reply) => {
                            let _ = reply.send(Ok(()));
                        }
                        Command::Ask(_, _, reply) => {
                            let _ = reply.send(Err("nobody is signed in to ask".into()));
                        }
                        Command::Privacy(_, reply) => {
                            let _ = reply.send(Err("not at the sign-in screen".into()));
                        }
                        Command::PrivacyExcluded(_, reply) => {
                            let _ = reply.send(Ok(()));
                        }
                        _ => {}
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(Ui {
            commands: command_sender,
            events,
            thread: Some(thread),
            tray_shown: false,
        })
    }

    /// Shows the help window (help ID and code), or closes it (None).
    pub fn show_help(&self, shown: Option<(&str, &str)>) {
        self.send(Command::Help(
            shown.map(|(id, code)| (id.to_string(), code.to_string())),
        ));
    }

    /// Darkens the monitors (Some) or shows them again (None); an error if the screen cannot be hidden from
    /// capture here. Ok(true) when the session banner (with its End button) is above the dark screen.
    pub fn set_privacy(&self, look: Option<PrivacyLook>) -> Result<bool, String> {
        let (reply, answer) = mpsc::channel();
        self.send(Command::Privacy(look, reply));
        answer
            .recv_timeout(Duration::from_secs(5))
            .map_err(|_| "the UI thread did not answer".to_string())?
    }

    #[cfg_attr(target_os = "linux", allow(dead_code))]
    pub fn set_privacy_excluded(&self, excluded: bool) -> Result<(), String> {
        let (reply, answer) = mpsc::channel();
        self.send(Command::PrivacyExcluded(excluded, reply));
        answer
            .recv_timeout(Duration::from_secs(5))
            .map_err(|_| "the UI thread did not answer".to_string())?
    }

    /// Shows the banner for a session with `viewer`; an error if it cannot be shown.
    pub fn session_started(&self, viewer: &str) -> Result<(), String> {
        let (reply, answer) = mpsc::channel();
        // The name as the banner shows it: no control or direction characters, at most 40 characters.
        self.send(Command::SessionStarted(
            crate::session::clean_name(viewer),
            reply,
        ));
        answer
            .recv_timeout(Duration::from_secs(5))
            .map_err(|_| "the UI thread did not answer".to_string())?
    }

    pub fn session_ended(&self) {
        self.send(Command::SessionEnded);
    }

    /// Asks the person whether `viewer` may control the PC; the answer comes as
    /// `PresenceEvent::Consent` (denied after `seconds`).
    pub fn ask(&self, question: &str, seconds: u32) -> Result<(), String> {
        let (reply, answer) = mpsc::channel();
        self.send(Command::Ask(question.to_string(), seconds, reply));
        answer
            .recv_timeout(Duration::from_secs(5))
            .map_err(|_| "the UI thread did not answer".to_string())?
    }

    pub fn press_consent(&self, allow: bool) {
        self.send(Command::PressConsent(allow));
    }

    pub fn press_end(&self) {
        self.send(Command::PressEnd);
    }

    pub fn press_menu(&self, item: u16) {
        self.send(Command::PressMenu(item));
    }

    pub fn lock_settings(&self, locked: bool) {
        self.send(Command::LockSettings(locked));
    }

    pub fn set_password_hash(&self, hash: Option<String>) {
        self.send(Command::PasswordHash(hash));
    }

    /// How this PC stands with the account server (the tray menu shows it; "Log in..." while not online).
    pub fn set_account_state(&self, state: AccountState) {
        self.send(Command::Account(state));
    }

    pub fn set_fixed_code(&self, set: bool) {
        self.send(Command::FixedCode(set));
    }

    /// Opens the main window (on `page` if given).
    pub fn show_main(&self, page: Option<Page>) {
        self.send(Command::ShowMain(page));
    }

    pub fn set_devices(&self, devices: Vec<MainDevice>) {
        self.send(Command::Devices(devices));
    }

    pub fn set_user(&self, user: &str) {
        self.send(Command::User(user.to_string()));
    }

    pub fn set_transfers(&self, rows: Vec<TransferRow>) {
        self.send(Command::Transfers(rows));
    }

    /// The sessions this PC started (each device's page in the main window lists its own).
    pub fn set_connections(&self, rows: Vec<ConnectionRow>) {
        self.send(Command::Connections(rows));
    }

    /// A notification from the tray icon (a toast).
    pub fn notify(&self, text: &str) {
        self.send(Command::Notify(text.to_string()));
    }

    /// Tests: pictures of the main window's pages (main_devices.bmp ...) into `folder`.
    /// Tests: the Settings page's clicks and language, a line each (see Presence::test_settings).
    pub fn test_settings(&self) -> Vec<String> {
        let (reply, answer) = mpsc::channel();
        self.send(Command::TestSettings(reply));
        answer
            .recv_timeout(Duration::from_secs(20))
            .unwrap_or_else(|_| vec!["settings test: the UI did not answer".to_string()])
    }

    pub fn save_main_pictures(&self, folder: &Path) -> Result<(), String> {
        let (reply, answer) = mpsc::channel();
        self.send(Command::MainPictures(folder.to_path_buf(), reply));
        answer
            .recv_timeout(Duration::from_secs(20))
            .map_err(|_| "the UI did not answer".to_string())?
    }

    pub fn try_event(&self) -> Option<PresenceEvent> {
        self.events.try_recv().ok()
    }

    pub fn wait_event(&self, timeout: Duration) -> Option<PresenceEvent> {
        self.events.recv_timeout(timeout).ok()
    }

    fn send(&self, command: Command) {
        let _ = self.commands.send(command);
    }
}

impl Drop for Ui {
    fn drop(&mut self) {
        // Dropping the command channel ends the UI thread, which removes the icon.
        let (sender, _) = mpsc::channel();
        drop(std::mem::replace(&mut self.commands, sender));
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn run(
    settings: &Settings,
    pictures: Option<&Path>,
    commands: &Receiver<Command>,
    events: &Sender<PresenceEvent>,
    ready: &Sender<Result<bool, String>>,
) {
    let password = Some(settings.settings_password.clone());
    let mut presence = match Presence::new(
        settings.allow_account_control,
        settings.ask_before_session,
        settings.share_clipboard,
        settings.receive_files,
        settings.allow_terminal,
        // The Run key is the truth: it may be changed outside Queqiao too.
        crate::autostart::enabled(),
        settings.lock_after_session,
    ) {
        Ok(presence) => presence,
        Err(error) => {
            let _ = ready.send(Err(format!("tray icon: {}", error.message())));
            return;
        }
    };
    presence.set_password_check(password_check(password));
    presence.set_auto_update(settings.auto_update);
    presence.set_fixed_code(!settings.fixed_help_code.is_empty());
    presence.set_help_settings(settings.help_mode(), settings.help_files);
    let _ = ready.send(Ok(presence.tray_shown()));
    // (when, file name) of a picture to take once the window has drawn itself.
    let mut picture: Option<(Instant, &str)> = None;
    let mut privacy: Option<PrivacyScreen> = None;
    let mut banner_checked = Instant::now();
    loop {
        loop {
            match commands.try_recv() {
                Ok(Command::SessionStarted(viewer, reply)) => {
                    let _ = reply.send(presence.session_started(&viewer).map_err(|e| e.message()));
                    picture = Some((Instant::now() + Duration::from_millis(400), "banner.bmp"));
                }
                // The screen always comes back when a session ends, however it ends.
                Ok(Command::SessionEnded) => {
                    privacy = None;
                    presence.session_ended();
                }
                Ok(Command::Privacy(look, reply)) => {
                    privacy = None;
                    let result = match look {
                        None => Ok(true),
                        // The test square is plain, so all of it can be looked for.
                        Some(look) => match PrivacyScreen::show(
                            if look.area.is_some() {
                                ""
                            } else {
                                queqiao_core::text::tr(PRIVACY_TEXT)
                            },
                            look.area,
                            look.color,
                        ) {
                            Ok(screen) => {
                                presence.raise_banner();
                                let banner_above = presence.banner_above(screen.hwnd());
                                privacy = Some(screen);
                                Ok(banner_above)
                            }
                            Err(error) => Err(format!(
                                "{} (hiding a window from capture needs Windows 10 version 2004 or later)",
                                error.message()
                            )),
                        },
                    };
                    let _ = reply.send(result);
                }
                Ok(Command::PrivacyExcluded(excluded, reply)) => {
                    let result = match &privacy {
                        Some(screen) => screen.set_excluded(excluded).map_err(|e| e.message()),
                        None => Err("no privacy screen".into()),
                    };
                    let _ = reply.send(result);
                }
                Ok(Command::Help(shown)) => match shown {
                    Some((help_id, code)) => {
                        let _ = presence.show_help(&help_id, &code);
                    }
                    None => presence.hide_help(),
                },
                Ok(Command::Ask(question, seconds, reply)) => {
                    let _ = reply.send(
                        presence
                            .ask_consent(&question, seconds)
                            .map_err(|e| e.message()),
                    );
                    picture = Some((Instant::now() + Duration::from_millis(400), "prompt.bmp"));
                }
                Ok(Command::PressConsent(allow)) => presence.press_consent(allow),
                Ok(Command::PressEnd) => presence.press_end(),
                Ok(Command::PressMenu(item)) => {
                    if let Some(event) = presence.choose(item) {
                        let _ = events.send(event);
                    }
                }
                Ok(Command::ShowMain(page)) => {
                    let _ = presence.show_main(page);
                }
                Ok(Command::Devices(devices)) => presence.set_devices(devices),
                Ok(Command::User(user)) => presence.set_user(&user),
                Ok(Command::Transfers(rows)) => presence.set_transfers(rows),
                Ok(Command::Connections(rows)) => presence.set_connections(rows),
                Ok(Command::Notify(text)) => presence.notify(&text),
                Ok(Command::MainPictures(folder, reply)) => {
                    let _ = reply.send(
                        presence
                            .save_main_pictures(&folder)
                            .map_err(|e| e.to_string()),
                    );
                }
                Ok(Command::TestSettings(reply)) => {
                    let _ = reply.send(presence.test_settings());
                }
                Ok(Command::LockSettings(locked)) => presence.set_settings_locked(locked),
                Ok(Command::Account(state)) => presence.set_account_state(state),
                Ok(Command::FixedCode(set)) => presence.set_fixed_code(set),
                Ok(Command::PasswordHash(hash)) => {
                    presence.set_password_check(password_check(hash))
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return,
            }
        }
        if let (Some(folder), Some((due, name))) = (pictures, picture)
            && Instant::now() >= due
        {
            let window = if name == "banner.bmp" {
                presence.banner_window()
            } else {
                presence.prompt_window()
            };
            if let Some(window) = window {
                let _ = queqiao_display::save_window_picture(window, &folder.join(name));
            }
            picture = None;
        }
        // The session banner stays in sight: shown again within a second if something hid or minimised it.
        if banner_checked.elapsed() >= Duration::from_secs(1) {
            banner_checked = Instant::now();
            presence.keep_banner_shown();
        }
        if let Some(event) = presence.next(30)
            && events.send(event).is_err()
        {
            return;
        }
    }
}

/// What the person at this PC chose (the tray menu changes it).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Settings {
    /// This account's other devices may control the PC.
    pub allow_account_control: bool,
    /// Ask the person at the PC before each session.
    pub ask_before_session: bool,
    /// Share the clipboard with the controlling device.
    #[serde(default = "yes")]
    pub share_clipboard: bool,
    /// Accept files from the controlling device.
    #[serde(default = "yes")]
    pub receive_files: bool,
    /// Let the controlling device open command lines here.
    #[serde(default = "yes")]
    pub allow_terminal: bool,
    /// Lock this PC when a session ends (whoever ended it).
    #[serde(default)]
    pub lock_after_session: bool,
    /// The settings password's stored form (PBKDF2); empty: none.
    #[serde(default)]
    pub settings_password: String,
    /// Install updates by themselves (signed by the release key; never during a session).
    #[serde(default = "yes")]
    pub auto_update: bool,
    /// A fixed help code (DPAPI-encrypted for this person, hex); empty: none. Whoever has this PC's help ID and the
    /// code may ask to help; the person here is asked every time all the same.
    #[serde(default)]
    pub fixed_help_code: String,
    /// How someone of another account may start helping (as UU's three ways): "both" (the help code, then the person
    /// at this PC allows it), "code" (the help code alone), "ask" (no code, the person allows each request).
    #[serde(default = "help_mode_both")]
    pub help_mode: String,
    /// Files, also copied on the clipboard, while someone of another account helps (as UU's "allow file transfer
    /// while being assisted"; off: they go neither way).
    #[serde(default)]
    pub help_files: bool,
}

/// The check for the settings password's stored form.
fn password_check(hash: Option<String>) -> Option<queqiao_display::PasswordCheck> {
    hash.filter(|hash| !hash.is_empty()).map(|hash| {
        Box::new(move |typed: &str| queqiao_account::check_local_password(&hash, typed))
            as queqiao_display::PasswordCheck
    })
}

fn yes() -> bool {
    true
}

fn help_mode_both() -> String {
    "both".into()
}

impl Settings {
    pub fn help_mode(&self) -> queqiao_display::HelpMode {
        match self.help_mode.as_str() {
            "code" => queqiao_display::HelpMode::CodeOnly,
            "ask" => queqiao_display::HelpMode::AskOnly,
            _ => queqiao_display::HelpMode::CodeAndAsk,
        }
    }

    pub fn set_help_mode(&mut self, mode: queqiao_display::HelpMode) {
        self.help_mode = match mode {
            queqiao_display::HelpMode::CodeOnly => "code",
            queqiao_display::HelpMode::AskOnly => "ask",
            queqiao_display::HelpMode::CodeAndAsk => "both",
        }
        .into();
    }

    /// The fixed help code, decrypted (None: none, or it cannot be read).
    pub fn fixed_help_code(&self) -> Option<String> {
        let sealed = unhex(&self.fixed_help_code)?;
        #[cfg(windows)]
        let plain = queqiao_update::release::unprotect(&sealed).ok()?;
        #[cfg(target_os = "linux")]
        let plain = queqiao_account::unprotect_secret(&sealed).ok()?;
        String::from_utf8(plain)
            .ok()
            .filter(|code| !code.is_empty())
    }

    pub fn set_fixed_help_code(&mut self, code: Option<&str>) -> Result<(), String> {
        self.fixed_help_code = match code {
            #[cfg(windows)]
            Some(code) => queqiao_update::release::protect(code.as_bytes())?
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect(),
            #[cfg(target_os = "linux")]
            Some(code) => queqiao_account::protect_secret(code.as_bytes())?
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect(),
            None => String::new(),
        };
        self.save()
    }
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    if text.is_empty() || !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(text.get(at..at + 2)?, 16).ok())
        .collect()
}

impl Default for Settings {
    fn default() -> Settings {
        // As UU Remote: your own devices may connect without a prompt (unattended access to your own PCs);
        // the banner and the tray icon always show it.
        Settings {
            allow_account_control: true,
            ask_before_session: false,
            share_clipboard: true,
            receive_files: true,
            allow_terminal: true,
            lock_after_session: false,
            settings_password: String::new(),
            auto_update: true,
            fixed_help_code: String::new(),
            help_mode: help_mode_both(),
            help_files: false,
        }
    }
}

impl Settings {
    pub fn load() -> Settings {
        queqiao_account::data_folder()
            .ok()
            .and_then(|folder| std::fs::read_to_string(folder.join("agent_settings.json")).ok())
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> Result<(), String> {
        let folder = queqiao_account::data_folder().map_err(|e| e.to_string())?;
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(folder.join("agent_settings.json"), text).map_err(|e| e.to_string())
    }
}

/// One line per session in `sessions.log`: start (Unix seconds), seconds, viewer name, viewer device id,
/// how it ended.
pub fn record_session(viewer: &str, viewer_id: &str, started: SystemTime, ended: &str) {
    let Ok(folder) = queqiao_account::data_folder() else {
        return;
    };
    let start = started
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let seconds = started.elapsed().map_or(0, |d| d.as_secs());
    let clean = |text: &str| text.replace(['\t', '\r', '\n'], " ");
    let line = format!(
        "{start}\t{seconds}\t{}\t{}\t{}\r\n",
        clean(viewer),
        clean(viewer_id),
        clean(ended)
    );
    use std::io::Write;
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(folder.join(if cfg!(feature = "test-switches") {
            // A test build keeps its own list, never the person's.
            "sessions.test.log"
        } else {
            "sessions.log"
        }))
    {
        let _ = file.write_all(line.as_bytes());
    }
}
