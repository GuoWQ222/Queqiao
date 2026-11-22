//! Windows' H.264 decoder (a Media Foundation transform) with D3D11 video acceleration: it decodes on the
//! GPU through the driver's DXVA support, whatever the vendor. Its pictures arrive as slices of a texture
//! array the decoder owns; each is copied into a texture of ours right away.

use std::mem::ManuallyDrop;
use std::ptr::null_mut;

use windows::Win32::Foundation::{E_FAIL, E_UNEXPECTED};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::System::Variant::VARIANT;
use windows::core::{Error, Interface, Result};

use crate::decoder::{DecodedFrame, Nv12Ring};
use crate::mf::{at, start_media_foundation};

pub(crate) struct MfDecoder {
    transform: IMFTransform,
    _manager: IMFDXGIDeviceManager,
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    ring: Nv12Ring,
    /// Picture size (the decoder's frames can be taller: 1088 rows for a 1080-row picture).
    display: (u32, u32),
    /// Row pitch of pictures that arrive in memory.
    stride: u32,
    frame_height: u32,
    provides_samples: bool,
    output_size: u32,
    pub(crate) name: String,
    /// Pictures that came in memory, not as GPU textures (the decoder fell back to its CPU path).
    pub(crate) cpu_frames: u32,
    packed: Vec<u8>,
}

/// Decoders registered for `codec` (Store packages like the AV1 Video Extension included), with their names.
fn decoders(codec: crate::VideoCodec) -> Result<Vec<(IMFActivate, String)>> {
    let input = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: *crate::mf::subtype(codec),
    };
    let flags = MFT_ENUM_FLAG(
        MFT_ENUM_FLAG_SYNCMFT.0 | MFT_ENUM_FLAG_HARDWARE.0 | MFT_ENUM_FLAG_SORTANDFILTER.0,
    );
    let mut list: *mut Option<IMFActivate> = null_mut();
    let mut count = 0;
    unsafe {
        MFTEnumEx(
            MFT_CATEGORY_VIDEO_DECODER,
            flags,
            Some(&input),
            None,
            &mut list,
            &mut count,
        )
    }
    .map_err(at("MFTEnumEx"))?;
    let mut found = Vec::new();
    if !list.is_null() {
        let slots = unsafe { std::slice::from_raw_parts_mut(list, count as usize) };
        for slot in slots.iter_mut() {
            if let Some(activate) = slot.take() {
                let name = crate::mf::friendly_name(&activate);
                found.push((activate, name));
            }
        }
        unsafe { windows::Win32::System::Com::CoTaskMemFree(Some(list as *const _)) };
    }
    Ok(found)
}

/// Whether a decoder for `codec` is registered on this PC.
pub(crate) fn has_decoder(codec: crate::VideoCodec) -> bool {
    unsafe {
        let _ = windows::Win32::System::Com::CoInitializeEx(
            None,
            windows::Win32::System::Com::COINIT_MULTITHREADED,
        );
        let _ = MFStartup(MF_VERSION, MFSTARTUP_FULL);
    }
    decoders(codec).is_ok_and(|list| !list.is_empty())
}

enum Output {
    Frame(DecodedFrame),
    NeedInput,
    Changed,
}

impl MfDecoder {
    pub(crate) fn open(
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
        codec: crate::VideoCodec,
    ) -> Result<Self> {
        start_media_foundation(device).map_err(at("Media Foundation"))?;
        let (transform, name): (IMFTransform, String) = match codec {
            crate::VideoCodec::H264 => (
                unsafe { CoCreateInstance(&CLSID_MSH264DecoderMFT, None, CLSCTX_INPROC_SERVER) }
                    .map_err(at("Windows H.264 decoder"))?,
                "Windows H.264 decoder (D3D11 video)".into(),
            ),
            _ => {
                let (activate, name) = decoders(codec)?.into_iter().next().ok_or_else(|| {
                    Error::new(E_FAIL, format!("no {} decoder is registered", codec.name()))
                })?;
                (
                    unsafe { activate.ActivateObject() }.map_err(at("activate"))?,
                    format!("{name} (D3D11 video)"),
                )
            }
        };
        let attributes = unsafe { transform.GetAttributes() }?;
        if unsafe { attributes.GetUINT32(&MF_SA_D3D11_AWARE) }.unwrap_or(0) == 0 {
            return Err(Error::new(E_FAIL, format!("{name}: not D3D11-aware")));
        }
        let _ = unsafe { attributes.SetUINT32(&MF_LOW_LATENCY, 1) };
        if let Ok(codec) = transform.cast::<ICodecAPI>() {
            let _ = unsafe { codec.SetValue(&CODECAPI_AVLowLatencyMode, &VARIANT::from(true)) };
        }

        let mut token = 0;
        let mut manager: Option<IMFDXGIDeviceManager> = None;
        unsafe { MFCreateDXGIDeviceManager(&mut token, &mut manager) }?;
        let manager = manager.ok_or_else(|| Error::from(E_UNEXPECTED))?;
        unsafe {
            manager.ResetDevice(device, token)?;
            let raw = manager.as_raw() as usize;
            transform
                .ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, raw)
                .map_err(at("D3D11 video decoding on this device"))?;
        }

        let input_type = unsafe { MFCreateMediaType() }?;
        unsafe {
            input_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            input_type.SetGUID(&MF_MT_SUBTYPE, crate::mf::subtype(codec))?;
            // The AV1 decoder wants a size and frame rate with the type; any will do, the stream's own sequence
            // header replaces them (a stream change). Windows' H.264 decoder takes the type without them.
            if codec != crate::VideoCodec::H264 {
                input_type.SetUINT64(&MF_MT_FRAME_SIZE, (1920u64 << 32) | 1080)?;
                input_type.SetUINT64(&MF_MT_FRAME_RATE, (60u64 << 32) | 1)?;
                input_type
                    .SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
            }
            transform
                .SetInputType(0, &input_type, 0)
                .map_err(at("input type"))?;
        }
        let mut decoder = Self {
            transform,
            _manager: manager,
            device: device.clone(),
            context: context.clone(),
            ring: Nv12Ring::new(),
            display: (0, 0),
            stride: 0,
            frame_height: 0,
            provides_samples: false,
            output_size: 0,
            name,
            cpu_frames: 0,
            packed: Vec::new(),
        };
        decoder.choose_output_type()?;
        unsafe {
            decoder
                .transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            decoder
                .transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
        }
        Ok(decoder)
    }

    pub(crate) fn decode(
        &mut self,
        data: &[u8],
        time: i64,
        out: &mut Vec<DecodedFrame>,
    ) -> Result<()> {
        let sample = memory_sample(data, time)?;
        let mut accepted = unsafe { self.transform.ProcessInput(0, &sample, 0) };
        if matches!(&accepted, Err(e) if e.code() == MF_E_NOTACCEPTING) {
            self.pull(out)?;
            accepted = unsafe { self.transform.ProcessInput(0, &sample, 0) };
        }
        accepted.map_err(at("decoder input"))?;
        self.pull(out)
    }

    /// Takes every picture that is ready.
    fn pull(&mut self, out: &mut Vec<DecodedFrame>) -> Result<()> {
        loop {
            match self.take_output()? {
                Output::Frame(frame) => out.push(frame),
                Output::Changed => {}
                Output::NeedInput => return Ok(()),
            }
        }
    }

    /// Picks NV12 among the decoder's output types and reads the picture size from it.
    fn choose_output_type(&mut self) -> Result<()> {
        for index in 0.. {
            let Ok(media_type) = (unsafe { self.transform.GetOutputAvailableType(0, index) })
            else {
                break;
            };
            if unsafe { media_type.GetGUID(&MF_MT_SUBTYPE) }? != MFVideoFormat_NV12 {
                continue;
            }
            unsafe { self.transform.SetOutputType(0, &media_type, 0) }?;
            let size = unsafe { media_type.GetUINT64(&MF_MT_FRAME_SIZE) }.unwrap_or(0);
            let (width, height) = ((size >> 32) as u32, size as u32);
            self.frame_height = height;
            self.display = display_area(&media_type).unwrap_or((width, height));
            self.stride = unsafe { media_type.GetUINT32(&MF_MT_DEFAULT_STRIDE) }.unwrap_or(width);
            let info = unsafe { self.transform.GetOutputStreamInfo(0) }?;
            self.provides_samples =
                (info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32) != 0;
            self.output_size = info.cbSize;
            return Ok(());
        }
        Err(Error::new(E_FAIL, "the decoder offers no NV12 output"))
    }

    fn take_output(&mut self) -> Result<Output> {
        let sample = if self.provides_samples {
            None
        } else {
            let sample = unsafe { MFCreateSample() }?;
            let size = self.output_size.max(1);
            unsafe { sample.AddBuffer(&MFCreateMemoryBuffer(size)?) }?;
            Some(sample)
        };
        let mut buffers = [MFT_OUTPUT_DATA_BUFFER {
            dwStreamID: 0,
            pSample: ManuallyDrop::new(sample),
            dwStatus: 0,
            pEvents: ManuallyDrop::new(None),
        }];
        let mut status = 0;
        let result = unsafe { self.transform.ProcessOutput(0, &mut buffers, &mut status) };
        let [buffer] = buffers;
        let sample = ManuallyDrop::into_inner(buffer.pSample);
        drop(ManuallyDrop::into_inner(buffer.pEvents));
        match result {
            Ok(()) => {}
            Err(error) if error.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                // The first keyframe tells the decoder the real picture size.
                self.choose_output_type()?;
                return Ok(Output::Changed);
            }
            Err(error) if error.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => {
                return Ok(Output::NeedInput);
            }
            Err(error) => return Err(error),
        }
        let Some(sample) = sample else {
            return Ok(Output::NeedInput);
        };
        let time = unsafe { sample.GetSampleTime() }.unwrap_or(0);
        let media_buffer = unsafe { sample.GetBufferByIndex(0) }?;
        let texture = self.ring.next(&self.device, self.display)?;
        if let Ok(dxgi) = media_buffer.cast::<IMFDXGIBuffer>() {
            let mut raw = null_mut();
            unsafe { dxgi.GetResource(&ID3D11Texture2D::IID, &mut raw) }?;
            let source = unsafe { ID3D11Texture2D::from_raw(raw) };
            let index = unsafe { dxgi.GetSubresourceIndex() }?;
            let area = D3D11_BOX {
                left: 0,
                top: 0,
                front: 0,
                right: self.display.0,
                bottom: self.display.1,
                back: 1,
            };
            let ctx = &self.context;
            unsafe { ctx.CopySubresourceRegion(&texture, 0, 0, 0, 0, &source, index, Some(&area)) };
        } else {
            self.upload_from_memory(&media_buffer, &texture)?;
            self.cpu_frames += 1;
        }
        Ok(Output::Frame(DecodedFrame {
            texture,
            width: self.display.0,
            height: self.display.1,
            time,
        }))
    }

    /// A picture in memory (stride x frame height of Y, then the UV rows): packed to the display size.
    fn upload_from_memory(
        &mut self,
        buffer: &IMFMediaBuffer,
        texture: &ID3D11Texture2D,
    ) -> Result<()> {
        let (width, height) = (self.display.0 as usize, self.display.1 as usize);
        let stride = self.stride as usize;
        let chroma_start = stride * self.frame_height as usize;
        let mut data: *mut u8 = null_mut();
        let mut length = 0;
        unsafe { buffer.Lock(&mut data, None, Some(&mut length)) }?;
        let source = unsafe { std::slice::from_raw_parts(data, length as usize) };
        self.packed.clear();
        for row in 0..height {
            self.packed
                .extend_from_slice(&source[row * stride..row * stride + width]);
        }
        for row in 0..height / 2 {
            let start = chroma_start + row * stride;
            self.packed.extend_from_slice(&source[start..start + width]);
        }
        unsafe { buffer.Unlock() }?;
        let pitch = self.display.0;
        let ctx = &self.context;
        unsafe { ctx.UpdateSubresource(texture, 0, None, self.packed.as_ptr().cast(), pitch, 0) };
        Ok(())
    }
}

impl Drop for MfDecoder {
    fn drop(&mut self) {
        let _ = unsafe {
            self.transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0)
        };
    }
}

/// The picture area inside the decoder's (possibly padded) frame, if the type says.
fn display_area(media_type: &IMFMediaType) -> Option<(u32, u32)> {
    let mut area = MFVideoArea::default();
    let bytes = unsafe {
        std::slice::from_raw_parts_mut((&raw mut area).cast::<u8>(), size_of::<MFVideoArea>())
    };
    unsafe { media_type.GetBlob(&MF_MT_MINIMUM_DISPLAY_APERTURE, bytes, None) }.ok()?;
    let (width, height) = (area.Area.cx, area.Area.cy);
    (width > 0 && height > 0).then_some((width as u32, height as u32))
}

/// An encoded access unit as a Media Foundation sample.
fn memory_sample(data: &[u8], time: i64) -> Result<IMFSample> {
    let buffer = unsafe { MFCreateMemoryBuffer(data.len() as u32) }?;
    let mut target: *mut u8 = null_mut();
    unsafe {
        buffer.Lock(&mut target, None, None)?;
        std::ptr::copy_nonoverlapping(data.as_ptr(), target, data.len());
        buffer.Unlock()?;
        buffer.SetCurrentLength(data.len() as u32)?;
    }
    let sample = unsafe { MFCreateSample() }?;
    unsafe {
        sample.AddBuffer(&buffer)?;
        sample.SetSampleTime(time)?;
    }
    Ok(sample)
}
