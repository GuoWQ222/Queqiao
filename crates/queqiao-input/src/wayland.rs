//! Input in a Wayland session (docs/LINUX.md), where X11's XTEST reaches only XWayland's windows: the wlroots
//! compositors' virtual pointer (zwlr_virtual_pointer_v1) and the virtual keyboard (zwp_virtual_keyboard_v1), which
//! sway, Hyprland, Wayfire and other wlroots desktops offer. Positions map onto the whole output layout, whose size
//! XWayland's screen has (positions are desktop pixels, as under X11). The keyboard takes this desktop's keymap: the
//! seat's own (what its keyboard uses), or where the seat has no keyboard yet, the system's default from
//! libxkbcommon (loaded at run time), so keys mean what they mean here. GNOME and KDE have neither protocol: there
//! input goes through the RemoteDesktop portal session the capture shares (queqiao-portal), positions on its stream.

use std::ffi::{CStr, c_char, c_void};
use std::io::Write;
use std::os::fd::{AsFd, FromRawFd, OwnedFd};
use std::time::Duration;

use queqiao_core::protocol::{InputEvent, MouseButton};
use wayland_client::protocol::{wl_keyboard, wl_pointer, wl_registry, wl_seat};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::{
    zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1,
    zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1,
};
use wayland_protocols_wlr::virtual_pointer::v1::client::{
    zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1,
    zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1,
};

use crate::ScreenRect;
use crate::scancode::x11_keycode;

/// Linux input event codes of the buttons (linux/input-event-codes.h).
const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;
const BTN_MIDDLE: u32 = 0x112;
const BTN_SIDE: u32 = 0x113;
const BTN_EXTRA: u32 = 0x114;
/// Windows' wheel units per notch, and the length Wayland gives a notch.
const NOTCH: i32 = 120;
const NOTCH_LENGTH: f64 = 15.0;
/// A new virtual device makes the seat gain a pointer or keyboard; programs take a moment to listen to it.
const SETTLE: Duration = Duration::from_millis(500);

/// Whether this is a Wayland session (as the capture decides it).
pub(crate) fn wayland_session() -> bool {
    let set = |name: &str| std::env::var_os(name).is_some_and(|value| !value.is_empty());
    set("WAYLAND_DISPLAY")
        && (std::env::var("XDG_SESSION_TYPE").is_ok_and(|kind| kind == "wayland")
            || !set("DISPLAY"))
}

#[derive(Default)]
struct Globals {
    seat: Option<wl_seat::WlSeat>,
    pointers: Option<ZwlrVirtualPointerManagerV1>,
    keyboards: Option<ZwpVirtualKeyboardManagerV1>,
    capabilities: u32,
    keymap: Option<(OwnedFd, u32)>,
}

impl Dispatch<wl_registry::WlRegistry, ()> for Globals {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            match interface.as_str() {
                "wl_seat" if state.seat.is_none() => {
                    state.seat = Some(registry.bind(name, version.min(7), qh, ()));
                }
                "zwlr_virtual_pointer_manager_v1" => {
                    state.pointers = Some(registry.bind(name, version.min(2), qh, ()));
                }
                "zwp_virtual_keyboard_manager_v1" => {
                    state.keyboards = Some(registry.bind(name, 1, qh, ()));
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for Globals {
    fn event(
        state: &mut Self,
        _: &wl_seat::WlSeat,
        event: wl_seat::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities {
            capabilities: WEnum::Value(capabilities),
        } = event
        {
            state.capabilities = capabilities.bits();
        }
    }
}

impl Dispatch<wl_keyboard::WlKeyboard, ()> for Globals {
    fn event(
        state: &mut Self,
        _: &wl_keyboard::WlKeyboard,
        event: wl_keyboard::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_keyboard::Event::Keymap { fd, size, .. } = event {
            state.keymap = Some((fd, size));
        }
    }
}

wayland_client::delegate_noop!(Globals: ignore ZwlrVirtualPointerManagerV1);
wayland_client::delegate_noop!(Globals: ignore ZwlrVirtualPointerV1);
wayland_client::delegate_noop!(Globals: ignore ZwpVirtualKeyboardManagerV1);
wayland_client::delegate_noop!(Globals: ignore ZwpVirtualKeyboardV1);

/// The system's default keymap (XKB_DEFAULT_* honoured) as text, from libxkbcommon.
fn default_keymap() -> Result<String, String> {
    type ContextNew = unsafe extern "C" fn(i32) -> *mut c_void;
    type KeymapNew = unsafe extern "C" fn(*mut c_void, *const c_void, i32) -> *mut c_void;
    type AsString = unsafe extern "C" fn(*mut c_void, i32) -> *mut c_char;
    type Free = unsafe extern "C" fn(*mut c_void);
    unsafe {
        let library = libc::dlopen(
            c"libxkbcommon.so.0".as_ptr(),
            libc::RTLD_NOW | libc::RTLD_LOCAL,
        );
        if library.is_null() {
            return Err("no libxkbcommon for a keymap".into());
        }
        let get = |name: &CStr| {
            let address = libc::dlsym(library, name.as_ptr());
            (!address.is_null()).then_some(address)
        };
        let (
            Some(context_new),
            Some(keymap_new),
            Some(as_string),
            Some(keymap_unref),
            Some(context_unref),
        ) = (
            get(c"xkb_context_new"),
            get(c"xkb_keymap_new_from_names"),
            get(c"xkb_keymap_get_as_string"),
            get(c"xkb_keymap_unref"),
            get(c"xkb_context_unref"),
        )
        else {
            return Err("libxkbcommon lacks a function".into());
        };
        let context_new = std::mem::transmute_copy::<*mut c_void, ContextNew>(&context_new);
        let keymap_new = std::mem::transmute_copy::<*mut c_void, KeymapNew>(&keymap_new);
        let as_string = std::mem::transmute_copy::<*mut c_void, AsString>(&as_string);
        let keymap_unref = std::mem::transmute_copy::<*mut c_void, Free>(&keymap_unref);
        let context_unref = std::mem::transmute_copy::<*mut c_void, Free>(&context_unref);
        let context = context_new(0);
        if context.is_null() {
            return Err("libxkbcommon: no context".into());
        }
        let keymap = keymap_new(context, std::ptr::null(), 0);
        let text = if keymap.is_null() {
            Err("libxkbcommon made no keymap".to_string())
        } else {
            let text = as_string(keymap, 1);
            let result = if text.is_null() {
                Err("libxkbcommon: no keymap text".to_string())
            } else {
                let owned = CStr::from_ptr(text).to_string_lossy().into_owned();
                libc::free(text.cast());
                Ok(owned)
            };
            keymap_unref(keymap);
            result
        };
        context_unref(context);
        text
    }
}

/// The keymap text in a memory file, as the protocol takes it (the file and its size with the final NUL).
fn keymap_file(text: &str) -> Result<(OwnedFd, u32), String> {
    let fd = unsafe { libc::memfd_create(c"queqiao-keymap".as_ptr(), libc::MFD_CLOEXEC) };
    if fd < 0 {
        return Err(format!("memfd_create: {}", std::io::Error::last_os_error()));
    }
    let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
    file.write_all(text.as_bytes())
        .and_then(|()| file.write_all(&[0]))
        .map_err(|e| e.to_string())?;
    Ok((OwnedFd::from(file), text.len() as u32 + 1))
}

/// The output layout's size, as XWayland's screen has it; else the captured screen's far corner.
fn layout_size(screen: ScreenRect) -> (u32, u32) {
    use x11rb::connection::Connection as _;
    x11rb::connect(None)
        .ok()
        .and_then(|(connection, number)| {
            let root = connection.setup().roots.get(number)?;
            Some((
                u32::from(root.width_in_pixels),
                u32::from(root.height_in_pixels),
            ))
        })
        .filter(|(width, height)| *width > 0 && *height > 0)
        .unwrap_or((
            (screen.left.max(0) as u32 + screen.width).max(1),
            (screen.top.max(0) as u32 + screen.height).max(1),
        ))
}

fn now_ms() -> u32 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| (since.as_millis() & 0xFFFF_FFFF) as u32)
}

/// Input on Wayland: the compositor's virtual devices, or the RemoteDesktop portal.
pub(crate) enum WaylandInjector {
    Virtual(Box<WaylandInput>),
    Portal {
        session: std::sync::Arc<queqiao_portal::PortalSession>,
        wheel: (i32, i32),
    },
}

impl WaylandInjector {
    /// The virtual devices where the compositor has them, else the portal session's RemoteDesktop.
    pub(crate) fn new(screen: ScreenRect) -> Result<WaylandInjector, String> {
        match WaylandInput::new(screen) {
            Ok(input) => Ok(WaylandInjector::Virtual(Box::new(input))),
            Err(virtual_error) => {
                let session = queqiao_portal::session()
                    .map_err(|portal_error| format!("{virtual_error}; {portal_error}"))?;
                if !session.has_input() {
                    return Err(virtual_error);
                }
                session.bring_in_devices();
                Ok(WaylandInjector::Portal {
                    session,
                    wheel: (0, 0),
                })
            }
        }
    }

    /// How input goes, for the reports.
    pub(crate) fn describe(&self) -> String {
        match self {
            WaylandInjector::Virtual(_) => "the compositor's virtual pointer and keyboard".into(),
            WaylandInjector::Portal { session, .. } => format!("the {}", session.describe()),
        }
    }

    pub(crate) fn inject(&mut self, screen: ScreenRect, event: &InputEvent) -> Result<(), String> {
        let (session, wheel) = match self {
            WaylandInjector::Virtual(input) => return input.inject(screen, event),
            WaylandInjector::Portal { session, wheel } => (session, wheel),
        };
        match *event {
            InputEvent::MouseMove { x, y } => {
                // On the stream: its logical pixels.
                let (width, height) = session.size;
                let place = |size: u32, value: u16| {
                    f64::from(size.saturating_sub(1)) * f64::from(value) / 65535.0
                };
                session.pointer_to(place(width, x), place(height, y))
            }
            InputEvent::MouseDelta { dx, dy } => session.pointer_by(f64::from(dx), f64::from(dy)),
            InputEvent::MouseButton { button, down } => {
                let code = match button {
                    MouseButton::Left => BTN_LEFT,
                    MouseButton::Right => BTN_RIGHT,
                    MouseButton::Middle => BTN_MIDDLE,
                    MouseButton::X1 => BTN_SIDE,
                    MouseButton::X2 => BTN_EXTRA,
                };
                session.button(code as i32, down)
            }
            InputEvent::Wheel { delta, horizontal } => {
                let pending = if horizontal {
                    &mut wheel.1
                } else {
                    &mut wheel.0
                };
                *pending += i32::from(delta);
                let notches = *pending / NOTCH;
                *pending -= notches * NOTCH;
                if notches == 0 {
                    return Ok(());
                }
                // Windows' positive is up or right; the portal's vertical positive is down.
                if horizontal {
                    session.scroll(false, notches)
                } else {
                    session.scroll(true, -notches)
                }
            }
            InputEvent::Key {
                scancode,
                extended,
                down,
            } => {
                let Some(code) =
                    x11_keycode(scancode, extended).and_then(|code| code.checked_sub(8))
                else {
                    return Err(format!(
                        "no key for scancode {scancode:#x}{} here",
                        if extended { " (E0)" } else { "" }
                    ));
                };
                session.key(i32::from(code), down)
            }
        }
    }
}

pub(crate) struct WaylandInput {
    connection: Connection,
    queue: EventQueue<Globals>,
    globals: Globals,
    pointer: ZwlrVirtualPointerV1,
    keyboard: ZwpVirtualKeyboardV1,
    extent: (u32, u32),
    /// Wheel movement not yet a whole notch (vertical, horizontal).
    wheel: (i32, i32),
}

impl WaylandInput {
    /// The virtual pointer and keyboard on this desktop's seat; an error where the compositor has neither protocol.
    pub(crate) fn new(screen: ScreenRect) -> Result<WaylandInput, String> {
        let connection = Connection::connect_to_env().map_err(|e| format!("Wayland: {e}"))?;
        let mut queue = connection.new_event_queue();
        let qh = queue.handle();
        connection.display().get_registry(&qh, ());
        let mut globals = Globals::default();
        queue
            .roundtrip(&mut globals)
            .map_err(|e| format!("Wayland: {e}"))?;
        queue
            .roundtrip(&mut globals)
            .map_err(|e| format!("Wayland: {e}"))?;
        let seat = globals.seat.clone().ok_or("Wayland: no seat")?;
        let (Some(pointers), Some(keyboards)) =
            (globals.pointers.clone(), globals.keyboards.clone())
        else {
            return Err(
                "this Wayland desktop has no virtual pointer and keyboard (wlroots' protocols); GNOME and KDE take \
                 input through the RemoteDesktop portal, not here yet"
                    .into(),
            );
        };
        if globals.capabilities & wl_seat::Capability::Keyboard.bits() != 0 {
            // Only for its keymap: released at once, so other programs' keys do not come here.
            let keyboard = seat.get_keyboard(&qh, ());
            queue
                .roundtrip(&mut globals)
                .map_err(|e| format!("Wayland: {e}"))?;
            if keyboard.version() >= 3 {
                keyboard.release();
            }
        }
        let (fd, size) = match globals.keymap.take() {
            Some(keymap) => keymap,
            None => keymap_file(&default_keymap()?)?,
        };
        let pointer = pointers.create_virtual_pointer(Some(&seat), &qh, ());
        let keyboard = keyboards.create_virtual_keyboard(&seat, &qh, ());
        keyboard.keymap(wl_keyboard::KeymapFormat::XkbV1.into(), fd.as_fd(), size);
        connection.flush().map_err(|e| format!("Wayland: {e}"))?;
        queue
            .roundtrip(&mut globals)
            .map_err(|e| format!("Wayland: {e}"))?;
        std::thread::sleep(SETTLE);
        Ok(WaylandInput {
            connection,
            queue,
            globals,
            pointer,
            keyboard,
            extent: layout_size(screen),
            wheel: (0, 0),
        })
    }

    pub(crate) fn inject(&mut self, screen: ScreenRect, event: &InputEvent) -> Result<(), String> {
        let time = now_ms();
        match *event {
            InputEvent::MouseMove { x, y } => {
                let (x, y) = screen.desktop_point(x, y);
                let (width, height) = self.extent;
                self.pointer.motion_absolute(
                    time,
                    (x.max(0) as u32).min(width - 1),
                    (y.max(0) as u32).min(height - 1),
                    width,
                    height,
                );
                self.pointer.frame();
            }
            InputEvent::MouseDelta { dx, dy } => {
                self.pointer.motion(time, f64::from(dx), f64::from(dy));
                self.pointer.frame();
            }
            InputEvent::MouseButton { button, down } => {
                let code = match button {
                    MouseButton::Left => BTN_LEFT,
                    MouseButton::Right => BTN_RIGHT,
                    MouseButton::Middle => BTN_MIDDLE,
                    MouseButton::X1 => BTN_SIDE,
                    MouseButton::X2 => BTN_EXTRA,
                };
                let state = if down {
                    wl_pointer::ButtonState::Pressed
                } else {
                    wl_pointer::ButtonState::Released
                };
                self.pointer.button(time, code, state);
                self.pointer.frame();
            }
            InputEvent::Wheel { delta, horizontal } => {
                let pending = if horizontal {
                    &mut self.wheel.1
                } else {
                    &mut self.wheel.0
                };
                *pending += i32::from(delta);
                let notches = *pending / NOTCH;
                *pending -= notches * NOTCH;
                if notches != 0 {
                    // Windows' positive is away from the person (up) or right; Wayland's vertical positive is down.
                    let (axis, steps) = if horizontal {
                        (wl_pointer::Axis::HorizontalScroll, notches)
                    } else {
                        (wl_pointer::Axis::VerticalScroll, -notches)
                    };
                    self.pointer.axis_source(wl_pointer::AxisSource::Wheel);
                    self.pointer
                        .axis_discrete(time, axis, f64::from(steps) * NOTCH_LENGTH, steps);
                    self.pointer.frame();
                }
            }
            InputEvent::Key {
                scancode,
                extended,
                down,
            } => {
                // X key codes are evdev codes + 8.
                let Some(code) =
                    x11_keycode(scancode, extended).and_then(|code| code.checked_sub(8))
                else {
                    return Err(format!(
                        "no key for scancode {scancode:#x}{} here",
                        if extended { " (E0)" } else { "" }
                    ));
                };
                self.keyboard.key(time, u32::from(code), u32::from(down));
            }
        }
        self.connection
            .flush()
            .map_err(|e| format!("Wayland: {e}"))?;
        // What the compositor sent meanwhile (the seat's changes), read only when there is some: never waits.
        if let Some(guard) = self.connection.prepare_read() {
            use std::os::fd::AsRawFd;
            let mut poll = libc::pollfd {
                fd: guard.connection_fd().as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            if unsafe { libc::poll(&mut poll, 1, 0) } > 0 {
                let _ = guard.read();
            }
        }
        self.queue
            .dispatch_pending(&mut self.globals)
            .map_err(|e| format!("Wayland: {e}"))?;
        Ok(())
    }
}
