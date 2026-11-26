//! Intel's encoders driven directly through oneVPL, as UU Remote does (and Queqiao does NVENC for NVIDIA): through
//! Media Foundation, Intel's Quick Sync encoder ignored bitrate changes in constant-bitrate mode and offers neither
//! H.265 4:4:4 nor 10-bit. oneVPL gives H.264 and H.265, 4:4:4 and 10-bit (H.265), the bitrate changed in place, and
//! keyframes on request.
//!
//! The runtime is the graphics driver's own: `libmfx64-gen.dll` in the folder the Intel display adapter's registry key
//! names (`DriverStorePathForVPL`), which must lie in Windows' driver store (only administrators write there).
//! Nothing of Intel's is shipped; the types come from oneVPL's headers (MIT), `vpl_sys.rs`.
//!
//! Frames: the runtime allocates its input surfaces on our Direct3D 11 device (oneVPL 2.x, `GetSurfaceForEncode`);
//! each frame's texture is copied into one on the GPU, encoded at once (one frame in flight, no B-frames).

use std::mem::zeroed;
use std::ptr::null_mut;

use windows::Win32::Foundation::{E_FAIL, FreeLibrary, HMODULE};
use windows::Win32::Graphics::Direct3D10::ID3D10Multithread;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_TEXTURE2D_DESC, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
};
use windows::Win32::System::LibraryLoader::{
    GetProcAddress, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW,
};
use windows::Win32::System::Registry::{
    HKEY, HKEY_LOCAL_MACHINE, KEY_READ, REG_VALUE_TYPE, RegCloseKey, RegEnumKeyExW, RegOpenKeyExW,
    RegQueryValueExW,
};
use windows::Win32::System::SystemInformation::GetSystemDirectoryW;
use windows::core::{Error, HSTRING, Interface, PCSTR, PCWSTR, PWSTR, Result, s, w};

use crate::encoder::{EncodedFrame, EncoderConfig};
use crate::format::{Codec, EncodeFormat};
use crate::vpl_sys as sys;

type Initialize =
    unsafe extern "C" fn(sys::mfxInitializationParam, *mut sys::mfxSession) -> sys::mfxStatus;
type Close = unsafe extern "C" fn(sys::mfxSession) -> sys::mfxStatus;
type SetHandle =
    unsafe extern "C" fn(sys::mfxSession, sys::mfxHandleType, sys::mfxHDL) -> sys::mfxStatus;
type Query = unsafe extern "C" fn(
    sys::mfxSession,
    *mut sys::mfxVideoParam,
    *mut sys::mfxVideoParam,
) -> sys::mfxStatus;
type WithParams = unsafe extern "C" fn(sys::mfxSession, *mut sys::mfxVideoParam) -> sys::mfxStatus;
type EncodeFrame = unsafe extern "C" fn(
    sys::mfxSession,
    *mut sys::mfxEncodeCtrl,
    *mut sys::mfxFrameSurface1,
    *mut sys::mfxBitstream,
    *mut sys::mfxSyncPoint,
) -> sys::mfxStatus;
type Sync = unsafe extern "C" fn(sys::mfxSession, sys::mfxSyncPoint, u32) -> sys::mfxStatus;
type GetSurface =
    unsafe extern "C" fn(sys::mfxSession, *mut *mut sys::mfxFrameSurface1) -> sys::mfxStatus;

/// What GetProcAddress returns.
pub(crate) type Proc = unsafe extern "system" fn() -> isize;

struct Api {
    initialize: Initialize,
    close: Close,
    set_handle: SetHandle,
    query: Query,
    init: WithParams,
    reset: WithParams,
    get_video_param: WithParams,
    encode_close: Close,
    encode_frame: EncodeFrame,
    sync: Sync,
    get_surface: GetSurface,
}

impl Api {
    fn load(library: HMODULE) -> Result<Api> {
        let get = |name: PCSTR| -> Result<Proc> {
            unsafe { GetProcAddress(library, name) }.ok_or_else(|| {
                Error::new(
                    E_FAIL,
                    format!("the Intel runtime has no {}", unsafe { name.display() }),
                )
            })
        };
        // Each a function of the runtime with the signature of oneVPL's header (the types above).
        unsafe {
            Ok(Api {
                initialize: std::mem::transmute::<Proc, Initialize>(get(s!("MFXInitialize"))?),
                close: std::mem::transmute::<Proc, Close>(get(s!("MFXClose"))?),
                set_handle: std::mem::transmute::<Proc, SetHandle>(get(s!(
                    "MFXVideoCORE_SetHandle"
                ))?),
                query: std::mem::transmute::<Proc, Query>(get(s!("MFXVideoENCODE_Query"))?),
                init: std::mem::transmute::<Proc, WithParams>(get(s!("MFXVideoENCODE_Init"))?),
                reset: std::mem::transmute::<Proc, WithParams>(get(s!("MFXVideoENCODE_Reset"))?),
                get_video_param: std::mem::transmute::<Proc, WithParams>(get(s!(
                    "MFXVideoENCODE_GetVideoParam"
                ))?),
                encode_close: std::mem::transmute::<Proc, Close>(get(s!("MFXVideoENCODE_Close"))?),
                encode_frame: std::mem::transmute::<Proc, EncodeFrame>(get(s!(
                    "MFXVideoENCODE_EncodeFrameAsync"
                ))?),
                sync: std::mem::transmute::<Proc, Sync>(get(s!("MFXVideoCORE_SyncOperation"))?),
                get_surface: std::mem::transmute::<Proc, GetSurface>(get(s!(
                    "MFXMemory_GetSurfaceForEncode"
                ))?),
            })
        }
    }
}

/// The folders Intel's display drivers name for their oneVPL runtime (DriverStorePathForVPL in each display
/// adapter's key), only those inside Windows' driver store.
fn runtime_folders() -> Vec<String> {
    const CLASS: PCWSTR =
        w!(r"SYSTEM\CurrentControlSet\Control\Class\{4d36e968-e325-11ce-bfc1-08002be10318}");
    let mut system = [0u16; 260];
    let length = unsafe { GetSystemDirectoryW(Some(&mut system)) } as usize;
    let store = format!(
        "{}\\DriverStore\\",
        String::from_utf16_lossy(&system[..length.min(system.len())])
    )
    .to_ascii_lowercase();
    let mut folders = Vec::new();
    let mut class = HKEY::default();
    if unsafe { RegOpenKeyExW(HKEY_LOCAL_MACHINE, CLASS, None, KEY_READ, &mut class) }.is_err() {
        return folders;
    }
    for index in 0..64u32 {
        let mut name = [0u16; 64];
        let mut name_length = name.len() as u32;
        let found = unsafe {
            RegEnumKeyExW(
                class,
                index,
                Some(PWSTR(name.as_mut_ptr())),
                &mut name_length,
                None,
                None,
                None,
                None,
            )
        };
        if found.is_err() {
            break;
        }
        let mut adapter = HKEY::default();
        let sub = HSTRING::from_wide(&name[..name_length as usize]);
        if unsafe { RegOpenKeyExW(class, &sub, None, KEY_READ, &mut adapter) }.is_err() {
            continue;
        }
        let mut value = [0u16; 520];
        let mut size = (value.len() * 2) as u32;
        let mut kind = REG_VALUE_TYPE::default();
        let read = unsafe {
            RegQueryValueExW(
                adapter,
                w!("DriverStorePathForVPL"),
                None,
                Some(&mut kind),
                Some(value.as_mut_ptr().cast()),
                Some(&mut size),
            )
        };
        let _ = unsafe { RegCloseKey(adapter) };
        if read.is_err() {
            continue;
        }
        // REG_SZ or REG_MULTI_SZ: the first string.
        let chars = (size as usize / 2).min(value.len());
        let end = value[..chars].iter().position(|c| *c == 0).unwrap_or(chars);
        let folder = String::from_utf16_lossy(&value[..end]);
        if folder.to_ascii_lowercase().starts_with(&store) && !folders.contains(&folder) {
            folders.push(folder);
        }
    }
    let _ = unsafe { RegCloseKey(class) };
    folders
}

/// Intel's oneVPL runtime from the graphics driver.
pub(crate) fn load_runtime() -> Result<HMODULE> {
    let folders = runtime_folders();
    if folders.is_empty() {
        return Err(Error::new(
            E_FAIL,
            "no Intel graphics driver with a oneVPL runtime (DriverStorePathForVPL)",
        ));
    }
    let mut last = Error::new(E_FAIL, "no oneVPL runtime");
    for folder in folders {
        let path = HSTRING::from(format!("{folder}\\libmfx64-gen.dll"));
        match unsafe {
            LoadLibraryExW(
                &path,
                None,
                LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32,
            )
        } {
            Ok(library) => return Ok(library),
            Err(error) => last = error,
        }
    }
    Err(last)
}

pub(crate) fn check(status: sys::mfxStatus, what: &str) -> Result<sys::mfxStatus> {
    if status >= 0 {
        Ok(status)
    } else {
        Err(Error::new(
            E_FAIL,
            format!("oneVPL {what}: status {status}"),
        ))
    }
}

/// mfxExtEncoderResetOption (mfxstructures.h; not in the generated bindings): whether a reset starts a new sequence.
#[repr(C)]
struct ResetOption {
    header: sys::mfxExtBuffer,
    start_new_sequence: u16,
    reserved: [u16; 11],
}

/// The coding options the encoder is opened with: no HRD conformance (no buffering parameters in the stream's
/// headers). With them, a new bitrate changed the headers, so a reset could only begin a new sequence (refused
/// with StartNewSequence off: status -15, sunshine 2026-11-26).
fn coding_option() -> Box<sys::mfxExtCodingOption> {
    let mut option: Box<sys::mfxExtCodingOption> = Box::new(unsafe { zeroed() });
    option.Header = sys::mfxExtBuffer {
        BufferId: sys::MFX_EXTBUFF_CODING_OPTION as u32,
        BufferSz: size_of::<sys::mfxExtCodingOption>() as u32,
    };
    option.NalHrdConformance = sys::MFX_CODINGOPTION_OFF as u16;
    option.VuiNalHrdParameters = sys::MFX_CODINGOPTION_OFF as u16;
    option
}

/// kbit/s for the 16-bit fields, with the multiplier the parameters carry.
fn kbps(bits: u32, multiplier: u16) -> u16 {
    (bits / 1000 / u32::from(multiplier.max(1))).clamp(1, u32::from(u16::MAX)) as u16
}

/// The peak bitrate, as a multiple of the mean (as the Media Foundation path).
const PEAK: f64 = 1.5;

pub(crate) struct VplEncoder {
    library: HMODULE,
    api: Api,
    session: sys::mfxSession,
    params: sys::mfxVideoParam,
    /// The coding options given with `params` (their ExtParam points here while the encoder lives).
    coding: Box<sys::mfxExtCodingOption>,
    extra: Box<[*mut sys::mfxExtBuffer; 1]>,
    context: ID3D11DeviceContext,
    buffer: Vec<u8>,
    ready: Vec<EncodedFrame>,
    pub(crate) name: String,
    pub(crate) format: EncodeFormat,
}

impl VplEncoder {
    pub(crate) fn open(
        device: &ID3D11Device,
        config: &EncoderConfig,
        format: EncodeFormat,
    ) -> Result<VplEncoder> {
        if format.codec == Codec::H264 && (format.chroma444 || format.ten_bit) {
            return Err(Error::new(
                E_FAIL,
                "Intel's encoders make H.264 in 4:2:0 8-bit only",
            ));
        }
        let library = load_runtime()?;
        let api = match Api::load(library) {
            Ok(api) => api,
            Err(error) => {
                let _ = unsafe { FreeLibrary(library) };
                return Err(error);
            }
        };
        let mut encoder = VplEncoder {
            library,
            api,
            session: null_mut(),
            params: unsafe { zeroed() },
            coding: coding_option(),
            extra: Box::new([null_mut()]),
            context: unsafe { device.GetImmediateContext() }?,
            buffer: Vec::new(),
            ready: Vec::new(),
            name: format!("Intel oneVPL {} (hardware)", format.name()),
            format,
        };
        encoder.start(device, config)?;
        Ok(encoder)
    }

    fn start(&mut self, device: &ID3D11Device, config: &EncoderConfig) -> Result<()> {
        let mut init: sys::mfxInitializationParam = unsafe { zeroed() };
        init.AccelerationMode = sys::MFX_ACCEL_MODE_VIA_D3D11;
        check(
            unsafe { (self.api.initialize)(init, &mut self.session) },
            "initialize",
        )?;
        // The runtime uses the device's context from its own threads too.
        if let Ok(multithread) = device.cast::<ID3D10Multithread>() {
            let _ = unsafe { multithread.SetMultithreadProtected(true) };
        }
        check(
            unsafe {
                (self.api.set_handle)(
                    self.session,
                    sys::MFX_HANDLE_D3D11_DEVICE,
                    device.as_raw() as sys::mfxHDL,
                )
            },
            "set the D3D11 device",
        )?;
        self.params = params(config, self.format);
        self.extra[0] = (&raw mut *self.coding).cast::<sys::mfxExtBuffer>();
        self.params.ExtParam = self.extra.as_mut_ptr();
        self.params.NumExtParam = 1;
        let mut checked = self.params;
        check(
            unsafe { (self.api.query)(self.session, &mut self.params, &mut checked) },
            "query",
        )?;
        self.params = checked;
        check(
            unsafe { (self.api.init)(self.session, &mut self.params) },
            "init",
        )?;
        let mut got: sys::mfxVideoParam = unsafe { zeroed() };
        check(
            unsafe { (self.api.get_video_param)(self.session, &mut got) },
            "get video param",
        )?;
        let mfx = unsafe { got.__bindgen_anon_1.mfx };
        let buffer_kb = u32::from(unsafe { mfx.__bindgen_anon_1.__bindgen_anon_1.BufferSizeInKB })
            * u32::from(mfx.BRCParamMultiplier.max(1));
        // Room for a keyframe at least: the encoder's buffer size, or 4 bytes a pixel.
        let size = (buffer_kb as usize * 1000).max((config.width * config.height * 4) as usize);
        self.buffer = vec![0u8; size];
        Ok(())
    }

    pub(crate) fn poll(&mut self, out: &mut Vec<EncodedFrame>) {
        out.append(&mut self.ready);
    }

    /// Encodes one frame now (`input`: NV12, or the screen's BGRA for 4:4:4); the frame is ready when this returns.
    pub(crate) fn encode(
        &mut self,
        input: &ID3D11Texture2D,
        time: i64,
        keyframe: bool,
    ) -> Result<()> {
        let mut surface: *mut sys::mfxFrameSurface1 = null_mut();
        check(
            unsafe { (self.api.get_surface)(self.session, &mut surface) },
            "surface for encode",
        )?;
        let result = self.encode_surface(surface, input, time, keyframe);
        unsafe {
            let interface = (*surface).__bindgen_anon_1.FrameInterface;
            if let Some(release) = interface.as_ref().and_then(|i| i.Release) {
                release(surface);
            }
        }
        result
    }

    fn encode_surface(
        &mut self,
        surface: *mut sys::mfxFrameSurface1,
        input: &ID3D11Texture2D,
        time: i64,
        keyframe: bool,
    ) -> Result<()> {
        // The surface's texture, on our device: the frame copied into it on the GPU.
        let mut handle: sys::mfxHDL = null_mut();
        let mut kind: sys::mfxResourceType = 0;
        unsafe {
            let interface = (*surface).__bindgen_anon_1.FrameInterface;
            let native = interface
                .as_ref()
                .and_then(|i| i.GetNativeHandle)
                .ok_or_else(|| Error::new(E_FAIL, "oneVPL surface without a native handle"))?;
            check(native(surface, &mut handle, &mut kind), "native handle")?;
        }
        if kind != sys::MFX_RESOURCE_DX11_TEXTURE || handle.is_null() {
            return Err(Error::new(
                E_FAIL,
                format!("oneVPL surface of resource type {kind}, not a D3D11 texture"),
            ));
        }
        let target = unsafe { ID3D11Texture2D::from_raw_borrowed(&handle) }
            .ok_or_else(|| Error::new(E_FAIL, "oneVPL surface texture"))?;
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { target.GetDesc(&mut desc) };
        if desc.ArraySize != 1 {
            return Err(Error::new(
                E_FAIL,
                format!("oneVPL surface in a texture array of {}", desc.ArraySize),
            ));
        }
        unsafe {
            self.context
                .CopySubresourceRegion(target, 0, 0, 0, 0, input, 0, None);
            // 90 kHz, as RTP.
            (*surface).Data.TimeStamp = (time.max(0) as u64) * 9 / 1000;
        }
        let mut control: sys::mfxEncodeCtrl = unsafe { zeroed() };
        let control_pointer = if keyframe {
            control.FrameType =
                (sys::MFX_FRAMETYPE_I | sys::MFX_FRAMETYPE_IDR | sys::MFX_FRAMETYPE_REF) as u16;
            &mut control as *mut sys::mfxEncodeCtrl
        } else {
            null_mut()
        };
        let mut bitstream: sys::mfxBitstream = unsafe { zeroed() };
        bitstream.Data = self.buffer.as_mut_ptr();
        bitstream.MaxLength = self.buffer.len() as u32;
        let mut sync: sys::mfxSyncPoint = null_mut();
        let mut status;
        let mut busy = 0;
        loop {
            status = unsafe {
                (self.api.encode_frame)(
                    self.session,
                    control_pointer,
                    surface,
                    &mut bitstream,
                    &mut sync,
                )
            };
            if status == sys::MFX_WRN_DEVICE_BUSY && busy < 100 {
                busy += 1;
                std::thread::sleep(std::time::Duration::from_millis(1));
                continue;
            }
            break;
        }
        if status == sys::MFX_ERR_MORE_DATA {
            return Ok(());
        }
        check(status, "encode")?;
        if sync.is_null() {
            return Ok(());
        }
        check(unsafe { (self.api.sync)(self.session, sync, 1000) }, "sync")?;
        let start = bitstream.DataOffset as usize;
        let end = (start + bitstream.DataLength as usize).min(self.buffer.len());
        let keyframe =
            i32::from(bitstream.FrameType) & (sys::MFX_FRAMETYPE_IDR | sys::MFX_FRAMETYPE_I) != 0;
        self.ready.push(EncodedFrame {
            data: self.buffer[start..end].to_vec(),
            time,
            keyframe,
        });
        Ok(())
    }

    /// Changes the bitrate of the running encoder: a reset of its rate control that goes on with the same sequence
    /// (StartNewSequence off). Without it the reset began a new sequence with a keyframe (measured on sunshine's Iris
    /// Xe 2026-11-26: every change, up or down, one more keyframe; in a session over a phone's network each one is many
    /// times a frame's size and held the picture up).
    pub(crate) fn set_bitrate(&mut self, bitrate: u32) -> Result<()> {
        unsafe {
            let mfx = &mut self.params.__bindgen_anon_1.mfx;
            let multiplier = mfx.BRCParamMultiplier;
            let settings = &mut mfx.__bindgen_anon_1.__bindgen_anon_1;
            settings.__bindgen_anon_2.TargetKbps = kbps(bitrate, multiplier);
            settings.__bindgen_anon_3.MaxKbps =
                kbps((f64::from(bitrate) * PEAK) as u32, multiplier);
        }
        let mut option = ResetOption {
            header: sys::mfxExtBuffer {
                BufferId: sys::MFX_EXTBUFF_ENCODER_RESET_OPTION as u32,
                BufferSz: size_of::<ResetOption>() as u32,
            },
            start_new_sequence: sys::MFX_CODINGOPTION_OFF as u16,
            reserved: [0; 11],
        };
        let mut buffers = [
            (&raw mut *self.coding).cast::<sys::mfxExtBuffer>(),
            (&raw mut option).cast::<sys::mfxExtBuffer>(),
        ];
        self.params.ExtParam = buffers.as_mut_ptr();
        self.params.NumExtParam = 2;
        let status = unsafe { (self.api.reset)(self.session, &mut self.params) };
        self.params.ExtParam = self.extra.as_mut_ptr();
        self.params.NumExtParam = 1;
        check(status, "reset (bitrate)").map(|_| ())
    }
}

/// The encoder's parameters: low latency (one frame in flight, no B-frames, the GPU's own low-power encoder),
/// peak-constrained VBR as the Media Foundation path. H.264 on the fastest preset (7); H.265 on the balanced one (4):
/// on 7 its motion search reaches too short for a quick drag. Measured 2026-11-26 on sunshine's Iris Xe (codec_probe,
/// 2240x1400 at 5 Mbit/s, the page moving 7/28/56 rows a frame): H.265 on 7 47.8/37.1/30.7 dB, on 4 48.2/48.9/41.0, on 1
/// 48.4/50.0/46.5 (H.264 on 7 38.5/40.5/39.6, on 4 under 1 dB more); screen change -> sent in a session 12.8 ms
/// median on 7, 14.6 on 4 (H.264 on 7: 14.1), 15.8 on 1 with fewer frames.
fn params(config: &EncoderConfig, format: EncodeFormat) -> sys::mfxVideoParam {
    let mut params: sys::mfxVideoParam = unsafe { zeroed() };
    params.AsyncDepth = 1;
    params.IOPattern = sys::MFX_IOPATTERN_IN_VIDEO_MEMORY as u16;
    let align = |value: u32, to: u32| value.div_ceil(to) * to;
    unsafe {
        let mfx = &mut params.__bindgen_anon_1.mfx;
        mfx.CodecId = match format.codec {
            Codec::H264 => sys::MFX_CODEC_AVC,
            Codec::H265 => sys::MFX_CODEC_HEVC,
        } as u32;
        mfx.CodecProfile = match (format.codec, format.chroma444, format.ten_bit) {
            (Codec::H264, ..) => sys::MFX_PROFILE_AVC_HIGH,
            (Codec::H265, true, _) => sys::MFX_PROFILE_HEVC_REXT,
            (Codec::H265, false, true) => sys::MFX_PROFILE_HEVC_MAIN10,
            (Codec::H265, false, false) => sys::MFX_PROFILE_HEVC_MAIN,
        } as u16;
        // Test switches (QUEQIAO_FORCE): vpl_lowpower:off, vpl_tu:1..7 (1 best quality, 7 best speed), to compare
        // the presets on the same content.
        mfx.LowPower = if queqiao_core::forced("vpl_lowpower").as_deref() == Some("off") {
            sys::MFX_CODINGOPTION_OFF
        } else {
            sys::MFX_CODINGOPTION_ON
        } as u16;
        // Room for up to 200 Mbit/s at the peak in the 16-bit kbit/s fields.
        let most = (config.bitrate.max(200_000_000) as f64 * PEAK) as u32 / 1000;
        mfx.BRCParamMultiplier = most.div_ceil(u32::from(u16::MAX)).max(1) as u16;
        let multiplier = mfx.BRCParamMultiplier;
        let settings = &mut mfx.__bindgen_anon_1.__bindgen_anon_1;
        settings.TargetUsage = queqiao_core::forced("vpl_tu")
            .and_then(|tu| tu.parse::<u16>().ok())
            .filter(|tu| (1..=7).contains(tu))
            .unwrap_or(match format.codec {
                Codec::H264 => sys::MFX_TARGETUSAGE_BEST_SPEED,
                Codec::H265 => sys::MFX_TARGETUSAGE_BALANCED,
            } as u16);
        settings.GopPicSize = (config.fps * 10).min(u32::from(u16::MAX)) as u16;
        settings.GopRefDist = 1;
        settings.IdrInterval = 0;
        settings.NumRefFrame = 1;
        settings.RateControlMethod = sys::MFX_RATECONTROL_VBR as u16;
        settings.__bindgen_anon_2.TargetKbps = kbps(config.bitrate, multiplier);
        settings.__bindgen_anon_3.MaxKbps =
            kbps((f64::from(config.bitrate) * PEAK) as u32, multiplier);
        let info = &mut mfx.FrameInfo;
        let (fourcc, chroma) = match (format.chroma444, format.ten_bit) {
            (true, false) => (sys::MFX_FOURCC_AYUV, sys::MFX_CHROMAFORMAT_YUV444),
            (true, true) => (sys::MFX_FOURCC_Y410, sys::MFX_CHROMAFORMAT_YUV444),
            (false, true) => (sys::MFX_FOURCC_P010, sys::MFX_CHROMAFORMAT_YUV420),
            (false, false) => (sys::MFX_FOURCC_NV12, sys::MFX_CHROMAFORMAT_YUV420),
        };
        info.FourCC = fourcc as u32;
        info.ChromaFormat = chroma as u16;
        if format.ten_bit {
            info.BitDepthLuma = 10;
            info.BitDepthChroma = 10;
            // P010 keeps its 10 bits in the high bits of each 16; Y410 packs them.
            info.Shift = u16::from(!format.chroma444);
        }
        info.FrameRateExtN = config.fps.max(1);
        info.FrameRateExtD = 1;
        info.PicStruct = sys::MFX_PICSTRUCT_PROGRESSIVE as u16;
        let size = &mut info.__bindgen_anon_1.__bindgen_anon_1;
        size.Width = align(config.width, 16) as u16;
        size.Height = align(config.height, 16) as u16;
        size.CropW = config.width as u16;
        size.CropH = config.height as u16;
    }
    params
}

impl Drop for VplEncoder {
    fn drop(&mut self) {
        unsafe {
            if !self.session.is_null() {
                let _ = (self.api.encode_close)(self.session);
                let _ = (self.api.close)(self.session);
            }
            let _ = FreeLibrary(self.library);
        }
    }
}

// The raw pointers are only used from the thread that owns the encoder, as with NVENC.
unsafe impl Send for VplEncoder {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bitrate_fits_the_16_bit_fields() {
        let config = EncoderConfig::for_screen(2240, 1400, 60);
        let params = params(&config, EncodeFormat::H264);
        let mfx = unsafe { params.__bindgen_anon_1.mfx };
        let target = u32::from(unsafe {
            mfx.__bindgen_anon_1
                .__bindgen_anon_1
                .__bindgen_anon_2
                .TargetKbps
        }) * u32::from(mfx.BRCParamMultiplier);
        // Within a multiplier's step of the configured bitrate.
        let wanted = config.bitrate / 1000;
        assert!(
            target <= wanted && wanted - target < u32::from(mfx.BRCParamMultiplier),
            "{target} for {wanted}"
        );
        assert_eq!(kbps(300_000_000, 1), u16::MAX);
    }
}
