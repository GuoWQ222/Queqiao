//! What the person at this PC sees, on Linux (docs/LINUX.md), with the API of Windows' ui.rs: nothing happens unseen.
//! Under X11 for now: during every session a banner over the top of the screen names who controls the PC, and a click
//! on it ends the session; a session whose banner cannot be shown is refused, as on Windows. The tray icon
//! (StatusNotifierItem), the consent prompt and the privacy screen come with the GUI (step 5): asked for, they say so.
//!
//! Also the agent's settings, `agent_settings.json` in the data folder (~/.local/share/queqiao or as
//! queqiao_account::data_folder says), with Windows' fields and defaults.

use std::cell::RefCell;
use std::path::PathBuf;

use queqiao_display::PresenceEvent;
use serde::{Deserialize, Serialize};
use x11rb::connection::Connection;
use x11rb::protocol::Event;
use x11rb::protocol::xproto::{
    Char2b, ConfigureWindowAux, ConnectionExt as _, CreateGCAux, CreateWindowAux, EventMask,
    StackMode, Window, WindowClass,
};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

/// Where a privacy test square goes: left, top, right, bottom (Windows: a RECT).
pub type Area = (i32, i32, i32, i32);

/// The privacy screen over all monitors (`area` None), or for tests a square of `color` (0x00BBGGRR).
pub struct PrivacyLook {
    pub area: Option<Area>,
    pub color: u32,
}

const BANNER_HEIGHT: u16 = 28;
/// Dark red, as a warning; white text.
const BANNER_BACKGROUND: u32 = 0x00B0_1020;
const BANNER_TEXT: u32 = 0x00FF_FFFF;

struct Banner {
    connection: RustConnection,
    window: Window,
    gc: u32,
    font: Option<u32>,
    text: Vec<Char2b>,
}

pub struct Ui {
    banner: RefCell<Option<Banner>>,
    events: RefCell<Vec<PresenceEvent>>,
    /// No tray icon on Linux yet.
    pub tray_shown: bool,
}

impl Ui {
    /// With `pictures` (tests), Windows saves the banner's picture there; not on Linux.
    pub fn start(_settings: &Settings, _pictures: Option<PathBuf>) -> Result<Ui, String> {
        Ok(Ui {
            banner: RefCell::new(None),
            events: RefCell::new(Vec::new()),
            tray_shown: false,
        })
    }

    /// Darkens the monitors (Some) or shows them again (None): not on Linux yet, so it is refused (the viewer is told).
    pub fn set_privacy(&self, look: Option<PrivacyLook>) -> Result<bool, String> {
        match look {
            None => Ok(false),
            Some(PrivacyLook { area, color }) => Err(format!(
                "the privacy screen ({}): not on Linux yet (docs/LINUX.md step 5)",
                match area {
                    Some(_) => format!("a test square of #{color:06X}"),
                    None => "the whole screen".into(),
                }
            )),
        }
    }

    /// Shows the banner for a session with `viewer`; an error (and so no session) if it cannot be shown.
    pub fn session_started(&self, viewer: &str) -> Result<(), String> {
        let banner = Banner::show(&crate::session::clean_name(viewer))
            .map_err(|error| format!("no banner on this display: {error}"))?;
        *self.banner.borrow_mut() = Some(banner);
        Ok(())
    }

    pub fn session_ended(&self) {
        if let Some(banner) = self.banner.borrow_mut().take() {
            let _ = banner.connection.destroy_window(banner.window);
            let _ = banner.connection.flush();
        }
    }

    /// For tests: as a click on the banner.
    pub fn press_end(&self) {
        self.events.borrow_mut().push(PresenceEvent::EndSession);
    }

    /// What the person did since the last call: a click on the banner ends the session.
    pub fn try_event(&self) -> Option<PresenceEvent> {
        if let Some(banner) = self.banner.borrow().as_ref() {
            let mut redraw = false;
            while let Ok(Some(event)) = banner.connection.poll_for_event() {
                match event {
                    Event::ButtonPress(_) => {
                        self.events.borrow_mut().push(PresenceEvent::EndSession)
                    }
                    Event::Expose(_) => redraw = true,
                    _ => {}
                }
            }
            // Kept above windows opened since.
            let above = ConfigureWindowAux::new().stack_mode(StackMode::ABOVE);
            let _ = banner.connection.configure_window(banner.window, &above);
            if redraw {
                banner.draw();
            }
            let _ = banner.connection.flush();
        }
        let mut events = self.events.borrow_mut();
        (!events.is_empty()).then(|| events.remove(0))
    }
}

impl Banner {
    fn show(viewer: &str) -> Result<Banner, Box<dyn std::error::Error>> {
        let (connection, screen_number) = x11rb::connect(None)?;
        let screen = connection.setup().roots[screen_number].clone();
        let window = connection.generate_id()?;
        connection.create_window(
            screen.root_depth,
            window,
            screen.root,
            0,
            0,
            screen.width_in_pixels,
            BANNER_HEIGHT,
            0,
            WindowClass::INPUT_OUTPUT,
            screen.root_visual,
            &CreateWindowAux::new()
                .override_redirect(1)
                .background_pixel(BANNER_BACKGROUND)
                .event_mask(EventMask::EXPOSURE | EventMask::BUTTON_PRESS),
        )?;
        // The fixed font in its Unicode encoding, else any "fixed"; without fonts the bar alone.
        let font = [
            "-misc-fixed-bold-r-normal--13-120-75-75-c-70-iso10646-1",
            "fixed",
        ]
        .into_iter()
        .find_map(|name| {
            let id = connection.generate_id().ok()?;
            connection
                .open_font(id, name.as_bytes())
                .ok()?
                .check()
                .ok()
                .map(|()| id)
        });
        let gc = connection.generate_id()?;
        let mut values = CreateGCAux::new()
            .foreground(BANNER_TEXT)
            .background(BANNER_BACKGROUND);
        if let Some(font) = font {
            values = values.font(font);
        }
        connection.create_gc(gc, window, &values)?;
        connection.map_window(window)?;
        let message =
            format!("Queqiao: {viewer} is controlling this PC. Click this bar to end the session.");
        // UCS-2 for ImageText16: characters beyond the first plane show as '?'.
        let text = message
            .chars()
            .map(|c| u16::try_from(u32::from(c)).unwrap_or(u16::from(b'?')))
            .map(|unit| Char2b {
                byte1: (unit >> 8) as u8,
                byte2: unit as u8,
            })
            .take(250)
            .collect();
        let banner = Banner {
            connection,
            window,
            gc,
            font,
            text,
        };
        banner.draw();
        banner.connection.sync()?;
        Ok(banner)
    }

    fn draw(&self) {
        if self.font.is_some() {
            let _ = self
                .connection
                .image_text16(self.window, self.gc, 10, 19, &self.text);
        }
    }
}

/// What the person at this PC chose (Windows: the tray menu changes it; Linux reads the same file).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Settings {
    pub allow_account_control: bool,
    pub ask_before_session: bool,
    #[serde(default = "yes")]
    pub share_clipboard: bool,
    #[serde(default = "yes")]
    pub receive_files: bool,
    #[serde(default = "yes")]
    pub allow_terminal: bool,
    #[serde(default)]
    pub lock_after_session: bool,
}

fn yes() -> bool {
    true
}

impl Default for Settings {
    /// As on Windows (and UU Remote): your own devices may connect without a prompt; the banner always shows it.
    fn default() -> Settings {
        Settings {
            allow_account_control: true,
            ask_before_session: false,
            share_clipboard: true,
            receive_files: true,
            allow_terminal: true,
            lock_after_session: false,
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
}
