//! Input through the kernel (uinput), for the Queqiao service's desktop helper (docs/LINUX.md): a pointer with
//! absolute axes and a keyboard that the compositor, the sign-in and lock screens and the text console all take as
//! devices of the PC's own. `/dev/uinput` lets whoever opens it type as anyone, so only that helper (root) opens it,
//! and only for the viewer of a running session (docs/LINUX.md, Security).
//!
//! The pointer is made as QEMU's USB tablet is (absolute X and Y, three buttons and two more, the wheels): every
//! compositor that runs in a virtual machine takes such a device, and maps its range onto the screens.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;

use queqiao_core::protocol::{InputEvent, MouseButton};

use crate::scancode::evdev_from_scancode;

const EV_SYN: u16 = 0x00;
const EV_KEY: u16 = 0x01;
const EV_REL: u16 = 0x02;
const EV_ABS: u16 = 0x03;
const SYN_REPORT: u16 = 0;
const REL_HWHEEL: u16 = 0x06;
const REL_WHEEL: u16 = 0x08;
const REL_WHEEL_HI_RES: u16 = 0x0b;
const REL_HWHEEL_HI_RES: u16 = 0x0c;
const ABS_X: u16 = 0x00;
const ABS_Y: u16 = 0x01;
const BTN_LEFT: u16 = 0x110;
const BTN_RIGHT: u16 = 0x111;
const BTN_MIDDLE: u16 = 0x112;
const BTN_SIDE: u16 = 0x113;
const BTN_EXTRA: u16 = 0x114;
/// The keyboard's keys: KEY_ESC (1) to KEY_MICMUTE (248), the range the scancode table maps into.
const LAST_KEY: u16 = 248;
const BUS_VIRTUAL: u16 = 0x06;
/// The pointer's range on each axis: the protocol's own (0 = left/top, 65535 = right/bottom).
const ABS_MAX: i32 = 65535;

const fn io(nr: u64) -> u64 {
    (0x55 << 8) | nr
}

const fn iow(nr: u64, size: usize) -> u64 {
    (1 << 30) | ((size as u64) << 16) | (0x55 << 8) | nr
}

#[repr(C)]
struct Setup {
    bustype: u16,
    vendor: u16,
    product: u16,
    version: u16,
    name: [u8; 80],
    ff_effects_max: u32,
}

#[repr(C)]
struct AbsSetup {
    code: u16,
    value: i32,
    minimum: i32,
    maximum: i32,
    fuzz: i32,
    flat: i32,
    resolution: i32,
}

#[repr(C)]
struct Event {
    seconds: i64,
    microseconds: i64,
    kind: u16,
    code: u16,
    value: i32,
}

const UI_DEV_CREATE: u64 = io(1);
const UI_DEV_DESTROY: u64 = io(2);
const UI_DEV_SETUP: u64 = iow(3, size_of::<Setup>());
const UI_ABS_SETUP: u64 = iow(4, size_of::<AbsSetup>());
const UI_SET_EVBIT: u64 = iow(100, size_of::<libc::c_int>());
const UI_SET_KEYBIT: u64 = iow(101, size_of::<libc::c_int>());
const UI_SET_RELBIT: u64 = iow(102, size_of::<libc::c_int>());
const UI_SET_ABSBIT: u64 = iow(103, size_of::<libc::c_int>());

/// One virtual device, removed when dropped.
struct Device {
    file: File,
}

impl Device {
    fn create(
        name: &str,
        product: u16,
        configure: impl FnOnce(&File) -> std::io::Result<()>,
    ) -> Result<Device, String> {
        let file = OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open("/dev/uinput")
            .map_err(|e| format!("/dev/uinput: {e}"))?;
        configure(&file).map_err(|e| format!("{name}: {e}"))?;
        let mut setup = Setup {
            bustype: BUS_VIRTUAL,
            // Not a real vendor's number: 0x5171 ("Qq").
            vendor: 0x5171,
            product,
            version: 1,
            name: [0; 80],
            ff_effects_max: 0,
        };
        setup.name[..name.len().min(79)].copy_from_slice(&name.as_bytes()[..name.len().min(79)]);
        call(
            &file,
            UI_DEV_SETUP,
            &mut setup as *mut Setup as libc::c_ulong,
        )
        .and_then(|()| call(&file, UI_DEV_CREATE, 0))
        .map_err(|e| format!("{name}: {e}"))?;
        Ok(Device { file })
    }

    fn send(&mut self, events: &[(u16, u16, i32)]) -> Result<(), String> {
        let mut bytes = Vec::with_capacity((events.len() + 1) * size_of::<Event>());
        for &(kind, code, value) in events.iter().chain([(EV_SYN, SYN_REPORT, 0)].iter()) {
            let event = Event {
                seconds: 0,
                microseconds: 0,
                kind,
                code,
                value,
            };
            bytes.extend_from_slice(unsafe {
                std::slice::from_raw_parts(
                    (&event as *const Event).cast::<u8>(),
                    size_of::<Event>(),
                )
            });
        }
        self.file
            .write_all(&bytes)
            .map_err(|e| format!("uinput: {e}"))
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        let _ = call(&self.file, UI_DEV_DESTROY, 0);
    }
}

fn call(file: &File, request: u64, argument: libc::c_ulong) -> std::io::Result<()> {
    if unsafe { libc::ioctl(file.as_raw_fd(), request as _, argument) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

fn set_bits(file: &File, kind: u64, codes: impl IntoIterator<Item = u16>) -> std::io::Result<()> {
    for code in codes {
        call(file, kind, libc::c_ulong::from(code))?;
    }
    Ok(())
}

/// The helper's pointer and keyboard (made at once; they stay until dropped).
pub struct UinputInjector {
    pointer: Device,
    keyboard: Device,
    /// The wheels' movement not yet sent as whole notches (the protocol's units: 120 a notch).
    wheel: i32,
    hwheel: i32,
}

impl UinputInjector {
    pub fn new() -> Result<UinputInjector, String> {
        let pointer = Device::create("Queqiao pointer", 1, |file| {
            set_bits(file, UI_SET_EVBIT, [EV_KEY, EV_ABS, EV_REL])?;
            set_bits(
                file,
                UI_SET_KEYBIT,
                [BTN_LEFT, BTN_RIGHT, BTN_MIDDLE, BTN_SIDE, BTN_EXTRA],
            )?;
            set_bits(
                file,
                UI_SET_RELBIT,
                [REL_WHEEL, REL_HWHEEL, REL_WHEEL_HI_RES, REL_HWHEEL_HI_RES],
            )?;
            set_bits(file, UI_SET_ABSBIT, [ABS_X, ABS_Y])?;
            for code in [ABS_X, ABS_Y] {
                let mut axis = AbsSetup {
                    code,
                    value: 0,
                    minimum: 0,
                    maximum: ABS_MAX,
                    fuzz: 0,
                    flat: 0,
                    resolution: 0,
                };
                call(
                    file,
                    UI_ABS_SETUP,
                    &mut axis as *mut AbsSetup as libc::c_ulong,
                )?;
            }
            Ok(())
        })?;
        let keyboard = Device::create("Queqiao keyboard", 2, |file| {
            set_bits(file, UI_SET_EVBIT, [EV_KEY])?;
            set_bits(file, UI_SET_KEYBIT, 1..=LAST_KEY)
        })?;
        Ok(UinputInjector {
            pointer,
            keyboard,
            wheel: 0,
            hwheel: 0,
        })
    }

    pub fn inject(&mut self, event: &InputEvent) -> Result<(), String> {
        match *event {
            InputEvent::MouseMove { x, y } => self
                .pointer
                .send(&[(EV_ABS, ABS_X, i32::from(x)), (EV_ABS, ABS_Y, i32::from(y))]),
            InputEvent::MouseButton { button, down } => {
                let code = match button {
                    MouseButton::Left => BTN_LEFT,
                    MouseButton::Right => BTN_RIGHT,
                    MouseButton::Middle => BTN_MIDDLE,
                    MouseButton::X1 => BTN_SIDE,
                    MouseButton::X2 => BTN_EXTRA,
                };
                self.pointer.send(&[(EV_KEY, code, i32::from(down))])
            }
            InputEvent::Wheel { delta, horizontal } => {
                // Both: the high-resolution value (also 120 a notch) and whole notches for programs that read only
                // those. Positive is away from the person (up) or to the right, in both.
                let (fine, coarse, rest) = if horizontal {
                    (REL_HWHEEL_HI_RES, REL_HWHEEL, &mut self.hwheel)
                } else {
                    (REL_WHEEL_HI_RES, REL_WHEEL, &mut self.wheel)
                };
                *rest += i32::from(delta);
                let notches = *rest / 120;
                *rest -= notches * 120;
                let mut events = vec![(EV_REL, fine, i32::from(delta))];
                if notches != 0 {
                    events.push((EV_REL, coarse, notches));
                }
                self.pointer.send(&events)
            }
            InputEvent::Key {
                scancode,
                extended,
                down,
            } => {
                let code = evdev_from_scancode(scancode, extended)
                    .filter(|code| (1..=LAST_KEY).contains(code))
                    .ok_or_else(|| format!("no key for scancode {scancode:#x}"))?;
                self.keyboard.send(&[(EV_KEY, code, i32::from(down))])
            }
            InputEvent::MouseDelta { .. } => {
                Err("relative movement (game mouse) is not for the sign-in or lock screen".into())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_kernel_structures_have_the_kernels_sizes() {
        // linux/uinput.h and linux/input.h on x86_64.
        assert_eq!(size_of::<Setup>(), 92);
        assert_eq!(size_of::<AbsSetup>(), 28);
        assert_eq!(size_of::<Event>(), 24);
        assert_eq!(UI_DEV_SETUP, 0x405c5503);
        assert_eq!(UI_ABS_SETUP, 0x401c5504);
        assert_eq!(UI_SET_EVBIT, 0x40045564);
    }
}
