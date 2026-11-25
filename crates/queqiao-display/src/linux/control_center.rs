//! The session window's control center on Linux (Windows: control_center.rs; what it shows: panel.rs), drawn by the
//! GUI thread (egui) as a small frameless window under the session bar: the groups in a column on the left, the one
//! pointed at opening its choices on the right (check marks, dots, a line of explanation, the keys, the volume as a
//! slider), in Queqiao's night ink and gold. It closes when a choice is made, on Esc, or when it loses the keyboard (a
//! click elsewhere), as a menu does. Choices reach the session window as the bar's clicks do (window.rs).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use eframe::egui;
use queqiao_core::text::tr;

use super::Result;
use super::gui::{self, Opened, View};
use crate::panel::{Mark, PanelItem};

const INK: egui::Color32 = egui::Color32::from_rgb(0x14, 0x1A, 0x3B);
const HOVER: egui::Color32 = egui::Color32::from_rgb(0x26, 0x31, 0x69);
const GOLD: egui::Color32 = egui::Color32::from_rgb(0xE3, 0xB4, 0x5A);
const TEXT: egui::Color32 = egui::Color32::from_rgb(0xEE, 0xF0, 0xFA);
const MUTED: egui::Color32 = egui::Color32::from_rgb(0xA9, 0xB1, 0xD6);
const DIM: egui::Color32 = egui::Color32::from_rgb(0x66, 0x6E, 0x99);
const DANGER: egui::Color32 = egui::Color32::from_rgb(0xFF, 0x8A, 0x7A);

/// What the person chose: a menu id, or a slider's new value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Choice {
    Menu(u16),
    Value(u16, u8),
}

struct CenterView {
    items: Vec<PanelItem>,
    checks: HashMap<u16, bool>,
    open: Option<usize>,
    position: Option<[f32; 2]>,
    choices: Arc<Mutex<Vec<Choice>>>,
    /// False once the window closed (a choice, Esc, a click elsewhere).
    alive: Arc<AtomicBool>,
    /// Had the keyboard once (losing it then closes the window).
    focused_once: bool,
    done: bool,
}

impl CenterView {
    fn choose(&mut self, choice: Choice) {
        if let Ok(mut choices) = self.choices.lock() {
            choices.push(choice);
        }
        if matches!(choice, Choice::Menu(_)) {
            self.done = true;
        }
    }

    fn checked(&self, id: u16) -> bool {
        self.checks.get(&id).copied().unwrap_or(false)
    }

    /// A row of the column: icon, label, and at its right a chevron, a check or the key.
    fn row(
        ui: &mut egui::Ui,
        selected: bool,
        icon: &str,
        label: &str,
        right: &str,
        colour: egui::Color32,
    ) -> egui::Response {
        let (rect, response) =
            ui.allocate_exact_size(egui::vec2(ui.available_width(), 32.0), egui::Sense::click());
        if selected || response.hovered() {
            ui.painter()
                .rect_filled(rect.shrink2(egui::vec2(2.0, 1.0)), 8.0, HOVER);
        }
        let middle = rect.center().y;
        let font = egui::FontId::proportional(14.0);
        ui.painter().text(
            egui::pos2(rect.left() + 18.0, middle),
            egui::Align2::CENTER_CENTER,
            icon,
            font.clone(),
            if colour == DANGER { DANGER } else { GOLD },
        );
        ui.painter().text(
            egui::pos2(rect.left() + 36.0, middle),
            egui::Align2::LEFT_CENTER,
            label,
            font,
            colour,
        );
        if !right.is_empty() {
            ui.painter().text(
                egui::pos2(rect.right() - 10.0, middle),
                egui::Align2::RIGHT_CENTER,
                right,
                egui::FontId::proportional(12.0),
                MUTED,
            );
        }
        response
    }
}

impl View for CenterView {
    fn builder(&self) -> egui::ViewportBuilder {
        let builder = egui::ViewportBuilder::default()
            .with_title(format!("Queqiao - {}", tr("Control center")))
            .with_inner_size([600.0, 470.0])
            .with_resizable(false)
            .with_decorations(false)
            .with_always_on_top()
            .with_taskbar(false);
        match self.position {
            Some(position) => builder.with_position(position),
            None => builder,
        }
    }

    fn draw(&mut self, ctx: &egui::Context) -> bool {
        let keep = self.draw_center(ctx);
        if !keep {
            self.alive.store(false, Ordering::Relaxed);
        }
        keep
    }

    fn closed(&mut self) {
        self.alive.store(false, Ordering::Relaxed);
    }
}

impl CenterView {
    fn draw_center(&mut self, ctx: &egui::Context) -> bool {
        if self.done {
            return false;
        }
        let focused = ctx.input(|input| input.viewport().focused).unwrap_or(true);
        if focused {
            self.focused_once = true;
        } else if self.focused_once {
            return false;
        }
        if ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
            return false;
        }
        let mut chosen: Option<Choice> = None;
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(INK).inner_margin(10.0))
            .show(ctx, |ui| {
                ui.style_mut().visuals.override_text_color = Some(TEXT);
                ui.style_mut().spacing.item_spacing.y = 2.0;
                ui.columns(2, |columns| {
                    // The column: groups, then full screen and end.
                    let items = self.items.clone();
                    for (index, item) in items.iter().enumerate() {
                        match item {
                            PanelItem::Group { icon, label, .. } => {
                                let response = Self::row(
                                    &mut columns[0],
                                    self.open == Some(index),
                                    icon.symbol(),
                                    label,
                                    "›",
                                    TEXT,
                                );
                                if response.hovered() || response.clicked() {
                                    self.open = Some(index);
                                }
                            }
                            PanelItem::Action {
                                id,
                                icon,
                                label,
                                shortcut,
                                danger,
                                enabled,
                                ..
                            } => {
                                let colour = if !*enabled {
                                    DIM
                                } else if *danger {
                                    DANGER
                                } else {
                                    TEXT
                                };
                                let response = Self::row(
                                    &mut columns[0],
                                    false,
                                    icon.symbol(),
                                    label,
                                    shortcut,
                                    colour,
                                );
                                if response.hovered() {
                                    self.open = None;
                                }
                                if response.clicked() && *enabled {
                                    chosen = Some(Choice::Menu(*id));
                                }
                            }
                            PanelItem::Separator => {
                                columns[0].separator();
                            }
                            PanelItem::Heading(label) => {
                                columns[0].label(egui::RichText::new(label).small().color(MUTED));
                            }
                            PanelItem::Slider { .. } => {}
                        }
                    }
                    // The open group's choices.
                    let group = match self.open.and_then(|index| self.items.get(index)) {
                        Some(PanelItem::Group { items, .. }) => items.clone(),
                        _ => Vec::new(),
                    };
                    let ui = &mut columns[1];
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        for item in &group {
                            match item {
                                PanelItem::Heading(label) => {
                                    ui.add_space(4.0);
                                    ui.label(
                                        egui::RichText::new(label).small().strong().color(MUTED),
                                    );
                                }
                                PanelItem::Separator => {
                                    ui.separator();
                                }
                                PanelItem::Slider { id, label, value } => {
                                    let mut level = *value;
                                    ui.label(label.as_str());
                                    let response =
                                        ui.add(egui::Slider::new(&mut level, 0..=100).suffix(" %"));
                                    if response.changed() {
                                        crate::panel::set_value(&mut self.items, *id, level);
                                        chosen = Some(Choice::Value(*id, level));
                                    }
                                }
                                PanelItem::Action {
                                    id,
                                    label,
                                    detail,
                                    shortcut,
                                    mark,
                                    danger,
                                    enabled,
                                    ..
                                } => {
                                    let on = self.checked(*id);
                                    let mark_text = match mark {
                                        Mark::Check if on => "✔ ",
                                        Mark::Radio if on => "● ",
                                        Mark::Check | Mark::Radio => "    ",
                                        Mark::None => "",
                                    };
                                    let colour = if !*enabled {
                                        DIM
                                    } else if *danger {
                                        DANGER
                                    } else {
                                        TEXT
                                    };
                                    let mut text =
                                        egui::RichText::new(format!("{mark_text}{label}"))
                                            .color(colour);
                                    if on && *mark != Mark::None {
                                        text = text.strong();
                                    }
                                    let response = ui.add_enabled(
                                        *enabled,
                                        egui::Button::new(text).frame(false),
                                    );
                                    if !shortcut.is_empty() {
                                        ui.label(
                                            egui::RichText::new(shortcut).small().color(MUTED),
                                        );
                                    }
                                    if !detail.is_empty() {
                                        ui.label(egui::RichText::new(detail).small().color(MUTED));
                                    }
                                    if response.clicked() {
                                        chosen = Some(Choice::Menu(*id));
                                    }
                                }
                                PanelItem::Group { .. } => {}
                            }
                        }
                    });
                });
            });
        if let Some(choice) = chosen {
            self.choose(choice);
        }
        !self.done
    }
}

/// The control center while it is open: choices come from `choices`.
pub(crate) struct ControlCenter {
    _opened: Opened,
    choices: Arc<Mutex<Vec<Choice>>>,
    /// False once the window closed (a choice, Esc, a click elsewhere).
    alive: Arc<AtomicBool>,
}

impl ControlCenter {
    /// Opens it with `items` (check marks from `checks`); `position`: its top left on the screen, where known;
    /// `group`: open from the start (tests).
    pub(crate) fn open(
        items: Vec<PanelItem>,
        checks: HashMap<u16, bool>,
        position: Option<[f32; 2]>,
        group: Option<usize>,
    ) -> Result<ControlCenter> {
        let choices = Arc::new(Mutex::new(Vec::new()));
        let alive = Arc::new(AtomicBool::new(true));
        let view = CenterView {
            items,
            checks,
            open: group,
            position,
            choices: choices.clone(),
            alive: alive.clone(),
            focused_once: false,
            done: false,
        };
        Ok(ControlCenter {
            _opened: gui::open(Box::new(view))?,
            choices,
            alive,
        })
    }

    /// The window as a handle (tests: its picture).
    pub(crate) fn hwnd(&self) -> super::window::Hwnd {
        self._opened.hwnd()
    }

    /// Still open (not closed by a choice, Esc or a click elsewhere).
    pub(crate) fn is_open(&self) -> bool {
        self.alive.load(Ordering::Relaxed)
    }

    pub(crate) fn take(&self) -> Vec<Choice> {
        self.choices
            .lock()
            .map(|mut choices| std::mem::take(&mut *choices))
            .unwrap_or_default()
    }
}
