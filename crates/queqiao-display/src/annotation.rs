//! Drawing on the controlled PC's screen (annotation, 2026-11-28, for help sessions): the viewer's pen strokes shown to
//! the person at that PC in a window over the shown screen, on top of everything, that never takes a click, a key or
//! the focus. Screen capture does take it in (no display affinity is set on it, unlike the privacy screen's), so the
//! viewer sees the strokes in the picture as the person there does.
//!
//! The strokes arrive as `protocol::Annotation`, positions 0..=65535 across the screen as the pointer's. What they are
//! (`Strokes`) and how a line is drawn into a picture with smooth edges (`Canvas`) are the same everywhere; the window
//! is each system's: Windows a layered window with per-pixel alpha (below), Linux an X11 window shaped to the strokes
//! (linux/annotation.rs). Each window lives on a thread of its own, which the session's loop never waits for.

use queqiao_core::protocol::{Annotation, MAX_STROKE_WIDTH};

#[cfg(target_os = "linux")]
use crate::linux::annotation as painter;
#[cfg(windows)]
use windows_painter as painter;

/// Where the drawing goes: the shown screen in desktop pixels (as the agent's injector has it).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DrawingArea {
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
}

/// The most points kept in all strokes together, and the most strokes: a viewer cannot make this PC hold more (about
/// 4 MB). What comes beyond is not drawn.
pub const MAX_POINTS: usize = 500_000;
pub const MAX_STROKES: usize = 5_000;

/// One stroke as it came: its colour (0xRRGGBB), its width (pixels of a 1080-line screen) and its points.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Stroke {
    pub(crate) colour: u32,
    pub(crate) width: u8,
    pub(crate) points: Vec<(u16, u16)>,
}

/// What a step of drawing changes on the screen, for the window to draw only that.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Op {
    /// A stroke's first point: a dot.
    Dot {
        at: (u16, u16),
        colour: u32,
        width: u8,
    },
    /// A stroke goes on from one point to the next.
    Line {
        from: (u16, u16),
        to: (u16, u16),
        colour: u32,
        width: u8,
    },
    Clear,
    /// Everything drawn again into `area` (the shown screen moved or changed its size).
    Redraw {
        area: DrawingArea,
        strokes: Vec<Stroke>,
    },
    /// The window goes and its thread ends.
    Quit,
}

/// The strokes drawn so far, and the one being drawn.
#[derive(Default)]
pub(crate) struct Strokes {
    pub(crate) list: Vec<Stroke>,
    /// The last stroke is still being drawn (it began and has not ended).
    open: bool,
    points: usize,
}

impl Strokes {
    /// Takes one step; what it changes on the screen (`On` and `End` change nothing; `Off` takes everything away).
    pub(crate) fn apply(&mut self, annotation: Annotation) -> Option<Op> {
        match annotation {
            Annotation::On | Annotation::End => {
                self.open = false;
                None
            }
            Annotation::Begin { colour, width } => {
                self.open = self.list.len() < MAX_STROKES;
                if self.open {
                    self.list.push(Stroke {
                        colour: colour & 0xFF_FFFF,
                        width: width.clamp(1, MAX_STROKE_WIDTH),
                        points: Vec::new(),
                    });
                }
                None
            }
            Annotation::Point { x, y } => {
                if !self.open || self.points >= MAX_POINTS {
                    return None;
                }
                let stroke = self.list.last_mut()?;
                let previous = stroke.points.last().copied();
                if previous == Some((x, y)) {
                    return None;
                }
                stroke.points.push((x, y));
                self.points += 1;
                let (colour, width) = (stroke.colour, stroke.width);
                Some(match previous {
                    None => Op::Dot {
                        at: (x, y),
                        colour,
                        width,
                    },
                    Some(from) => Op::Line {
                        from,
                        to: (x, y),
                        colour,
                        width,
                    },
                })
            }
            Annotation::Clear | Annotation::Off => {
                self.list.clear();
                self.open = false;
                self.points = 0;
                Some(Op::Clear)
            }
        }
    }

    /// The strokes with at least one point (what the person there sees).
    pub(crate) fn shown(&self) -> usize {
        self.list
            .iter()
            .filter(|stroke| !stroke.points.is_empty())
            .count()
    }
}

/// The pixel a protocol position stands for on a screen `size` pixels wide (or high), as the agent's injector maps
/// the pointer (so a stroke lies where the pointer would be).
pub(crate) fn pixel_index(value: u16, size: u32) -> i32 {
    (i64::from(size.saturating_sub(1)) * i64::from(value) / 65535) as i32
}

/// [`pixel_index`]'s pixel at its centre (Windows draws there; X11 draws on whole pixels).
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn pixel(value: u16, size: u32) -> f32 {
    pixel_index(value, size) as f32 + 0.5
}

/// A stroke's width in this screen's pixels: given for a 1080-line screen, scaled to the short side of this one.
pub(crate) fn screen_width(width: u8, area: DrawingArea) -> f32 {
    let short = area.width.min(area.height).max(1) as f32;
    (f32::from(width) * short / 1080.0).max(1.0)
}

/// The drawing over the shown screen: a window of this system's (see the module's notes), with the strokes so far.
/// Dropped, the window goes.
pub struct DrawingOverlay {
    strokes: Strokes,
    area: DrawingArea,
    painter: painter::Painter,
}

impl DrawingOverlay {
    /// The window over `area` (empty: nothing shows yet). Fails where the window cannot be made (no X server, no
    /// SHAPE extension, Windows refusing it): the reason, to tell the viewer.
    pub fn show(area: DrawingArea) -> Result<DrawingOverlay, String> {
        if area.width == 0 || area.height == 0 {
            return Err("the screen has no size".into());
        }
        Ok(DrawingOverlay {
            strokes: Strokes::default(),
            area,
            painter: painter::Painter::start(area)?,
        })
    }

    /// One step of the viewer's drawing.
    pub fn apply(&mut self, annotation: Annotation) {
        if let Some(op) = self.strokes.apply(annotation) {
            self.painter.send(op);
        }
    }

    /// The strokes shown now.
    pub fn strokes(&self) -> usize {
        self.strokes.shown()
    }

    /// The area it covers.
    pub fn area(&self) -> DrawingArea {
        self.area
    }

    /// Over `area` from now on (the shown screen changed, or its resolution did): the strokes drawn again there, at the
    /// same places relative to the screen.
    pub fn move_to(&mut self, area: DrawingArea) {
        if area == self.area || area.width == 0 || area.height == 0 {
            return;
        }
        self.area = area;
        self.painter.send(Op::Redraw {
            area,
            strokes: self.strokes.list.clone(),
        });
    }
}

/// A picture to draw strokes into: premultiplied BGRA pixels (0xAARRGGBB), rows top-down, fully transparent where
/// nothing is drawn (Windows' layered window shows it as it is; the X server draws Linux's strokes itself).
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) struct Canvas<'a> {
    pub(crate) pixels: &'a mut [u32],
    pub(crate) width: usize,
    pub(crate) height: usize,
}

/// A rectangle of pixels changed: left and top in it, right and bottom just outside.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Dirty {
    pub(crate) left: i32,
    pub(crate) top: i32,
    pub(crate) right: i32,
    pub(crate) bottom: i32,
}

impl Dirty {
    pub(crate) fn union(self, other: Option<Dirty>) -> Dirty {
        match other {
            None => self,
            Some(other) => Dirty {
                left: self.left.min(other.left),
                top: self.top.min(other.top),
                right: self.right.max(other.right),
                bottom: self.bottom.max(other.bottom),
            },
        }
    }
}

#[cfg_attr(not(windows), allow(dead_code))]
impl Canvas<'_> {
    /// Everything transparent again.
    pub(crate) fn clear(&mut self) {
        self.pixels.fill(0);
    }

    /// A line with round ends from `from` to `to` (pixel positions; the same twice: a dot), `width` pixels thick, in
    /// `colour` (0xRRGGBB, opaque), its edges smoothed (each pixel covered by how far its centre is inside). What it
    /// changed, if anything.
    pub(crate) fn line(
        &mut self,
        from: (f32, f32),
        to: (f32, f32),
        width: f32,
        colour: u32,
    ) -> Option<Dirty> {
        let radius = width / 2.0;
        let reach = radius + 1.0;
        let clip = |value: f32, size: usize| (value.max(0.0) as usize).min(size);
        let left = clip((from.0.min(to.0) - reach).floor(), self.width);
        let right = clip((from.0.max(to.0) + reach).ceil(), self.width);
        let top = clip((from.1.min(to.1) - reach).floor(), self.height);
        let bottom = clip((from.1.max(to.1) + reach).ceil(), self.height);
        if left >= right || top >= bottom {
            return None;
        }
        let (dx, dy) = (to.0 - from.0, to.1 - from.1);
        let length = dx * dx + dy * dy;
        let (red, green, blue) = ((colour >> 16) & 0xFF, (colour >> 8) & 0xFF, colour & 0xFF);
        for y in top..bottom {
            let row = y * self.width;
            let py = y as f32 + 0.5;
            for x in left..right {
                let px = x as f32 + 0.5;
                // The nearest point of the line, and how much of this pixel the pen covers.
                let along = if length > 0.0 {
                    (((px - from.0) * dx + (py - from.1) * dy) / length).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                let (nx, ny) = (from.0 + along * dx - px, from.1 + along * dy - py);
                let coverage = (radius + 0.5 - (nx * nx + ny * ny).sqrt()).clamp(0.0, 1.0);
                if coverage <= 0.0 {
                    continue;
                }
                // The pen over what is there (premultiplied: each channel times the alpha already).
                let alpha = (coverage * 255.0).round() as u32;
                let pixel = &mut self.pixels[row + x];
                let keep = 255 - alpha;
                let mix = |source: u32, shift: u32| {
                    let below = (*pixel >> shift) & 0xFF;
                    (source * alpha + below * keep + 127) / 255
                };
                *pixel = (mix(255, 24) << 24)
                    | (mix(red, 16) << 16)
                    | (mix(green, 8) << 8)
                    | mix(blue, 0);
            }
        }
        Some(Dirty {
            left: left as i32,
            top: top as i32,
            right: right as i32,
            bottom: bottom as i32,
        })
    }
}

/// Windows: a layered window (WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE)
/// over the shown screen, its picture a 32-bit bitmap with per-pixel alpha that `Canvas` draws into, put on the screen
/// with UpdateLayeredWindowIndirect (only the changed rectangle, at most 60 times a second). Transparent to input and
/// never activated, so the person's (and the viewer's) clicks and keys reach the programs underneath.
#[cfg(windows)]
mod windows_painter {
    use std::sync::Once;
    use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
    use std::time::{Duration, Instant};

    use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
    use windows::Win32::Graphics::Gdi::{
        AC_SRC_ALPHA, AC_SRC_OVER, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION,
        CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, GdiFlush,
        HBITMAP, HDC, HGDIOBJ, SelectObject,
    };
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::HiDpi::{
        DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetThreadDpiAwarenessContext,
    };
    use windows::Win32::UI::WindowsAndMessaging::*;
    use windows::core::{PCWSTR, w};

    use super::{Canvas, Dirty, DrawingArea, Op, pixel, screen_width};

    const CLASS: PCWSTR = w!("QueqiaoDrawing");
    static REGISTER: Once = Once::new();
    /// At most this often on the screen (strokes come up to 120 points a second).
    const FRAME: Duration = Duration::from_millis(16);
    /// Put back on top of other topmost windows this often (one shown later goes above it).
    const RAISE: Duration = Duration::from_secs(1);

    /// The window's thread: the steps go to it, it draws them.
    pub(crate) struct Painter {
        steps: Sender<Op>,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    impl Painter {
        pub(crate) fn start(area: DrawingArea) -> Result<Painter, String> {
            let (steps, inbox) = mpsc::channel();
            let (ready, made) = mpsc::sync_channel(1);
            let thread = std::thread::Builder::new()
                .name("queqiao-drawing".into())
                .spawn(move || match Surface::new(area) {
                    Ok(mut surface) => {
                        let _ = ready.send(Ok(()));
                        surface.run(&inbox);
                    }
                    Err(error) => {
                        let _ = ready.send(Err(error));
                    }
                })
                .map_err(|error| format!("the drawing's thread: {error}"))?;
            match made.recv() {
                Ok(Ok(())) => Ok(Painter {
                    steps,
                    thread: Some(thread),
                }),
                Ok(Err(error)) => {
                    let _ = thread.join();
                    Err(error)
                }
                Err(_) => Err("the drawing's thread ended".into()),
            }
        }

        pub(crate) fn send(&self, op: Op) {
            let _ = self.steps.send(op);
        }
    }

    impl Drop for Painter {
        fn drop(&mut self) {
            let _ = self.steps.send(Op::Quit);
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    /// The window and its picture (a DIB section selected into a memory DC).
    struct Surface {
        hwnd: HWND,
        memory: HDC,
        bitmap: HBITMAP,
        old: HGDIOBJ,
        bits: *mut u32,
        area: DrawingArea,
        /// What changed since the window last showed it; the whole of it after a move (None: nothing).
        dirty: Option<Dirty>,
        whole: bool,
    }

    impl Surface {
        fn new(area: DrawingArea) -> Result<Surface, String> {
            // Physical pixels on this thread, whatever the program's DPI setting: the window must cover the screen
            // exactly, and the positions are the screen's pixels.
            let _ =
                unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
            let instance = unsafe { GetModuleHandleW(None) }.map_err(|error| error.message())?;
            REGISTER.call_once(|| unsafe {
                let class = WNDCLASSEXW {
                    cbSize: size_of::<WNDCLASSEXW>() as u32,
                    lpfnWndProc: Some(drawing_proc),
                    hInstance: instance.into(),
                    lpszClassName: CLASS,
                    ..Default::default()
                };
                RegisterClassExW(&class);
            });
            let hwnd = unsafe {
                CreateWindowExW(
                    WS_EX_LAYERED
                        | WS_EX_TRANSPARENT
                        | WS_EX_TOPMOST
                        | WS_EX_TOOLWINDOW
                        | WS_EX_NOACTIVATE,
                    CLASS,
                    w!("Queqiao drawing"),
                    WS_POPUP,
                    area.left,
                    area.top,
                    area.width as i32,
                    area.height as i32,
                    None,
                    None,
                    Some(instance.into()),
                    None,
                )
            }
            .map_err(|error| error.message())?;
            let mut surface = Surface {
                hwnd,
                memory: HDC::default(),
                bitmap: HBITMAP::default(),
                old: HGDIOBJ::default(),
                bits: std::ptr::null_mut(),
                area,
                dirty: None,
                whole: true,
            };
            surface.allocate(area)?;
            // Shown (all transparent) without being activated; never hidden from capture.
            surface.present();
            let _ = unsafe { ShowWindow(hwnd, SW_SHOWNOACTIVATE) };
            Ok(surface)
        }

        /// A transparent picture of `area`'s size (the old one freed).
        fn allocate(&mut self, area: DrawingArea) -> Result<(), String> {
            self.free_picture();
            let memory = unsafe { CreateCompatibleDC(None) };
            if memory.is_invalid() {
                return Err("no memory DC for the drawing".into());
            }
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: area.width as i32,
                    biHeight: -(area.height as i32),
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
            let bitmap = match unsafe {
                CreateDIBSection(Some(memory), &info, DIB_RGB_COLORS, &mut bits, None, 0)
            } {
                Ok(bitmap) if !bits.is_null() => bitmap,
                Ok(bitmap) => {
                    let _ = unsafe { DeleteObject(bitmap.into()) };
                    let _ = unsafe { DeleteDC(memory) };
                    return Err("the drawing's picture has no pixels".into());
                }
                Err(error) => {
                    let _ = unsafe { DeleteDC(memory) };
                    return Err(error.message());
                }
            };
            self.old = unsafe { SelectObject(memory, bitmap.into()) };
            (self.memory, self.bitmap, self.bits) = (memory, bitmap, bits.cast());
            self.area = area;
            // A new DIB section is all zero: transparent.
            self.whole = true;
            Ok(())
        }

        fn free_picture(&mut self) {
            if !self.memory.is_invalid() {
                unsafe {
                    SelectObject(self.memory, self.old);
                    let _ = DeleteObject(self.bitmap.into());
                    let _ = DeleteDC(self.memory);
                }
            }
            (self.memory, self.bits) = (HDC::default(), std::ptr::null_mut());
        }

        fn canvas(&mut self) -> Canvas<'_> {
            let (width, height) = (self.area.width as usize, self.area.height as usize);
            Canvas {
                // The DIB section's pixels: this thread alone writes them, GDI does not draw into it.
                pixels: unsafe { std::slice::from_raw_parts_mut(self.bits, width * height) },
                width,
                height,
            }
        }

        fn apply(&mut self, op: Op) {
            let area = self.area;
            let at = |(x, y): (u16, u16)| (pixel(x, area.width), pixel(y, area.height));
            let changed = match op {
                Op::Dot {
                    at: p,
                    colour,
                    width,
                } => self
                    .canvas()
                    .line(at(p), at(p), screen_width(width, area), colour),
                Op::Line {
                    from,
                    to,
                    colour,
                    width,
                } => self
                    .canvas()
                    .line(at(from), at(to), screen_width(width, area), colour),
                Op::Clear => {
                    self.canvas().clear();
                    self.whole = true;
                    None
                }
                Op::Redraw { area, strokes } => {
                    if area != self.area {
                        if self.allocate(area).is_err() {
                            return;
                        }
                        let _ = unsafe {
                            SetWindowPos(
                                self.hwnd,
                                Some(HWND_TOPMOST),
                                area.left,
                                area.top,
                                area.width as i32,
                                area.height as i32,
                                SWP_NOACTIVATE,
                            )
                        };
                    }
                    self.canvas().clear();
                    let at = |(x, y): (u16, u16)| (pixel(x, area.width), pixel(y, area.height));
                    for stroke in strokes {
                        let width = screen_width(stroke.width, area);
                        let mut previous = None;
                        for point in stroke.points {
                            let from = previous.unwrap_or(point);
                            self.canvas()
                                .line(at(from), at(point), width, stroke.colour);
                            previous = Some(point);
                        }
                    }
                    self.whole = true;
                    None
                }
                Op::Quit => None,
            };
            if let Some(changed) = changed {
                self.dirty = Some(changed.union(self.dirty));
            }
        }

        /// The picture on the screen: the changed rectangle (all of it after a clear or a move).
        fn present(&mut self) {
            let dirty = self.dirty.take();
            let whole = std::mem::take(&mut self.whole);
            if !whole && dirty.is_none() {
                return;
            }
            let blend = BLENDFUNCTION {
                BlendOp: AC_SRC_OVER as u8,
                BlendFlags: 0,
                SourceConstantAlpha: 255,
                AlphaFormat: AC_SRC_ALPHA as u8,
            };
            let position = POINT {
                x: self.area.left,
                y: self.area.top,
            };
            let size = SIZE {
                cx: self.area.width as i32,
                cy: self.area.height as i32,
            };
            let source = POINT::default();
            let rect = dirty.map(|dirty| RECT {
                left: dirty.left,
                top: dirty.top,
                right: dirty.right,
                bottom: dirty.bottom,
            });
            let info = UPDATELAYEREDWINDOWINFO {
                cbSize: size_of::<UPDATELAYEREDWINDOWINFO>() as u32,
                hdcDst: HDC::default(),
                pptDst: &position,
                psize: &size,
                hdcSrc: self.memory,
                pptSrc: &source,
                crKey: COLORREF(0),
                pblend: &blend,
                dwFlags: ULW_ALPHA,
                prcDirty: match (&rect, whole) {
                    (Some(rect), false) => rect as *const RECT,
                    _ => std::ptr::null(),
                },
            };
            unsafe {
                // The pixels written by hand reach GDI first.
                let _ = GdiFlush();
                let _ = UpdateLayeredWindowIndirect(self.hwnd, &info);
            }
        }

        fn run(&mut self, inbox: &Receiver<Op>) {
            let (mut shown_at, mut raised_at) = (Instant::now(), Instant::now());
            'thread: loop {
                // The window's own messages (it has few: it takes no input).
                let mut message = MSG::default();
                while unsafe { PeekMessageW(&mut message, None, 0, 0, PM_REMOVE) }.as_bool() {
                    unsafe {
                        let _ = TranslateMessage(&message);
                        DispatchMessageW(&message);
                    }
                }
                let pending = self.dirty.is_some() || self.whole;
                let wait = if pending {
                    FRAME.saturating_sub(shown_at.elapsed())
                } else {
                    Duration::from_millis(50)
                };
                match inbox.recv_timeout(wait) {
                    Ok(Op::Quit) | Err(RecvTimeoutError::Disconnected) => break 'thread,
                    Ok(op) => {
                        self.apply(op);
                        // Everything that came meanwhile, then one picture for it all.
                        while let Ok(op) = inbox.try_recv() {
                            if op == Op::Quit {
                                break 'thread;
                            }
                            self.apply(op);
                        }
                    }
                    Err(RecvTimeoutError::Timeout) => {}
                }
                if (self.dirty.is_some() || self.whole) && shown_at.elapsed() >= FRAME {
                    self.present();
                    shown_at = Instant::now();
                }
                if raised_at.elapsed() >= RAISE {
                    let _ = unsafe {
                        SetWindowPos(
                            self.hwnd,
                            Some(HWND_TOPMOST),
                            0,
                            0,
                            0,
                            0,
                            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOOWNERZORDER,
                        )
                    };
                    raised_at = Instant::now();
                }
            }
        }
    }

    impl Drop for Surface {
        fn drop(&mut self) {
            let _ = unsafe { DestroyWindow(self.hwnd) };
            self.free_picture();
        }
    }

    extern "system" fn drawing_proc(
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match message {
            // Never takes the focus or a click.
            WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
            WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize),
            _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strokes_draw_dots_then_lines_and_clear() {
        let mut strokes = Strokes::default();
        assert_eq!(strokes.apply(Annotation::On), None);
        // A point before any stroke began draws nothing.
        assert_eq!(strokes.apply(Annotation::Point { x: 1, y: 1 }), None);
        assert_eq!(
            strokes.apply(Annotation::Begin {
                colour: 0xFF_00FF,
                width: 6
            }),
            None
        );
        assert_eq!(strokes.shown(), 0);
        assert_eq!(
            strokes.apply(Annotation::Point { x: 10, y: 20 }),
            Some(Op::Dot {
                at: (10, 20),
                colour: 0xFF_00FF,
                width: 6
            })
        );
        // The same point again changes nothing.
        assert_eq!(strokes.apply(Annotation::Point { x: 10, y: 20 }), None);
        assert_eq!(
            strokes.apply(Annotation::Point { x: 30, y: 40 }),
            Some(Op::Line {
                from: (10, 20),
                to: (30, 40),
                colour: 0xFF_00FF,
                width: 6
            })
        );
        assert_eq!(strokes.apply(Annotation::End), None);
        // After its end a stroke takes no more points.
        assert_eq!(strokes.apply(Annotation::Point { x: 50, y: 60 }), None);
        assert_eq!(strokes.shown(), 1);
        // A stroke that never got a point is not counted.
        strokes.apply(Annotation::Begin {
            colour: 0,
            width: 3,
        });
        strokes.apply(Annotation::End);
        assert_eq!(strokes.shown(), 1);
        assert_eq!(strokes.apply(Annotation::Clear), Some(Op::Clear));
        assert_eq!(strokes.shown(), 0);
    }

    #[test]
    fn a_viewer_cannot_make_this_pc_keep_more_than_the_most() {
        let mut strokes = Strokes::default();
        strokes.apply(Annotation::Begin {
            colour: 0,
            width: 1,
        });
        let mut drawn = 0;
        for i in 0..(MAX_POINTS + 10) {
            let x = (i % 60_000) as u16;
            let y = (i / 60_000) as u16;
            drawn += usize::from(strokes.apply(Annotation::Point { x, y }).is_some());
        }
        assert_eq!(drawn, MAX_POINTS);
        let mut strokes = Strokes::default();
        for _ in 0..(MAX_STROKES + 5) {
            strokes.apply(Annotation::Begin {
                colour: 0,
                width: 1,
            });
            strokes.apply(Annotation::Point { x: 1, y: 1 });
        }
        assert_eq!(strokes.shown(), MAX_STROKES);
    }

    #[test]
    fn positions_map_as_the_pointers_do() {
        assert_eq!(pixel(0, 1920), 0.5);
        assert_eq!(pixel(65535, 1920), 1919.5);
        // 1919 * 32768 / 65535 = 959.5, rounded down as the injector does.
        assert_eq!(pixel(32768, 1920), 959.5);
        let screen = |width, height| DrawingArea {
            left: 0,
            top: 0,
            width,
            height,
        };
        // A stroke looks as thick on any screen: twice the pixels on a 2160-line one.
        assert_eq!(screen_width(5, screen(1920, 1080)), 5.0);
        assert_eq!(screen_width(5, screen(3840, 2160)), 10.0);
        assert_eq!(screen_width(1, screen(640, 360)), 1.0);
    }

    #[test]
    fn a_line_is_opaque_inside_smooth_at_its_edges_and_transparent_elsewhere() {
        let (width, height) = (40, 20);
        let mut pixels = vec![0u32; width * height];
        let mut canvas = Canvas {
            pixels: &mut pixels,
            width,
            height,
        };
        let dirty = canvas
            .line((5.5, 10.5), (30.5, 10.5), 6.0, 0xFF_0000)
            .unwrap();
        // Its rectangle holds it, within the picture.
        assert!(dirty.left <= 2 && dirty.right >= 34 && dirty.top <= 7 && dirty.bottom >= 14);
        assert!(dirty.left >= 0 && dirty.right <= 40 && dirty.top >= 0 && dirty.bottom <= 20);
        let at = |x: usize, y: usize| pixels[y * width + x];
        // On the line: opaque red (premultiplied BGRA).
        assert_eq!(at(15, 10), 0xFFFF_0000);
        assert_eq!(at(15, 8), 0xFFFF_0000);
        // Its round end reaches past the end point, not much further.
        assert_eq!(at(32, 10), 0xFFFF_0000);
        assert_eq!(at(36, 10), 0);
        // Far off: untouched.
        assert_eq!(at(15, 1), 0);
        assert_eq!(at(0, 0), 0);
        // At the edge: partly covered, premultiplied (no channel above the alpha).
        let edge = at(15, 13);
        let alpha = edge >> 24;
        assert!(alpha > 0 && alpha < 255, "{edge:#x}");
        assert!(
            (edge >> 16) & 0xFF <= alpha && edge & 0xFFFF == 0,
            "{edge:#x}"
        );
        // A dot is a round spot; a line off the picture changes nothing; clearing empties it.
        let mut pixels = vec![0u32; width * height];
        let mut canvas = Canvas {
            pixels: &mut pixels,
            width,
            height,
        };
        assert!(
            canvas
                .line((10.5, 10.5), (10.5, 10.5), 4.0, 0x00_FF00)
                .is_some()
        );
        assert_eq!(
            canvas.line((-50.0, -50.0), (-40.0, -45.0), 4.0, 0x00_FF00),
            None
        );
        canvas.clear();
        assert!(pixels.iter().all(|&pixel| pixel == 0));
    }

    #[test]
    fn a_line_over_another_colour_covers_it() {
        let (width, height) = (20, 20);
        let mut pixels = vec![0u32; width * height];
        let mut canvas = Canvas {
            pixels: &mut pixels,
            width,
            height,
        };
        canvas.line((2.5, 10.5), (17.5, 10.5), 8.0, 0x00_00FF);
        canvas.line((10.5, 2.5), (10.5, 17.5), 8.0, 0xFF_FF00);
        assert_eq!(pixels[10 * width + 10], 0xFFFF_FF00);
        assert_eq!(pixels[10 * width + 3], 0xFF00_00FF);
    }
}
