//! Software H.264 encoder: OpenH264 (Cisco, BSD licence), compiled into Queqiao, as UU Remote does.
//! Runs on the CPU, so it works on every PC: no GPU encoder, virtual machines, blocked drivers, and
//! Windows "N" editions without Media Foundation.
//!
//! The frame still arrives as an NV12 texture (converted on the GPU, or on WARP without a GPU); it is
//! copied to memory and handed to OpenH264 as I420 (the U and V samples de-interleaved).

use std::time::Instant;

use openh264::OpenH264API;
use openh264::encoder::{
    BitRate, Encoder, EncoderConfig as OpenH264Config, FrameRate, FrameType, IntraFramePeriod,
    RateControlMode, UsageType, VuiConfig,
};
use openh264::formats::YUVSlices;
use windows::Win32::Foundation::{E_FAIL, E_UNEXPECTED};
use windows::Win32::Graphics::Direct3D11::*;
use windows::core::{Error, Result};

use crate::encoder::{EncodedFrame, EncoderConfig, SoftwareTiming};

pub(crate) struct SoftwareEncoder {
    encoder: Encoder,
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    staging: Option<ID3D11Texture2D>,
    u: Vec<u8>,
    v: Vec<u8>,
    ready: Vec<EncodedFrame>,
    pub(crate) name: String,
    pub(crate) timing: SoftwareTiming,
}

impl SoftwareEncoder {
    pub(crate) fn open(
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
        config: &EncoderConfig,
    ) -> Result<Self> {
        // Up to 4 threads: more slices cost compression, and the agent shares the CPU with the user.
        let threads = std::thread::available_parallelism().map_or(1, |n| n.get().clamp(1, 4));
        let settings = OpenH264Config::new()
            .usage_type(UsageType::ScreenContentRealTime)
            .rate_control_mode(RateControlMode::Bitrate)
            .bitrate(BitRate::from_bps(config.bitrate))
            .max_frame_rate(FrameRate::from_hz(config.fps as f32))
            .skip_frames(false)
            .intra_frame_period(IntraFramePeriod::from_num_frames(config.fps * 10))
            .num_threads(threads as u16)
            .vui(VuiConfig::bt709());
        let encoder = Encoder::with_api_config(OpenH264API::from_source(), settings)
            .map_err(|e| Error::new(E_FAIL, format!("OpenH264: {e}")))?;
        Ok(Self {
            encoder,
            device: device.clone(),
            context: context.clone(),
            staging: None,
            u: Vec::new(),
            v: Vec::new(),
            ready: Vec::new(),
            name: format!("OpenH264 (software, {threads} threads)"),
            timing: SoftwareTiming::default(),
        })
    }

    pub(crate) fn poll(&mut self, out: &mut Vec<EncodedFrame>) {
        out.append(&mut self.ready);
    }

    /// Encodes one NV12 frame now (synchronously). `time` is in 100 ns units.
    pub(crate) fn encode(
        &mut self,
        nv12: &ID3D11Texture2D,
        time: i64,
        keyframe: bool,
    ) -> Result<()> {
        if keyframe {
            self.encoder.force_intra_frame();
        }
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
        let chroma_width = width / 2;
        self.u.resize(chroma_width * (height / 2), 0);
        self.v.resize(chroma_width * (height / 2), 0);
        let rows = self
            .u
            .chunks_exact_mut(chroma_width)
            .zip(self.v.chunks_exact_mut(chroma_width));
        for ((u_row, v_row), uv_row) in rows.zip(uv.chunks(pitch)) {
            let pairs = uv_row[..width].as_chunks::<2>().0;
            for ((u, v), &[cb, cr]) in u_row.iter_mut().zip(v_row.iter_mut()).zip(pairs) {
                *u = cb;
                *v = cr;
            }
        }
        let copied = Instant::now();
        let source = YUVSlices::new(
            (y, &self.u, &self.v),
            (width, height),
            (pitch, chroma_width, chroma_width),
        );
        let encoded = self
            .encoder
            .encode(&source)
            .map(|stream| (stream.to_vec(), stream.frame_type()));
        unsafe { self.context.Unmap(&staging, 0) };
        let done = Instant::now();
        self.timing.frames += 1;
        self.timing.copy_ms += (copied - started).as_secs_f64() * 1e3;
        self.timing.encode_ms += (done - copied).as_secs_f64() * 1e3;
        let (data, frame_type) =
            encoded.map_err(|e| Error::new(E_FAIL, format!("OpenH264: {e}")))?;
        if !data.is_empty() {
            self.ready.push(EncodedFrame {
                data,
                time,
                keyframe: matches!(frame_type, FrameType::IDR | FrameType::I),
            });
        }
        Ok(())
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
