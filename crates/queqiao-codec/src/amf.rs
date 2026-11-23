//! AMD's encoders driven directly through AMF, as UU Remote uses the vendor SDKs (and Queqiao NVENC and oneVPL).
//! AMD's hardware encodes 4:2:0 only: H.264, and H.265 in 8 or 10 bits (AV1 on newer cards, not used yet).
//!
//! The runtime is the graphics driver's own: `amfrt64.dll` in System32, installed with AMD's driver. Nothing of AMD's
//! is shipped; the types come from AMF's headers (MIT), `amf_sys.rs`. AMF works on our Direct3D 11 device: each
//! frame's texture (NV12, or P010 for 10-bit) is handed over as it is, encoded at once, and its output taken before
//! the next (one frame in flight, no B-frames, keyframes on request, the bitrate changed in place).

use std::ffi::c_void;
use std::mem::zeroed;
use std::ptr::null_mut;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{E_FAIL, FreeLibrary, HMODULE};
use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11Texture2D};
use windows::Win32::System::LibraryLoader::{
    GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW,
};
use windows::core::{Error, Interface, PCWSTR, Result, s, w};

use crate::amf_sys as sys;
use crate::encoder::{EncodedFrame, EncoderConfig};
use crate::format::{Codec, EncodeFormat};

const fn full_version(major: u64, minor: u64, release: u64, build: u64) -> u64 {
    (major << 48) | (minor << 32) | (release << 16) | build
}

/// The headers' version (AMF 1.5.3): a runtime at least this new is asked for this one, an older one for its own
/// (the interfaces Queqiao uses are older than both).
const HEADERS: u64 = full_version(1, 5, 3, 0);
/// The oldest runtime used: AMF 1.4 (drivers from 2017 on).
const OLDEST: u64 = full_version(1, 4, 0, 0);
const _: () = assert!(OLDEST < HEADERS);

/// The peak bitrate, as a multiple of the mean (as the other encoders).
const PEAK: f64 = 1.5;
/// How long an encoded frame may take to come out before it counts as lost.
const OUTPUT_WAIT: Duration = Duration::from_millis(200);

/// The AMFBuffer interface id (core/Buffer.h).
const IID_BUFFER: sys::AMFGuid = sys::AMFGuid {
    data1: 0xb04b_7248,
    data2: 0xb6f0,
    data3: 0x4321,
    data41: 0xb6,
    data42: 0x91,
    data43: 0xba,
    data44: 0xa4,
    data45: 0x74,
    data46: 0x0f,
    data47: 0x9f,
    data48: 0xcb,
};

/// Calls a method of an AMF interface (a C struct whose first field points at its function table).
macro_rules! call {
    ($object:expr, $method:ident $(, $argument:expr)*) => {{
        let object = $object;
        match (*(*object).pVtbl).$method {
            Some(method) => Ok(method(object $(, $argument)*)),
            None => Err(Error::new(E_FAIL, concat!("AMF has no ", stringify!($method)))),
        }
    }};
}

fn check(result: sys::AMF_RESULT, what: &str) -> Result<()> {
    if result == sys::AMF_OK {
        Ok(())
    } else {
        Err(Error::new(E_FAIL, format!("AMF {what}: result {result}")))
    }
}

fn int(value: i64) -> sys::AMFVariantStruct {
    let mut variant: sys::AMFVariantStruct = unsafe { zeroed() };
    variant.type_ = sys::AMF_VARIANT_INT64;
    variant.__bindgen_anon_1.int64Value = value;
    variant
}

fn boolean(value: bool) -> sys::AMFVariantStruct {
    let mut variant: sys::AMFVariantStruct = unsafe { zeroed() };
    variant.type_ = sys::AMF_VARIANT_BOOL;
    variant.__bindgen_anon_1.boolValue = u8::from(value);
    variant
}

fn size(width: u32, height: u32) -> sys::AMFVariantStruct {
    let mut variant: sys::AMFVariantStruct = unsafe { zeroed() };
    variant.type_ = sys::AMF_VARIANT_SIZE;
    variant.__bindgen_anon_1.sizeValue = sys::AMFSize {
        width: width as i32,
        height: height as i32,
    };
    variant
}

fn rate(fps: u32) -> sys::AMFVariantStruct {
    let mut variant: sys::AMFVariantStruct = unsafe { zeroed() };
    variant.type_ = sys::AMF_VARIANT_RATE;
    variant.__bindgen_anon_1.rateValue = sys::AMFRate { num: fps, den: 1 };
    variant
}

/// The property names of one codec's encoder (AMF names H.264's plainly and H.265's with "Hevc").
struct Names {
    component: PCWSTR,
    usage: PCWSTR,
    profile: PCWSTR,
    quality: PCWSTR,
    frame_size: PCWSTR,
    frame_rate: PCWSTR,
    rate_control: PCWSTR,
    target: PCWSTR,
    peak: PCWSTR,
    vbv: PCWSTR,
    enforce_hrd: PCWSTR,
    filler: PCWSTR,
    force_picture: PCWSTR,
}

const AVC: Names = Names {
    component: w!("AMFVideoEncoderVCE_AVC"),
    usage: w!("Usage"),
    profile: w!("Profile"),
    quality: w!("QualityPreset"),
    frame_size: w!("FrameSize"),
    frame_rate: w!("FrameRate"),
    rate_control: w!("RateControlMethod"),
    target: w!("TargetBitrate"),
    peak: w!("PeakBitrate"),
    vbv: w!("VBVBufferSize"),
    enforce_hrd: w!("EnforceHRD"),
    filler: w!("FillerDataEnable"),
    force_picture: w!("ForcePictureType"),
};

const HEVC: Names = Names {
    component: w!("AMFVideoEncoderHW_HEVC"),
    usage: w!("HevcUsage"),
    profile: w!("HevcProfile"),
    quality: w!("HevcQualityPreset"),
    frame_size: w!("HevcFrameSize"),
    frame_rate: w!("HevcFrameRate"),
    rate_control: w!("HevcRateControlMethod"),
    target: w!("HevcTargetBitrate"),
    peak: w!("HevcPeakBitrate"),
    vbv: w!("HevcVBVBufferSize"),
    enforce_hrd: w!("HevcEnforceHRD"),
    filler: w!("HevcFillerDataEnable"),
    force_picture: w!("HevcForcePictureType"),
};

type Init = unsafe extern "C" fn(u64, *mut *mut sys::AMFFactory) -> sys::AMF_RESULT;
type QueryVersion = unsafe extern "C" fn(*mut u64) -> sys::AMF_RESULT;

pub(crate) struct AmfEncoder {
    library: HMODULE,
    context: *mut sys::AMFContext,
    component: *mut sys::AMFComponent,
    names: &'static Names,
    ready: Vec<EncodedFrame>,
    fps: u32,
    pub(crate) name: String,
    pub(crate) format: EncodeFormat,
}

impl AmfEncoder {
    pub(crate) fn open(
        device: &ID3D11Device,
        config: &EncoderConfig,
        format: EncodeFormat,
    ) -> Result<AmfEncoder> {
        if format.chroma444 {
            return Err(Error::new(E_FAIL, "AMD's encoders make 4:2:0 only"));
        }
        if format.codec == Codec::H264 && format.ten_bit {
            return Err(Error::new(
                E_FAIL,
                "AMD's encoders make H.264 in 8 bits only",
            ));
        }
        let library =
            unsafe { LoadLibraryExW(w!("amfrt64.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32) }
                .map_err(|e| {
                    Error::new(e.code(), "no AMD graphics driver with AMF (amfrt64.dll)")
                })?;
        let mut encoder = AmfEncoder {
            library,
            context: null_mut(),
            component: null_mut(),
            names: match format.codec {
                Codec::H264 => &AVC,
                Codec::H265 => &HEVC,
            },
            ready: Vec::new(),
            fps: config.fps.max(1),
            name: format!("AMD AMF {} (hardware)", format.name()),
            format,
        };
        // From here on, Drop cleans up whatever was made.
        encoder.start(device, config)?;
        Ok(encoder)
    }

    fn start(&mut self, device: &ID3D11Device, config: &EncoderConfig) -> Result<()> {
        let query = unsafe { GetProcAddress(self.library, s!("AMFQueryVersion")) }
            .ok_or_else(|| Error::new(E_FAIL, "AMFQueryVersion missing"))?;
        let query: QueryVersion = unsafe { std::mem::transmute::<_, QueryVersion>(query) };
        let mut runtime = 0u64;
        check(unsafe { query(&mut runtime) }, "runtime version")?;
        if runtime < OLDEST {
            return Err(Error::new(
                E_FAIL,
                format!("the AMD driver's AMF ({:x}) is older than 1.4", runtime),
            ));
        }
        let init = unsafe { GetProcAddress(self.library, s!("AMFInit")) }
            .ok_or_else(|| Error::new(E_FAIL, "AMFInit missing"))?;
        let init: Init = unsafe { std::mem::transmute::<_, Init>(init) };
        let mut factory: *mut sys::AMFFactory = null_mut();
        check(unsafe { init(runtime.min(HEADERS), &mut factory) }, "init")?;
        unsafe {
            check(call!(factory, CreateContext, &mut self.context)?, "context")?;
            check(
                call!(self.context, InitDX11, device.as_raw(), sys::AMF_DX11_0)?,
                "D3D11 device",
            )?;
            check(
                call!(
                    factory,
                    CreateComponent,
                    self.context,
                    self.names.component.as_ptr(),
                    &mut self.component
                )?,
                "encoder component",
            )?;
        }
        let hevc = self.format.codec == Codec::H265;
        // The usage first: it sets the others' defaults.
        self.set(
            self.names.usage,
            int(i64::from(if hevc {
                sys::AMF_VIDEO_ENCODER_HEVC_USAGE_ULTRA_LOW_LATENCY
            } else {
                sys::AMF_VIDEO_ENCODER_USAGE_ULTRA_LOW_LATENCY
            })),
        )?;
        let profile = match (hevc, self.format.ten_bit) {
            (false, _) => sys::AMF_VIDEO_ENCODER_PROFILE_HIGH,
            (true, false) => sys::AMF_VIDEO_ENCODER_HEVC_PROFILE_MAIN,
            (true, true) => sys::AMF_VIDEO_ENCODER_HEVC_PROFILE_MAIN_10,
        };
        self.set(self.names.profile, int(i64::from(profile)))?;
        if hevc {
            let depth = if self.format.ten_bit {
                sys::AMF_COLOR_BIT_DEPTH_10
            } else {
                sys::AMF_COLOR_BIT_DEPTH_8
            };
            self.set(w!("HevcColorBitDepth"), int(i64::from(depth)))?;
            self.set(
                w!("HevcNominalRange"),
                int(i64::from(sys::AMF_VIDEO_ENCODER_HEVC_NOMINAL_RANGE_STUDIO)),
            )?;
            // Parameter sets with every keyframe (a viewer joins or recovers at any keyframe).
            self.set(
                w!("HevcHeaderInsertionMode"),
                int(i64::from(
                    sys::AMF_VIDEO_ENCODER_HEVC_HEADER_INSERTION_MODE_IDR_ALIGNED,
                )),
            )?;
        } else {
            // No B-frames (they would hold frames back).
            self.set(w!("BPicturesPattern"), int(0))?;
        }
        let quality = if hevc {
            sys::AMF_VIDEO_ENCODER_HEVC_QUALITY_PRESET_SPEED
        } else {
            sys::AMF_VIDEO_ENCODER_QUALITY_PRESET_SPEED
        };
        self.set(self.names.quality, int(i64::from(quality)))?;
        self.set(self.names.frame_size, size(config.width, config.height))?;
        self.set(self.names.frame_rate, rate(self.fps))?;
        let control = if hevc {
            sys::AMF_VIDEO_ENCODER_HEVC_RATE_CONTROL_METHOD_PEAK_CONSTRAINED_VBR
        } else {
            sys::AMF_VIDEO_ENCODER_RATE_CONTROL_METHOD_PEAK_CONSTRAINED_VBR
        };
        self.set(self.names.rate_control, int(i64::from(control)))?;
        self.set(self.names.enforce_hrd, boolean(false))?;
        self.set(self.names.filler, boolean(false))?;
        self.set(w!("LowLatencyInternal"), boolean(true))?;
        self.apply_bitrate(config.bitrate)?;
        let surface = if self.format.ten_bit {
            sys::AMF_SURFACE_P010
        } else {
            sys::AMF_SURFACE_NV12
        };
        unsafe {
            check(
                call!(
                    self.component,
                    Init,
                    surface,
                    config.width as i32,
                    config.height as i32
                )?,
                "encoder init",
            )
        }
    }

    fn set(&self, name: PCWSTR, value: sys::AMFVariantStruct) -> Result<()> {
        let result = unsafe { call!(self.component, SetProperty, name.as_ptr(), value)? };
        check(result, &format!("property {}", unsafe { name.display() }))
    }

    fn apply_bitrate(&self, bitrate: u32) -> Result<()> {
        self.set(self.names.target, int(i64::from(bitrate)))?;
        self.set(self.names.peak, int((f64::from(bitrate) * PEAK) as i64))?;
        // About one frame's worth of buffer: no frame waits behind a large one for long.
        self.set(self.names.vbv, int(i64::from(bitrate / self.fps * 2)))
    }

    pub(crate) fn poll(&mut self, out: &mut Vec<EncodedFrame>) {
        out.append(&mut self.ready);
    }

    /// Encodes one texture (NV12, or P010 for 10-bit) now; the frame is ready when this returns.
    pub(crate) fn encode(
        &mut self,
        input: &ID3D11Texture2D,
        time: i64,
        keyframe: bool,
    ) -> Result<()> {
        let mut surface: *mut sys::AMFSurface = null_mut();
        unsafe {
            check(
                call!(
                    self.context,
                    CreateSurfaceFromDX11Native,
                    input.as_raw(),
                    &mut surface,
                    null_mut()
                )?,
                "surface from texture",
            )?;
        }
        let result = self.submit(surface, time, keyframe);
        unsafe {
            let _ = call!(surface, Release);
        }
        result?;
        self.take_output(time)
    }

    fn submit(&mut self, surface: *mut sys::AMFSurface, time: i64, keyframe: bool) -> Result<()> {
        unsafe {
            call!(surface, SetPts, time)?;
            if keyframe {
                let idr = if self.format.codec == Codec::H265 {
                    sys::AMF_VIDEO_ENCODER_HEVC_PICTURE_TYPE_IDR
                } else {
                    sys::AMF_VIDEO_ENCODER_PICTURE_TYPE_IDR
                };
                check(
                    call!(
                        surface,
                        SetProperty,
                        self.names.force_picture.as_ptr(),
                        int(i64::from(idr))
                    )?,
                    "keyframe",
                )?;
                if self.format.codec == Codec::H264 {
                    call!(
                        surface,
                        SetProperty,
                        w!("InsertSPS").as_ptr(),
                        boolean(true)
                    )?;
                    call!(
                        surface,
                        SetProperty,
                        w!("InsertPPS").as_ptr(),
                        boolean(true)
                    )?;
                }
            }
            let deadline = Instant::now() + OUTPUT_WAIT;
            loop {
                let result = call!(self.component, SubmitInput, surface.cast::<sys::AMFData>())?;
                if result != sys::AMF_INPUT_FULL || Instant::now() > deadline {
                    return check(result, "submit");
                }
                // The encoder holds frames still: take one out, then submit again.
                self.take_output(time)?;
            }
        }
    }

    /// Waits for the next encoded frame (up to OUTPUT_WAIT) and keeps it for `poll`.
    fn take_output(&mut self, time: i64) -> Result<()> {
        let deadline = Instant::now() + OUTPUT_WAIT;
        loop {
            let mut data: *mut sys::AMFData = null_mut();
            let result = unsafe { call!(self.component, QueryOutput, &mut data)? };
            if result == sys::AMF_OK && !data.is_null() {
                let frame = unsafe { self.copy_out(data, time) };
                unsafe {
                    let _ = call!(data, Release);
                }
                self.ready.push(frame?);
                return Ok(());
            }
            if result != sys::AMF_REPEAT && result != sys::AMF_OK {
                return check(result, "output");
            }
            if Instant::now() > deadline {
                return Err(Error::new(E_FAIL, "AMF gave no frame within 200 ms"));
            }
            std::thread::sleep(Duration::from_micros(500));
        }
    }

    unsafe fn copy_out(&self, data: *mut sys::AMFData, time: i64) -> Result<EncodedFrame> {
        let mut buffer: *mut c_void = null_mut();
        unsafe {
            check(
                call!(data, QueryInterface, &IID_BUFFER, &mut buffer)?,
                "output buffer",
            )?;
            let buffer = buffer.cast::<sys::AMFBuffer>();
            let length = call!(buffer, GetSize)?;
            let native = call!(buffer, GetNative)?;
            let bytes = if native.is_null() {
                Vec::new()
            } else {
                std::slice::from_raw_parts(native.cast::<u8>(), length).to_vec()
            };
            let _ = call!(buffer, Release);
            let keyframe = match self.format.codec {
                Codec::H264 => crate::nal_types(&bytes).contains(&5),
                Codec::H265 => crate::hevc_nal_types(&bytes)
                    .iter()
                    .any(|kind| (19..=21).contains(kind)),
            };
            Ok(EncodedFrame {
                data: bytes,
                time,
                keyframe,
            })
        }
    }

    /// Changes the bitrate of the running encoder (no keyframe).
    pub(crate) fn set_bitrate(&mut self, bitrate: u32) -> Result<()> {
        self.apply_bitrate(bitrate)
    }
}

impl Drop for AmfEncoder {
    fn drop(&mut self) {
        unsafe {
            if !self.component.is_null() {
                let _ = call!(self.component, Terminate);
                let _ = call!(self.component, Release);
            }
            if !self.context.is_null() {
                let _ = call!(self.context, Terminate);
                let _ = call!(self.context, Release);
            }
            let _ = FreeLibrary(self.library);
        }
    }
}

// The raw pointers are only used from the thread that owns the encoder, as with NVENC.
unsafe impl Send for AmfEncoder {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_are_packed_as_amf_does() {
        assert_eq!(HEADERS, 0x0001_0005_0003_0000);
    }

    #[test]
    fn variants_carry_their_type() {
        let v = int(42);
        assert_eq!(v.type_, sys::AMF_VARIANT_INT64);
        assert_eq!(unsafe { v.__bindgen_anon_1.int64Value }, 42);
        let s = size(1920, 1080);
        assert_eq!(s.type_, sys::AMF_VARIANT_SIZE);
        assert_eq!(unsafe { s.__bindgen_anon_1.sizeValue.height }, 1080);
    }
}
