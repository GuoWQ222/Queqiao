//! NVIDIA's decoder on Linux (docs/LINUX.md; Windows: nvdec.rs, which hands its pictures to D3D11): NVDEC through the
//! driver's libnvcuvid.so.1 on a CUDA context of its own (cuda.rs): H.264 and AV1 4:2:0, H.265 4:2:0 and 4:4:4, 8 and
//! 10 bits, as the GPU decodes them (`caps`). Each picture is copied from NVDEC's surface into memory, its planes as
//! NVDEC lays them out (NV12, P016, or three planes for 4:4:4: `PictureFormat`), and the presenter converts it on the
//! CPU (yuv.rs, as Windows does when its GPU cannot). Nothing of NVIDIA's is shipped: the types are FFmpeg's
//! nv-codec-headers (MIT, nvdec_sys.rs, shared with Windows: `unsigned long` is `c_ulong` there, so the layout follows
//! the system), the functions come from the driver at run time.

use std::collections::VecDeque;
use std::ffi::c_void;
use std::mem::zeroed;
use std::ptr::null_mut;
use std::sync::OnceLock;

use queqiao_core::protocol::VideoCodec;

use crate::cuda::{Cuda, Library};
use crate::linux_decode::{DecodedFrame, Picture, PictureFormat};
use crate::nvdec_sys as sys;

type CuvidCreateParser =
    unsafe extern "C" fn(*mut sys::CUvideoparser, *mut sys::CUVIDPARSERPARAMS) -> sys::CUresult;
type CuvidParse =
    unsafe extern "C" fn(sys::CUvideoparser, *mut sys::CUVIDSOURCEDATAPACKET) -> sys::CUresult;
type CuvidDestroyParser = unsafe extern "C" fn(sys::CUvideoparser) -> sys::CUresult;
type CuvidCreateDecoder = unsafe extern "C" fn(
    *mut sys::CUvideodecoder,
    *mut sys::CUVIDDECODECREATEINFO,
) -> sys::CUresult;
type CuvidDestroyDecoder = unsafe extern "C" fn(sys::CUvideodecoder) -> sys::CUresult;
type CuvidDecode =
    unsafe extern "C" fn(sys::CUvideodecoder, *mut sys::CUVIDPICPARAMS) -> sys::CUresult;
type CuvidMap = unsafe extern "C" fn(
    sys::CUvideodecoder,
    i32,
    *mut u64,
    *mut u32,
    *mut sys::CUVIDPROCPARAMS,
) -> sys::CUresult;
type CuvidUnmap = unsafe extern "C" fn(sys::CUvideodecoder, u64) -> sys::CUresult;
type CuvidCaps = unsafe extern "C" fn(*mut sys::CUVIDDECODECAPS) -> sys::CUresult;

/// The driver's NVDEC functions (libnvcuvid.so.1).
struct Api {
    create_parser: CuvidCreateParser,
    parse: CuvidParse,
    destroy_parser: CuvidDestroyParser,
    create_decoder: CuvidCreateDecoder,
    destroy_decoder: CuvidDestroyDecoder,
    decode: CuvidDecode,
    map_frame: CuvidMap,
    unmap_frame: CuvidUnmap,
    caps: CuvidCaps,
    _library: Library,
}

impl Api {
    fn open() -> Result<Api, String> {
        let library = Library::open(c"libnvcuvid.so.1")?;
        unsafe {
            Ok(Api {
                create_parser: library.function(c"cuvidCreateVideoParser")?,
                parse: library.function(c"cuvidParseVideoData")?,
                destroy_parser: library.function(c"cuvidDestroyVideoParser")?,
                create_decoder: library.function(c"cuvidCreateDecoder")?,
                destroy_decoder: library.function(c"cuvidDestroyDecoder")?,
                decode: library.function(c"cuvidDecodePicture")?,
                map_frame: library.function(c"cuvidMapVideoFrame64")?,
                unmap_frame: library.function(c"cuvidUnmapVideoFrame64")?,
                caps: library.function(c"cuvidGetDecoderCaps")?,
                _library: library,
            })
        }
    }
}

fn cuvid_codec(codec: VideoCodec) -> sys::cudaVideoCodec {
    match codec {
        VideoCodec::H264 => sys::cudaVideoCodec_H264,
        VideoCodec::H265 => sys::cudaVideoCodec_HEVC,
        VideoCodec::Av1 => sys::cudaVideoCodec_AV1,
    }
}

fn codec_name(codec: VideoCodec) -> &'static str {
    match codec {
        VideoCodec::H264 => "H.264",
        VideoCodec::H265 => "H.265",
        VideoCodec::Av1 => "AV1",
    }
}

/// What this PC's first NVIDIA GPU's NVDEC decodes, found once.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Caps {
    /// H.264 8-bit 4:2:0.
    pub(crate) h264: bool,
    /// H.265 as `protocol::hevc` bits (Main, Main 10, 4:4:4, 4:4:4 10-bit).
    pub(crate) hevc: u8,
    /// AV1 8-bit 4:2:0.
    pub(crate) av1: bool,
}

/// NVDEC's capabilities; None without NVDEC (no NVIDIA driver, no GPU, or it failed).
pub(crate) fn caps() -> Option<Caps> {
    static CAPS: OnceLock<Option<Caps>> = OnceLock::new();
    *CAPS.get_or_init(|| {
        let cuda = Cuda::open(0).ok()?;
        let api = Api::open().ok()?;
        let decodes = |codec: VideoCodec, chroma444: bool, ten_bit: bool| {
            let mut caps: sys::CUVIDDECODECAPS = unsafe { zeroed() };
            caps.eCodecType = cuvid_codec(codec);
            caps.eChromaFormat = if chroma444 {
                sys::cudaVideoChromaFormat_444
            } else {
                sys::cudaVideoChromaFormat_420
            };
            caps.nBitDepthMinus8 = if ten_bit { 2 } else { 0 };
            let result = unsafe { (api.caps)(&mut caps) };
            result == sys::CUDA_SUCCESS && caps.bIsSupported != 0
        };
        cuda.current(|| {
            let mut hevc = 0;
            for (chroma444, ten_bit) in [(false, false), (false, true), (true, false), (true, true)]
            {
                if decodes(VideoCodec::H265, chroma444, ten_bit) {
                    hevc |= queqiao_core::protocol::hevc::bit(chroma444, ten_bit);
                }
            }
            Ok(Caps {
                h264: decodes(VideoCodec::H264, false, false),
                hevc,
                av1: decodes(VideoCodec::Av1, false, false),
            })
        })
        .ok()
    })
}

/// The stream as the decoder was made for it.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Shape {
    width: u32,
    height: u32,
    chroma444: bool,
    /// Bits over 8 (0 or 2).
    depth: u32,
    surfaces: u32,
}

/// What the parser's callbacks share with the decoder (they run inside `cuvidParseVideoData`).
struct Shared {
    api: *const Api,
    decoder: sys::CUvideodecoder,
    shape: Option<Shape>,
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
    if !chroma444 && format.chroma_format != sys::cudaVideoChromaFormat_420 {
        shared.error = Some("NVDEC: a stream neither 4:2:0 nor 4:4:4".into());
        return 0;
    }
    let surfaces = u32::from(format.min_num_decode_surfaces).max(4) + 2;
    let shape = Shape {
        width,
        height,
        chroma444,
        depth,
        surfaces,
    };
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
    create.bitDepthMinus8 = depth as _;
    create.DeinterlaceMode = sys::cudaVideoDeinterlaceMode_Weave;
    create.ulWidth = format.coded_width as _;
    create.ulHeight = format.coded_height as _;
    create.ulMaxWidth = format.coded_width as _;
    create.ulMaxHeight = format.coded_height as _;
    create.ulTargetWidth = width as _;
    create.ulTargetHeight = height as _;
    create.display_area.left = area.left as i16;
    create.display_area.top = area.top as i16;
    create.display_area.right = area.right as i16;
    create.display_area.bottom = area.bottom as i16;
    create.ulNumDecodeSurfaces = surfaces as _;
    create.ulNumOutputSurfaces = 2;
    create.ulCreationFlags = sys::cudaVideoCreate_PreferCUVID as _;
    let mut decoder: sys::CUvideodecoder = null_mut();
    let result = unsafe { (api.create_decoder)(&mut decoder, &mut create) };
    if result != sys::CUDA_SUCCESS {
        shared.error = Some(format!(
            "NVDEC cannot decode this stream ({width}x{height}, {}, {} bits): CUDA error {result}",
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

pub(crate) struct NvdecDecoder {
    // Dropped in this order: the parser and decoder (in `drop`), the functions, then the CUDA context.
    api: Box<Api>,
    parser: sys::CUvideoparser,
    shared: Box<Shared>,
    codec: VideoCodec,
    pub(crate) name: String,
    cuda: Cuda,
}

impl NvdecDecoder {
    pub(crate) fn new(codec: VideoCodec) -> Result<NvdecDecoder, String> {
        let caps = caps().ok_or("NVDEC: no NVIDIA GPU with its decoder here")?;
        let supported = match codec {
            VideoCodec::H264 => caps.h264,
            VideoCodec::H265 => caps.hevc != 0,
            VideoCodec::Av1 => caps.av1,
        };
        if !supported {
            return Err(format!(
                "NVDEC: this GPU does not decode {}",
                codec_name(codec)
            ));
        }
        let cuda = Cuda::open(0)?;
        let api = Box::new(Api::open()?);
        let mut shared = Box::new(Shared {
            api: &*api,
            decoder: null_mut(),
            shape: None,
            shown: VecDeque::new(),
            error: None,
        });
        let mut params: sys::CUVIDPARSERPARAMS = unsafe { zeroed() };
        params.CodecType = cuvid_codec(codec);
        params.ulMaxNumDecodeSurfaces = 1;
        params.ulMaxDisplayDelay = 0;
        params.pUserData = (&mut *shared as *mut Shared).cast();
        params.pfnSequenceCallback = Some(on_sequence);
        params.pfnDecodePicture = Some(on_decode);
        params.pfnDisplayPicture = Some(on_display);
        let mut parser: sys::CUvideoparser = null_mut();
        let made = unsafe { (api.create_parser)(&mut parser, &mut params) };
        if made != sys::CUDA_SUCCESS {
            return Err(format!("NVDEC parser: CUDA error {made}"));
        }
        Ok(NvdecDecoder {
            name: format!(
                "NVIDIA NVDEC {} decoder (CUDA on {})",
                codec_name(codec),
                cuda.device_name
            ),
            api,
            parser,
            shared,
            codec,
            cuda,
        })
    }

    /// What the stream turned out to be ("H.265 4:4:4 10-bit"), once its headers were read.
    pub(crate) fn stream(&self) -> String {
        match self.shared.shape {
            Some(shape) => format!(
                "{} {}{}",
                codec_name(self.codec),
                if shape.chroma444 { "4:4:4" } else { "4:2:0" },
                if shape.depth > 0 { " 10-bit" } else { "" }
            ),
            None => String::new(),
        }
    }

    /// Decodes one access unit and appends its picture (in memory) to `out`.
    pub(crate) fn decode(
        &mut self,
        data: &[u8],
        time: i64,
        out: &mut Vec<DecodedFrame>,
    ) -> Result<(), String> {
        let (api, parser, shared) = (&self.api, self.parser, &mut self.shared);
        let cuda = &self.cuda;
        cuda.current(|| {
            let mut packet: sys::CUVIDSOURCEDATAPACKET = unsafe { zeroed() };
            packet.flags = (sys::CUVID_PKT_ENDOFPICTURE | sys::CUVID_PKT_TIMESTAMP) as _;
            packet.payload_size = data.len() as _;
            packet.payload = data.as_ptr();
            packet.timestamp = time;
            let parsed = unsafe { (api.parse)(parser, &mut packet) };
            if let Some(error) = shared.error.take() {
                return Err(error);
            }
            if parsed != sys::CUDA_SUCCESS {
                return Err(format!("NVDEC parse: CUDA error {parsed}"));
            }
            while let Some(index) = shared.shown.pop_front() {
                out.push(show(api, cuda, shared, index, time)?);
            }
            Ok(())
        })
    }
}

/// NVDEC's surface `index` copied into memory: its planes one after another, as the picture's format says.
fn show(
    api: &Api,
    cuda: &Cuda,
    shared: &Shared,
    index: i32,
    time: i64,
) -> Result<DecodedFrame, String> {
    let shape = shared.shape.ok_or("NVDEC: a picture before the sequence")?;
    let mut frame: u64 = 0;
    let mut pitch: u32 = 0;
    let mut process: sys::CUVIDPROCPARAMS = unsafe { zeroed() };
    process.progressive_frame = 1;
    let mapped =
        unsafe { (api.map_frame)(shared.decoder, index, &mut frame, &mut pitch, &mut process) };
    if mapped != sys::CUDA_SUCCESS {
        return Err(format!("NVDEC map picture: CUDA error {mapped}"));
    }
    // The surface's planes start every `aligned` rows (its height, made even, as on Windows); the picture keeps even
    // sizes (4:2:0's chroma is 2x2).
    let aligned = ((shape.height + 1) & !1) as usize;
    let (width, height) = ((shape.width & !1) as usize, (shape.height & !1) as usize);
    let sample = if shape.depth > 0 { 2 } else { 1 };
    let row = width * sample;
    // (first row on the surface, rows) of each plane.
    let planes: Vec<(usize, usize)> = if shape.chroma444 {
        (0..3).map(|plane| (plane * aligned, height)).collect()
    } else {
        vec![(0, height), (aligned, height / 2)]
    };
    let mut data = vec![0u8; row * planes.iter().map(|(_, rows)| rows).sum::<usize>()];
    let mut copied = Ok(());
    let mut rest = data.as_mut_slice();
    for (first, rows) in planes {
        let (target, after) = rest.split_at_mut(row * rows);
        rest = after;
        copied = copied.and_then(|()| {
            cuda.download(
                frame + (first * pitch as usize) as u64,
                pitch as usize,
                target,
                row,
                row,
                rows,
            )
        });
    }
    unsafe { (api.unmap_frame)(shared.decoder, frame) };
    copied?;
    Ok(DecodedFrame {
        texture: Picture {
            data,
            pitch: row,
            rows: height,
            format: match (shape.chroma444, shape.depth > 0) {
                (false, false) => PictureFormat::Nv12,
                (false, true) => PictureFormat::P016,
                (true, false) => PictureFormat::Yuv444,
                (true, true) => PictureFormat::Yuv444P16,
            },
        },
        width: width as u32,
        height: height as u32,
        time,
    })
}

impl Drop for NvdecDecoder {
    fn drop(&mut self) {
        let (api, parser, decoder) = (&self.api, self.parser, self.shared.decoder);
        let _ = self.cuda.current(|| {
            if !parser.is_null() {
                unsafe { (api.destroy_parser)(parser) };
            }
            if !decoder.is_null() {
                unsafe { (api.destroy_decoder)(decoder) };
            }
            Ok(())
        });
    }
}
