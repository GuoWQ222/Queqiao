//! The session window on X11, with the API of Windows' window.rs: input in the window collected as [`WindowEvent`]s
//! (keys as the protocol's scancodes, through the evdev table), `pump` called from the session's own loop, full screen
//! and "always on top" through the window manager (EWMH), the game mouse (the pointer held in the window, moves as
//! deltas), shortcuts sent while the window has the keyboard (a keyboard grab), the boss key as a global key grab.
//! The session menu is kept but not shown: it comes with the session bar (step 5).

use std::cell::{Cell, RefCell};
use std::time::{Duration, Instant};

use queqiao_input::scancode::scancode_from_evdev;
use x11rb::connection::Connection;
use x11rb::protocol::Event;
use x11rb::protocol::randr::ConnectionExt as _;
use x11rb::protocol::xproto::{
    AtomEnum, ChangeWindowAttributesAux, ClientMessageEvent, ConfigureWindowAux,
    ConnectionExt as _, CreateWindowAux, EventMask, GrabMode, GrabStatus, ModMask, PropMode,
    Window as XWindow, WindowClass,
};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

use super::cursor::{RemoteCursor, XCursor};
use super::{Result, x11_error};
use crate::model::WindowEvent;

x11rb::atom_manager! {
    pub(super) Atoms: AtomsCookie {
        WM_PROTOCOLS,
        WM_DELETE_WINDOW,
        UTF8_STRING,
        _NET_WM_NAME,
        _NET_WM_STATE,
        _NET_WM_STATE_FULLSCREEN,
        _NET_WM_STATE_ABOVE,
        _NET_WM_WINDOW_OPACITY,
    }
}

/// An event handled for longer than this is reported (`take_slow_messages`).
const SLOW: Duration = Duration::from_millis(50);

/// The X window, as Windows' HWND is passed around (`.0` its number).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hwnd(pub usize);

/// A rectangle in screen pixels (Windows: RECT).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

pub struct Window {
    pub(super) connection: RustConnection,
    pub(super) window: XWindow,
    root: XWindow,
    pub(super) depth: u8,
    atoms: Atoms,
    size: (u32, u32),
    visible: Cell<bool>,
    closed: bool,
    full_screen: bool,
    on_top: Cell<bool>,
    opacity: Cell<u8>,
    game_mouse: bool,
    send_shortcuts: bool,
    keyboard_grabbed: bool,
    focused: bool,
    pointer_hidden: Cell<bool>,
    /// That PC's pointer shape as this window's cursor, and the invisible one.
    remote_cursor: Option<XCursor>,
    hidden_cursor: RefCell<Option<XCursor>>,
    /// Hotkeys grabbed on the root window: id, modifiers, keycode.
    hotkeys: RefCell<Vec<(i32, u16, u8)>>,
    /// The session menu's items (id, text, checked): kept for the session bar to come.
    menu: RefCell<Vec<(u16, String, bool)>>,
    events: RefCell<Vec<WindowEvent>>,
    slow: Vec<String>,
}

impl Window {
    /// A window whose drawing area is `width` x `height`.
    pub fn new(title: &str, width: i32, height: i32) -> Result<Self> {
        let (connection, screen_number) = x11rb::connect(None).map_err(x11_error)?;
        let screen = connection.setup().roots[screen_number].clone();
        let atoms = Atoms::new(&connection)
            .map_err(x11_error)?
            .reply()
            .map_err(x11_error)?;
        let window = connection.generate_id().map_err(x11_error)?;
        let (width, height) = (
            width.clamp(64, 16384) as u16,
            height.clamp(64, 16384) as u16,
        );
        let events = EventMask::KEY_PRESS
            | EventMask::KEY_RELEASE
            | EventMask::BUTTON_PRESS
            | EventMask::BUTTON_RELEASE
            | EventMask::POINTER_MOTION
            | EventMask::FOCUS_CHANGE
            | EventMask::STRUCTURE_NOTIFY
            | EventMask::EXPOSURE;
        connection
            .create_window(
                screen.root_depth,
                window,
                screen.root,
                0,
                0,
                width,
                height,
                0,
                WindowClass::INPUT_OUTPUT,
                screen.root_visual,
                &CreateWindowAux::new()
                    .background_pixel(screen.black_pixel)
                    .event_mask(events),
            )
            .map_err(x11_error)?;
        connection
            .change_property32(
                PropMode::REPLACE,
                window,
                atoms.WM_PROTOCOLS,
                AtomEnum::ATOM,
                &[atoms.WM_DELETE_WINDOW],
            )
            .map_err(x11_error)?;
        let window = Window {
            connection,
            window,
            root: screen.root,
            depth: screen.root_depth,
            atoms,
            size: (u32::from(width), u32::from(height)),
            visible: Cell::new(true),
            closed: false,
            full_screen: false,
            on_top: Cell::new(false),
            opacity: Cell::new(100),
            game_mouse: false,
            send_shortcuts: false,
            keyboard_grabbed: false,
            focused: false,
            pointer_hidden: Cell::new(false),
            remote_cursor: None,
            hidden_cursor: RefCell::new(None),
            hotkeys: RefCell::new(Vec::new()),
            menu: RefCell::new(Vec::new()),
            events: RefCell::new(Vec::new()),
            slow: Vec::new(),
        };
        window.set_title(title);
        window
            .connection
            .map_window(window.window)
            .map_err(x11_error)?;
        window.connection.flush().map_err(x11_error)?;
        Ok(window)
    }

    /// The session menu's items: kept (no menu is shown on Linux yet).
    pub fn add_menu_items(&self, items: &[(u16, &str)]) {
        self.menu.borrow_mut().extend(
            items
                .iter()
                .map(|(id, text)| (*id, text.to_string(), false)),
        );
    }

    pub fn add_submenu(&self, _title: &str, items: &[(u16, &str)]) {
        self.add_menu_items(items);
    }

    pub fn rename_menu_item(&self, id: u16, text: &str) {
        if let Some(item) = self.menu.borrow_mut().iter_mut().find(|item| item.0 == id) {
            item.1 = text.to_string();
        }
    }

    pub fn check_menu_item(&self, id: u16, checked: bool) {
        if let Some(item) = self.menu.borrow_mut().iter_mut().find(|item| item.0 == id) {
            item.2 = checked;
        }
    }

    /// That PC's pointer shape as this window's cursor (None: the usual arrow).
    pub fn set_remote_cursor(&mut self, cursor: Option<RemoteCursor>) {
        let made = cursor.and_then(|cursor| XCursor::new(&self.connection, self.root, &cursor));
        let old = std::mem::replace(&mut self.remote_cursor, made);
        self.apply_cursor();
        if let Some(old) = old {
            old.free(&self.connection);
        }
    }

    /// Hides the pointer over the window (that PC's pointer is hidden) or shows it again.
    pub fn set_pointer_hidden(&self, hidden: bool) {
        self.pointer_hidden.set(hidden);
        self.apply_cursor();
    }

    fn apply_cursor(&self) {
        let cursor = if self.pointer_hidden.get() || self.game_mouse {
            let mut hidden = self.hidden_cursor.borrow_mut();
            if hidden.is_none() {
                *hidden = XCursor::new(&self.connection, self.root, &RemoteCursor::invisible());
            }
            hidden.as_ref().map_or(0, |c| c.id)
        } else {
            self.remote_cursor.as_ref().map_or(0, |c| c.id)
        };
        let _ = self.connection.change_window_attributes(
            self.window,
            &ChangeWindowAttributesAux::new().cursor(cursor),
        );
        let _ = self.connection.flush();
    }

    /// Registers a key with `modifiers` (X modifier bits) and X keycode `key` anywhere on the display: it comes as
    /// [`WindowEvent::Hotkey`]. False if another program has it.
    pub fn register_hotkey(&self, id: i32, modifiers: u32, key: u32) -> bool {
        let (Ok(modifiers), Ok(key)) = (u16::try_from(modifiers), u8::try_from(key)) else {
            return false;
        };
        // With and without Caps Lock and Num Lock (Mod2), which X counts as modifiers too.
        let locks = [
            0u16,
            u16::from(ModMask::LOCK),
            u16::from(ModMask::M2),
            u16::from(ModMask::LOCK | ModMask::M2),
        ];
        let grabbed = locks.iter().all(|lock| {
            self.connection
                .grab_key(
                    true,
                    self.root,
                    ModMask::from(modifiers | lock),
                    key,
                    GrabMode::ASYNC,
                    GrabMode::ASYNC,
                )
                .ok()
                .and_then(|cookie| cookie.check().ok())
                .is_some()
        });
        if grabbed {
            self.hotkeys.borrow_mut().push((id, modifiers, key));
        } else {
            self.ungrab(modifiers, key);
        }
        grabbed
    }

    fn ungrab(&self, modifiers: u16, key: u8) {
        for lock in [
            0u16,
            u16::from(ModMask::LOCK),
            u16::from(ModMask::M2),
            u16::from(ModMask::LOCK | ModMask::M2),
        ] {
            let _ = self
                .connection
                .ungrab_key(key, self.root, ModMask::from(modifiers | lock));
        }
        let _ = self.connection.flush();
    }

    pub fn unregister_hotkey(&self, id: i32) {
        let removed: Vec<(i32, u16, u8)> = self
            .hotkeys
            .borrow()
            .iter()
            .copied()
            .filter(|h| h.0 == id)
            .collect();
        self.hotkeys.borrow_mut().retain(|h| h.0 != id);
        for (_, modifiers, key) in removed {
            self.ungrab(modifiers, key);
        }
    }

    /// For tests: as the hotkey pressed.
    pub fn press_hotkey(&self, id: i32) {
        self.events.borrow_mut().push(WindowEvent::Hotkey(id));
    }

    pub fn set_visible(&self, visible: bool) {
        self.visible.set(visible);
        let _ = if visible {
            self.connection.map_window(self.window)
        } else {
            self.connection.unmap_window(self.window)
        };
        let _ = self.connection.flush();
    }

    pub fn is_visible(&self) -> bool {
        self.visible.get()
    }

    /// Game mouse: the pointer is held in the window (hidden, put back in the middle after each move) and moves go
    /// to that PC as deltas ([`WindowEvent::MouseDelta`]).
    pub fn set_game_mouse(&mut self, on: bool) {
        self.game_mouse = on;
        if on {
            let events =
                EventMask::POINTER_MOTION | EventMask::BUTTON_PRESS | EventMask::BUTTON_RELEASE;
            let _ = self.connection.grab_pointer(
                true,
                self.window,
                events,
                GrabMode::ASYNC,
                GrabMode::ASYNC,
                self.window,
                x11rb::NONE,
                x11rb::CURRENT_TIME,
            );
            self.warp_to_middle();
        } else {
            let _ = self.connection.ungrab_pointer(x11rb::CURRENT_TIME);
        }
        self.apply_cursor();
    }

    fn middle(&self) -> (i16, i16) {
        ((self.size.0 / 2) as i16, (self.size.1 / 2) as i16)
    }

    fn warp_to_middle(&self) {
        let (x, y) = self.middle();
        let _ = self
            .connection
            .warp_pointer(x11rb::NONE, self.window, 0, 0, 0, 0, x, y);
        let _ = self.connection.flush();
    }

    /// Shortcuts (Alt+Tab, the Windows key...) go to that PC while the window has the keyboard: it is grabbed then.
    pub fn set_send_shortcuts(&mut self, on: bool) {
        self.send_shortcuts = on;
        self.update_keyboard_grab();
    }

    fn update_keyboard_grab(&mut self) {
        let want = self.send_shortcuts && self.focused && self.visible.get();
        if want && !self.keyboard_grabbed {
            let grabbed = self
                .connection
                .grab_keyboard(
                    true,
                    self.window,
                    x11rb::CURRENT_TIME,
                    GrabMode::ASYNC,
                    GrabMode::ASYNC,
                )
                .ok()
                .and_then(|cookie| cookie.reply().ok())
                .is_some_and(|reply| reply.status == GrabStatus::SUCCESS);
            self.keyboard_grabbed = grabbed;
        } else if !want && self.keyboard_grabbed {
            let _ = self.connection.ungrab_keyboard(x11rb::CURRENT_TIME);
            self.keyboard_grabbed = false;
        }
        let _ = self.connection.flush();
    }

    pub fn is_sending_shortcuts(&self) -> bool {
        self.send_shortcuts
    }

    pub fn is_game_mouse(&self) -> bool {
        self.game_mouse
    }

    /// Asks the window manager to keep the window above others (or not).
    pub fn set_always_on_top(&self, on: bool) {
        self.on_top.set(on);
        self.net_wm_state(on, self.atoms._NET_WM_STATE_ABOVE);
    }

    pub fn is_full_screen(&self) -> bool {
        self.full_screen
    }

    /// The window's opacity, 20 to 100 percent (the compositor applies it).
    pub fn set_opacity(&self, percent: u8) {
        let percent = percent.clamp(20, 100);
        self.opacity.set(percent);
        let value = (u64::from(percent) * u64::from(u32::MAX) / 100) as u32;
        let _ = if percent >= 100 {
            self.connection
                .delete_property(self.window, self.atoms._NET_WM_WINDOW_OPACITY)
        } else {
            self.connection.change_property32(
                PropMode::REPLACE,
                self.window,
                self.atoms._NET_WM_WINDOW_OPACITY,
                AtomEnum::CARDINAL,
                &[value],
            )
        };
        let _ = self.connection.flush();
    }

    pub fn opacity(&self) -> u8 {
        self.opacity.get()
    }

    /// Makes the drawing area `width` x `height` (not while full screen).
    pub fn set_client_size(&self, width: u32, height: u32) {
        if self.full_screen {
            return;
        }
        let size = ConfigureWindowAux::new()
            .width(width.clamp(64, 16384))
            .height(height.clamp(64, 16384));
        let _ = self.connection.configure_window(self.window, &size);
        let _ = self.connection.flush();
    }

    /// Full screen on the monitor the window is on (the window manager does it, EWMH).
    pub fn set_full_screen(&mut self, on: bool) {
        self.full_screen = on;
        self.net_wm_state(on, self.atoms._NET_WM_STATE_FULLSCREEN);
    }

    fn net_wm_state(&self, on: bool, state: u32) {
        let message = ClientMessageEvent::new(
            32,
            self.window,
            self.atoms._NET_WM_STATE,
            [u32::from(on), state, 0, 1, 0],
        );
        let mask = EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY;
        let _ = self.connection.send_event(false, self.root, mask, message);
        let _ = self.connection.flush();
    }

    /// The monitor the window is on (RandR), or the whole screen.
    pub fn monitor_rect(&self) -> Option<Rect> {
        let place = self
            .connection
            .translate_coordinates(self.window, self.root, 0, 0)
            .ok()?
            .reply()
            .ok()?;
        let (x, y) = (
            i32::from(place.dst_x) + self.size.0 as i32 / 2,
            i32::from(place.dst_y) + self.size.1 as i32 / 2,
        );
        let monitors = self
            .connection
            .randr_get_monitors(self.root, true)
            .ok()
            .and_then(|cookie| cookie.reply().ok())
            .map(|reply| reply.monitors)
            .unwrap_or_default();
        let rect = |left: i32, top: i32, width: i32, height: i32| Rect {
            left,
            top,
            right: left + width,
            bottom: top + height,
        };
        monitors
            .iter()
            .map(|m| rect(m.x.into(), m.y.into(), m.width.into(), m.height.into()))
            .find(|r| (r.left..r.right).contains(&x) && (r.top..r.bottom).contains(&y))
            .or_else(|| {
                let screen = &self.connection.setup().roots[0];
                Some(rect(
                    0,
                    0,
                    screen.width_in_pixels.into(),
                    screen.height_in_pixels.into(),
                ))
            })
    }

    pub fn set_title(&self, title: &str) {
        let _ = self.connection.change_property8(
            PropMode::REPLACE,
            self.window,
            self.atoms._NET_WM_NAME,
            self.atoms.UTF8_STRING,
            title.as_bytes(),
        );
        let _ = self.connection.change_property8(
            PropMode::REPLACE,
            self.window,
            AtomEnum::WM_NAME,
            AtomEnum::STRING,
            title.as_bytes(),
        );
        let _ = self.connection.flush();
    }

    /// Asks the person to confirm (yes / no): zenity's question when it is there, else no.
    pub fn confirm(&self, title: &str, text: &str) -> bool {
        super::forms::zenity(&["--question", "--title", title, "--text", text]).is_some()
    }

    pub fn hwnd(&self) -> Hwnd {
        Hwnd(self.window as usize)
    }

    /// Handles what came for the window. False once it was closed.
    pub fn pump(&mut self) -> bool {
        let mut pending = Vec::new();
        while let Ok(Some(event)) = self.connection.poll_for_event() {
            pending.push(event);
        }
        for index in 0..pending.len() {
            let started = Instant::now();
            // Key repeat: X sends a release and a press at the same time; only the press goes on (as Windows'
            // repeated key-down).
            if let Event::KeyRelease(release) = &pending[index]
                && let Some(Event::KeyPress(press)) = pending.get(index + 1)
                && press.detail == release.detail
                && press.time == release.time
            {
                continue;
            }
            self.handle(&pending[index]);
            if started.elapsed() > SLOW && self.slow.len() < 20 {
                self.slow.push(format!(
                    "event {:?} took {} ms",
                    pending[index].response_type(),
                    started.elapsed().as_millis()
                ));
            }
        }
        let _ = self.connection.flush();
        !self.closed
    }

    fn handle(&mut self, event: &Event) {
        match event {
            Event::KeyPress(key) if key.event == self.root => {
                let modifiers = u16::from(key.state)
                    & !(u16::from(ModMask::LOCK) | u16::from(ModMask::M2) | 0xFF00);
                let found = self
                    .hotkeys
                    .borrow()
                    .iter()
                    .find(|(_, m, k)| *k == key.detail && *m == modifiers)
                    .map(|(id, _, _)| *id);
                if let Some(id) = found {
                    self.events.borrow_mut().push(WindowEvent::Hotkey(id));
                }
            }
            Event::KeyPress(key) => self.key(key.detail, true),
            Event::KeyRelease(key) => self.key(key.detail, false),
            Event::ButtonPress(button) => self.button(button.detail, true),
            Event::ButtonRelease(button) => self.button(button.detail, false),
            Event::MotionNotify(motion) => {
                let (x, y) = (i32::from(motion.event_x), i32::from(motion.event_y));
                if self.game_mouse {
                    let (mx, my) = self.middle();
                    let (dx, dy) = (x - i32::from(mx), y - i32::from(my));
                    if (dx, dy) != (0, 0) {
                        self.events
                            .borrow_mut()
                            .push(WindowEvent::MouseDelta { dx, dy });
                        self.warp_to_middle();
                    }
                } else {
                    self.events
                        .borrow_mut()
                        .push(WindowEvent::MouseMove { x, y });
                }
            }
            Event::FocusIn(_) => {
                self.focused = true;
                self.update_keyboard_grab();
            }
            Event::FocusOut(focus) => {
                // A grab's own focus changes are not the window losing the keyboard.
                if focus.mode != x11rb::protocol::xproto::NotifyMode::GRAB
                    && focus.mode != x11rb::protocol::xproto::NotifyMode::UNGRAB
                {
                    self.focused = false;
                    self.events.borrow_mut().push(WindowEvent::FocusLost);
                    self.update_keyboard_grab();
                }
            }
            Event::ConfigureNotify(configure) if configure.window == self.window => {
                self.size = (u32::from(configure.width), u32::from(configure.height));
            }
            Event::MapNotify(_) => self.visible.set(true),
            Event::UnmapNotify(_) => self.visible.set(false),
            Event::ClientMessage(message)
                if message.type_ == self.atoms.WM_PROTOCOLS
                    && message.data.as_data32()[0] == self.atoms.WM_DELETE_WINDOW =>
            {
                self.closed = true;
            }
            _ => {}
        }
    }

    fn key(&mut self, keycode: u8, down: bool) {
        // X keycodes are evdev codes + 8; keys the table does not know are left out.
        let Some((scancode, extended)) = keycode
            .checked_sub(8)
            .and_then(|code| scancode_from_evdev(u16::from(code)))
        else {
            return;
        };
        self.events.borrow_mut().push(WindowEvent::Key {
            scancode,
            extended,
            down,
        });
    }

    fn button(&mut self, button: u8, down: bool) {
        let event = match button {
            1 => WindowEvent::MouseButton { button: 0, down },
            2 => WindowEvent::MouseButton { button: 2, down },
            3 => WindowEvent::MouseButton { button: 1, down },
            8 => WindowEvent::MouseButton { button: 3, down },
            9 => WindowEvent::MouseButton { button: 4, down },
            // The wheel: a press per notch (the release says nothing more).
            4..=7 if !down => return,
            4 => WindowEvent::Wheel {
                delta: 120,
                horizontal: false,
            },
            5 => WindowEvent::Wheel {
                delta: -120,
                horizontal: false,
            },
            6 => WindowEvent::Wheel {
                delta: -120,
                horizontal: true,
            },
            7 => WindowEvent::Wheel {
                delta: 120,
                horizontal: true,
            },
            _ => return,
        };
        self.events.borrow_mut().push(event);
    }

    pub fn take_slow_messages(&mut self) -> Vec<String> {
        std::mem::take(&mut self.slow)
    }

    pub fn take_events(&mut self) -> Vec<WindowEvent> {
        std::mem::take(self.events.get_mut())
    }

    /// The drawing area's size.
    pub fn client_size(&self) -> (u32, u32) {
        self.size
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        for (_, modifiers, key) in std::mem::take(self.hotkeys.get_mut()) {
            self.ungrab(modifiers, key);
        }
        if self.game_mouse {
            let _ = self.connection.ungrab_pointer(x11rb::CURRENT_TIME);
        }
        if self.keyboard_grabbed {
            let _ = self.connection.ungrab_keyboard(x11rb::CURRENT_TIME);
        }
        let _ = self.connection.destroy_window(self.window);
        for cursor in [
            self.remote_cursor.take(),
            self.hidden_cursor.get_mut().take(),
        ]
        .into_iter()
        .flatten()
        {
            cursor.free(&self.connection);
        }
        let _ = self.connection.flush();
    }
}
