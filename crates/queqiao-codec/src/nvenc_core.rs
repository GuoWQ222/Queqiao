//! NVIDIA's encoder (NVENC) through the Video Codec SDK API: the session shared by Windows (D3D11 textures,
//! nvenc.rs) and Linux (CUDA memory, nvenc_cuda.rs). The platform layer loads the driver at run time, so the program
//! still starts without NVIDIA graphics.
//!
//! Two API versions, the same session code compiled for each (nvenc_session.rs): 12.0 where the driver has it
//! (R520 and later, late 2022), which adds AV1; 11.1 for older drivers (from R470, mid-2021), which make H.264 and
//! H.265 as before. The newest header would need the newest driver (measured: a March 2026 driver supports 13.0 and
//! refused 13.1), so neither is newer than needed. `QUEQIAO_NVENC_API=11` keeps a newer driver on 11.1 (to test that
//! path).
//!
//! Used synchronously: each input is registered once, then per frame mapped, encoded, and its bitstream locked
//! (which waits for the GPU). Settings follow NVIDIA's advice for game streaming: preset P1 with the
//! ultra-low-latency tuning, constant bitrate with a one-frame buffer, no B-frames, an endless GOP with keyframes on
//! request, the sequence header (SPS/PPS) repeated with every keyframe, BT.709 limited range.

use std::ffi::c_void;

use crate::config::{EncodedFrame, EncoderConfig};
use crate::format::{Codec, EncodeFormat};

/// NvEncodeAPICreateInstance: fills the function table (of the API version it is called for).
pub(crate) type CreateInstance = unsafe extern "C" fn(*mut c_void) -> u32;
/// NvEncodeAPIGetMaxSupportedVersion: the driver's highest API version as (major << 4) | minor.
pub(crate) type MaxVersion = unsafe extern "C" fn(*mut u32) -> u32;

/// NV_ENC_DEVICE_TYPE and NV_ENC_INPUT_RESOURCE_TYPE values: the same in both API versions.
#[cfg(windows)]
pub(crate) const DEVICE_DIRECTX: u32 = 0;
#[cfg(target_os = "linux")]
pub(crate) const DEVICE_CUDA: u32 = 1;
#[cfg(windows)]
pub(crate) const RESOURCE_DIRECTX: u32 = 0;
#[cfg(target_os = "linux")]
pub(crate) const RESOURCE_CUDA_DEVICE_PTR: u32 = 1;

/// Whether NVENC makes `format` (or AV1 for `config`) at all (checked before the driver is loaded).
pub(crate) fn supports(format: EncodeFormat) -> Result<(), String> {
    if format.codec == Codec::H264 && format.ten_bit {
        Err("NVENC makes H.264 in 8 bits only".into())
    } else {
        Ok(())
    }
}

/// An input registered with NVENC (a texture on Windows, CUDA memory on Linux).
#[derive(Clone, Copy)]
pub(crate) struct Registered {
    resource: *mut c_void,
    /// Bytes between rows; 0 when NVENC knows it from the resource (Direct3D textures).
    pitch: u32,
}

/// API 11.1 (nvEncodeAPI.h of nv-codec-headers n11.1.5.3): H.264 and H.265.
pub(crate) mod v11 {
    use crate::nvenc_sys as sys;

    // Struct versions of SDK 11.1; they change with the header.
    const FUNCTION_LIST_VER: u32 = struct_version(2, false);
    const OPEN_SESSION_VER: u32 = struct_version(1, false);
    const PRESET_CONFIG_VER: u32 = struct_version(4, true);
    const CONFIG_VER: u32 = struct_version(7, true);
    const INITIALIZE_VER: u32 = struct_version(5, true);
    const CREATE_BITSTREAM_VER: u32 = struct_version(1, false);
    const REGISTER_RESOURCE_VER: u32 = struct_version(3, false);
    const MAP_INPUT_VER: u32 = struct_version(4, false);
    const PIC_PARAMS_VER: u32 = struct_version(4, true);
    const LOCK_BITSTREAM_VER: u32 = struct_version(1, false);
    const RECONFIGURE_VER: u32 = struct_version(1, true);
    const CODEC_AV1: Option<sys::GUID> = None;
    const PROFILE_AV1_MAIN: Option<sys::GUID> = None;

    fn av1_config(_config: &mut sys::NV_ENC_CONFIG) -> Result<(), String> {
        Err("AV1 needs NVENC API 12".into())
    }

    include!("nvenc_session.rs");
}

/// API 12.0 (nvEncodeAPI.h of nv-codec-headers n12.0.16.1): H.264, H.265 and AV1.
pub(crate) mod v12 {
    use crate::nvenc_sys12 as sys;

    // Struct versions of SDK 12.0.
    const FUNCTION_LIST_VER: u32 = struct_version(2, false);
    const OPEN_SESSION_VER: u32 = struct_version(1, false);
    const PRESET_CONFIG_VER: u32 = struct_version(4, true);
    const CONFIG_VER: u32 = struct_version(8, true);
    const INITIALIZE_VER: u32 = struct_version(5, true);
    const CREATE_BITSTREAM_VER: u32 = struct_version(1, false);
    const REGISTER_RESOURCE_VER: u32 = struct_version(4, false);
    const MAP_INPUT_VER: u32 = struct_version(4, false);
    const PIC_PARAMS_VER: u32 = struct_version(6, true);
    const LOCK_BITSTREAM_VER: u32 = struct_version(2, false);
    const RECONFIGURE_VER: u32 = struct_version(1, true);
    const CODEC_AV1: Option<sys::GUID> = Some(guid(
        0x0a35_2289,
        0x0aa7,
        0x4759,
        [0x86, 0x2d, 0x5d, 0x15, 0xcd, 0x16, 0xd2, 0x54],
    ));
    const PROFILE_AV1_MAIN: Option<sys::GUID> = Some(guid(
        0x5f2a_39f5,
        0xf14e,
        0x4f95,
        [0x9a, 0x9e, 0xb7, 0x6d, 0x56, 0x8f, 0xcf, 0x97],
    ));

    /// AV1 Main, 8-bit 4:2:0 from NV12: low-overhead OBUs (what decoders and AV1's RTP payload take), an endless GOP,
    /// the sequence header with every keyframe, BT.709 limited range.
    fn av1_config(config: &mut sys::NV_ENC_CONFIG) -> Result<(), String> {
        let av1 = unsafe { &mut config.encodeCodecConfig.av1Config };
        av1.idrPeriod = INFINITE_GOP;
        av1.set_repeatSeqHdr(1);
        av1.set_outputAnnexBFormat(0);
        av1.set_chromaFormatIDC(1);
        av1.set_inputPixelBitDepthMinus8(0);
        av1.set_pixelBitDepthMinus8(0);
        av1.colorPrimaries = sys::NV_ENC_VUI_COLOR_PRIMARIES_BT709;
        av1.transferCharacteristics = sys::NV_ENC_VUI_TRANSFER_CHARACTERISTIC_BT709;
        av1.matrixCoefficients = sys::NV_ENC_VUI_MATRIX_COEFFS_BT709;
        av1.colorRange = 0;
        Ok(())
    }

    include!("nvenc_session.rs");
}

/// One NVENC session, on whichever API version the driver has.
pub(crate) enum NvencCore {
    V11(v11::Session),
    V12(v12::Session),
}

impl NvencCore {
    /// Opens a session on `device` (a D3D11 device, or a CUDA context, as `device_type` says) through the driver's
    /// `create` and `max_version` entry points: API 12 where the driver has it, else 11.1 (no AV1 there).
    pub(crate) fn open(
        create: CreateInstance,
        max_version: MaxVersion,
        device_type: u32,
        device: *mut c_void,
        config: &EncoderConfig,
        format: EncodeFormat,
    ) -> Result<Self, String> {
        supports(format)?;
        let av1 = config.codec == crate::VideoCodec::Av1;
        if av1 && (format.chroma444 || format.ten_bit) {
            return Err("NVENC makes AV1 in 8-bit 4:2:0 here".into());
        }
        let mut supported = 0u32;
        let code = unsafe { max_version(&mut supported) };
        if code != 0 {
            return Err(format!("NVENC driver version: status {code}"));
        }
        let (major, minor) = (supported >> 4, supported & 0xF);
        let forced_11 = std::env::var("QUEQIAO_NVENC_API").is_ok_and(|v| v == "11");
        if (major, minor) >= (12, 0) && !forced_11 {
            v12::Session::open(create, device_type, device, config, format).map(Self::V12)
        } else if av1 {
            Err(if forced_11 {
                "AV1 needs NVENC API 12 (QUEQIAO_NVENC_API=11 keeps this PC on 11.1)".to_string()
            } else {
                format!(
                    "AV1 needs NVENC API 12 (NVIDIA driver 522 or later on Windows, 520 on Linux); this driver has \
                     {major}.{minor}"
                )
            })
        } else if (major, minor) >= (11, 1) {
            v11::Session::open(create, device_type, device, config, format).map(Self::V11)
        } else {
            Err(format!(
                "the NVIDIA driver supports NVENC API {major}.{minor}, Queqiao needs 11.1 (driver R470) or later"
            ))
        }
    }

    pub(crate) fn name(&self) -> &str {
        match self {
            Self::V11(session) => &session.name,
            Self::V12(session) => &session.name,
        }
    }

    /// Adds `more` to the name (the platform layer's part: the GPU, CUDA).
    #[cfg(target_os = "linux")]
    pub(crate) fn add_to_name(&mut self, more: &str) {
        match self {
            Self::V11(session) => session.name.push_str(more),
            Self::V12(session) => session.name.push_str(more),
        }
    }

    pub(crate) fn format(&self) -> EncodeFormat {
        match self {
            Self::V11(session) => session.format,
            Self::V12(session) => session.format,
        }
    }

    pub(crate) fn poll(&mut self, out: &mut Vec<EncodedFrame>) {
        match self {
            Self::V11(session) => session.poll(out),
            Self::V12(session) => session.poll(out),
        }
    }

    /// The input registered under `key`, if it is (Windows: its textures come again and again).
    #[cfg(windows)]
    pub(crate) fn registered(&self, key: usize) -> Option<Registered> {
        match self {
            Self::V11(session) => session.registered(key),
            Self::V12(session) => session.registered(key),
        }
    }

    /// Registers an input under `key` the first time it is seen (see the session's `register`).
    pub(crate) fn register(
        &mut self,
        key: usize,
        resource_type: u32,
        resource: *mut c_void,
        size: (u32, u32),
        pitch: u32,
    ) -> Result<Registered, String> {
        match self {
            Self::V11(session) => session.register(key, resource_type, resource, size, pitch),
            Self::V12(session) => session.register(key, resource_type, resource, size, pitch),
        }
    }

    /// Encodes one registered input now; the frame is ready when this returns.
    pub(crate) fn encode(
        &mut self,
        input: Registered,
        time: i64,
        keyframe: bool,
    ) -> Result<(), String> {
        match self {
            Self::V11(session) => session.encode(input, time, keyframe),
            Self::V12(session) => session.encode(input, time, keyframe),
        }
    }

    /// Changes the bitrate of the running session (no keyframe, no reset).
    pub(crate) fn set_bitrate(&mut self, bitrate: u32) -> Result<(), String> {
        match self {
            Self::V11(session) => session.set_bitrate(bitrate),
            Self::V12(session) => session.set_bitrate(bitrate),
        }
    }
}
