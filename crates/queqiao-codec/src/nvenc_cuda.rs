//! NVIDIA's encoder (NVENC) on Linux: the shared session (nvenc_core.rs) on a CUDA context, fed from memory through
//! one pitched CUDA buffer: the CPU converter's NV12 (4:2:0) or P010 (10-bit 4:2:0), or the screen's B, G, R, X
//! (4:4:4) or those widened to R10G10B10A2 (10-bit 4:4:4), which NVENC turns into YUV itself, as on Windows. `libnvidia-encode.so.1` and `libcuda.so.1` come with NVIDIA's driver and are
//! loaded at run time.

use std::mem::ManuallyDrop;

use crate::config::{EncodedFrame, EncoderConfig};
use crate::cuda::{CuDevicePtr, Cuda, Library};
use crate::format::EncodeFormat;
use crate::nvenc_core::{self, CreateInstance, MaxVersion, NvencCore, Registered};
use crate::nvenc_sys as sys;

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

pub(crate) struct NvencCuda {
    /// Dropped first, by hand (`Drop`): the session goes before the memory it registered and the context.
    core: ManuallyDrop<NvencCore>,
    buffer: CuDevicePtr,
    pitch: usize,
    registered: Registered,
    input: Input,
    size: (usize, usize),
    cuda: Cuda,
    _library: Library,
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
        let mut core = NvencCore::open(
            create,
            max_version,
            sys::NV_ENC_DEVICE_TYPE_CUDA,
            cuda.context(),
            config,
            format,
        )?;
        core.name = format!("{}, CUDA on {}", core.name, cuda.device_name);
        let (width, height) = (config.width as usize, config.height as usize);
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
                    sys::NV_ENC_INPUT_RESOURCE_TYPE_CUDADEVICEPTR,
                    buffer as *mut std::ffi::c_void,
                    (config.width, config.height),
                    pitch_u32,
                )
            });
        let registered = match registered {
            Ok(registered) => registered,
            Err(error) => {
                // The session first, then its memory.
                drop(core);
                cuda.free(buffer);
                return Err(error);
            }
        };
        Ok(NvencCuda {
            core: ManuallyDrop::new(core),
            buffer,
            pitch,
            registered,
            input,
            size: (width, height),
            cuda,
            _library: library,
        })
    }

    pub(crate) fn name(&self) -> &str {
        &self.core.name
    }

    pub(crate) fn format(&self) -> EncodeFormat {
        self.core.format
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
        unsafe { ManuallyDrop::drop(&mut self.core) };
        self.cuda.free(self.buffer);
    }
}
