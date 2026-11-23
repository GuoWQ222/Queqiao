//! NVIDIA's decoder driven directly (NVDEC, through the driver's nvcuvid.dll and nvcuda.dll), for viewers on NVIDIA
//! graphics: H.264 and H.265, 4:2:0 and 4:4:4, 8 and 10 bits (Windows' own H.265 decoder needs an extension from the
//! Store and decodes no 4:4:4). Nothing of NVIDIA's is shipped; the types come from FFmpeg's nv-codec-headers (MIT),
//! `nvdec_sys.rs`; the functions are taken from the driver's DLLs at run time.
//!
//! Each picture: NVDEC's surface (its planes one after another) copied on the GPU into a D3D11 texture of one channel
//! (CUDA's D3D11 interop: no trip through memory), then a pixel shader turns it into BGRA, BT.709 limited range as
//! every Queqiao picture, in the ring the viewer shows. Low latency: no display delay, one picture per access unit.

use std::collections::VecDeque;
use std::ffi::c_void;
use std::mem::zeroed;
use std::ptr::null_mut;

use windows::Win32::Foundation::{E_FAIL, FreeLibrary, HMODULE};
use windows::Win32::Graphics::Direct3D::Fxc::D3DCompile;
use windows::Win32::Graphics::Direct3D::{D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST, ID3DBlob};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_R8_UNORM, DXGI_FORMAT_R16_UNORM,
    DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::System::LibraryLoader::{
    GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW,
};
use windows::core::{Error, Interface, PCSTR, Result, s, w};

use crate::decoder::{DecodedFrame, RING};
use crate::format::Codec;
use crate::nvdec_sys as sys;

type Proc = unsafe extern "system" fn() -> isize;
type CuInit = unsafe extern "system" fn(u32) -> sys::CUresult;
type CuD3D11GetDevice = unsafe extern "system" fn(*mut sys::CUdevice, *mut c_void) -> sys::CUresult;
type CuCtxCreate =
    unsafe extern "system" fn(*mut sys::CUcontext, u32, sys::CUdevice) -> sys::CUresult;
type CuCtxDestroy = unsafe extern "system" fn(sys::CUcontext) -> sys::CUresult;
type CuCtxPush = unsafe extern "system" fn(sys::CUcontext) -> sys::CUresult;
type CuCtxPop = unsafe extern "system" fn(*mut sys::CUcontext) -> sys::CUresult;
type CuRegister =
    unsafe extern "system" fn(*mut sys::CUgraphicsResource, *mut c_void, u32) -> sys::CUresult;
type CuUnregister = unsafe extern "system" fn(sys::CUgraphicsResource) -> sys::CUresult;
type CuMap =
    unsafe extern "system" fn(u32, *mut sys::CUgraphicsResource, sys::CUstream) -> sys::CUresult;
type CuMappedArray = unsafe extern "system" fn(
    *mut sys::CUarray,
    sys::CUgraphicsResource,
    u32,
    u32,
) -> sys::CUresult;
type CuMemcpy2D = unsafe extern "system" fn(*const sys::CUDA_MEMCPY2D) -> sys::CUresult;
type CuvidCreateParser = unsafe extern "system" fn(
    *mut sys::CUvideoparser,
    *mut sys::CUVIDPARSERPARAMS,
) -> sys::CUresult;
type CuvidParse =
    unsafe extern "system" fn(sys::CUvideoparser, *mut sys::CUVIDSOURCEDATAPACKET) -> sys::CUresult;
type CuvidDestroyParser = unsafe extern "system" fn(sys::CUvideoparser) -> sys::CUresult;
type CuvidCreateDecoder = unsafe extern "system" fn(
    *mut sys::CUvideodecoder,
    *mut sys::CUVIDDECODECREATEINFO,
) -> sys::CUresult;
type CuvidDestroyDecoder = unsafe extern "system" fn(sys::CUvideodecoder) -> sys::CUresult;
type CuvidDecode =
    unsafe extern "system" fn(sys::CUvideodecoder, *mut sys::CUVIDPICPARAMS) -> sys::CUresult;
type CuvidMap = unsafe extern "system" fn(
    sys::CUvideodecoder,
    i32,
    *mut u64,
    *mut u32,
    *mut sys::CUVIDPROCPARAMS,
) -> sys::CUresult;
type CuvidUnmap = unsafe extern "system" fn(sys::CUvideodecoder, u64) -> sys::CUresult;
type CuvidCaps = unsafe extern "system" fn(*mut sys::CUVIDDECODECAPS) -> sys::CUresult;

/// The driver's functions (nvcuda.dll and nvcuvid.dll).
struct Api {
    cuda: HMODULE,
    cuvid: HMODULE,
    init: CuInit,
    d3d11_device: CuD3D11GetDevice,
    ctx_create: CuCtxCreate,
    ctx_destroy: CuCtxDestroy,
    ctx_push: CuCtxPush,
    ctx_pop: CuCtxPop,
    register: CuRegister,
    unregister: CuUnregister,
    map: CuMap,
    unmap: CuMap,
    mapped_array: CuMappedArray,
    memcpy2d: CuMemcpy2D,
    create_parser: CuvidCreateParser,
    parse: CuvidParse,
    destroy_parser: CuvidDestroyParser,
    create_decoder: CuvidCreateDecoder,
    destroy_decoder: CuvidDestroyDecoder,
    decode: CuvidDecode,
    map_frame: CuvidMap,
    unmap_frame: CuvidUnmap,
    caps: CuvidCaps,
}

impl Api {
    fn load() -> Result<Api> {
        let cuda = unsafe { LoadLibraryExW(w!("nvcuda.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32) }
            .map_err(|e| Error::new(e.code(), "no NVIDIA driver with CUDA (nvcuda.dll)"))?;
        let cuvid = match unsafe {
            LoadLibraryExW(w!("nvcuvid.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32)
        } {
            Ok(library) => library,
            Err(error) => {
                let _ = unsafe { FreeLibrary(cuda) };
                return Err(Error::new(
                    error.code(),
                    "no NVIDIA video decoder (nvcuvid.dll)",
                ));
            }
        };
        let get = |library: HMODULE, name: PCSTR| -> Result<Proc> {
            unsafe { GetProcAddress(library, name) }.ok_or_else(|| {
                Error::new(
                    E_FAIL,
                    format!("the NVIDIA driver has no {}", unsafe { name.display() }),
                )
            })
        };
        let made: Result<Api> = (|| unsafe {
            Ok(Api {
                cuda,
                cuvid,
                init: std::mem::transmute::<Proc, CuInit>(get(cuda, s!("cuInit"))?),
                d3d11_device: std::mem::transmute::<Proc, CuD3D11GetDevice>(get(
                    cuda,
                    s!("cuD3D11GetDevice"),
                )?),
                ctx_create: std::mem::transmute::<Proc, CuCtxCreate>(get(
                    cuda,
                    s!("cuCtxCreate_v2"),
                )?),
                ctx_destroy: std::mem::transmute::<Proc, CuCtxDestroy>(get(
                    cuda,
                    s!("cuCtxDestroy_v2"),
                )?),
                ctx_push: std::mem::transmute::<Proc, CuCtxPush>(get(
                    cuda,
                    s!("cuCtxPushCurrent_v2"),
                )?),
                ctx_pop: std::mem::transmute::<Proc, CuCtxPop>(get(
                    cuda,
                    s!("cuCtxPopCurrent_v2"),
                )?),
                register: std::mem::transmute::<Proc, CuRegister>(get(
                    cuda,
                    s!("cuGraphicsD3D11RegisterResource"),
                )?),
                unregister: std::mem::transmute::<Proc, CuUnregister>(get(
                    cuda,
                    s!("cuGraphicsUnregisterResource"),
                )?),
                map: std::mem::transmute::<Proc, CuMap>(get(cuda, s!("cuGraphicsMapResources"))?),
                unmap: std::mem::transmute::<Proc, CuMap>(get(
                    cuda,
                    s!("cuGraphicsUnmapResources"),
                )?),
                mapped_array: std::mem::transmute::<Proc, CuMappedArray>(get(
                    cuda,
                    s!("cuGraphicsSubResourceGetMappedArray"),
                )?),
                memcpy2d: std::mem::transmute::<Proc, CuMemcpy2D>(get(cuda, s!("cuMemcpy2D_v2"))?),
                create_parser: std::mem::transmute::<Proc, CuvidCreateParser>(get(
                    cuvid,
                    s!("cuvidCreateVideoParser"),
                )?),
                parse: std::mem::transmute::<Proc, CuvidParse>(get(
                    cuvid,
                    s!("cuvidParseVideoData"),
                )?),
                destroy_parser: std::mem::transmute::<Proc, CuvidDestroyParser>(get(
                    cuvid,
                    s!("cuvidDestroyVideoParser"),
                )?),
                create_decoder: std::mem::transmute::<Proc, CuvidCreateDecoder>(get(
                    cuvid,
                    s!("cuvidCreateDecoder"),
                )?),
                destroy_decoder: std::mem::transmute::<Proc, CuvidDestroyDecoder>(get(
                    cuvid,
                    s!("cuvidDestroyDecoder"),
                )?),
                decode: std::mem::transmute::<Proc, CuvidDecode>(get(
                    cuvid,
                    s!("cuvidDecodePicture"),
                )?),
                map_frame: std::mem::transmute::<Proc, CuvidMap>(get(
                    cuvid,
                    s!("cuvidMapVideoFrame64"),
                )?),
                unmap_frame: std::mem::transmute::<Proc, CuvidUnmap>(get(
                    cuvid,
                    s!("cuvidUnmapVideoFrame64"),
                )?),
                caps: std::mem::transmute::<Proc, CuvidCaps>(get(
                    cuvid,
                    s!("cuvidGetDecoderCaps"),
                )?),
            })
        })();
        if made.is_err() {
            unsafe {
                let _ = FreeLibrary(cuvid);
                let _ = FreeLibrary(cuda);
            }
        }
        made
    }
}

fn check(result: sys::CUresult, what: &str) -> Result<()> {
    if result == sys::CUDA_SUCCESS {
        Ok(())
    } else {
        Err(Error::new(
            E_FAIL,
            format!("NVDEC {what}: CUDA error {result}"),
        ))
    }
}

/// The YUV -> BGRA shader: the planes one after another in one texture of one channel (8 or 16 bits); 4:2:0 has its
/// chroma interleaved (U V) at half size, 4:4:4 its U and V planes in full. BT.709, limited range.
const SHADER: &str = r#"
Texture2D<float> planes : register(t0);
cbuffer Info : register(b0) { uint height; uint chroma444; uint unused0; uint unused1; };
float4 vs(uint id : SV_VertexID) : SV_Position {
    float2 uv = float2((id << 1) & 2, id & 2);
    return float4(uv * float2(2, -2) + float2(-1, 1), 0, 1);
}
float4 ps(float4 position : SV_Position) : SV_Target {
    uint2 p = uint2(position.xy);
    float y = planes.Load(int3(p.x, p.y, 0));
    float u, v;
    if (chroma444 != 0) {
        u = planes.Load(int3(p.x, height + p.y, 0));
        v = planes.Load(int3(p.x, 2 * height + p.y, 0));
    } else {
        uint2 c = p / 2;
        u = planes.Load(int3(c.x * 2, height + c.y, 0));
        v = planes.Load(int3(c.x * 2 + 1, height + c.y, 0));
    }
    float l = (y - 16.0 / 255.0) * (255.0 / 219.0);
    float cb = (u - 128.0 / 255.0) * (255.0 / 224.0);
    float cr = (v - 128.0 / 255.0) * (255.0 / 224.0);
    return float4(saturate(l + 1.5748 * cr), saturate(l - 0.1873 * cb - 0.4681 * cr), saturate(l + 1.8556 * cb), 1);
}
"#;

fn compile(entry: PCSTR, target: PCSTR) -> Result<ID3DBlob> {
    let mut code: Option<ID3DBlob> = None;
    let mut errors: Option<ID3DBlob> = None;
    let result = unsafe {
        D3DCompile(
            SHADER.as_ptr().cast(),
            SHADER.len(),
            s!("nvdec"),
            None,
            None::<&windows::Win32::Graphics::Direct3D::ID3DInclude>,
            entry,
            target,
            0,
            0,
            &mut code,
            Some(&mut errors),
        )
    };
    if let Err(error) = result {
        let text = errors
            .map(|blob| unsafe {
                String::from_utf8_lossy(std::slice::from_raw_parts(
                    blob.GetBufferPointer().cast::<u8>(),
                    blob.GetBufferSize(),
                ))
                .into_owned()
            })
            .unwrap_or_default();
        return Err(Error::new(error.code(), format!("NVDEC shader: {text}")));
    }
    code.ok_or_else(|| Error::new(E_FAIL, "NVDEC shader: no code"))
}

fn bytes(blob: &ID3DBlob) -> &[u8] {
    unsafe {
        std::slice::from_raw_parts(blob.GetBufferPointer().cast::<u8>(), blob.GetBufferSize())
    }
}

/// What the parser's callbacks share with the decoder (they run inside `cuvidParseVideoData`).
struct Shared {
    api: *const Api,
    codec: Codec,
    decoder: sys::CUvideodecoder,
    /// The stream as the decoder was made for it: (width, height, chroma 4:4:4, bit depth over 8, surfaces).
    shape: Option<(u32, u32, bool, u32, u32)>,
    /// Pictures to show, in order (NVDEC's surface indices).
    shown: VecDeque<i32>,
    error: Option<String>,
}

unsafe extern "C" fn on_sequence(user: *mut c_void, format: *mut sys::CUVIDEOFORMAT) -> i32 {
    let shared = unsafe { &mut *user.cast::<Shared>() };
    let format = unsafe { &*format };
    let api = unsafe { &*shared.api };
    let area = format.display_area;
    let width = (area.right - area.left).max(0) as u32;
    let height = (area.bottom - area.top).max(0) as u32;
    let chroma444 = format.chroma_format == sys::cudaVideoChromaFormat_444;
    let depth = u32::from(format.bit_depth_luma_minus8);
    let surfaces = u32::from(format.min_num_decode_surfaces).max(4) + 2;
    let shape = (width, height, chroma444, depth, surfaces);
    if shared.shape == Some(shape) && !shared.decoder.is_null() {
        return surfaces as i32;
    }
    if !shared.decoder.is_null() {
        unsafe { (api.destroy_decoder)(shared.decoder) };
        shared.decoder = null_mut();
    }
    let mut create: sys::CUVIDDECODECREATEINFO = unsafe { zeroed() };
    create.CodecType = format.codec;
    create.ChromaFormat = format.chroma_format;
    create.OutputFormat = match (chroma444, depth > 0) {
        (false, false) => sys::cudaVideoSurfaceFormat_NV12,
        (false, true) => sys::cudaVideoSurfaceFormat_P016,
        (true, false) => sys::cudaVideoSurfaceFormat_YUV444,
        (true, true) => sys::cudaVideoSurfaceFormat_YUV444_16Bit,
    };
    create.bitDepthMinus8 = depth;
    create.DeinterlaceMode = sys::cudaVideoDeinterlaceMode_Weave;
    create.ulWidth = format.coded_width;
    create.ulHeight = format.coded_height;
    create.ulMaxWidth = format.coded_width;
    create.ulMaxHeight = format.coded_height;
    create.ulTargetWidth = width;
    create.ulTargetHeight = height;
    create.display_area.left = area.left as i16;
    create.display_area.top = area.top as i16;
    create.display_area.right = area.right as i16;
    create.display_area.bottom = area.bottom as i16;
    create.ulNumDecodeSurfaces = surfaces;
    create.ulNumOutputSurfaces = 2;
    create.ulCreationFlags = sys::cudaVideoCreate_PreferCUVID as _;
    let mut decoder: sys::CUvideodecoder = null_mut();
    let result = unsafe { (api.create_decoder)(&mut decoder, &mut create) };
    if result != sys::CUDA_SUCCESS {
        shared.error = Some(format!(
            "NVDEC cannot decode this stream ({}x{}, {}, {} bits): CUDA error {result}",
            width,
            height,
            if chroma444 { "4:4:4" } else { "4:2:0" },
            8 + depth
        ));
        return 0;
    }
    shared.decoder = decoder;
    shared.shape = Some(shape);
    surfaces as i32
}

unsafe extern "C" fn on_decode(user: *mut c_void, picture: *mut sys::CUVIDPICPARAMS) -> i32 {
    let shared = unsafe { &mut *user.cast::<Shared>() };
    if shared.decoder.is_null() {
        return 0;
    }
    let api = unsafe { &*shared.api };
    let result = unsafe { (api.decode)(shared.decoder, picture) };
    if result != sys::CUDA_SUCCESS {
        shared.error = Some(format!("NVDEC decode: CUDA error {result}"));
        return 0;
    }
    1
}

unsafe extern "C" fn on_display(user: *mut c_void, info: *mut sys::CUVIDPARSERDISPINFO) -> i32 {
    let shared = unsafe { &mut *user.cast::<Shared>() };
    if !info.is_null() {
        shared.shown.push_back(unsafe { (*info).picture_index });
    }
    1
}

/// The textures of one stream shape: NVDEC's planes (one channel, the copy target) and the BGRA ring.
struct Surfaces {
    shape: (u32, u32, bool, u32),
    planes: ID3D11Texture2D,
    planes_view: ID3D11ShaderResourceView,
    resource: sys::CUgraphicsResource,
    ring: Vec<(ID3D11Texture2D, ID3D11RenderTargetView)>,
    next: usize,
}

pub(crate) struct NvdecDecoder {
    api: Box<Api>,
    context: sys::CUcontext,
    parser: sys::CUvideoparser,
    shared: Box<Shared>,
    device: ID3D11Device,
    d3d: ID3D11DeviceContext,
    vertex: ID3D11VertexShader,
    pixel: ID3D11PixelShader,
    info: ID3D11Buffer,
    surfaces: Option<Surfaces>,
    pub(crate) name: String,
}

impl NvdecDecoder {
    pub(crate) fn open(
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
        codec: Codec,
    ) -> Result<NvdecDecoder> {
        let api = Box::new(Api::load()?);
        check(unsafe { (api.init)(0) }, "init")?;
        // The CUDA device behind our D3D11 device (none: not an NVIDIA GPU).
        let adapter = device
            .cast::<IDXGIDevice>()
            .and_then(|d| unsafe { d.GetAdapter() })?;
        let mut cuda_device: sys::CUdevice = 0;
        check(
            unsafe { (api.d3d11_device)(&mut cuda_device, adapter.as_raw()) },
            "the GPU (not an NVIDIA one?)",
        )?;
        let mut cuda: sys::CUcontext = null_mut();
        check(
            unsafe { (api.ctx_create)(&mut cuda, 0, cuda_device) },
            "context",
        )?;
        let mut pop: sys::CUcontext = null_mut();
        unsafe { (api.ctx_pop)(&mut pop) };
        let vertex_code = compile(s!("vs"), s!("vs_4_0"))?;
        let pixel_code = compile(s!("ps"), s!("ps_4_0"))?;
        let mut vertex = None;
        let mut pixel = None;
        unsafe {
            device.CreateVertexShader(
                bytes(&vertex_code),
                None::<&ID3D11ClassLinkage>,
                Some(&mut vertex),
            )?;
            device.CreatePixelShader(
                bytes(&pixel_code),
                None::<&ID3D11ClassLinkage>,
                Some(&mut pixel),
            )?;
        }
        let info_desc = D3D11_BUFFER_DESC {
            ByteWidth: 16,
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
            ..Default::default()
        };
        let mut info = None;
        unsafe { device.CreateBuffer(&info_desc, None, Some(&mut info)) }?;
        let mut shared = Box::new(Shared {
            api: &*api,
            codec,
            decoder: null_mut(),
            shape: None,
            shown: VecDeque::new(),
            error: None,
        });
        let mut params: sys::CUVIDPARSERPARAMS = unsafe { zeroed() };
        params.CodecType = match codec {
            Codec::H264 => sys::cudaVideoCodec_H264,
            Codec::H265 => sys::cudaVideoCodec_HEVC,
        };
        params.ulMaxNumDecodeSurfaces = 1;
        params.ulMaxDisplayDelay = 0;
        params.pUserData = (&mut *shared as *mut Shared).cast();
        params.pfnSequenceCallback = Some(on_sequence);
        params.pfnDecodePicture = Some(on_decode);
        params.pfnDisplayPicture = Some(on_display);
        let mut parser: sys::CUvideoparser = null_mut();
        let made = unsafe { (api.create_parser)(&mut parser, &mut params) };
        let missing = || Error::new(E_FAIL, "NVDEC: no shader");
        let decoder = NvdecDecoder {
            name: format!(
                "NVIDIA NVDEC {} decoder (hardware)",
                match codec {
                    Codec::H264 => "H.264",
                    Codec::H265 => "H.265",
                }
            ),
            api,
            context: cuda,
            parser,
            shared,
            device: device.clone(),
            d3d: context.clone(),
            vertex: vertex.ok_or_else(missing)?,
            pixel: pixel.ok_or_else(missing)?,
            info: info.ok_or_else(missing)?,
            surfaces: None,
        };
        check(made, "parser")?;
        Ok(decoder)
    }

    /// What the stream turned out to be ("H.265 4:4:4 10-bit"), once its headers were read.
    pub(crate) fn stream(&self) -> String {
        match self.shared.shape {
            Some((_, _, chroma444, depth, _)) => format!(
                "{} {}{}",
                match self.shared.codec {
                    Codec::H264 => "H.264",
                    Codec::H265 => "H.265",
                },
                if chroma444 { "4:4:4" } else { "4:2:0" },
                if depth > 0 { " 10-bit" } else { "" }
            ),
            None => String::new(),
        }
    }

    /// Decodes one access unit and appends its picture (BGRA) to `out`.
    pub(crate) fn decode(
        &mut self,
        data: &[u8],
        time: i64,
        out: &mut Vec<DecodedFrame>,
    ) -> Result<()> {
        check(unsafe { (self.api.ctx_push)(self.context) }, "context")?;
        let result = self.decode_in_context(data, time, out);
        let mut pop: sys::CUcontext = null_mut();
        unsafe { (self.api.ctx_pop)(&mut pop) };
        result
    }

    fn decode_in_context(
        &mut self,
        data: &[u8],
        time: i64,
        out: &mut Vec<DecodedFrame>,
    ) -> Result<()> {
        let mut packet: sys::CUVIDSOURCEDATAPACKET = unsafe { zeroed() };
        packet.flags = (sys::CUVID_PKT_ENDOFPICTURE | sys::CUVID_PKT_TIMESTAMP) as _;
        packet.payload_size = data.len() as _;
        packet.payload = data.as_ptr();
        packet.timestamp = time;
        check(
            unsafe { (self.api.parse)(self.parser, &mut packet) },
            "parse",
        )?;
        if let Some(error) = self.shared.error.take() {
            return Err(Error::new(E_FAIL, error));
        }
        while let Some(index) = self.shared.shown.pop_front() {
            out.push(self.show(index, time)?);
        }
        Ok(())
    }

    /// NVDEC's surface `index` into the next BGRA ring texture.
    fn show(&mut self, index: i32, time: i64) -> Result<DecodedFrame> {
        let (width, height, chroma444, depth, _) = self
            .shared
            .shape
            .ok_or_else(|| Error::new(E_FAIL, "NVDEC: a picture before the sequence"))?;
        self.prepare((width, height, chroma444, depth))?;
        let mut frame: u64 = 0;
        let mut pitch: u32 = 0;
        let mut process: sys::CUVIDPROCPARAMS = unsafe { zeroed() };
        process.progressive_frame = 1;
        check(
            unsafe {
                (self.api.map_frame)(
                    self.shared.decoder,
                    index,
                    &mut frame,
                    &mut pitch,
                    &mut process,
                )
            },
            "map picture",
        )?;
        let copied = self.copy_planes(frame, pitch as usize, width, height, chroma444, depth);
        unsafe { (self.api.unmap_frame)(self.shared.decoder, frame) };
        copied?;
        self.convert(width, height, chroma444, time)
    }

    /// The textures for this shape (made again when it changes).
    fn prepare(&mut self, shape: (u32, u32, bool, u32)) -> Result<()> {
        if self.surfaces.as_ref().is_some_and(|s| s.shape == shape) {
            return Ok(());
        }
        if let Some(old) = self.surfaces.take() {
            unsafe { (self.api.unregister)(old.resource) };
        }
        let (width, height, chroma444, depth) = shape;
        let rows = if chroma444 {
            height * 3
        } else {
            height + height / 2
        };
        let format: DXGI_FORMAT = if depth > 0 {
            DXGI_FORMAT_R16_UNORM
        } else {
            DXGI_FORMAT_R8_UNORM
        };
        let texture = |format: DXGI_FORMAT, height: u32, bind: u32| -> Result<ID3D11Texture2D> {
            let desc = D3D11_TEXTURE2D_DESC {
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
                BindFlags: bind,
                CPUAccessFlags: 0,
                MiscFlags: 0,
            };
            let mut made = None;
            unsafe { self.device.CreateTexture2D(&desc, None, Some(&mut made)) }?;
            made.ok_or_else(|| Error::new(E_FAIL, "NVDEC texture"))
        };
        let planes = texture(format, rows, D3D11_BIND_SHADER_RESOURCE.0 as u32)?;
        let mut planes_view = None;
        unsafe {
            self.device
                .CreateShaderResourceView(&planes, None, Some(&mut planes_view))
        }?;
        let mut resource: sys::CUgraphicsResource = null_mut();
        check(
            unsafe {
                (self.api.register)(
                    &mut resource,
                    planes.as_raw(),
                    sys::CU_GRAPHICS_REGISTER_FLAGS_NONE as u32,
                )
            },
            "register the texture",
        )?;
        let mut ring = Vec::new();
        for _ in 0..RING {
            let bgra = texture(
                DXGI_FORMAT_B8G8R8A8_UNORM,
                height,
                (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
            )?;
            let mut target = None;
            unsafe {
                self.device
                    .CreateRenderTargetView(&bgra, None, Some(&mut target))
            }?;
            ring.push((
                bgra,
                target.ok_or_else(|| Error::new(E_FAIL, "NVDEC render target"))?,
            ));
        }
        self.surfaces = Some(Surfaces {
            shape,
            planes,
            planes_view: planes_view.ok_or_else(|| Error::new(E_FAIL, "NVDEC view"))?,
            resource,
            ring,
            next: 0,
        });
        Ok(())
    }

    /// NVDEC's planes (luma `height` rows, then chroma: half the rows for 4:2:0, two planes for 4:4:4; each plane after
    /// the surface's height of rows) into the planes texture, on the GPU.
    fn copy_planes(
        &mut self,
        frame: u64,
        pitch: usize,
        width: u32,
        height: u32,
        chroma444: bool,
        depth: u32,
    ) -> Result<()> {
        let surfaces = self
            .surfaces
            .as_mut()
            .ok_or_else(|| Error::new(E_FAIL, "NVDEC: no textures"))?;
        let mut resource = surfaces.resource;
        check(
            unsafe { (self.api.map)(1, &mut resource, null_mut()) },
            "map the texture",
        )?;
        let copied = (|| {
            let mut array: sys::CUarray = null_mut();
            check(
                unsafe { (self.api.mapped_array)(&mut array, resource, 0, 0) },
                "texture array",
            )?;
            let sample = if depth > 0 { 2 } else { 1 };
            // The surface's planes start every `aligned` rows (NVDEC's height for this stream, even).
            let aligned = ((height + 1) & !1) as usize;
            let planes: Vec<(usize, usize, usize)> = if chroma444 {
                (0..3)
                    .map(|plane| (plane * aligned, plane * height as usize, height as usize))
                    .collect()
            } else {
                vec![
                    (0, 0, height as usize),
                    (aligned, height as usize, (height / 2) as usize),
                ]
            };
            for (source_row, target_row, rows) in planes {
                let mut copy: sys::CUDA_MEMCPY2D = unsafe { zeroed() };
                copy.srcMemoryType = sys::CU_MEMORYTYPE_DEVICE;
                copy.srcDevice = frame + (source_row * pitch) as u64;
                copy.srcPitch = pitch;
                copy.dstMemoryType = sys::CU_MEMORYTYPE_ARRAY;
                copy.dstArray = array;
                copy.dstY = target_row;
                copy.WidthInBytes = width as usize * sample;
                copy.Height = rows;
                check(unsafe { (self.api.memcpy2d)(&copy) }, "copy")?;
            }
            Ok(())
        })();
        unsafe { (self.api.unmap)(1, &mut resource, null_mut()) };
        copied
    }

    /// The planes texture into the next ring texture as BGRA (the shader), on our D3D11 device.
    fn convert(
        &mut self,
        width: u32,
        height: u32,
        chroma444: bool,
        time: i64,
    ) -> Result<DecodedFrame> {
        let surfaces = self
            .surfaces
            .as_mut()
            .ok_or_else(|| Error::new(E_FAIL, "NVDEC: no textures"))?;
        let (texture, target) = surfaces.ring[surfaces.next].clone();
        surfaces.next = (surfaces.next + 1) % surfaces.ring.len();
        let info: [u32; 4] = [height, u32::from(chroma444), 0, 0];
        let viewport = D3D11_VIEWPORT {
            TopLeftX: 0.0,
            TopLeftY: 0.0,
            Width: width as f32,
            Height: height as f32,
            MinDepth: 0.0,
            MaxDepth: 1.0,
        };
        unsafe {
            let d3d = &self.d3d;
            d3d.UpdateSubresource(&self.info, 0, None, info.as_ptr().cast(), 0, 0);
            d3d.OMSetRenderTargets(
                Some(&[Some(target.clone())]),
                None::<&ID3D11DepthStencilView>,
            );
            d3d.RSSetViewports(Some(&[viewport]));
            d3d.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            d3d.IASetInputLayout(None::<&ID3D11InputLayout>);
            d3d.VSSetShader(&self.vertex, None);
            d3d.PSSetShader(&self.pixel, None);
            d3d.PSSetShaderResources(0, Some(&[Some(surfaces.planes_view.clone())]));
            d3d.PSSetConstantBuffers(0, Some(&[Some(self.info.clone())]));
            d3d.Draw(3, 0);
            // Nothing left bound: the presenter and the video processor use the same context.
            d3d.PSSetShaderResources(0, Some(&[None]));
            d3d.OMSetRenderTargets(None, None::<&ID3D11DepthStencilView>);
        }
        let _ = &surfaces.planes;
        Ok(DecodedFrame {
            texture,
            width,
            height,
            time,
        })
    }
}

/// What H.265 NVIDIA's decoder takes on this GPU (`protocol::hevc` bits; 0 without NVIDIA graphics).
pub(crate) fn hevc_caps(device: &ID3D11Device) -> u8 {
    let Ok(api) = Api::load() else {
        return 0;
    };
    let caps = (|| -> Result<u8> {
        check(unsafe { (api.init)(0) }, "init")?;
        let adapter = device
            .cast::<IDXGIDevice>()
            .and_then(|d| unsafe { d.GetAdapter() })?;
        let mut cuda_device: sys::CUdevice = 0;
        check(
            unsafe { (api.d3d11_device)(&mut cuda_device, adapter.as_raw()) },
            "GPU",
        )?;
        let mut cuda: sys::CUcontext = null_mut();
        check(
            unsafe { (api.ctx_create)(&mut cuda, 0, cuda_device) },
            "context",
        )?;
        let mut caps = 0;
        for (chroma444, ten_bit) in [(false, false), (false, true), (true, false), (true, true)] {
            let mut query: sys::CUVIDDECODECAPS = unsafe { zeroed() };
            query.eCodecType = sys::cudaVideoCodec_HEVC;
            query.eChromaFormat = if chroma444 {
                sys::cudaVideoChromaFormat_444
            } else {
                sys::cudaVideoChromaFormat_420
            };
            query.nBitDepthMinus8 = if ten_bit { 2 } else { 0 };
            if unsafe { (api.caps)(&mut query) } == sys::CUDA_SUCCESS && query.bIsSupported != 0 {
                caps |= queqiao_core::protocol::hevc::bit(chroma444, ten_bit);
            }
        }
        unsafe { (api.ctx_destroy)(cuda) };
        Ok(caps)
    })()
    .unwrap_or(0);
    unsafe {
        let _ = FreeLibrary(api.cuvid);
        let _ = FreeLibrary(api.cuda);
    }
    caps
}

impl Drop for NvdecDecoder {
    fn drop(&mut self) {
        unsafe {
            (self.api.ctx_push)(self.context);
            if !self.parser.is_null() {
                (self.api.destroy_parser)(self.parser);
            }
            if !self.shared.decoder.is_null() {
                (self.api.destroy_decoder)(self.shared.decoder);
            }
            if let Some(surfaces) = self.surfaces.take() {
                (self.api.unregister)(surfaces.resource);
            }
            let mut pop: sys::CUcontext = null_mut();
            (self.api.ctx_pop)(&mut pop);
            (self.api.ctx_destroy)(self.context);
            let _ = FreeLibrary(self.api.cuvid);
            let _ = FreeLibrary(self.api.cuda);
        }
    }
}

// The raw pointers are only used from the thread that owns the decoder.
unsafe impl Send for NvdecDecoder {}
