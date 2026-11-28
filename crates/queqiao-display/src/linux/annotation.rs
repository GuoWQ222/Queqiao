//! Drawing on the controlled PC's screen on X11 (annotation.rs has what a drawing is; Windows' window is there too):
//! an override-redirect window over the shown screen, shaped to the strokes (SHAPE's bounding shape, from a 1-bit mask
//! of them) with an empty input shape, so every click goes to what is underneath; never focused (it takes no keys),
//! raised again each second. Shaped rather than a 32-bit window with alpha: alpha needs a compositing manager, and
//! without one (Xvfb, window managers that do not composite) the transparent pixels of such a window show black over
//! the whole screen. The X server draws the strokes itself (wide lines with round ends) into a pixmap kept for
//! exposures and into the mask; screen capture (the root window) takes the window in as any other.

use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use x11rb::connection::{Connection, RequestConnection};
use x11rb::protocol::Event;
use x11rb::protocol::shape::{self, ConnectionExt as _, SK, SO};
use x11rb::protocol::xproto::{
    Arc, AtomEnum, CapStyle, ChangeGCAux, ClipOrdering, ConfigureWindowAux, ConnectionExt as _,
    CreateGCAux, CreateWindowAux, EventMask, JoinStyle, PropMode, Rectangle, Segment, StackMode,
    WindowClass,
};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

use crate::annotation::{Dirty, DrawingArea, Op, pixel_index, screen_width};

/// The window's shape follows the strokes at most this often (the server makes a region of the whole mask each time).
const SHAPE_GAP: Duration = Duration::from_millis(30);
/// Raised above other windows this often (one mapped later goes above it).
const RAISE: Duration = Duration::from_secs(1);

fn x11(error: impl std::fmt::Display) -> String {
    format!("X11: {error}")
}

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

/// The window, its picture and mask, and the pens drawing them.
struct Surface {
    connection: RustConnection,
    root: u32,
    depth: u8,
    window: u32,
    /// The strokes in colour (the window's depth), and where they are (depth 1: the window's shape).
    picture: u32,
    mask: u32,
    pen: u32,
    mask_pen: u32,
    eraser: u32,
    copier: u32,
    /// The root visual's channel masks (red, green, blue), for a colour's pixel value.
    channels: (u32, u32, u32),
    area: DrawingArea,
    /// The mask changed since the shape last followed it; the picture's part to show then.
    shape_due: bool,
    shown: Option<Dirty>,
}

impl Surface {
    fn new(area: DrawingArea) -> Result<Surface, String> {
        let (connection, screen_number) = x11rb::connect(None).map_err(x11)?;
        if connection
            .extension_information(shape::X11_EXTENSION_NAME)
            .map_err(x11)?
            .is_none()
        {
            return Err("the X server has no SHAPE extension".into());
        }
        // The input shape (clicks passing through) came with SHAPE 1.1.
        let version = connection
            .shape_query_version()
            .map_err(x11)?
            .reply()
            .map_err(x11)?;
        if (version.major_version, version.minor_version) < (1, 1) {
            return Err(format!(
                "the X server's SHAPE is {}.{}: 1.1 is needed for clicks to pass through",
                version.major_version, version.minor_version
            ));
        }
        let screen = &connection.setup().roots[screen_number];
        let (root, depth, visual) = (screen.root, screen.root_depth, screen.root_visual);
        let channels = screen
            .allowed_depths
            .iter()
            .flat_map(|depth| depth.visuals.iter())
            .find(|candidate| candidate.visual_id == visual)
            .map_or((0xFF_0000, 0xFF00, 0xFF), |found| {
                (found.red_mask, found.green_mask, found.blue_mask)
            });
        let window = connection.generate_id().map_err(x11)?;
        connection
            .create_window(
                depth,
                window,
                root,
                area.left as i16,
                area.top as i16,
                area.width as u16,
                area.height as u16,
                0,
                WindowClass::INPUT_OUTPUT,
                visual,
                // Not managed by the window manager (no frame, no focus, not in a task list); no background, so
                // nothing is painted where no stroke is.
                &CreateWindowAux::new()
                    .override_redirect(1)
                    .border_pixel(0)
                    .event_mask(EventMask::EXPOSURE),
            )
            .map_err(x11)?
            .check()
            .map_err(x11)?;
        let _ = connection.change_property8(
            PropMode::REPLACE,
            window,
            AtomEnum::WM_NAME,
            AtomEnum::STRING,
            b"Queqiao drawing",
        );
        let _ = connection.change_property8(
            PropMode::REPLACE,
            window,
            AtomEnum::WM_CLASS,
            AtomEnum::STRING,
            b"queqiao-drawing\0Queqiao\0",
        );
        let mut surface = Surface {
            connection,
            root,
            depth,
            window,
            picture: 0,
            mask: 0,
            pen: 0,
            mask_pen: 0,
            eraser: 0,
            copier: 0,
            channels,
            area,
            shape_due: false,
            shown: None,
        };
        surface.allocate(area)?;
        // No clicks taken anywhere (an empty input shape), nothing shown yet (an empty mask), then mapped on top.
        let connection = &surface.connection;
        connection
            .shape_rectangles(
                SO::SET,
                SK::INPUT,
                ClipOrdering::UNSORTED,
                window,
                0,
                0,
                &[],
            )
            .map_err(x11)?;
        // Checked (each a round trip, once): an error here comes back now, not later as an event nobody reads.
        connection
            .shape_mask(SO::SET, SK::BOUNDING, window, 0, 0, surface.mask)
            .map_err(x11)?
            .check()
            .map_err(x11)?;
        connection
            .map_window(window)
            .map_err(x11)?
            .check()
            .map_err(x11)?;
        let _ = connection.configure_window(
            window,
            &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE),
        );
        connection.flush().map_err(x11)?;
        Ok(surface)
    }

    /// A picture and a mask of `area`'s size (the old ones freed), the mask empty, and the pens to draw them.
    fn allocate(&mut self, area: DrawingArea) -> Result<(), String> {
        self.free();
        let connection = &self.connection;
        let (width, height) = (area.width as u16, area.height as u16);
        let id = || connection.generate_id().map_err(x11);
        let (picture, mask) = (id()?, id()?);
        let (pen, mask_pen, eraser, copier) = (id()?, id()?, id()?, id()?);
        connection
            .create_pixmap(self.depth, picture, self.root, width, height)
            .map_err(x11)?;
        connection
            .create_pixmap(1, mask, self.root, width, height)
            .map_err(x11)?
            .check()
            .map_err(x11)?;
        let round = CreateGCAux::new()
            .cap_style(CapStyle::ROUND)
            .join_style(JoinStyle::ROUND)
            .graphics_exposures(0);
        connection
            .create_gc(pen, picture, &round.foreground(0))
            .map_err(x11)?;
        connection
            .create_gc(mask_pen, mask, &round.foreground(1))
            .map_err(x11)?;
        connection
            .create_gc(eraser, mask, &CreateGCAux::new().foreground(0))
            .map_err(x11)?;
        connection
            .create_gc(copier, picture, &CreateGCAux::new().graphics_exposures(0))
            .map_err(x11)?;
        (self.picture, self.mask) = (picture, mask);
        (self.pen, self.mask_pen, self.eraser, self.copier) = (pen, mask_pen, eraser, copier);
        self.area = area;
        self.erase();
        Ok(())
    }

    fn free(&mut self) {
        let connection = &self.connection;
        for gc in [self.pen, self.mask_pen, self.eraser, self.copier] {
            if gc != 0 {
                let _ = connection.free_gc(gc);
            }
        }
        for pixmap in [self.picture, self.mask] {
            if pixmap != 0 {
                let _ = connection.free_pixmap(pixmap);
            }
        }
        (self.picture, self.mask) = (0, 0);
        (self.pen, self.mask_pen, self.eraser, self.copier) = (0, 0, 0, 0);
    }

    /// The mask empty: nothing shows (once the shape follows it).
    fn erase(&mut self) {
        let _ = self.connection.poly_fill_rectangle(
            self.mask,
            self.eraser,
            &[Rectangle {
                x: 0,
                y: 0,
                width: self.area.width as u16,
                height: self.area.height as u16,
            }],
        );
        self.shape_due = true;
    }

    /// A colour (0xRRGGBB) as the root visual's pixel value.
    fn pixel_of(&self, colour: u32) -> u32 {
        let channel = |value: u32, mask: u32| {
            if mask == 0 {
                return 0;
            }
            let shift = mask.trailing_zeros();
            let bits = (mask >> shift).count_ones().min(8);
            ((value >> (8 - bits)) << shift) & mask
        };
        let (red, green, blue) = self.channels;
        channel((colour >> 16) & 0xFF, red)
            | channel((colour >> 8) & 0xFF, green)
            | channel(colour & 0xFF, blue)
    }

    /// A line from `from` to `to` (protocol positions; the same twice: a dot), into the picture and the mask.
    fn stroke(&mut self, from: (u16, u16), to: (u16, u16), colour: u32, width: u8) {
        let area = self.area;
        let at = |(x, y): (u16, u16)| {
            (
                pixel_index(x, area.width) as i16,
                pixel_index(y, area.height) as i16,
            )
        };
        let ((x1, y1), (x2, y2)) = (at(from), at(to));
        let thick = (screen_width(width, area).round() as u16).max(1);
        let connection = &self.connection;
        let _ = connection.change_gc(
            self.pen,
            &ChangeGCAux::new()
                .foreground(self.pixel_of(colour))
                .line_width(u32::from(thick)),
        );
        let _ = connection.change_gc(
            self.mask_pen,
            &ChangeGCAux::new().line_width(u32::from(thick)),
        );
        for (drawable, gc) in [(self.picture, self.pen), (self.mask, self.mask_pen)] {
            if from == to {
                // A round spot (a line of no length draws nothing for sure).
                let half = (thick / 2) as i16;
                let _ = connection.poly_fill_arc(
                    drawable,
                    gc,
                    &[Arc {
                        x: x1.saturating_sub(half),
                        y: y1.saturating_sub(half),
                        width: thick,
                        height: thick,
                        angle1: 0,
                        angle2: 360 * 64,
                    }],
                );
            } else {
                let _ = connection.poly_segment(drawable, gc, &[Segment { x1, y1, x2, y2 }]);
            }
        }
        let reach = i32::from(thick) / 2 + 2;
        let changed = Dirty {
            left: i32::from(x1.min(x2)) - reach,
            top: i32::from(y1.min(y2)) - reach,
            right: i32::from(x1.max(x2)) + reach + 1,
            bottom: i32::from(y1.max(y2)) + reach + 1,
        };
        self.shown = Some(changed.union(self.shown));
        self.shape_due = true;
    }

    fn apply(&mut self, op: Op) {
        match op {
            Op::Dot { at, colour, width } => self.stroke(at, at, colour, width),
            Op::Line {
                from,
                to,
                colour,
                width,
            } => self.stroke(from, to, colour, width),
            Op::Clear => self.erase(),
            Op::Redraw { area, strokes } => {
                if area != self.area {
                    if self.allocate(area).is_err() {
                        return;
                    }
                    let _ = self.connection.configure_window(
                        self.window,
                        &ConfigureWindowAux::new()
                            .x(area.left)
                            .y(area.top)
                            .width(area.width)
                            .height(area.height)
                            .stack_mode(StackMode::ABOVE),
                    );
                }
                self.erase();
                for stroke in strokes {
                    let mut previous = None;
                    for point in stroke.points {
                        self.stroke(
                            previous.unwrap_or(point),
                            point,
                            stroke.colour,
                            stroke.width,
                        );
                        previous = Some(point);
                    }
                }
            }
            Op::Quit => {}
        }
    }

    /// The window's shape as the mask is now, then the strokes' new part painted (drawing into the window outside
    /// its old shape would have been cut away).
    fn reshape(&mut self) {
        let connection = &self.connection;
        let _ = connection.shape_mask(SO::SET, SK::BOUNDING, self.window, 0, 0, self.mask);
        if let Some(part) = self.shown.take() {
            self.copy(part);
        }
        self.shape_due = false;
    }

    /// The picture's `part` into the window (also when part of it was uncovered: an exposure).
    fn copy(&self, part: Dirty) {
        let (width, height) = (self.area.width as i32, self.area.height as i32);
        let (left, top) = (part.left.clamp(0, width), part.top.clamp(0, height));
        let (right, bottom) = (part.right.clamp(0, width), part.bottom.clamp(0, height));
        if left >= right || top >= bottom {
            return;
        }
        let _ = self.connection.copy_area(
            self.picture,
            self.window,
            self.copier,
            left as i16,
            top as i16,
            left as i16,
            top as i16,
            (right - left) as u16,
            (bottom - top) as u16,
        );
    }

    fn run(&mut self, inbox: &Receiver<Op>) {
        let mut shaped_at = Instant::now()
            .checked_sub(SHAPE_GAP)
            .unwrap_or_else(Instant::now);
        let mut raised_at = Instant::now();
        'thread: loop {
            loop {
                match self.connection.poll_for_event() {
                    Ok(Some(Event::Expose(exposed))) => self.copy(Dirty {
                        left: i32::from(exposed.x),
                        top: i32::from(exposed.y),
                        right: i32::from(exposed.x) + i32::from(exposed.width),
                        bottom: i32::from(exposed.y) + i32::from(exposed.height),
                    }),
                    Ok(Some(_)) => {}
                    Ok(None) => break,
                    // The X server is gone: so is the window.
                    Err(_) => break 'thread,
                }
            }
            let wait = if self.shape_due {
                SHAPE_GAP.saturating_sub(shaped_at.elapsed())
            } else {
                Duration::from_millis(50)
            };
            match inbox.recv_timeout(wait) {
                Ok(Op::Quit) | Err(RecvTimeoutError::Disconnected) => break 'thread,
                Ok(op) => {
                    self.apply(op);
                    // Everything that came meanwhile, then one shape for it all.
                    while let Ok(op) = inbox.try_recv() {
                        if op == Op::Quit {
                            break 'thread;
                        }
                        self.apply(op);
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
            }
            if self.shape_due && shaped_at.elapsed() >= SHAPE_GAP {
                self.reshape();
                shaped_at = Instant::now();
            }
            if raised_at.elapsed() >= RAISE {
                let _ = self.connection.configure_window(
                    self.window,
                    &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE),
                );
                raised_at = Instant::now();
            }
            if self.connection.flush().is_err() {
                break;
            }
        }
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        self.free();
        let _ = self.connection.destroy_window(self.window);
        let _ = self.connection.flush();
    }
}
