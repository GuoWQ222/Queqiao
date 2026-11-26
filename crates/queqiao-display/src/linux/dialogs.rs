//! Small dialogs on Linux (Windows: forms.rs's ask_text, ask_secret, confirm, show_info, show_error): drawn by the GUI
//! thread, each waits for the person's answer as Windows' do; also the menu (Windows: popup_menu at the pointer), as a
//! small window of its items. Escape and closing the window cancel.

use std::sync::mpsc::{self, Sender};

use eframe::egui;
use queqiao_core::text::tr;

use super::gui::{self, View};

enum Kind {
    Text {
        secret: bool,
    },
    Info {
        error: bool,
    },
    Confirm,
    /// A question with its own action button (the label) beside Cancel; the action is the default.
    Action(String),
    /// The items (id, text, checked); the answer is the chosen id.
    Menu(Vec<(u16, String, bool)>),
}

/// What the person answered: OK with the text typed, or cancelled.
enum Answer {
    Ok(String),
    Cancel,
}

struct DialogView {
    title: String,
    prompt: String,
    kind: Kind,
    text: String,
    focused: bool,
    answer: Option<Sender<Answer>>,
}

impl DialogView {
    fn finish(&mut self, answer: Answer) -> bool {
        if let Some(sender) = self.answer.take() {
            let _ = sender.send(answer);
        }
        false
    }
}

impl View for DialogView {
    fn builder(&self) -> egui::ViewportBuilder {
        egui::ViewportBuilder::default()
            .with_title(&self.title)
            .with_inner_size([
                400.0,
                match &self.kind {
                    Kind::Text { .. } => 150.0,
                    Kind::Menu(items) => 56.0 + 32.0 * items.len() as f32,
                    _ => 140.0,
                },
            ])
            .with_resizable(false)
            .with_always_on_top()
    }

    fn draw(&mut self, ctx: &egui::Context) -> bool {
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            return self.finish(Answer::Cancel);
        }
        let enter = ctx.input(|i| i.key_pressed(egui::Key::Enter));
        let mut answer = None;
        egui::CentralPanel::default().show(ctx, |ui| {
            if let Kind::Info { error: true } = self.kind {
                ui.colored_label(egui::Color32::from_rgb(0xD0, 0x40, 0x40), &self.prompt);
                ui.add_space(8.0);
            } else if !self.prompt.is_empty() {
                ui.label(&self.prompt);
                ui.add_space(8.0);
            }
            if let Kind::Menu(items) = &self.kind {
                for (id, text, checked) in items {
                    let label = if *checked {
                        format!("✔  {text}")
                    } else {
                        format!("     {text}")
                    };
                    let item =
                        egui::Button::new(label).min_size(egui::vec2(ui.available_width(), 26.0));
                    if ui.add(item).clicked() {
                        answer = Some(Answer::Ok(id.to_string()));
                    }
                }
                ui.add_space(6.0);
            }
            if let Kind::Text { secret } = self.kind {
                let field = ui.add(
                    egui::TextEdit::singleline(&mut self.text)
                        .password(secret)
                        .desired_width(f32::INFINITY),
                );
                if !self.focused {
                    field.request_focus();
                    self.focused = true;
                }
                if enter {
                    answer = Some(Answer::Ok(self.text.clone()));
                }
                ui.add_space(8.0);
            }
            ui.with_layout(
                egui::Layout::right_to_left(egui::Align::Min),
                |ui| match self.kind {
                    Kind::Text { .. } => {
                        if ui.button(tr("Cancel")).clicked() {
                            answer = Some(Answer::Cancel);
                        }
                        if ui.button(tr("OK")).clicked() {
                            answer = Some(Answer::Ok(self.text.clone()));
                        }
                    }
                    Kind::Info { .. } => {
                        if ui.button(tr("OK")).clicked() || enter {
                            answer = Some(Answer::Ok(String::new()));
                        }
                    }
                    Kind::Menu(_) => {
                        if ui.button(tr("Cancel")).clicked() {
                            answer = Some(Answer::Cancel);
                        }
                    }
                    Kind::Confirm => {
                        // No is the default: Enter never confirms.
                        if ui.button(tr("No")).clicked() || enter {
                            answer = Some(Answer::Cancel);
                        }
                        if ui.button(tr("Yes")).clicked() {
                            answer = Some(Answer::Ok(String::new()));
                        }
                    }
                    Kind::Action(ref label) => {
                        if ui.button(tr("Cancel")).clicked() {
                            answer = Some(Answer::Cancel);
                        }
                        // The action in the accent colour, as the main window's buttons (UU Remote's dialog).
                        let action = egui::Button::new(
                            egui::RichText::new(label.as_str()).color(egui::Color32::WHITE),
                        )
                        .fill(egui::Color32::from_rgb(0x3B, 0x7B, 0xF6));
                        if ui.add(action).clicked() || enter {
                            answer = Some(Answer::Ok(String::new()));
                        }
                    }
                },
            );
        });
        match answer {
            Some(answer) => self.finish(answer),
            None => true,
        }
    }

    fn closed(&mut self) {
        self.finish(Answer::Cancel);
    }
}

/// Opens the dialog and waits for the answer; None when the GUI cannot start (the caller goes on as if cancelled).
fn ask(title: &str, prompt: &str, kind: Kind, initial: &str) -> Option<Answer> {
    let (sender, answer) = mpsc::channel();
    let view = DialogView {
        title: title.to_string(),
        prompt: prompt.to_string(),
        kind,
        text: initial.to_string(),
        focused: false,
        answer: Some(sender),
    };
    let _opened = gui::open(Box::new(view)).ok()?;
    answer.recv().ok()
}

/// A line of text from the person (None: cancelled).
pub fn ask_text(title: &str, prompt: &str, initial: &str) -> Option<String> {
    match ask(title, prompt, Kind::Text { secret: false }, initial)? {
        Answer::Ok(text) => Some(text),
        Answer::Cancel => None,
    }
}

/// A password from the person, not shown as it is typed (None: cancelled).
pub fn ask_secret(title: &str, prompt: &str) -> Option<String> {
    match ask(title, prompt, Kind::Text { secret: true }, "")? {
        Answer::Ok(text) => Some(text),
        Answer::Cancel => None,
    }
}

/// A menu (Windows: at the pointer): the chosen item's id, None when cancelled.
pub fn menu(title: &str, items: &[(u16, &str, bool)]) -> Option<u16> {
    let items = items
        .iter()
        .map(|(id, text, checked)| (*id, (*text).to_string(), *checked))
        .collect();
    match ask(title, "", Kind::Menu(items), "")? {
        Answer::Ok(id) => id.parse().ok(),
        Answer::Cancel => None,
    }
}

/// Yes or no (no unless Yes is pressed).
pub fn confirm(title: &str, text: &str) -> bool {
    matches!(ask(title, text, Kind::Confirm, ""), Some(Answer::Ok(_)))
}

/// The question before taking control of a PC another device of the account controls ([`crate::take_over_question`]):
/// true for Take Control, false for Cancel or a closed window.
pub fn confirm_take_over(controller: &str) -> bool {
    let (title, text, action) = crate::take_over_question(controller);
    matches!(
        ask(&title, &text, Kind::Action(action), ""),
        Some(Answer::Ok(_))
    )
}

pub fn show_info(title: &str, text: &str) {
    if ask(title, text, Kind::Info { error: false }, "").is_none() {
        eprintln!("{title}: {text}");
    }
}

pub fn show_error(title: &str, text: &str) {
    if ask(title, text, Kind::Info { error: true }, "").is_none() {
        eprintln!("{title}: {text}");
    }
}
