//! PulseAudio (and PipeWire's PulseAudio server) at 48 kHz stereo float, with the API of wasapi.rs (docs/LINUX.md).
//! libpulse-simple is loaded at run time (no sound server: no sound, and the programs still start). Its reads and
//! writes block, so each stream has a thread of its own doing them against a ring: `read` drains what came, `write`
//! queues up to 100 ms, and `padding` counts what is queued here and in the server, as WASAPI's calls do. Devices are
//! named as `pactl list short sinks|sources` lists them; the default output's monitor is what loopback captures.

use std::collections::VecDeque;
use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use crate::{CHANNELS, FRAME, SAMPLE_RATE, VirtualMicrophone};

/// No COM on Linux: kept so the shared code reads the same.
pub struct Com;

impl Com {
    pub fn init() -> Com {
        Com
    }
}

/// pa_sample_spec.
#[repr(C)]
struct SampleSpec {
    format: c_int,
    rate: u32,
    channels: u8,
}

/// pa_buffer_attr (u32::MAX: the server's default).
#[repr(C)]
struct BufferAttr {
    maxlength: u32,
    tlength: u32,
    prebuf: u32,
    minreq: u32,
    fragsize: u32,
}

const SAMPLE_FLOAT32LE: c_int = 5;
const DIRECTION_PLAYBACK: c_int = 1;
const DIRECTION_RECORD: c_int = 2;
/// What a stream keeps queued here at most: 100 ms, as WASAPI's buffer.
const BUFFER_FRAMES: u32 = SAMPLE_RATE / 10;
/// The server's own playback buffer: 30 ms, so the sound is not late.
const SERVER_PLAYBACK_MS: u32 = 30;
/// Captured sound kept for `read` at most (1 s): older is dropped when nobody reads.
const MOST_KEPT: usize = SAMPLE_RATE as usize * CHANNELS;

type New = unsafe extern "C" fn(
    *const c_char,
    *const c_char,
    c_int,
    *const c_char,
    *const c_char,
    *const SampleSpec,
    *const c_void,
    *const BufferAttr,
    *mut c_int,
) -> *mut c_void;
type Read = unsafe extern "C" fn(*mut c_void, *mut c_void, usize, *mut c_int) -> c_int;
type Write = unsafe extern "C" fn(*mut c_void, *const c_void, usize, *mut c_int) -> c_int;
type Latency = unsafe extern "C" fn(*mut c_void, *mut c_int) -> u64;
type Free = unsafe extern "C" fn(*mut c_void);
type Strerror = unsafe extern "C" fn(c_int) -> *const c_char;

struct Api {
    new: New,
    read: Read,
    write: Write,
    latency: Latency,
    free: Free,
    strerror: Strerror,
}

/// libpulse-simple, loaded once and kept.
fn api() -> Result<&'static Api, String> {
    static API: OnceLock<Result<Api, String>> = OnceLock::new();
    API.get_or_init(|| {
        let handle = unsafe {
            libc::dlopen(
                c"libpulse-simple.so.0".as_ptr(),
                libc::RTLD_NOW | libc::RTLD_LOCAL,
            )
        };
        if handle.is_null() {
            return Err("no PulseAudio library (libpulse-simple.so.0)".into());
        }
        let function = |name: &CStr| {
            let address = unsafe { libc::dlsym(handle, name.as_ptr()) };
            if address.is_null() {
                Err(format!("{} missing", name.to_string_lossy()))
            } else {
                Ok(address)
            }
        };
        // dlsym on the handle also finds pa_strerror in libpulse, which libpulse-simple links.
        unsafe {
            Ok(Api {
                new: std::mem::transmute::<*mut c_void, New>(function(c"pa_simple_new")?),
                read: std::mem::transmute::<*mut c_void, Read>(function(c"pa_simple_read")?),
                write: std::mem::transmute::<*mut c_void, Write>(function(c"pa_simple_write")?),
                latency: std::mem::transmute::<*mut c_void, Latency>(function(
                    c"pa_simple_get_latency",
                )?),
                free: std::mem::transmute::<*mut c_void, Free>(function(c"pa_simple_free")?),
                strerror: std::mem::transmute::<*mut c_void, Strerror>(function(c"pa_strerror")?),
            })
        }
    })
    .as_ref()
    .map_err(Clone::clone)
}

fn error_text(api: &Api, code: c_int) -> String {
    let text = unsafe { (api.strerror)(code) };
    if text.is_null() {
        format!("error {code}")
    } else {
        unsafe { CStr::from_ptr(text) }
            .to_string_lossy()
            .into_owned()
    }
}

/// A pa_simple connection, used by its stream's thread only.
struct Simple(*mut c_void);

unsafe impl Send for Simple {}

struct Shared {
    /// Interleaved samples: captured and not read yet, or written and not sent to the server yet.
    ring: Mutex<VecDeque<f32>>,
    stop: AtomicBool,
    failed: Mutex<Option<String>>,
    /// Playback: frames in the server not played yet.
    server_frames: AtomicU32,
}

pub struct Stream {
    pub name: String,
    default: bool,
    record: bool,
    /// A capture of an output: the mute generation it was opened at (it follows the sound when that changes).
    follows: Option<u32>,
    shared: Arc<Shared>,
    /// Closed by the thread when it ends.
    ended: Option<mpsc::Receiver<()>>,
}

impl Stream {
    /// What the output plays (`device`: a sink's name; None: the default output).
    pub fn loopback(device: Option<&str>) -> Result<Stream, String> {
        let generation = MUTE_GENERATION.load(Ordering::Relaxed);
        let sink = device.map(|wanted| find("sinks", wanted)).transpose()?;
        // While this output's speakers are muted, its sound is in the null sink.
        let muted = MUTED
            .lock()
            .ok()
            .and_then(|muted| muted.as_ref().map(|m| m.sink.clone()));
        let source = match (&sink, muted) {
            (Some(sink), Some(real)) if *sink == real => format!("{MUTED_SINK}.monitor"),
            (None, Some(_)) => format!("{MUTED_SINK}.monitor"),
            (Some(sink), _) => format!("{sink}.monitor"),
            (None, None) => "@DEFAULT_MONITOR@".to_string(),
        };
        let mut stream = Stream::open(Some(&source), device.is_none(), true)?;
        stream.follows = Some(generation);
        if let Some(sink) = sink {
            stream.name = format!("{sink}.monitor");
        }
        Ok(stream)
    }

    pub fn render(device: Option<&str>) -> Result<Stream, String> {
        let sink = device.map(|wanted| find("sinks", wanted)).transpose()?;
        Stream::open(sink.as_deref(), device.is_none(), false)
    }

    pub fn microphone(device: Option<&str>) -> Result<Stream, String> {
        let source = device.map(|wanted| find("sources", wanted)).transpose()?;
        Stream::open(source.as_deref(), device.is_none(), true)
    }

    fn open(device: Option<&str>, default: bool, record: bool) -> Result<Stream, String> {
        let api = api()?;
        let spec = SampleSpec {
            format: SAMPLE_FLOAT32LE,
            rate: SAMPLE_RATE,
            channels: CHANNELS as u8,
        };
        let frame_bytes = (CHANNELS * 4) as u32;
        let attr = if record {
            // The server hands over 10 ms at a time.
            BufferAttr {
                maxlength: u32::MAX,
                tlength: u32::MAX,
                prebuf: u32::MAX,
                minreq: u32::MAX,
                fragsize: FRAME as u32 * frame_bytes,
            }
        } else {
            BufferAttr {
                maxlength: u32::MAX,
                tlength: SAMPLE_RATE * SERVER_PLAYBACK_MS / 1000 * frame_bytes,
                prebuf: u32::MAX,
                minreq: u32::MAX,
                fragsize: u32::MAX,
            }
        };
        let device_c = device
            .map(|d| CString::new(d).map_err(|_| "a device name with a NUL".to_string()))
            .transpose()?;
        let mut code = 0;
        let simple = unsafe {
            (api.new)(
                std::ptr::null(),
                c"Queqiao".as_ptr(),
                if record {
                    DIRECTION_RECORD
                } else {
                    DIRECTION_PLAYBACK
                },
                device_c.as_ref().map_or(std::ptr::null(), |d| d.as_ptr()),
                if record {
                    c"capture".as_ptr()
                } else {
                    c"playback".as_ptr()
                },
                &spec,
                std::ptr::null(),
                &attr,
                &mut code,
            )
        };
        let name = device.unwrap_or(if record {
            "the default input"
        } else {
            "the default output"
        });
        if simple.is_null() {
            return Err(format!("{name}: {}", error_text(api, code)));
        }
        let shared = Arc::new(Shared {
            ring: Mutex::new(VecDeque::new()),
            stop: AtomicBool::new(false),
            failed: Mutex::new(None),
            server_frames: AtomicU32::new(0),
        });
        let (done, ended) = mpsc::channel();
        let simple = Simple(simple);
        let thread_shared = shared.clone();
        std::thread::Builder::new()
            .name("queqiao-pulse".into())
            .spawn(move || {
                let simple = simple;
                if record {
                    record_loop(api, &simple, &thread_shared);
                } else {
                    playback_loop(api, &simple, &thread_shared);
                }
                unsafe { (api.free)(simple.0) };
                drop(done);
            })
            .map_err(|e| e.to_string())?;
        Ok(Stream {
            name: if name == "@DEFAULT_MONITOR@" {
                "the default output".into()
            } else {
                name.to_string()
            },
            default,
            record,
            follows: None,
            shared,
            ended: Some(ended),
        })
    }

    fn failure(&self) -> Result<(), String> {
        match self
            .shared
            .failed
            .lock()
            .ok()
            .and_then(|failed| failed.clone())
        {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    /// What came since the last call (interleaved stereo), at once.
    pub fn read(&self, out: &mut Vec<f32>) -> Result<(), String> {
        out.clear();
        if !self.record {
            return Err("not a capture stream".into());
        }
        self.failure()?;
        if self
            .follows
            .is_some_and(|at| at != MUTE_GENERATION.load(Ordering::Relaxed))
        {
            return Err("the sound moved (this PC's speakers muted or back)".into());
        }
        if let Ok(mut ring) = self.shared.ring.lock() {
            out.extend(ring.drain(..));
        }
        Ok(())
    }

    /// Frames queued and not played yet (here and in the server).
    pub fn padding(&self) -> Result<u32, String> {
        self.failure()?;
        let here = self
            .shared
            .ring
            .lock()
            .map_or(0, |ring| ring.len() / CHANNELS) as u32;
        Ok(here + self.shared.server_frames.load(Ordering::Relaxed))
    }

    /// Queues as much of `samples` as fits in the 100 ms buffer, at once.
    pub fn write(&self, samples: &[f32]) -> Result<(), String> {
        if self.record {
            return Err("not a render stream".into());
        }
        let free = BUFFER_FRAMES.saturating_sub(self.padding()?) as usize;
        let frames = (samples.len() / CHANNELS).min(free);
        if let Ok(mut ring) = self.shared.ring.lock() {
            ring.extend(&samples[..frames * CHANNELS]);
        }
        Ok(())
    }

    /// Opened on the default device (the server moves such a stream when the default changes).
    pub fn is_default(&self) -> bool {
        self.default
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        // A read ends within 10 ms while sound (or silence) flows; the thread is left to end by itself otherwise.
        if let Some(ended) = self.ended.take() {
            let _ = ended.recv_timeout(Duration::from_millis(300));
        }
    }
}

fn fail(shared: &Shared, error: String) {
    if let Ok(mut failed) = shared.failed.lock() {
        *failed = Some(error);
    }
}

fn record_loop(api: &Api, simple: &Simple, shared: &Shared) {
    let mut buffer = vec![0f32; FRAME * CHANNELS];
    while !shared.stop.load(Ordering::Relaxed) {
        let mut code = 0;
        let bytes = buffer.len() * 4;
        let result = unsafe { (api.read)(simple.0, buffer.as_mut_ptr().cast(), bytes, &mut code) };
        if result < 0 {
            fail(shared, error_text(api, code));
            return;
        }
        if let Ok(mut ring) = shared.ring.lock() {
            ring.extend(&buffer);
            let extra = ring.len().saturating_sub(MOST_KEPT);
            ring.drain(..extra);
        }
    }
}

fn playback_loop(api: &Api, simple: &Simple, shared: &Shared) {
    let mut chunk = Vec::with_capacity(FRAME * CHANNELS);
    while !shared.stop.load(Ordering::Relaxed) {
        let mut code = 0;
        let latency = unsafe { (api.latency)(simple.0, &mut code) };
        if latency != u64::MAX {
            let frames = latency * u64::from(SAMPLE_RATE) / 1_000_000;
            shared
                .server_frames
                .store(frames.min(u64::from(u32::MAX)) as u32, Ordering::Relaxed);
        }
        chunk.clear();
        if let Ok(mut ring) = shared.ring.lock() {
            let take = ring.len().min(FRAME * CHANNELS);
            chunk.extend(ring.drain(..take));
        }
        if chunk.is_empty() {
            std::thread::sleep(Duration::from_millis(2));
            continue;
        }
        let result =
            unsafe { (api.write)(simple.0, chunk.as_ptr().cast(), chunk.len() * 4, &mut code) };
        if result < 0 {
            fail(shared, error_text(api, code));
            return;
        }
    }
}

/// Where the sound goes while this PC's speakers are muted: a null sink of PulseAudio's own.
const MUTED_SINK: &str = "queqiao_muted";

/// The muted output: its sink, the default output before, the null sink's module.
struct Muted {
    sink: String,
    default: String,
    module: String,
}

static MUTED: Mutex<Option<Muted>> = Mutex::new(None);
/// Changes each time the sound moves (captures of an output open again).
static MUTE_GENERATION: AtomicU32 = AtomicU32::new(0);

/// Mutes or unmutes an output's speakers (`device`: a sink by name or part of it; None: the default), as Windows
/// does: silent here, still captured. Returns whether it was muted before.
pub fn set_mute(device: Option<&str>, mute: bool) -> Result<bool, String> {
    let mut muted = MUTED
        .lock()
        .map_err(|_| "the mute state is poisoned".to_string())?;
    if !mute {
        let Some(state) = muted.take() else {
            return Ok(false);
        };
        let moved = move_inputs(MUTED_SINK, &state.sink);
        let _ = pactl(&["set-default-sink", &state.default]);
        let _ = pactl(&["unload-module", &state.module]);
        MUTE_GENERATION.fetch_add(1, Ordering::Relaxed);
        moved?;
        return Ok(true);
    }
    if muted.is_some() {
        return Ok(true);
    }
    let default = default_sink()?;
    let sink = match device {
        Some(wanted) => find("sinks", wanted)?,
        None => default.clone(),
    };
    // Muted by the person already: nothing to do (and nothing to put back).
    if pactl(&["get-sink-mute", &sink]).is_ok_and(|state| state.trim().ends_with("yes")) {
        return Ok(true);
    }
    let module = pactl(&[
        "load-module",
        "module-null-sink",
        &format!("sink_name={MUTED_SINK}"),
        "sink_properties=device.description=Queqiao-speakers-muted",
    ])?
    .trim()
    .to_string();
    let moved = move_inputs(&sink, MUTED_SINK);
    if sink == default {
        let _ = pactl(&["set-default-sink", MUTED_SINK]);
    }
    *muted = Some(Muted {
        sink,
        default,
        module,
    });
    MUTE_GENERATION.fetch_add(1, Ordering::Relaxed);
    moved.map(|()| false)
}

/// The default output's name (`pactl info`: every PulseAudio and PipeWire version has it).
fn default_sink() -> Result<String, String> {
    pactl(&["info"])?
        .lines()
        .find_map(|line| line.strip_prefix("Default Sink: "))
        .map(|name| name.trim().to_string())
        .ok_or_else(|| "no default output".to_string())
}

/// Moves every stream playing into sink `from` to sink `to`.
fn move_inputs(from: &str, to: &str) -> Result<(), String> {
    let index = pactl(&["list", "short", "sinks"])?
        .lines()
        .find(|line| line.split('\t').nth(1) == Some(from))
        .and_then(|line| line.split('\t').next().map(str::to_string))
        .ok_or_else(|| format!("no output {from}"))?;
    for line in pactl(&["list", "short", "sink-inputs"])?.lines() {
        let mut fields = line.split('\t');
        if let (Some(input), Some(sink)) = (fields.next(), fields.next())
            && sink == index
        {
            pactl(&["move-sink-input", input, to])?;
        }
    }
    Ok(())
}

/// The outputs (sinks), by name.
pub fn render_devices() -> Vec<String> {
    list("sinks", |_| true)
}

/// The sink or source (`kind`: "sinks", "sources") named `wanted`, or the first whose name holds it (case aside), as
/// Windows finds a device by part of its name.
fn find(kind: &str, wanted: &str) -> Result<String, String> {
    let names = list(kind, |_| true);
    let lower = wanted.to_lowercase();
    names
        .iter()
        .find(|name| *name == wanted)
        .or_else(|| {
            names
                .iter()
                .find(|name| name.to_lowercase().contains(&lower))
        })
        .cloned()
        .ok_or_else(|| format!("no active sound device named like \"{wanted}\""))
}

fn list(kind: &str, keep: impl Fn(&str) -> bool) -> Vec<String> {
    pactl(&["list", "short", kind])
        .map(|text| {
            text.lines()
                .filter_map(|line| line.split('\t').nth(1))
                .filter(|name| keep(name))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// The sink the viewer's microphone is played into; programs here take its monitor as their microphone.
const MICROPHONE_SINK: &str = "queqiao_microphone";

/// A virtual cable on this PC (as Windows' pairs of an installed cable's speaker and microphone): a null sink of
/// PulseAudio's own, made the first time it is needed (nothing is installed) and named "Queqiao microphone"; what is
/// played into it comes out of its monitor, which programs can use as their microphone.
pub fn virtual_microphone() -> Option<VirtualMicrophone> {
    if !render_devices().iter().any(|name| name == MICROPHONE_SINK) {
        pactl(&[
            "load-module",
            "module-null-sink",
            &format!("sink_name={MICROPHONE_SINK}"),
            "sink_properties=device.description=Queqiao-microphone",
        ])
        .ok()?;
    }
    render_devices()
        .iter()
        .any(|name| name == MICROPHONE_SINK)
        .then(|| VirtualMicrophone {
            speaker: MICROPHONE_SINK.to_string(),
            microphone: format!("{MICROPHONE_SINK}.monitor"),
        })
}

fn pactl(args: &[&str]) -> Result<String, String> {
    let output = Command::new("pactl")
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|error| format!("pactl: {error}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(format!(
            "pactl {}: {}",
            args.first().unwrap_or(&""),
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}
