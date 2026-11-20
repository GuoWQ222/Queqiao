//! Software H.264 decoder: OpenH264 on the CPU (compiled in, as UU Remote does). Works on every PC,
//! also without GPU video decoding or Media Foundation. Its I420 pictures are packed to NV12 and
//! uploaded, so the viewer gets the same kind of texture as from the GPU decoder.

use openh264::OpenH264API;
use openh264::decoder::{Decoder, DecoderConfig};
use openh264::formats::YUVSource;
use windows::Win32::Foundation::E_FAIL;
use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11DeviceContext};
use windows::core::{Error, Result};

use crate::decoder::{DecodedFrame, Nv12Ring};

pub(crate) struct SoftwareDecoder {
    decoder: Decoder,
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    ring: Nv12Ring,
    nv12: Vec<u8>,
    pub(crate) name: String,
}

impl SoftwareDecoder {
    pub(crate) fn open(device: &ID3D11Device, context: &ID3D11DeviceContext) -> Result<Self> {
        let decoder = Decoder::with_api_config(OpenH264API::from_source(), DecoderConfig::new())
            .map_err(|e| Error::new(E_FAIL, format!("OpenH264 decoder: {e}")))?;
        Ok(Self {
            decoder,
            device: device.clone(),
            context: context.clone(),
            ring: Nv12Ring::new(),
            nv12: Vec::new(),
            name: "OpenH264 decoder (software)".into(),
        })
    }

    pub(crate) fn decode(
        &mut self,
        data: &[u8],
        time: i64,
        out: &mut Vec<DecodedFrame>,
    ) -> Result<()> {
        let Self {
            decoder,
            device,
            context,
            ring,
            nv12,
            ..
        } = self;
        let decoded = decoder
            .decode(data)
            .map_err(|e| Error::new(E_FAIL, format!("OpenH264 decoder: {e}")))?;
        let Some(yuv) = decoded else {
            return Ok(());
        };
        let (width, height) = yuv.dimensions();
        let (y_stride, u_stride, v_stride) = yuv.strides();
        nv12.clear();
        for row in yuv.y().chunks(y_stride).take(height) {
            nv12.extend_from_slice(&row[..width]);
        }
        let rows = yuv.u().chunks(u_stride).zip(yuv.v().chunks(v_stride));
        for (u_row, v_row) in rows.take(height / 2) {
            for (&u, &v) in u_row[..width / 2].iter().zip(&v_row[..width / 2]) {
                nv12.push(u);
                nv12.push(v);
            }
        }
        let size = (width as u32, height as u32);
        let texture = ring.next(device, size)?;
        unsafe { context.UpdateSubresource(&texture, 0, None, nv12.as_ptr().cast(), size.0, 0) };
        out.push(DecodedFrame {
            texture,
            width: size.0,
            height: size.1,
            time,
        });
        Ok(())
    }
}
