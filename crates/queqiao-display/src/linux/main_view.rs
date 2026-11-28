//! Queqiao's main window on Linux, drawn as Windows draws it (main_window.rs), measure for measure: the night-blue
//! sidebar (the magpie and the name, the account's devices, Help, Transfers and Settings, the account at the bottom)
//! and a white page beside it. A device's page has its state, a card of a night sky joining this PC to it by a bridge
//! of magpies (Connect, or Wake up when it is offline), its tools and the latest connections to it; Help, Transfers
//! and Settings (tabs of cards, settings.rs) as on Windows. Before 2026-11-26 egui's own dark widgets drew it.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

use eframe::egui;
use egui::{Align2, Color32, CornerRadius, Pos2, Rect, Stroke, StrokeKind, Vec2};
use queqiao_core::text::tr;

use super::gui::View;
use super::presence::{Action, Shown};
use super::theme::{self, Icon};
use crate::model::{AccountState, MainDevice, Page};
use crate::settings::{Control, Tab};
use crate::tray::{MENU_FIXED_CODE, MENU_HELP};

// Layout, as Windows' (96-DPI units are egui's points). WIDTH x HEIGHT is the window's first size and its smallest:
// dragged larger, the page's width (`content`) and the height (`height`) follow it.
const WIDTH: f32 = 880.0;
const HEIGHT: f32 = 580.0;
const SIDEBAR: f32 = 216.0;
/// The page's left margin.
const LEFT: f32 = 32.0;
const NAV_TOP_BASE: f32 = 318.0;

thread_local! {
    /// The window's size this frame (points), never below WIDTH x HEIGHT.
    static SIZE: std::cell::Cell<(f32, f32)> = const { std::cell::Cell::new((WIDTH, HEIGHT)) };
}

/// The page content's width (600 at the first size).
fn content() -> f32 {
    SIZE.get().0 - SIDEBAR - LEFT - 32.0
}

fn height() -> f32 {
    SIZE.get().1
}

/// How much taller than at first the window is: the device list takes it, the page links move down by it.
fn taller() -> f32 {
    SIZE.get().1 - HEIGHT
}

fn nav_top() -> f32 {
    NAV_TOP_BASE + taller()
}
const NAV_STEP: f32 = 40.0;
const DEVICE_ROW: f32 = 36.0;
fn hero() -> [f32; 4] {
    [LEFT, 92.0, content(), 180.0]
}

fn actions() -> [f32; 4] {
    [LEFT, 288.0, content(), 52.0]
}
const RECENT_Y: f32 = 360.0;
/// The "being controlled" banner under the heading, and how far it moves the rest of the page down (as Windows').
fn banner() -> [f32; 4] {
    [LEFT, 90.0, content(), 46.0]
}
const BANNER_SPACE: f32 = 58.0;
const BANNER_BACK: Color32 = Color32::from_rgb(0xEA, 0xF2, 0xFE);
const BANNER_EDGE: Color32 = Color32::from_rgb(0xC6, 0xDC, 0xF6);
const BIRD: f32 = 36.0;

/// The magpie pictures, made once the window is drawn first.
struct Pictures {
    logo: Option<egui::TextureHandle>,
    bird: Option<egui::TextureHandle>,
}

pub(super) struct MainView {
    shown: Arc<Mutex<Shown>>,
    sender: Sender<Action>,
    /// The title the window has (tr("Queqiao") in the language it was set in).
    title: &'static str,
    help_id: String,
    code: String,
    selected: Option<String>,
    /// More...'s menu is open (for the chosen device).
    more_open: bool,
    pictures: Option<Pictures>,
    /// Shared with Presence: false once closed.
    open: Arc<AtomicBool>,
}

impl MainView {
    pub(super) fn new(
        shown: Arc<Mutex<Shown>>,
        sender: Sender<Action>,
        open: Arc<AtomicBool>,
    ) -> MainView {
        MainView {
            shown,
            sender,
            title: tr("Queqiao"),
            help_id: String::new(),
            code: String::new(),
            selected: None,
            more_open: false,
            pictures: None,
            open,
        }
    }

    fn send(&self, action: Action) {
        let _ = self.sender.send(action);
    }

    fn set_page(&self, page: Page) {
        if let Ok(mut shown) = self.shown.lock() {
            shown.page = page;
        }
    }

    /// The chosen device: the one clicked, else the first other device (this PC is listed first).
    fn chosen<'a>(&self, shown: &'a Shown) -> Option<&'a MainDevice> {
        self.selected
            .as_ref()
            .and_then(|id| shown.devices.iter().find(|device| &device.id == id))
            .or_else(|| shown.devices.iter().find(|device| !device.this_pc))
            .or_else(|| shown.devices.first())
    }

    fn sidebar(&mut self, ui: &mut egui::Ui, shown: &Shown) {
        let side = ui.max_rect();
        let at = |x: f32, y: f32, w: f32, h: f32| {
            Rect::from_min_size(side.min + Vec2::new(x, y), Vec2::new(w, h))
        };
        let painter = ui.painter().clone();
        painter.rect_filled(side, CornerRadius::ZERO, theme::SIDE_BG);
        // The magpie and the name.
        let logo = at(22.0, 12.0, 36.0, 36.0);
        painter.rect_filled(logo, CornerRadius::same(8), theme::WHITE);
        if let Some(texture) = self.pictures.as_ref().and_then(|p| p.logo.as_ref()) {
            painter.image(
                texture.id(),
                logo.shrink(3.0),
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                Color32::WHITE,
            );
        }
        theme::text_in(
            &painter,
            at(66.0, 12.0, 140.0, 36.0),
            Align2::LEFT_CENTER,
            tr("Queqiao"),
            theme::bold(19.0),
            theme::WHITE,
        );
        // My devices, and their count.
        let label = at(18.0, 62.0, 186.0, 20.0);
        theme::text_in(
            &painter,
            label,
            Align2::LEFT_CENTER,
            tr("My devices"),
            theme::regular(12.0),
            theme::SIDE_MUTED,
        );
        if !shown.devices.is_empty() {
            theme::text_in(
                &painter,
                label,
                Align2::RIGHT_CENTER,
                &shown.devices.len().to_string(),
                theme::regular(12.0),
                theme::SIDE_MUTED,
            );
        }
        let list = at(10.0, 84.0, SIDEBAR - 20.0, 216.0 + taller());
        let chosen = (shown.page == Page::Devices)
            .then(|| self.chosen(shown).map(|device| device.id.clone()))
            .flatten();
        let mut clicked = None;
        ui.scope_builder(egui::UiBuilder::new().max_rect(list), |ui| {
            egui::ScrollArea::vertical()
                .id_salt("devices")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing = Vec2::ZERO;
                    for device in &shown.devices {
                        let (row, response) = ui.allocate_exact_size(
                            Vec2::new(list.width(), DEVICE_ROW),
                            egui::Sense::click(),
                        );
                        let response = response.on_hover_cursor(egui::CursorIcon::PointingHand);
                        let painter = ui.painter();
                        let selected = chosen.as_deref() == Some(device.id.as_str());
                        if selected || response.hovered() {
                            painter.rect_filled(
                                row.shrink2(Vec2::new(0.0, 2.0)),
                                CornerRadius::same(6),
                                theme::SIDE_SELECTED,
                            );
                        }
                        if selected {
                            painter.rect_filled(
                                Rect::from_min_size(
                                    row.min + Vec2::new(0.0, 8.0),
                                    Vec2::new(3.0, 20.0),
                                ),
                                CornerRadius::same(2),
                                theme::GOLD,
                            );
                        }
                        let dot = if device.online || device.this_pc {
                            theme::ONLINE
                        } else {
                            theme::OFFLINE
                        };
                        painter.rect_filled(
                            Rect::from_center_size(
                                Pos2::new(row.left() + 18.0, row.center().y),
                                Vec2::splat(5.0),
                            ),
                            CornerRadius::same(1),
                            dot,
                        );
                        let mut name_width = row.width() - 44.0;
                        if device.this_pc {
                            let chip_text = tr("This PC");
                            let galley = theme::fitted(
                                painter,
                                chip_text,
                                theme::regular(12.0),
                                theme::SIDE_TEXT,
                                80.0,
                            );
                            let chip = Rect::from_min_size(
                                Pos2::new(
                                    row.right() - galley.size().x - 18.0,
                                    row.center().y - 10.0,
                                ),
                                Vec2::new(galley.size().x + 14.0, 20.0),
                            );
                            painter.rect_filled(chip, CornerRadius::same(10), theme::SIDE_CHIP);
                            painter.galley(
                                chip.center() - galley.size() / 2.0,
                                galley,
                                theme::SIDE_TEXT,
                            );
                            name_width = chip.left() - row.left() - 36.0;
                        }
                        if !device.controlled_by.is_empty() {
                            // Being controlled (as UU Remote's list): a screen and the one device controlling it.
                            let chip = Rect::from_min_size(
                                Pos2::new(
                                    row.left() + 30.0 + name_width - 34.0,
                                    row.center().y - 10.0,
                                ),
                                Vec2::new(34.0, 20.0),
                            );
                            painter.rect_filled(chip, CornerRadius::same(8), theme::SIDE_CHIP);
                            theme::icon(
                                painter,
                                Icon::Devices,
                                Rect::from_center_size(
                                    Pos2::new(chip.left() + 11.0, chip.center().y),
                                    Vec2::splat(12.0),
                                ),
                                theme::GOLD,
                            );
                            theme::text_in(
                                painter,
                                Rect::from_min_max(
                                    Pos2::new(chip.left() + 18.0, chip.top()),
                                    chip.right_bottom(),
                                ),
                                Align2::CENTER_CENTER,
                                "1",
                                theme::regular(12.0),
                                theme::SIDE_TEXT,
                            );
                            name_width -= 40.0;
                        }
                        theme::text_in(
                            painter,
                            Rect::from_min_size(
                                Pos2::new(row.left() + 30.0, row.top()),
                                Vec2::new(name_width, row.height()),
                            ),
                            Align2::LEFT_CENTER,
                            &device.name,
                            theme::regular(14.0),
                            if device.online || device.this_pc {
                                theme::SIDE_TEXT
                            } else {
                                theme::SIDE_MUTED
                            },
                        );
                        if response.clicked() {
                            clicked = Some(device.id.clone());
                        }
                    }
                });
        });
        if let Some(id) = clicked {
            self.selected = Some(id);
            self.more_open = false;
            self.set_page(Page::Devices);
        }
        // Help, Transfers, Settings.
        painter.line_segment(
            [
                side.min + Vec2::new(18.0, nav_top() - 12.0),
                side.min + Vec2::new(SIDEBAR - 18.0, nav_top() - 12.0),
            ],
            Stroke::new(1.0_f32, theme::SIDE_LINE),
        );
        for (index, (page, glyph)) in [
            (Page::Help, Icon::Help),
            (Page::Transfers, Icon::Transfers),
            (Page::Settings, Icon::Settings),
        ]
        .into_iter()
        .enumerate()
        {
            let row = at(
                10.0,
                nav_top() + index as f32 * NAV_STEP,
                SIDEBAR - 20.0,
                36.0,
            );
            let response = theme::hit(ui, row, ("nav", index));
            let selected = shown.page == page;
            if selected || response.hovered() {
                painter.rect_filled(row, CornerRadius::same(6), theme::SIDE_SELECTED);
            }
            if selected {
                painter.rect_filled(
                    Rect::from_min_size(row.min + Vec2::new(0.0, 8.0), Vec2::new(3.0, 20.0)),
                    CornerRadius::same(2),
                    theme::GOLD,
                );
            }
            theme::icon(
                &painter,
                glyph,
                Rect::from_center_size(
                    Pos2::new(row.left() + 22.0, row.center().y),
                    Vec2::splat(18.0),
                ),
                theme::SIDE_TEXT,
            );
            theme::text_in(
                &painter,
                Rect::from_min_size(
                    row.min + Vec2::new(46.0, 0.0),
                    Vec2::new(row.width() - 50.0, row.height()),
                ),
                Align2::LEFT_CENTER,
                page.title(),
                theme::regular(14.0),
                if selected {
                    theme::WHITE
                } else {
                    theme::SIDE_TEXT
                },
            );
            if response.clicked() {
                self.more_open = false;
                self.set_page(page);
            }
        }
        // The account: a gold circle with the initial, the name, the state, Log in or out.
        painter.line_segment(
            [
                side.min + Vec2::new(18.0, height() - 82.0),
                side.min + Vec2::new(SIDEBAR - 18.0, height() - 82.0),
            ],
            Stroke::new(1.0_f32, theme::SIDE_LINE),
        );
        let area = at(12.0, height() - 70.0, SIDEBAR - 24.0, 58.0);
        let response = theme::hit(ui, area, "account");
        let center = Pos2::new(area.left() + 24.0, area.center().y);
        painter.circle_filled(center, 17.0, theme::GOLD);
        let (name, initial) = match shown.account {
            AccountState::LoggedOut => (tr("Not logged in").to_string(), "?".to_string()),
            _ => (
                shown.user.clone(),
                shown
                    .user
                    .chars()
                    .next()
                    .map(|c| c.to_uppercase().to_string())
                    .unwrap_or_default(),
            ),
        };
        painter.text(
            center,
            Align2::CENTER_CENTER,
            initial,
            theme::bold(15.0),
            theme::ON_GOLD,
        );
        let x = area.left() + 52.0;
        theme::text_in(
            &painter,
            Rect::from_min_size(
                Pos2::new(x, area.top() + 2.0),
                Vec2::new(area.right() - x, 18.0),
            ),
            Align2::LEFT_CENTER,
            &name,
            theme::bold(14.0),
            theme::WHITE,
        );
        let (state, dot) = match shown.account {
            AccountState::Online => (tr("Online"), theme::ONLINE),
            AccountState::Offline => (tr("Cannot reach the server"), theme::OFFLINE),
            AccountState::LoggedOut => ("", theme::OFFLINE),
        };
        if !state.is_empty() {
            painter.circle_filled(Pos2::new(x + 3.0, area.top() + 29.0), 3.0, dot);
            theme::text_in(
                &painter,
                Rect::from_min_size(
                    Pos2::new(x + 10.0, area.top() + 20.0),
                    Vec2::new(area.right() - x - 10.0, 18.0),
                ),
                Align2::LEFT_CENTER,
                state,
                theme::regular(12.0),
                if dot == theme::ONLINE {
                    theme::ONLINE
                } else {
                    theme::SIDE_MUTED
                },
            );
        }
        let link = if shown.account == AccountState::LoggedOut {
            tr("Log in")
        } else {
            tr("Log out")
        };
        theme::text_in(
            &painter,
            Rect::from_min_size(
                Pos2::new(x, area.top() + 37.0),
                Vec2::new(area.right() - x, 18.0),
            ),
            Align2::LEFT_CENTER,
            link,
            theme::regular(12.0),
            theme::GOLD,
        );
        if response.clicked() {
            self.send(Action::Account);
        }
    }

    /// A page's title and the line under it (Help and Transfers).
    fn heading(&self, painter: &egui::Painter, page: Rect, title: &str, subtitle: &str) {
        let at = |y: f32, h: f32| {
            Rect::from_min_size(page.min + Vec2::new(LEFT, y), Vec2::new(content(), h))
        };
        theme::text_in(
            painter,
            at(24.0, 40.0),
            Align2::LEFT_CENTER,
            title,
            theme::bold(26.0),
            theme::TEXT,
        );
        theme::text_in(
            painter,
            at(64.0, 22.0),
            Align2::LEFT_CENTER,
            subtitle,
            theme::regular(12.0),
            theme::SECONDARY,
        );
    }

    fn devices(&mut self, ui: &mut egui::Ui, page: Rect, shown: &Shown) {
        let at = |x: f32, y: f32, w: f32, h: f32| {
            Rect::from_min_size(page.min + Vec2::new(x, y), Vec2::new(w, h))
        };
        let painter = ui.painter().clone();
        if shown.account == AccountState::LoggedOut && shown.devices.is_empty() {
            self.heading(
                &painter,
                page,
                Page::Devices.title(),
                tr("Log in to see your devices."),
            );
            if theme::gold_button(ui, at(LEFT, 182.0, 140.0, 38.0), tr("Log in")) {
                self.send(Action::Account);
            }
            return;
        }
        let Some(device) = self.chosen(shown).cloned() else {
            self.heading(
                &painter,
                page,
                Page::Devices.title(),
                tr("No devices yet: log in to this account on another PC, and it appears here."),
            );
            return;
        };
        // The pill (this PC, online, offline) and the name.
        let (pill, colors) = if device.this_pc {
            (tr("This PC"), theme::PILL_SELF)
        } else if device.online {
            (tr("Online"), theme::PILL_ON)
        } else {
            (tr("Offline"), theme::PILL_OFF)
        };
        let galley = theme::fitted(&painter, pill, theme::regular(12.0), colors.1, 120.0);
        let pill_rect = at(LEFT, 32.0, galley.size().x + 26.0, 24.0);
        painter.rect_filled(pill_rect, CornerRadius::same(12), colors.0);
        painter.circle_filled(
            Pos2::new(pill_rect.left() + 10.0, pill_rect.center().y),
            3.0,
            colors.1,
        );
        painter.galley(
            Pos2::new(
                pill_rect.left() + 17.0,
                pill_rect.center().y - galley.size().y / 2.0,
            ),
            galley,
            colors.1,
        );
        let name_x = pill_rect.right() - page.left() + 12.0;
        theme::text_in(
            &painter,
            at(name_x, 24.0, LEFT + content() - name_x, 40.0),
            Align2::LEFT_CENTER,
            &device.name,
            theme::bold(26.0),
            theme::TEXT,
        );
        let line = if device.this_pc {
            tr("This PC: your other devices can control it.")
        } else if device.online {
            tr("Online: you can connect now.")
        } else {
            tr("Offline: a device of yours on its network can wake it.")
        };
        theme::text_in(
            &painter,
            at(LEFT, 62.0, content(), 20.0),
            Align2::LEFT_CENTER,
            line,
            theme::regular(12.0),
            theme::SECONDARY,
        );
        // Another device of the account controls it (UU Remote's "被控中", the user, 2026-11-26): the banner, with
        // Disconnect; the rest of the page below it.
        let below = if device.controlled_by.is_empty() {
            0.0
        } else {
            self.banner(ui, page, &device);
            BANNER_SPACE
        };
        self.hero(ui, page, shown, &device, below);
        self.actions(ui, page, &device, below);
        // Recent connections.
        let recent_y = RECENT_Y + below;
        theme::text_in(
            &painter,
            at(LEFT, recent_y, content(), 24.0),
            Align2::LEFT_CENTER,
            tr("Recent connections"),
            theme::bold(16.0),
            theme::TEXT,
        );
        let rows: Vec<_> = shown
            .connections
            .iter()
            .filter(|row| row.device_id == device.id)
            // Three at the first size; more as the window grows taller.
            .take(3 + (taller() / 38.0).max(0.0) as usize)
            .collect();
        if rows.is_empty() {
            let empty = if device.this_pc {
                tr("Sessions from this PC to your other devices are listed on their pages.")
            } else {
                tr("No connections from this PC yet.")
            };
            theme::text_in(
                &painter,
                at(LEFT, recent_y + 32.0, content(), 20.0),
                Align2::LEFT_CENTER,
                empty,
                theme::regular(12.0),
                theme::SECONDARY,
            );
        }
        for (index, row) in rows.into_iter().enumerate() {
            let y = recent_y + 32.0 + index as f32 * 38.0;
            theme::icon(
                &painter,
                Icon::Devices,
                at(LEFT, y + 9.0, 20.0, 20.0),
                theme::SECONDARY,
            );
            theme::text_in(
                &painter,
                at(LEFT + 32.0, y, 300.0, 38.0),
                Align2::LEFT_CENTER,
                &row.when,
                theme::regular(14.0),
                theme::TEXT,
            );
            theme::text_in(
                &painter,
                at(LEFT + 300.0, y, content() - 300.0, 38.0),
                Align2::RIGHT_CENTER,
                &row.length,
                theme::regular(12.0),
                theme::SECONDARY,
            );
            painter.line_segment(
                [
                    page.min + Vec2::new(LEFT + 32.0, y + 38.0),
                    page.min + Vec2::new(LEFT + content(), y + 38.0),
                ],
                Stroke::new(1.0_f32, theme::DIVIDER),
            );
        }
    }

    /// The "being controlled" banner under the heading (as the Windows main window's): a pale blue band, a screen, the
    /// label, a divider, the sentence, Disconnect on the right.
    fn banner(&mut self, ui: &mut egui::Ui, page: Rect, device: &MainDevice) {
        let painter = ui.painter().clone();
        let at = |x: f32, y: f32, w: f32, h: f32| {
            Rect::from_min_size(page.min + Vec2::new(x, y), Vec2::new(w, h))
        };
        let [bx, by, bw, bh] = banner();
        let band = at(bx, by, bw, bh);
        painter.rect_filled(band, CornerRadius::same(10), BANNER_BACK);
        painter.rect_stroke(
            band,
            CornerRadius::same(10),
            Stroke::new(1.0_f32, BANNER_EDGE),
            StrokeKind::Inside,
        );
        let (label, words) = crate::model::controlled_banner();
        theme::icon(
            &painter,
            Icon::Devices,
            Rect::from_center_size(
                Pos2::new(band.left() + 26.0, band.center().y),
                Vec2::splat(18.0),
            ),
            theme::ACCENT,
        );
        let galley = theme::fitted(&painter, label, theme::bold(14.0), theme::ACCENT, 160.0);
        let label_right = band.left() + 46.0 + galley.size().x;
        painter.galley(
            Pos2::new(band.left() + 46.0, band.center().y - galley.size().y / 2.0),
            galley,
            theme::ACCENT,
        );
        painter.line_segment(
            [
                Pos2::new(label_right + 12.0, band.top() + 14.0),
                Pos2::new(label_right + 12.0, band.bottom() - 14.0),
            ],
            Stroke::new(1.0_f32, BANNER_EDGE),
        );
        let end = Rect::from_min_size(
            Pos2::new(band.right() - 114.0, band.center().y - 14.0),
            Vec2::new(102.0, 28.0),
        );
        // The sentence on one line where it fits (Chinese), else wrapped onto two, centred in the band.
        let area = Rect::from_min_max(
            Pos2::new(label_right + 26.0, band.top()),
            Pos2::new(end.left() - 10.0, band.bottom()),
        );
        let galley = painter.layout(
            words.to_string(),
            theme::regular(12.0),
            theme::ACCENT,
            area.width(),
        );
        painter.galley(
            Pos2::new(area.left(), area.center().y - galley.size().y / 2.0),
            galley,
            theme::ACCENT,
        );
        if theme::button(ui, end, tr("Disconnect"), true) {
            self.send(Action::EndControl(device.id.clone()));
        }
    }

    /// The card: a night sky, this PC and the device joined by a dotted arch, three magpies on it while the device is
    /// online (the bridge forms), the gold button under it. `below`: lower by this (the banner above it).
    fn hero(
        &mut self,
        ui: &mut egui::Ui,
        page: Rect,
        shown: &Shown,
        device: &MainDevice,
        below: f32,
    ) {
        let painter = ui.painter().clone();
        let card = Rect::from_min_size(
            page.min + Vec2::new(hero()[0], hero()[1] + below),
            Vec2::new(hero()[2], hero()[3]),
        );
        // The gradient: a mesh from the top colour to the bottom one, then the corners rounded off with the page's white.
        let mut mesh = egui::Mesh::default();
        mesh.colored_vertex(card.left_top(), theme::HERO_TOP);
        mesh.colored_vertex(card.right_top(), theme::HERO_TOP);
        mesh.colored_vertex(card.right_bottom(), theme::HERO_BOTTOM);
        mesh.colored_vertex(card.left_bottom(), theme::HERO_BOTTOM);
        mesh.add_triangle(0, 1, 2);
        mesh.add_triangle(0, 2, 3);
        painter.add(egui::Shape::mesh(mesh));
        round_corners(&painter, card, 12.0, theme::WHITE);
        // Stars: the same scatter every time.
        let mut seed: u32 = 2_166_136_261;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        };
        let stars = [
            Color32::from_rgb(0xEA, 0xEE, 0xF5),
            Color32::from_rgb(0xC0, 0xC6, 0xD6),
            Color32::from_rgb(0x74, 0x7F, 0x95),
        ];
        for _ in 0..46 {
            let x = card.left() + 12.0 + (next() % (card.width() - 24.0) as u32) as f32;
            let y = card.top() + 12.0 + (next() % (card.height() - 24.0) as u32) as f32;
            let size = if next() % 3 == 0 { 2.0 } else { 1.0 };
            let color = stars[(next() % 3) as usize];
            painter.rect_filled(
                Rect::from_min_size(Pos2::new(x, y), Vec2::splat(size)),
                CornerRadius::ZERO,
                color,
            );
        }
        let this_name = shown
            .devices
            .iter()
            .find(|device| device.this_pc)
            .map_or(tr("This PC").to_string(), |device| device.name.clone());
        let end = |x: f32, color: Color32, name: &str, under: &str| {
            theme::icon(
                &painter,
                Icon::Devices,
                Rect::from_center_size(Pos2::new(x, card.top() + 54.0), Vec2::splat(30.0)),
                color,
            );
            theme::text_in(
                &painter,
                Rect::from_center_size(Pos2::new(x, card.top() + 86.0), Vec2::new(160.0, 20.0)),
                Align2::CENTER_CENTER,
                name,
                theme::bold(14.0),
                color,
            );
            theme::text_in(
                &painter,
                Rect::from_center_size(Pos2::new(x, card.top() + 105.0), Vec2::new(160.0, 18.0)),
                Align2::CENTER_CENTER,
                under,
                theme::regular(12.0),
                theme::HERO_MUTED,
            );
        };
        if device.this_pc {
            end(card.center().x, theme::WHITE, &this_name, tr("This PC"));
            theme::text_in(
                &painter,
                Rect::from_min_size(
                    Pos2::new(card.left() + 20.0, card.bottom() - 44.0),
                    Vec2::new(card.width() - 40.0, 22.0),
                ),
                Align2::CENTER_CENTER,
                tr("Your other devices connect to it from their own Queqiao."),
                theme::regular(12.0),
                theme::HERO_MUTED,
            );
            return;
        }
        let (left_x, right_x) = (card.left() + 92.0, card.right() - 92.0);
        end(left_x, theme::WHITE, &this_name, tr("This PC"));
        let (color, status) = if device.online {
            (theme::WHITE, tr("Online"))
        } else {
            (theme::HERO_FAINT, tr("Offline"))
        };
        end(right_x, color, &device.name, status);
        // The arch: dotted, from beside one PC to beside the other, its top near the card's top.
        let (x0, y0) = (left_x + 52.0, card.top() + 60.0);
        let (x2, y2) = (right_x - 52.0, y0);
        let (x1, y1) = ((x0 + x2) / 2.0, card.top() + 6.0);
        let along = |t: f32| {
            let u = 1.0 - t;
            Pos2::new(
                u * u * x0 + 2.0 * u * t * x1 + t * t * x2,
                u * u * y0 + 2.0 * u * t * y1 + t * t * y2,
            )
        };
        let arch = if device.online {
            theme::GOLD
        } else {
            theme::HERO_FAINT
        };
        for k in 0..=34 {
            painter.circle_filled(along(k as f32 / 34.0), 1.1, arch);
        }
        if device.online {
            for t in [0.3, 0.5, 0.7] {
                let at = along(t);
                match self.pictures.as_ref().and_then(|p| p.bird.as_ref()) {
                    Some(bird) => {
                        painter.image(
                            bird.id(),
                            Rect::from_center_size(at - Vec2::new(0.0, 4.0), Vec2::splat(BIRD)),
                            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                            Color32::WHITE,
                        );
                    }
                    None => {
                        painter.circle_filled(at, 4.0, theme::GOLD);
                    }
                }
            }
        }
        let button = Rect::from_min_size(
            Pos2::new(card.center().x - 70.0, card.bottom() - 54.0),
            Vec2::new(140.0, 38.0),
        );
        let label = if device.online {
            tr("Connect")
        } else {
            tr("Wake up")
        };
        if theme::gold_button(ui, button, label) {
            self.send(if device.online {
                Action::Connect(device.id.clone())
            } else {
                Action::Wake(device.id.clone())
            });
        }
    }

    /// The tools under the card: Send files, Command line, Wake up, More (each where it can be used now).
    fn actions(&mut self, ui: &mut egui::Ui, page: Rect, device: &MainDevice, below: f32) {
        let row = Rect::from_min_size(
            page.min + Vec2::new(actions()[0], actions()[1] + below),
            Vec2::new(actions()[2], actions()[3]),
        );
        let painter = ui.painter().clone();
        painter.rect_filled(row, CornerRadius::same(10), theme::WHITE);
        painter.rect_stroke(
            row,
            CornerRadius::same(10),
            Stroke::new(1.0_f32, theme::DIVIDER),
            StrokeKind::Inside,
        );
        let other = !device.this_pc;
        let cells = [
            (Icon::Files, tr("Send files"), other && device.online),
            (Icon::Terminal, tr("Command line"), other && device.online),
            (Icon::Wake, tr("Wake up"), other && !device.online),
            (Icon::More, tr("More"), other),
        ];
        let width = row.width() / cells.len() as f32;
        for (index, (glyph, label, enabled)) in cells.into_iter().enumerate() {
            let cell = Rect::from_min_size(
                row.min + Vec2::new(index as f32 * width, 0.0),
                Vec2::new(width, row.height()),
            );
            if index > 0 {
                painter.line_segment(
                    [
                        cell.left_top() + Vec2::new(0.0, 14.0),
                        cell.left_bottom() - Vec2::new(0.0, 14.0),
                    ],
                    Stroke::new(1.0_f32, theme::DIVIDER),
                );
            }
            let response = if enabled {
                theme::hit(ui, cell, ("action", index))
            } else {
                ui.interact(cell, ui.id().with(("action", index)), egui::Sense::hover())
            };
            if enabled && response.hovered() {
                painter.rect_filled(cell.shrink(4.0), CornerRadius::same(8), theme::CARD);
            }
            let color = if enabled {
                theme::TEXT
            } else {
                theme::DISABLED
            };
            let galley = theme::fitted(&painter, label, theme::regular(14.0), color, width - 50.0);
            let total = 18.0 + 10.0 + galley.size().x;
            let x = cell.center().x - total / 2.0;
            theme::icon(
                &painter,
                glyph,
                Rect::from_center_size(Pos2::new(x + 9.0, cell.center().y), Vec2::splat(18.0)),
                if enabled {
                    theme::ACCENT
                } else {
                    theme::DISABLED
                },
            );
            painter.galley(
                Pos2::new(x + 28.0, cell.center().y - galley.size().y / 2.0),
                galley,
                color,
            );
            if enabled && response.clicked() {
                match glyph {
                    Icon::Files => self.send(Action::Files(device.id.clone())),
                    Icon::Terminal => self.send(Action::Terminal(device.id.clone())),
                    Icon::Wake => self.send(Action::Wake(device.id.clone())),
                    _ => self.more_open = !self.more_open,
                }
            }
        }
        // More...'s menu, under its cell.
        if self.more_open && other {
            let anchor = Pos2::new(row.right() - width, row.bottom() + 4.0);
            let area = egui::Area::new(ui.id().with("more"))
                .order(egui::Order::Foreground)
                .fixed_pos(anchor)
                .show(ui.ctx(), |ui| {
                    egui::Frame::new()
                        .fill(theme::WHITE)
                        .stroke(Stroke::new(1.0_f32, theme::DIVIDER))
                        .corner_radius(8)
                        .inner_margin(6.0)
                        .show(ui, |ui| {
                            theme::light(ui);
                            if ui.button(tr("Remove from this account...")).clicked() {
                                self.send(Action::Remove(device.id.clone()));
                                self.more_open = false;
                            }
                        });
                });
            if area.response.clicked_elsewhere() {
                self.more_open = false;
            }
        }
    }

    fn help(&mut self, ui: &mut egui::Ui, page: Rect, shown: &Shown) {
        let at = |x: f32, y: f32, w: f32, h: f32| {
            Rect::from_min_size(page.min + Vec2::new(x, y), Vec2::new(w, h))
        };
        let painter = ui.painter().clone();
        self.heading(&painter, page, Page::Help.title(), Page::Help.subtitle());
        theme::text_in(
            &painter,
            at(LEFT, 98.0, content(), 24.0),
            Align2::LEFT_CENTER,
            tr("Let someone help this PC"),
            theme::bold(16.0),
            theme::TEXT,
        );
        let card = at(LEFT, 128.0, content(), 132.0);
        painter.rect_filled(card, CornerRadius::same(12), theme::CARD);
        painter.rect_stroke(
            card,
            CornerRadius::same(12),
            Stroke::new(1.0_f32, theme::DIVIDER),
            StrokeKind::Inside,
        );
        match &shown.help {
            Some((help_id, code)) => {
                theme::text_in(
                    &painter,
                    at(LEFT + 20.0, 138.0, 260.0, 18.0),
                    Align2::LEFT_CENTER,
                    tr("Help ID"),
                    theme::regular(12.0),
                    theme::SECONDARY,
                );
                theme::text_in(
                    &painter,
                    at(LEFT + 20.0, 156.0, 280.0, 44.0),
                    Align2::LEFT_CENTER,
                    &queqiao_core::invitation::spaced_id(help_id),
                    theme::bold(30.0),
                    theme::TEXT,
                );
                theme::text_in(
                    &painter,
                    at(LEFT + 320.0, 138.0, 260.0, 18.0),
                    Align2::LEFT_CENTER,
                    tr("Code"),
                    theme::regular(12.0),
                    theme::SECONDARY,
                );
                theme::text_in(
                    &painter,
                    at(LEFT + 320.0, 156.0, 280.0, 44.0),
                    Align2::LEFT_CENTER,
                    &queqiao_core::invitation::halved_code(code),
                    theme::bold(30.0),
                    theme::ACCENT,
                );
                let buttons = [
                    (tr("Copy invitation"), Action::CopyInvitation),
                    (tr("New code"), Action::HelpNewCode),
                    (tr("New help ID"), Action::NewHelpId),
                    (tr("Stop"), Action::HelpClose),
                ];
                for (index, (label, action)) in buttons.into_iter().enumerate() {
                    if theme::button(
                        ui,
                        at(LEFT + 20.0 + index as f32 * 146.0, 212.0, 134.0, 34.0),
                        label,
                        true,
                    ) {
                        self.send(action);
                    }
                }
            }
            None => {
                let text = tr(
                    "Someone of another account can control this PC with its help ID and code, after you allow it each time. Command lines, files, restarting and the sign-in screen stay closed to them.",
                );
                let galley = painter.layout(
                    text.to_string(),
                    theme::regular(14.0),
                    theme::SECONDARY,
                    content() - 40.0,
                );
                painter.galley(
                    page.min + Vec2::new(LEFT + 20.0, 140.0),
                    galley,
                    theme::SECONDARY,
                );
                if theme::button(
                    ui,
                    at(LEFT + 20.0, 212.0, 200.0, 34.0),
                    tr("Let someone help me..."),
                    true,
                ) {
                    self.send(Action::Choose(MENU_HELP));
                }
                let fixed = if shown.settings.fixed_code {
                    tr("Remove the fixed help code")
                } else {
                    tr("Fixed help code...")
                };
                if theme::button(
                    ui,
                    at(LEFT + content() - 200.0, 212.0, 180.0, 34.0),
                    fixed,
                    true,
                ) {
                    self.send(Action::Choose(MENU_FIXED_CODE));
                }
            }
        }
        theme::text_in(
            &painter,
            at(LEFT, 290.0, content(), 24.0),
            Align2::LEFT_CENTER,
            tr("Help someone"),
            theme::bold(16.0),
            theme::TEXT,
        );
        theme::text_in(
            &painter,
            at(LEFT, 334.0, 220.0, 18.0),
            Align2::LEFT_CENTER,
            tr("Help ID"),
            theme::regular(12.0),
            theme::SECONDARY,
        );
        theme::text_in(
            &painter,
            at(LEFT + 236.0, 334.0, 220.0, 18.0),
            Align2::LEFT_CENTER,
            tr("Code"),
            theme::regular(12.0),
            theme::SECONDARY,
        );
        let id_response = ui.put(
            at(LEFT, 356.0, 220.0, 30.0),
            egui::TextEdit::singleline(&mut self.help_id)
                .font(theme::regular(14.0))
                .margin(Vec2::new(8.0, 6.0)),
        );
        let code_response = ui.put(
            at(LEFT + 236.0, 356.0, 220.0, 30.0),
            egui::TextEdit::singleline(&mut self.code)
                .font(theme::regular(14.0))
                .margin(Vec2::new(8.0, 6.0)),
        );
        // An invitation pasted into either field fills both.
        for typed in [self.help_id.clone(), self.code.clone()] {
            if let Some((help_id, code)) = queqiao_core::invitation::parse(&typed) {
                self.help_id = help_id;
                self.code = code;
            }
        }
        let enter = (id_response.lost_focus() || code_response.lost_focus())
            && ui.input(|input| input.key_pressed(egui::Key::Enter));
        if (theme::button(
            ui,
            at(LEFT + 472.0, 354.0, 128.0, 34.0),
            tr("Connect"),
            true,
        ) || enter)
            && !self.help_id.trim().is_empty()
        {
            self.send(Action::HelpSomeone {
                help_id: self.help_id.chars().filter(char::is_ascii_digit).collect(),
                code: crate::model::normalize_code(&self.code),
            });
        }
        let hint = tr(
            "Pasting an invitation into either field fills in both. The person there is asked before you see their screen.",
        );
        let galley = painter.layout(
            hint.to_string(),
            theme::regular(12.0),
            theme::SECONDARY,
            content(),
        );
        painter.galley(page.min + Vec2::new(LEFT, 396.0), galley, theme::SECONDARY);
    }

    fn transfers(&mut self, ui: &mut egui::Ui, page: Rect, shown: &Shown) {
        let at = |x: f32, y: f32, w: f32, h: f32| {
            Rect::from_min_size(page.min + Vec2::new(x, y), Vec2::new(w, h))
        };
        let painter = ui.painter().clone();
        self.heading(
            &painter,
            page,
            Page::Transfers.title(),
            Page::Transfers.subtitle(),
        );
        if shown.transfers.is_empty() {
            let text = tr(
                "Nothing yet: files sent in a session (drag them onto its window) and files received appear here.",
            );
            let galley = painter.layout(
                text.to_string(),
                theme::regular(14.0),
                theme::SECONDARY,
                content(),
            );
            painter.galley(page.min + Vec2::new(LEFT, 110.0), galley, theme::SECONDARY);
        }
        let list = at(LEFT, 100.0, content(), 372.0);
        ui.scope_builder(egui::UiBuilder::new().max_rect(list), |ui| {
            egui::ScrollArea::vertical()
                .id_salt("transfers")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing = Vec2::ZERO;
                    for row in &shown.transfers {
                        let (rect, _) = ui.allocate_exact_size(
                            Vec2::new(list.width(), 52.0),
                            egui::Sense::hover(),
                        );
                        let painter = ui.painter();
                        let square = Rect::from_min_size(
                            rect.min + Vec2::new(12.0, 10.0),
                            Vec2::splat(32.0),
                        );
                        painter.rect_filled(square, CornerRadius::same(6), theme::CARD);
                        theme::icon(
                            painter,
                            if row.received {
                                Icon::Received
                            } else {
                                Icon::Sent
                            },
                            square.shrink(8.0),
                            theme::ACCENT,
                        );
                        theme::text_in(
                            painter,
                            Rect::from_min_size(
                                rect.min + Vec2::new(58.0, 8.0),
                                Vec2::new(rect.width() - 70.0, 20.0),
                            ),
                            Align2::LEFT_CENTER,
                            &row.name,
                            theme::regular(14.0),
                            theme::TEXT,
                        );
                        theme::text_in(
                            painter,
                            Rect::from_min_size(
                                rect.min + Vec2::new(58.0, 28.0),
                                Vec2::new(rect.width() - 70.0, 18.0),
                            ),
                            Align2::LEFT_CENTER,
                            &row.detail,
                            theme::regular(12.0),
                            theme::SECONDARY,
                        );
                        painter.line_segment(
                            [
                                rect.left_bottom() + Vec2::new(58.0, -1.0),
                                rect.right_bottom() - Vec2::new(0.0, 1.0),
                            ],
                            Stroke::new(1.0_f32, theme::DIVIDER),
                        );
                    }
                });
        });
        if theme::button(
            ui,
            at(LEFT, 492.0, 180.0, 34.0),
            tr("Open received files"),
            true,
        ) {
            self.send(Action::OpenReceived);
        }
        if theme::button(
            ui,
            at(LEFT + 192.0, 492.0, 140.0, 34.0),
            tr("Clear the list"),
            !shown.transfers.is_empty(),
        ) {
            self.send(Action::ClearTransfers);
        }
    }

    /// Settings: the title, the tabs (a short gold line under the chosen one), then its cards, as on Windows.
    fn settings(&mut self, ui: &mut egui::Ui, page: Rect, shown: &Shown) {
        let at = |x: f32, y: f32, w: f32, h: f32| {
            Rect::from_min_size(page.min + Vec2::new(x, y), Vec2::new(w, h))
        };
        let painter = ui.painter().clone();
        theme::text_in(
            &painter,
            at(LEFT, 20.0, content(), 40.0),
            Align2::LEFT_CENTER,
            Page::Settings.title(),
            theme::bold(26.0),
            theme::TEXT,
        );
        let mut x = LEFT;
        for tab in Tab::ALL {
            let galley =
                theme::fitted(&painter, tab.title(), theme::bold(16.0), theme::TEXT, 200.0);
            let width = galley.size().x;
            let place = at(x, 66.0, width, 30.0);
            let response = theme::hit(ui, place.expand2(Vec2::new(8.0, 4.0)), ("tab", tab.title()));
            let chosen = tab == shown.tab;
            painter.galley(
                Pos2::new(place.left(), place.center().y - galley.size().y / 2.0),
                galley,
                if chosen {
                    theme::TEXT
                } else {
                    theme::SECONDARY
                },
            );
            if chosen {
                painter.rect_filled(
                    Rect::from_center_size(
                        Pos2::new(place.center().x, page.top() + 100.0),
                        Vec2::new(24.0, 4.0),
                    ),
                    CornerRadius::same(2),
                    theme::GOLD,
                );
            }
            if response.clicked()
                && let Ok(mut state) = self.shown.lock()
            {
                state.tab = tab;
            }
            x += width + 30.0;
        }
        painter.line_segment(
            [
                page.min + Vec2::new(LEFT, 106.0),
                page.min + Vec2::new(LEFT + content(), 106.0),
            ],
            Stroke::new(1.0_f32, theme::DIVIDER),
        );
        let area = at(LEFT, 112.0, content() + 16.0, height() - 124.0);
        let rows = crate::settings::rows(shown.tab, &shown.settings);
        ui.scope_builder(egui::UiBuilder::new().max_rect(area), |ui| {
            egui::ScrollArea::vertical()
                .id_salt(("settings", shown.tab.title()))
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.add_enabled_ui(!shown.locked, |ui| {
                        ui.spacing_mut().item_spacing = Vec2::new(8.0, 0.0);
                        let mut index = 0;
                        while index < rows.len() {
                            let mut end = index + 1;
                            while end < rows.len() && rows[end].sub {
                                end += 1;
                            }
                            if index > 0 {
                                ui.add_space(10.0);
                            }
                            egui::Frame::new()
                                .fill(theme::WHITE)
                                .stroke(Stroke::new(1.0_f32, theme::DIVIDER))
                                .corner_radius(12)
                                .show(ui, |ui| {
                                    ui.set_width(content());
                                    ui.spacing_mut().item_spacing = Vec2::ZERO;
                                    for (offset, row) in rows[index..end].iter().enumerate() {
                                        self.setting(ui, row, index + offset, offset > 0);
                                    }
                                });
                            index = end;
                        }
                        ui.add_space(8.0);
                    });
                });
        });
    }

    /// One card's row: its icon, title, the line under it, and its control on the right.
    fn setting(
        &mut self,
        ui: &mut egui::Ui,
        row: &crate::settings::Row,
        index: usize,
        line_above: bool,
    ) {
        let height = if row.detail.is_empty() { 52.0 } else { 68.0 };
        let (rect, _) = ui.allocate_exact_size(Vec2::new(content(), height), egui::Sense::hover());
        let painter = ui.painter().clone();
        if line_above {
            painter.line_segment(
                [
                    rect.left_top() + Vec2::new(56.0, 0.0),
                    rect.right_top() - Vec2::new(18.0, 0.0),
                ],
                Stroke::new(1.0_f32, theme::DIVIDER),
            );
        }
        if let Some(glyph) = settings_icon(row.glyph) {
            theme::icon(
                &painter,
                glyph,
                Rect::from_center_size(
                    Pos2::new(rect.left() + 30.0, rect.center().y),
                    Vec2::splat(18.0),
                ),
                theme::TEXT,
            );
        }
        let right = rect.right() - 18.0;
        // The control, laid out from the right; the title stops before it.
        let control_width = match &row.control {
            Control::Switch { .. } => 84.0,
            Control::Choice { .. } => 200.0,
            Control::Buttons(buttons) => buttons.len() as f32 * 150.0,
            Control::Keys { keys, .. } => 40.0 + keys.len() as f32 * 56.0,
        };
        let title_width = (right - control_width - 16.0 - (rect.left() + 56.0)).max(80.0);
        let (title_y, detail_y) = if row.detail.is_empty() {
            (rect.center().y, 0.0)
        } else {
            (rect.top() + 24.0, rect.top() + 46.0)
        };
        theme::text_in(
            &painter,
            Rect::from_center_size(
                Pos2::new(rect.left() + 56.0 + title_width / 2.0, title_y),
                Vec2::new(title_width, 22.0),
            ),
            Align2::LEFT_CENTER,
            &row.title,
            theme::regular(14.0),
            theme::TEXT,
        );
        if !row.detail.is_empty() {
            theme::text_in(
                &painter,
                Rect::from_center_size(
                    Pos2::new(rect.left() + 56.0 + title_width / 2.0, detail_y),
                    Vec2::new(title_width, 20.0),
                ),
                Align2::LEFT_CENTER,
                &row.detail,
                theme::regular(12.0),
                theme::SECONDARY,
            );
        }
        let middle = rect.center().y;
        match &row.control {
            Control::Switch { id, on } => {
                let place = Rect::from_min_size(
                    Pos2::new(right - 44.0, middle - 12.0),
                    Vec2::new(44.0, 24.0),
                );
                let response = ui.put(place, |ui: &mut egui::Ui| theme::switch(ui, *on));
                if response.clicked() {
                    self.send(Action::Choose(*id));
                }
                theme::text_in(
                    &painter,
                    Rect::from_min_size(
                        Pos2::new(right - 84.0, middle - 10.0),
                        Vec2::new(32.0, 20.0),
                    ),
                    Align2::RIGHT_CENTER,
                    if *on { tr("On") } else { tr("Off") },
                    theme::regular(12.0),
                    theme::SECONDARY,
                );
            }
            Control::Choice { value, options } => {
                let place = Rect::from_min_size(
                    Pos2::new(right - 200.0, middle - 16.0),
                    Vec2::new(200.0, 32.0),
                );
                ui.scope_builder(egui::UiBuilder::new().max_rect(place), |ui| {
                    theme::light(ui);
                    egui::ComboBox::from_id_salt(("settings choice", index))
                        .selected_text(value)
                        .width(190.0)
                        .show_ui(ui, |ui| {
                            for (id, label, chosen) in options {
                                if ui.selectable_label(*chosen, label).clicked() {
                                    self.send(Action::Choose(*id));
                                }
                            }
                        });
                });
            }
            Control::Buttons(buttons) => {
                let mut x = right;
                for (id, label) in buttons.iter().rev() {
                    let galley =
                        theme::fitted(&painter, label, theme::regular(14.0), theme::TEXT, 300.0);
                    let width = galley.size().x + 32.0;
                    x -= width;
                    if theme::button(
                        ui,
                        Rect::from_min_size(Pos2::new(x, middle - 16.0), Vec2::new(width, 32.0)),
                        label,
                        true,
                    ) {
                        self.send(Action::Choose(*id));
                    }
                    x -= 10.0;
                }
            }
            Control::Keys { id, keys } => {
                let pencil =
                    Rect::from_min_size(Pos2::new(right - 30.0, middle - 15.0), Vec2::splat(30.0));
                let response = theme::hit(ui, pencil, ("keys", *id));
                if response.hovered() {
                    painter.rect_filled(pencil, CornerRadius::same(6), theme::CARD);
                }
                draw_pencil(&painter, pencil.shrink(8.0), theme::SECONDARY);
                if response.clicked() {
                    self.send(Action::Choose(*id));
                }
                if keys.is_empty() {
                    theme::text_in(
                        &painter,
                        Rect::from_min_size(
                            Pos2::new(right - 160.0, middle - 12.0),
                            Vec2::new(120.0, 24.0),
                        ),
                        Align2::RIGHT_CENTER,
                        tr("Not set"),
                        theme::regular(12.0),
                        theme::SECONDARY,
                    );
                }
                let mut x = right - 38.0;
                for key in keys.iter().rev() {
                    let galley =
                        theme::fitted(&painter, key, theme::regular(12.0), theme::ACCENT, 120.0);
                    let width = (galley.size().x + 18.0).max(30.0);
                    x -= width;
                    let chip =
                        Rect::from_min_size(Pos2::new(x, middle - 12.0), Vec2::new(width, 24.0));
                    painter.rect_filled(chip, CornerRadius::same(6), theme::PILL_SELF.0);
                    painter.galley(chip.center() - galley.size() / 2.0, galley, theme::ACCENT);
                    x -= 6.0;
                }
            }
        }
    }
}

/// Settings' card icons (Segoe MDL2 code points on Windows) as the shapes drawn here.
fn settings_icon(glyph: &str) -> Option<Icon> {
    Some(match glyph {
        "\u{E7E8}" => Icon::Wake,
        "\u{E895}" | "\u{E896}" => Icon::Transfers,
        "\u{E946}" | "\u{E774}" | "\u{E839}" | "\u{E7F4}" => Icon::Devices,
        "\u{E716}" | "\u{E8BD}" => Icon::Help,
        "\u{E8B7}" | "\u{E77F}" => Icon::Files,
        "\u{E756}" | "\u{E765}" => Icon::Terminal,
        "\u{E72E}" => Icon::Settings,
        _ => return None,
    })
}

/// A pencil (Change), drawn.
fn draw_pencil(painter: &egui::Painter, rect: Rect, color: Color32) {
    let stroke = Stroke::new(1.3_f32, color);
    let (a, b) = (
        rect.left_bottom() + Vec2::new(1.0, -1.0),
        rect.right_top() + Vec2::new(-2.0, 2.0),
    );
    painter.line_segment([a, b], stroke);
    painter.line_segment([a + Vec2::new(3.0, 0.0), b + Vec2::new(3.0, 0.0)], stroke);
    painter.line_segment([b, b + Vec2::new(3.0, 0.0)], stroke);
    painter.line_segment([a, a + Vec2::new(3.0, 0.0)], stroke);
}

/// The corners of `rect` rounded off to `radius` by covering what lies outside them with `color` (the page's).
fn round_corners(painter: &egui::Painter, rect: Rect, radius: f32, color: Color32) {
    let steps = 8;
    for (corner, center, from) in [
        (
            rect.left_top(),
            rect.left_top() + Vec2::splat(radius),
            180.0_f32,
        ),
        (
            rect.right_top(),
            rect.right_top() + Vec2::new(-radius, radius),
            270.0,
        ),
        (
            rect.right_bottom(),
            rect.right_bottom() - Vec2::splat(radius),
            0.0,
        ),
        (
            rect.left_bottom(),
            rect.left_bottom() + Vec2::new(radius, -radius),
            90.0,
        ),
    ] {
        let mut mesh = egui::Mesh::default();
        mesh.colored_vertex(corner, color);
        for k in 0..=steps {
            let (sin, cos) = (from + 90.0 * k as f32 / steps as f32)
                .to_radians()
                .sin_cos();
            mesh.colored_vertex(center + Vec2::new(cos, sin) * radius, color);
        }
        for k in 0..steps as u32 {
            mesh.add_triangle(0, k + 1, k + 2);
        }
        painter.add(egui::Shape::mesh(mesh));
    }
}

impl View for MainView {
    fn builder(&self) -> egui::ViewportBuilder {
        let mut builder = egui::ViewportBuilder::default()
            .with_title(tr("Queqiao"))
            .with_inner_size([WIDTH, HEIGHT])
            .with_min_inner_size([WIDTH, HEIGHT])
            .with_resizable(true);
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
        if !self.open.load(Ordering::Relaxed) {
            return false;
        }
        let screen = ctx.screen_rect();
        SIZE.set((screen.width().max(WIDTH), screen.height().max(HEIGHT)));
        let Ok(shown) = self.shown.lock().map(|shown| shown.clone()) else {
            return true;
        };
        // The title in the language in use (it can change in Settings while the window is open).
        let title = tr("Queqiao");
        if self.title != title {
            self.title = title;
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.to_string()));
        }
        if self.pictures.is_none() {
            let load = |name: &str, image: Option<(u32, u32, Vec<u8>)>| {
                image.map(|(width, height, rgba)| {
                    ctx.load_texture(
                        name,
                        egui::ColorImage::from_rgba_unmultiplied(
                            [width as usize, height as usize],
                            &rgba,
                        ),
                        egui::TextureOptions::LINEAR,
                    )
                })
            };
            self.pictures = Some(Pictures {
                logo: load("queqiao-logo", super::icon::icon(64)),
                bird: load("queqiao-bird", super::icon::spread_magpie(72)),
            });
        }
        egui::SidePanel::left("sidebar")
            .exact_width(SIDEBAR)
            .resizable(false)
            .show_separator_line(false)
            .frame(egui::Frame::new().fill(theme::SIDE_BG))
            .show(ctx, |ui| self.sidebar(ui, &shown));
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(theme::WHITE))
            .show(ctx, |ui| {
                theme::light(ui);
                let page = ui.max_rect();
                match shown.page {
                    Page::Devices => self.devices(ui, page, &shown),
                    Page::Help => self.help(ui, page, &shown),
                    Page::Transfers => self.transfers(ui, page, &shown),
                    Page::Settings => self.settings(ui, page, &shown),
                }
            });
        true
    }

    fn closed(&mut self) {
        // Closed: Queqiao goes on in the tray (as on Windows).
        self.open.store(false, Ordering::Relaxed);
    }
}
