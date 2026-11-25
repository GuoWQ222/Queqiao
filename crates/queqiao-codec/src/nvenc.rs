//! NVIDIA's encoder (NVENC) on Windows, driven directly through the Video Codec SDK API beside the Media
//! Foundation path (UU Remote uses the vendor SDKs directly): the shared session (nvenc_core.rs) on the D3D11
//! device, fed with the converter's textures. The driver's `nvEncodeAPI64.dll` is loaded at run time, so the
//! program still starts on PCs without NVIDIA graphics.
//!
//! Formats (`EncodeFormat`): H.264 4:2:0 or 4:4:4, H.265 4:2:0 or 4:4:4, H.265 in 8 or 10 bits; 4:4:4 from RGB
//! textures, which NVENC converts to YUV 4:4:4 itself. AV1 (8-bit 4:2:0, from NV12) where the driver has API 12.

use windows::Win32::Foundation::{E_FAIL, FreeLibrary, HMODULE};
use windows::Win32::Graphics::Direct3D11::{D3D11_TEXTURE2D_DESC, ID3D11Device, ID3D11Texture2D};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_R10G10B10A2_UNORM,
};
use windows::Win32::System::LibraryLoader::{
    GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW,
};
use windows::core::{Error, Interface, Result, s, w};

use crate::encoder::{EncodedFrame, EncoderConfig};
use crate::format::EncodeFormat;
use crate::nvenc_core::{self, CreateInstance, MaxVersion, NvencCore};

/// The texture NVENC takes for `format`: YUV 4:2:0 from the converter (NV12, P010); for 4:4:4 the screen's RGB
/// (scaled only), which NVENC turns into YUV 4:4:4 itself (its D3D11 input has no packed 10-bit YUV 4:4:4).
fn input_format(format: EncodeFormat) -> DXGI_FORMAT {
    match (format.chroma444, format.ten_bit) {
        (true, false) => DXGI_FORMAT_B8G8R8A8_UNORM,
        (true, true) => DXGI_FORMAT_R10G10B10A2_UNORM,
        _ => format.input(),
    }
}

/// The driver's library, freed when the encoder goes, after its session (`NvencEncoder`'s fields drop in order).
struct Library(HMODULE);

impl Drop for Library {
    fn drop(&mut self) {
        let _ = unsafe { FreeLibrary(self.0) };
    }
}

pub(crate) struct NvencEncoder {
    core: NvencCore,
    _library: Library,
}

impl NvencEncoder {
    pub(crate) fn open(
        device: &ID3D11Device,
        config: &EncoderConfig,
        format: EncodeFormat,
    ) -> Result<Self> {
        nvenc_core::supports(format).map_err(failed)?;
        let library =
            unsafe { LoadLibraryExW(w!("nvEncodeAPI64.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32) }
                .map_err(|e| {
                    Error::new(e.code(), "no NVIDIA encoder driver (nvEncodeAPI64.dll)")
                })?;
        let library = Library(library);
        let max_version =
            unsafe { GetProcAddress(library.0, s!("NvEncodeAPIGetMaxSupportedVersion")) }
                .ok_or_else(|| Error::new(E_FAIL, "NvEncodeAPIGetMaxSupportedVersion missing"))?;
        let max_version: MaxVersion = unsafe { std::mem::transmute(max_version) };
        let create = unsafe { GetProcAddress(library.0, s!("NvEncodeAPICreateInstance")) }
            .ok_or_else(|| Error::new(E_FAIL, "NvEncodeAPICreateInstance missing"))?;
        let create: CreateInstance = unsafe { std::mem::transmute(create) };
        let core = NvencCore::open(
            create,
            max_version,
            nvenc_core::DEVICE_DIRECTX,
            device.as_raw(),
            config,
            format,
        )
        .map_err(failed)?;
        Ok(Self {
            core,
            _library: library,
        })
    }

    pub(crate) fn name(&self) -> &str {
        self.core.name()
    }

    pub(crate) fn format(&self) -> EncodeFormat {
        self.core.format()
    }

    pub(crate) fn poll(&mut self, out: &mut Vec<EncodedFrame>) {
        self.core.poll(out);
    }

    /// The texture format `encode` takes.
    pub(crate) fn input_format(&self) -> DXGI_FORMAT {
        input_format(self.core.format())
    }

    /// Encodes one texture (in `input_format`) now; the frame is ready when this returns. Each texture is
    /// registered with NVENC the first time it is seen.
    pub(crate) fn encode(
        &mut self,
        input: &ID3D11Texture2D,
        time: i64,
        keyframe: bool,
    ) -> Result<()> {
        let key = input.as_raw() as usize;
        let registered = match self.core.registered(key) {
            Some(registered) => registered,
            None => {
                let mut desc = D3D11_TEXTURE2D_DESC::default();
                unsafe { input.GetDesc(&mut desc) };
                self.core
                    .register(
                        key,
                        nvenc_core::RESOURCE_DIRECTX,
                        input.as_raw(),
                        (desc.Width, desc.Height),
                        0,
                    )
                    .map_err(failed)?
            }
        };
        self.core.encode(registered, time, keyframe).map_err(failed)
    }

    /// Changes the bitrate of the running session (no keyframe, no reset).
    pub(crate) fn set_bitrate(&mut self, bitrate: u32) -> Result<()> {
        self.core.set_bitrate(bitrate).map_err(failed)
    }
}

fn failed(message: String) -> Error {
    Error::new(E_FAIL, message)
}
