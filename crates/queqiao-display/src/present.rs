//! Shows decoded NV12 pictures in a window through a D3D11 swap chain. The flip model (lowest latency,
//! Windows 10) is used when available, the older blit model otherwise. Pictures are converted to BGRA
//! and scaled to the window (aspect kept, black bars) by the GPU video processor, or by the CPU on
//! devices without one (WARP, basic display drivers). Presenting never waits for the display refresh.

use std::mem::ManuallyDrop;
use std::ptr::null_mut;

use windows::Win32::Foundation::{E_UNEXPECTED, RECT};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::core::{Error, Interface, Result};

use crate::window::Window;
use crate::yuv::nv12_to_bgra;

pub struct Presenter {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    swap_chain: IDXGISwapChain1,
    window_size: (u32, u32),
    flip_model: bool,
    method: Option<Method>,
    gpu_refused: Option<String>,
}

enum Method {
    Gpu(Box<GpuBlit>),
    Cpu(Box<CpuBlit>),
}

impl Presenter {
    pub fn new(
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
        window: &Window,
    ) -> Result<Self> {
        let size = window.client_size();
        let factory: IDXGIFactory2 =
            unsafe { device.cast::<IDXGIDevice>()?.GetAdapter()?.GetParent() }?;
        let mut desc = DXGI_SWAP_CHAIN_DESC1 {
            Width: size.0,
            Height: size.1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
            BufferCount: 2,
            Scaling: DXGI_SCALING_STRETCH,
            SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
            AlphaMode: DXGI_ALPHA_MODE_IGNORE,
            ..Default::default()
        };
        let hwnd = window.hwnd();
        let none = None::<&IDXGIOutput>;
        let (swap_chain, flip_model) =
            match unsafe { factory.CreateSwapChainForHwnd(device, hwnd, &desc, None, none) } {
                Ok(chain) => (chain, true),
                Err(_) => {
                    // Before Windows 10 or on drivers without flip-model support.
                    desc.SwapEffect = DXGI_SWAP_EFFECT_DISCARD;
                    desc.BufferCount = 1;
                    let chain =
                        unsafe { factory.CreateSwapChainForHwnd(device, hwnd, &desc, None, none) }?;
                    (chain, false)
                }
            };
        Ok(Self {
            device: device.clone(),
            context: context.clone(),
            swap_chain,
            window_size: size,
            flip_model,
            method: None,
            gpu_refused: None,
        })
    }

    /// For reports: swap-chain model and conversion path.
    pub fn description(&self) -> String {
        let model = if self.flip_model {
            "flip model"
        } else {
            "blit model"
        };
        let method = match (&self.method, &self.gpu_refused) {
            (Some(Method::Gpu(_)), _) => "GPU video processor".to_string(),
            (Some(Method::Cpu(_)), Some(reason)) => format!("CPU ({reason})"),
            (Some(Method::Cpu(_)), None) => "CPU".to_string(),
            (None, _) => "nothing shown yet".to_string(),
        };
        format!("{model}, {method}")
    }

    /// Shows an NV12 picture of `size` (width, height) in the window and presents it.
    pub fn show(
        &mut self,
        nv12: &ID3D11Texture2D,
        size: (u32, u32),
        window: &Window,
    ) -> Result<()> {
        let window_size = window.client_size();
        if window_size != self.window_size {
            self.method = None;
            let format = DXGI_FORMAT_UNKNOWN;
            let flags = DXGI_SWAP_CHAIN_FLAG(0);
            unsafe {
                self.swap_chain
                    .ResizeBuffers(0, window_size.0, window_size.1, format, flags)
            }?;
            self.window_size = window_size;
        }
        let back: ID3D11Texture2D = unsafe { self.swap_chain.GetBuffer(0) }?;
        let stale = match &self.method {
            Some(Method::Gpu(blit)) => blit.input != size,
            Some(Method::Cpu(blit)) => blit.input != size,
            None => true,
        };
        if stale {
            self.method = Some(self.choose(size)?);
        }
        let drawn = match self.method.as_mut() {
            Some(Method::Gpu(blit)) => blit.draw(nv12, &back),
            Some(Method::Cpu(blit)) => blit.draw(nv12, &back),
            None => Err(Error::from(E_UNEXPECTED)),
        };
        let gpu_failed = drawn.is_err() && matches!(self.method, Some(Method::Gpu(_)));
        if gpu_failed {
            // A driver may refuse the decoder's textures as video processor input: use the CPU from now on.
            self.gpu_refused = drawn.err().map(|error| error.message());
            let mut cpu = CpuBlit::new(&self.device, &self.context, size, self.window_size);
            cpu.draw(nv12, &back)?;
            self.method = Some(Method::Cpu(Box::new(cpu)));
        } else {
            drawn?;
        }
        drop(back);
        unsafe { self.swap_chain.Present(0, DXGI_PRESENT(0)) }.ok()
    }

    fn choose(&mut self, input: (u32, u32)) -> Result<Method> {
        if self.gpu_refused.is_none() {
            match GpuBlit::new(&self.device, &self.context, input, self.window_size) {
                Ok(blit) => return Ok(Method::Gpu(Box::new(blit))),
                Err(error) => self.gpu_refused = Some(error.message()),
            }
        }
        let cpu = CpuBlit::new(&self.device, &self.context, input, self.window_size);
        Ok(Method::Cpu(Box::new(cpu)))
    }
}

/// The aspect-preserving area of `output` that an `input`-sized picture fills, centred.
pub fn fit(input: (u32, u32), output: (u32, u32)) -> RECT {
    let scale =
        (f64::from(output.0) / f64::from(input.0)).min(f64::from(output.1) / f64::from(input.1));
    let width = ((f64::from(input.0) * scale).round() as i32).clamp(1, output.0 as i32);
    let height = ((f64::from(input.1) * scale).round() as i32).clamp(1, output.1 as i32);
    let left = (output.0 as i32 - width) / 2;
    let top = (output.1 as i32 - height) / 2;
    RECT {
        left,
        top,
        right: left + width,
        bottom: top + height,
    }
}

/// NV12 -> BGRA on the GPU, straight into the swap chain's buffer.
struct GpuBlit {
    input: (u32, u32),
    /// Whether the pictures come as RGB (NVIDIA's decoder converts them itself), as the stream's colour space was
    /// last set.
    rgb_input: Option<bool>,
    video_device: ID3D11VideoDevice,
    video_context: ID3D11VideoContext,
    enumerator: ID3D11VideoProcessorEnumerator,
    processor: ID3D11VideoProcessor,
}

impl GpuBlit {
    fn new(
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
        input: (u32, u32),
        output: (u32, u32),
    ) -> Result<Self> {
        let video_device: ID3D11VideoDevice = device.cast()?;
        let video_context: ID3D11VideoContext = context.cast()?;
        let rate = DXGI_RATIONAL {
            Numerator: 60,
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
        let whole = RECT {
            left: 0,
            top: 0,
            right: output.0 as i32,
            bottom: output.1 as i32,
        };
        let picture = fit(input, output);
        let black = D3D11_VIDEO_COLOR {
            Anonymous: D3D11_VIDEO_COLOR_0 {
                RGBA: D3D11_VIDEO_COLOR_RGBA {
                    R: 0.0,
                    G: 0.0,
                    B: 0.0,
                    A: 1.0,
                },
            },
        };
        unsafe {
            let vc = &video_context;
            vc.VideoProcessorSetStreamAutoProcessingMode(&processor, 0, false);
            vc.VideoProcessorSetStreamFrameFormat(
                &processor,
                0,
                D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            );
            vc.VideoProcessorSetOutputTargetRect(&processor, true, Some(&whole));
            vc.VideoProcessorSetStreamDestRect(&processor, 0, true, Some(&picture));
            vc.VideoProcessorSetOutputBackgroundColor(&processor, false, &black);
            if let Ok(vc1) = video_context.cast::<ID3D11VideoContext1>() {
                let yuv = DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709;
                vc1.VideoProcessorSetStreamColorSpace1(&processor, 0, yuv);
                vc1.VideoProcessorSetOutputColorSpace1(
                    &processor,
                    DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709,
                );
            } else {
                // Bit field: bit 2 YCbCr_Matrix = BT.709, bits 4-5 Nominal_Range = 16-235; output RGB full.
                let stream = D3D11_VIDEO_PROCESSOR_COLOR_SPACE {
                    _bitfield: 0b1_0100,
                };
                let out = D3D11_VIDEO_PROCESSOR_COLOR_SPACE { _bitfield: 0 };
                vc.VideoProcessorSetStreamColorSpace(&processor, 0, &stream);
                vc.VideoProcessorSetOutputColorSpace(&processor, &out);
            }
        }
        Ok(Self {
            input,
            rgb_input: None,
            video_device,
            video_context,
            enumerator,
            processor,
        })
    }

    fn draw(&mut self, nv12: &ID3D11Texture2D, back: &ID3D11Texture2D) -> Result<()> {
        // YUV pictures (NV12, P010, AYUV, Y410) are BT.709 limited range; BGRA ones (NVDEC's, converted on the GPU)
        // full-range RGB already.
        let mut picture_desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { nv12.GetDesc(&mut picture_desc) };
        let rgb = matches!(
            picture_desc.Format,
            DXGI_FORMAT_B8G8R8A8_UNORM | DXGI_FORMAT_R8G8B8A8_UNORM | DXGI_FORMAT_R10G10B10A2_UNORM
        );
        if self.rgb_input != Some(rgb) {
            self.rgb_input = Some(rgb);
            if let Ok(vc1) = self.video_context.cast::<ID3D11VideoContext1>() {
                let space = if rgb {
                    DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709
                } else {
                    DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709
                };
                unsafe { vc1.VideoProcessorSetStreamColorSpace1(&self.processor, 0, space) };
            } else {
                let stream = D3D11_VIDEO_PROCESSOR_COLOR_SPACE {
                    _bitfield: if rgb { 0 } else { 0b1_0100 },
                };
                unsafe {
                    self.video_context.VideoProcessorSetStreamColorSpace(
                        &self.processor,
                        0,
                        &stream,
                    )
                };
            }
        }
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
        let (vd, en) = (&self.video_device, &self.enumerator);
        unsafe {
            vd.CreateVideoProcessorInputView(nv12, en, &input_desc, Some(&mut input))?;
            vd.CreateVideoProcessorOutputView(back, en, &output_desc, Some(&mut output))?;
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
        drop(ManuallyDrop::into_inner(stream.pInputSurface));
        result
    }
}

/// NV12 -> BGRA on the CPU: picture read back, converted and scaled in memory, written into the buffer.
struct CpuBlit {
    input: (u32, u32),
    output: (u32, u32),
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    staging: Option<ID3D11Texture2D>,
    bgra: Vec<u8>,
}

impl CpuBlit {
    fn new(
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
        input: (u32, u32),
        output: (u32, u32),
    ) -> Self {
        Self {
            input,
            output,
            device: device.clone(),
            context: context.clone(),
            staging: None,
            bgra: Vec::new(),
        }
    }

    fn draw(&mut self, nv12: &ID3D11Texture2D, back: &ID3D11Texture2D) -> Result<()> {
        if self.staging.is_none() {
            let mut desc = D3D11_TEXTURE2D_DESC::default();
            unsafe { nv12.GetDesc(&mut desc) };
            desc.Usage = D3D11_USAGE_STAGING;
            desc.BindFlags = 0;
            desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
            desc.MiscFlags = 0;
            let mut out: Option<ID3D11Texture2D> = None;
            unsafe { self.device.CreateTexture2D(&desc, None, Some(&mut out)) }?;
            self.staging = out;
        }
        let staging = self
            .staging
            .as_ref()
            .ok_or_else(|| Error::from(E_UNEXPECTED))?;
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        unsafe {
            self.context.CopyResource(staging, nv12);
            self.context
                .Map(staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
        }
        let (width, height) = (self.input.0 as usize, self.input.1 as usize);
        let pitch = mapped.RowPitch as usize;
        let data = unsafe {
            std::slice::from_raw_parts(mapped.pData.cast::<u8>(), pitch * height * 3 / 2)
        };
        let out_size = (self.output.0 as usize, self.output.1 as usize);
        self.bgra.resize(out_size.0 * out_size.1 * 4, 0);
        nv12_to_bgra(data, pitch, (width, height), &mut self.bgra, out_size);
        unsafe {
            self.context.Unmap(staging, 0);
            let row_pitch = self.output.0 * 4;
            let bytes = self.bgra.as_ptr().cast();
            self.context
                .UpdateSubresource(back, 0, None, bytes, row_pitch, 0);
        }
        Ok(())
    }
}
