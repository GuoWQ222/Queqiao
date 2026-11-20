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

use queqiao_display::{Presence, PresenceEvent};
use serde::{Deserialize, Serialize};

enum Command {
    SessionStarted(String, Sender<Result<(), String>>),
    SessionEnded,
    Ask(String, u32, Sender<Result<(), String>>),
    /// For tests: the same path as a click.
    PressConsent(bool),
    PressEnd,
}

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

    /// Shows the banner for a session with `viewer`; an error if it cannot be shown.
    pub fn session_started(&self, viewer: &str) -> Result<(), String> {
        let (reply, answer) = mpsc::channel();
        self.send(Command::SessionStarted(viewer.to_string(), reply));
        answer
            .recv_timeout(Duration::from_secs(5))
            .map_err(|_| "the UI thread did not answer".to_string())?
    }

    pub fn session_ended(&self) {
        self.send(Command::SessionEnded);
    }

    /// Asks the person whether `viewer` may control the PC; the answer comes as
    /// `PresenceEvent::Consent` (denied after `seconds`).
    pub fn ask(&self, viewer: &str, seconds: u32) -> Result<(), String> {
        let (reply, answer) = mpsc::channel();
        self.send(Command::Ask(viewer.to_string(), seconds, reply));
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
    let mut presence = match Presence::new(
        settings.allow_account_control,
        settings.ask_before_session,
        settings.share_clipboard,
        settings.receive_files,
    ) {
        Ok(presence) => presence,
        Err(error) => {
            let _ = ready.send(Err(format!("tray icon: {}", error.message())));
            return;
        }
    };
    let _ = ready.send(Ok(presence.tray_shown()));
    // (when, file name) of a picture to take once the window has drawn itself.
    let mut picture: Option<(Instant, &str)> = None;
    loop {
        loop {
            match commands.try_recv() {
                Ok(Command::SessionStarted(viewer, reply)) => {
                    let _ = reply.send(presence.session_started(&viewer).map_err(|e| e.message()));
                    picture = Some((Instant::now() + Duration::from_millis(400), "banner.bmp"));
                }
                Ok(Command::SessionEnded) => presence.session_ended(),
                Ok(Command::Ask(viewer, seconds, reply)) => {
                    let _ = reply.send(
                        presence
                            .ask_consent(&viewer, seconds)
                            .map_err(|e| e.message()),
                    );
                    picture = Some((Instant::now() + Duration::from_millis(400), "prompt.bmp"));
                }
                Ok(Command::PressConsent(allow)) => presence.press_consent(allow),
                Ok(Command::PressEnd) => presence.press_end(),
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
}

fn yes() -> bool {
    true
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
        .open(folder.join("sessions.log"))
    {
        let _ = file.write_all(line.as_bytes());
    }
}
