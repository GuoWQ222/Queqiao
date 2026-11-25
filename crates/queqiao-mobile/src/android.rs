//! The core for Android: the native methods of `com.queqiao.app.Core` (JNI). Frames and sound packets are copied into
//! direct ByteBuffers the app keeps (no copy on the Java side).

use std::time::Duration;

use jni::JNIEnv;
use jni::objects::{JByteBuffer, JClass, JLongArray, JString};
use jni::sys::{jboolean, jint, jlong, jstring};

use crate::Core;

fn core<'a>(handle: jlong) -> Option<&'a Core> {
    unsafe { (handle as *const Core).as_ref() }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_queqiao_app_Core_start(
    mut env: JNIEnv,
    _class: JClass,
    folder: JString,
    device_name: JString,
) -> jlong {
    let folder: String = env.get_string(&folder).map(Into::into).unwrap_or_default();
    let name: String = env
        .get_string(&device_name)
        .map(Into::into)
        .unwrap_or_default();
    Box::into_raw(Box::new(Core::start(&folder, &name))) as jlong
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_queqiao_app_Core_stop(_env: JNIEnv, _class: JClass, handle: jlong) {
    if handle != 0 {
        drop(unsafe { Box::from_raw(handle as *mut Core) });
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_queqiao_app_Core_command(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
    json: JString,
) -> jboolean {
    let Some(core) = core(handle) else {
        return 0;
    };
    let json: String = env.get_string(&json).map(Into::into).unwrap_or_default();
    u8::from(core.command(&json))
}

/// The next event as JSON, or null when none came within `timeout_ms`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_queqiao_app_Core_nextEvent(
    env: JNIEnv,
    _class: JClass,
    handle: jlong,
    timeout_ms: jint,
) -> jstring {
    let Some(core) = core(handle) else {
        return std::ptr::null_mut();
    };
    match core.next_event(Duration::from_millis(timeout_ms.max(0) as u64)) {
        Some(event) => env
            .new_string(event)
            .map(|text| text.into_raw())
            .unwrap_or(std::ptr::null_mut()),
        None => std::ptr::null_mut(),
    }
}

/// The next frame into `out` (a direct ByteBuffer): its length, 0 if none came, minus its length if `out` is too
/// small. `info`: [codec (0 H.264, 1 AV1, 2 H.265), after a loss (0/1), time in 90 kHz units].
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_queqiao_app_Core_nextFrame(
    env: JNIEnv,
    _class: JClass,
    handle: jlong,
    timeout_ms: jint,
    out: JByteBuffer,
    info: JLongArray,
) -> jint {
    let Some(core) = core(handle) else {
        return 0;
    };
    let Some(frame) = core.next_frame(Duration::from_millis(timeout_ms.max(0) as u64)) else {
        return 0;
    };
    let (Ok(address), Ok(capacity)) = (
        env.get_direct_buffer_address(&out),
        env.get_direct_buffer_capacity(&out),
    ) else {
        return 0;
    };
    if frame.data.len() > capacity {
        return -(frame.data.len() as jint);
    }
    unsafe { std::ptr::copy_nonoverlapping(frame.data.as_ptr(), address, frame.data.len()) };
    let values = [
        jlong::from(crate::codec_number(frame.codec)),
        jlong::from(frame.after_loss),
        frame.time_90khz as jlong,
    ];
    let _ = env.set_long_array_region(&info, 0, &values);
    frame.data.len() as jint
}

/// The next Opus packet into `out`: as `nextFrame`; `time[0]` its time in 48 kHz units.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_queqiao_app_Core_nextAudio(
    env: JNIEnv,
    _class: JClass,
    handle: jlong,
    timeout_ms: jint,
    out: JByteBuffer,
    time: JLongArray,
) -> jint {
    let Some(core) = core(handle) else {
        return 0;
    };
    let Some(packet) = core.next_audio(Duration::from_millis(timeout_ms.max(0) as u64)) else {
        return 0;
    };
    let (Ok(address), Ok(capacity)) = (
        env.get_direct_buffer_address(&out),
        env.get_direct_buffer_capacity(&out),
    ) else {
        return 0;
    };
    if packet.data.len() > capacity {
        return -(packet.data.len() as jint);
    }
    unsafe { std::ptr::copy_nonoverlapping(packet.data.as_ptr(), address, packet.data.len()) };
    let _ = env.set_long_array_region(&time, 0, &[packet.time_48khz as jlong]);
    packet.data.len() as jint
}
