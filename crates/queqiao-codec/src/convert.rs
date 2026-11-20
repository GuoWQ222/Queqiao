//! BGRA -> NV12 (the format H.264 encoders take), scaled when the encode size differs from the screen.
//!
//! First choice: the D3D11 video processor on the GPU. Fallback: the CPU (`cpu_convert`), for devices
//! without video support (Microsoft's software adapter WARP, basic display drivers, some virtual
//! machines). `QUEQIAO_FORCE=convert:cpu` forces the CPU path for testing.

use std::mem::ManuallyDrop;
use std::ptr::null_mut;

use windows::Win32::Foundation::E_UNEXPECTED;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::core::{Error, Interface, Result};

use crate::cpu_convert::{Tap, bgra_to_nv12, taps};

/// Converts BGRA textures (desktop images, full-range RGB) into NV12 textures (BT.709, limited range
/// 16-235, which is what H.264 decoders expect by default).
pub struct Nv12Converter {
    method: Method,
    width: u32,
    height: u32,
    /// Why the GPU path was not used, if it was not.
    fallback: Option<String>,
}

enum Method {
    Gpu(VideoProcessor),
    Cpu(Box<CpuConverter>),
}

impl Nv12Converter {
    /// `input` is the BGRA size, `output` the NV12 size (both width, height; the output must be even).
    pub fn new(
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
        input: (u32, u32),
        output: (u32, u32),
        fps: u32,
    ) -> Result<Self> {
        let forced_cpu = queqiao_core::forced("convert").as_deref() == Some("cpu");
        let gpu = if forced_cpu {
            Err("CPU forced (QUEQIAO_FORCE=convert:cpu)".to_string())
        } else {
            VideoProcessor::new(device, context, input, output, fps).map_err(|e| e.message())
        };
        let (method, fallback) = match gpu {
            Ok(processor) => (Method::Gpu(processor), None),
            Err(reason) => {
                let cpu = CpuConverter::new(device, context, input, output);
                (Method::Cpu(Box::new(cpu)), Some(reason))
            }
        };
        Ok(Self {
            method,
            width: output.0,
            height: output.1,
            fallback,
        })
    }

    /// "GPU video processor" or "CPU (<why the GPU path was not used>)".
    pub fn description(&self) -> String {
        match (&self.method, &self.fallback) {
            (Method::Gpu(_), _) => "GPU video processor".into(),
            (Method::Cpu(_), Some(reason)) => format!("CPU ({reason})"),
            (Method::Cpu(_), None) => "CPU".into(),
        }
    }

    /// An NV12 texture that [`Nv12Converter::convert`] can write and the encoder can read.
    pub fn create_output(&self, device: &ID3D11Device) -> Result<ID3D11Texture2D> {
        // The video processor renders into the texture; the CPU path uploads with UpdateSubresource.
        let bind = match self.method {
            Method::Gpu(_) => D3D11_BIND_RENDER_TARGET.0 as u32,
            Method::Cpu(_) => 0,
        };
        let desc = D3D11_TEXTURE2D_DESC {
            Width: self.width,
            Height: self.height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_NV12,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: bind,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let mut out: Option<ID3D11Texture2D> = None;
        unsafe { device.CreateTexture2D(&desc, None, Some(&mut out)) }?;
        out.ok_or_else(|| Error::from(E_UNEXPECTED))
    }

    /// Converts `source` (BGRA, the input size) into `target` (NV12, from `create_output`).
    pub fn convert(&mut self, source: &ID3D11Texture2D, target: &ID3D11Texture2D) -> Result<()> {
        match &mut self.method {
            Method::Gpu(processor) => processor.convert(source, target),
            Method::Cpu(cpu) => cpu.convert(source, target),
        }
    }
}

/// The D3D11 video processor. Its work is queued on the GPU; the encoder reading the target on the
/// same device waits for it.
struct VideoProcessor {
    video_device: ID3D11VideoDevice,
    video_context: ID3D11VideoContext,
    enumerator: ID3D11VideoProcessorEnumerator,
    processor: ID3D11VideoProcessor,
}

impl VideoProcessor {
    fn new(
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
        input: (u32, u32),
        output: (u32, u32),
        fps: u32,
    ) -> Result<Self> {
        let video_device: ID3D11VideoDevice = device.cast()?;
        let video_context: ID3D11VideoContext = context.cast()?;
        let rate = DXGI_RATIONAL {
            Numerator: fps.max(1),
            Denominator: 1,
        };
        let content = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
            InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            InputFrameRate: rate,
            InputWidth: input.0,
            InputHeight: input.1,
            OutputFrameRate: rate,
            OutputWidth: output.0,
            OutputHeight: output.1,
            Usage: D3D11_VIDEO_USAGE_OPTIMAL_SPEED,
        };
        let enumerator = unsafe { video_device.CreateVideoProcessorEnumerator(&content) }?;
        let processor = unsafe { video_device.CreateVideoProcessor(&enumerator, 0) }?;
        unsafe { video_context.VideoProcessorSetStreamAutoProcessingMode(&processor, 0, false) };
        set_color_spaces(&video_context, &processor);
        Ok(Self {
            video_device,
            video_context,
            enumerator,
            processor,
        })
    }

    fn convert(&self, source: &ID3D11Texture2D, target: &ID3D11Texture2D) -> Result<()> {
        let input_desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
            FourCC: 0,
            ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
                Texture2D: D3D11_TEX2D_VPIV {
                    MipSlice: 0,
                    ArraySlice: 0,
                },
            },
        };
        let output_desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
            ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 {
                Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 },
            },
        };
        let mut input: Option<ID3D11VideoProcessorInputView> = None;
        let mut output: Option<ID3D11VideoProcessorOutputView> = None;
        let vd = &self.video_device;
        let en = &self.enumerator;
        unsafe {
            vd.CreateVideoProcessorInputView(source, en, &input_desc, Some(&mut input))?;
            vd.CreateVideoProcessorOutputView(target, en, &output_desc, Some(&mut output))?;
        }
        let output = output.ok_or_else(|| Error::from(E_UNEXPECTED))?;
        let stream = D3D11_VIDEO_PROCESSOR_STREAM {
            Enable: true.into(),
            OutputIndex: 0,
            InputFrameOrField: 0,
            PastFrames: 0,
            FutureFrames: 0,
            ppPastSurfaces: null_mut(),
            pInputSurface: ManuallyDrop::new(input),
            ppFutureSurfaces: null_mut(),
            ppPastSurfacesRight: null_mut(),
            pInputSurfaceRight: ManuallyDrop::new(None),
            ppFutureSurfacesRight: null_mut(),
        };
        let streams = std::slice::from_ref(&stream);
        let vc = &self.video_context;
        let result = unsafe { vc.VideoProcessorBlt(&self.processor, &output, 0, streams) };
        // The stream struct holds the input view in a ManuallyDrop: release it here.
        drop(ManuallyDrop::into_inner(stream.pInputSurface));
        result
    }
}

/// CPU path: BGRA texture -> memory -> scaled NV12 in memory -> NV12 texture.
struct CpuConverter {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    output_width: u32,
    columns: Vec<Tap>,
    rows: Vec<Tap>,
    staging: Option<ID3D11Texture2D>,
    nv12: Vec<u8>,
}

impl CpuConverter {
    fn new(
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
        input: (u32, u32),
        output: (u32, u32),
    ) -> Self {
        Self {
            device: device.clone(),
            context: context.clone(),
            output_width: output.0,
            columns: taps(input.0, output.0),
            rows: taps(input.1, output.1),
            staging: None,
            nv12: Vec::new(),
        }
    }

    fn convert(&mut self, source: &ID3D11Texture2D, target: &ID3D11Texture2D) -> Result<()> {
        let staging = self.staging_for(source)?;
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        unsafe {
            self.context.CopyResource(&staging, source);
            self.context
                .Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
        }
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { staging.GetDesc(&mut desc) };
        let pitch = mapped.RowPitch as usize;
        let length = pitch * desc.Height as usize;
        let bgra = unsafe { std::slice::from_raw_parts(mapped.pData.cast::<u8>(), length) };
        bgra_to_nv12(bgra, pitch, &self.columns, &self.rows, &mut self.nv12);
        unsafe {
            self.context.Unmap(&staging, 0);
            let data = self.nv12.as_ptr().cast();
            self.context
                .UpdateSubresource(target, 0, None, data, self.output_width, 0);
        }
        Ok(())
    }

    fn staging_for(&mut self, source: &ID3D11Texture2D) -> Result<ID3D11Texture2D> {
        if let Some(staging) = &self.staging {
            return Ok(staging.clone());
        }
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { source.GetDesc(&mut desc) };
        desc.Usage = D3D11_USAGE_STAGING;
        desc.BindFlags = 0;
        desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
        desc.MiscFlags = 0;
        let mut out: Option<ID3D11Texture2D> = None;
        unsafe { self.device.CreateTexture2D(&desc, None, Some(&mut out)) }?;
        let staging = out.ok_or_else(|| Error::from(E_UNEXPECTED))?;
        self.staging = Some(staging.clone());
        Ok(staging)
    }
}

/// Input full-range RGB, output BT.709 limited range. Windows 10 drivers take the DXGI color space
/// (ID3D11VideoContext1); older ones only the D3D11 bit field, set the same way.
fn set_color_spaces(context: &ID3D11VideoContext, processor: &ID3D11VideoProcessor) {
    if let Ok(context1) = context.cast::<ID3D11VideoContext1>() {
        unsafe {
            let rgb = DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709;
            let yuv = DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709;
            context1.VideoProcessorSetStreamColorSpace1(processor, 0, rgb);
            context1.VideoProcessorSetOutputColorSpace1(processor, yuv);
        }
        return;
    }
    // Bit field: bit 1 RGB_Range (0 = full 0-255), bit 2 YCbCr_Matrix (1 = BT.709),
    // bits 4-5 Nominal_Range (1 = 16-235).
    let input = D3D11_VIDEO_PROCESSOR_COLOR_SPACE { _bitfield: 0b100 };
    let output = D3D11_VIDEO_PROCESSOR_COLOR_SPACE {
        _bitfield: 0b1_0100,
    };
    unsafe {
        context.VideoProcessorSetStreamColorSpace(processor, 0, &input);
        context.VideoProcessorSetOutputColorSpace(processor, &output);
    }
}
