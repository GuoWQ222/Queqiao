//! The login form on Linux, drawn as Windows' (login_window.rs) after UU Remote's (the user, 2026-11-26): a pale blue
//! sky fading to white, the magpie and Queqiao's name, the user name and password on underlines, one button, the
//! server's fields only when none is built in or saved, a status line that wraps (an error is read whole), the
//! copyright at the foot. Logging in and signing up are two pages, as on Windows (the user, 2026-11-27): "Log in" for
//! an account that exists, "Create account" with the password typed twice, a link under the button between them.
//! Polled with `next`, as on Windows.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui::{self, Align2, Color32, CornerRadius, Pos2, Rect, Stroke, Vec2};
use queqiao_core::text::tr;

use super::Result;
use super::gui::{self, Opened, View};
use super::theme;
use super::window::Hwnd;
use crate::model::{LoginAction, LoginFields, sign_up_problem};

/// The window's size, the fields' column, the sky's height (as Windows').
const WIDTH: f32 = 420.0;
const HEIGHT: f32 = 490.0;
const LEFT: f32 = 60.0;
const FIELD: f32 = 300.0;
const SKY_HEIGHT: f32 = 118.0;
const SKY: Color32 = Color32::from_rgb(0xE3, 0xEF, 0xFE);
const BUTTON_OFF: Color32 = Color32::from_rgb(0xC8, 0xCF, 0xD8);
const FOOT: Color32 = Color32::from_rgb(0xA4, 0xA9, 0xB4);

#[derive(Default)]
struct State {
    fields: LoginFields,
    status: String,
    busy: bool,
    /// The server's fields shown: none built in or saved (a developer's build). With one, no way to another from here
    /// (the user, 2026-11-27: "Use another server..." was a way for a scammer to talk someone onto theirs).
    show_server: bool,
    /// The sign-up page (else the login page), and its password typed again.
    sign_up: bool,
    repeat: String,
    actions: Vec<LoginAction>,
}

/// Where each part goes, as Windows' layout: the rows of the fields shown, the button, the link, the status.
fn layout(show_server: bool, sign_up: bool) -> (Vec<f32>, f32, f32, f32) {
    let mut rows = vec![124.0, 182.0];
    let extra = usize::from(sign_up) + 2 * usize::from(show_server);
    for _ in 0..extra {
        rows.push(rows[rows.len() - 1] + 58.0);
    }
    let button = rows[rows.len() - 1] + 72.0;
    (rows, button, button + 50.0, button + 80.0)
}

/// The window's height for both pages: room for the status and the copyright below the taller one.
fn height(show_server: bool) -> f32 {
    HEIGHT.max(layout(show_server, true).3 + 48.0 + 40.0)
}

struct LoginView {
    title: String,
    height: f32,
    state: Arc<Mutex<State>>,
    focused: bool,
    logo: Option<egui::TextureHandle>,
}

impl View for LoginView {
    fn builder(&self) -> egui::ViewportBuilder {
        let mut builder = egui::ViewportBuilder::default()
            .with_title(&self.title)
            .with_inner_size([WIDTH, self.height])
            .with_resizable(false);
        if let Some((width, height, rgba)) = super::icon::icon(64) {
            builder = builder.with_icon(egui::IconData {
                rgba,
                width,
                height,
            });
        }
        builder
    }

    fn draw(&mut self, ctx: &egui::Context) -> bool {
        if self.logo.is_none()
            && let Some((width, height, rgba)) = super::icon::icon(96)
        {
            self.logo = Some(ctx.load_texture(
                "queqiao-login-logo",
                egui::ColorImage::from_rgba_unmultiplied([width as usize, height as usize], &rgba),
                egui::TextureOptions::LINEAR,
            ));
        }
        let Ok(mut state) = self.state.lock() else {
            return false;
        };
        let enter = ctx.input(|i| i.key_pressed(egui::Key::Enter));
        let mut pressed = false;
        let mut switched = false;
        let height = self.height;
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(theme::WHITE))
            .show(ctx, |ui| {
                let page = ui.max_rect();
                let painter = ui.painter().clone();
                let at = |x: f32, y: f32, w: f32, h: f32| {
                    Rect::from_min_size(page.min + Vec2::new(x, y), Vec2::new(w, h))
                };
                // The sky: pale blue fading to white.
                let sky = Rect::from_min_size(page.min, Vec2::new(page.width(), SKY_HEIGHT));
                let mut mesh = egui::Mesh::default();
                mesh.colored_vertex(sky.left_top(), SKY);
                mesh.colored_vertex(sky.right_top(), SKY);
                mesh.colored_vertex(sky.right_bottom(), theme::WHITE);
                mesh.colored_vertex(sky.left_bottom(), theme::WHITE);
                mesh.add_triangle(0, 1, 2);
                mesh.add_triangle(0, 2, 3);
                painter.add(egui::Shape::mesh(mesh));
                // The magpie and the name, side by side in the middle.
                let name = painter.layout_no_wrap(
                    tr("Queqiao").to_string(),
                    theme::bold(26.0),
                    theme::TEXT,
                );
                let start = page.center().x - (44.0 + 12.0 + name.size().x) / 2.0;
                if let Some(logo) = &self.logo {
                    painter.image(
                        logo.id(),
                        Rect::from_min_size(Pos2::new(start, page.top() + 30.0), Vec2::splat(44.0)),
                        Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                        Color32::WHITE,
                    );
                }
                painter.galley(
                    Pos2::new(start + 56.0, page.top() + 52.0 - name.size().y / 2.0),
                    name,
                    theme::TEXT,
                );
                theme::text_in(
                    &painter,
                    at(0.0, 84.0, page.width(), 20.0),
                    Align2::CENTER_CENTER,
                    if state.sign_up {
                        tr("Create a new account")
                    } else {
                        tr("Log in to your account")
                    },
                    theme::regular(12.0),
                    theme::SECONDARY,
                );
                let State {
                    fields,
                    status,
                    busy,
                    show_server,
                    sign_up,
                    repeat,
                    ..
                } = &mut *state;
                let (rows, button, link_y, status_y) = layout(*show_server, *sign_up);
                let mut first_empty = None;
                let mut entries = vec![
                    (tr("User name"), &mut fields.user, false),
                    (tr("Password"), &mut fields.password, true),
                ];
                if *sign_up {
                    entries.push((tr("Repeat the password"), repeat, true));
                }
                if *show_server {
                    entries.push((tr("Server (host:port)"), &mut fields.server, false));
                    entries.push((tr("Server key (SHA-256)"), &mut fields.pin, false));
                }
                for ((label, value, secret), y) in entries.into_iter().zip(rows) {
                    theme::text_in(
                        &painter,
                        at(LEFT, y, FIELD, 16.0),
                        Align2::LEFT_CENTER,
                        label,
                        theme::regular(12.0),
                        theme::SECONDARY,
                    );
                    let empty = value.is_empty();
                    let field = ui.put(
                        at(LEFT, y + 20.0, FIELD, 24.0),
                        egui::TextEdit::singleline(value)
                            .password(secret)
                            .frame(false)
                            .font(theme::regular(15.0))
                            .text_color(theme::TEXT),
                    );
                    // The underline, in the accent colour while the field has the keyboard.
                    let (width, color) = if field.has_focus() {
                        (2.0_f32, theme::ACCENT)
                    } else {
                        (1.0, theme::DIVIDER)
                    };
                    painter.line_segment(
                        [
                            page.min + Vec2::new(LEFT, y + 46.0),
                            page.min + Vec2::new(LEFT + FIELD, y + 46.0),
                        ],
                        Stroke::new(width, color),
                    );
                    if (empty || secret) && first_empty.is_none() {
                        first_empty = Some(field);
                    }
                }
                // Start where something is missing.
                if !self.focused {
                    if let Some(field) = first_empty {
                        field.request_focus();
                    }
                    self.focused = true;
                }
                // The one button: rounded, in the accent colour (grey while a login is under way).
                let area = at(LEFT, button, FIELD, 40.0);
                let response = theme::hit(ui, area, "log-in");
                painter.rect_filled(
                    area,
                    CornerRadius::same(8),
                    if *busy { BUTTON_OFF } else { theme::ACCENT },
                );
                theme::text_in(
                    &painter,
                    area,
                    Align2::CENTER_CENTER,
                    if *sign_up {
                        tr("Create account")
                    } else {
                        tr("Log in")
                    },
                    theme::bold(15.0),
                    theme::WHITE,
                );
                if (response.clicked() || enter) && !*busy {
                    pressed = true;
                }
                // The link to the other page.
                let area = at(LEFT, link_y, FIELD, 22.0);
                let response = theme::hit(ui, area, "other-page");
                theme::text_in(
                    &painter,
                    area,
                    Align2::CENTER_CENTER,
                    if *sign_up {
                        tr("Already have an account? Log in")
                    } else {
                        tr("No account yet? Create one")
                    },
                    theme::regular(12.0),
                    if *busy { BUTTON_OFF } else { theme::ACCENT },
                );
                if response.clicked() && !*busy {
                    switched = true;
                }
                // The status, each line centred (an error is read whole).
                if !status.is_empty() {
                    ui.scope_builder(
                        egui::UiBuilder::new()
                            .max_rect(at(LEFT - 20.0, status_y, FIELD + 40.0, 48.0))
                            .layout(egui::Layout::top_down(egui::Align::Center)),
                        |ui| {
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(status.as_str())
                                        .font(theme::regular(12.0))
                                        .color(theme::SECONDARY),
                                )
                                .wrap(),
                            );
                        },
                    );
                }
                theme::text_in(
                    &painter,
                    at(0.0, height - 32.0, page.width(), 20.0),
                    Align2::CENTER_CENTER,
                    tr("© 2026 GuoWQ222. All rights reserved."),
                    theme::regular(12.0),
                    FOOT,
                );
            });
        if switched {
            state.sign_up = !state.sign_up;
            state.repeat.clear();
            state.status = if state.sign_up {
                tr("A user name has 3 to 32 letters, digits, dots, dashes or underscores, and starts with a letter or digit.")
                    .to_string()
            } else {
                String::new()
            };
            self.focused = false;
        } else if pressed {
            let fields = trimmed(&state.fields);
            if !state.sign_up {
                state.actions.push(LoginAction::Submit(fields));
            } else {
                match sign_up_problem(&fields, &state.repeat) {
                    Some(problem) => state.status = problem.to_string(),
                    None => state.actions.push(LoginAction::SignUp(fields)),
                }
            }
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
    opened: Opened,
    state: Arc<Mutex<State>>,
}

impl LoginForm {
    pub fn new(title: &str, fields: &LoginFields) -> Result<LoginForm> {
        // The server's fields only without one built in or saved (a developer's build).
        let show_server = fields.server.is_empty() || fields.pin.is_empty();
        let state = Arc::new(Mutex::new(State {
            fields: fields.clone(),
            show_server,
            ..State::default()
        }));
        let view = LoginView {
            title: title.to_string(),
            height: height(show_server),
            state: state.clone(),
            focused: false,
            logo: None,
        };
        Ok(LoginForm {
            opened: gui::open(Box::new(view))?,
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
            state.repeat = fields.password.clone();
        }
        gui::repaint();
    }

    /// Goes to the sign-up page or back to the login page (for tests; the person uses the link).
    pub fn show_sign_up(&self, sign_up: bool) {
        if let Ok(mut state) = self.state.lock() {
            state.sign_up = sign_up;
        }
        gui::repaint();
    }

    /// Presses the button: "Log in", or "Create account" on the sign-up page (for tests).
    pub fn submit(&self) {
        if let Ok(mut state) = self.state.lock() {
            let fields = trimmed(&state.fields);
            let action = if state.sign_up {
                LoginAction::SignUp(fields)
            } else {
                LoginAction::Submit(fields)
            };
            state.actions.push(action);
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

    /// For tests: the form's picture.
    pub fn save_picture(&self, path: &Path) -> std::io::Result<()> {
        self.opened.save_picture(path)
    }
}
