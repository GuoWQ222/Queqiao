//! The banner during a session and the consent prompt on Linux (Windows: presence.rs), drawn by the GUI thread.
//! Nothing happens unseen: the banner stays above other windows during every session, names who controls the PC, and
//! its End button ends the session. The prompt denies unless Allow is pressed: Enter, Esc, closing it and the
//! countdown all deny, as on Windows.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui;
use queqiao_core::text::{tr, trf};
use x11rb::connection::Connection;

use super::Result;
use super::gui::{self, Opened, View};
use super::window::Hwnd;

/// The banner's look: a dark bar, 440 x 40 as on Windows, at the top in the middle.
const BANNER_SIZE: [f32; 2] = [440.0, 40.0];

#[derive(Default)]
struct BannerState {
    end_pressed: bool,
}

struct BannerView {
    text: String,
    state: Arc<Mutex<BannerState>>,
    left: f32,
    /// When it was last put above other windows.
    raised: Option<Instant>,
}

impl View for BannerView {
    fn builder(&self) -> egui::ViewportBuilder {
        egui::ViewportBuilder::default()
            .with_title(tr("Queqiao"))
            .with_inner_size(BANNER_SIZE)
            .with_position([self.left, 0.0])
            .with_decorations(false)
            .with_resizable(false)
            .with_always_on_top()
            .with_taskbar(false)
    }

    fn draw(&mut self, ctx: &egui::Context) -> bool {
        // Kept above windows opened since, every 2 s (a command each frame would make it draw without end).
        if self
            .raised
            .is_none_or(|at| at.elapsed() >= Duration::from_secs(2))
        {
            self.raised = Some(Instant::now());
            ctx.send_viewport_cmd(egui::ViewportCommand::WindowLevel(
                egui::WindowLevel::AlwaysOnTop,
            ));
        }
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(egui::Color32::from_rgb(0x20, 0x20, 0x24))
                    .inner_margin(6.0),
            )
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.label(
                        egui::RichText::new(&self.text)
                            .color(egui::Color32::from_rgb(0xF0, 0xF0, 0xF0)),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button(tr("End")).clicked()
                            && let Ok(mut state) = self.state.lock()
                        {
                            state.end_pressed = true;
                        }
                    });
                });
            });
        true
    }

    fn closed(&mut self) {
        // Closing it (the window manager's keys) ends the session, as End does.
        if let Ok(mut state) = self.state.lock() {
            state.end_pressed = true;
        }
    }
}

/// The session banner (Windows: the same, for the agent and the service's desktop helper).
pub struct SessionBanner {
    _opened: Opened,
    state: Arc<Mutex<BannerState>>,
}

impl SessionBanner {
    pub fn show(viewer: &str) -> Result<SessionBanner> {
        let state = Arc::new(Mutex::new(BannerState::default()));
        let width = screen_width().unwrap_or(1280.0);
        let view = BannerView {
            text: format!(
                "\u{25CF}  {}",
                trf("{viewer} is controlling this PC", &[("viewer", viewer)])
            ),
            state: state.clone(),
            left: ((width - BANNER_SIZE[0]) / 2.0).max(0.0),
            raised: None,
        };
        Ok(SessionBanner {
            _opened: gui::open(Box::new(view))?,
            state,
        })
    }

    /// The banner's handle (for its picture).
    pub fn hwnd(&self) -> Hwnd {
        self._opened.hwnd()
    }

    /// True once End was pressed (`wait_ms`: Windows pumps its window that long; here the GUI thread draws it).
    pub fn end_pressed(&self, wait_ms: u32) -> bool {
        let pressed = self.state.lock().is_ok_and(|state| state.end_pressed);
        if !pressed && wait_ms > 0 {
            std::thread::sleep(Duration::from_millis(u64::from(wait_ms.min(50))));
        }
        pressed || self.state.lock().is_ok_and(|state| state.end_pressed)
    }

    /// Presses End (for tests: the same path as a click).
    pub fn press_end(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.end_pressed = true;
        }
    }
}

/// The width of the screen the windows open on (X11's root window), for placing the banner.
fn screen_width() -> Option<f32> {
    let (connection, screen) = x11rb::connect(None).ok()?;
    Some(f32::from(connection.setup().roots[screen].width_in_pixels))
}

#[derive(Default)]
struct PromptState {
    /// The answer once there is one: allowed or denied.
    answer: Option<bool>,
}

struct PromptView {
    question: String,
    deadline: Instant,
    state: Arc<Mutex<PromptState>>,
}

impl View for PromptView {
    fn builder(&self) -> egui::ViewportBuilder {
        egui::ViewportBuilder::default()
            .with_title(tr("Queqiao - allow remote control?"))
            .with_inner_size([420.0, 170.0])
            .with_resizable(false)
            .with_always_on_top()
    }

    fn draw(&mut self, ctx: &egui::Context) -> bool {
        let left = self.deadline.saturating_duration_since(Instant::now());
        let mut answer = None;
        if left.is_zero() {
            answer = Some(false);
        }
        // Enter and Esc deny (Deny is the default button).
        if ctx.input(|i| i.key_pressed(egui::Key::Enter) || i.key_pressed(egui::Key::Escape)) {
            answer = Some(false);
        }
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.label(&self.question);
            ui.add_space(8.0);
            ui.label(trf(
                "Denied automatically in {seconds} s unless you allow it.",
                &[("seconds", &(left.as_secs() + 1).to_string())],
            ));
            ui.add_space(12.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                if ui.button(tr("Allow")).clicked() {
                    answer = Some(true);
                }
                if ui.button(tr("Deny")).clicked() {
                    answer = Some(false);
                }
            });
        });
        ctx.request_repaint_after(Duration::from_millis(200));
        match answer {
            Some(allowed) => {
                if let Ok(mut state) = self.state.lock() {
                    state.answer.get_or_insert(allowed);
                }
                false
            }
            None => true,
        }
    }

    fn closed(&mut self) {
        if let Ok(mut state) = self.state.lock() {
            state.answer.get_or_insert(false);
        }
    }
}

/// The consent prompt: whether the person at this PC lets a viewer control it; denied after `seconds`.
pub struct ConsentPrompt {
    _opened: Opened,
    state: Arc<Mutex<PromptState>>,
}

impl ConsentPrompt {
    pub fn ask(question: &str, seconds: u32) -> Result<ConsentPrompt> {
        let state = Arc::new(Mutex::new(PromptState::default()));
        let view = PromptView {
            question: question.to_string(),
            deadline: Instant::now() + Duration::from_secs(u64::from(seconds)),
            state: state.clone(),
        };
        Ok(ConsentPrompt {
            _opened: gui::open(Box::new(view))?,
            state,
        })
    }

    /// The prompt's handle (for its picture).
    pub fn hwnd(&self) -> Hwnd {
        self._opened.hwnd()
    }

    /// The answer once given: Some(true) allowed, Some(false) denied (also by the countdown or closing it).
    pub fn answer(&self) -> Option<bool> {
        self.state.lock().ok().and_then(|state| state.answer)
    }

    /// For tests: Allow or Deny as a click.
    pub fn press(&self, allow: bool) {
        if let Ok(mut state) = self.state.lock() {
            state.answer.get_or_insert(allow);
        }
        gui::repaint();
    }
}
