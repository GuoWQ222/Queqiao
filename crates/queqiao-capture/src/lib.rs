//! Screen capture with DXGI Desktop Duplication.
//!
//! Frames stay on the GPU: [`Duplicator::acquire`] returns the desktop image as a D3D11 texture on the
//! duplicator's device, where the encoder will read it. Needs a Direct3D 11 GPU and a process in the
//! interactive session (not session 0, so not straight from an SSH shell). The process should be
//! per-monitor DPI aware so that sizes are real pixels.

use windows::Win32::Foundation::{E_UNEXPECTED, HMODULE};
use windows::Win32::Graphics::Direct3D::*;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows::core::{Error, Interface, Result};

/// The screen being captured.
#[derive(Clone, Debug)]
pub struct OutputInfo {
    /// GPU name, e.g. "NVIDIA GeForce RTX 5080".
    pub adapter: String,
    /// Windows display name, e.g. `\\.\DISPLAY1`.
    pub output: String,
    pub width: u32,
    pub height: u32,
    pub refresh_hz: f64,
    /// Clockwise rotation of the display in degrees (0, 90, 180, 270).
    pub rotation: u32,
    /// `DuplicateOutput1` (we choose the pixel format) or the older `DuplicateOutput`.
    pub method: &'static str,
}

/// One screen update. The texture belongs to the duplication and stays valid until the next
/// [`Duplicator::acquire`] or [`Duplicator::release`].
pub struct Frame {
    pub texture: ID3D11Texture2D,
    /// QueryPerformanceCounter time at which the update reached the screen; 0 if only the mouse moved.
    pub present_qpc: i64,
    /// QueryPerformanceCounter time at which `acquire` returned.
    pub acquired_qpc: i64,
    /// Screen updates since the previous frame; more than 1 means updates were merged (we were late).
    pub accumulated: u32,
}

/// A frame copied to memory: tightly packed rows, 4 bytes per pixel in B, G, R, A order.
pub struct BgraImage {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

/// Desktop duplication of one screen, with the D3D11 device it runs on.
pub struct Duplicator {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    duplication: IDXGIOutputDuplication,
    info: OutputInfo,
    holding_frame: bool,
    staging: Option<(ID3D11Texture2D, (u32, u32))>,
}

impl Duplicator {
    /// Starts duplicating the main screen: the one whose top-left corner is the desktop's origin. On PCs
    /// with virtual display drivers (other remote-control tools install them) the first screen DXGI lists
    /// is not necessarily the real one.
    pub fn primary() -> Result<Self> {
        let screens = attached_outputs()?;
        let main = screens
            .iter()
            .position(|(_, _, desc)| {
                let origin = desc.DesktopCoordinates;
                origin.left == 0 && origin.top == 0
            })
            .unwrap_or(0);
        let Some((adapter, output, _)) = screens.into_iter().nth(main) else {
            return Err(Error::new(DXGI_ERROR_NOT_FOUND, "no screen is attached"));
        };
        Self::open(adapter, output)
    }

    /// Starts duplicating screen `index`, counting the screens attached to the desktop across all GPUs
    /// in DXGI's order. The D3D11 device is created on the GPU that drives that screen.
    pub fn new(index: usize) -> Result<Self> {
        let screens = attached_outputs()?;
        let count = screens.len();
        let Some((adapter, output, _)) = screens.into_iter().nth(index) else {
            let text = format!("screen {index} not found ({count} attached)");
            return Err(Error::new(DXGI_ERROR_NOT_FOUND, text));
        };
        Self::open(adapter, output)
    }

    fn open(adapter: IDXGIAdapter1, output: IDXGIOutput) -> Result<Self> {
        let (device, context) = create_device(&adapter)?;
        let (duplication, method) = duplicate(&output, &device)?;
        let desc = unsafe { duplication.GetDesc() };
        let adapter_desc = unsafe { adapter.GetDesc1() }?;
        let output_desc = unsafe { output.GetDesc() }?;
        let rate = desc.ModeDesc.RefreshRate;
        let info = OutputInfo {
            adapter: wide_to_string(&adapter_desc.Description),
            output: wide_to_string(&output_desc.DeviceName),
            width: desc.ModeDesc.Width,
            height: desc.ModeDesc.Height,
            refresh_hz: f64::from(rate.Numerator) / f64::from(rate.Denominator.max(1)),
            rotation: rotation_degrees(desc.Rotation),
            method,
        };
        Ok(Self {
            device,
            context,
            duplication,
            info,
            holding_frame: false,
            staging: None,
        })
    }

    pub fn info(&self) -> &OutputInfo {
        &self.info
    }

    /// The D3D11 device that owns the frame textures (the encoder must use the same device).
    pub fn device(&self) -> &ID3D11Device {
        &self.device
    }

    pub fn context(&self) -> &ID3D11DeviceContext {
        &self.context
    }

    /// Waits up to `timeout_ms` for the next screen update; `Ok(None)` if nothing changed in that time.
    /// Releases the previous frame first. After an error, check [`needs_restart`].
    ///
    /// The first frame after the duplication starts can be a mouse-only update (`present_qpc == 0`)
    /// whose texture is still empty (measured in M1 step 1: all black). Only frames with
    /// `present_qpc != 0` carry a new desktop image.
    pub fn acquire(&mut self, timeout_ms: u32) -> Result<Option<Frame>> {
        self.release()?;
        let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource: Option<IDXGIResource> = None;
        let dup = &self.duplication;
        let result = unsafe { dup.AcquireNextFrame(timeout_ms, &mut info, &mut resource) };
        let acquired_qpc = qpc_now();
        match result {
            Ok(()) => {}
            Err(error) if error.code() == DXGI_ERROR_WAIT_TIMEOUT => return Ok(None),
            Err(error) => return Err(error),
        }
        self.holding_frame = true;
        let resource = resource.ok_or_else(|| Error::from(E_UNEXPECTED))?;
        Ok(Some(Frame {
            texture: resource.cast()?,
            present_qpc: info.LastPresentTime,
            acquired_qpc,
            accumulated: info.AccumulatedFrames,
        }))
    }

    /// Gives the current frame back to Windows (the next `acquire` does this by itself).
    pub fn release(&mut self) -> Result<()> {
        if self.holding_frame {
            self.holding_frame = false;
            unsafe { self.duplication.ReleaseFrame() }?;
        }
        Ok(())
    }

    /// Copies a frame into memory. Slow path, for checks, thumbnails and a software encoder.
    pub fn copy_to_cpu(&mut self, frame: &Frame) -> Result<BgraImage> {
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { frame.texture.GetDesc(&mut desc) };
        let staging = self.staging_texture(&desc)?;
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        unsafe {
            self.context.CopyResource(&staging, &frame.texture);
            self.context
                .Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
        }
        let row_bytes = desc.Width as usize * 4;
        let mut pixels = vec![0u8; row_bytes * desc.Height as usize];
        let base = mapped.pData.cast::<u8>();
        let pitch = mapped.RowPitch as usize;
        for (y, row) in pixels.chunks_exact_mut(row_bytes).enumerate() {
            let source = unsafe { std::slice::from_raw_parts(base.add(y * pitch), row_bytes) };
            row.copy_from_slice(source);
        }
        unsafe { self.context.Unmap(&staging, 0) };
        Ok(BgraImage {
            width: desc.Width,
            height: desc.Height,
            pixels,
        })
    }

    /// CPU-readable texture of the frame's size, created once and reused.
    fn staging_texture(&mut self, frame: &D3D11_TEXTURE2D_DESC) -> Result<ID3D11Texture2D> {
        let size = (frame.Width, frame.Height);
        let cached = self.staging.as_ref().filter(|(_, s)| *s == size);
        if let Some((texture, _)) = cached {
            return Ok(texture.clone());
        }
        let desc = D3D11_TEXTURE2D_DESC {
            Usage: D3D11_USAGE_STAGING,
            BindFlags: 0,
            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
            MiscFlags: 0,
            ..*frame
        };
        let mut out: Option<ID3D11Texture2D> = None;
        unsafe { self.device.CreateTexture2D(&desc, None, Some(&mut out)) }?;
        let texture = out.ok_or_else(|| Error::from(E_UNEXPECTED))?;
        self.staging = Some((texture.clone(), size));
        Ok(texture)
    }
}

/// True if the duplication must be created again: desktop switch (login screen, UAC), display mode
/// change, or a full-screen exclusive application.
pub fn needs_restart(error: &Error) -> bool {
    error.code() == DXGI_ERROR_ACCESS_LOST
}

/// Current QueryPerformanceCounter value, the clock of [`Frame::present_qpc`].
pub fn qpc_now() -> i64 {
    let mut now = 0;
    let _ = unsafe { QueryPerformanceCounter(&mut now) };
    now
}

/// QueryPerformanceCounter ticks per second.
pub fn qpc_frequency() -> i64 {
    let mut frequency = 0;
    let _ = unsafe { QueryPerformanceFrequency(&mut frequency) };
    frequency
}

/// The screens attached to the desktop, across all GPUs, with the GPU each belongs to.
fn attached_outputs() -> Result<Vec<(IDXGIAdapter1, IDXGIOutput, DXGI_OUTPUT_DESC)>> {
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }?;
    let mut screens = Vec::new();
    for adapter_index in 0.. {
        let Ok(adapter) = (unsafe { factory.EnumAdapters1(adapter_index) }) else {
            break;
        };
        for output_index in 0.. {
            let Ok(output) = (unsafe { adapter.EnumOutputs(output_index) }) else {
                break;
            };
            let desc = unsafe { output.GetDesc() }?;
            if desc.AttachedToDesktop.as_bool() {
                screens.push((adapter.clone(), output, desc));
            }
        }
    }
    Ok(screens)
}

/// D3D11 device on `adapter`, with BGRA support (desktop images) and video support (hardware encoder).
fn create_device(adapter: &IDXGIAdapter1) -> Result<(ID3D11Device, ID3D11DeviceContext)> {
    let levels = [D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0];
    let flags = D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT;
    let mut device = None;
    let mut context = None;
    unsafe {
        D3D11CreateDevice(
            adapter,
            D3D_DRIVER_TYPE_UNKNOWN,
            HMODULE::default(),
            flags,
            Some(&levels),
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )
    }?;
    let missing = || Error::from(E_UNEXPECTED);
    Ok((device.ok_or_else(missing)?, context.ok_or_else(missing)?))
}

/// Starts the duplication. `DuplicateOutput1` asks for BGRA explicitly and needs per-monitor DPI
/// awareness; `DuplicateOutput` is the fallback.
fn duplicate(
    output: &IDXGIOutput,
    device: &ID3D11Device,
) -> Result<(IDXGIOutputDuplication, &'static str)> {
    if let Ok(output5) = output.cast::<IDXGIOutput5>() {
        let formats = [Common::DXGI_FORMAT_B8G8R8A8_UNORM];
        if let Ok(duplication) = unsafe { output5.DuplicateOutput1(device, 0, &formats) } {
            return Ok((duplication, "DuplicateOutput1"));
        }
    }
    let output1: IDXGIOutput1 = output.cast()?;
    Ok((
        unsafe { output1.DuplicateOutput(device) }?,
        "DuplicateOutput",
    ))
}

fn rotation_degrees(rotation: Common::DXGI_MODE_ROTATION) -> u32 {
    match rotation {
        Common::DXGI_MODE_ROTATION_ROTATE90 => 90,
        Common::DXGI_MODE_ROTATION_ROTATE180 => 180,
        Common::DXGI_MODE_ROTATION_ROTATE270 => 270,
        _ => 0,
    }
}

/// Text of a fixed-size UTF-16 field (ends at the first NUL).
fn wide_to_string(text: &[u16]) -> String {
    let len = text.iter().position(|&c| c == 0).unwrap_or(text.len());
    String::from_utf16_lossy(&text[..len])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_text_ends_at_nul() {
        let mut field = [0u16; 8];
        field[..3].copy_from_slice(&[0x52, 0x54, 0x58]);
        assert_eq!(wide_to_string(&field), "RTX");
        assert_eq!(wide_to_string(&[0x41, 0x42]), "AB");
    }
}
