//! The viewer's lists and forms on Linux (Windows: forms.rs's DeviceList, HelpForm, PickList, PasswordForm), with their
//! API: polled with `next`, as on Windows. The device list, the "help someone" form, the session's transfers (a list
//! with buttons) and the form asking for that PC's password (its lock screen).

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
    opened: Opened,
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
            opened: gui::open(Box::new(view))?,
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

    pub fn save_picture(&self, path: &Path) -> std::io::Result<()> {
        self.opened.save_picture(path)
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
    opened: Opened,
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
            opened: gui::open(Box::new(view))?,
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

    pub fn save_picture(&self, path: &Path) -> std::io::Result<()> {
        self.opened.save_picture(path)
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

/// A button of the list pressed, and the row selected then.
type Picked = (u16, Option<usize>);

/// The id `PickList::next` gives when the list was closed.
pub const PICK_CLOSED: u16 = 0xFFFF;

#[derive(Default)]
struct PickState {
    items: Vec<String>,
    selected: Option<usize>,
}

struct PickView {
    title: String,
    heading: String,
    buttons: Vec<(u16, String)>,
    state: Arc<Mutex<PickState>>,
    actions: Arc<Mutex<Vec<Picked>>>,
}

impl PickView {
    fn push(&self, id: u16) {
        let selected = self.state.lock().ok().and_then(|state| state.selected);
        if let Ok(mut actions) = self.actions.lock() {
            actions.push((id, selected));
        }
    }
}

impl View for PickView {
    fn builder(&self) -> egui::ViewportBuilder {
        egui::ViewportBuilder::default()
            .with_title(&self.title)
            .with_inner_size([520.0, 320.0])
    }

    fn draw(&mut self, ctx: &egui::Context) -> bool {
        let mut pressed = None;
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.label(&self.heading);
            ui.add_space(6.0);
            if let Ok(mut state) = self.state.lock() {
                egui::ScrollArea::vertical()
                    .max_height(200.0)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        let items = state.items.clone();
                        for (index, item) in items.iter().enumerate() {
                            if ui
                                .selectable_label(state.selected == Some(index), item)
                                .clicked()
                            {
                                state.selected = Some(index);
                            }
                        }
                    });
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                for (id, text) in &self.buttons {
                    if ui.button(text).clicked() {
                        pressed = Some(*id);
                    }
                }
            });
        });
        if let Some(id) = pressed {
            self.push(id);
        }
        true
    }

    fn closed(&mut self) {
        self.push(PICK_CLOSED);
    }
}

/// A list with buttons (the session's transfers).
pub struct PickList {
    opened: Opened,
    state: Arc<Mutex<PickState>>,
    actions: Arc<Mutex<Vec<Picked>>>,
}

impl PickList {
    pub fn new(title: &str, heading: &str, buttons: &[(u16, &str)]) -> Result<PickList> {
        let state = Arc::new(Mutex::new(PickState::default()));
        let actions = Arc::new(Mutex::new(Vec::new()));
        let view = PickView {
            title: title.to_string(),
            heading: heading.to_string(),
            buttons: buttons
                .iter()
                .map(|(id, text)| (*id, (*text).to_string()))
                .collect(),
            state: state.clone(),
            actions: actions.clone(),
        };
        Ok(PickList {
            opened: gui::open(Box::new(view))?,
            state,
            actions,
        })
    }

    /// Shows `items`, keeping the selected row where it was.
    pub fn set_items(&mut self, items: Vec<String>) {
        if let Ok(mut state) = self.state.lock()
            && state.items != items
        {
            state.selected = state.selected.filter(|index| *index < items.len());
            state.items = items;
            gui::repaint();
        }
    }

    pub fn selected(&self) -> Option<usize> {
        self.state.lock().ok().and_then(|state| state.selected)
    }

    /// For tests: the list's picture.
    pub fn save_picture(&self, path: &Path) -> std::io::Result<()> {
        self.opened.save_picture(path)
    }

    /// A button pressed (with the selected row), or None; Some((PICK_CLOSED, _)) when the window was closed.
    pub fn next(&mut self, wait_ms: u32) -> Option<(u16, Option<usize>)> {
        wait_for(&self.actions, wait_ms)
    }
}

/// What the person did in the password form.
pub enum PasswordAction {
    Submit(String),
    Cancel,
}

#[derive(Default)]
struct PasswordState {
    password: String,
    status: String,
    /// The field takes the keyboard (again, after a wrong password).
    focus: bool,
}

struct PasswordView {
    title: String,
    text: String,
    state: Arc<Mutex<PasswordState>>,
    actions: Arc<Mutex<Vec<PasswordAction>>>,
}

impl View for PasswordView {
    fn builder(&self) -> egui::ViewportBuilder {
        egui::ViewportBuilder::default()
            .with_title(&self.title)
            .with_inner_size([420.0, 210.0])
            .with_resizable(false)
            .with_always_on_top()
    }

    fn draw(&mut self, ctx: &egui::Context) -> bool {
        let Ok(mut state) = self.state.lock() else {
            return false;
        };
        let mut action = None;
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            action = Some(PasswordAction::Cancel);
        }
        let enter = ctx.input(|i| i.key_pressed(egui::Key::Enter));
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.add(egui::Label::new(self.text.as_str()).wrap());
            ui.add_space(8.0);
            let field = ui.add(
                egui::TextEdit::singleline(&mut state.password)
                    .password(true)
                    .desired_width(f32::INFINITY),
            );
            if state.focus {
                field.request_focus();
                state.focus = false;
            }
            ui.label(state.status.as_str());
            ui.add_space(8.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                if ui.button(tr("Cancel")).clicked() {
                    action = Some(PasswordAction::Cancel);
                }
                if ui.button(tr("OK")).clicked() || enter {
                    action = Some(PasswordAction::Submit(state.password.clone()));
                }
            });
        });
        drop(state);
        if let Some(action) = action
            && let Ok(mut actions) = self.actions.lock()
        {
            actions.push(action);
        }
        true
    }

    fn closed(&mut self) {
        if let Ok(mut actions) = self.actions.lock() {
            actions.push(PasswordAction::Cancel);
        }
    }
}

/// The form asking for that PC's password (its lock screen).
pub struct PasswordForm {
    opened: Opened,
    state: Arc<Mutex<PasswordState>>,
    actions: Arc<Mutex<Vec<PasswordAction>>>,
}

impl PasswordForm {
    pub fn new(title: &str, text: &str) -> Result<PasswordForm> {
        let state = Arc::new(Mutex::new(PasswordState {
            focus: true,
            ..PasswordState::default()
        }));
        let actions = Arc::new(Mutex::new(Vec::new()));
        let view = PasswordView {
            title: title.to_string(),
            text: text.to_string(),
            state: state.clone(),
            actions: actions.clone(),
        };
        Ok(PasswordForm {
            opened: gui::open(Box::new(view))?,
            state,
            actions,
        })
    }

    /// A line under the field (the field is emptied).
    pub fn set_status(&self, text: &str) {
        if let Ok(mut state) = self.state.lock() {
            state.status = text.to_string();
            state.password.clear();
            state.focus = true;
        }
        gui::repaint();
    }

    /// For tests: the form's picture.
    pub fn save_picture(&self, path: &Path) -> std::io::Result<()> {
        self.opened.save_picture(path)
    }

    pub fn next(&mut self, wait_ms: u32) -> Option<PasswordAction> {
        wait_for(&self.actions, wait_ms)
    }
}
