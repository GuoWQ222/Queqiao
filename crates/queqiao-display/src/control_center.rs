//! The session window's control center on Windows (what it shows: panel.rs; Linux: linux/control_center.rs). Opened by
//! the bar's "Control center" button, as UU Remote's: a column of groups and commands under the button, the group
//! under the pointer opening its choices beside it (check marks, dots, a line of explanation, the keys that do the
//! same, the volume as a slider). In Queqiao's night ink and gold, like the bar.
//!
//! Two layered popups owned by the session window (the column, and the open group beside it), drawn with GDI as the
//! bar is. The column holds the mouse (capture) while open, so a click elsewhere closes it and never reaches the other
//! PC; the session goes on meanwhile (no modal loop: the picture keeps coming). A choice reaches the session window as
//! its menu items do (WM_SYSCOMMAND with the item's id); a slider's value as WM_PANEL_VALUE.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Once;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, ClientToScreen, CreateFontW, DEFAULT_CHARSET,
    DT_CENTER, DT_LEFT, DT_RIGHT, DeleteObject, FF_DONTCARE, GetDC, GetMonitorInfoW, HFONT,
    MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint, OUT_DEFAULT_PRECIS, ReleaseDC,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, VK_DOWN, VK_ESCAPE, VK_LEFT, VK_RETURN, VK_RIGHT, VK_SPACE, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};

use crate::overlay::{
    fill, fill_round, paint_layered_at, px, rect, text, text_width, text_wrapped, wrapped_height,
};
use crate::panel::{Mark, PanelItem};

const CLASS: PCWSTR = w!("QueqiaoControlCenter");
static REGISTER: Once = Once::new();

/// What the control center sends the session window for a slider's new value (wparam: its id, lparam: 0..=100).
pub(crate) const WM_PANEL_VALUE: u32 = WM_APP + 0x52;

// Colours (0x00RRGGBB): Queqiao's night ink and gold, as on the bar.
const INK: u32 = 0x0014_1A3B;
const HOVER: u32 = 0x0026_3169;
const GOLD: u32 = 0x00E3_B45A;
const TEXT: u32 = 0x00EE_F0FA;
const MUTED: u32 = 0x00A9_B1D6;
const DIM: u32 = 0x0066_6E99;
const DIVIDER: u32 = 0x002E_3666;
const DANGER: u32 = 0x00FF_8A7A;
const TRACK: u32 = 0x003A_4370;
/// Nearly opaque: text over the remote picture must stay readable.
const OPAQUE: u32 = 250;

// Sizes in 96-DPI units.
const MAIN_WIDTH: i32 = 228;
const SUB_WIDTH: i32 = 304;
const ITEM: i32 = 36;
const SUB_ITEM: i32 = 32;
const SLIDER: i32 = 54;
const HEADING: i32 = 28;
const SEPARATOR: i32 = 9;
const PAD: i32 = 8;
const GAP: i32 = 6;

/// An item's place in its column (pixels from the column's top).
#[derive(Clone, Copy, Default)]
struct Place {
    top: i32,
    height: i32,
}

struct Fonts {
    label: HFONT,
    glyph: HFONT,
    detail: HFONT,
    heading: HFONT,
    /// The groups' chevron (a glyph).
    chevron: HFONT,
}

struct Center {
    owner: HWND,
    main: HWND,
    sub: HWND,
    scale: f64,
    items: Vec<PanelItem>,
    checks: HashMap<u16, bool>,
    fonts: Fonts,
    main_rect: RECT,
    sub_rect: Option<RECT>,
    main_places: Vec<Place>,
    sub_places: Vec<Place>,
    /// The group open beside the column (its index in `items`).
    open: Option<usize>,
    hover_main: Option<usize>,
    hover_sub: Option<usize>,
    /// The keyboard works in the open group's column.
    in_sub: bool,
    /// A slider of the open group being dragged (its index there).
    dragging: Option<usize>,
    work: RECT,
}

thread_local! {
    static CENTER: RefCell<Option<Center>> = const { RefCell::new(None) };
}

/// What the window procedure does once the state is let go (destroying windows sends messages back to it).
enum Then {
    Nothing,
    Close { refocus: bool },
    Choose(HWND, u16),
    Value(HWND, u16, u8),
}

pub(crate) fn is_open() -> bool {
    CENTER.with_borrow(Option::is_some)
}

/// Opens the control center of the session window `owner` under `anchor` (the button's bottom right, on the screen),
/// with `items`; the check marks and dots as `checks` has them. `group`: open beside it from the start (tests). Already
/// open: closes it (the button works as a switch).
pub(crate) fn open(
    owner: HWND,
    anchor: POINT,
    items: Vec<PanelItem>,
    checks: HashMap<u16, bool>,
    group: Option<usize>,
) {
    if is_open() {
        close(true);
        return;
    }
    let Ok(instance) = (unsafe { GetModuleHandleW(None) }) else {
        return;
    };
    REGISTER.call_once(|| unsafe {
        let class = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(center_proc),
            hInstance: instance.into(),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            lpszClassName: CLASS,
            ..Default::default()
        };
        RegisterClassExW(&class);
    });
    let make = |extra: WINDOW_EX_STYLE| unsafe {
        CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOOLWINDOW | extra,
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
    let Ok(main) = make(WINDOW_EX_STYLE(0)) else {
        return;
    };
    let Ok(sub) = make(WS_EX_NOACTIVATE) else {
        let _ = unsafe { DestroyWindow(main) };
        return;
    };
    let scale = f64::from(unsafe { GetDpiForWindow(owner) }.max(96)) / 96.0;
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
    let fonts = Fonts {
        label: font(14, 400, w!("Segoe UI")),
        glyph: font(16, 400, w!("Segoe MDL2 Assets")),
        detail: font(12, 400, w!("Segoe UI")),
        heading: font(12, 600, w!("Segoe UI")),
        chevron: font(10, 400, w!("Segoe MDL2 Assets")),
    };
    let work = work_area(anchor);
    let mut center = Center {
        owner,
        main,
        sub,
        scale,
        items,
        checks,
        fonts,
        main_rect: RECT::default(),
        sub_rect: None,
        main_places: Vec::new(),
        sub_places: Vec::new(),
        open: None,
        hover_main: None,
        hover_sub: None,
        in_sub: false,
        dragging: None,
        work,
    };
    center.layout_main(anchor);
    if let Some(index) =
        group.filter(|index| matches!(center.items.get(*index), Some(PanelItem::Group { .. })))
    {
        center.open_group(index);
        center.hover_main = Some(index);
    }
    center.paint_main();
    center.paint_sub();
    CENTER.with_borrow_mut(|state| *state = Some(center));
    unsafe {
        let _ = SetForegroundWindow(main);
        SetCapture(main);
    }
}

/// Closes the control center; `refocus`: the session window takes the keyboard again.
pub(crate) fn close(refocus: bool) {
    let Some(center) = CENTER.with_borrow_mut(Option::take) else {
        return;
    };
    unsafe {
        let _ = ReleaseCapture();
        let _ = DestroyWindow(center.sub);
        let _ = DestroyWindow(center.main);
        for font in [
            center.fonts.label,
            center.fonts.glyph,
            center.fonts.detail,
            center.fonts.heading,
            center.fonts.chevron,
        ] {
            let _ = DeleteObject(font.into());
        }
        if refocus {
            let _ = SetForegroundWindow(center.owner);
        }
    }
}

/// The work area (screen without the taskbar) of the monitor at `point`.
fn work_area(point: POINT) -> RECT {
    let monitor = unsafe { MonitorFromPoint(point, MONITOR_DEFAULTTONEAREST) };
    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    if unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
        info.rcWork
    } else {
        RECT {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1080,
        }
    }
}

fn contains(area: &RECT, point: POINT) -> bool {
    (area.left..area.right).contains(&point.x) && (area.top..area.bottom).contains(&point.y)
}

impl Center {
    fn px(&self, value: i32) -> i32 {
        px(value, self.scale)
    }

    fn checked(&self, id: u16) -> bool {
        self.checks.get(&id).copied().unwrap_or(false)
    }

    fn group_items(&self) -> &[PanelItem] {
        match self.open.and_then(|index| self.items.get(index)) {
            Some(PanelItem::Group { items, .. }) => items,
            _ => &[],
        }
    }

    /// The column's places and its spot under the anchor (its right edge there), inside the work area.
    fn layout_main(&mut self, anchor: POINT) {
        let mut top = self.px(PAD);
        self.main_places = self
            .items
            .iter()
            .map(|item| {
                let height = self.px(match item {
                    PanelItem::Separator => SEPARATOR,
                    PanelItem::Heading(_) => HEADING,
                    PanelItem::Slider { .. } => SLIDER,
                    _ => ITEM,
                });
                let place = Place { top, height };
                top += height;
                place
            })
            .collect();
        let (width, height) = (self.px(MAIN_WIDTH), top + self.px(PAD));
        let left = (anchor.x - width)
            .min(self.work.right - width)
            .max(self.work.left);
        let top = (anchor.y + self.px(GAP))
            .min(self.work.bottom - height)
            .max(self.work.top);
        self.main_rect = RECT {
            left,
            top,
            right: left + width,
            bottom: top + height,
        };
    }

    /// Opens group `index` beside the column: its places, and its spot left of the column (right where there is no
    /// room), level with the group's row.
    fn open_group(&mut self, index: usize) {
        self.open = Some(index);
        self.hover_sub = None;
        self.dragging = None;
        let width = self.px(SUB_WIDTH);
        let text_width = width - self.px(38) - self.px(14);
        let dc = unsafe { GetDC(None) };
        let mut top = self.px(PAD);
        let places: Vec<Place> = self
            .group_items()
            .iter()
            .map(|item| {
                let height = match item {
                    PanelItem::Separator => self.px(SEPARATOR),
                    PanelItem::Heading(_) => self.px(HEADING),
                    PanelItem::Slider { .. } => self.px(SLIDER),
                    PanelItem::Action { detail, .. } if !detail.is_empty() => {
                        self.px(SUB_ITEM)
                            + wrapped_height(dc, detail, text_width, self.fonts.detail)
                            + self.px(6)
                    }
                    _ => self.px(SUB_ITEM),
                };
                let place = Place { top, height };
                top += height;
                place
            })
            .collect();
        unsafe { ReleaseDC(None, dc) };
        self.sub_places = places;
        let height = top + self.px(PAD);
        let row = self.main_places.get(index).copied().unwrap_or_default();
        let left = if self.main_rect.left - width - self.px(GAP) >= self.work.left {
            self.main_rect.left - width - self.px(GAP)
        } else {
            self.main_rect.right + self.px(GAP)
        };
        let top = (self.main_rect.top + row.top - self.px(PAD))
            .min(self.work.bottom - height)
            .max(self.work.top);
        self.sub_rect = Some(RECT {
            left,
            top,
            right: left + width,
            bottom: top + height,
        });
    }

    fn close_group(&mut self) {
        self.open = None;
        self.sub_rect = None;
        self.hover_sub = None;
        self.in_sub = false;
        self.dragging = None;
    }

    fn paint_main(&self) {
        let area = self.main_rect;
        let s = self.scale;
        let highlight = [self.hover_main, self.open];
        let fonts = &self.fonts;
        paint_layered_at(
            self.main,
            area.left,
            area.top,
            area.right - area.left,
            area.bottom - area.top,
            px(12, s),
            OPAQUE,
            |dc, w, h| {
                fill(dc, 0, 0, w, h, INK);
                fill(dc, px(18, s), 0, w - px(36, s), 1, GOLD);
                for (index, (item, place)) in self.items.iter().zip(&self.main_places).enumerate() {
                    let (y, height) = (place.top, place.height);
                    match item {
                        PanelItem::Separator => {
                            fill(dc, px(14, s), y + height / 2, w - px(28, s), 1, DIVIDER);
                        }
                        PanelItem::Heading(label) => {
                            text(
                                dc,
                                label,
                                rect(px(16, s), y, w, height),
                                fonts.heading,
                                MUTED,
                                DT_LEFT.0,
                            );
                        }
                        PanelItem::Slider { .. } => {}
                        PanelItem::Group { icon, label, .. }
                        | PanelItem::Action { icon, label, .. } => {
                            let (danger, enabled, shortcut, mark, id) = match item {
                                PanelItem::Action {
                                    danger,
                                    enabled,
                                    shortcut,
                                    mark,
                                    id,
                                    ..
                                } => (*danger, *enabled, shortcut.as_str(), *mark, *id),
                                _ => (false, true, "", Mark::None, 0),
                            };
                            if enabled && highlight.contains(&Some(index)) {
                                fill_round(
                                    dc,
                                    &rect(px(6, s), y + px(2, s), w - px(12, s), height - px(4, s)),
                                    px(8, s),
                                    HOVER,
                                );
                            }
                            let color = if !enabled {
                                DIM
                            } else if danger {
                                DANGER
                            } else {
                                TEXT
                            };
                            let icon_color = if !enabled {
                                DIM
                            } else if danger {
                                DANGER
                            } else {
                                GOLD
                            };
                            text(
                                dc,
                                icon.glyph(),
                                rect(px(14, s), y, px(22, s), height),
                                fonts.glyph,
                                icon_color,
                                DT_CENTER.0,
                            );
                            text(
                                dc,
                                label,
                                rect(px(46, s), y, w - px(90, s), height),
                                fonts.label,
                                color,
                                DT_LEFT.0,
                            );
                            let right = rect(px(14, s), y, w - px(28, s), height);
                            if matches!(item, PanelItem::Group { .. }) {
                                text(dc, "\u{E76C}", right, fonts.chevron, MUTED, DT_RIGHT.0);
                            } else if mark != Mark::None && self.checked(id) {
                                text(dc, "\u{E73E}", right, fonts.glyph, GOLD, DT_RIGHT.0);
                            } else if !shortcut.is_empty() {
                                text(dc, shortcut, right, fonts.detail, MUTED, DT_RIGHT.0);
                            }
                        }
                    }
                }
            },
        );
    }

    fn paint_sub(&self) {
        let Some(area) = self.sub_rect else {
            let _ = unsafe { ShowWindow(self.sub, SW_HIDE) };
            return;
        };
        let s = self.scale;
        let fonts = &self.fonts;
        let items = self.group_items();
        paint_layered_at(
            self.sub,
            area.left,
            area.top,
            area.right - area.left,
            area.bottom - area.top,
            px(12, s),
            OPAQUE,
            |dc, w, h| {
                fill(dc, 0, 0, w, h, INK);
                for (index, (item, place)) in items.iter().zip(&self.sub_places).enumerate() {
                    let (y, height) = (place.top, place.height);
                    match item {
                        PanelItem::Separator => {
                            fill(dc, px(14, s), y + height / 2, w - px(28, s), 1, DIVIDER);
                        }
                        PanelItem::Heading(label) => {
                            text(
                                dc,
                                label,
                                rect(px(16, s), y + px(4, s), w, height - px(4, s)),
                                fonts.heading,
                                MUTED,
                                DT_LEFT.0,
                            );
                        }
                        PanelItem::Slider { label, value, .. } => {
                            let row = px(30, s);
                            text(
                                dc,
                                label,
                                rect(px(16, s), y, w, row),
                                fonts.label,
                                TEXT,
                                DT_LEFT.0,
                            );
                            text(
                                dc,
                                &format!("{value} %"),
                                rect(px(14, s), y, w - px(30, s), row),
                                fonts.detail,
                                MUTED,
                                DT_RIGHT.0,
                            );
                            let (left, right) = slider_track(w, s);
                            let track_y = y + px(36, s);
                            fill_round(
                                dc,
                                &rect(left, track_y, right - left, px(4, s)),
                                px(4, s),
                                TRACK,
                            );
                            let at = left + (right - left) * i32::from(*value) / 100;
                            fill_round(
                                dc,
                                &rect(left, track_y, at - left, px(4, s)),
                                px(4, s),
                                GOLD,
                            );
                            let knob = px(14, s);
                            let ring =
                                if self.hover_sub == Some(index) || self.dragging == Some(index) {
                                    TEXT
                                } else {
                                    GOLD
                                };
                            fill_round(
                                dc,
                                &rect(at - knob / 2, track_y + px(2, s) - knob / 2, knob, knob),
                                knob,
                                ring,
                            );
                        }
                        PanelItem::Group { label, .. } => {
                            text(
                                dc,
                                label,
                                rect(px(38, s), y, w, height),
                                fonts.label,
                                MUTED,
                                DT_LEFT.0,
                            );
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
                            if *enabled && self.hover_sub == Some(index) {
                                fill_round(
                                    dc,
                                    &rect(px(6, s), y + px(1, s), w - px(12, s), height - px(2, s)),
                                    px(8, s),
                                    HOVER,
                                );
                            }
                            let row = px(SUB_ITEM, s);
                            let on = self.checked(*id);
                            match mark {
                                Mark::Check if on => {
                                    text(
                                        dc,
                                        "\u{E73E}",
                                        rect(px(12, s), y, px(20, s), row),
                                        fonts.glyph,
                                        GOLD,
                                        DT_CENTER.0,
                                    );
                                }
                                Mark::Radio if on => {
                                    let dot = px(7, s);
                                    fill_round(
                                        dc,
                                        &rect(px(22, s) - dot / 2, y + row / 2 - dot / 2, dot, dot),
                                        dot,
                                        GOLD,
                                    );
                                }
                                _ => {}
                            }
                            let color = if !*enabled {
                                DIM
                            } else if *danger {
                                DANGER
                            } else {
                                TEXT
                            };
                            let shortcut_width = if shortcut.is_empty() {
                                0
                            } else {
                                text_width(dc, shortcut, fonts.detail) + px(12, s)
                            };
                            text(
                                dc,
                                label,
                                rect(px(38, s), y, w - px(52, s) - shortcut_width, row),
                                fonts.label,
                                color,
                                DT_LEFT.0,
                            );
                            if !shortcut.is_empty() {
                                text(
                                    dc,
                                    shortcut,
                                    rect(px(14, s), y, w - px(28, s), row),
                                    fonts.detail,
                                    MUTED,
                                    DT_RIGHT.0,
                                );
                            }
                            if !detail.is_empty() {
                                text_wrapped(
                                    dc,
                                    detail,
                                    rect(px(38, s), y + row - px(5, s), w - px(52, s), height),
                                    fonts.detail,
                                    if *enabled { MUTED } else { DIM },
                                );
                            }
                        }
                    }
                }
            },
        );
    }

    /// The column's item at `point` (screen), if it can be pointed at.
    fn main_at(&self, point: POINT) -> Option<usize> {
        if !contains(&self.main_rect, point) {
            return None;
        }
        let y = point.y - self.main_rect.top;
        self.main_places
            .iter()
            .position(|place| (place.top..place.top + place.height).contains(&y))
            .filter(|index| self.items[*index].selectable())
    }

    /// The open group's item at `point` (screen), if it can be pointed at.
    fn sub_at(&self, point: POINT) -> Option<usize> {
        let area = self.sub_rect?;
        if !contains(&area, point) {
            return None;
        }
        let y = point.y - area.top;
        self.sub_places
            .iter()
            .position(|place| (place.top..place.top + place.height).contains(&y))
            .filter(|index| self.group_items()[*index].selectable())
    }

    /// The slider at index `index` of the open group set from the pointer's `x` (screen); its new value if it changed.
    fn slide(&mut self, index: usize, x: i32) -> Option<(u16, u8)> {
        let area = self.sub_rect?;
        let (left, right) = slider_track(area.right - area.left, self.scale);
        let value = ((x - area.left - left) * 100 / (right - left).max(1)).clamp(0, 100) as u8;
        let group = self.open?;
        let PanelItem::Group { items, .. } = &mut self.items[group] else {
            return None;
        };
        match items.get_mut(index) {
            Some(PanelItem::Slider { id, value: old, .. }) if *old != value => {
                *old = value;
                Some((*id, value))
            }
            _ => None,
        }
    }

    /// The pointer moved to `point` (screen): highlights, the group under it opened, a slider dragged.
    fn moved(&mut self, point: POINT) -> Then {
        if let Some(index) = self.dragging {
            let changed = self.slide(index, point.x);
            self.paint_sub();
            return match changed {
                Some((id, value)) => Then::Value(self.owner, id, value),
                None => Then::Nothing,
            };
        }
        let (main, sub) = (self.main_at(point), self.sub_at(point));
        let (old_main, old_sub, old_open) = (self.hover_main, self.hover_sub, self.open);
        if let Some(index) = main {
            self.hover_main = Some(index);
            self.in_sub = false;
            match self.items[index] {
                PanelItem::Group { .. } if self.open != Some(index) => self.open_group(index),
                PanelItem::Group { .. } => {}
                _ => self.close_group(),
            }
        } else if sub.is_some() {
            self.hover_sub = sub;
            self.in_sub = true;
        } else {
            self.hover_sub = None;
            if self.open.is_none() {
                self.hover_main = None;
            }
        }
        if (old_main, old_sub, old_open) != (self.hover_main, self.hover_sub, self.open) {
            self.paint_main();
            self.paint_sub();
        }
        Then::Nothing
    }

    /// A press at `point`: a slider grabbed; outside both columns, the control center closes.
    fn pressed(&mut self, point: POINT) -> Then {
        if let Some(index) = self.sub_at(point)
            && matches!(self.group_items()[index], PanelItem::Slider { .. })
        {
            self.dragging = Some(index);
            let changed = self.slide(index, point.x);
            self.paint_sub();
            return match changed {
                Some((id, value)) => Then::Value(self.owner, id, value),
                None => Then::Nothing,
            };
        }
        let inside = contains(&self.main_rect, point)
            || self.sub_rect.is_some_and(|area| contains(&area, point));
        if inside {
            Then::Nothing
        } else {
            Then::Close { refocus: true }
        }
    }

    /// A release at `point`: the choice under it made.
    fn released(&mut self, point: POINT) -> Then {
        if self.dragging.take().is_some() {
            self.paint_sub();
            return Then::Nothing;
        }
        if let Some(index) = self.sub_at(point) {
            return self.choose(&self.group_items()[index].clone());
        }
        if let Some(index) = self.main_at(point) {
            return self.choose(&self.items[index].clone());
        }
        Then::Nothing
    }

    fn choose(&self, item: &PanelItem) -> Then {
        match item {
            PanelItem::Action {
                id, enabled: true, ..
            } => Then::Choose(self.owner, *id),
            _ => Then::Nothing,
        }
    }

    /// A key: Up and Down move, Right or Enter open a group, Left goes back, Enter or Space choose, Esc closes.
    fn key(&mut self, key: u16) -> Then {
        let step = |items: &[PanelItem], from: Option<usize>, down: bool| -> Option<usize> {
            let count = items.len();
            let mut at = from.unwrap_or(if down { count - 1 } else { 0 });
            for _ in 0..count {
                at = if down {
                    (at + 1) % count
                } else {
                    (at + count - 1) % count
                };
                if items[at].selectable() {
                    return Some(at);
                }
            }
            None
        };
        let then = match key {
            k if k == VK_ESCAPE.0 && self.in_sub => {
                self.in_sub = false;
                self.hover_sub = None;
                Then::Nothing
            }
            k if k == VK_ESCAPE.0 => Then::Close { refocus: true },
            k if k == VK_UP.0 || k == VK_DOWN.0 => {
                let down = k == VK_DOWN.0;
                if self.in_sub {
                    self.hover_sub = step(self.group_items(), self.hover_sub, down);
                } else if !self.items.is_empty() {
                    self.hover_main = step(&self.items, self.hover_main, down);
                    match self.hover_main {
                        Some(index) if matches!(self.items[index], PanelItem::Group { .. }) => {
                            self.open_group(index)
                        }
                        _ => self.close_group(),
                    }
                }
                Then::Nothing
            }
            k if k == VK_RIGHT.0 && !self.in_sub && self.open.is_some() => {
                self.in_sub = true;
                self.hover_sub = step(self.group_items(), None, true);
                Then::Nothing
            }
            k if k == VK_LEFT.0 && self.in_sub => {
                self.in_sub = false;
                self.hover_sub = None;
                Then::Nothing
            }
            k if k == VK_RETURN.0 || k == VK_SPACE.0 => {
                if self.in_sub {
                    match self
                        .hover_sub
                        .and_then(|index| self.group_items().get(index))
                    {
                        Some(item) => self.choose(&item.clone()),
                        None => Then::Nothing,
                    }
                } else if let Some(index) = self.hover_main {
                    if self.open == Some(index) {
                        self.in_sub = true;
                        self.hover_sub = step(self.group_items(), None, true);
                        Then::Nothing
                    } else {
                        self.choose(&self.items[index].clone())
                    }
                } else {
                    Then::Nothing
                }
            }
            _ => Then::Nothing,
        };
        self.paint_main();
        self.paint_sub();
        then
    }
}

/// A slider's track, left and right ends (pixels in a column `width` wide).
fn slider_track(width: i32, scale: f64) -> (i32, i32) {
    (px(16, scale), width - px(16, scale))
}

/// The point of a mouse message (`lparam`, in `hwnd`'s client area) on the screen.
fn screen_point(hwnd: HWND, lparam: LPARAM) -> POINT {
    let mut point = POINT {
        x: (lparam.0 & 0xFFFF) as i16 as i32,
        y: ((lparam.0 >> 16) & 0xFFFF) as i16 as i32,
    };
    let _ = unsafe { ClientToScreen(hwnd, &mut point) };
    point
}

extern "system" fn center_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // Only the column's own messages work the control center (the group beside it is drawn, never asked).
    let ours = |center: &Center| center.main == hwnd;
    let then = CENTER.with(|state| {
        let Ok(mut state) = state.try_borrow_mut() else {
            return None;
        };
        let center = state.as_mut().filter(|center| ours(center))?;
        Some(match message {
            WM_MOUSEMOVE => center.moved(screen_point(hwnd, lparam)),
            WM_LBUTTONDOWN => center.pressed(screen_point(hwnd, lparam)),
            WM_RBUTTONDOWN | WM_MBUTTONDOWN => {
                let point = screen_point(hwnd, lparam);
                match center.pressed(point) {
                    Then::Value(..) => Then::Nothing,
                    other => other,
                }
            }
            WM_LBUTTONUP => center.released(screen_point(hwnd, lparam)),
            WM_KEYDOWN | WM_SYSKEYDOWN => center.key(wparam.0 as u16),
            // Another window took the mouse or the keyboard: closed, without taking the keyboard back.
            WM_CAPTURECHANGED if lparam.0 as *mut std::ffi::c_void != hwnd.0 => {
                Then::Close { refocus: false }
            }
            WM_ACTIVATE if (wparam.0 & 0xFFFF) as u32 == WA_INACTIVE => {
                Then::Close { refocus: false }
            }
            _ => return None,
        })
    });
    match then {
        None => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
        Some(then) => {
            match then {
                Then::Nothing => {}
                Then::Close { refocus } => close(refocus),
                Then::Choose(owner, id) => {
                    close(true);
                    let _ = unsafe {
                        PostMessageW(
                            Some(owner),
                            WM_SYSCOMMAND,
                            WPARAM(usize::from(id)),
                            LPARAM(0),
                        )
                    };
                }
                Then::Value(owner, id, value) => {
                    let _ = unsafe {
                        PostMessageW(
                            Some(owner),
                            WM_PANEL_VALUE,
                            WPARAM(usize::from(id)),
                            LPARAM(isize::from(value)),
                        )
                    };
                }
            }
            LRESULT(0)
        }
    }
}
