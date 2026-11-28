//! NVIDIA's encoder (NVENC) on Linux: the shared session (nvenc_core.rs) on a CUDA context, fed from memory through
//! one pitched CUDA buffer: the CPU converter's NV12 (4:2:0) or P010 (10-bit 4:2:0), or the screen's B, G, R, X
//! (4:4:4) or those widened to R10G10B10A2 (10-bit 4:4:4), which NVENC turns into YUV itself, as on Windows. `libnvidia-encode.so.1` and `libcuda.so.1` come with NVIDIA's driver and are
//! loaded at run time.

use std::mem::ManuallyDrop;
use std::sync::Mutex;

use crate::config::{EncodedFrame, EncoderConfig};
use crate::cuda::{CuDevicePtr, Cuda, Library};
use crate::format::EncodeFormat;
use crate::nvenc_core::{self, CreateInstance, MaxVersion, NvencCore, Registered};

/// What the encoder takes from memory for a format.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Input {
    /// Y rows, then interleaved U, V rows (half as many).
    Nv12,
    /// As NV12 in 16-bit samples, the 10 bits at the top.
    P010,
    /// The screen's pixels as they are (B, G, R, X bytes), at the encode size.
    Bgrx,
    /// The screen's pixels widened to 10 bits a colour: R10G10B10A2 words, red lowest.
    Rgb10,
}

/// The sessions open in this process, and the one held open past its drop (see [`Closing`]).
struct Sessions {
    open: usize,
    held: Option<Closing>,
}

static SESSIONS: Mutex<Sessions> = Mutex::new(Sessions {
    open: 0,
    held: None,
});

pub(crate) struct NvencCuda {
    /// Taken by hand when dropped (`Drop`, into [`Closing`]).
    core: ManuallyDrop<NvencCore>,
    buffer: CuDevicePtr,
    pitch: usize,
    registered: Registered,
    input: Input,
    size: (usize, usize),
    cuda: ManuallyDrop<Cuda>,
    library: ManuallyDrop<Library>,
    /// Opened while no other session was: closed last (see [`Closing`]).
    first: bool,
}

/// A session being closed: the session first, then the memory it registered, the context, the library.
///
/// NVIDIA's driver (570.153.02 on the Linux build machine) crashed a few seconds after the first of the process's
/// sessions (opened while no other one was) was closed while a later one stayed open: a fault on one of the driver's
/// own threads, in libnvcuvid.so.1 (which libnvidia-encode loads). The session's encoder replaced at a new size
/// (Super Screen on or off, a screen change) took the agent down so. Closing the later ones first, or the first one
/// last, crashed nothing: the first is held open (idle) until the others are closed, then closed itself.
struct Closing {
    core: ManuallyDrop<NvencCore>,
    buffer: CuDevicePtr,
    cuda: Cuda,
    _library: Library,
}

// Held in SESSIONS and closed by whichever thread closes the last session: NVENC and CUDA take these calls from any
// thread (the context is made current by the calls that need it), and nothing else uses the session by then.
unsafe impl Send for Closing {}

impl Drop for Closing {
    fn drop(&mut self) {
        unsafe { ManuallyDrop::drop(&mut self.core) };
        self.cuda.free(self.buffer);
    }
}

impl NvencCuda {
    /// A session on CUDA GPU `ordinal` for `config`'s size, making `format`.
    pub(crate) fn open(
        ordinal: i32,
        config: &EncoderConfig,
        format: EncodeFormat,
    ) -> Result<Self, String> {
        nvenc_core::supports(format)?;
        let input = match (format.chroma444, format.ten_bit) {
            (false, false) => Input::Nv12,
            (false, true) => Input::P010,
            (true, false) => Input::Bgrx,
            (true, true) => Input::Rgb10,
        };
        let library = Library::open(c"libnvidia-encode.so.1")
            .map_err(|_| "no NVIDIA encoder driver (libnvidia-encode.so.1)".to_string())?;
        let (create, max_version): (CreateInstance, MaxVersion) = unsafe {
            (
                library.function(c"NvEncodeAPICreateInstance")?,
                library.function(c"NvEncodeAPIGetMaxSupportedVersion")?,
            )
        };
        let cuda = Cuda::open(ordinal)?;
        let (width, height) = (config.width as usize, config.height as usize);
        // Counted before it opens: whether another session is open decides which is closed last.
        let first = SESSIONS.lock().is_ok_and(|mut sessions| {
            sessions.open += 1;
            sessions.open == 1
        });
        let opened = (|| {
            let mut core = NvencCore::open(
                create,
                max_version,
                nvenc_core::DEVICE_CUDA,
                cuda.context(),
                config,
                format,
            )?;
            core.add_to_name(&format!(", CUDA on {}", cuda.device_name));
            let (width_bytes, rows) = match input {
                Input::Nv12 => (width, height + height / 2),
                Input::P010 => (width * 2, height + height / 2),
                Input::Bgrx | Input::Rgb10 => (width * 4, height),
            };
            let (buffer, pitch) = cuda.alloc_pitch(width_bytes, rows)?;
            let registered = u32::try_from(pitch)
                .map_err(|_| "CUDA pitch out of range".to_string())
                .and_then(|pitch_u32| {
                    core.register(
                        buffer as usize,
                        nvenc_core::RESOURCE_CUDA_DEVICE_PTR,
                        buffer as *mut std::ffi::c_void,
                        (config.width, config.height),
                        pitch_u32,
                    )
                });
            match registered {
                Ok(registered) => Ok((core, buffer, pitch, registered)),
                Err(error) => {
                    // The session first, then its memory.
                    drop(core);
                    cuda.free(buffer);
                    Err(error)
                }
            }
        })();
        let (core, buffer, pitch, registered) = opened.inspect_err(|_| {
            if let Ok(mut sessions) = SESSIONS.lock() {
                sessions.open = sessions.open.saturating_sub(1);
            }
        })?;
        Ok(NvencCuda {
            core: ManuallyDrop::new(core),
            buffer,
            pitch,
            registered,
            input,
            size: (width, height),
            cuda: ManuallyDrop::new(cuda),
            library: ManuallyDrop::new(library),
            first,
        })
    }

    pub(crate) fn name(&self) -> &str {
        self.core.name()
    }

    pub(crate) fn format(&self) -> EncodeFormat {
        self.core.format()
    }

    /// What `encode` takes.
    pub(crate) fn input(&self) -> Input {
        self.input
    }

    pub(crate) fn poll(&mut self, out: &mut Vec<EncodedFrame>) {
        self.core.poll(out);
    }

    /// Uploads one picture and encodes it now; the frame is ready when this returns. NV12 and P010: `planes` is the Y
    /// plane then the U, V plane, rows `pitch` bytes apart; B, G, R, X and R10G10B10A2: one plane.
    pub(crate) fn encode(
        &mut self,
        planes: &[&[u8]],
        pitch: usize,
        time: i64,
        keyframe: bool,
    ) -> Result<(), String> {
        let (width, height) = self.size;
        match (self.input, planes) {
            (Input::Nv12 | Input::P010, [y, uv]) => {
                let row_bytes = if self.input == Input::P010 {
                    width * 2
                } else {
                    width
                };
                self.cuda
                    .upload(y, pitch, self.buffer, self.pitch, row_bytes, height)?;
                let chroma = self.buffer + (self.pitch * height) as CuDevicePtr;
                self.cuda
                    .upload(uv, pitch, chroma, self.pitch, row_bytes, height / 2)?;
            }
            (Input::Bgrx | Input::Rgb10, [pixels]) => {
                self.cuda
                    .upload(pixels, pitch, self.buffer, self.pitch, width * 4, height)?;
            }
            _ => return Err(format!("{:?} input in {} planes", self.input, planes.len())),
        }
        self.core.encode(self.registered, time, keyframe)
    }

    /// Changes the bitrate of the running session (no keyframe, no reset).
    pub(crate) fn set_bitrate(&mut self, bitrate: u32) -> Result<(), String> {
        self.core.set_bitrate(bitrate)
    }
}

impl Drop for NvencCuda {
    fn drop(&mut self) {
        // Each part taken once, here.
        let closing = unsafe {
            Closing {
                core: ManuallyDrop::new(ManuallyDrop::take(&mut self.core)),
                buffer: self.buffer,
                cuda: ManuallyDrop::take(&mut self.cuda),
                _library: ManuallyDrop::take(&mut self.library),
            }
        };
        let Ok(mut sessions) = SESSIONS.lock() else {
            return;
        };
        if self.first && sessions.open > 1 {
            // Still counted as open: a session opened meanwhile is not the first.
            sessions.held = Some(closing);
            return;
        }
        drop(closing);
        sessions.open = sessions.open.saturating_sub(1);
        // Only the first one left, held: closed now too.
        if sessions.open == 1
            && let Some(held) = sessions.held.take()
        {
            drop(held);
            sessions.open = 0;
        }
    }
}
