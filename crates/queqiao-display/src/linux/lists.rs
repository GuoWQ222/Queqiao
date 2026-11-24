//! The viewer's device list and the "help someone" form on Linux (Windows: forms.rs's DeviceList and HelpForm), with
//! their API: polled with `next`, as on Windows.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui;
use queqiao_core::text::tr;

use super::Result;
use super::gui::{self, Opened, View};
use super::window::Hwnd;
use crate::model::{DeviceRow, HelpAction, ListAction, normalize_code};

/// Waits up to `wait_ms` for an action in `actions`.
fn wait_for<T>(state: &Mutex<Vec<T>>, wait_ms: u32) -> Option<T> {
    let until = Instant::now() + Duration::from_millis(u64::from(wait_ms));
    loop {
        if let Ok(mut actions) = state.lock()
            && !actions.is_empty()
        {
            return Some(actions.remove(0));
        }
        if Instant::now() >= until {
            return None;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[derive(Default)]
struct ListState {
    rows: Vec<DeviceRow>,
    selected: Option<usize>,
    status: String,
    enabled: bool,
}

struct ListView {
    title: String,
    heading: String,
    state: Arc<Mutex<ListState>>,
    actions: Arc<Mutex<Vec<ListAction>>>,
}

impl ListView {
    fn push(&self, action: ListAction) {
        if let Ok(mut actions) = self.actions.lock() {
            actions.push(action);
        }
    }
}

impl View for ListView {
    fn builder(&self) -> egui::ViewportBuilder {
        egui::ViewportBuilder::default()
            .with_title(&self.title)
            .with_inner_size([540.0, 400.0])
    }

    fn draw(&mut self, ctx: &egui::Context) -> bool {
        let Ok(mut state) = self.state.lock() else {
            return false;
        };
        let mut pressed: Option<u8> = None;
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading(&self.heading);
            ui.label(tr("Double-click a device to connect to it."));
            ui.add_space(6.0);
            ui.add_enabled_ui(state.enabled, |ui| {
                egui::ScrollArea::vertical()
                    .max_height(250.0)
                    .show(ui, |ui| {
                        let rows = state.rows.clone();
                        for (index, row) in rows.iter().enumerate() {
                            let response = ui.selectable_label(
                                state.selected == Some(index),
                                format!("{}    {}", row.name, row.status),
                            );
                            if response.clicked() {
                                state.selected = Some(index);
                            }
                            if response.double_clicked() {
                                state.selected = Some(index);
                                pressed = Some(0);
                            }
                        }
                    });
                ui.label(state.status.as_str());
                ui.horizontal(|ui| {
                    for (id, text) in [
                        (1, tr("Log out")),
                        (2, tr("Wake up")),
                        (3, tr("Help someone...")),
                        (4, tr("More...")),
                        (0, tr("Connect")),
                    ] {
                        if ui.button(text).clicked() {
                            pressed = Some(id);
                        }
                    }
                });
            });
        });
        if ctx.input(|i| i.key_pressed(egui::Key::Enter)) && state.enabled {
            pressed = Some(0);
        }
        let selected = state
            .selected
            .and_then(|index| state.rows.get(index))
            .map(|row| row.id.clone());
        let action = match (pressed, selected) {
            (Some(1), _) => Some(ListAction::LogOut),
            (Some(3), _) => Some(ListAction::Help),
            (Some(0), Some(id)) => Some(ListAction::Connect(id)),
            (Some(2), Some(id)) => Some(ListAction::Wake(id)),
            (Some(4), Some(id)) => Some(ListAction::More(id)),
            (Some(_), None) => {
                state.status = tr("Select a device first.").into();
                None
            }
            _ => None,
        };
        drop(state);
        if let Some(action) = action {
            self.push(action);
        }
        true
    }

    fn closed(&mut self) {
        self.push(ListAction::Closed);
    }
}

pub struct DeviceList {
    _opened: Opened,
    state: Arc<Mutex<ListState>>,
    actions: Arc<Mutex<Vec<ListAction>>>,
}

impl DeviceList {
    pub fn new(title: &str, heading: &str) -> Result<DeviceList> {
        let state = Arc::new(Mutex::new(ListState {
            enabled: true,
            ..ListState::default()
        }));
        let actions = Arc::new(Mutex::new(Vec::new()));
        let view = ListView {
            title: title.to_string(),
            heading: heading.to_string(),
            state: state.clone(),
            actions: actions.clone(),
        };
        Ok(DeviceList {
            _opened: gui::open(Box::new(view))?,
            state,
            actions,
        })
    }

    pub fn hwnd(&self) -> Hwnd {
        Hwnd(0)
    }

    /// The devices shown; the selection stays on the same device where it can.
    pub fn set_rows(&mut self, rows: Vec<DeviceRow>) {
        if let Ok(mut state) = self.state.lock() {
            let selected = state
                .selected
                .and_then(|i| state.rows.get(i))
                .map(|row| row.id.clone());
            state.selected = selected.and_then(|id| rows.iter().position(|row| row.id == id));
            state.rows = rows;
        }
        gui::repaint();
    }

    /// Selects the device with this id or name; false when there is none.
    pub fn select(&self, key: &str) -> bool {
        let Ok(mut state) = self.state.lock() else {
            return false;
        };
        let Some(index) = state
            .rows
            .iter()
            .position(|row| row.id == key || row.name.eq_ignore_ascii_case(key))
        else {
            return false;
        };
        state.selected = Some(index);
        gui::repaint();
        true
    }

    /// Presses "Connect" (for tests).
    pub fn press_connect(&self) {
        let id = self.state.lock().ok().and_then(|state| {
            state
                .selected
                .and_then(|i| state.rows.get(i))
                .map(|row| row.id.clone())
        });
        if let (Some(id), Ok(mut actions)) = (id, self.actions.lock()) {
            actions.push(ListAction::Connect(id));
        }
    }

    pub fn set_status(&self, text: &str) {
        if let Ok(mut state) = self.state.lock() {
            state.status = text.to_string();
        }
        gui::repaint();
    }

    pub fn save_picture(&self, _path: &Path) -> std::io::Result<()> {
        Err(std::io::Error::other("pictures of forms: not on Linux yet"))
    }

    /// Greys the window out while a session runs.
    pub fn set_enabled(&self, enabled: bool) {
        if let Ok(mut state) = self.state.lock() {
            state.enabled = enabled;
        }
        gui::repaint();
    }

    pub fn next(&mut self, wait_ms: u32) -> Option<ListAction> {
        wait_for(&self.actions, wait_ms)
    }
}

#[derive(Default)]
struct HelpState {
    help_id: String,
    code: String,
    status: String,
}

struct HelpFormView {
    state: Arc<Mutex<HelpState>>,
    actions: Arc<Mutex<Vec<HelpAction>>>,
    focused: bool,
}

fn submit(state: &HelpState) -> HelpAction {
    HelpAction::Submit {
        help_id: state.help_id.chars().filter(char::is_ascii_digit).collect(),
        code: normalize_code(&state.code),
    }
}

impl View for HelpFormView {
    fn builder(&self) -> egui::ViewportBuilder {
        egui::ViewportBuilder::default()
            .with_title(tr("Queqiao - help someone"))
            .with_inner_size([380.0, 270.0])
            .with_resizable(false)
    }

    fn draw(&mut self, ctx: &egui::Context) -> bool {
        let Ok(mut state) = self.state.lock() else {
            return false;
        };
        let mut connect = ctx.input(|i| i.key_pressed(egui::Key::Enter));
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.add(egui::Label::new(tr(
                "Ask the person you help for the help ID and code that Queqiao shows them (tray menu: Let someone help me...).",
            )).wrap());
            ui.add_space(6.0);
            ui.label(tr("Help ID"));
            let field = ui.add(egui::TextEdit::singleline(&mut state.help_id).desired_width(f32::INFINITY));
            if !self.focused {
                field.request_focus();
                self.focused = true;
            }
            ui.label(tr("Code (leave empty to ask the person to allow it)"));
            ui.add(egui::TextEdit::singleline(&mut state.code).desired_width(f32::INFINITY));
            ui.add(egui::Label::new(state.status.as_str()).wrap());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                if ui.button(tr("Connect")).clicked() {
                    connect = true;
                }
            });
        });
        if connect {
            let action = submit(&state);
            drop(state);
            if let Ok(mut actions) = self.actions.lock() {
                actions.push(action);
            }
        }
        true
    }

    fn closed(&mut self) {
        if let Ok(mut actions) = self.actions.lock() {
            actions.push(HelpAction::Cancel);
        }
    }
}

pub struct HelpForm {
    _opened: Opened,
    state: Arc<Mutex<HelpState>>,
    actions: Arc<Mutex<Vec<HelpAction>>>,
}

impl HelpForm {
    pub fn new() -> Result<HelpForm> {
        let state = Arc::new(Mutex::new(HelpState::default()));
        let actions = Arc::new(Mutex::new(Vec::new()));
        let view = HelpFormView {
            state: state.clone(),
            actions: actions.clone(),
            focused: false,
        };
        Ok(HelpForm {
            _opened: gui::open(Box::new(view))?,
            state,
            actions,
        })
    }

    pub fn hwnd(&self) -> Hwnd {
        Hwnd(0)
    }

    /// Fills the form and presses Connect (tests).
    pub fn submit_with(&self, help_id: &str, code: &str) {
        self.fill(help_id, code);
        self.press_connect();
    }

    /// Fills the form (from an invitation), leaving Connect to the person.
    pub fn fill(&self, help_id: &str, code: &str) {
        if let Ok(mut state) = self.state.lock() {
            state.help_id = queqiao_core::invitation::spaced_id(help_id);
            state.code = queqiao_core::invitation::halved_code(code);
        }
        gui::repaint();
    }

    /// Presses Connect as a click would (tests).
    pub fn press_connect(&self) {
        let action = self.state.lock().ok().map(|state| submit(&state));
        if let (Some(action), Ok(mut actions)) = (action, self.actions.lock()) {
            actions.push(action);
        }
    }

    pub fn save_picture(&self, _path: &Path) -> std::io::Result<()> {
        Err(std::io::Error::other("pictures of forms: not on Linux yet"))
    }

    pub fn set_status(&self, text: &str) {
        if let Ok(mut state) = self.state.lock() {
            state.status = text.to_string();
        }
        gui::repaint();
    }

    pub fn next(&mut self, wait_ms: u32) -> Option<HelpAction> {
        wait_for(&self.actions, wait_ms)
    }
}
