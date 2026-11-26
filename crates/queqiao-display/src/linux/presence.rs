//! What the person at the controlled PC sees and can do, on Linux, with the API of Windows' presence.rs (so the agent's
//! UI thread, ui.rs, is the same): the tray icon (a StatusNotifierItem: KDE, Ubuntu's GNOME with its AppIndicator
//! extension, most other desktops) with the shared menu (tray.rs); the banner during a session and the consent prompt;
//! the help window; the main window (Devices, Help, Transfers, Settings). Without a tray (no StatusNotifierWatcher)
//! the main window opens instead, so Queqiao is never running unseen.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use eframe::egui;
use queqiao_core::text::{tr, trf};

use super::banner::{ConsentPrompt, SessionBanner};
use super::gui::{self, Opened, View};
use super::main_view::MainView;
use super::window::Hwnd;
use super::{Error, Result, dialogs};
use crate::model::{
    AccountState, ConnectionRow, HelpMode, MainDevice, Page, PasswordCheck, PresenceEvent,
    TransferRow,
};
use crate::tray::*;

/// The socket a second start of Queqiao asks the running one to show its main window through (Windows: it finds the
/// window by its class), in the private runtime folder; it takes that one request only.
fn show_socket() -> Option<std::path::PathBuf> {
    Some(queqiao_core::platform::runtime_folder()?.join("agent.sock"))
}

const SHOW_REQUEST: &[u8; 5] = b"show\n";

/// Answers "show" from a second start with the main window. Another Queqiao that listens already (a test's second
/// agent) keeps the socket.
fn listen_for_show(sender: Sender<Action>) {
    use std::io::Read;
    use std::os::unix::net::{UnixListener, UnixStream};
    let Some(path) = show_socket() else {
        return;
    };
    if UnixStream::connect(&path).is_ok() {
        return;
    }
    let _ = std::fs::remove_file(&path);
    let Ok(listener) = UnixListener::bind(&path) else {
        return;
    };
    let _ = std::thread::Builder::new()
        .name("queqiao-show".into())
        .spawn(move || {
            for mut stream in listener.incoming().flatten() {
                let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                let mut request = [0u8; 5];
                if stream.read_exact(&mut request).is_ok()
                    && &request == SHOW_REQUEST
                    && sender.send(Action::OpenMain).is_err()
                {
                    break;
                }
            }
        });
}

/// Opened again: asks the Queqiao that runs for this user to show its main window; false when none answers.
pub fn show_running_main() -> bool {
    use std::io::Write;
    show_socket()
        .and_then(|path| std::os::unix::net::UnixStream::connect(path).ok())
        .is_some_and(|mut stream| stream.write_all(SHOW_REQUEST).is_ok())
}

/// What the windows and the tray asked for, handled in `next`.
pub(super) enum Action {
    Choose(u16),
    OpenMain,
    Connect(String),
    /// The "being controlled" banner's Disconnect.
    EndControl(String),
    Files(String),
    Terminal(String),
    Wake(String),
    Remove(String),
    HelpSomeone {
        help_id: String,
        code: String,
    },
    CopyInvitation,
    NewHelpId,
    HelpNewCode,
    HelpClose,
    OpenReceived,
    ClearTransfers,
    Account,
}

/// What the main window shows (kept also while it is closed).
#[derive(Clone)]
pub(super) struct Shown {
    pub(super) page: Page,
    pub(super) user: String,
    pub(super) account: AccountState,
    pub(super) devices: Vec<MainDevice>,
    pub(super) transfers: Vec<TransferRow>,
    pub(super) connections: Vec<ConnectionRow>,
    pub(super) help: Option<(String, String)>,
    /// The Settings page: what it shows (settings.rs), its tab; locked while someone of another account controls.
    pub(super) settings: crate::settings::Shown,
    pub(super) tab: crate::settings::Tab,
    pub(super) locked: bool,
}

/// The dialogs the tray model asks through (tray.rs).
struct LinuxDialogs;

impl Dialogs for LinuxDialogs {
    fn ask_secret(&self, title: &str, prompt: &str) -> Option<String> {
        dialogs::ask_secret(title, prompt)
    }

    fn ask_text(&self, title: &str, prompt: &str, initial: &str) -> Option<String> {
        dialogs::ask_text(title, prompt, initial)
    }

    fn show_error(&self, title: &str, text: &str) {
        dialogs::show_error(title, text);
    }
}

pub struct Presence {
    menu: Tray,
    tray: Option<ksni::blocking::Handle<SniTray>>,
    actions: Receiver<Action>,
    sender: Sender<Action>,
    banner: Option<SessionBanner>,
    /// End was pressed and reported for this session.
    end_reported: bool,
    prompt: Option<ConsentPrompt>,
    help: Option<Opened>,
    main: Option<Opened>,
    /// The main window is on screen (false once the person closed it).
    main_open: Arc<AtomicBool>,
    shown: Arc<Mutex<Shown>>,
    /// The help invitation, for "Copy invitation" (held on the clipboard while this lives).
    invitation: String,
    clipboard: Option<queqiao_input::Clipboard>,
}

impl Presence {
    /// The tray icon, with the current settings.
    pub fn new(
        allow: bool,
        ask: bool,
        clipboard: bool,
        files: bool,
        terminal: bool,
        autostart: bool,
        lock_after: bool,
    ) -> Result<Presence> {
        let (sender, actions) = mpsc::channel();
        let menu = Tray::new(
            allow, ask, clipboard, files, terminal, autostart, lock_after,
        );
        let tray = {
            use ksni::blocking::TrayMethods;
            SniTray {
                entries: menu.entries(),
                status: menu.status(),
                sender: Mutex::new(sender.clone()),
            }
            .spawn()
            .ok()
        };
        let mut presence = Presence {
            menu,
            tray,
            actions,
            sender,
            banner: None,
            end_reported: false,
            prompt: None,
            help: None,
            main: None,
            main_open: Arc::new(AtomicBool::new(false)),
            shown: Arc::new(Mutex::new(Shown {
                page: Page::Devices,
                user: String::new(),
                account: AccountState::Online,
                devices: Vec::new(),
                transfers: Vec::new(),
                connections: Vec::new(),
                help: None,
                settings: crate::settings::Shown::default(),
                tab: crate::settings::Tab::default(),
                locked: false,
            })),
            invitation: String::new(),
            clipboard: None,
        };
        presence.refresh();
        listen_for_show(presence.sender.clone());
        if presence.tray.is_none() {
            // No tray on this desktop: the main window shows that Queqiao runs.
            presence.show_main(None)?;
        }
        Ok(presence)
    }

    /// True once the icon is in the tray (false without a StatusNotifierWatcher).
    pub fn tray_shown(&self) -> bool {
        self.tray.is_some()
    }

    /// The tray menu and the main window's settings, after a change.
    fn refresh(&mut self) {
        if let Some(tray) = &self.tray {
            let (entries, status) = (self.menu.entries(), self.menu.status());
            let _ = tray.update(move |tray| {
                tray.entries = entries;
                tray.status = status;
            });
        }
        if let Ok(mut shown) = self.shown.lock() {
            let m = &self.menu;
            shown.account = m.account;
            shown.settings = crate::settings::Shown {
                help_mode: m.help_mode,
                help_files: m.help_files,
                allow: m.allow,
                ask: m.ask,
                clipboard: m.clipboard,
                files: m.files,
                terminal: m.terminal,
                autostart: m.autostart,
                lock_after: m.lock_after,
                auto_update: m.auto_update,
                password: m.password_check.is_some(),
                fixed_code: m.fixed_code,
                language: queqiao_core::text::saved_choice(),
                keys: queqiao_core::hotkeys::Keys::load(),
                version: queqiao_core::VERSION.to_string(),
            };
            shown.locked = m.settings_locked;
        }
        gui::repaint();
    }

    /// The banner for a session with `viewer`.
    pub fn session_started(&mut self, viewer: &str) -> Result<()> {
        self.menu.viewer = Some(viewer.to_string());
        self.banner = Some(SessionBanner::show(viewer)?);
        self.end_reported = false;
        self.refresh();
        Ok(())
    }

    pub fn session_ended(&mut self) {
        self.menu.viewer = None;
        self.banner = None;
        self.refresh();
    }

    /// Asks the person `question` (who wants to control the PC); denied after `seconds`.
    pub fn ask_consent(&mut self, question: &str, seconds: u32) -> Result<()> {
        self.prompt = Some(ConsentPrompt::ask(question, seconds)?);
        Ok(())
    }

    /// Presses Allow or Deny on the prompt (for tests: the same path as a click).
    pub fn press_consent(&self, allow: bool) {
        if let Some(prompt) = &self.prompt {
            prompt.press(allow);
        }
    }

    /// Presses End on the banner (for tests).
    pub fn press_end(&self) {
        if let Some(banner) = &self.banner {
            banner.press_end();
        }
    }

    /// The banner keeps itself above other windows (the GUI thread raises it).
    pub fn raise_banner(&self) {}

    /// The banner stays in sight (it raises itself every 2 s); true while one is shown.
    pub fn keep_banner_shown(&self) -> bool {
        self.banner.is_some()
    }

    /// Whether the banner is above `other` (the privacy screen, which Linux does not have yet).
    pub fn banner_above(&self, _other: Hwnd) -> bool {
        self.banner.is_some()
    }

    /// The banner's and the prompt's windows, for the tests' pictures: not known for egui's windows here.
    pub fn banner_window(&self) -> Option<Hwnd> {
        self.banner.as_ref().map(SessionBanner::hwnd)
    }

    pub fn prompt_window(&self) -> Option<Hwnd> {
        self.prompt.as_ref().map(ConsentPrompt::hwnd)
    }

    /// The help window with this PC's help ID and code.
    pub fn show_help(&mut self, help_id: &str, code: &str) -> Result<()> {
        self.invitation = queqiao_core::invitation::text(help_id, code);
        if let Ok(mut shown) = self.shown.lock() {
            shown.help = Some((help_id.to_string(), code.to_string()));
        }
        let view = HelpView {
            help_id: queqiao_core::invitation::spaced_id(help_id),
            code: queqiao_core::invitation::halved_code(code),
            sender: self.sender.clone(),
        };
        self.help = Some(gui::open(Box::new(view))?);
        gui::repaint();
        Ok(())
    }

    pub fn hide_help(&mut self) {
        self.help = None;
        if let Ok(mut shown) = self.shown.lock() {
            shown.help = None;
        }
        gui::repaint();
    }

    /// The main window, on `page` if given.
    pub fn show_main(&mut self, page: Option<Page>) -> Result<()> {
        if let (Some(page), Ok(mut shown)) = (page, self.shown.lock()) {
            shown.page = page;
        }
        if !self.main_open.load(Ordering::Relaxed) {
            self.main = None;
            self.main_open.store(true, Ordering::Relaxed);
            let view = MainView::new(
                self.shown.clone(),
                self.sender.clone(),
                self.main_open.clone(),
            );
            self.main = Some(gui::open(Box::new(view))?);
        }
        gui::repaint();
        Ok(())
    }

    /// Tests, as on Windows: the Settings page's cards; a click on "Ask before each session"'s switch (as the window
    /// sends it) turns it over through the usual path (the agent reports the change); choosing English draws the
    /// windows in it at once. Run with a settings folder of the test's own (XDG_CONFIG_HOME).
    pub fn test_settings(&mut self) -> Vec<String> {
        let mut lines = Vec::new();
        if let Err(error) = self.show_main(Some(Page::Settings)) {
            return vec![format!(
                "settings test: no main window: {}",
                error.message()
            )];
        }
        let shown = self
            .shown
            .lock()
            .map(|shown| shown.settings.clone())
            .unwrap_or_default();
        let cards: usize = crate::settings::Tab::ALL
            .iter()
            .map(|tab| crate::settings::rows(*tab, &shown).len())
            .sum();
        lines.push(format!(
            "settings test: {cards} cards on {} tabs",
            crate::settings::Tab::ALL.len()
        ));
        // Twice (on, then back): the settings stay as they were (later tests sharing them were asked first,
        // 2026-11-26).
        let clicked = self.sender.send(Action::Choose(MENU_ASK)).is_ok()
            && self.sender.send(Action::Choose(MENU_ASK)).is_ok();
        lines.push(format!(
            "settings test: clicked the switch of asking before each session twice: {clicked}"
        ));
        let before = queqiao_core::text::language();
        let _ = self.choose(crate::tray::MENU_LANGUAGE_EN);
        let after = queqiao_core::text::language();
        gui::repaint();
        lines.push(format!(
            "settings test: display language {before:?} -> {after:?} after choosing English, the main window's title: {}",
            tr("Queqiao")
        ));
        let _ = self.choose(crate::tray::MENU_LANGUAGE_SYSTEM);
        lines
    }

    /// Tests: pictures of the main window's pages (main_devices.bmp, main_help.bmp ...) into `folder`, as on Windows.
    pub fn save_main_pictures(&mut self, folder: &Path) -> std::io::Result<()> {
        self.show_main(None)
            .map_err(|e| std::io::Error::other(e.message()))?;
        let back = self.shown.lock().map_or(Page::Devices, |shown| shown.page);
        for page in Page::ALL {
            if let Ok(mut shown) = self.shown.lock() {
                shown.page = page;
            }
            gui::repaint();
            // A frame or two with the page (the GUI thread draws it).
            std::thread::sleep(Duration::from_millis(300));
            let name = format!("main_{}.bmp", format!("{page:?}").to_lowercase());
            self.main
                .as_ref()
                .ok_or_else(|| std::io::Error::other("the main window closed"))?
                .save_picture(&folder.join(name))?;
        }
        // Each further tab of Settings: main_settings_security.bmp ... (main_settings.bmp is its first).
        for tab in &crate::settings::Tab::ALL[1..] {
            if let Ok(mut shown) = self.shown.lock() {
                shown.page = Page::Settings;
                shown.tab = *tab;
            }
            gui::repaint();
            std::thread::sleep(Duration::from_millis(300));
            let name = format!("main_settings_{}.bmp", format!("{tab:?}").to_lowercase());
            self.main
                .as_ref()
                .ok_or_else(|| std::io::Error::other("the main window closed"))?
                .save_picture(&folder.join(name))?;
        }
        if let Ok(mut shown) = self.shown.lock() {
            shown.page = back;
            shown.tab = crate::settings::Tab::General;
        }
        gui::repaint();
        Ok(())
    }

    /// A notification (the desktop's, through notify-send where it is there).
    pub fn notify(&mut self, text: &str) {
        let shown = std::process::Command::new("notify-send")
            .args(["--app-name=Queqiao", "Queqiao", text])
            .stdin(std::process::Stdio::null())
            .status()
            .is_ok_and(|status| status.success());
        if !shown {
            eprintln!("Queqiao: {text}");
        }
    }

    pub fn set_user(&mut self, user: &str) {
        if let Ok(mut shown) = self.shown.lock() {
            shown.user = user.to_string();
        }
        gui::repaint();
    }

    pub fn set_devices(&mut self, devices: Vec<MainDevice>) {
        if let Ok(mut shown) = self.shown.lock() {
            shown.devices = devices;
        }
        gui::repaint();
    }

    pub fn set_transfers(&mut self, transfers: Vec<TransferRow>) {
        if let Ok(mut shown) = self.shown.lock() {
            shown.transfers = transfers;
        }
        gui::repaint();
    }

    pub fn set_connections(&mut self, connections: Vec<ConnectionRow>) {
        if let Ok(mut shown) = self.shown.lock() {
            shown.connections = connections;
        }
        gui::repaint();
    }

    pub fn set_help_settings(&mut self, mode: HelpMode, files: bool) {
        self.menu.help_mode = mode;
        self.menu.help_files = files;
        self.refresh();
    }

    pub fn set_password_check(&mut self, check: Option<PasswordCheck>) {
        self.menu.password_check = check;
        self.refresh();
    }

    pub fn set_fixed_code(&mut self, set: bool) {
        self.menu.fixed_code = set;
        self.refresh();
    }

    pub fn set_auto_update(&mut self, on: bool) {
        self.menu.auto_update = on;
        self.refresh();
    }

    pub fn set_account_state(&mut self, state: AccountState) {
        self.menu.account = state;
        self.refresh();
    }

    pub fn set_settings_locked(&mut self, locked: bool) {
        self.menu.settings_locked = locked;
        self.refresh();
    }

    /// What choosing tray menu item `item` does (also the path tests take).
    pub fn choose(&mut self, item: u16) -> Option<PresenceEvent> {
        if item == MENU_OPEN {
            return self.show_main(None).ok().map(|()| PresenceEvent::MainShown);
        }
        // The display language and the session keys: this PC's own, nothing for the agent. The main window draws in
        // the language in use each time, so a new one shows at once.
        match crate::tray::choose_local(item, &LinuxDialogs) {
            crate::tray::Local::Other => {}
            crate::tray::Local::Unchanged => return None,
            crate::tray::Local::Keys | crate::tray::Local::Language => {
                self.refresh();
                return None;
            }
        }
        let event = self.menu.choose(item, &LinuxDialogs);
        self.refresh();
        event
    }

    /// What the person did, waiting up to `wait_ms` for it.
    pub fn next(&mut self, wait_ms: u32) -> Option<PresenceEvent> {
        if let Some(prompt) = &self.prompt
            && let Some(allowed) = prompt.answer()
        {
            self.prompt = None;
            return Some(PresenceEvent::Consent(allowed));
        }
        if !self.end_reported
            && self
                .banner
                .as_ref()
                .is_some_and(|banner| banner.end_pressed(0))
        {
            self.end_reported = true;
            return Some(PresenceEvent::EndSession);
        }
        let action = self
            .actions
            .recv_timeout(Duration::from_millis(u64::from(wait_ms)))
            .ok()?;
        self.act(action)
    }

    fn act(&mut self, action: Action) -> Option<PresenceEvent> {
        match action {
            Action::Choose(item) => self.choose(item),
            Action::OpenMain => self.show_main(None).ok().map(|()| PresenceEvent::MainShown),
            // One device controls a PC at a time (as UU Remote): with another device in control, the person is asked.
            Action::Connect(id) => {
                let controller = self
                    .shown
                    .lock()
                    .ok()
                    .and_then(|shown| {
                        shown
                            .devices
                            .iter()
                            .find(|d| d.id == id)
                            .map(|d| d.controlled_by.clone())
                    })
                    .unwrap_or_default();
                if controller.is_empty() {
                    Some(PresenceEvent::Connect(id))
                } else {
                    dialogs::confirm_take_over(&controller).then_some(PresenceEvent::TakeOver(id))
                }
            }
            Action::EndControl(id) => Some(PresenceEvent::EndControl(id)),
            Action::Files(id) => Some(PresenceEvent::SendFiles(id)),
            Action::Terminal(id) => Some(PresenceEvent::OpenCommandLine(id)),
            Action::Wake(id) => Some(PresenceEvent::Wake(id)),
            Action::Remove(id) => {
                let name = self
                    .shown
                    .lock()
                    .ok()
                    .and_then(|shown| shown.devices.iter().find(|d| d.id == id).map(|d| d.name.clone()))
                    .unwrap_or_else(|| id.clone());
                let question = trf(
                    "Take {name} out of your account? It can no longer control your devices or be controlled until it logs in again.",
                    &[("name", &name)],
                );
                dialogs::confirm(tr("Queqiao"), &question).then_some(PresenceEvent::RemoveDevice(id))
            }
            Action::HelpSomeone { help_id, code } => {
                Some(PresenceEvent::HelpSomeone { help_id, code })
            }
            Action::CopyInvitation => {
                if self.clipboard.is_none() {
                    self.clipboard = queqiao_input::Clipboard::start().ok();
                }
                if let Some(clipboard) = &self.clipboard {
                    clipboard.set_text(self.invitation.clone());
                    dialogs::show_info(
                        tr("Queqiao"),
                        tr("Copied: send it to the person who helps you."),
                    );
                }
                None
            }
            Action::NewHelpId => Some(PresenceEvent::NewHelpId),
            Action::HelpNewCode => Some(PresenceEvent::HelpNewCode),
            Action::HelpClose => {
                self.hide_help();
                Some(PresenceEvent::HelpClose)
            }
            Action::OpenReceived => Some(PresenceEvent::OpenReceivedFiles),
            Action::ClearTransfers => Some(PresenceEvent::ClearTransfers),
            Action::Account => match self.menu.account {
                AccountState::LoggedOut => self.choose(MENU_LOGIN),
                _ => dialogs::confirm(
                    tr("Queqiao"),
                    tr("Log this PC out of your account? Your devices can no longer control it until it logs in again."),
                )
                .then_some(PresenceEvent::LogOut),
            },
        }
    }
}

/// The tray icon: its menu is the shared model's entries; choosing sends the id to `next`.
struct SniTray {
    entries: Vec<Entry>,
    status: String,
    sender: Mutex<Sender<Action>>,
}

impl SniTray {
    fn send(&self, action: Action) {
        if let Ok(sender) = self.sender.lock() {
            let _ = sender.send(action);
        }
    }
}

impl ksni::Tray for SniTray {
    fn id(&self) -> String {
        "queqiao".into()
    }

    fn title(&self) -> String {
        tr("Queqiao").into()
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        [22, 32, 48]
            .into_iter()
            .filter_map(super::icon::icon)
            .map(|(width, height, rgba)| ksni::Icon {
                width: width as i32,
                height: height as i32,
                // ARGB in network byte order.
                data: rgba
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .flat_map(|p| [p[3], p[0], p[1], p[2]])
                    .collect(),
            })
            .collect()
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            title: self.status.clone(),
            ..Default::default()
        }
    }

    /// A click on the icon: the main window.
    fn activate(&mut self, _x: i32, _y: i32) {
        self.send(Action::OpenMain);
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        self.entries
            .iter()
            .map(|entry| match entry {
                Entry::Separator => ksni::MenuItem::Separator,
                Entry::Status(text) => ksni::menu::StandardItem {
                    label: text.clone(),
                    enabled: false,
                    ..Default::default()
                }
                .into(),
                Entry::Item {
                    id,
                    text,
                    checked,
                    enabled,
                    ..
                } => {
                    let id = *id;
                    let checkable = matches!(
                        id,
                        MENU_ALLOW
                            | MENU_ASK
                            | MENU_CLIPBOARD
                            | MENU_FILES
                            | MENU_TERMINAL
                            | MENU_AUTOSTART
                            | MENU_LOCK_AFTER
                            | MENU_AUTO_UPDATE
                    );
                    if checkable {
                        ksni::menu::CheckmarkItem {
                            label: text.clone(),
                            enabled: *enabled,
                            checked: *checked,
                            activate: Box::new(move |tray: &mut SniTray| {
                                tray.send(Action::Choose(id))
                            }),
                            ..Default::default()
                        }
                        .into()
                    } else {
                        ksni::menu::StandardItem {
                            label: text.clone(),
                            enabled: *enabled,
                            activate: Box::new(move |tray: &mut SniTray| {
                                tray.send(Action::Choose(id))
                            }),
                            ..Default::default()
                        }
                        .into()
                    }
                }
            })
            .collect()
    }
}

/// The help window: this PC's help ID and code, New code, Copy invitation, Close.
struct HelpView {
    help_id: String,
    code: String,
    sender: Sender<Action>,
}

impl View for HelpView {
    fn builder(&self) -> egui::ViewportBuilder {
        egui::ViewportBuilder::default()
            .with_title(tr("Queqiao - let someone help you"))
            .with_inner_size([420.0, 230.0])
            .with_resizable(false)
    }

    fn draw(&mut self, ctx: &egui::Context) -> bool {
        let mut keep = true;
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.label(tr(
                "Give these to the person who helps you. They can ask once to control this PC; you decide in a prompt.",
            ));
            ui.add_space(10.0);
            let big = |text: String| egui::RichText::new(text).size(20.0).strong();
            ui.label(big(format!("{}   {}", tr("Help ID:"), self.help_id)));
            ui.label(big(format!("{}   {}", tr("Code:"), self.code)));
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                if ui.button(tr("Copy invitation")).clicked() {
                    let _ = self.sender.send(Action::CopyInvitation);
                }
                if ui.button(tr("New code")).clicked() {
                    let _ = self.sender.send(Action::HelpNewCode);
                }
                if ui.button(tr("Close")).clicked() {
                    let _ = self.sender.send(Action::HelpClose);
                    keep = false;
                }
            });
        });
        keep
    }

    fn closed(&mut self) {
        let _ = self.sender.send(Action::HelpClose);
    }
}

/// The privacy screen on Linux: not yet (an X11 window over the screen would be captured too; docs/LINUX.md).
pub struct PrivacyScreen {
    never: std::convert::Infallible,
}

impl PrivacyScreen {
    pub fn show(
        _text: &str,
        _area: Option<super::window::Rect>,
        _color: u32,
    ) -> Result<PrivacyScreen> {
        Err(Error::from(
            "the privacy screen: not on Linux yet (docs/LINUX.md step 5)",
        ))
    }

    pub fn hwnd(&self) -> Hwnd {
        match self.never {}
    }

    pub fn set_excluded(&self, _excluded: bool) -> Result<()> {
        match self.never {}
    }
}
