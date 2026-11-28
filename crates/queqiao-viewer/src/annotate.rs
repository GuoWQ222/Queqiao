//! Drawing on that PC's screen (annotation, 2026-11-28, for help sessions): the session window's drawing mode, turned
//! on in the control center's Tools or the window menu (or by choosing a pen). While it is on, the mouse in the
//! session window draws pen strokes that the person at that PC sees on top of everything (the agent's window,
//! queqiao_display::DrawingOverlay) instead of controlling that PC; keys still go there. Esc or the toggle ends it,
//! and the strokes go with it. The pointer here is a spot in the pen's colour meanwhile.
//!
//! That PC answers each step (`Message::AnnotateState`): one that never answers drawing mode (a Queqiao before it)
//! or cannot show it (Wayland, its lock screen) ends it here, with a word to the person.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use queqiao_core::protocol::{Annotation, InputEvent, Message, PointerKind, PointerShape};
use queqiao_core::text::tr;
use queqiao_display::{PanelItem, RemoteCursor, Window, WindowEvent};

use crate::session::MENU_GAME_MOUSE;

/// The window menu's and control center's items (system-menu ids: multiples of 16, as session.rs's).
pub(crate) const MENU_DRAW: u16 = 0x1500;
pub(crate) const MENU_DRAW_CLEAR: u16 = 0x1510;
/// The pens: (menu id, colour 0xRRGGBB, label).
pub(crate) const MENU_PENS: [(u16, u32, &str); 4] = [
    (0x1520, 0xE5_3935, "Red pen"),
    (0x1530, 0xFD_D835, "Yellow pen"),
    (0x1540, 0x43_A047, "Green pen"),
    (0x1550, 0x1E_88E5, "Blue pen"),
];

/// Every menu id of drawing's.
pub(crate) fn menu_ids() -> Vec<u16> {
    let mut ids = vec![MENU_DRAW, MENU_DRAW_CLEAR];
    ids.extend(MENU_PENS.iter().map(|(id, _, _)| *id));
    ids
}

/// The pen's thickness in pixels of a 1080-line screen (that PC scales it to its own).
pub(crate) const PEN_WIDTH: u8 = 5;
/// A point waits while it is this close (window pixels) to the last one sent, and until this long after it: at most
/// 120 points a second, none for a hand's tremble.
const MIN_STEP: i32 = 2;
const MIN_GAP: Duration = Duration::from_micros(8_334);
/// That PC answers drawing mode within this, or the person is told it cannot draw there.
const ANSWER_WAIT: Duration = Duration::from_secs(4);
/// The Esc key (scancode set 1).
const ESCAPE: u16 = 0x01;

/// What became of a window event while drawing.
pub(crate) struct Outcome {
    /// To send to that PC.
    pub(crate) messages: Vec<Message>,
    /// The event was drawing's (the mouse): it does not go to that PC as input.
    pub(crate) taken: bool,
    /// Drawing mode ended (Esc): the window shows it.
    pub(crate) ended: bool,
}

pub(crate) struct Drawing {
    on: bool,
    pen: u32,
    width: u8,
    /// The pointer's last place in the window (a stroke starts there).
    at: Option<(i32, i32)>,
    /// A stroke is being drawn (the left button is down).
    stroke: bool,
    /// The last point sent (window pixels) and when.
    sent: Option<((i32, i32), Instant)>,
    /// A point the throttle holds back (window pixels, and as sent): it goes once due, or when the stroke ends.
    waiting: Option<((i32, i32), (u16, u16))>,
    /// Drawing mode was asked of that PC then, and it has not answered yet.
    asked: Option<Instant>,
    /// Drawing mode was turned off this many times without that PC's answer yet (its "not shown" is no refusal).
    offs_unanswered: u32,
    /// Esc went down while drawing: its release is drawing's too.
    escape_down: bool,
    /// For the report.
    times_on: u32,
    strokes: u32,
    points: u32,
}

/// A window position as that PC's screen position (0..=65535), as the pointer's moves map it.
fn position(at: (i32, i32), picture: (u32, u32), window: (u32, u32)) -> Option<(u16, u16)> {
    let moved = WindowEvent::MouseMove { x: at.0, y: at.1 };
    match crate::session::translate(moved, picture, window, &mut HashSet::new()).first() {
        Some(Message::Input(InputEvent::MouseMove { x, y })) => Some((*x, *y)),
        _ => None,
    }
}

impl Drawing {
    pub(crate) fn new() -> Drawing {
        Drawing {
            on: false,
            pen: MENU_PENS[0].1,
            width: PEN_WIDTH,
            at: None,
            stroke: false,
            sent: None,
            waiting: None,
            asked: None,
            offs_unanswered: 0,
            escape_down: false,
            times_on: 0,
            strokes: 0,
            points: 0,
        }
    }

    pub(crate) fn is_on(&self) -> bool {
        self.on
    }

    /// Whether the window's events go through [`Drawing::event`] now: while drawing, and Esc's release after Esc ended
    /// it (that PC never had the press).
    pub(crate) fn takes_events(&self) -> bool {
        self.on || self.escape_down
    }

    fn start(&mut self, now: Instant) -> Vec<Message> {
        self.on = true;
        self.asked = Some(now);
        self.times_on += 1;
        vec![Message::Annotate(Annotation::On)]
    }

    /// Drawing mode off: the stroke ended, that PC told (its strokes go).
    pub(crate) fn stop(&mut self) -> Vec<Message> {
        if !self.on {
            return Vec::new();
        }
        let mut messages = self.finish(Instant::now());
        self.on = false;
        self.asked = None;
        self.offs_unanswered += 1;
        messages.push(Message::Annotate(Annotation::Off));
        messages
    }

    /// One of drawing's menu items was chosen: on or off, clear, a pen (which also turns drawing on).
    pub(crate) fn menu(&mut self, id: u16, now: Instant) -> Vec<Message> {
        match id {
            MENU_DRAW if self.on => self.stop(),
            MENU_DRAW => self.start(now),
            MENU_DRAW_CLEAR if self.on => vec![Message::Annotate(Annotation::Clear)],
            _ => match MENU_PENS.iter().find(|(item, _, _)| *item == id) {
                Some((_, colour, _)) => {
                    self.pen = *colour;
                    if self.on { Vec::new() } else { self.start(now) }
                }
                None => Vec::new(),
            },
        }
    }

    /// Whether a point at `at` may go now.
    fn due(&self, at: (i32, i32), now: Instant) -> bool {
        match self.sent {
            None => true,
            Some((last, when)) => {
                (at.0 - last.0).abs().max((at.1 - last.1).abs()) >= MIN_STEP
                    && now.duration_since(when) >= MIN_GAP
            }
        }
    }

    fn send_point(&mut self, at: (i32, i32), point: (u16, u16), now: Instant) -> Message {
        self.sent = Some((at, now));
        self.waiting = None;
        self.points += 1;
        Message::Annotate(Annotation::Point {
            x: point.0,
            y: point.1,
        })
    }

    /// The stroke's end: the point held back (where the pointer stopped), then End.
    fn finish(&mut self, now: Instant) -> Vec<Message> {
        if !self.stroke {
            return Vec::new();
        }
        self.stroke = false;
        let mut messages = Vec::new();
        if let Some((at, point)) = self.waiting.take() {
            messages.push(self.send_point(at, point, now));
        }
        messages.push(Message::Annotate(Annotation::End));
        messages
    }

    /// A window event while drawing (and Esc's release after Esc ended it: see [`Drawing::takes_events`]). `picture`:
    /// the picture's size; `window`: the window's.
    pub(crate) fn event(
        &mut self,
        event: &WindowEvent,
        picture: (u32, u32),
        window: (u32, u32),
        now: Instant,
    ) -> Outcome {
        let taken = |messages: Vec<Message>| Outcome {
            messages,
            taken: true,
            ended: false,
        };
        let escape_released = matches!(
            *event,
            WindowEvent::Key {
                scancode: ESCAPE,
                extended: false,
                down: false,
            }
        );
        if !self.on {
            // Drawing mode is over: only Esc's release is still drawing's, anything else goes on as always.
            if escape_released {
                self.escape_down = false;
            }
            return Outcome {
                messages: Vec::new(),
                taken: escape_released,
                ended: false,
            };
        }
        match *event {
            WindowEvent::MouseMove { x, y } => {
                self.at = Some((x, y));
                if !self.stroke {
                    return taken(Vec::new());
                }
                let Some(point) = position((x, y), picture, window) else {
                    return taken(Vec::new());
                };
                if self.due((x, y), now) {
                    let message = self.send_point((x, y), point, now);
                    taken(vec![message])
                } else {
                    self.waiting = Some(((x, y), point));
                    taken(Vec::new())
                }
            }
            WindowEvent::MouseButton { button: 0, down } => {
                // A press without its release ends the stroke before.
                let mut messages = self.finish(now);
                if down
                    && let Some(at) = self.at
                    && let Some(point) = position(at, picture, window)
                {
                    self.stroke = true;
                    self.strokes += 1;
                    self.sent = None;
                    messages.push(Message::Annotate(Annotation::Begin {
                        colour: self.pen,
                        width: self.width,
                    }));
                    messages.push(self.send_point(at, point, now));
                }
                taken(messages)
            }
            // The other buttons and the wheel do nothing while drawing, nor does the game mouse's movement.
            WindowEvent::MouseButton { .. }
            | WindowEvent::Wheel { .. }
            | WindowEvent::MouseDelta { .. } => taken(Vec::new()),
            WindowEvent::Key {
                scancode: ESCAPE,
                extended: false,
                down: true,
            } => {
                self.escape_down = true;
                Outcome {
                    messages: self.stop(),
                    taken: true,
                    ended: true,
                }
            }
            // The window lost the keyboard: the stroke ends; its keys are let go as always.
            WindowEvent::FocusLost => Outcome {
                messages: self.finish(now),
                taken: false,
                ended: false,
            },
            _ => Outcome {
                messages: Vec::new(),
                taken: false,
                ended: false,
            },
        }
    }

    /// Each pass of the session's loop: the point held back once due; and whether drawing mode just ended because
    /// that PC never answered it (then the messages end it there too, in case it was only slow).
    pub(crate) fn poll(&mut self, now: Instant) -> (Vec<Message>, bool) {
        let mut messages = Vec::new();
        if self.stroke
            && let Some((at, point)) = self.waiting
            && self.due(at, now)
        {
            messages.push(self.send_point(at, point, now));
        }
        let unanswered = self
            .asked
            .is_some_and(|asked| now.duration_since(asked) >= ANSWER_WAIT);
        if self.on && unanswered {
            messages.extend(self.stop());
            return (messages, true);
        }
        (messages, false)
    }

    /// That PC's answer: shown, or not. True when it ends drawing mode here (that PC cannot show the drawing).
    pub(crate) fn answered(&mut self, shown: bool) -> bool {
        if shown {
            self.asked = None;
            return false;
        }
        if self.offs_unanswered > 0 {
            self.offs_unanswered -= 1;
            return false;
        }
        if self.on {
            self.on = false;
            self.stroke = false;
            self.asked = None;
            return true;
        }
        false
    }

    /// The report's line, if the person drew at all.
    pub(crate) fn report(&self) -> Option<String> {
        (self.times_on > 0).then(|| {
            format!(
                "drawing on that PC's screen: on {} times, {} strokes, {} points sent",
                self.times_on, self.strokes, self.points
            )
        })
    }
}

/// The pointer while drawing: a round spot in the pen's colour with a dark and a light ring (seen on any picture), its
/// middle the hot spot.
pub(crate) fn pen_pointer(colour: u32) -> PointerShape {
    const SIZE: u16 = 15;
    let middle = f32::from(SIZE / 2);
    let mut data = Vec::with_capacity(usize::from(SIZE) * usize::from(SIZE) * 4);
    for y in 0..SIZE {
        for x in 0..SIZE {
            let distance = (f32::from(x) - middle).hypot(f32::from(y) - middle);
            // B, G, R, A.
            let pixel = if distance <= 3.5 {
                [colour as u8, (colour >> 8) as u8, (colour >> 16) as u8, 255]
            } else if distance <= 5.0 {
                [0x20, 0x20, 0x20, 255]
            } else if distance <= 6.5 {
                [0xFF, 0xFF, 0xFF, 255]
            } else {
                [0, 0, 0, 0]
            };
            data.extend(pixel);
        }
    }
    PointerShape {
        kind: PointerKind::Color,
        width: SIZE,
        height: SIZE,
        hot_x: SIZE / 2,
        hot_y: SIZE / 2,
        data,
    }
}

/// The window menu's items: drawing on or off, clear, the pens.
pub(crate) fn add_menu(window: &Window) {
    window.add_menu_items(&[
        (MENU_DRAW, tr("Draw on that PC's screen")),
        (MENU_DRAW_CLEAR, tr("Clear drawings")),
    ]);
    let pens: Vec<(u16, &str)> = MENU_PENS
        .iter()
        .map(|(id, _, label)| (*id, tr(label)))
        .collect();
    window.add_submenu(tr("Pen colour"), &pens);
    window.check_menu_item(MENU_DRAW, false);
    window.check_menu_item(MENU_PENS[0].0, true);
}

/// The control center's Tools begin with drawing.
pub(crate) fn panel_items() -> Vec<PanelItem> {
    let mut items = vec![
        PanelItem::heading(tr("Drawing")),
        PanelItem::check(MENU_DRAW, tr("Draw on that PC's screen")).detail(tr(
            "Your mouse draws on that PC's screen for the person there instead of controlling it; keys still go there. Esc ends it, and the drawings go with it.",
        )),
    ];
    items.extend(
        MENU_PENS
            .iter()
            .map(|(id, _, label)| PanelItem::radio(*id, tr(label))),
    );
    items.push(PanelItem::action(MENU_DRAW_CLEAR, tr("Clear drawings")));
    items.push(PanelItem::Separator);
    items
}

/// The session window as drawing mode has it: the menus' marks, and the pointer (the pen's spot while drawing, no
/// game mouse then; that PC's own shape again after: `remote`, hidden if `remote_hidden`).
pub(crate) fn show(
    window: &mut Window,
    drawing: &Drawing,
    remote: Option<&PointerShape>,
    remote_hidden: bool,
) {
    window.check_menu_item(MENU_DRAW, drawing.on);
    for (id, colour, _) in MENU_PENS {
        window.check_menu_item(id, colour == drawing.pen);
    }
    if drawing.on {
        if window.is_game_mouse() {
            window.set_game_mouse(false);
            window.check_menu_item(MENU_GAME_MOUSE, false);
        }
        window.set_remote_cursor(RemoteCursor::from_shape(&pen_pointer(drawing.pen)));
        window.set_pointer_hidden(false);
    } else {
        window.set_remote_cursor(remote.and_then(RemoteCursor::from_shape));
        window.set_pointer_hidden(remote_hidden);
    }
}

/// Tests (--annotate-test): drawing as a person would, through the session window's own events, from 1.2 s after the
/// first picture: drawing mode on (its menu item), a stroke drawn with the mouse (40 moves in a second), a picture of
/// the window with it (viewer_annotation.bmp, before the session's other tests put their windows over it: the control
/// center at 3 s, the file transfer window after), the drawings cleared, Esc. The input test's moves and key go to that
/// PC meanwhile (through the drawing's window there). That PC must answer each step: shown with 0 strokes, with 1, with
/// 0 again, then not shown.
#[derive(Default)]
pub(crate) struct DrawTest {
    step: u8,
    moves: usize,
    pub(crate) answers: Vec<(bool, u16)>,
}

/// The test's pen: magenta, which no window on the test's screen has (the script looks for it there and in the
/// picture), thick enough to keep its colour through the picture's 4:2:0.
pub(crate) const TEST_PEN: u32 = 0xFF_00FF;
pub(crate) const TEST_WIDTH: u8 = 14;
const TEST_MOVES: usize = 40;
/// When each step comes, in milliseconds after the first picture.
const TEST_ON: u64 = 1200;
const TEST_STROKE: u64 = 1600;
const TEST_PICTURE: u64 = 2850;
const TEST_CLEAR: u64 = 4000;
const TEST_ESCAPE: u64 = 5000;

impl DrawTest {
    /// The window events due now (`since_first`: since the first picture), and whether the window's picture is due.
    pub(crate) fn events(
        &mut self,
        since_first: Option<Duration>,
        drawing: &mut Drawing,
        picture: (u32, u32),
        window: (u32, u32),
    ) -> (Vec<WindowEvent>, bool) {
        let Some(at) = since_first else {
            return (Vec::new(), false);
        };
        if picture.0 == 0 || picture.1 == 0 {
            return (Vec::new(), false);
        }
        let ms = |ms: u64| Duration::from_millis(ms);
        // The stroke: a W across the upper right of that screen (fractions of it), in the window's pixels.
        let area = queqiao_display::fit(picture, window);
        let place = |index: usize| {
            let along = index as f64 / (TEST_MOVES - 1) as f64;
            let zigzag = 1.0 - ((along * 4.0) % 2.0 - 1.0).abs();
            let (fx, fy) = (0.55 + 0.35 * along, 0.15 + 0.25 * zigzag);
            let x = f64::from(area.left) + fx * f64::from(area.right - area.left - 1);
            let y = f64::from(area.top) + fy * f64::from(area.bottom - area.top - 1);
            WindowEvent::MouseMove {
                x: x.round() as i32,
                y: y.round() as i32,
            }
        };
        let mut events = Vec::new();
        let mut picture_due = false;
        match self.step {
            0 if at >= ms(TEST_ON) => {
                drawing.pen = TEST_PEN;
                drawing.width = TEST_WIDTH;
                events.push(WindowEvent::Menu(MENU_DRAW));
                self.step = 1;
            }
            1 if at >= ms(TEST_STROKE) => {
                events.push(place(0));
                events.push(WindowEvent::MouseButton {
                    button: 0,
                    down: true,
                });
                self.moves = 1;
                self.step = 2;
            }
            2 => {
                while self.moves < TEST_MOVES && at >= ms(TEST_STROKE + 25 * self.moves as u64) {
                    events.push(place(self.moves));
                    self.moves += 1;
                }
                if self.moves == TEST_MOVES {
                    events.push(WindowEvent::MouseButton {
                        button: 0,
                        down: false,
                    });
                    self.step = 3;
                }
            }
            3 if at >= ms(TEST_PICTURE) => {
                picture_due = true;
                self.step = 4;
            }
            4 if at >= ms(TEST_CLEAR) => {
                events.push(WindowEvent::Menu(MENU_DRAW_CLEAR));
                self.step = 5;
            }
            5 if at >= ms(TEST_ESCAPE) => {
                for down in [true, false] {
                    events.push(WindowEvent::Key {
                        scancode: ESCAPE,
                        extended: false,
                        down,
                    });
                }
                self.step = 6;
            }
            _ => {}
        }
        (events, picture_due)
    }

    pub(crate) fn result(&self, drawing: &Drawing) -> String {
        let expected = [(true, 0), (true, 1), (true, 0), (false, 0)];
        let answers: Vec<String> = self
            .answers
            .iter()
            .map(|(shown, strokes)| {
                if *shown {
                    format!("shown with {strokes} strokes")
                } else {
                    "not shown".to_string()
                }
            })
            .collect();
        format!(
            "annotate test: {} (steps done: {} of 6; {} points sent; that PC answered: {})",
            if self.step == 6 && self.answers == expected && drawing.points >= 10 {
                "PASS"
            } else {
                "FAIL"
            },
            self.step,
            drawing.points,
            if answers.is_empty() {
                "nothing".to_string()
            } else {
                answers.join(", ")
            }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point(message: &Message) -> Option<(u16, u16)> {
        match message {
            Message::Annotate(Annotation::Point { x, y }) => Some((*x, *y)),
            _ => None,
        }
    }

    #[test]
    fn the_mouse_draws_a_stroke_and_esc_ends_drawing() {
        let (picture, window) = ((1920, 1080), (1920, 1080));
        let mut drawing = Drawing::new();
        let now = Instant::now();
        assert_eq!(
            drawing.menu(MENU_DRAW, now),
            [Message::Annotate(Annotation::On)]
        );
        assert!(drawing.is_on());
        // Moving without the button down draws nothing (and nothing goes to that PC as input).
        let moved = drawing.event(&WindowEvent::MouseMove { x: 0, y: 0 }, picture, window, now);
        assert!(moved.taken && moved.messages.is_empty());
        let pressed = drawing.event(
            &WindowEvent::MouseButton {
                button: 0,
                down: true,
            },
            picture,
            window,
            now,
        );
        assert_eq!(
            pressed.messages,
            [
                Message::Annotate(Annotation::Begin {
                    colour: MENU_PENS[0].1,
                    width: PEN_WIDTH
                }),
                Message::Annotate(Annotation::Point { x: 0, y: 0 })
            ]
        );
        // Too soon and too near: held back; far enough and later: sent, as a screen position.
        let near = drawing.event(&WindowEvent::MouseMove { x: 1, y: 0 }, picture, window, now);
        assert!(near.messages.is_empty());
        let later = now + Duration::from_millis(20);
        let far = drawing.event(
            &WindowEvent::MouseMove { x: 1919, y: 1079 },
            picture,
            window,
            later,
        );
        assert_eq!(
            far.messages.iter().filter_map(point).collect::<Vec<_>>(),
            [(65535, 65535)]
        );
        // A quick move is held back, then sent once due.
        let quick = drawing.event(
            &WindowEvent::MouseMove { x: 960, y: 540 },
            picture,
            window,
            later + Duration::from_millis(1),
        );
        assert!(quick.messages.is_empty());
        let (polled, ended) = drawing.poll(later + Duration::from_millis(10));
        assert!(!ended);
        assert_eq!(polled.len(), 1);
        assert!(point(&polled[0]).is_some());
        // Right clicks and the wheel do nothing; a key goes on to that PC.
        let right = drawing.event(
            &WindowEvent::MouseButton {
                button: 1,
                down: true,
            },
            picture,
            window,
            later,
        );
        assert!(right.taken && right.messages.is_empty());
        let key = WindowEvent::Key {
            scancode: 0x1E,
            extended: false,
            down: true,
        };
        assert!(!drawing.event(&key, picture, window, later).taken);
        // Released: the stroke ends.
        let released = drawing.event(
            &WindowEvent::MouseButton {
                button: 0,
                down: false,
            },
            picture,
            window,
            later,
        );
        assert_eq!(released.messages, [Message::Annotate(Annotation::End)]);
        // Esc ends drawing mode (and does not reach that PC, nor does its release).
        let escape = |down| WindowEvent::Key {
            scancode: ESCAPE,
            extended: false,
            down,
        };
        let ended = drawing.event(&escape(true), picture, window, later);
        assert!(ended.taken && ended.ended);
        assert_eq!(ended.messages, [Message::Annotate(Annotation::Off)]);
        assert!(!drawing.is_on());
        assert!(drawing.event(&escape(false), picture, window, later).taken);
        // That PC's "not shown" for that Off is no refusal.
        assert!(!drawing.answered(false));
    }

    #[test]
    fn drawing_ends_when_that_pc_cannot_show_it() {
        let now = Instant::now();
        // It never answers (a Queqiao before drawing): ended after the wait, with Off in case it was slow.
        let mut drawing = Drawing::new();
        drawing.menu(MENU_DRAW, now);
        assert_eq!(
            drawing.poll(now + Duration::from_secs(1)),
            (Vec::new(), false)
        );
        let (messages, ended) = drawing.poll(now + ANSWER_WAIT);
        assert!(ended && !drawing.is_on());
        assert_eq!(messages, [Message::Annotate(Annotation::Off)]);
        // It answers: no wait any more.
        let mut drawing = Drawing::new();
        drawing.menu(MENU_DRAW, now);
        assert!(!drawing.answered(true));
        assert!(!drawing.poll(now + ANSWER_WAIT * 2).1);
        // It refuses (Wayland): ended here.
        assert!(drawing.answered(false));
        assert!(!drawing.is_on());
    }

    #[test]
    fn choosing_a_pen_starts_drawing_in_its_colour() {
        let now = Instant::now();
        let mut drawing = Drawing::new();
        let (blue, colour, _) = MENU_PENS[3];
        assert_eq!(drawing.menu(blue, now), [Message::Annotate(Annotation::On)]);
        assert_eq!(drawing.pen, colour);
        assert!(drawing.menu(MENU_PENS[1].0, now).is_empty());
        assert_eq!(
            drawing.menu(MENU_DRAW_CLEAR, now),
            [Message::Annotate(Annotation::Clear)]
        );
        // Clearing while not drawing sends nothing.
        drawing.stop();
        assert!(drawing.menu(MENU_DRAW_CLEAR, now).is_empty());
    }

    #[test]
    fn the_pen_pointer_is_a_valid_shape_in_the_pens_colour() {
        let shape = pen_pointer(0x12_3456);
        assert!(shape.is_valid());
        let middle =
            (usize::from(shape.hot_y) * usize::from(shape.width) + usize::from(shape.hot_x)) * 4;
        assert_eq!(&shape.data[middle..middle + 4], [0x56, 0x34, 0x12, 255]);
        assert_eq!(&shape.data[..4], [0, 0, 0, 0]);
    }
}
