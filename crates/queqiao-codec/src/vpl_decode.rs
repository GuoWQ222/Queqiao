//! Intel's decoders driven directly through oneVPL (the graphics driver's runtime, as for encoding, `vpl.rs`):
//! H.264 and H.265, 4:2:0 and 4:4:4, 8 and 10 bits, on the GPU. Each picture is copied out of the runtime's surface
//! into a ring of textures in the decoder's own output format (NV12, P010, AYUV, Y410), which the viewer's video
//! processor shows like NV12.
//!
//! Low latency: each call hands over one complete access unit (`MFX_BITSTREAM_COMPLETE_FRAME`) and gets its picture
//! back at once; nothing is reordered (the encoders make no B-frames). Before the first keyframe with parameter sets
//! the data is dropped (the viewer asks for a keyframe); a new size or format starts the decoder over.

use std::mem::zeroed;
use std::ptr::null_mut;

use windows::Win32::Foundation::{E_FAIL, FreeLibrary, HMODULE};
use windows::Win32::Graphics::Direct3D10::ID3D10Multithread;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BOX, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, ID3D11Device, ID3D11DeviceContext,
    ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT, DXGI_SAMPLE_DESC};
use windows::Win32::System::LibraryLoader::GetProcAddress;
use windows::core::{Error, Interface, PCSTR, Result, s};

use crate::decoder::{DecodedFrame, RING};
use crate::format::Codec;
use crate::vpl::{Proc, check, load_runtime};
use crate::vpl_sys as sys;

type Initialize =
    unsafe extern "C" fn(sys::mfxInitializationParam, *mut sys::mfxSession) -> sys::mfxStatus;
type Close = unsafe extern "C" fn(sys::mfxSession) -> sys::mfxStatus;
type SetHandle =
    unsafe extern "C" fn(sys::mfxSession, sys::mfxHandleType, sys::mfxHDL) -> sys::mfxStatus;
type DecodeHeader = unsafe extern "C" fn(
    sys::mfxSession,
    *mut sys::mfxBitstream,
    *mut sys::mfxVideoParam,
) -> sys::mfxStatus;
type WithParams = unsafe extern "C" fn(sys::mfxSession, *mut sys::mfxVideoParam) -> sys::mfxStatus;
type DecodeFrame = unsafe extern "C" fn(
    sys::mfxSession,
    *mut sys::mfxBitstream,
    *mut sys::mfxFrameSurface1,
    *mut *mut sys::mfxFrameSurface1,
    *mut sys::mfxSyncPoint,
) -> sys::mfxStatus;
type Sync = unsafe extern "C" fn(sys::mfxSession, sys::mfxSyncPoint, u32) -> sys::mfxStatus;

struct Api {
    initialize: Initialize,
    close: Close,
    set_handle: SetHandle,
    decode_header: DecodeHeader,
    init: WithParams,
    decode_close: Close,
    decode_frame: DecodeFrame,
    sync: Sync,
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
                decode_header: std::mem::transmute::<Proc, DecodeHeader>(get(s!(
                    "MFXVideoDECODE_DecodeHeader"
                ))?),
                init: std::mem::transmute::<Proc, WithParams>(get(s!("MFXVideoDECODE_Init"))?),
                decode_close: std::mem::transmute::<Proc, Close>(get(s!("MFXVideoDECODE_Close"))?),
                decode_frame: std::mem::transmute::<Proc, DecodeFrame>(get(s!(
                    "MFXVideoDECODE_DecodeFrameAsync"
                ))?),
                sync: std::mem::transmute::<Proc, Sync>(get(s!("MFXVideoCORE_SyncOperation"))?),
            })
        }
    }
}

/// A few textures of one size and format, handed out in turn.
struct Ring {
    textures: Vec<ID3D11Texture2D>,
    next: usize,
    shape: (u32, u32, DXGI_FORMAT),
}

impl Ring {
    fn next(
        &mut self,
        device: &ID3D11Device,
        shape: (u32, u32, DXGI_FORMAT),
    ) -> Result<ID3D11Texture2D> {
        if self.shape != shape || self.textures.is_empty() {
            let desc = D3D11_TEXTURE2D_DESC {
                Width: shape.0,
                Height: shape.1,
                MipLevels: 1,
                ArraySize: 1,
                Format: shape.2,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: 0,
                CPUAccessFlags: 0,
                MiscFlags: 0,
            };
            self.textures.clear();
            for _ in 0..RING {
                let mut texture: Option<ID3D11Texture2D> = None;
                unsafe { device.CreateTexture2D(&desc, None, Some(&mut texture)) }?;
                self.textures
                    .push(texture.ok_or_else(|| Error::new(E_FAIL, "decoded picture texture"))?);
            }
            self.shape = shape;
            self.next = 0;
        }
        let texture = self.textures[self.next].clone();
        self.next = (self.next + 1) % self.textures.len();
        Ok(texture)
    }
}

pub(crate) struct VplDecoder {
    library: HMODULE,
    api: Api,
    session: sys::mfxSession,
    codec: Codec,
    initialized: bool,
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    /// Bytes handed over but not yet taken by the decoder.
    pending: Vec<u8>,
    ring: Ring,
    pub(crate) name: String,
    /// What the stream turned out to be, once its headers were read ("H.265 4:4:4 10-bit").
    pub(crate) stream: String,
}

impl VplDecoder {
    pub(crate) fn open(
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
        codec: Codec,
    ) -> Result<VplDecoder> {
        let library = load_runtime()?;
        let api = match Api::load(library) {
            Ok(api) => api,
            Err(error) => {
                let _ = unsafe { FreeLibrary(library) };
                return Err(error);
            }
        };
        let mut decoder = VplDecoder {
            library,
            api,
            session: null_mut(),
            codec,
            initialized: false,
            device: device.clone(),
            context: context.clone(),
            pending: Vec::new(),
            ring: Ring {
                textures: Vec::new(),
                next: 0,
                shape: (0, 0, DXGI_FORMAT(0)),
            },
            name: format!(
                "Intel oneVPL {} decoder (hardware)",
                match codec {
                    Codec::H264 => "H.264",
                    Codec::H265 => "H.265",
                }
            ),
            stream: String::new(),
        };
        let mut init: sys::mfxInitializationParam = unsafe { zeroed() };
        init.AccelerationMode = sys::MFX_ACCEL_MODE_VIA_D3D11;
        check(
            unsafe { (decoder.api.initialize)(init, &mut decoder.session) },
            "initialize",
        )?;
        if let Ok(multithread) = device.cast::<ID3D10Multithread>() {
            let _ = unsafe { multithread.SetMultithreadProtected(true) };
        }
        check(
            unsafe {
                (decoder.api.set_handle)(
                    decoder.session,
                    sys::MFX_HANDLE_D3D11_DEVICE,
                    device.as_raw() as sys::mfxHDL,
                )
            },
            "set the D3D11 device",
        )?;
        Ok(decoder)
    }

    /// Decodes one access unit (Annex-B, as the encoder made it) and appends its picture to `out`.
    pub(crate) fn decode(
        &mut self,
        data: &[u8],
        time: i64,
        out: &mut Vec<DecodedFrame>,
    ) -> Result<()> {
        self.pending.extend_from_slice(data);
        let result = self.run(time, out);
        if result.is_err() {
            self.pending.clear();
        }
        result
    }

    fn run(&mut self, time: i64, out: &mut Vec<DecodedFrame>) -> Result<()> {
        let mut bitstream: sys::mfxBitstream = unsafe { zeroed() };
        bitstream.Data = self.pending.as_mut_ptr();
        bitstream.DataLength = self.pending.len() as u32;
        bitstream.MaxLength = self.pending.len() as u32;
        bitstream.DataFlag = sys::MFX_BITSTREAM_COMPLETE_FRAME as u16;
        if !self.initialized && !self.start(&mut bitstream)? {
            // No parameter sets yet: wait for a keyframe.
            self.pending.clear();
            return Ok(());
        }
        let mut restarted = false;
        let mut busy = 0;
        loop {
            let mut surface: *mut sys::mfxFrameSurface1 = null_mut();
            let mut sync: sys::mfxSyncPoint = null_mut();
            let status = unsafe {
                (self.api.decode_frame)(
                    self.session,
                    &mut bitstream,
                    null_mut(),
                    &mut surface,
                    &mut sync,
                )
            };
            match status {
                sys::MFX_WRN_DEVICE_BUSY if busy < 100 => {
                    busy += 1;
                    std::thread::sleep(std::time::Duration::from_millis(1));
                    continue;
                }
                sys::MFX_WRN_VIDEO_PARAM_CHANGED => continue,
                sys::MFX_ERR_MORE_DATA => break,
                sys::MFX_ERR_INCOMPATIBLE_VIDEO_PARAM if !restarted => {
                    // A new size or format: the decoder starts over from this access unit's headers.
                    restarted = true;
                    unsafe { (self.api.decode_close)(self.session) };
                    self.initialized = false;
                    if !self.start(&mut bitstream)? {
                        break;
                    }
                    continue;
                }
                _ => {}
            }
            check(status, "decode")?;
            if surface.is_null() || sync.is_null() {
                continue;
            }
            let copied = self.copy_out(surface, sync, time, out);
            unsafe {
                let interface = (*surface).__bindgen_anon_1.FrameInterface;
                if let Some(release) = interface.as_ref().and_then(|i| i.Release) {
                    release(surface);
                }
            }
            copied?;
        }
        let used = (bitstream.DataOffset as usize).min(self.pending.len());
        self.pending.drain(..used);
        // A complete access unit is used up; what is left over could only be a broken one.
        if self.pending.len() > 4 << 20 {
            self.pending.clear();
        }
        Ok(())
    }

    /// Reads the stream's parameter sets and starts the decoder; false without them.
    fn start(&mut self, bitstream: &mut sys::mfxBitstream) -> Result<bool> {
        let mut params: sys::mfxVideoParam = unsafe { zeroed() };
        params.__bindgen_anon_1.mfx.CodecId = match self.codec {
            Codec::H264 => sys::MFX_CODEC_AVC,
            Codec::H265 => sys::MFX_CODEC_HEVC,
        } as u32;
        let status = unsafe { (self.api.decode_header)(self.session, bitstream, &mut params) };
        if status == sys::MFX_ERR_MORE_DATA {
            return Ok(false);
        }
        check(status, "decode header")?;
        params.IOPattern = sys::MFX_IOPATTERN_OUT_VIDEO_MEMORY as u16;
        params.AsyncDepth = 1;
        check(
            unsafe { (self.api.init)(self.session, &mut params) },
            "decoder init",
        )?;
        let info = unsafe { params.__bindgen_anon_1.mfx.FrameInfo };
        self.stream = format!(
            "{} {}{}",
            match self.codec {
                Codec::H264 => "H.264",
                Codec::H265 => "H.265",
            },
            if u32::from(info.ChromaFormat) == sys::MFX_CHROMAFORMAT_YUV444 as u32 {
                "4:4:4"
            } else {
                "4:2:0"
            },
            if info.BitDepthLuma > 8 { " 10-bit" } else { "" }
        );
        self.initialized = true;
        Ok(true)
    }

    /// Waits for the picture and copies it (its visible part) into the next ring texture.
    fn copy_out(
        &mut self,
        surface: *mut sys::mfxFrameSurface1,
        sync: sys::mfxSyncPoint,
        time: i64,
        out: &mut Vec<DecodedFrame>,
    ) -> Result<()> {
        check(unsafe { (self.api.sync)(self.session, sync, 1000) }, "sync")?;
        let mut handle: sys::mfxHDL = null_mut();
        let mut kind: sys::mfxResourceType = 0;
        let (width, height) = unsafe {
            let interface = (*surface).__bindgen_anon_1.FrameInterface;
            let native = interface
                .as_ref()
                .and_then(|i| i.GetNativeHandle)
                .ok_or_else(|| Error::new(E_FAIL, "oneVPL surface without a native handle"))?;
            check(native(surface, &mut handle, &mut kind), "native handle")?;
            let size = (*surface).Info.__bindgen_anon_1.__bindgen_anon_1;
            (u32::from(size.CropW), u32::from(size.CropH))
        };
        if kind != sys::MFX_RESOURCE_DX11_TEXTURE || handle.is_null() {
            return Err(Error::new(
                E_FAIL,
                format!("oneVPL picture of resource type {kind}, not a D3D11 texture"),
            ));
        }
        let source = unsafe { ID3D11Texture2D::from_raw_borrowed(&handle) }
            .ok_or_else(|| Error::new(E_FAIL, "oneVPL picture texture"))?;
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { source.GetDesc(&mut desc) };
        if desc.ArraySize != 1 {
            return Err(Error::new(
                E_FAIL,
                format!("oneVPL picture in a texture array of {}", desc.ArraySize),
            ));
        }
        // Planar 4:2:0 copies need even sizes.
        let (width, height) = (width.min(desc.Width) & !1, height.min(desc.Height) & !1);
        let target = self.ring.next(&self.device, (width, height, desc.Format))?;
        let area = D3D11_BOX {
            left: 0,
            top: 0,
            front: 0,
            right: width,
            bottom: height,
            back: 1,
        };
        unsafe {
            self.context
                .CopySubresourceRegion(&target, 0, 0, 0, 0, source, 0, Some(&area));
        }
        out.push(DecodedFrame {
            texture: target,
            width,
            height,
            time,
        });
        Ok(())
    }
}

impl Drop for VplDecoder {
    fn drop(&mut self) {
        unsafe {
            if !self.session.is_null() {
                if self.initialized {
                    let _ = (self.api.decode_close)(self.session);
                }
                let _ = (self.api.close)(self.session);
            }
            let _ = FreeLibrary(self.library);
        }
    }
}

// The raw pointers are only used from the thread that owns the decoder.
unsafe impl Send for VplDecoder {}
