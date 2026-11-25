//! The core for C (iOS's Swift calls it through include/queqiao_mobile.h). Strings are UTF-8, NUL-terminated;
//! buffers are the caller's. Every function takes the handle `qq_start` gave and checks it for null.

use std::ffi::{CStr, c_char};
use std::time::Duration;

use crate::Core;

/// Starts the core: `folder` for this device's files (the app's own), `device_name` as the device list shows it.
/// Free with `qq_stop`.
///
/// # Safety
/// `folder` and `device_name` are NUL-terminated UTF-8 strings (or null).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn qq_start(folder: *const c_char, device_name: *const c_char) -> *mut Core {
    let text = |pointer: *const c_char| {
        if pointer.is_null() {
            String::new()
        } else {
            unsafe { CStr::from_ptr(pointer) }
                .to_string_lossy()
                .into_owned()
        }
    };
    Box::into_raw(Box::new(Core::start(&text(folder), &text(device_name))))
}

/// Ends the core (its session and account) and frees it.
///
/// # Safety
/// `core` came from `qq_start` and is not used afterwards.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn qq_stop(core: *mut Core) {
    if !core.is_null() {
        drop(unsafe { Box::from_raw(core) });
    }
}

/// A command as JSON (see `Core::command`): 1 if taken, 0 if not understood.
///
/// # Safety
/// `core` came from `qq_start`; `json` is a NUL-terminated UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn qq_command(core: *mut Core, json: *const c_char) -> i32 {
    let Some(core) = (unsafe { core.as_ref() }) else {
        return 0;
    };
    if json.is_null() {
        return 0;
    }
    let json = unsafe { CStr::from_ptr(json) }.to_string_lossy();
    i32::from(core.command(&json))
}

/// The next event as JSON into `out` (NUL-terminated), waiting up to `timeout_ms`: its length, 0 if none came, or the
/// length needed (negative) when `out` is too small (the event is then lost).
///
/// # Safety
/// `core` came from `qq_start`; `out` holds `capacity` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn qq_next_event(
    core: *mut Core,
    timeout_ms: u32,
    out: *mut u8,
    capacity: usize,
) -> isize {
    let Some(core) = (unsafe { core.as_ref() }) else {
        return 0;
    };
    let Some(event) = core.next_event(Duration::from_millis(u64::from(timeout_ms))) else {
        return 0;
    };
    let bytes = event.as_bytes();
    if out.is_null() || bytes.len() + 1 > capacity {
        return -((bytes.len() + 1) as isize);
    }
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), out, bytes.len());
        *out.add(bytes.len()) = 0;
    }
    bytes.len() as isize
}

/// What `qq_next_frame` tells about a frame.
#[repr(C)]
pub struct QqFrameInfo {
    /// 0 H.264, 1 AV1, 2 H.265.
    pub codec: u32,
    /// 1 after a loss: the decoder should wait for a keyframe.
    pub after_loss: u32,
    pub time_90khz: u64,
}

/// The next frame (Annex B) into `out`, waiting up to `timeout_ms`: its length, 0 if none came, or the length needed
/// (negative) when `out` is too small (the frame is then lost: give at least 4 MB).
///
/// # Safety
/// `core` came from `qq_start`; `out` holds `capacity` bytes; `info` is null or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn qq_next_frame(
    core: *mut Core,
    timeout_ms: u32,
    out: *mut u8,
    capacity: usize,
    info: *mut QqFrameInfo,
) -> isize {
    let Some(core) = (unsafe { core.as_ref() }) else {
        return 0;
    };
    let Some(frame) = core.next_frame(Duration::from_millis(u64::from(timeout_ms))) else {
        return 0;
    };
    if out.is_null() || frame.data.len() > capacity {
        return -(frame.data.len() as isize);
    }
    unsafe {
        std::ptr::copy_nonoverlapping(frame.data.as_ptr(), out, frame.data.len());
        if let Some(info) = info.as_mut() {
            info.codec = crate::codec_number(frame.codec);
            info.after_loss = u32::from(frame.after_loss);
            info.time_90khz = frame.time_90khz;
        }
    }
    frame.data.len() as isize
}

/// The next Opus packet of the PC's sound (48 kHz) into `out`: as `qq_next_frame`; `time_48khz` if not null.
///
/// # Safety
/// `core` came from `qq_start`; `out` holds `capacity` bytes; `time_48khz` is null or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn qq_next_audio(
    core: *mut Core,
    timeout_ms: u32,
    out: *mut u8,
    capacity: usize,
    time_48khz: *mut u64,
) -> isize {
    let Some(core) = (unsafe { core.as_ref() }) else {
        return 0;
    };
    let Some(packet) = core.next_audio(Duration::from_millis(u64::from(timeout_ms))) else {
        return 0;
    };
    if out.is_null() || packet.data.len() > capacity {
        return -(packet.data.len() as isize);
    }
    unsafe {
        std::ptr::copy_nonoverlapping(packet.data.as_ptr(), out, packet.data.len());
        if let Some(time) = time_48khz.as_mut() {
            *time = packet.time_48khz;
        }
    }
    packet.data.len() as isize
}

/// The next sound packet decoded (iOS): 48 kHz stereo float samples, interleaved, into `out` (room for `capacity`
/// floats: 5760 * 2 holds the longest packet). Returns the samples per channel written, 0 when nothing came within
/// `timeout_ms` (or the packet would not decode); `time_48khz` gets the packet's time.
///
/// # Safety
/// `core` from `qq_start`; `out` holds `capacity` floats; `time_48khz` is writable or null.
#[cfg(not(target_os = "android"))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn qq_next_audio_pcm(
    core: *mut Core,
    timeout_ms: u32,
    out: *mut f32,
    capacity: usize,
    time_48khz: *mut u64,
) -> isize {
    let Some(core) = (unsafe { core.as_ref() }) else {
        return 0;
    };
    if out.is_null() {
        return 0;
    }
    let pcm = unsafe { std::slice::from_raw_parts_mut(out, capacity) };
    let Some((samples, time)) =
        core.next_audio_pcm(Duration::from_millis(u64::from(timeout_ms)), pcm)
    else {
        return 0;
    };
    if let Some(slot) = unsafe { time_48khz.as_mut() } {
        *slot = time;
    }
    samples as isize
}
