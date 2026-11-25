//! The session window's overlays on Linux (Windows: overlay.rs), in Queqiao's night-blue and gold: the bar at the top
//! centre with the session's commands, which tucks away into a small tab when the pointer leaves it; the tip under a
//! button when the bar has no room for labels; the card of figures at the bottom right.
//!
//! Each is a child window of the session window, so it moves, hides and goes with it and never covers other programs.
//! They have an X connection of their own: their pointer events never reach the session's input, so a click on the
//! bar never goes to the other PC; the card and the tip let the pointer through (an empty SHAPE input region), and the
//! bar's and the tab's corners are cut round (the SHAPE bounding region). They are drawn on the CPU (text by ab_glyph
//! with egui's font and the system's for Chinese and Japanese; the buttons' icons drawn as shapes, Segoe MDL2's
//! glyphs being Windows' own) and put with PutImage. A click on the bar reaches the session window as a client
//! message (_QUEQIAO_BAR) that it takes as a menu command, as Windows' bar sends WM_SYSCOMMAND.

use std::sync::OnceLock;
use std::time::{Duration, Instant};

use ab_glyph::{Font, FontArc, PxScale, ScaleFont, point};
use queqiao_core::text::tr;
use x11rb::connection::Connection;
use x11rb::protocol::Event;
use x11rb::protocol::shape::{self, ConnectionExt as _};
use x11rb::protocol::xproto::{
    ClientMessageEvent, ClipOrdering, ConfigureWindowAux, ConnectionExt as _, CreateGCAux,
    CreateWindowAux, EventMask, ImageFormat, Rectangle, StackMode, WindowClass,
};
use x11rb::rust_connection::RustConnection;

use super::window::Hwnd;
use super::{Result, x11_error};
use crate::model::{BAR_END, BarButton, Figures};

// Colours (0x00RRGGBB), as on Windows.
const INK: u32 = 0x0014_1A3B;
const GOLD: u32 = 0x00E3_B45A;
const TEXT: u32 = 0x00EE_F0FA;
const MUTED: u32 = 0x00A9_B1D6;
const END_RED: u32 = 0x00C8_4A3C;
const END_HOVER: u32 = 0x00DE_5C4C;
const HOVER: u32 = 0x0026_3169;
const DIVIDER: u32 = 0x003A_4370;
const GOOD: u32 = 0x006F_D3A0;
const FAIR: u32 = 0x00E8_B04A;
const POOR: u32 = 0x00E8_6A5A;
/// How long the bar stays after the pointer left it.
const BAR_STAYS: Duration = Duration::from_millis(1500);
/// The pointer this near the top of the picture brings the bar out (pixels).
const BAR_REACH: i32 = 56;
/// The message a click on the bar sends the session window (data: the button's id).
pub(super) const BAR_ATOM: &str = "_QUEQIAO_BAR";
/// The cursor font's hand (XC_hand2).
const HAND: u16 = 60;

/// egui's font, then the system's for Chinese and Japanese (as the GUI thread finds them), loaded once.
fn fonts() -> &'static [FontArc] {
    static FONTS: OnceLock<Vec<FontArc>> = OnceLock::new();
    FONTS.get_or_init(|| {
        let mut fonts: Vec<FontArc> = FontArc::try_from_slice(epaint_default_fonts::UBUNTU_LIGHT)
            .into_iter()
            .collect();
        fonts.extend(
            super::gui::system_font_files()
                .into_iter()
                .filter_map(|path| std::fs::read(path).ok())
                .filter_map(|bytes| FontArc::try_from_vec(bytes).ok()),
        );
        fonts
    })
}

/// Text in `size` pixels (the font's em): each character from the first font that has it.
fn glyphs(text: &str) -> impl Iterator<Item = (&'static FontArc, ab_glyph::GlyphId)> + '_ {
    let fonts = fonts();
    text.chars().filter_map(move |c| {
        fonts
            .iter()
            .map(|font| (font, font.glyph_id(c)))
            .find(|(_, id)| id.0 != 0)
            .or_else(|| fonts.first().map(|font| (font, font.glyph_id(c))))
    })
}

fn text_width(text: &str, size: f32) -> i32 {
    glyphs(text)
        .map(|(font, id)| font.as_scaled(PxScale::from(size)).h_advance(id))
        .sum::<f32>()
        .ceil() as i32
}

/// A picture being drawn: 0x00RRGGBB pixels, top row first.
struct Canvas {
    width: i32,
    height: i32,
    pixels: Vec<u32>,
}

impl Canvas {
    fn new(width: i32, height: i32, colour: u32) -> Canvas {
        Canvas {
            width,
            height,
            pixels: vec![colour; (width.max(0) * height.max(0)) as usize],
        }
    }

    fn blend(&mut self, x: i32, y: i32, colour: u32, alpha: f32) {
        if x < 0 || y < 0 || x >= self.width || y >= self.height || alpha <= 0.0 {
            return;
        }
        let at = (y * self.width + x) as usize;
        let alpha = alpha.min(1.0);
        let under = self.pixels[at];
        let mix = |shift: u32| {
            let a = ((colour >> shift) & 0xFF) as f32;
            let b = ((under >> shift) & 0xFF) as f32;
            ((a * alpha + b * (1.0 - alpha)).round() as u32) << shift
        };
        self.pixels[at] = mix(16) | mix(8) | mix(0);
    }

    fn fill(&mut self, x: i32, y: i32, width: i32, height: i32, colour: u32) {
        for row in y.max(0)..(y + height).min(self.height) {
            for column in x.max(0)..(x + width).min(self.width) {
                self.pixels[(row * self.width + column) as usize] = colour;
            }
        }
    }

    /// A rectangle with round corners of `radius`, edges smoothed.
    fn fill_round(&mut self, x: i32, y: i32, width: i32, height: i32, radius: i32, colour: u32) {
        let radius = radius.min(width / 2).min(height / 2) as f32;
        for row in y..y + height {
            for column in x..x + width {
                let (px, py) = (column as f32 + 0.5, row as f32 + 0.5);
                let cx = px.clamp(x as f32 + radius, (x + width) as f32 - radius);
                let cy = py.clamp(y as f32 + radius, (y + height) as f32 - radius);
                let distance = ((px - cx).powi(2) + (py - cy).powi(2)).sqrt();
                self.blend(column, row, colour, radius - distance + 0.5);
            }
        }
    }

    /// A line `thickness` wide from (x0, y0) to (x1, y1), edges smoothed (the icons).
    fn line(&mut self, from: (f32, f32), to: (f32, f32), thickness: f32, colour: u32) {
        let half = thickness / 2.0;
        let (left, right) = (from.0.min(to.0) - half - 1.0, from.0.max(to.0) + half + 1.0);
        let (top, bottom) = (from.1.min(to.1) - half - 1.0, from.1.max(to.1) + half + 1.0);
        let (dx, dy) = (to.0 - from.0, to.1 - from.1);
        let length = (dx * dx + dy * dy).max(1e-6);
        for row in top.floor() as i32..bottom.ceil() as i32 {
            for column in left.floor() as i32..right.ceil() as i32 {
                let (px, py) = (column as f32 + 0.5, row as f32 + 0.5);
                let t = (((px - from.0) * dx + (py - from.1) * dy) / length).clamp(0.0, 1.0);
                let (nx, ny) = (from.0 + t * dx, from.1 + t * dy);
                let distance = ((px - nx).powi(2) + (py - ny).powi(2)).sqrt();
                self.blend(column, row, colour, half - distance + 0.5);
            }
        }
    }

    /// Text in `size` pixels from `x`, centred on the line `middle`; its width.
    fn text(&mut self, text: &str, x: i32, middle: i32, size: f32, colour: u32) -> i32 {
        let scale = PxScale::from(size);
        let mut pen = x as f32;
        for (font, id) in glyphs(text) {
            let scaled = font.as_scaled(scale);
            let baseline = middle as f32 + (scaled.ascent() + scaled.descent()) / 2.0;
            let glyph = id.with_scale_and_position(scale, point(pen, baseline));
            pen += scaled.h_advance(id);
            if let Some(outline) = font.outline_glyph(glyph) {
                let bounds = outline.px_bounds();
                outline.draw(|gx, gy, coverage| {
                    self.blend(
                        bounds.min.x as i32 + gx as i32,
                        bounds.min.y as i32 + gy as i32,
                        colour,
                        coverage,
                    );
                });
            }
        }
        (pen - x as f32).ceil() as i32
    }

    /// Text ending at `right`.
    fn text_right(&mut self, text: &str, right: i32, middle: i32, size: f32, colour: u32) {
        let width = text_width(text, size);
        self.text(text, right - width, middle, size, colour);
    }

    /// The button's icon (Windows: its Segoe MDL2 glyph) in a 16-pixel square at (x, y).
    fn icon(&mut self, glyph: &str, x: i32, y: i32, colour: u32) {
        let (x, y) = (x as f32, y as f32);
        let p = |a: f32, b: f32| (x + a, y + b);
        match glyph {
            // Full screen: four corners.
            "\u{E740}" => {
                for (corner, h, v) in [
                    (p(1.0, 1.0), 5.0, 5.0),
                    (p(15.0, 1.0), -5.0, 5.0),
                    (p(1.0, 15.0), 5.0, -5.0),
                    (p(15.0, 15.0), -5.0, -5.0),
                ] {
                    self.line(corner, (corner.0 + h, corner.1), 1.6, colour);
                    self.line(corner, (corner.0, corner.1 + v), 1.6, colour);
                }
            }
            // Ctrl+Alt+Del: a keyboard.
            "\u{E765}" => {
                for (from, to) in [
                    (p(1.0, 3.5), p(15.0, 3.5)),
                    (p(1.0, 12.5), p(15.0, 12.5)),
                    (p(1.0, 3.5), p(1.0, 12.5)),
                    (p(15.0, 3.5), p(15.0, 12.5)),
                ] {
                    self.line(from, to, 1.4, colour);
                }
                for column in 0..4 {
                    for row in 0..2 {
                        let at = p(4.0 + column as f32 * 2.7, 6.0 + row as f32 * 2.4);
                        self.line(at, (at.0 + 0.6, at.1), 1.4, colour);
                    }
                }
                self.line(p(5.0, 10.5), p(11.0, 10.5), 1.4, colour);
            }
            // Send files: an arrow up onto a line.
            "\u{E898}" => {
                self.line(p(8.0, 2.0), p(8.0, 11.5), 1.6, colour);
                self.line(p(8.0, 2.0), p(4.0, 6.0), 1.6, colour);
                self.line(p(8.0, 2.0), p(12.0, 6.0), 1.6, colour);
                self.line(p(2.0, 14.5), p(14.0, 14.5), 1.6, colour);
            }
            // Command line: a window with a prompt.
            "\u{E756}" => {
                for (from, to) in [
                    (p(1.0, 2.0), p(15.0, 2.0)),
                    (p(1.0, 14.0), p(15.0, 14.0)),
                    (p(1.0, 2.0), p(1.0, 14.0)),
                    (p(15.0, 2.0), p(15.0, 14.0)),
                ] {
                    self.line(from, to, 1.4, colour);
                }
                self.line(p(4.0, 6.0), p(7.0, 8.5), 1.4, colour);
                self.line(p(7.0, 8.5), p(4.0, 11.0), 1.4, colour);
                self.line(p(8.5, 11.0), p(12.0, 11.0), 1.4, colour);
            }
            // Figures: three bars.
            "\u{E9D9}" => {
                for (column, height) in [(2.5, 6.0), (8.0, 12.0), (13.5, 9.0)] {
                    self.line(p(column, 15.0), p(column, 15.0 - height), 2.4, colour);
                }
            }
            // File transfer: a folder.
            "\u{E8B7}" => {
                for (from, to) in [
                    (p(1.0, 4.0), p(6.0, 4.0)),
                    (p(6.0, 4.0), p(7.5, 6.0)),
                    (p(7.5, 6.0), p(15.0, 6.0)),
                    (p(15.0, 6.0), p(15.0, 14.0)),
                    (p(15.0, 14.0), p(1.0, 14.0)),
                    (p(1.0, 14.0), p(1.0, 4.0)),
                ] {
                    self.line(from, to, 1.4, colour);
                }
            }
            // Control center: a gear (a ring with teeth).
            "\u{E713}" => {
                for step in 0..8 {
                    let angle = step as f32 * std::f32::consts::PI / 4.0;
                    let (sin, cos) = angle.sin_cos();
                    self.line(
                        p(8.0 + cos * 5.0, 8.0 + sin * 5.0),
                        p(8.0 + cos * 7.0, 8.0 + sin * 7.0),
                        2.0,
                        colour,
                    );
                }
                for step in 0..16 {
                    let (a, b) = (
                        step as f32 * std::f32::consts::PI / 8.0,
                        (step + 1) as f32 * std::f32::consts::PI / 8.0,
                    );
                    self.line(
                        p(8.0 + a.cos() * 4.5, 8.0 + a.sin() * 4.5),
                        p(8.0 + b.cos() * 4.5, 8.0 + b.sin() * 4.5),
                        1.4,
                        colour,
                    );
                }
            }
            // More: three dots.
            "\u{E712}" => {
                for column in [3.0, 8.0, 13.0] {
                    self.line(p(column, 8.0), p(column + 0.1, 8.0), 2.6, colour);
                }
            }
            // End: a cross.
            "\u{E711}" => {
                self.line(p(3.0, 3.0), p(13.0, 13.0), 1.7, colour);
                self.line(p(13.0, 3.0), p(3.0, 13.0), 1.7, colour);
            }
            _ => self.line(p(7.0, 8.0), p(9.0, 8.0), 3.0, colour),
        }
    }

    /// The pixels as PutImage takes them for a 24-bit window (32 bits a pixel: blue, green, red, unused).
    fn bytes(&self) -> Vec<u8> {
        self.pixels
            .iter()
            .flat_map(|pixel| pixel.to_le_bytes())
            .collect()
    }
}

/// One overlay window: where it is (in the session window) and its picture, kept for Expose.
struct Piece {
    window: u32,
    shown: bool,
    place: (i32, i32, i32, i32),
    picture: Option<Canvas>,
}

pub struct SessionOverlay {
    connection: RustConnection,
    owner: u32,
    bar_atom: u32,
    gc: u32,
    depth: u8,
    name: String,
    buttons: Vec<BarButton>,
    bar: Piece,
    card: Piece,
    tip: Piece,
    /// The session window's size.
    area: (i32, i32),
    /// The pointer in the session window (None: outside it).
    pointer: Option<(i32, i32)>,
    /// The bar is out (else only its tab shows).
    bar_out: bool,
    last_near: Instant,
    card_on: bool,
    figures: Figures,
    /// The button under the pointer, and the one pressed.
    hover: Option<usize>,
    pressed: Option<usize>,
    /// Each button's place in the bar and id.
    places: Vec<((i32, i32, i32, i32), u16)>,
    /// Tests: the bar stays out (for a picture of it).
    keep_out: bool,
    name_width: i32,
    button_widths: [Vec<i32>; 2],
    /// How much of the bar shows (see `width_at`), by the window's width.
    level: usize,
    /// What the bar was drawn with last.
    drawn: Option<Drawn>,
}

const NAME_SIZE: f32 = 14.0;
/// The delay's dot and the time after the name ("● 00:12:34").
const CLOCK_WIDTH: i32 = 70;
const LABEL_SIZE: f32 = 13.5;
const SMALL_SIZE: f32 = 12.5;

impl SessionOverlay {
    /// The overlays of the session window `owner` (the other PC's `name`), with the bar's `buttons`.
    pub fn new(owner: Hwnd, name: &str, buttons: Vec<BarButton>) -> Result<SessionOverlay> {
        let (connection, _) = x11rb::connect(None).map_err(x11_error)?;
        let owner = owner.0 as u32;
        let geometry = connection
            .get_geometry(owner)
            .map_err(x11_error)?
            .reply()
            .map_err(x11_error)?;
        let area = (i32::from(geometry.width), i32::from(geometry.height));
        let depth = geometry.depth;
        let bar_atom = connection
            .intern_atom(false, BAR_ATOM.as_bytes())
            .map_err(x11_error)?
            .reply()
            .map_err(x11_error)?
            .atom;
        // The pointer over the picture brings the bar out; the window's size places the overlays.
        connection
            .change_window_attributes(
                owner,
                &x11rb::protocol::xproto::ChangeWindowAttributesAux::new().event_mask(
                    EventMask::POINTER_MOTION
                        | EventMask::LEAVE_WINDOW
                        | EventMask::STRUCTURE_NOTIFY,
                ),
            )
            .map_err(x11_error)?;
        let hand = {
            let font = connection.generate_id().map_err(x11_error)?;
            let cursor = connection.generate_id().map_err(x11_error)?;
            connection.open_font(font, b"cursor").map_err(x11_error)?;
            connection
                .create_glyph_cursor(
                    cursor,
                    font,
                    font,
                    HAND,
                    HAND + 1,
                    0,
                    0,
                    0,
                    0xFFFF,
                    0xFFFF,
                    0xFFFF,
                )
                .map_err(x11_error)?;
            connection.close_font(font).map_err(x11_error)?;
            cursor
        };
        let make = |events: EventMask, cursor: u32| -> Result<u32> {
            let window = connection.generate_id().map_err(x11_error)?;
            connection
                .create_window(
                    x11rb::COPY_DEPTH_FROM_PARENT,
                    window,
                    owner,
                    0,
                    0,
                    1,
                    1,
                    0,
                    WindowClass::INPUT_OUTPUT,
                    x11rb::COPY_FROM_PARENT,
                    &CreateWindowAux::new()
                        .background_pixel(INK)
                        .event_mask(events)
                        .cursor(cursor),
                )
                .map_err(x11_error)?;
            Ok(window)
        };
        let bar = make(
            EventMask::EXPOSURE
                | EventMask::POINTER_MOTION
                | EventMask::ENTER_WINDOW
                | EventMask::LEAVE_WINDOW
                | EventMask::BUTTON_PRESS
                | EventMask::BUTTON_RELEASE,
            hand,
        )?;
        let card = make(EventMask::EXPOSURE, x11rb::NONE)?;
        let tip = make(EventMask::EXPOSURE, x11rb::NONE)?;
        connection.free_cursor(hand).map_err(x11_error)?;
        // The card and the tip let the pointer through to the picture.
        for window in [card, tip] {
            connection
                .shape_rectangles(
                    shape::SO::SET,
                    shape::SK::INPUT,
                    ClipOrdering::UNSORTED,
                    window,
                    0,
                    0,
                    &[],
                )
                .map_err(x11_error)?;
        }
        let gc = connection.generate_id().map_err(x11_error)?;
        connection
            .create_gc(gc, bar, &CreateGCAux::new())
            .map_err(x11_error)?;
        let name_width = text_width(name, NAME_SIZE);
        let button_widths = [true, false].map(|labelled| {
            buttons
                .iter()
                .map(|button| {
                    let label = if labelled && !button.label.is_empty() {
                        6 + text_width(&button.label, LABEL_SIZE)
                    } else {
                        0
                    };
                    12 + 16 + label + 12
                })
                .collect()
        });
        let piece = |window| Piece {
            window,
            shown: false,
            place: (0, 0, 0, 0),
            picture: None,
        };
        let mut overlay = SessionOverlay {
            connection,
            owner,
            bar_atom,
            gc,
            depth,
            name: name.to_string(),
            buttons,
            bar: piece(bar),
            card: piece(card),
            tip: piece(tip),
            area,
            pointer: None,
            bar_out: true,
            last_near: Instant::now(),
            card_on: false,
            figures: Figures::default(),
            hover: None,
            pressed: None,
            places: Vec::new(),
            keep_out: false,
            name_width,
            button_widths,
            level: 0,
            drawn: None,
        };
        overlay.tick();
        Ok(overlay)
    }

    /// Shows or hides the card of figures.
    pub fn set_card(&mut self, on: bool) {
        self.card_on = on;
        if on {
            self.draw_card();
        } else {
            self.hide(Which::Card);
        }
        let _ = self.connection.flush();
    }

    pub fn card_shown(&self) -> bool {
        self.card_on
    }

    /// Tests: keeps the bar out whatever the pointer does (for a picture of it).
    pub fn keep_bar_out(&mut self, on: bool) {
        self.keep_out = on;
    }

    /// Tests: a click on the bar's button `id`: false when the bar does not show that button now.
    pub fn click_for_test(&self, id: u16) -> bool {
        if !self.places.iter().any(|(_, place_id)| *place_id == id) {
            return false;
        }
        self.send(id);
        true
    }

    /// Tests: the button `index` drawn as under the pointer (its highlight, and its tip when the bar has no labels).
    pub fn hover_for_test(&mut self, index: Option<usize>) {
        self.hover = index;
    }

    /// New figures (once a second): the card is drawn again.
    pub fn set_figures(&mut self, figures: Figures) {
        self.figures = figures;
        if self.card_on {
            self.draw_card();
        }
        // The bar's clock and delay dot.
        if self.bar_out && self.level < 2 {
            self.draw_bar();
        }
        let _ = self.connection.flush();
    }

    /// Where the bar's button `id` ends (its bottom right, in the session window), while it shows.
    pub fn anchor_of(&self, id: u16) -> Option<(i32, i32)> {
        let ((left, top, width, height), _) =
            self.places.iter().find(|(_, place_id)| *place_id == id)?;
        Some((
            self.bar.place.0 + left + width,
            self.bar.place.1 + top + height,
        ))
    }

    /// Follows the pointer and the session window's size, brings the bar out or tucks it away. Called often (the
    /// session's loop); it only draws what changed.
    pub fn tick(&mut self) {
        while let Ok(Some(event)) = self.connection.poll_for_event() {
            self.handle(event);
        }
        let room = self.area.0 - 24;
        self.level = (0..2)
            .find(|&level| self.width_at(level) <= room)
            .unwrap_or(2);
        let (bar_width, bar_height) = (self.width_at(self.level), 36);
        let bar_left = (self.area.0 - bar_width) / 2;
        if let Some((x, y)) = self.pointer {
            let over_bar =
                (bar_left..bar_left + bar_width).contains(&x) && (8..8 + bar_height).contains(&y);
            let near_top = (0..self.area.0).contains(&x) && (0..BAR_REACH).contains(&y);
            if over_bar || near_top {
                self.last_near = Instant::now();
            }
        }
        self.bar_out = self.keep_out || self.last_near.elapsed() < BAR_STAYS;
        let state = (self.bar_out, self.level, self.hover, self.area);
        if self.drawn != Some(state) {
            self.drawn = Some(state);
            self.draw_bar();
            if self.card_on {
                self.draw_card();
            }
        }
        let _ = self.connection.flush();
    }

    fn handle(&mut self, event: Event) {
        match event {
            Event::Expose(expose) if expose.count == 0 => {
                for which in [Which::Bar, Which::Card, Which::Tip] {
                    if self.piece(which).window == expose.window {
                        self.put(which);
                    }
                }
            }
            Event::ConfigureNotify(configure) if configure.window == self.owner => {
                self.area = (i32::from(configure.width), i32::from(configure.height));
            }
            Event::MotionNotify(motion) if motion.event == self.owner => {
                self.pointer = Some((i32::from(motion.event_x), i32::from(motion.event_y)));
            }
            Event::MotionNotify(motion) if motion.event == self.bar.window => {
                let (x, y) = (i32::from(motion.event_x), i32::from(motion.event_y));
                self.pointer = Some((self.bar.place.0 + x, self.bar.place.1 + y));
                self.hover = self.place_at(x, y);
            }
            Event::EnterNotify(enter) if enter.event == self.bar.window => {
                self.last_near = Instant::now();
                self.hover = self.place_at(i32::from(enter.event_x), i32::from(enter.event_y));
            }
            Event::LeaveNotify(leave) if leave.event == self.bar.window => {
                self.hover = None;
                self.pressed = None;
            }
            // Out of the session window (not into the bar, its child).
            Event::LeaveNotify(leave)
                if leave.event == self.owner
                    && leave.detail != x11rb::protocol::xproto::NotifyDetail::INFERIOR =>
            {
                self.pointer = None;
            }
            Event::ButtonPress(press) if press.event == self.bar.window && press.detail == 1 => {
                self.pressed = self.place_at(i32::from(press.event_x), i32::from(press.event_y));
                if !self.bar_out {
                    self.last_near = Instant::now();
                }
            }
            Event::ButtonRelease(release)
                if release.event == self.bar.window && release.detail == 1 =>
            {
                let at = self.place_at(i32::from(release.event_x), i32::from(release.event_y));
                if let Some(index) = at.filter(|index| Some(*index) == self.pressed)
                    && let Some((_, id)) = self.places.get(index)
                {
                    self.send(*id);
                }
                self.pressed = None;
            }
            _ => {}
        }
    }

    /// The button at (x, y) in the bar.
    fn place_at(&self, x: i32, y: i32) -> Option<usize> {
        self.places
            .iter()
            .position(|((left, top, width, height), _)| {
                (*left..left + width).contains(&x) && (*top..top + height).contains(&y)
            })
    }

    /// The button's command to the session window (as its menu would give it).
    fn send(&self, id: u16) {
        let message =
            ClientMessageEvent::new(32, self.owner, self.bar_atom, [u32::from(id), 0, 0, 0, 0]);
        let _ = self
            .connection
            .send_event(false, self.owner, EventMask::NO_EVENT, message);
        let _ = self.connection.flush();
    }

    /// The bar's width at `level`: 0 all of it, 1 without the buttons' labels, 2 without the name either.
    fn width_at(&self, level: usize) -> i32 {
        let name = if level < 2 {
            14 + self.name_width + 10 + CLOCK_WIDTH + 4
        } else {
            4
        };
        name + self.button_widths[level.min(1)].iter().sum::<i32>() + 8
    }

    fn piece(&mut self, which: Which) -> &mut Piece {
        match which {
            Which::Bar => &mut self.bar,
            Which::Card => &mut self.card,
            Which::Tip => &mut self.tip,
        }
    }

    /// Places `which` at (x, y) in the session window with this picture, its corners cut round by `radius`.
    fn show(&mut self, which: Which, x: i32, y: i32, canvas: Canvas, radius: i32) {
        let (width, height) = (canvas.width, canvas.height);
        let connection = &self.connection;
        let piece = match which {
            Which::Bar => &mut self.bar,
            Which::Card => &mut self.card,
            Which::Tip => &mut self.tip,
        };
        let place = (x, y, width, height);
        if piece.place != place || !piece.shown {
            let _ = connection.configure_window(
                piece.window,
                &ConfigureWindowAux::new()
                    .x(x)
                    .y(y)
                    .width(width.max(1) as u32)
                    .height(height.max(1) as u32)
                    .stack_mode(StackMode::ABOVE),
            );
            // Round corners: each row of the window, shortened where the corners are cut.
            let rows: Vec<Rectangle> = (0..height)
                .map(|row| {
                    let r = radius as f32;
                    let from_edge = (row as f32 + 0.5).min(height as f32 - row as f32 - 0.5);
                    let inset = if from_edge < r {
                        (r - (r * r - (r - from_edge).powi(2)).max(0.0).sqrt()).round() as i32
                    } else {
                        0
                    };
                    Rectangle {
                        x: inset as i16,
                        y: row as i16,
                        width: (width - 2 * inset).max(0) as u16,
                        height: 1,
                    }
                })
                .collect();
            let _ = connection.shape_rectangles(
                shape::SO::SET,
                shape::SK::BOUNDING,
                ClipOrdering::Y_SORTED,
                piece.window,
                0,
                0,
                &rows,
            );
            if !piece.shown {
                let _ = connection.map_window(piece.window);
                piece.shown = true;
            }
            piece.place = place;
        }
        piece.picture = Some(canvas);
        self.put(which);
    }

    /// Puts the piece's picture on its window (drawn, or exposed again).
    fn put(&mut self, which: Which) {
        let (gc, depth) = (self.gc, self.depth);
        let connection = &self.connection;
        let piece = match which {
            Which::Bar => &self.bar,
            Which::Card => &self.card,
            Which::Tip => &self.tip,
        };
        if let Some(canvas) = &piece.picture
            && piece.shown
        {
            let _ = connection.put_image(
                ImageFormat::Z_PIXMAP,
                piece.window,
                gc,
                canvas.width as u16,
                canvas.height as u16,
                0,
                0,
                0,
                depth,
                &canvas.bytes(),
            );
        }
    }

    fn hide(&mut self, which: Which) {
        let connection = &self.connection;
        let piece = match which {
            Which::Bar => &mut self.bar,
            Which::Card => &mut self.card,
            Which::Tip => &mut self.tip,
        };
        if piece.shown {
            let _ = connection.unmap_window(piece.window);
            piece.shown = false;
        }
    }

    fn draw_bar(&mut self) {
        if !self.bar_out {
            // The tab: a small gold-edged pill at the top centre.
            let (width, height) = (64, 10);
            let mut tab = Canvas::new(width, height, INK);
            tab.fill(16, height / 2 - 1, width - 32, 2, GOLD);
            self.places.clear();
            self.hover = None;
            self.show(Which::Bar, (self.area.0 - width) / 2, 4, tab, height / 2);
            self.hide(Which::Tip);
            return;
        }
        let (width, height) = (self.width_at(self.level), 36);
        let mut bar = Canvas::new(width, height, INK);
        let mut x = 4;
        if self.level < 2 {
            x = 14;
            bar.text(&self.name, x, height / 2, NAME_SIZE, TEXT);
            x += self.name_width + 10;
            // The delay's dot and the session's time, as UU Remote shows its signal and time.
            let dot = match self.figures.delay_ms {
                Some(ms) if ms < 30.0 => GOOD,
                Some(ms) if ms < 80.0 => FAIR,
                Some(_) => POOR,
                None => MUTED,
            };
            bar.fill_round(x, height / 2 - 3, 7, 7, 3, dot);
            let elapsed = self.figures.elapsed.as_secs();
            let clock = format!(
                "{:02}:{:02}:{:02}",
                elapsed / 3600,
                elapsed / 60 % 60,
                elapsed % 60
            );
            bar.text(&clock, x + 12, height / 2, SMALL_SIZE, MUTED);
            x += CLOCK_WIDTH;
            bar.fill(x, height / 2 - 8, 1, 16, DIVIDER);
            x += 4;
        }
        let mut places = Vec::new();
        let widths = self.button_widths[self.level.min(1)].clone();
        for (index, (button, button_width)) in self.buttons.iter().zip(&widths).enumerate() {
            let place = (x, 4, *button_width, height - 8);
            let end = button.id == BAR_END;
            let hovered = self.hover == Some(index);
            if end || hovered {
                let colour = match (end, hovered) {
                    (true, true) => END_HOVER,
                    (true, false) => END_RED,
                    _ => HOVER,
                };
                bar.fill_round(place.0, place.1, place.2, place.3, 14, colour);
            }
            let colour = if end { 0x00FF_FFFF } else { TEXT };
            bar.icon(
                button.glyph,
                x + 12,
                height / 2 - 8,
                if end { colour } else { GOLD },
            );
            if self.level == 0 && !button.label.is_empty() {
                bar.text(
                    &button.label,
                    x + 12 + 16 + 6,
                    height / 2,
                    LABEL_SIZE,
                    colour,
                );
            }
            places.push((place, button.id));
            x += button_width;
        }
        // A gold hairline along the top, Queqiao's touch.
        bar.fill(14, 0, width - 28, 1, GOLD);
        let left = (self.area.0 - width) / 2;
        self.show(Which::Bar, left, 8, bar, height / 2);
        // Without labels on the bar, the one under the pointer shows beneath it.
        let tip = self
            .hover
            .filter(|_| self.level > 0)
            .and_then(|index| Some((places.get(index)?.0, self.buttons.get(index)?.label.clone())))
            .filter(|(_, label)| !label.is_empty());
        self.places = places;
        match tip {
            Some((place, label)) => {
                let tip_width = text_width(&label, LABEL_SIZE) + 20;
                let tip_height = 26;
                let mut canvas = Canvas::new(tip_width, tip_height, INK);
                canvas.text(&label, 10, tip_height / 2, LABEL_SIZE, TEXT);
                let tip_left = left + place.0 + place.2 / 2 - tip_width / 2;
                self.show(Which::Tip, tip_left, 8 + height + 6, canvas, 8);
            }
            None => self.hide(Which::Tip),
        }
    }

    fn draw_card(&mut self) {
        let (width, height) = (214, 176);
        let mut card = Canvas::new(width, height, INK);
        let pad = 12;
        let figures = &self.figures;
        // The path and the delay, coloured by how good it is.
        let (dot, delay) = match figures.delay_ms {
            Some(ms) if ms < 30.0 => (GOOD, format!("{ms:.0} ms")),
            Some(ms) if ms < 80.0 => (FAIR, format!("{ms:.0} ms")),
            Some(ms) => (POOR, format!("{ms:.0} ms")),
            None => (MUTED, "- ms".to_string()),
        };
        card.fill_round(pad, 14, 8, 8, 4, dot);
        let path = if figures.relayed {
            tr("Relayed")
        } else {
            tr("Direct")
        };
        card.text(path, pad + 14, 18, NAME_SIZE, TEXT);
        card.text_right(&delay, width - pad, 18, NAME_SIZE, dot);
        let elapsed = figures.elapsed.as_secs();
        let dash = |value: &str| {
            if value.is_empty() {
                "-".to_string()
            } else {
                value.to_string()
            }
        };
        let rows = [
            (tr("Frame rate"), format!("{:.0} fps", figures.fps)),
            (tr("Data"), dash(&figures.rate)),
            (tr("Codec"), dash(&figures.codec)),
            (
                tr("Picture"),
                if figures.picture.0 > 0 {
                    format!("{} × {}", figures.picture.0, figures.picture.1)
                } else {
                    "-".to_string()
                },
            ),
            (
                tr("Time"),
                format!(
                    "{:02}:{:02}:{:02}",
                    elapsed / 3600,
                    elapsed / 60 % 60,
                    elapsed % 60
                ),
            ),
        ];
        for (index, (label, value)) in rows.iter().enumerate() {
            let middle = 42 + index as i32 * 20;
            card.text(label, pad, middle, SMALL_SIZE, MUTED);
            card.text_right(value, width - pad, middle, LABEL_SIZE, TEXT);
        }
        // The delay's recent course, as bars (taller is slower).
        let (graph_top, graph_height) = (140, 24);
        let delays: Vec<f64> = figures
            .delays
            .iter()
            .rev()
            .take(40)
            .rev()
            .copied()
            .collect();
        if !delays.is_empty() {
            let top_value = delays.iter().copied().fold(20.0_f64, f64::max);
            let step = (width - 2 * pad) / 40;
            for (index, value) in delays.iter().enumerate() {
                let bar = ((value / top_value) * f64::from(graph_height)).max(1.0) as i32;
                let colour = if *value < 30.0 {
                    GOOD
                } else if *value < 80.0 {
                    FAIR
                } else {
                    POOR
                };
                card.fill(
                    pad + index as i32 * step,
                    graph_top + graph_height - bar,
                    (step - 1).max(1),
                    bar,
                    colour,
                );
            }
        }
        card.fill(14, 0, width - 28, 1, GOLD);
        let (left, top) = (self.area.0 - width - 12, self.area.1 - height - 12);
        self.show(Which::Card, left, top, card, 12);
    }
}

/// What the bar is drawn with: out or tucked, its level, the button under the pointer, the window's size.
type Drawn = (bool, usize, Option<usize>, (i32, i32));

#[derive(Clone, Copy)]
enum Which {
    Bar,
    Card,
    Tip,
}

impl Drop for SessionOverlay {
    fn drop(&mut self) {
        for window in [self.bar.window, self.card.window, self.tip.window] {
            let _ = self.connection.destroy_window(window);
        }
        let _ = self.connection.free_gc(self.gc);
        let _ = self.connection.flush();
    }
}
