//! The login form on Linux (Windows: forms.rs's LoginForm): server, its certificate's fingerprint, user name and
//! password; Log in (Enter) or Create an account; a status line that wraps, so an error is read whole. Polled with
//! `next`, as on Windows.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui;
use queqiao_core::text::tr;

use super::gui::{self, Opened, View};
use super::window::Hwnd;
use super::{Error, Result};
use crate::model::{LoginAction, LoginFields};

#[derive(Default)]
struct State {
    fields: LoginFields,
    status: String,
    busy: bool,
    actions: Vec<LoginAction>,
}

struct LoginView {
    title: String,
    state: Arc<Mutex<State>>,
    focused: bool,
}

impl View for LoginView {
    fn builder(&self) -> egui::ViewportBuilder {
        egui::ViewportBuilder::default()
            .with_title(&self.title)
            .with_inner_size([380.0, 360.0])
            .with_resizable(false)
    }

    fn draw(&mut self, ctx: &egui::Context) -> bool {
        let Ok(mut state) = self.state.lock() else {
            return false;
        };
        let enter = ctx.input(|i| i.key_pressed(egui::Key::Enter));
        let mut pressed = None;
        egui::CentralPanel::default().show(ctx, |ui| {
            let State {
                fields,
                status,
                busy,
                ..
            } = &mut *state;
            let mut first_empty = None;
            for (label, value, secret) in [
                (tr("Server (host:port)"), &mut fields.server, false),
                (
                    tr("Server certificate fingerprint (SHA-256)"),
                    &mut fields.pin,
                    false,
                ),
                (tr("User name"), &mut fields.user, false),
                (tr("Password"), &mut fields.password, true),
            ] {
                ui.label(label);
                let empty = value.is_empty();
                let field = ui.add(
                    egui::TextEdit::singleline(value)
                        .password(secret)
                        .desired_width(f32::INFINITY),
                );
                if (empty || secret) && first_empty.is_none() {
                    first_empty = Some(field);
                }
                ui.add_space(4.0);
            }
            // Start where something is missing.
            if !self.focused {
                if let Some(field) = first_empty {
                    field.request_focus();
                }
                self.focused = true;
            }
            ui.add_space(4.0);
            ui.add(egui::Label::new(status.as_str()).wrap());
            ui.add_space(8.0);
            ui.add_enabled_ui(!*busy, |ui| {
                ui.horizontal(|ui| {
                    if ui.button(tr("Create an account")).clicked() {
                        pressed = Some(false);
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button(tr("Log in")).clicked() || enter {
                            pressed = Some(true);
                        }
                    });
                });
            });
        });
        if let Some(login) = pressed
            && !state.busy
        {
            let fields = trimmed(&state.fields);
            state.actions.push(if login {
                LoginAction::Submit(fields)
            } else {
                LoginAction::Register(fields)
            });
        }
        true
    }

    fn closed(&mut self) {
        if let Ok(mut state) = self.state.lock() {
            state.actions.push(LoginAction::Cancel);
        }
    }
}

fn trimmed(fields: &LoginFields) -> LoginFields {
    LoginFields {
        server: fields.server.trim().to_string(),
        pin: fields.pin.trim().to_string(),
        user: fields.user.trim().to_string(),
        password: fields.password.clone(),
    }
}

pub struct LoginForm {
    _opened: Opened,
    state: Arc<Mutex<State>>,
}

impl LoginForm {
    pub fn new(title: &str, fields: &LoginFields) -> Result<LoginForm> {
        let state = Arc::new(Mutex::new(State {
            fields: fields.clone(),
            ..State::default()
        }));
        let view = LoginView {
            title: title.to_string(),
            state: state.clone(),
            focused: false,
        };
        Ok(LoginForm {
            _opened: gui::open(Box::new(view))?,
            state,
        })
    }

    /// No window handle for egui's windows here (Windows: the form's HWND, an owner for its dialogs).
    pub fn hwnd(&self) -> Hwnd {
        Hwnd(0)
    }

    pub fn fields(&self) -> LoginFields {
        self.state
            .lock()
            .map(|state| trimmed(&state.fields))
            .unwrap_or_default()
    }

    /// Fills the form (for tests, which then call `submit`).
    pub fn set_fields(&self, fields: &LoginFields) {
        if let Ok(mut state) = self.state.lock() {
            state.fields = fields.clone();
        }
        gui::repaint();
    }

    /// Presses "Log in" (for tests).
    pub fn submit(&self) {
        if let Ok(mut state) = self.state.lock() {
            let fields = trimmed(&state.fields);
            state.actions.push(LoginAction::Submit(fields));
        }
    }

    /// Presses "Create an account" (for tests).
    pub fn press_signup(&self) {
        if let Ok(mut state) = self.state.lock() {
            let fields = trimmed(&state.fields);
            state.actions.push(LoginAction::Register(fields));
        }
    }

    pub fn set_status(&self, text: &str) {
        if let Ok(mut state) = self.state.lock() {
            state.status = text.to_string();
        }
        gui::repaint();
    }

    /// While a login is under way: no second press, a note in the status line.
    pub fn set_busy(&self, busy: bool, status: &str) {
        if let Ok(mut state) = self.state.lock() {
            state.busy = busy;
            state.status = status.to_string();
        }
        gui::repaint();
    }

    /// What the person did, waiting up to `wait_ms`.
    pub fn next(&mut self, wait_ms: u32) -> Option<LoginAction> {
        let until = Instant::now() + Duration::from_millis(u64::from(wait_ms));
        loop {
            if let Ok(mut state) = self.state.lock()
                && !state.actions.is_empty()
            {
                return Some(state.actions.remove(0));
            }
            if Instant::now() >= until {
                return None;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// For tests: the form's picture (not for egui's windows here yet).
    pub fn save_picture(&self, _path: &Path) -> std::io::Result<()> {
        Err(std::io::Error::other(
            Error::from("pictures of forms: not on Linux yet").message(),
        ))
    }
}
