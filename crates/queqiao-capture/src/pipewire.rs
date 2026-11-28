//! A PipeWire video stream for screen capture on Wayland (docs/LINUX.md): libpipewire-0.3.so.0 is loaded at run time
//! (as CUDA and PulseAudio are), so Queqiao starts where it is missing and builds without its headers. Only the stable
//! C API is used: a thread loop, a context on the portal's connection (its file descriptor), one input stream on the
//! portal's node. The formats it may send (EnumFormat) and the one it chose (Format) are SPA pods, written and read
//! here by hand. Each frame is copied out in the stream's `process` callback (on PipeWire's thread) and handed over
//! under a lock; the buffer goes back to PipeWire at once. Where the portal gives the pointer apart (its metadata
//! mode), each buffer may carry its shape (spa_meta_cursor with a bitmap): taken here as well, and a buffer that
//! carries only the pointer (no picture data) is no frame.

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

// SPA's types (spa/utils/type.h), parameters, format keys and values (spa/param/*.h).
const TYPE_ID: u32 = 3;
const TYPE_INT: u32 = 4;
const TYPE_RECTANGLE: u32 = 10;
const TYPE_FRACTION: u32 = 11;
const TYPE_OBJECT: u32 = 15;
const TYPE_CHOICE: u32 = 19;
const TYPE_OBJECT_FORMAT: u32 = 0x40003;
const TYPE_OBJECT_PARAM_META: u32 = 0x40005;
const PARAM_ENUM_FORMAT: u32 = 3;
const PARAM_FORMAT: u32 = 4;
const PARAM_META: u32 = 6;
const PARAM_META_TYPE: u32 = 1;
const PARAM_META_SIZE: u32 = 2;
const META_CURSOR: u32 = 5;
/// spa_chunk flags: the data is not to be used.
const CHUNK_FLAG_CORRUPTED: i32 = 1;
/// spa_meta_cursor (id, flags, position, hotspot, bitmap offset) and spa_meta_bitmap (format, size, stride, offset).
const CURSOR_META: usize = 28;
const BITMAP_META: usize = 20;
const FORMAT_MEDIA_TYPE: u32 = 1;
const FORMAT_MEDIA_SUBTYPE: u32 = 2;
const FORMAT_VIDEO_FORMAT: u32 = 0x20001;
const FORMAT_VIDEO_SIZE: u32 = 0x20003;
const FORMAT_VIDEO_FRAMERATE: u32 = 0x20004;
const FORMAT_VIDEO_MAX_FRAMERATE: u32 = 0x20005;
const MEDIA_TYPE_VIDEO: u32 = 2;
const MEDIA_SUBTYPE_RAW: u32 = 1;
const CHOICE_RANGE: u32 = 1;
const CHOICE_ENUM: u32 = 3;
/// The 32-bit formats taken (spa_video_format): the B G R order first, which the encoders take as it is.
pub const VIDEO_FORMAT_RGBX: u32 = 7;
pub const VIDEO_FORMAT_BGRX: u32 = 8;
pub const VIDEO_FORMAT_RGBA: u32 = 11;
pub const VIDEO_FORMAT_BGRA: u32 = 12;
const VIDEO_FORMAT_ARGB: u32 = 13;
const VIDEO_FORMAT_ABGR: u32 = 14;
/// The largest pointer shape taken (as the protocol's MAX_POINTER).
const POINTER_MAX: u32 = 256;
const STREAM_FLAG_AUTOCONNECT: u32 = 1 << 0;
const STREAM_FLAG_MAP_BUFFERS: u32 = 1 << 2;
const DIRECTION_INPUT: u32 = 0;
const STREAM_STATE_ERROR: c_int = -1;

#[repr(C)]
struct SpaChunk {
    offset: u32,
    size: u32,
    stride: i32,
    flags: i32,
}

#[repr(C)]
struct SpaData {
    type_: u32,
    flags: u32,
    fd: i64,
    mapoffset: u32,
    maxsize: u32,
    data: *mut c_void,
    chunk: *mut SpaChunk,
}

#[repr(C)]
struct SpaBuffer {
    n_metas: u32,
    n_datas: u32,
    metas: *mut c_void,
    datas: *mut SpaData,
}

#[repr(C)]
struct SpaMeta {
    type_: u32,
    size: u32,
    data: *mut c_void,
}

#[repr(C)]
struct PwBuffer {
    buffer: *mut SpaBuffer,
}

#[repr(C)]
struct SpaPod {
    size: u32,
    type_: u32,
}

/// pw_stream_events, version 0 (the fields every PipeWire 0.3 has).
#[repr(C)]
struct StreamEvents {
    version: u32,
    destroy: Option<unsafe extern "C" fn(*mut c_void)>,
    state_changed: Option<unsafe extern "C" fn(*mut c_void, c_int, c_int, *const c_char)>,
    control_info: Option<unsafe extern "C" fn(*mut c_void, u32, *const c_void)>,
    io_changed: Option<unsafe extern "C" fn(*mut c_void, u32, *mut c_void, u32)>,
    param_changed: Option<unsafe extern "C" fn(*mut c_void, u32, *const SpaPod)>,
    add_buffer: Option<unsafe extern "C" fn(*mut c_void, *mut PwBuffer)>,
    remove_buffer: Option<unsafe extern "C" fn(*mut c_void, *mut PwBuffer)>,
    process: Option<unsafe extern "C" fn(*mut c_void)>,
    drained: Option<unsafe extern "C" fn(*mut c_void)>,
}

/// struct spa_hook: storage PipeWire fills (a list link, callbacks, a destructor, private data); kept larger than it.
#[repr(C)]
struct SpaHook([usize; 8]);

type Opaque = *mut c_void;

struct Api {
    library: Opaque,
    init: unsafe extern "C" fn(*mut c_int, *mut *mut *mut c_char),
    thread_loop_new: unsafe extern "C" fn(*const c_char, *const c_void) -> Opaque,
    thread_loop_get_loop: unsafe extern "C" fn(Opaque) -> Opaque,
    thread_loop_start: unsafe extern "C" fn(Opaque) -> c_int,
    thread_loop_stop: unsafe extern "C" fn(Opaque),
    thread_loop_lock: unsafe extern "C" fn(Opaque),
    thread_loop_unlock: unsafe extern "C" fn(Opaque),
    thread_loop_destroy: unsafe extern "C" fn(Opaque),
    context_new: unsafe extern "C" fn(Opaque, Opaque, usize) -> Opaque,
    context_connect_fd: unsafe extern "C" fn(Opaque, c_int, Opaque, usize) -> Opaque,
    context_destroy: unsafe extern "C" fn(Opaque),
    core_disconnect: unsafe extern "C" fn(Opaque) -> c_int,
    properties_new_string: unsafe extern "C" fn(*const c_char) -> Opaque,
    stream_new: unsafe extern "C" fn(Opaque, *const c_char, Opaque) -> Opaque,
    stream_add_listener:
        unsafe extern "C" fn(Opaque, *mut SpaHook, *const StreamEvents, *mut c_void),
    stream_connect: unsafe extern "C" fn(Opaque, u32, u32, u32, *mut *const SpaPod, u32) -> c_int,
    stream_disconnect: unsafe extern "C" fn(Opaque) -> c_int,
    stream_destroy: unsafe extern "C" fn(Opaque),
    stream_dequeue_buffer: unsafe extern "C" fn(Opaque) -> *mut PwBuffer,
    stream_queue_buffer: unsafe extern "C" fn(Opaque, *mut PwBuffer) -> c_int,
    stream_update_params: unsafe extern "C" fn(Opaque, *mut *const SpaPod, u32) -> c_int,
}

/// A function's address from dlsym as the function pointer type `T` (pointer-sized).
fn function_pointer<T: Copy>(address: *mut c_void) -> T {
    assert_eq!(size_of::<T>(), size_of::<*mut c_void>());
    unsafe { std::mem::transmute_copy::<*mut c_void, T>(&address) }
}

impl Api {
    fn open() -> Result<Api, String> {
        let library = unsafe {
            libc::dlopen(
                c"libpipewire-0.3.so.0".as_ptr(),
                libc::RTLD_NOW | libc::RTLD_LOCAL,
            )
        };
        if library.is_null() {
            return Err("PipeWire (libpipewire-0.3.so.0) is not installed".into());
        }
        let get = |name: &CStr| -> Result<*mut c_void, String> {
            let address = unsafe { libc::dlsym(library, name.as_ptr()) };
            if address.is_null() {
                Err(format!("PipeWire: no {}", name.to_string_lossy()))
            } else {
                Ok(address)
            }
        };
        macro_rules! function {
            ($name:literal) => {
                function_pointer(get($name)?)
            };
        }
        Ok(Api {
            library,
            init: function!(c"pw_init"),
            thread_loop_new: function!(c"pw_thread_loop_new"),
            thread_loop_get_loop: function!(c"pw_thread_loop_get_loop"),
            thread_loop_start: function!(c"pw_thread_loop_start"),
            thread_loop_stop: function!(c"pw_thread_loop_stop"),
            thread_loop_lock: function!(c"pw_thread_loop_lock"),
            thread_loop_unlock: function!(c"pw_thread_loop_unlock"),
            thread_loop_destroy: function!(c"pw_thread_loop_destroy"),
            context_new: function!(c"pw_context_new"),
            context_connect_fd: function!(c"pw_context_connect_fd"),
            context_destroy: function!(c"pw_context_destroy"),
            core_disconnect: function!(c"pw_core_disconnect"),
            properties_new_string: function!(c"pw_properties_new_string"),
            stream_new: function!(c"pw_stream_new"),
            stream_add_listener: function!(c"pw_stream_add_listener"),
            stream_connect: function!(c"pw_stream_connect"),
            stream_disconnect: function!(c"pw_stream_disconnect"),
            stream_destroy: function!(c"pw_stream_destroy"),
            stream_dequeue_buffer: function!(c"pw_stream_dequeue_buffer"),
            stream_queue_buffer: function!(c"pw_stream_queue_buffer"),
            stream_update_params: function!(c"pw_stream_update_params"),
        })
    }
}

impl Drop for Api {
    fn drop(&mut self) {
        // Not closed: PipeWire's own threads may still run its code while the process lives.
        let _ = self.library;
    }
}

// SPA pods, written by hand: [size u32][type u32][body], each padded to 8 bytes.
fn pod(type_: u32, body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(8 + body.len() + 7);
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(&type_.to_le_bytes());
    out.extend_from_slice(body);
    while out.len() % 8 != 0 {
        out.push(0);
    }
    out
}

fn words(values: &[u32]) -> Vec<u8> {
    values
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect()
}

/// A choice of `kind` among values of `child_type`, each `values_per` words.
fn choice(kind: u32, child_type: u32, child_words: u32, values: &[u32]) -> Vec<u8> {
    let mut body = words(&[kind, 0, child_words * 4, child_type]);
    body.extend(words(values));
    pod(TYPE_CHOICE, &body)
}

fn property(key: u32, value: Vec<u8>) -> Vec<u8> {
    let mut out = words(&[key, 0]);
    out.extend(value);
    out
}

/// The size a stream asks the screen's side for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Asked {
    /// A monitor's stream: this size first (the screen's own), any other the screen's side has taken as well.
    Default(u32, u32),
    /// A virtual monitor's (Super Screen): exactly this size and refresh rate. mutter makes the virtual monitor at the
    /// size the stream agrees on, so a choice here would let it pick its own default (1280 x 720) instead.
    Exact {
        width: u32,
        height: u32,
        refresh: u32,
    },
}

/// The EnumFormat pod: raw video, these 32-bit formats, any size up to 8192 (or exactly the one asked for), up to 240
/// frames a second.
fn enum_format(asked: Asked) -> Vec<u8> {
    let mut body = words(&[TYPE_OBJECT_FORMAT, PARAM_ENUM_FORMAT]);
    body.extend(property(
        FORMAT_MEDIA_TYPE,
        pod(TYPE_ID, &words(&[MEDIA_TYPE_VIDEO])),
    ));
    body.extend(property(
        FORMAT_MEDIA_SUBTYPE,
        pod(TYPE_ID, &words(&[MEDIA_SUBTYPE_RAW])),
    ));
    body.extend(property(
        FORMAT_VIDEO_FORMAT,
        choice(
            CHOICE_ENUM,
            TYPE_ID,
            1,
            &[
                VIDEO_FORMAT_BGRX,
                VIDEO_FORMAT_BGRX,
                VIDEO_FORMAT_BGRA,
                VIDEO_FORMAT_RGBX,
                VIDEO_FORMAT_RGBA,
            ],
        ),
    ));
    match asked {
        Asked::Default(width, height) => body.extend(property(
            FORMAT_VIDEO_SIZE,
            choice(
                CHOICE_RANGE,
                TYPE_RECTANGLE,
                2,
                &[width.max(1), height.max(1), 1, 1, 8192, 8192],
            ),
        )),
        Asked::Exact { width, height, .. } => body.extend(property(
            FORMAT_VIDEO_SIZE,
            pod(TYPE_RECTANGLE, &words(&[width.max(1), height.max(1)])),
        )),
    }
    body.extend(property(
        FORMAT_VIDEO_FRAMERATE,
        choice(CHOICE_RANGE, TYPE_FRACTION, 2, &[60, 1, 0, 1, 240, 1]),
    ));
    // mutter gives a virtual monitor the refresh rate of the stream's highest frame rate.
    if let Asked::Exact { refresh, .. } = asked {
        body.extend(property(
            FORMAT_VIDEO_MAX_FRAMERATE,
            pod(TYPE_FRACTION, &words(&[refresh.clamp(1, 240), 1])),
        ));
    }
    pod(TYPE_OBJECT, &body)
}

/// The Meta pod asking for the pointer's metadata: room for a bitmap of 64 x 64 by default, up to 1024 x 1024. The
/// screen's side offers one size (mutter: room for 384 x 384), which must fall in this range or no pointer meta comes;
/// shapes larger than POINTER_MAX are passed over when read.
fn cursor_meta_param() -> Vec<u8> {
    let size = |side: u32| (CURSOR_META + BITMAP_META) as u32 + side * side * 4;
    let mut body = words(&[TYPE_OBJECT_PARAM_META, PARAM_META]);
    body.extend(property(
        PARAM_META_TYPE,
        pod(TYPE_ID, &words(&[META_CURSOR])),
    ));
    body.extend(property(
        PARAM_META_SIZE,
        choice(CHOICE_RANGE, TYPE_INT, 1, &[size(64), size(1), size(1024)]),
    ));
    pod(TYPE_OBJECT, &body)
}

fn word(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

/// The first value of a pod (a choice's default) as words: (its type, its body).
fn first_value(value_type: u32, body: &[u8]) -> Option<(u32, &[u8])> {
    if value_type == TYPE_CHOICE {
        let child_size = word(body, 8)? as usize;
        let child_type = word(body, 12)?;
        Some((child_type, body.get(16..16 + child_size)?))
    } else {
        Some((value_type, body))
    }
}

/// The video format and size of a Format pod (`pod`: its header and body).
fn parse_format(pod: &[u8]) -> Option<(u32, u32, u32)> {
    let size = word(pod, 0)? as usize;
    if word(pod, 4)? != TYPE_OBJECT || pod.len() < 8 + size {
        return None;
    }
    let body = &pod[8..8 + size];
    let (mut format, mut extent) = (None, None);
    let mut at = 8;
    while at + 16 <= body.len() {
        let key = word(body, at)?;
        let value_size = word(body, at + 8)? as usize;
        let value_type = word(body, at + 12)?;
        let value = body.get(at + 16..at + 16 + value_size)?;
        match (key, first_value(value_type, value)) {
            (FORMAT_VIDEO_FORMAT, Some((TYPE_ID, value))) => format = word(value, 0),
            (FORMAT_VIDEO_SIZE, Some((TYPE_RECTANGLE, value))) => {
                extent = Some((word(value, 0)?, word(value, 4)?))
            }
            _ => {}
        }
        at += 16 + value_size.div_ceil(8) * 8;
    }
    let (width, height) = extent?;
    Some((format?, width, height))
}

/// The latest frame, copied out of PipeWire's buffer.
#[derive(Default)]
pub struct Frame {
    pub pixels: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub stride: usize,
    /// spa_video_format: BGRx, BGRA, RGBx or RGBA.
    pub format: u32,
    /// Counts the frames that came.
    pub sequence: u64,
    /// When it came (`qpc_now`).
    pub arrived_qpc: i64,
}

/// The pointer's shape from the metadata: B, G, R, A rows (premultiplied, as the compositors give it).
#[derive(Clone, PartialEq, Eq)]
pub struct PointerImage {
    pub width: u32,
    pub height: u32,
    pub hot_x: i32,
    pub hot_y: i32,
    pub bgra: Vec<u8>,
}

/// What the metadata told of the pointer since it was last taken.
#[derive(Default)]
struct Pointer {
    shape: Option<PointerImage>,
    shape_new: bool,
    visible: Option<bool>,
    visible_new: bool,
}

/// What the stream's callbacks (on PipeWire's thread) share with the capture's thread.
struct Shared {
    api: *const Api,
    stream: Opaque,
    /// The pointer apart from the picture (the portal's metadata mode): its meta asked for and read.
    pointer_apart: bool,
    pointer: Mutex<Pointer>,
    /// The format PipeWire chose: (spa_video_format, width, height).
    format: Mutex<Option<(u32, u32, u32)>>,
    state: AtomicI32,
    error: Mutex<Option<String>>,
    frame: Mutex<Frame>,
    arrived: Condvar,
}

unsafe extern "C" fn on_state(data: *mut c_void, _old: c_int, state: c_int, error: *const c_char) {
    let shared = unsafe { &*data.cast::<Shared>() };
    shared.state.store(state, Ordering::Relaxed);
    if state == STREAM_STATE_ERROR {
        let text = if error.is_null() {
            "the stream failed".into()
        } else {
            unsafe { CStr::from_ptr(error) }
                .to_string_lossy()
                .into_owned()
        };
        if let Ok(mut error) = shared.error.lock() {
            *error = Some(text);
        }
        // Under the frame's lock, so a waiter cannot miss it.
        let _frame = shared.frame.lock();
        shared.arrived.notify_all();
    }
}

unsafe extern "C" fn on_param(data: *mut c_void, id: u32, param: *const SpaPod) {
    let shared = unsafe { &*data.cast::<Shared>() };
    if id != PARAM_FORMAT || param.is_null() {
        return;
    }
    let size = unsafe { (*param).size } as usize;
    let bytes = unsafe { std::slice::from_raw_parts(param.cast::<u8>(), 8 + size) };
    if let Ok(mut format) = shared.format.lock() {
        *format = parse_format(bytes);
    }
    // With the format agreed, the pointer's metadata is asked for (as PipeWire's own examples do it).
    if shared.pointer_apart {
        let api = unsafe { &*shared.api };
        let meta = cursor_meta_param();
        let mut params = [meta.as_ptr().cast::<SpaPod>()];
        unsafe { (api.stream_update_params)(shared.stream, params.as_mut_ptr(), 1) };
    }
    // Under the frame's lock, so a waiter for the size cannot miss it.
    let _frame = shared.frame.lock();
    shared.arrived.notify_all();
}

unsafe extern "C" fn on_process(data: *mut c_void) {
    let shared = unsafe { &*data.cast::<Shared>() };
    let api = unsafe { &*shared.api };
    let buffer = unsafe { (api.stream_dequeue_buffer)(shared.stream) };
    if buffer.is_null() {
        return;
    }
    let spa = unsafe { &*(*buffer).buffer };
    if shared.pointer_apart {
        read_pointer(shared, spa);
    }
    let format = shared.format.lock().ok().and_then(|format| *format);
    if spa.n_datas > 0
        && let Some((format, width, height)) = format
    {
        let data = unsafe { &*spa.datas };
        let chunk = unsafe { &*data.chunk };
        // A buffer without memory here (DMA-BUF, not asked for), an empty one (nothing changed, or only the pointer
        // moved) or one marked not to be used is passed over.
        if !data.data.is_null() && chunk.size > 0 && chunk.flags & CHUNK_FLAG_CORRUPTED == 0 {
            let stride = if chunk.stride > 0 {
                chunk.stride as usize
            } else {
                width as usize * 4
            };
            let start = chunk.offset as usize % data.maxsize.max(1) as usize;
            let length = (chunk.size as usize).min(data.maxsize as usize - start);
            let bytes =
                unsafe { std::slice::from_raw_parts(data.data.cast::<u8>().add(start), length) };
            if let Ok(mut frame) = shared.frame.lock() {
                frame.pixels.clear();
                frame.pixels.extend_from_slice(bytes);
                frame.width = width;
                frame.height = height;
                frame.stride = stride;
                frame.format = format;
                frame.sequence += 1;
                frame.arrived_qpc = crate::qpc_now();
                shared.arrived.notify_all();
            }
        }
    }
    unsafe { (api.stream_queue_buffer)(shared.stream, buffer) };
}

/// The pointer's metadata in a buffer: a new shape (its bitmap), or hidden (a bitmap of no size). A cursor id of 0, or
/// no bitmap, tells nothing new of the shape.
fn read_pointer(shared: &Shared, spa: &SpaBuffer) {
    if spa.metas.is_null() {
        return;
    }
    let metas =
        unsafe { std::slice::from_raw_parts(spa.metas.cast::<SpaMeta>(), spa.n_metas as usize) };
    for meta in metas {
        if meta.type_ != META_CURSOR || meta.data.is_null() || (meta.size as usize) < CURSOR_META {
            continue;
        }
        let bytes =
            unsafe { std::slice::from_raw_parts(meta.data.cast::<u8>(), meta.size as usize) };
        let int = |at: usize| word(bytes, at).map(|value| value as i32);
        let (Some(id), Some(hot_x), Some(hot_y), Some(bitmap)) =
            (word(bytes, 0), int(16), int(20), word(bytes, 24))
        else {
            continue;
        };
        let bitmap = bitmap as usize;
        if id == 0 || bitmap < CURSOR_META || bitmap + BITMAP_META > bytes.len() {
            continue;
        }
        let (Some(format), Some(width), Some(height), Some(stride), Some(offset)) = (
            word(bytes, bitmap),
            word(bytes, bitmap + 4),
            word(bytes, bitmap + 8),
            int(bitmap + 12),
            word(bytes, bitmap + 16),
        ) else {
            continue;
        };
        let Ok(mut pointer) = shared.pointer.lock() else {
            return;
        };
        if width == 0 || height == 0 {
            if pointer.visible != Some(false) {
                pointer.visible = Some(false);
                pointer.visible_new = true;
            }
            continue;
        }
        let stride = if stride > 0 {
            stride as usize
        } else {
            width as usize * 4
        };
        let start = bitmap + offset as usize;
        let end = start + stride * (height as usize - 1) + width as usize * 4;
        let order = match format {
            VIDEO_FORMAT_RGBA => [2, 1, 0, 3],
            VIDEO_FORMAT_BGRA => [0, 1, 2, 3],
            VIDEO_FORMAT_ARGB => [3, 2, 1, 0],
            VIDEO_FORMAT_ABGR => [1, 2, 3, 0],
            _ => continue,
        };
        if width > POINTER_MAX || height > POINTER_MAX || end > bytes.len() {
            continue;
        }
        let mut bgra = Vec::with_capacity(width as usize * height as usize * 4);
        for row in 0..height as usize {
            let line = &bytes[start + row * stride..start + row * stride + width as usize * 4];
            for pixel in line.as_chunks::<4>().0 {
                bgra.extend(order.map(|index| pixel[index]));
            }
        }
        let image = PointerImage {
            width,
            height,
            hot_x,
            hot_y,
            bgra,
        };
        if pointer.shape.as_ref() != Some(&image) {
            pointer.shape = Some(image);
            pointer.shape_new = true;
        }
        if pointer.visible != Some(true) {
            pointer.visible = Some(true);
            pointer.visible_new = true;
        }
    }
}

static EVENTS: StreamEvents = StreamEvents {
    version: 0,
    destroy: None,
    state_changed: Some(on_state),
    control_info: None,
    io_changed: None,
    param_changed: Some(on_param),
    add_buffer: None,
    remove_buffer: None,
    process: Some(on_process),
    drained: None,
};

/// A running stream of the portal's node.
pub struct Stream {
    api: Box<Api>,
    thread_loop: Opaque,
    context: Opaque,
    core: Opaque,
    shared: Box<Shared>,
    _hook: Box<SpaHook>,
}

// The raw pointers are PipeWire's, used under its thread loop's lock.
unsafe impl Send for Stream {}

impl Stream {
    /// Connects to the portal's PipeWire (`fd`, from OpenPipeWireRemote) and streams `node` at the size `asked`.
    /// `pointer_apart`: the portal gives the pointer as metadata (its shape is read; the pictures have none).
    pub fn open(
        fd: std::os::fd::OwnedFd,
        node: u32,
        asked: Asked,
        pointer_apart: bool,
    ) -> Result<Stream, String> {
        use std::os::fd::IntoRawFd;
        let api = Box::new(Api::open()?);
        unsafe { (api.init)(std::ptr::null_mut(), std::ptr::null_mut()) };
        let thread_loop =
            unsafe { (api.thread_loop_new)(c"queqiao-pipewire".as_ptr(), std::ptr::null()) };
        if thread_loop.is_null() {
            return Err("PipeWire: no thread loop".into());
        }
        let context = unsafe {
            (api.context_new)(
                (api.thread_loop_get_loop)(thread_loop),
                std::ptr::null_mut(),
                0,
            )
        };
        if context.is_null() {
            unsafe { (api.thread_loop_destroy)(thread_loop) };
            return Err("PipeWire: no context".into());
        }
        let mut shared = Box::new(Shared {
            api: &*api,
            stream: std::ptr::null_mut(),
            pointer_apart,
            pointer: Mutex::new(Pointer::default()),
            format: Mutex::new(None),
            state: AtomicI32::new(0),
            error: Mutex::new(None),
            frame: Mutex::new(Frame::default()),
            arrived: Condvar::new(),
        });
        let mut hook = Box::new(SpaHook([0; 8]));
        let made = (|| {
            if unsafe { (api.thread_loop_start)(thread_loop) } < 0 {
                return Err("PipeWire: the thread loop did not start".to_string());
            }
            unsafe { (api.thread_loop_lock)(thread_loop) };
            let result = (|| {
                // The portal's connection: PipeWire takes the descriptor over.
                let core = unsafe {
                    (api.context_connect_fd)(context, fd.into_raw_fd(), std::ptr::null_mut(), 0)
                };
                if core.is_null() {
                    return Err("PipeWire: the portal's connection was refused".to_string());
                }
                let properties =
                    CString::new("media.type=Video media.category=Capture media.role=Screen")
                        .unwrap();
                let stream = unsafe {
                    (api.stream_new)(
                        core,
                        c"queqiao-screen".as_ptr(),
                        (api.properties_new_string)(properties.as_ptr()),
                    )
                };
                if stream.is_null() {
                    return Err("PipeWire: no stream".to_string());
                }
                shared.stream = stream;
                unsafe {
                    (api.stream_add_listener)(
                        stream,
                        &mut *hook,
                        &EVENTS,
                        (&mut *shared as *mut Shared).cast(),
                    )
                };
                let formats = enum_format(asked);
                let mut params = [formats.as_ptr().cast::<SpaPod>()];
                let connected = unsafe {
                    (api.stream_connect)(
                        stream,
                        DIRECTION_INPUT,
                        node,
                        STREAM_FLAG_AUTOCONNECT | STREAM_FLAG_MAP_BUFFERS,
                        params.as_mut_ptr(),
                        1,
                    )
                };
                if connected < 0 {
                    return Err(format!(
                        "PipeWire: connecting the stream failed ({connected})"
                    ));
                }
                Ok(core)
            })();
            unsafe { (api.thread_loop_unlock)(thread_loop) };
            result
        })();
        match made {
            Ok(core) => Ok(Stream {
                api,
                thread_loop,
                context,
                core,
                shared,
                _hook: hook,
            }),
            Err(error) => {
                unsafe {
                    (api.thread_loop_stop)(thread_loop);
                    (api.context_destroy)(context);
                    (api.thread_loop_destroy)(thread_loop);
                }
                Err(error)
            }
        }
    }

    /// Waits up to `wait` for the format PipeWire and the screen's side agree on: the size of the pictures to come
    /// (none may come for a while: a still screen sends none on some desktops).
    pub fn size(&self, wait: Duration) -> Result<Option<(u32, u32)>, String> {
        let until = Instant::now() + wait;
        let mut frame = self
            .shared
            .frame
            .lock()
            .map_err(|_| "PipeWire: poisoned".to_string())?;
        loop {
            if let Some(error) = self
                .shared
                .error
                .lock()
                .ok()
                .and_then(|error| error.clone())
            {
                return Err(format!("PipeWire: {error}"));
            }
            if let Some((_, width, height)) =
                self.shared.format.lock().ok().and_then(|format| *format)
            {
                return Ok(Some((width, height)));
            }
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(None);
            }
            frame = self
                .shared
                .arrived
                .wait_timeout(frame, left)
                .map_err(|_| "PipeWire: poisoned".to_string())?
                .0;
        }
    }

    /// A new pointer shape from the metadata since it was last taken.
    pub fn take_pointer_shape(&self) -> Option<PointerImage> {
        let mut pointer = self.shared.pointer.lock().ok()?;
        if !pointer.shape_new {
            return None;
        }
        pointer.shape_new = false;
        pointer.shape.clone()
    }

    /// The pointer's visibility from the metadata, when it changed since it was last taken.
    pub fn take_pointer_visible(&self) -> Option<bool> {
        let mut pointer = self.shared.pointer.lock().ok()?;
        if !pointer.visible_new {
            return None;
        }
        pointer.visible_new = false;
        pointer.visible
    }

    /// Waits up to `wait` for a frame newer than `after` (a sequence number) and hands it to `take`.
    pub fn next<T>(
        &self,
        after: u64,
        wait: Duration,
        take: impl FnOnce(&Frame) -> T,
    ) -> Result<Option<T>, String> {
        let until = Instant::now() + wait;
        let mut frame = self
            .shared
            .frame
            .lock()
            .map_err(|_| "PipeWire: poisoned".to_string())?;
        loop {
            if let Some(error) = self
                .shared
                .error
                .lock()
                .ok()
                .and_then(|error| error.clone())
            {
                return Err(format!("PipeWire: {error}"));
            }
            if frame.sequence > after {
                return Ok(Some(take(&frame)));
            }
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(None);
            }
            frame = self
                .shared
                .arrived
                .wait_timeout(frame, left)
                .map_err(|_| "PipeWire: poisoned".to_string())?
                .0;
        }
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        let api = &self.api;
        unsafe {
            (api.thread_loop_lock)(self.thread_loop);
            if !self.shared.stream.is_null() {
                (api.stream_disconnect)(self.shared.stream);
                (api.stream_destroy)(self.shared.stream);
            }
            (api.core_disconnect)(self.core);
            (api.thread_loop_unlock)(self.thread_loop);
            (api.thread_loop_stop)(self.thread_loop);
            (api.context_destroy)(self.context);
            (api.thread_loop_destroy)(self.thread_loop);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_format_pod_reads_back() {
        // A Format as PipeWire fixes it: BGRx, 1920x1080 (plain values), and the framerate as a choice.
        let mut body = words(&[TYPE_OBJECT_FORMAT, PARAM_FORMAT]);
        body.extend(property(
            FORMAT_MEDIA_TYPE,
            pod(TYPE_ID, &words(&[MEDIA_TYPE_VIDEO])),
        ));
        body.extend(property(
            FORMAT_VIDEO_FORMAT,
            pod(TYPE_ID, &words(&[VIDEO_FORMAT_BGRX])),
        ));
        body.extend(property(
            FORMAT_VIDEO_SIZE,
            pod(TYPE_RECTANGLE, &words(&[1920, 1080])),
        ));
        body.extend(property(
            FORMAT_VIDEO_FRAMERATE,
            choice(CHOICE_RANGE, TYPE_FRACTION, 2, &[60, 1, 0, 1, 240, 1]),
        ));
        assert_eq!(
            parse_format(&pod(TYPE_OBJECT, &body)),
            Some((VIDEO_FORMAT_BGRX, 1920, 1080))
        );
        // Our EnumFormat reads back with the defaults first.
        assert_eq!(
            parse_format(&enum_format(Asked::Default(1280, 720))),
            Some((VIDEO_FORMAT_BGRX, 1280, 720))
        );
    }

    /// The properties of an object pod: (key, value type, value body).
    fn properties(pod: &[u8]) -> Vec<(u32, u32, Vec<u8>)> {
        let size = word(pod, 0).unwrap() as usize;
        let body = &pod[8..8 + size];
        let mut found = Vec::new();
        let mut at = 8;
        while at + 16 <= body.len() {
            let key = word(body, at).unwrap();
            let value_size = word(body, at + 8).unwrap() as usize;
            let value_type = word(body, at + 12).unwrap();
            found.push((
                key,
                value_type,
                body[at + 16..at + 16 + value_size].to_vec(),
            ));
            at += 16 + value_size.div_ceil(8) * 8;
        }
        found
    }

    #[test]
    fn a_virtual_monitor_is_asked_for_at_exactly_its_size() {
        let format = enum_format(Asked::Exact {
            width: 2560,
            height: 1440,
            refresh: 60,
        });
        assert_eq!(parse_format(&format), Some((VIDEO_FORMAT_BGRX, 2560, 1440)));
        let properties = properties(&format);
        // The size a plain rectangle (no choice for the screen's side to fixate otherwise), the highest frame rate
        // the refresh rate.
        let size = properties
            .iter()
            .find(|(key, _, _)| *key == FORMAT_VIDEO_SIZE)
            .unwrap();
        assert_eq!(
            (size.1, size.2.clone()),
            (TYPE_RECTANGLE, words(&[2560, 1440]))
        );
        let max = properties
            .iter()
            .find(|(key, _, _)| *key == FORMAT_VIDEO_MAX_FRAMERATE)
            .unwrap();
        assert_eq!((max.1, max.2.clone()), (TYPE_FRACTION, words(&[60, 1])));
        // A monitor's stream offers a range and says nothing of the highest frame rate.
        let monitor = properties_of_default();
        assert!(
            monitor
                .iter()
                .any(|(key, kind, _)| *key == FORMAT_VIDEO_SIZE && *kind == TYPE_CHOICE)
        );
        assert!(
            monitor
                .iter()
                .all(|(key, _, _)| *key != FORMAT_VIDEO_MAX_FRAMERATE)
        );
    }

    fn properties_of_default() -> Vec<(u32, u32, Vec<u8>)> {
        properties(&enum_format(Asked::Default(1920, 1080)))
    }
}
