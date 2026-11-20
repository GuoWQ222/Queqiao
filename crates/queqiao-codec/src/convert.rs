//! BGRA -> NV12 on the GPU with the D3D11 video processor. NV12 is the format hardware encoders take.

use std::mem::ManuallyDrop;
use std::ptr::null_mut;

use windows::Win32::Foundation::E_UNEXPECTED;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::core::{Error, Interface, Result};

/// Converts BGRA textures (desktop images, full-range RGB) into NV12 textures (BT.709, limited range
/// 16-235, which is what H.264 decoders expect by default), scaling when the output size differs.
/// Works on any Direct3D 11 GPU, including Microsoft's software adapter (WARP).
pub struct Nv12Converter {
    video_device: ID3D11VideoDevice,
    video_context: ID3D11VideoContext,
    enumerator: ID3D11VideoProcessorEnumerator,
    processor: ID3D11VideoProcessor,
    width: u32,
    height: u32,
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
            width: output.0,
            height: output.1,
        })
    }

    /// An NV12 texture that [`Nv12Converter::convert`] can write and the encoder can read.
    pub fn create_output(&self, device: &ID3D11Device) -> Result<ID3D11Texture2D> {
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
            BindFlags: D3D11_BIND_RENDER_TARGET.0 as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let mut out: Option<ID3D11Texture2D> = None;
        unsafe { device.CreateTexture2D(&desc, None, Some(&mut out)) }?;
        out.ok_or_else(|| Error::from(E_UNEXPECTED))
    }

    /// Converts `source` (BGRA, the converter's size) into `target` (NV12, from `create_output`).
    /// The work is queued on the GPU; the encoder reading `target` on the same device waits for it.
    pub fn convert(&self, source: &ID3D11Texture2D, target: &ID3D11Texture2D) -> Result<()> {
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
