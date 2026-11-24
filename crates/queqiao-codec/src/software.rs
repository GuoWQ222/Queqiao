//! Software H.264 encoder on Windows: OpenH264 (software_core.rs) fed from a texture.
//!
//! The frame still arrives as an NV12 texture (converted on the GPU, or on WARP without a GPU); it is
//! copied to memory and handed to OpenH264 as I420 (the U and V samples de-interleaved).

use std::time::Instant;

use windows::Win32::Foundation::{E_FAIL, E_UNEXPECTED};
use windows::Win32::Graphics::Direct3D11::*;
use windows::core::{Error, Result};

use crate::encoder::{EncodedFrame, EncoderConfig};
use crate::software_core::SoftwareCore;

pub(crate) struct SoftwareEncoder {
    pub(crate) core: SoftwareCore,
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    staging: Option<ID3D11Texture2D>,
}

impl SoftwareEncoder {
    pub(crate) fn open(
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
        config: &EncoderConfig,
    ) -> Result<Self> {
        let core = SoftwareCore::open(config).map_err(|e| Error::new(E_FAIL, e))?;
        Ok(Self {
            core,
            device: device.clone(),
            context: context.clone(),
            staging: None,
        })
    }

    /// Changes the target bitrate (all layers) for the next frames.
    pub(crate) fn set_bitrate(&mut self, bitrate: u32) -> Result<()> {
        self.core
            .set_bitrate(bitrate)
            .map_err(|e| Error::new(E_FAIL, e))
    }

    pub(crate) fn poll(&mut self, out: &mut Vec<EncodedFrame>) {
        self.core.poll(out);
    }

    /// Encodes one NV12 frame now (synchronously). `time` is in 100 ns units.
    pub(crate) fn encode(
        &mut self,
        nv12: &ID3D11Texture2D,
        time: i64,
        keyframe: bool,
    ) -> Result<()> {
        let started = Instant::now();
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { nv12.GetDesc(&mut desc) };
        let (width, height) = (desc.Width as usize, desc.Height as usize);
        let staging = self.staging_texture(&desc)?;
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        unsafe {
            self.context.CopyResource(&staging, nv12);
            self.context
                .Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
        }
        // Mapped NV12: `height` rows of Y, then `height / 2` rows of interleaved U, V, all `pitch` apart.
        let pitch = mapped.RowPitch as usize;
        let base = mapped.pData.cast::<u8>().cast_const();
        let y = unsafe { std::slice::from_raw_parts(base, pitch * height) };
        let uv =
            unsafe { std::slice::from_raw_parts(base.add(pitch * height), pitch * height / 2) };
        let copy_ms = started.elapsed().as_secs_f64() * 1e3;
        let result = self
            .core
            .encode_nv12(y, uv, pitch, (width, height), time, keyframe, copy_ms);
        unsafe { self.context.Unmap(&staging, 0) };
        result.map_err(|e| Error::new(E_FAIL, e))
    }

    /// A CPU-readable NV12 texture of the frame's size, created once.
    fn staging_texture(&mut self, frame: &D3D11_TEXTURE2D_DESC) -> Result<ID3D11Texture2D> {
        if let Some(staging) = &self.staging {
            return Ok(staging.clone());
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
        let staging = out.ok_or_else(|| Error::from(E_UNEXPECTED))?;
        self.staging = Some(staging.clone());
        Ok(staging)
    }
}
