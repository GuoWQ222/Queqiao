//! AMD's decoders driven directly through AMF (the driver's amfrt64.dll, as the AMF encoder), for viewers on AMD
//! graphics: H.264 and H.265 in 8 or 10 bits (AMD's decoders take no 4:4:4). The decoder must be told the picture's
//! size and depth before it starts: it starts at the first keyframe, from the stream's sequence parameter set
//! (sps.rs). Each picture is copied out of AMF's surface into a ring of NV12 (or P010) textures on our device.

use std::ffi::c_void;
use std::ptr::null_mut;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{E_FAIL, FreeLibrary, HMODULE};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BOX, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, ID3D11Device, ID3D11DeviceContext,
    ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT, DXGI_FORMAT_NV12, DXGI_FORMAT_P010, DXGI_SAMPLE_DESC,
};
use windows::core::{Error, GUID, Interface, PCWSTR, Result, w};

use crate::amf::{boolean, factory, int};
use crate::amf_sys as sys;
use crate::decoder::{DecodedFrame, RING};
use crate::format::Codec;
use crate::sps::{Shape, shape};

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
        Err(Error::new(
            E_FAIL,
            format!("AMF decoder {what}: result {result}"),
        ))
    }
}

/// AMFSurface's interface id (core/Surface.h).
const IID_SURFACE: sys::AMFGuid = sys::AMFGuid {
    data1: 0x3075_dbe3,
    data2: 0x8718,
    data3: 0x4cfa,
    data41: 0x86,
    data42: 0xfb,
    data43: 0x21,
    data44: 0x14,
    data45: 0xc0,
    data46: 0xa5,
    data47: 0xa4,
    data48: 0x51,
};
/// The private data AMF puts on a texture of an array: the picture's slice (as FFmpeg's AMF code reads it).
const TEXTURE_ARRAY_INDEX: GUID = GUID::from_values(
    0x2811_5527,
    0xe7c3,
    0x4b66,
    [0x99, 0xd3, 0x4f, 0x2a, 0xe6, 0xb4, 0x7f, 0xaf],
);
/// AMF_VIDEO_DECODER_MODE_LOW_LATENCY: no pictures held back for reordering.
const REORDER_LOW_LATENCY: i64 = 2;
/// How long a picture may take to come out of the decoder.
const OUTPUT_WAIT: Duration = Duration::from_millis(200);

pub(crate) struct AmfDecoder {
    library: HMODULE,
    context: *mut sys::AMFContext,
    factory: *mut sys::AMFFactory,
    component: *mut sys::AMFComponent,
    codec: Codec,
    shape: Option<Shape>,
    device: ID3D11Device,
    d3d: ID3D11DeviceContext,
    ring: Vec<ID3D11Texture2D>,
    next: usize,
    pub(crate) name: String,
}

impl AmfDecoder {
    pub(crate) fn open(
        device: &ID3D11Device,
        d3d: &ID3D11DeviceContext,
        codec: Codec,
    ) -> Result<AmfDecoder> {
        let (library, factory) = factory()?;
        let mut decoder = AmfDecoder {
            library,
            context: null_mut(),
            factory,
            component: null_mut(),
            codec,
            shape: None,
            device: device.clone(),
            d3d: d3d.clone(),
            ring: Vec::new(),
            next: 0,
            name: format!(
                "AMD AMF {} decoder (hardware)",
                match codec {
                    Codec::H264 => "H.264",
                    Codec::H265 => "H.265",
                }
            ),
        };
        unsafe {
            check(
                call!(factory, CreateContext, &mut decoder.context)?,
                "context",
            )?;
            check(
                call!(decoder.context, InitDX11, device.as_raw(), sys::AMF_DX11_0)?,
                "D3D11 device (not an AMD GPU?)",
            )?;
            // Made here to know at once whether this GPU decodes the codec; started at the first keyframe.
            check(
                call!(
                    factory,
                    CreateComponent,
                    decoder.context,
                    component_id(codec).as_ptr(),
                    &mut decoder.component
                )?,
                "component",
            )?;
        }
        Ok(decoder)
    }

    /// What the stream turned out to be ("H.265 4:2:0 10-bit"), once its headers were read.
    pub(crate) fn stream(&self) -> String {
        match self.shape {
            Some(shape) => format!(
                "{} {}{}",
                match self.codec {
                    Codec::H264 => "H.264",
                    Codec::H265 => "H.265",
                },
                if shape.chroma444 { "4:4:4" } else { "4:2:0" },
                if shape.bit_depth > 8 { " 10-bit" } else { "" }
            ),
            None => String::new(),
        }
    }

    /// Starts (or starts again) the decoder for the stream's shape.
    fn start(&mut self, shape: Shape) -> Result<()> {
        if shape.chroma444 {
            return Err(Error::new(E_FAIL, "AMD's decoders take no 4:4:4"));
        }
        unsafe {
            if self.shape.is_some() {
                let _ = call!(self.component, Terminate);
            }
            let set = |name: PCWSTR, value: sys::AMFVariantStruct| -> Result<()> {
                check(
                    call!(self.component, SetProperty, name.as_ptr(), value)?,
                    "property",
                )
            };
            set(w!("ReorderMode"), int(REORDER_LOW_LATENCY))?;
            let _ = set(w!("LowLatencyDecode"), boolean(true));
            let format = if shape.bit_depth > 8 {
                sys::AMF_SURFACE_P010
            } else {
                sys::AMF_SURFACE_NV12
            };
            check(
                call!(
                    self.component,
                    Init,
                    format,
                    shape.width as i32,
                    shape.height as i32
                )?,
                "init",
            )?;
        }
        self.shape = Some(shape);
        self.ring.clear();
        Ok(())
    }

    /// Decodes one access unit and appends its picture to `out`.
    pub(crate) fn decode(
        &mut self,
        data: &[u8],
        time: i64,
        out: &mut Vec<DecodedFrame>,
    ) -> Result<()> {
        // At the first keyframe (and whenever the parameter sets change the shape), start the decoder.
        if let Some(found) = shape(self.codec, data)
            && self.shape != Some(found)
        {
            self.start(found)?;
        }
        if self.shape.is_none() {
            return Ok(());
        }
        let buffer = self.input(data, time)?;
        let mut result =
            unsafe { call!(self.component, SubmitInput, buffer.cast::<sys::AMFData>())? };
        if result == sys::AMF_INPUT_FULL {
            // A picture is waiting: take it, then the input goes in.
            self.take_output(time, out)?;
            result = unsafe { call!(self.component, SubmitInput, buffer.cast::<sys::AMFData>())? };
        }
        unsafe {
            let _ = call!(buffer, Release);
        }
        if result != sys::AMF_OK && result != sys::AMF_NEED_MORE_INPUT {
            return check(result, "submit");
        }
        self.take_output(time, out)
    }

    /// The access unit in an AMF host buffer.
    fn input(&mut self, data: &[u8], time: i64) -> Result<*mut sys::AMFBuffer> {
        let mut buffer: *mut sys::AMFBuffer = null_mut();
        unsafe {
            check(
                call!(
                    self.context,
                    AllocBuffer,
                    sys::AMF_MEMORY_HOST,
                    data.len(),
                    &mut buffer
                )?,
                "buffer",
            )?;
            let native = call!(buffer, GetNative)?;
            if native.is_null() {
                let _ = call!(buffer, Release);
                return Err(Error::new(E_FAIL, "AMF decoder: buffer without memory"));
            }
            std::ptr::copy_nonoverlapping(data.as_ptr(), native.cast::<u8>(), data.len());
            call!(buffer, SetPts, time)?;
        }
        Ok(buffer)
    }

    /// The decoded picture, when it comes (within OUTPUT_WAIT): copied into the next ring texture.
    fn take_output(&mut self, time: i64, out: &mut Vec<DecodedFrame>) -> Result<()> {
        let deadline = Instant::now() + OUTPUT_WAIT;
        loop {
            let mut data: *mut sys::AMFData = null_mut();
            let result = unsafe { call!(self.component, QueryOutput, &mut data)? };
            if !data.is_null() {
                let copied = self.copy_out(data, time);
                unsafe {
                    let _ = call!(data, Release);
                }
                out.push(copied?);
                return Ok(());
            }
            if result != sys::AMF_REPEAT && result != sys::AMF_OK || Instant::now() > deadline {
                return Ok(());
            }
            std::thread::sleep(Duration::from_micros(500));
        }
    }

    fn copy_out(&mut self, data: *mut sys::AMFData, time: i64) -> Result<DecodedFrame> {
        let shape = self
            .shape
            .ok_or_else(|| Error::new(E_FAIL, "AMF decoder: a picture before the sequence"))?;
        let mut surface: *mut c_void = null_mut();
        let texture = unsafe {
            check(
                call!(data, QueryInterface, &IID_SURFACE, &mut surface)?,
                "surface",
            )?;
            let surface = surface.cast::<sys::AMFSurface>();
            let plane = call!(surface, GetPlaneAt, 0)?;
            let native = if plane.is_null() {
                null_mut()
            } else {
                call!(plane, GetNative)?
            };
            let texture = ID3D11Texture2D::from_raw_borrowed(&native).cloned();
            let _ = call!(surface, Release);
            texture.ok_or_else(|| Error::new(E_FAIL, "AMF decoder: a picture without a texture"))?
        };
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { texture.GetDesc(&mut desc) };
        let mut index = 0u32;
        if desc.ArraySize > 1 {
            let mut size = std::mem::size_of::<u32>() as u32;
            unsafe {
                texture.GetPrivateData(
                    &TEXTURE_ARRAY_INDEX,
                    &mut size,
                    Some((&mut index as *mut u32).cast()),
                )
            }?;
        }
        let (width, height) = (shape.width & !1, shape.height & !1);
        let format: DXGI_FORMAT = if shape.bit_depth > 8 {
            DXGI_FORMAT_P010
        } else {
            DXGI_FORMAT_NV12
        };
        if self.ring.is_empty() {
            for _ in 0..RING {
                let ring_desc = D3D11_TEXTURE2D_DESC {
                    Width: width,
                    Height: height,
                    MipLevels: 1,
                    ArraySize: 1,
                    Format: format,
                    SampleDesc: DXGI_SAMPLE_DESC {
                        Count: 1,
                        Quality: 0,
                    },
                    Usage: D3D11_USAGE_DEFAULT,
                    BindFlags: 0,
                    CPUAccessFlags: 0,
                    MiscFlags: 0,
                };
                let mut made = None;
                unsafe {
                    self.device
                        .CreateTexture2D(&ring_desc, None, Some(&mut made))
                }?;
                self.ring
                    .push(made.ok_or_else(|| Error::new(E_FAIL, "AMF decoder texture"))?);
            }
        }
        let target = self.ring[self.next].clone();
        self.next = (self.next + 1) % self.ring.len();
        let area = D3D11_BOX {
            left: 0,
            top: 0,
            front: 0,
            right: width,
            bottom: height,
            back: 1,
        };
        unsafe {
            self.d3d
                .CopySubresourceRegion(&target, 0, 0, 0, 0, &texture, index, Some(&area));
        }
        Ok(DecodedFrame {
            texture: target,
            width,
            height,
            time,
        })
    }
}

fn component_id(codec: Codec) -> PCWSTR {
    match codec {
        Codec::H264 => w!("AMFVideoDecoderUVD_H264_AVC"),
        Codec::H265 => w!("AMFVideoDecoderHW_H265_HEVC"),
    }
}

/// What H.265 AMD's decoder takes on this GPU (`protocol::hevc` bits): Main and Main 10 when it makes an H.265
/// decoder on our device; 0 without AMD graphics.
pub(crate) fn hevc_caps(device: &ID3D11Device, d3d: &ID3D11DeviceContext) -> u8 {
    use queqiao_core::protocol::hevc;
    match AmfDecoder::open(device, d3d, Codec::H265) {
        Ok(_) => hevc::MAIN | hevc::MAIN10,
        Err(_) => 0,
    }
}

impl Drop for AmfDecoder {
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
        let _ = self.factory;
    }
}

// The raw pointers are only used from the thread that owns the decoder.
unsafe impl Send for AmfDecoder {}
