//! The session window's overlays, in Queqiao's night-blue and gold: a slim bar at the top centre with the session's
//! commands, which tucks away into a small tab when the pointer leaves it, and a card at the bottom right with the
//! session's figures (direct or relayed, the delay, frames, data, codec, picture, time, the delay's recent course).
//! Each is a layered popup owned by the session window, so it moves, hides and minimises with it and never covers
//! other programs; they are drawn with GDI into a 32-bit bitmap given per-pixel alpha (rounded, translucent). The bar
//! sends its commands to the session window as its menu items do (WM_SYSCOMMAND), "More" opens that same menu.

use crate::model::{BAR_END, BAR_MORE, BarButton, Figures};
use std::cell::RefCell;
use std::ffi::c_void;
use std::sync::Once;
use std::time::{Duration, Instant};

use queqiao_core::text::tr;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    AC_SRC_ALPHA, AC_SRC_OVER, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION,
    CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, ClientToScreen, CreateCompatibleDC, CreateDIBSection,
    CreateFontW, CreateSolidBrush, DEFAULT_CHARSET, DIB_RGB_COLORS, DT_CENTER, DT_END_ELLIPSIS,
    DT_LEFT, DT_NOPREFIX, DT_RIGHT, DT_SINGLELINE, DT_VCENTER, DT_WORDBREAK, DeleteDC,
    DeleteObject, DrawTextW, FF_DONTCARE, FillRect, GetDC, HDC, HFONT, OUT_DEFAULT_PRECIS,
    ReleaseDC, SelectObject, SetBkMode, SetTextColor, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, Result, w};

const CLASS: PCWSTR = w!("QueqiaoOverlay");
static REGISTER: Once = Once::new();

// Colours (0x00RRGGBB here: the bitmap is BGRA, written by hand).
const INK: u32 = 0x0014_1A3B;
const GOLD: u32 = 0x00E3_B45A;
const TEXT: u32 = 0x00EE_F0FA;
const MUTED: u32 = 0x00A9_B1D6;
const END_RED: u32 = 0x00C8_4A3C;
const GOOD: u32 = 0x006F_D3A0;
const FAIR: u32 = 0x00E8_B04A;
const POOR: u32 = 0x00E8_6A5A;
/// How opaque the bar and the card are (of 255).
const OPACITY: u32 = 232;
/// WM_MOUSELEAVE (winuser.h): the pointer left the bar (asked for with TrackMouseEvent).
const WM_MOUSELEAVE: u32 = 0x02A3;
/// How long the bar stays after the pointer left it.
const BAR_STAYS: Duration = Duration::from_millis(1500);
/// The pointer this near the top of the picture brings the bar out (96-DPI units).
const BAR_REACH: i32 = 56;

/// What the bar's window procedure needs: its layout and the session window it serves.
struct BarState {
    owner: HWND,
    scale: f64,
    /// Each button's place (pixels, in the bar) and id.
    places: Vec<(RECT, u16)>,
    hover: Option<usize>,
}

thread_local! {
    static BAR: RefCell<Option<BarState>> = const { RefCell::new(None) };
}

pub struct SessionOverlay {
    owner: HWND,
    scale: f64,
    name: String,
    buttons: Vec<BarButton>,
    bar: HWND,
    card: HWND,
    /// The label of the button under the pointer, under the bar, when the bar shows no labels.
    tip: HWND,
    /// The bar is out (else only its tab shows).
    bar_out: bool,
    last_near: Instant,
    card_on: bool,
    figures: Figures,
    /// Where the bar and card were put last (screen), to move them only when the window moved.
    placed: (RECT, bool, bool),
    hover_drawn: Option<usize>,
    fonts: (HFONT, HFONT, HFONT, HFONT),
    /// Tests: the bar stays out (for a picture of it).
    keep_out: bool,
    /// The name's width, and each button's with its label and without (pixels).
    name_width: i32,
    button_widths: [Vec<i32>; 2],
    /// How much of the bar shows (see `width_at`), by the window's width.
    level: usize,
}

impl SessionOverlay {
    /// The overlays of the session window `owner` (the other PC's `name`), with the bar's `buttons`.
    pub fn new(owner: HWND, name: &str, buttons: Vec<BarButton>) -> Result<SessionOverlay> {
        let instance = unsafe { GetModuleHandleW(None) }?;
        REGISTER.call_once(|| unsafe {
            let class = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(overlay_proc),
                hInstance: instance.into(),
                hCursor: LoadCursorW(None, IDC_HAND).unwrap_or_default(),
                lpszClassName: CLASS,
                ..Default::default()
            };
            RegisterClassExW(&class);
        });
        let make = |ex: WINDOW_EX_STYLE| unsafe {
            CreateWindowExW(
                ex | WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                CLASS,
                PCWSTR::null(),
                WS_POPUP,
                0,
                0,
                1,
                1,
                Some(owner),
                None,
                Some(instance.into()),
                None,
            )
        };
        let bar = make(WINDOW_EX_STYLE(0))?;
        // The card lets the mouse through to the picture.
        let card = make(WS_EX_TRANSPARENT)?;
        let tip = make(WS_EX_TRANSPARENT)?;
        let dpi = unsafe { GetDpiForWindow(owner) }.max(96);
        let scale = f64::from(dpi) / 96.0;
        let font = |size: i32, weight: i32, face: PCWSTR| unsafe {
            CreateFontW(
                -px(size, scale),
                0,
                0,
                0,
                weight,
                0,
                0,
                0,
                DEFAULT_CHARSET,
                OUT_DEFAULT_PRECIS,
                CLIP_DEFAULT_PRECIS,
                CLEARTYPE_QUALITY,
                FF_DONTCARE.0 as u32,
                face,
            )
        };
        let fonts = (
            font(13, 400, w!("Segoe UI")),
            font(13, 600, w!("Segoe UI")),
            font(15, 400, w!("Segoe MDL2 Assets")),
            font(12, 400, w!("Segoe UI")),
        );
        BAR.with_borrow_mut(|state| {
            *state = Some(BarState {
                owner,
                scale,
                places: Vec::new(),
                hover: None,
            })
        });
        let mut overlay = SessionOverlay {
            owner,
            scale,
            name: name.to_string(),
            buttons,
            bar,
            card,
            tip,
            bar_out: true,
            last_near: Instant::now(),
            card_on: false,
            figures: Figures::default(),
            placed: (RECT::default(), false, false),
            hover_drawn: None,
            fonts,
            keep_out: false,
            name_width: 0,
            button_widths: [Vec::new(), Vec::new()],
            level: 0,
        };
        overlay.measure();
        Ok(overlay)
    }

    /// Shows or hides the card of figures.
    pub fn set_card(&mut self, on: bool) {
        self.card_on = on;
        self.placed.2 = !on;
        self.tick();
    }

    pub fn card_shown(&self) -> bool {
        self.card_on
    }

    /// Tests: keeps the bar out whatever the pointer does (for a picture of it).
    pub fn keep_bar_out(&mut self, on: bool) {
        self.keep_out = on;
    }

    /// Tests: a click on the bar's button `id` (as the mouse gives it to the bar's window): false when the bar does not
    /// show that button now.
    pub fn click_for_test(&self, id: u16) -> bool {
        let place = BAR.with_borrow(|state| {
            state
                .as_ref()?
                .places
                .iter()
                .find(|(_, place_id)| *place_id == id)
                .map(|(place, _)| *place)
        });
        let Some(place) = place else {
            return false;
        };
        let (x, y) = (
            (place.left + place.right) / 2,
            (place.top + place.bottom) / 2,
        );
        let point = ((y as u32 & 0xFFFF) << 16) | (x as u32 & 0xFFFF);
        unsafe {
            SendMessageW(
                self.bar,
                WM_LBUTTONUP,
                Some(WPARAM(0)),
                Some(LPARAM(point as isize)),
            );
        }
        true
    }

    /// The bottom right of the bar's button `id` on the screen (where the control center opens), while it shows.
    pub fn anchor_of(&self, id: u16) -> Option<(i32, i32)> {
        let place = BAR.with_borrow(|state| {
            state
                .as_ref()?
                .places
                .iter()
                .find(|(_, place_id)| *place_id == id)
                .map(|(place, _)| *place)
        })?;
        let mut corner = POINT {
            x: place.right,
            y: place.bottom,
        };
        let _ = unsafe { ClientToScreen(self.bar, &mut corner) };
        Some((corner.x, corner.y))
    }

    /// Tests: the button `index` drawn as under the pointer (its highlight, and its tip when the bar has no labels).
    pub fn hover_for_test(&mut self, index: Option<usize>) {
        BAR.with_borrow_mut(|state| {
            if let Some(state) = state {
                state.hover = index;
            }
        });
    }

    /// New figures (once a second): the card is drawn again.
    pub fn set_figures(&mut self, figures: Figures) {
        self.figures = figures;
        if self.card_on {
            self.draw_card();
        }
        // The bar's clock and delay dot.
        if self.bar_out && self.level < 2 && self.placed.0.right > self.placed.0.left {
            self.draw_bar(self.placed.0);
        }
    }

    /// Follows the session window, and brings the bar out or tucks it away by where the pointer is. Called often
    /// (the session's loop); it only redraws what changed.
    pub fn tick(&mut self) {
        let mut client = RECT::default();
        let mut corner = POINT::default();
        unsafe {
            if IsIconic(self.owner).as_bool() || !IsWindowVisible(self.owner).as_bool() {
                return;
            }
            let _ = GetClientRect(self.owner, &mut client);
            let _ = ClientToScreen(self.owner, &mut corner);
        }
        let area = RECT {
            left: corner.x,
            top: corner.y,
            right: corner.x + client.right,
            bottom: corner.y + client.bottom,
        };
        // As much of the bar as the window has room for.
        let room = area.right - area.left - px(24, self.scale);
        let level = (0..2)
            .find(|&level| self.width_at(level) <= room)
            .unwrap_or(2);
        let relaid = level != self.level;
        self.level = level;
        let mut cursor = POINT::default();
        let _ = unsafe { GetCursorPos(&mut cursor) };
        let (bar_width, bar_height) = self.bar_size();
        let bar_left = area.left + (area.right - area.left - bar_width) / 2;
        let bar_top = area.top + px(8, self.scale);
        let over_bar = (bar_left..bar_left + bar_width).contains(&cursor.x)
            && (bar_top..bar_top + bar_height).contains(&cursor.y);
        let near_top = (area.left..area.right).contains(&cursor.x)
            && (area.top..area.top + px(BAR_REACH, self.scale)).contains(&cursor.y);
        if over_bar || near_top {
            self.last_near = Instant::now();
        }
        let out = self.keep_out || self.last_near.elapsed() < BAR_STAYS;
        let hover = BAR.with_borrow(|state| state.as_ref().and_then(|state| state.hover));
        let moved = self.placed.0 != area;
        if moved || relaid || out != self.bar_out || !self.placed.1 || hover != self.hover_drawn {
            self.bar_out = out;
            self.hover_drawn = hover;
            self.draw_bar(area);
            self.placed.1 = true;
        }
        if self.card_on && (moved || !self.placed.2) {
            self.draw_card_at(area);
            self.placed.2 = true;
        } else if !self.card_on && self.placed.2 {
            let _ = unsafe { ShowWindow(self.card, SW_HIDE) };
            self.placed.2 = false;
        }
        self.placed.0 = area;
    }

    /// The name's width and the buttons', with their labels and without, measured once.
    fn measure(&mut self) {
        let dc = unsafe { GetDC(None) };
        self.name_width = text_width(dc, &self.name, self.fonts.1);
        self.button_widths = [true, false].map(|labelled| {
            self.buttons
                .iter()
                .map(|button| self.button_width(dc, button, labelled))
                .collect()
        });
        unsafe { ReleaseDC(None, dc) };
    }

    /// The bar's width at `level`: 0 all of it, 1 without the buttons' labels, 2 without the name either.
    fn width_at(&self, level: usize) -> i32 {
        let name = if level < 2 {
            px(14, self.scale)
                + self.name_width
                + px(10, self.scale)
                + self.clock_width()
                + px(14, self.scale)
        } else {
            px(4, self.scale)
        };
        name + self.button_widths[level.min(1)].iter().sum::<i32>() + px(8, self.scale)
    }

    /// The delay dot and the session's time after the name ("● 00:12:34"), as UU Remote shows its signal and time.
    fn clock_width(&self) -> i32 {
        px(14, self.scale) + px(54, self.scale)
    }

    /// The bar's size when out (pixels).
    fn bar_size(&self) -> (i32, i32) {
        (self.width_at(self.level), px(36, self.scale))
    }

    fn button_width(&self, dc: HDC, button: &BarButton, labelled: bool) -> i32 {
        let label = if !labelled || button.label.is_empty() {
            0
        } else {
            px(6, self.scale) + text_width(dc, &button.label, self.fonts.0)
        };
        px(12, self.scale) + px(16, self.scale) + label + px(12, self.scale)
    }

    fn draw_bar(&mut self, area: RECT) {
        let s = self.scale;
        if !self.bar_out {
            // The tab: a small gold-edged pill at the top centre.
            let (width, height) = (px(64, s), px(10, s));
            let left = area.left + (area.right - area.left - width) / 2;
            let top = area.top + px(4, s);
            BAR.with_borrow_mut(|state| {
                if let Some(state) = state {
                    state.places.clear();
                    state.hover = None;
                }
            });
            paint_layered(self.bar, left, top, width, height, height, |dc, w, h| {
                fill(dc, 0, 0, w, h, INK);
                fill(dc, px(16, s), h / 2 - 1, w - px(32, s), 2, GOLD);
            });
            let _ = unsafe { ShowWindow(self.tip, SW_HIDE) };
            return;
        }
        let (width, height) = self.bar_size();
        let left = area.left + (area.right - area.left - width) / 2;
        let top = area.top + px(8, s);
        let fonts = self.fonts;
        let name = self.name.clone();
        let buttons = self.buttons.clone();
        let hover = self.hover_drawn;
        let mut places = Vec::new();
        let level = self.level;
        let widths = self.button_widths[level.min(1)].clone();
        let name_width = self.name_width;
        let clock_width = self.clock_width();
        let elapsed = self.figures.elapsed.as_secs();
        let clock = format!(
            "{:02}:{:02}:{:02}",
            elapsed / 3600,
            elapsed / 60 % 60,
            elapsed % 60
        );
        let dot = match self.figures.delay_ms {
            Some(ms) if ms < 30.0 => GOOD,
            Some(ms) if ms < 80.0 => FAIR,
            Some(_) => POOR,
            None => MUTED,
        };
        paint_layered(self.bar, left, top, width, height, height, |dc, w, h| {
            fill(dc, 0, 0, w, h, INK);
            let mut x = px(4, s);
            if level < 2 {
                x = px(14, s);
                text(
                    dc,
                    &name,
                    rect(x, 0, name_width, h),
                    fonts.1,
                    TEXT,
                    DT_LEFT.0,
                );
                x += name_width + px(10, s);
                // The delay's dot and the session's time.
                let size = px(7, s);
                fill_round(dc, &rect(x, (h - size) / 2, size, size), size, dot);
                text(
                    dc,
                    &clock,
                    rect(x + px(12, s), 0, clock_width - px(12, s), h),
                    fonts.3,
                    MUTED,
                    DT_LEFT.0,
                );
                x += clock_width + px(4, s);
                fill(dc, x, h / 2 - px(8, s), 1, px(16, s), 0x003A_4370);
                x += px(4, s);
            }
            for (index, (button, width)) in buttons.iter().zip(&widths).enumerate() {
                let place = RECT {
                    left: x,
                    top: px(4, s),
                    right: x + width,
                    bottom: h - px(4, s),
                };
                let end = button.id == BAR_END;
                if end || hover == Some(index) {
                    let color = match (end, hover == Some(index)) {
                        (true, true) => 0x00DE_5C4C,
                        (true, false) => END_RED,
                        _ => 0x0026_3169,
                    };
                    fill_round(dc, &place, px(14, s), color);
                }
                let color = if end { 0x00FF_FFFF } else { TEXT };
                let mut cx = x + px(12, s);
                text(
                    dc,
                    button.glyph,
                    rect(cx, 0, px(16, s), h),
                    fonts.2,
                    if end { color } else { GOLD },
                    DT_CENTER.0,
                );
                cx += px(16, s);
                if level == 0 && !button.label.is_empty() {
                    cx += px(6, s);
                    text(
                        dc,
                        &button.label,
                        rect(cx, 0, w - cx, h),
                        fonts.0,
                        color,
                        DT_LEFT.0,
                    );
                }
                places.push((place, button.id));
                x += width;
            }
        });
        // Without labels on the bar, the one under the pointer shows beneath it.
        let tip = hover
            .filter(|_| level > 0)
            .and_then(|index| Some((places.get(index)?.0, buttons.get(index)?.label.clone())))
            .filter(|(_, label)| !label.is_empty());
        match tip {
            Some((place, label)) => {
                let dc = unsafe { GetDC(None) };
                let tip_width = text_width(dc, &label, fonts.0) + px(20, s);
                unsafe { ReleaseDC(None, dc) };
                let tip_height = px(26, s);
                let tip_left = left + (place.left + place.right) / 2 - tip_width / 2;
                let tip_top = top + height + px(6, s);
                paint_layered(
                    self.tip,
                    tip_left,
                    tip_top,
                    tip_width,
                    tip_height,
                    px(8, s),
                    |dc, w, h| {
                        fill(dc, 0, 0, w, h, INK);
                        text(dc, &label, rect(0, 0, w, h), fonts.0, TEXT, DT_CENTER.0);
                    },
                );
            }
            None => {
                let _ = unsafe { ShowWindow(self.tip, SW_HIDE) };
            }
        }
        BAR.with_borrow_mut(|state| {
            if let Some(state) = state {
                state.places = places;
            }
        });
    }

    fn draw_card(&mut self) {
        let area = self.placed.0;
        if area.right > area.left {
            self.draw_card_at(area);
        }
    }

    fn draw_card_at(&mut self, area: RECT) {
        let s = self.scale;
        let (width, height) = (px(214, s), px(176, s));
        let left = area.right - width - px(12, s);
        let top = area.bottom - height - px(12, s);
        let fonts = self.fonts;
        let figures = self.figures.clone();
        paint_layered(
            self.card,
            left,
            top,
            width,
            height,
            px(12, s),
            |dc, w, _h| {
                fill(dc, 0, 0, w, height, INK);
                let pad = px(12, s);
                // The path and the delay, coloured by how good it is.
                let (dot, delay) = match figures.delay_ms {
                    Some(ms) if ms < 30.0 => (GOOD, format!("{ms:.0} ms")),
                    Some(ms) if ms < 80.0 => (FAIR, format!("{ms:.0} ms")),
                    Some(ms) => (POOR, format!("{ms:.0} ms")),
                    None => (MUTED, "- ms".to_string()),
                };
                fill_round(
                    dc,
                    &RECT {
                        left: pad,
                        top: px(14, s),
                        right: pad + px(8, s),
                        bottom: px(22, s),
                    },
                    px(8, s),
                    dot,
                );
                let path = if figures.relayed {
                    tr("Relayed")
                } else {
                    tr("Direct")
                };
                text(
                    dc,
                    path,
                    rect(pad + px(14, s), px(8, s), w - 2 * pad, px(20, s)),
                    fonts.1,
                    TEXT,
                    DT_LEFT.0,
                );
                text(
                    dc,
                    &delay,
                    rect(pad, px(8, s), w - 2 * pad, px(20, s)),
                    fonts.1,
                    dot,
                    DT_RIGHT.0,
                );
                let elapsed = figures.elapsed.as_secs();
                let rows = [
                    (tr("Frame rate"), format!("{:.0} fps", figures.fps)),
                    (tr("Data"), or_dash(&figures.rate)),
                    (tr("Codec"), or_dash(&figures.codec)),
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
                    let y = px(32, s) + index as i32 * px(20, s);
                    text(
                        dc,
                        label,
                        rect(pad, y, w - 2 * pad, px(20, s)),
                        fonts.3,
                        MUTED,
                        DT_LEFT.0,
                    );
                    text(
                        dc,
                        value,
                        rect(pad, y, w - 2 * pad, px(20, s)),
                        fonts.0,
                        TEXT,
                        DT_RIGHT.0,
                    );
                }
                // The delay's recent course, as bars (taller is slower).
                let graph_top = px(140, s);
                let graph_height = px(24, s);
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
                    let step = (w - 2 * pad) / 40;
                    for (index, value) in delays.iter().enumerate() {
                        let bar = ((value / top_value) * f64::from(graph_height)).max(1.0) as i32;
                        let color = if *value < 30.0 {
                            GOOD
                        } else if *value < 80.0 {
                            FAIR
                        } else {
                            POOR
                        };
                        let x = pad + index as i32 * step;
                        fill(
                            dc,
                            x,
                            graph_top + graph_height - bar,
                            (step - 1).max(1),
                            bar,
                            color,
                        );
                    }
                }
                // A gold hairline along the top, Queqiao's touch.
                fill(dc, px(14, s), 0, w - px(28, s), 1, GOLD);
            },
        );
        let _ = unsafe { ShowWindow(self.card, SW_SHOWNOACTIVATE) };
    }
}

impl Drop for SessionOverlay {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyWindow(self.bar);
            let _ = DestroyWindow(self.card);
            let _ = DestroyWindow(self.tip);
            for font in [self.fonts.0, self.fonts.1, self.fonts.2, self.fonts.3] {
                let _ = DeleteObject(font.into());
            }
        }
        BAR.with_borrow_mut(|state| *state = None);
    }
}

/// The value, or "-" while there is none yet.
fn or_dash(value: &str) -> String {
    if value.is_empty() {
        "-".to_string()
    } else {
        value.to_string()
    }
}

pub(crate) fn px(value: i32, scale: f64) -> i32 {
    (f64::from(value) * scale).round() as i32
}

pub(crate) fn rect(x: i32, y: i32, width: i32, height: i32) -> RECT {
    RECT {
        left: x,
        top: y,
        right: x + width,
        bottom: y + height,
    }
}

/// COLORREF from 0x00RRGGBB.
pub(crate) fn colorref(rgb: u32) -> COLORREF {
    COLORREF(((rgb & 0xFF) << 16) | (rgb & 0xFF00) | ((rgb >> 16) & 0xFF))
}

pub(crate) fn fill(dc: HDC, x: i32, y: i32, width: i32, height: i32, rgb: u32) {
    unsafe {
        let brush = CreateSolidBrush(colorref(rgb));
        FillRect(dc, &rect(x, y, width, height), brush);
        let _ = DeleteObject(brush.into());
    }
}

pub(crate) fn fill_round(dc: HDC, area: &RECT, round: i32, rgb: u32) {
    use windows::Win32::Graphics::Gdi::{GetStockObject, NULL_PEN, RoundRect};
    unsafe {
        let brush = CreateSolidBrush(colorref(rgb));
        let old_brush = SelectObject(dc, brush.into());
        let old_pen = SelectObject(dc, GetStockObject(NULL_PEN));
        let _ = RoundRect(
            dc,
            area.left,
            area.top,
            area.right + 1,
            area.bottom + 1,
            round,
            round,
        );
        SelectObject(dc, old_pen);
        SelectObject(dc, old_brush);
        let _ = DeleteObject(brush.into());
    }
}

pub(crate) fn text(dc: HDC, value: &str, area: RECT, font: HFONT, rgb: u32, align: u32) {
    // DrawTextW reads the text even when told it is 0 long: an empty Vec's dangling pointer crashes it.
    if value.is_empty() {
        return;
    }
    let mut value: Vec<u16> = value.encode_utf16().collect();
    let mut area = area;
    unsafe {
        let old = SelectObject(dc, font.into());
        SetBkMode(dc, TRANSPARENT);
        SetTextColor(dc, colorref(rgb));
        DrawTextW(
            dc,
            &mut value,
            &mut area,
            windows::Win32::Graphics::Gdi::DRAW_TEXT_FORMAT(
                align | DT_SINGLELINE.0 | DT_VCENTER.0 | DT_END_ELLIPSIS.0 | DT_NOPREFIX.0,
            ),
        );
        SelectObject(dc, old);
    }
}

pub(crate) fn text_width(dc: HDC, value: &str, font: HFONT) -> i32 {
    // DrawTextW reads the text even when told it is 0 long: an empty Vec's dangling pointer crashes it.
    if value.is_empty() {
        return 0;
    }
    let mut value: Vec<u16> = value.encode_utf16().collect();
    let mut area = RECT::default();
    unsafe {
        let old = SelectObject(dc, font.into());
        DrawTextW(
            dc,
            &mut value,
            &mut area,
            windows::Win32::Graphics::Gdi::DRAW_TEXT_FORMAT(
                0x400 | DT_SINGLELINE.0 | DT_NOPREFIX.0,
            ),
        );
        SelectObject(dc, old);
    }
    area.right - area.left
}

/// Text broken into lines within `area`'s width (from its top, left aligned).
pub(crate) fn text_wrapped(dc: HDC, value: &str, area: RECT, font: HFONT, rgb: u32) {
    if value.is_empty() {
        return;
    }
    let mut value: Vec<u16> = value.encode_utf16().collect();
    let mut area = area;
    unsafe {
        let old = SelectObject(dc, font.into());
        SetBkMode(dc, TRANSPARENT);
        SetTextColor(dc, colorref(rgb));
        DrawTextW(
            dc,
            &mut value,
            &mut area,
            windows::Win32::Graphics::Gdi::DRAW_TEXT_FORMAT(
                DT_LEFT.0 | DT_WORDBREAK.0 | DT_NOPREFIX.0,
            ),
        );
        SelectObject(dc, old);
    }
}

/// The height of `value` broken into lines `width` wide.
pub(crate) fn wrapped_height(dc: HDC, value: &str, width: i32, font: HFONT) -> i32 {
    if value.is_empty() {
        return 0;
    }
    let mut value: Vec<u16> = value.encode_utf16().collect();
    let mut area = rect(0, 0, width, 0);
    unsafe {
        let old = SelectObject(dc, font.into());
        DrawTextW(
            dc,
            &mut value,
            &mut area,
            windows::Win32::Graphics::Gdi::DRAW_TEXT_FORMAT(
                0x400 | DT_LEFT.0 | DT_WORDBREAK.0 | DT_NOPREFIX.0,
            ),
        );
        SelectObject(dc, old);
    }
    area.bottom - area.top
}

/// Draws `paint` into a 32-bit bitmap of `width` x `height`, gives it rounded corners (`round` radius) at OPACITY,
/// and puts it on the layered window `hwnd` at (left, top) on the screen.
fn paint_layered(
    hwnd: HWND,
    left: i32,
    top: i32,
    width: i32,
    height: i32,
    round: i32,
    paint: impl FnOnce(HDC, i32, i32),
) {
    paint_layered_at(hwnd, left, top, width, height, round, OPACITY, paint);
}

/// As `paint_layered`, `opacity` of 255.
#[allow(clippy::too_many_arguments)]
pub(crate) fn paint_layered_at(
    hwnd: HWND,
    left: i32,
    top: i32,
    width: i32,
    height: i32,
    round: i32,
    opacity: u32,
    paint: impl FnOnce(HDC, i32, i32),
) {
    if width <= 0 || height <= 0 {
        return;
    }
    unsafe {
        let screen = GetDC(None);
        let memory = CreateCompatibleDC(Some(screen));
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut c_void = std::ptr::null_mut();
        let Ok(bitmap) = CreateDIBSection(Some(memory), &info, DIB_RGB_COLORS, &mut bits, None, 0)
        else {
            let _ = DeleteDC(memory);
            ReleaseDC(None, screen);
            return;
        };
        let old = SelectObject(memory, bitmap.into());
        paint(memory, width, height);
        // GDI leaves the alpha bytes alone (or zero): set each pixel's from the rounded shape, premultiplied.
        let pixels = std::slice::from_raw_parts_mut(bits.cast::<u32>(), (width * height) as usize);
        let r = f64::from(round.min(width / 2).min(height / 2));
        for y in 0..height {
            for x in 0..width {
                let mut coverage = 0.0;
                for (dx, dy) in [(0.25, 0.25), (0.75, 0.25), (0.25, 0.75), (0.75, 0.75)] {
                    let (fx, fy) = (f64::from(x) + dx, f64::from(y) + dy);
                    let cx = fx.clamp(r, f64::from(width) - r);
                    let cy = fy.clamp(r, f64::from(height) - r);
                    if (fx - cx).powi(2) + (fy - cy).powi(2) <= r * r {
                        coverage += 0.25;
                    }
                }
                let alpha = (f64::from(opacity.min(255)) * coverage).round() as u32;
                let pixel = &mut pixels[(y * width + x) as usize];
                let scale = |channel: u32| (channel * alpha + 127) / 255;
                let (b, g, red) = (*pixel & 0xFF, (*pixel >> 8) & 0xFF, (*pixel >> 16) & 0xFF);
                *pixel = (alpha << 24) | (scale(red) << 16) | (scale(g) << 8) | scale(b);
            }
        }
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        let position = POINT { x: left, y: top };
        let size = SIZE {
            cx: width,
            cy: height,
        };
        let source = POINT::default();
        let _ = UpdateLayeredWindow(
            hwnd,
            Some(screen),
            Some(&raw const position),
            Some(&raw const size),
            Some(memory),
            Some(&raw const source),
            COLORREF(0),
            Some(&raw const blend),
            ULW_ALPHA,
        );
        if !IsWindowVisible(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        }
        SelectObject(memory, old);
        let _ = DeleteObject(bitmap.into());
        let _ = DeleteDC(memory);
        ReleaseDC(None, screen);
    }
}

extern "system" fn overlay_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_MOUSEMOVE => {
            let (x, y) = (
                (lparam.0 & 0xFFFF) as i16 as i32,
                ((lparam.0 >> 16) & 0xFFFF) as i16 as i32,
            );
            BAR.with_borrow_mut(|state| {
                if let Some(state) = state {
                    state.hover = state.places.iter().position(|(place, _)| {
                        (place.left..place.right).contains(&x)
                            && (place.top..place.bottom).contains(&y)
                    });
                }
            });
            // Told when the pointer leaves, to drop the highlight.
            let mut track = TRACKMOUSEEVENT {
                cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: hwnd,
                dwHoverTime: 0,
            };
            let _ = unsafe { TrackMouseEvent(&mut track) };
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            BAR.with_borrow_mut(|state| {
                if let Some(state) = state {
                    state.hover = None;
                }
            });
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let (x, y) = (
                (lparam.0 & 0xFFFF) as i16 as i32,
                ((lparam.0 >> 16) & 0xFFFF) as i16 as i32,
            );
            let hit = BAR.with_borrow(|state| {
                let state = state.as_ref()?;
                let (place, id) = state.places.iter().find(|(place, _)| {
                    (place.left..place.right).contains(&x) && (place.top..place.bottom).contains(&y)
                })?;
                Some((state.owner, *id, *place, state.scale))
            });
            if let Some((owner, id, place, _)) = hit {
                unsafe {
                    match id {
                        BAR_END => {
                            let _ = PostMessageW(Some(owner), WM_CLOSE, WPARAM(0), LPARAM(0));
                        }
                        BAR_MORE => {
                            // The control center, under the button: the session window opens it there.
                            let mut corner = POINT {
                                x: place.right,
                                y: place.bottom,
                            };
                            let _ = ClientToScreen(hwnd, &mut corner);
                            let packed =
                                ((corner.y as u32 & 0xFFFF) << 16) | (corner.x as u32 & 0xFFFF);
                            let _ = PostMessageW(
                                Some(owner),
                                WM_SYSCOMMAND,
                                WPARAM(usize::from(BAR_MORE)),
                                LPARAM(packed as i32 as isize),
                            );
                        }
                        id => {
                            let _ = PostMessageW(
                                Some(owner),
                                WM_SYSCOMMAND,
                                WPARAM(usize::from(id)),
                                LPARAM(0),
                            );
                        }
                    }
                }
            }
            LRESULT(0)
        }
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}
