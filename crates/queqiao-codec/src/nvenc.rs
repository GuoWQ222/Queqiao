//! NVIDIA's encoder (NVENC) driven directly through the Video Codec SDK API, beside the Media Foundation
//! path, to compare the two (UU Remote uses the vendor SDKs directly). Built against API 11.1, which
//! drivers from R470 (mid-2021) on support: the newest header would need the newest driver (measured: a
//! March 2026 driver supports 13.0 and refused 13.1). The driver's `nvEncodeAPI64.dll` is loaded at run
//! time, so the program still starts on PCs without NVIDIA graphics.
//!
//! Formats (`EncodeFormat`): H.264 4:2:0 or 4:4:4, H.265 4:2:0 or 4:4:4, H.265 in 8 or 10 bits; 4:4:4 from RGB
//! textures, which NVENC converts to YUV 4:4:4 itself.
//!
//! Used synchronously: each input texture is registered once, then per frame mapped, encoded, and its
//! bitstream locked (which waits for the GPU). Settings follow NVIDIA's advice for game streaming:
//! preset P1 with the ultra-low-latency tuning, constant bitrate with a one-frame buffer, no B-frames, an
//! endless GOP with keyframes on request, SPS/PPS repeated with every keyframe, BT.709 limited range.

use std::ffi::c_void;
use std::mem::zeroed;
use std::ptr::null_mut;

use windows::Win32::Foundation::{E_FAIL, FreeLibrary, HMODULE};
use windows::Win32::Graphics::Direct3D11::{D3D11_TEXTURE2D_DESC, ID3D11Device, ID3D11Texture2D};
use windows::Win32::System::LibraryLoader::{
    GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW,
};
use windows::core::{Error, Interface, Result, s, w};

use crate::encoder::{EncodedFrame, EncoderConfig};
use crate::format::{Codec, EncodeFormat};
use crate::nvenc_sys as sys;
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_R10G10B10A2_UNORM,
};

const API_VERSION: u32 = sys::NVENCAPI_MAJOR_VERSION | (sys::NVENCAPI_MINOR_VERSION << 24);

/// NVENCAPI_STRUCT_VERSION(v) from the header; `extended` adds the 1 << 31 some structs carry.
const fn struct_version(version: u32, extended: bool) -> u32 {
    API_VERSION | (version << 16) | (7 << 28) | if extended { 1 << 31 } else { 0 }
}

// Struct versions of SDK 11.1 (nvEncodeAPI.h of nv-codec-headers n11.1.5.3); they change with the header.
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
const INFINITE_GOP: u32 = 0xFFFF_FFFF;
/// A keyframe may use this many times a normal frame's bits (see `start`).
const KEYFRAME_SCALE: u8 = 8;

/// NVENC presets P1 (fastest) .. P7 (best quality); `QUEQIAO_NVENC_PRESET=1..7` picks one for measurements.
const PRESETS: [sys::GUID; 7] = [
    PRESET_P1,
    guid(
        0xf581_cfb8,
        0x88d6,
        0x4381,
        [0x93, 0xf0, 0xdf, 0x13, 0xf9, 0xc2, 0x7d, 0xab],
    ),
    guid(
        0x3685_0110,
        0x3a07,
        0x441f,
        [0x94, 0xd5, 0x36, 0x70, 0x63, 0x1f, 0x91, 0xf6],
    ),
    guid(
        0x90a7_b826,
        0xdf06,
        0x4862,
        [0xb9, 0xd2, 0xcd, 0x6d, 0x73, 0xa0, 0x86, 0x81],
    ),
    guid(
        0x21c6_e6b4,
        0x297a,
        0x4cba,
        [0x99, 0x8f, 0xb6, 0xcb, 0xde, 0x72, 0xad, 0xe3],
    ),
    guid(
        0x8e75_c279,
        0x6299,
        0x4ab6,
        [0x83, 0x02, 0x0b, 0x21, 0x5a, 0x33, 0x5c, 0xf5],
    ),
    guid(
        0x8484_8c12,
        0x6f71,
        0x4c13,
        [0x93, 0x1b, 0x53, 0xe2, 0x83, 0xf5, 0x79, 0x74],
    ),
];

const fn guid(data1: u32, data2: u16, data3: u16, data4: [u8; 8]) -> sys::GUID {
    sys::GUID {
        Data1: data1,
        Data2: data2,
        Data3: data3,
        Data4: data4,
    }
}

/// The preset to use: P1 unless `QUEQIAO_NVENC_PRESET` names another (for measurements).
fn chosen_preset() -> (usize, sys::GUID) {
    let number = std::env::var("QUEQIAO_NVENC_PRESET")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|n| (1..=7).contains(n))
        .unwrap_or(1);
    (number, PRESETS[number - 1])
}

const CODEC_H264: sys::GUID = sys::GUID {
    Data1: 0x6bc8_2762,
    Data2: 0x4e63,
    Data3: 0x4ca4,
    Data4: [0xaa, 0x85, 0x1e, 0x50, 0xf3, 0x21, 0xf6, 0xbf],
};
const CODEC_HEVC: sys::GUID = guid(
    0x790c_dc88,
    0x4522,
    0x4d7b,
    [0x94, 0x25, 0xbd, 0xa9, 0x97, 0x5f, 0x76, 0x03],
);
const PROFILE_HIGH_444: sys::GUID = guid(
    0x7ac6_63cb,
    0xa598,
    0x4960,
    [0xb8, 0x44, 0x33, 0x9b, 0x26, 0x1a, 0x7d, 0x52],
);
const PROFILE_HEVC_MAIN: sys::GUID = guid(
    0xb514_c39a,
    0xb55b,
    0x40fa,
    [0x87, 0x8f, 0xf1, 0x25, 0x3b, 0x4d, 0xfd, 0xec],
);
const PROFILE_HEVC_MAIN10: sys::GUID = guid(
    0xfa4d_2b6c,
    0x3a5b,
    0x411a,
    [0x80, 0x18, 0x0a, 0x3f, 0x5e, 0x3c, 0x9b, 0xe5],
);
/// H.265 range extensions: 4:4:4.
const PROFILE_HEVC_FREXT: sys::GUID = guid(
    0x51ec_32b5,
    0x1b4c,
    0x453c,
    [0x9c, 0xbd, 0xb6, 0x16, 0xbd, 0x62, 0x13, 0x41],
);

/// The texture NVENC takes for `format`: YUV 4:2:0 from the converter (NV12, P010); for 4:4:4 the screen's RGB
/// (scaled only), which NVENC turns into YUV 4:4:4 itself (its D3D11 input has no packed 10-bit YUV 4:4:4).
fn input_format(format: EncodeFormat) -> DXGI_FORMAT {
    match (format.chroma444, format.ten_bit) {
        (true, false) => DXGI_FORMAT_B8G8R8A8_UNORM,
        (true, true) => DXGI_FORMAT_R10G10B10A2_UNORM,
        _ => format.input(),
    }
}

/// NVENC's name for that texture format: ARGB is DXGI's B8G8R8A8 (blue in the lowest bits of the word), ABGR10
/// DXGI's R10G10B10A2 (red in the lowest).
fn buffer_format(format: EncodeFormat) -> sys::NV_ENC_BUFFER_FORMAT {
    match (format.chroma444, format.ten_bit) {
        (false, false) => sys::NV_ENC_BUFFER_FORMAT_NV12,
        (false, true) => sys::NV_ENC_BUFFER_FORMAT_YUV420_10BIT,
        (true, false) => sys::NV_ENC_BUFFER_FORMAT_ARGB,
        (true, true) => sys::NV_ENC_BUFFER_FORMAT_ABGR10,
    }
}

const PRESET_P1: sys::GUID = sys::GUID {
    Data1: 0xfc0a_8d3e,
    Data2: 0x45f8,
    Data3: 0x4cf8,
    Data4: [0x80, 0xc7, 0x29, 0x88, 0x71, 0x59, 0x0e, 0xbf],
};
const PROFILE_HIGH: sys::GUID = sys::GUID {
    Data1: 0xe7cb_c309,
    Data2: 0x4f7a,
    Data3: 0x4b89,
    Data4: [0xaf, 0x2a, 0xd5, 0x37, 0xc9, 0x2b, 0xe3, 0x10],
};

type CreateInstance =
    unsafe extern "C" fn(*mut sys::NV_ENCODE_API_FUNCTION_LIST) -> sys::NVENCSTATUS;
/// NvEncodeAPIGetMaxSupportedVersion: the driver's highest API version as (major << 4) | minor.
type MaxVersion = unsafe extern "C" fn(*mut u32) -> sys::NVENCSTATUS;

pub(crate) struct NvencEncoder {
    library: HMODULE,
    api: Box<sys::NV_ENCODE_API_FUNCTION_LIST>,
    session: *mut c_void,
    bitstream: sys::NV_ENC_OUTPUT_PTR,
    /// Textures registered with NVENC, by their COM pointer (the textures stay alive in the caller's ring).
    registered: Vec<(usize, sys::NV_ENC_REGISTERED_PTR)>,
    size: (u32, u32),
    ready: Vec<EncodedFrame>,
    frame_index: u32,
    pub(crate) name: String,
    /// The settings the session was opened with; `init.encodeConfig` points at `encode_config` (both boxed, so
    /// the pointer stays valid), for changing the bitrate later.
    encode_config: Box<sys::NV_ENC_CONFIG>,
    init: Box<sys::NV_ENC_INITIALIZE_PARAMS>,
    fps: u32,
    pub(crate) format: EncodeFormat,
}

impl NvencEncoder {
    pub(crate) fn open(
        device: &ID3D11Device,
        config: &EncoderConfig,
        format: EncodeFormat,
    ) -> Result<Self> {
        if format.codec == Codec::H264 && format.ten_bit {
            return Err(Error::new(E_FAIL, "NVENC makes H.264 in 8 bits only"));
        }
        let library =
            unsafe { LoadLibraryExW(w!("nvEncodeAPI64.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32) }
                .map_err(|e| {
                    Error::new(e.code(), "no NVIDIA encoder driver (nvEncodeAPI64.dll)")
                })?;
        let mut encoder = Self {
            library,
            api: Box::new(unsafe { zeroed() }),
            session: null_mut(),
            bitstream: null_mut(),
            registered: Vec::new(),
            size: (config.width, config.height),
            ready: Vec::new(),
            frame_index: 0,
            encode_config: Box::new(unsafe { zeroed() }),
            init: Box::new(unsafe { zeroed() }),
            fps: config.fps.max(1),
            name: format!(
                "NVIDIA NVENC {} (direct, API {}.{})",
                format.name(),
                sys::NVENCAPI_MAJOR_VERSION,
                sys::NVENCAPI_MINOR_VERSION
            ),
            format,
        };
        // From here on, Drop cleans up whatever was created.
        encoder.start(device, config)?;
        Ok(encoder)
    }

    fn start(&mut self, device: &ID3D11Device, config: &EncoderConfig) -> Result<()> {
        // The driver must support the API version the bindings were made for.
        let max_version =
            unsafe { GetProcAddress(self.library, s!("NvEncodeAPIGetMaxSupportedVersion")) }
                .ok_or_else(|| Error::new(E_FAIL, "NvEncodeAPIGetMaxSupportedVersion missing"))?;
        let max_version: MaxVersion = unsafe { std::mem::transmute(max_version) };
        let mut supported = 0u32;
        status(unsafe { max_version(&mut supported) }, "driver version")?;
        let (major, minor) = (supported >> 4, supported & 0xF);
        let built_for = (sys::NVENCAPI_MAJOR_VERSION, sys::NVENCAPI_MINOR_VERSION);
        if (major, minor) < built_for {
            return Err(Error::new(
                E_FAIL,
                format!(
                    "the NVIDIA driver supports NVENC API {major}.{minor}, Queqiao is built for {}.{}",
                    built_for.0, built_for.1
                ),
            ));
        }
        let create = unsafe { GetProcAddress(self.library, s!("NvEncodeAPICreateInstance")) }
            .ok_or_else(|| Error::new(E_FAIL, "NvEncodeAPICreateInstance missing"))?;
        let create: CreateInstance = unsafe { std::mem::transmute(create) };
        self.api.version = FUNCTION_LIST_VER;
        status(unsafe { create(&mut *self.api) }, "API instance")?;

        let mut open: sys::NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS = unsafe { zeroed() };
        open.version = OPEN_SESSION_VER;
        open.deviceType = sys::NV_ENC_DEVICE_TYPE_DIRECTX;
        open.device = device.as_raw();
        open.apiVersion = API_VERSION;
        let open_session = function(self.api.nvEncOpenEncodeSessionEx)?;
        status(
            unsafe { open_session(&mut open, &mut self.session) },
            "open session (driver too old?)",
        )?;

        let mut preset: sys::NV_ENC_PRESET_CONFIG = unsafe { zeroed() };
        preset.version = PRESET_CONFIG_VER;
        preset.presetCfg.version = CONFIG_VER;
        let tuning = sys::NV_ENC_TUNING_INFO_ULTRA_LOW_LATENCY;
        let get_preset = function(self.api.nvEncGetEncodePresetConfigEx)?;
        let (preset_number, preset_guid) = chosen_preset();
        let codec = match self.format.codec {
            Codec::H264 => CODEC_H264,
            Codec::H265 => CODEC_HEVC,
        };
        let preset_status =
            unsafe { get_preset(self.session, codec, preset_guid, tuning, &mut preset) };
        self.check(preset_status, "preset")?;
        self.name = format!("{}, preset P{preset_number}", self.name);

        let mut encode_config = preset.presetCfg;
        encode_config.version = CONFIG_VER;
        encode_config.profileGUID = match (
            self.format.codec,
            self.format.chroma444,
            self.format.ten_bit,
        ) {
            (Codec::H264, false, _) => PROFILE_HIGH,
            (Codec::H264, true, _) => PROFILE_HIGH_444,
            (Codec::H265, true, _) => PROFILE_HEVC_FREXT,
            (Codec::H265, false, true) => PROFILE_HEVC_MAIN10,
            (Codec::H265, false, false) => PROFILE_HEVC_MAIN,
        };
        encode_config.gopLength = INFINITE_GOP;
        encode_config.frameIntervalP = 1;
        let rc = &mut encode_config.rcParams;
        rc.rateControlMode = sys::NV_ENC_PARAMS_RC_CBR;
        rc.averageBitRate = config.bitrate;
        rc.maxBitRate = config.bitrate;
        // A one-frame buffer: each frame gets about its share of the bitrate, so no frame waits.
        rc.vbvBufferSize = config.bitrate / config.fps.max(1);
        rc.vbvInitialDelay = rc.vbvBufferSize;
        rc.set_zeroReorderDelay(1);
        // With a one-frame buffer a keyframe may by default use only 2x a normal frame's bits (low-latency
        // tuning). Measured on synthetic screen content, 1080p, first-picture luma PSNR: 2x 19.1 dB, 4x
        // 27.1 dB, 8x 33.7 dB (16 is out of range and falls back to 2). Keyframes are rare (start, loss).
        rc.lowDelayKeyFrameScale = KEYFRAME_SCALE;
        let chroma_format_idc = if self.format.chroma444 { 3 } else { 1 };
        let vui = match self.format.codec {
            Codec::H264 => {
                let h264 = unsafe { &mut encode_config.encodeCodecConfig.h264Config };
                h264.idrPeriod = INFINITE_GOP;
                h264.set_repeatSPSPPS(1);
                h264.chromaFormatIDC = chroma_format_idc;
                &mut h264.h264VUIParameters
            }
            Codec::H265 => {
                let hevc = unsafe { &mut encode_config.encodeCodecConfig.hevcConfig };
                hevc.idrPeriod = INFINITE_GOP;
                hevc.set_repeatSPSPPS(1);
                hevc.set_chromaFormatIDC(chroma_format_idc);
                hevc.set_pixelBitDepthMinus8(if self.format.ten_bit { 2 } else { 0 });
                &mut hevc.hevcVUIParameters
            }
        };
        vui.videoSignalTypePresentFlag = 1;
        vui.videoFormat = 5;
        vui.videoFullRangeFlag = 0;
        vui.colourDescriptionPresentFlag = 1;
        vui.colourPrimaries = 1;
        vui.transferCharacteristics = 1;
        vui.colourMatrix = 1;

        *self.encode_config = encode_config;
        let mut init: sys::NV_ENC_INITIALIZE_PARAMS = unsafe { zeroed() };
        init.version = INITIALIZE_VER;
        init.encodeGUID = codec;
        init.presetGUID = preset_guid;
        init.encodeWidth = config.width;
        init.encodeHeight = config.height;
        init.darWidth = config.width;
        init.darHeight = config.height;
        init.frameRateNum = config.fps.max(1);
        init.frameRateDen = 1;
        init.enablePTD = 1;
        init.encodeConfig = &mut *self.encode_config;
        init.maxEncodeWidth = config.width;
        init.maxEncodeHeight = config.height;
        init.tuningInfo = tuning;
        init.bufferFormat = buffer_format(self.format);
        *self.init = init;
        let initialize = function(self.api.nvEncInitializeEncoder)?;
        let init_status = unsafe { initialize(self.session, &mut *self.init) };
        self.check(init_status, "initialize")?;

        let mut buffer: sys::NV_ENC_CREATE_BITSTREAM_BUFFER = unsafe { zeroed() };
        buffer.version = CREATE_BITSTREAM_VER;
        let create_buffer = function(self.api.nvEncCreateBitstreamBuffer)?;
        let buffer_status = unsafe { create_buffer(self.session, &mut buffer) };
        self.check(buffer_status, "bitstream buffer")?;
        self.bitstream = buffer.bitstreamBuffer;
        Ok(())
    }

    pub(crate) fn poll(&mut self, out: &mut Vec<EncodedFrame>) {
        out.append(&mut self.ready);
    }

    /// The texture format `encode` takes.
    pub(crate) fn input_format(&self) -> DXGI_FORMAT {
        input_format(self.format)
    }

    /// Encodes one texture (in `input_format`) now; the frame is ready when this returns.
    pub(crate) fn encode(
        &mut self,
        input: &ID3D11Texture2D,
        time: i64,
        keyframe: bool,
    ) -> Result<()> {
        let registered = self.register(input)?;
        let mut map: sys::NV_ENC_MAP_INPUT_RESOURCE = unsafe { zeroed() };
        map.version = MAP_INPUT_VER;
        map.registeredResource = registered;
        let map_input = function(self.api.nvEncMapInputResource)?;
        let map_status = unsafe { map_input(self.session, &mut map) };
        self.check(map_status, "map input")?;

        let mut picture: sys::NV_ENC_PIC_PARAMS = unsafe { zeroed() };
        picture.version = PIC_PARAMS_VER;
        picture.inputWidth = self.size.0;
        picture.inputHeight = self.size.1;
        picture.inputPitch = self.size.0;
        picture.frameIdx = self.frame_index;
        picture.inputTimeStamp = time.max(0) as u64;
        picture.inputBuffer = map.mappedResource;
        picture.outputBitstream = self.bitstream;
        picture.bufferFmt = map.mappedBufferFmt;
        picture.pictureStruct = sys::NV_ENC_PIC_STRUCT_FRAME;
        if keyframe {
            picture.encodePicFlags =
                (sys::NV_ENC_PIC_FLAG_FORCEIDR | sys::NV_ENC_PIC_FLAG_OUTPUT_SPSPPS) as u32;
        }
        self.frame_index += 1;
        let encode_picture = function(self.api.nvEncEncodePicture)?;
        let encoded = unsafe { encode_picture(self.session, &mut picture) };
        let result = if encoded == sys::NV_ENC_SUCCESS {
            self.take_bitstream()
        } else {
            self.check(encoded, "encode")
        };
        let unmap = function(self.api.nvEncUnmapInputResource)?;
        let _ = unsafe { unmap(self.session, map.mappedResource) };
        result
    }

    /// Changes the bitrate of the running session (no keyframe, no reset): CBR with a one-frame buffer as at
    /// the start.
    pub(crate) fn set_bitrate(&mut self, bitrate: u32) -> Result<()> {
        let rc = &mut self.encode_config.rcParams;
        rc.averageBitRate = bitrate;
        rc.maxBitRate = bitrate;
        rc.vbvBufferSize = bitrate / self.fps;
        rc.vbvInitialDelay = rc.vbvBufferSize;
        let mut params: sys::NV_ENC_RECONFIGURE_PARAMS = unsafe { zeroed() };
        params.version = RECONFIGURE_VER;
        params.reInitEncodeParams = *self.init;
        params.reInitEncodeParams.encodeConfig = &mut *self.encode_config;
        params.set_resetEncoder(0);
        params.set_forceIDR(0);
        let reconfigure = function(self.api.nvEncReconfigureEncoder)?;
        let reconfigure_status = unsafe { reconfigure(self.session, &mut params) };
        self.check(reconfigure_status, "reconfigure (bitrate)")
    }

    fn take_bitstream(&mut self) -> Result<()> {
        let mut lock: sys::NV_ENC_LOCK_BITSTREAM = unsafe { zeroed() };
        lock.version = LOCK_BITSTREAM_VER;
        lock.outputBitstream = self.bitstream;
        let lock_bitstream = function(self.api.nvEncLockBitstream)?;
        let lock_status = unsafe { lock_bitstream(self.session, &mut lock) };
        self.check(lock_status, "lock bitstream")?;
        let data = unsafe {
            std::slice::from_raw_parts(
                lock.bitstreamBufferPtr.cast::<u8>(),
                lock.bitstreamSizeInBytes as usize,
            )
        }
        .to_vec();
        let keyframe = matches!(
            lock.pictureType,
            sys::NV_ENC_PIC_TYPE_IDR | sys::NV_ENC_PIC_TYPE_I
        );
        let unlock = function(self.api.nvEncUnlockBitstream)?;
        let _ = unsafe { unlock(self.session, self.bitstream) };
        self.ready.push(EncodedFrame {
            data,
            time: lock.outputTimeStamp as i64,
            keyframe,
        });
        Ok(())
    }

    /// Registers a texture with NVENC the first time it is seen.
    fn register(&mut self, texture: &ID3D11Texture2D) -> Result<sys::NV_ENC_REGISTERED_PTR> {
        let key = texture.as_raw() as usize;
        if let Some(&(_, registered)) = self.registered.iter().find(|(k, _)| *k == key) {
            return Ok(registered);
        }
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { texture.GetDesc(&mut desc) };
        let mut resource: sys::NV_ENC_REGISTER_RESOURCE = unsafe { zeroed() };
        resource.version = REGISTER_RESOURCE_VER;
        resource.resourceType = sys::NV_ENC_INPUT_RESOURCE_TYPE_DIRECTX;
        resource.width = desc.Width;
        resource.height = desc.Height;
        resource.resourceToRegister = texture.as_raw();
        resource.bufferFormat = buffer_format(self.format);
        let register_resource = function(self.api.nvEncRegisterResource)?;
        let register_status = unsafe { register_resource(self.session, &mut resource) };
        self.check(register_status, "register texture")?;
        self.registered.push((key, resource.registeredResource));
        Ok(resource.registeredResource)
    }

    /// An error with NVENC's own explanation, if the session can give one.
    fn check(&self, code: sys::NVENCSTATUS, step: &str) -> Result<()> {
        if code == sys::NV_ENC_SUCCESS {
            return Ok(());
        }
        let detail = self
            .api
            .nvEncGetLastErrorString
            .filter(|_| !self.session.is_null())
            .map(|last_error| unsafe { last_error(self.session) })
            .filter(|text| !text.is_null())
            .map(|text| {
                unsafe { std::ffi::CStr::from_ptr(text) }
                    .to_string_lossy()
                    .into_owned()
            })
            .unwrap_or_default();
        Err(Error::new(
            E_FAIL,
            format!("NVENC {step}: status {code} {detail}"),
        ))
    }
}

impl Drop for NvencEncoder {
    fn drop(&mut self) {
        unsafe {
            if !self.session.is_null() {
                if let Some(unregister) = self.api.nvEncUnregisterResource {
                    for (_, registered) in self.registered.drain(..) {
                        let _ = unregister(self.session, registered);
                    }
                }
                if !self.bitstream.is_null()
                    && let Some(destroy_buffer) = self.api.nvEncDestroyBitstreamBuffer
                {
                    let _ = destroy_buffer(self.session, self.bitstream);
                }
                if let Some(destroy) = self.api.nvEncDestroyEncoder {
                    let _ = destroy(self.session);
                }
            }
            let _ = FreeLibrary(self.library);
        }
    }
}

fn status(code: sys::NVENCSTATUS, step: &str) -> Result<()> {
    if code == sys::NV_ENC_SUCCESS {
        Ok(())
    } else {
        Err(Error::new(E_FAIL, format!("NVENC {step}: status {code}")))
    }
}

/// A function from NVENC's table (missing ones mean a driver older than the API we were built for).
fn function<T>(entry: Option<T>) -> Result<T> {
    entry.ok_or_else(|| Error::new(E_FAIL, "NVENC function missing (driver too old?)"))
}
