//! Queqiao's look on Linux, as Windows draws it (main_window.rs): its colours, a bold face, its icons drawn as shapes
//! (Windows takes them from Segoe MDL2 Assets, which Linux has not), and its buttons and switch, for egui windows.

use eframe::egui;
use egui::{
    Align2, Color32, CornerRadius, FontFamily, FontId, Pos2, Rect, Stroke, StrokeKind, Vec2,
};

// The sidebar's night blue, the page's white, star gold (the Windows constants, as RGB).
pub(crate) const SIDE_BG: Color32 = Color32::from_rgb(0x14, 0x1A, 0x3B);
pub(crate) const SIDE_SELECTED: Color32 = Color32::from_rgb(0x26, 0x31, 0x69);
pub(crate) const SIDE_TEXT: Color32 = Color32::from_rgb(0xD2, 0xD7, 0xEE);
pub(crate) const SIDE_MUTED: Color32 = Color32::from_rgb(0x88, 0x90, 0xB5);
pub(crate) const SIDE_LINE: Color32 = Color32::from_rgb(0x29, 0x31, 0x5E);
pub(crate) const SIDE_CHIP: Color32 = Color32::from_rgb(0x2D, 0x38, 0x74);
pub(crate) const GOLD: Color32 = Color32::from_rgb(0xE3, 0xB4, 0x5A);
pub(crate) const ON_GOLD: Color32 = Color32::from_rgb(0x1A, 0x1F, 0x3F);
pub(crate) const WHITE: Color32 = Color32::WHITE;
pub(crate) const TEXT: Color32 = Color32::from_rgb(0x1B, 0x21, 0x40);
pub(crate) const SECONDARY: Color32 = Color32::from_rgb(0x64, 0x6B, 0x8C);
pub(crate) const DISABLED: Color32 = Color32::from_rgb(0xB4, 0xB9, 0xCC);
pub(crate) const DIVIDER: Color32 = Color32::from_rgb(0xE6, 0xE9, 0xF2);
pub(crate) const CARD: Color32 = Color32::from_rgb(0xF5, 0xF6, 0xFA);
pub(crate) const ACCENT: Color32 = Color32::from_rgb(0x3D, 0x5F, 0xAE);
pub(crate) const ONLINE: Color32 = Color32::from_rgb(0x2F, 0xBF, 0x71);
pub(crate) const OFFLINE: Color32 = Color32::from_rgb(0x9A, 0xA1, 0xB8);
pub(crate) const BUTTON_BORDER: Color32 = Color32::from_rgb(0xCB, 0xCF, 0xDA);
pub(crate) const SWITCH_OFF: Color32 = Color32::from_rgb(0xB6, 0xBB, 0xC9);
/// Pills: (background, text).
pub(crate) const PILL_ON: (Color32, Color32) = (
    Color32::from_rgb(0xE3, 0xF6, 0xEC),
    Color32::from_rgb(0x17, 0x78, 0x4A),
);
pub(crate) const PILL_OFF: (Color32, Color32) = (
    Color32::from_rgb(0xEE, 0xF0, 0xF5),
    Color32::from_rgb(0x5D, 0x64, 0x80),
);
pub(crate) const PILL_SELF: (Color32, Color32) = (Color32::from_rgb(0xE8, 0xED, 0xFB), ACCENT);
// The card of the Devices page: a night sky.
pub(crate) const HERO_TOP: Color32 = Color32::from_rgb(0x25, 0x30, 0x70);
pub(crate) const HERO_BOTTOM: Color32 = Color32::from_rgb(0x0E, 0x13, 0x30);
pub(crate) const HERO_MUTED: Color32 = Color32::from_rgb(0xA9, 0xB1, 0xD6);
pub(crate) const HERO_FAINT: Color32 = Color32::from_rgb(0x5A, 0x6A, 0x8A);

/// The bold face (gui.rs loads the system's bold sans, Chinese too), or the regular one where there is none.
pub(crate) const BOLD: &str = "bold";

pub(crate) fn regular(size: f32) -> FontId {
    FontId::proportional(size)
}

pub(crate) fn bold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(BOLD.into()))
}

/// The icons Queqiao uses, as Windows has them from Segoe MDL2 Assets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Icon {
    Devices,
    Help,
    Transfers,
    Settings,
    Files,
    Terminal,
    Wake,
    More,
    Received,
    Sent,
}

/// `icon` drawn in `rect` (square-ish), lines `color`.
pub(crate) fn icon(painter: &egui::Painter, icon: Icon, rect: Rect, color: Color32) {
    let size = rect.width().min(rect.height());
    let c = rect.center();
    let u = size / 16.0;
    let line = Stroke::new((1.2 * u).max(1.0), color);
    let p = |x: f32, y: f32| Pos2::new(c.x + x * u, c.y + y * u);
    match icon {
        Icon::Devices => {
            painter.rect_stroke(
                Rect::from_min_max(p(-7.0, -6.0), p(7.0, 3.5)),
                CornerRadius::same(1),
                line,
                StrokeKind::Middle,
            );
            painter.line_segment([p(0.0, 3.5), p(0.0, 6.0)], line);
            painter.line_segment([p(-3.5, 6.5), p(3.5, 6.5)], line);
        }
        Icon::Help => {
            painter.circle_stroke(p(-2.5, -3.0), 2.6 * u, line);
            painter.add(egui::Shape::line(
                arc(p(-2.5, 7.0), 5.0 * u, 200.0, 340.0),
                line,
            ));
            painter.circle_stroke(p(4.0, -1.0), 2.0 * u, line);
            painter.add(egui::Shape::line(
                arc(p(4.5, 7.0), 3.8 * u, 215.0, 330.0),
                line,
            ));
        }
        Icon::Transfers | Icon::Received => {
            painter.line_segment([p(0.0, -7.0), p(0.0, 4.0)], line);
            painter.line_segment([p(-4.0, 0.0), p(0.0, 4.0)], line);
            painter.line_segment([p(4.0, 0.0), p(0.0, 4.0)], line);
            painter.line_segment([p(-6.0, 7.0), p(6.0, 7.0)], line);
        }
        Icon::Sent => {
            painter.line_segment([p(0.0, 7.0), p(0.0, -4.0)], line);
            painter.line_segment([p(-4.0, 0.0), p(0.0, -4.0)], line);
            painter.line_segment([p(4.0, 0.0), p(0.0, -4.0)], line);
            painter.line_segment([p(-6.0, -7.0), p(6.0, -7.0)], line);
        }
        Icon::Settings => {
            painter.circle_stroke(c, 2.6 * u, line);
            painter.circle_stroke(c, 5.4 * u, line);
            for k in 0..8 {
                let angle = k as f32 * std::f32::consts::PI / 4.0;
                let (sin, cos) = angle.sin_cos();
                painter.line_segment(
                    [
                        Pos2::new(c.x + cos * 5.4 * u, c.y + sin * 5.4 * u),
                        Pos2::new(c.x + cos * 7.4 * u, c.y + sin * 7.4 * u),
                    ],
                    Stroke::new(2.0 * u, color),
                );
            }
        }
        Icon::Files => {
            let points = vec![
                p(-5.0, -7.0),
                p(2.0, -7.0),
                p(5.5, -3.5),
                p(5.5, 7.0),
                p(-5.0, 7.0),
                p(-5.0, -7.0),
            ];
            painter.add(egui::Shape::line(points, line));
            painter.add(egui::Shape::line(
                vec![p(2.0, -7.0), p(2.0, -3.5), p(5.5, -3.5)],
                line,
            ));
        }
        Icon::Terminal => {
            painter.rect_stroke(
                Rect::from_min_max(p(-7.5, -6.0), p(7.5, 6.0)),
                CornerRadius::same(1),
                line,
                StrokeKind::Middle,
            );
            painter.add(egui::Shape::line(
                vec![p(-4.5, -2.0), p(-2.0, 0.5), p(-4.5, 3.0)],
                line,
            ));
            painter.line_segment([p(0.0, 3.0), p(4.0, 3.0)], line);
        }
        Icon::Wake => {
            painter.add(egui::Shape::line(arc(c, 6.0 * u, -60.0, 240.0), line));
            painter.line_segment([p(0.0, -7.5), p(0.0, -1.0)], line);
        }
        Icon::More => {
            for x in [-5.0, 0.0, 5.0] {
                painter.circle_filled(p(x, 0.0), 1.2 * u, color);
            }
        }
    }
}

/// Points along a circle's arc, `from` to `to` degrees (0 is right, angles grow clockwise as screen y does).
fn arc(center: Pos2, radius: f32, from: f32, to: f32) -> Vec<Pos2> {
    let steps = 24;
    (0..=steps)
        .map(|k| {
            let degrees = from + (to - from) * k as f32 / steps as f32;
            let (sin, cos) = degrees.to_radians().sin_cos();
            Pos2::new(center.x + cos * radius, center.y + sin * radius)
        })
        .collect()
}

/// A text laid out on one line, cut with "…" where it is wider than `width`.
pub(crate) fn fitted(
    painter: &egui::Painter,
    text: &str,
    font: FontId,
    color: Color32,
    width: f32,
) -> std::sync::Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::single_section(
        text.to_string(),
        egui::TextFormat::simple(font, color),
    );
    job.wrap = egui::text::TextWrapping {
        max_width: width.max(1.0),
        max_rows: 1,
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    painter.layout_job(job)
}

/// `text` drawn on one line in `rect`, placed by `align` within it, cut to fit.
pub(crate) fn text_in(
    painter: &egui::Painter,
    rect: Rect,
    align: Align2,
    text: &str,
    font: FontId,
    color: Color32,
) {
    let galley = fitted(painter, text, font, color, rect.width());
    let at = align.align_size_within_rect(galley.size(), rect).min;
    painter.galley(at, galley, color);
}

/// A clickable area with the hand over it.
pub(crate) fn hit(ui: &mut egui::Ui, rect: Rect, id: impl std::hash::Hash) -> egui::Response {
    ui.interact(rect, ui.id().with(id), egui::Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A white button with a grey border (Windows' standard button on the page): clicked or not.
pub(crate) fn button(ui: &mut egui::Ui, rect: Rect, label: &str, enabled: bool) -> bool {
    let response = if enabled {
        hit(ui, rect, ("button", label))
    } else {
        ui.interact(rect, ui.id().with(("button", label)), egui::Sense::hover())
    };
    let painter = ui.painter();
    let fill = if response.hovered() { CARD } else { WHITE };
    painter.rect_filled(rect, CornerRadius::same(6), fill);
    painter.rect_stroke(
        rect,
        CornerRadius::same(6),
        Stroke::new(1.0_f32, BUTTON_BORDER),
        StrokeKind::Inside,
    );
    let color = if enabled { TEXT } else { DISABLED };
    text_in(
        painter,
        rect,
        Align2::CENTER_CENTER,
        label,
        regular(14.0),
        color,
    );
    enabled && response.clicked()
}

/// The gold button (Connect, Wake up, Log in).
pub(crate) fn gold_button(ui: &mut egui::Ui, rect: Rect, label: &str) -> bool {
    let response = hit(ui, rect, ("gold", label));
    let fill = if response.hovered() {
        Color32::from_rgb(0xEC, 0xC2, 0x6E)
    } else {
        GOLD
    };
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same((rect.height() / 2.0) as u8), fill);
    text_in(
        painter,
        rect,
        Align2::CENTER_CENTER,
        label,
        bold(14.0),
        ON_GOLD,
    );
    response.clicked()
}

/// A switch as Settings shows it: blue and to the right when on.
pub(crate) fn switch(ui: &mut egui::Ui, on: bool) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(44.0, 24.0), egui::Sense::click());
    let response = response.on_hover_cursor(egui::CursorIcon::PointingHand);
    if ui.is_rect_visible(rect) {
        let t = ui.ctx().animate_bool(response.id, on);
        let radius = rect.height() / 2.0;
        let painter = ui.painter();
        if on {
            painter.rect_filled(rect, CornerRadius::same(12), ACCENT);
        } else {
            painter.rect_filled(rect, CornerRadius::same(12), WHITE);
            painter.rect_stroke(
                rect,
                CornerRadius::same(12),
                Stroke::new(1.0_f32, SWITCH_OFF),
                StrokeKind::Inside,
            );
        }
        let x = egui::lerp((rect.left() + radius)..=(rect.right() - radius), t);
        painter.circle_filled(
            Pos2::new(x, rect.center().y),
            radius - 4.0,
            if on { WHITE } else { SWITCH_OFF },
        );
    }
    response
}

/// egui's light look in Queqiao's colours, for a window's own `ui`.
pub(crate) fn light(ui: &mut egui::Ui) {
    ui.style_mut().visuals = visuals();
}

/// Queqiao's light look for egui: white fields and buttons with a grey border (as Windows' standard controls), its
/// blue for what is chosen. Every egui window starts with it (gui.rs); the session's dark panels set their own.
pub(crate) fn visuals() -> egui::Visuals {
    let mut visuals = egui::Visuals::light();
    visuals.override_text_color = Some(TEXT);
    visuals.panel_fill = WHITE;
    visuals.window_fill = WHITE;
    visuals.extreme_bg_color = WHITE;
    visuals.selection.bg_fill = Color32::from_rgb(0xC9, 0xD5, 0xF2);
    visuals.selection.stroke = Stroke::new(1.5_f32, ACCENT);
    let border = Stroke::new(1.0_f32, BUTTON_BORDER);
    for (widget, fill) in [
        (&mut visuals.widgets.inactive, WHITE),
        (&mut visuals.widgets.hovered, CARD),
        (&mut visuals.widgets.active, DIVIDER),
        (&mut visuals.widgets.open, CARD),
    ] {
        widget.corner_radius = CornerRadius::same(6);
        widget.weak_bg_fill = fill;
        widget.bg_fill = fill;
        widget.bg_stroke = border;
        widget.fg_stroke = Stroke::new(1.0_f32, TEXT);
    }
    visuals
}
