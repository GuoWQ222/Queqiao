//! Hardware H.264 encoder through Media Foundation: the GPU vendor's encoder (NVIDIA NVENC, AMD AMF,
//! Intel Quick Sync) behind one API, fed with NV12 textures on our D3D11 device, set up for low latency.
//!
//! Hardware encoders are asynchronous Media Foundation transforms: they send "need input" and "have
//! output" events, and frames go in and out only when asked. [`H264Encoder::poll`] handles the events
//! without blocking, so the caller decides how to wait.

use std::mem::ManuallyDrop;
use std::ptr::null_mut;

use windows::Win32::Foundation::{E_FAIL, E_UNEXPECTED};
use windows::Win32::Graphics::Direct3D10::ID3D10Multithread;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoTaskMemFree};
use windows::Win32::System::Variant::VARIANT;
use windows::core::{Error, GUID, Interface, PWSTR, Result};

/// What to encode.
#[derive(Clone, Debug)]
pub struct EncoderConfig {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    /// Target bitrate in bits per second (constant bitrate).
    pub bitrate: u32,
}

impl EncoderConfig {
    /// Starting settings for a picture of this size and rate. The bitrate suits desktop content (about
    /// 0.1 bit per pixel per frame, kept between 2 and 50 Mbit/s); the network's bandwidth estimate
    /// replaces it once a session runs.
    pub fn for_screen(width: u32, height: u32, fps: u32) -> Self {
        let bits = f64::from(width) * f64::from(height) * f64::from(fps) * 0.1;
        Self {
            width,
            height,
            fps,
            bitrate: bits.clamp(2e6, 50e6) as u32,
        }
    }
}

/// One encoded frame.
pub struct EncodedFrame {
    /// Annex-B H.264: NAL units, each after a 00 00 00 01 or 00 00 01 start code.
    pub data: Vec<u8>,
    /// The sample time given to [`H264Encoder::encode`], in 100 ns units.
    pub time: i64,
    /// An IDR frame: a decoder can start here.
    pub keyframe: bool,
}

pub struct H264Encoder {
    transform: IMFTransform,
    events: IMFMediaEventGenerator,
    codec: ICodecAPI,
    _manager: IMFDXGIDeviceManager,
    name: String,
    refused: Vec<&'static str>,
    input_requests: u32,
    frame_duration: i64,
    provides_samples: bool,
    output_size: u32,
    drained: bool,
}

impl H264Encoder {
    /// Opens the first hardware H.264 encoder that accepts `device` and `config`.
    pub fn new(device: &ID3D11Device, config: &EncoderConfig) -> Result<Self> {
        unsafe {
            // S_FALSE (already initialised) and RPC_E_CHANGED_MODE are fine: MF works in either mode.
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            MFStartup(MF_VERSION, MFSTARTUP_FULL).map_err(at("MFStartup"))?;
        }
        // Media Foundation uses the device from its own threads.
        let multithread: ID3D10Multithread = device.cast()?;
        let _ = unsafe { multithread.SetMultithreadProtected(true) };

        let mut failures = Vec::new();
        for (activate, name) in hardware_encoders()? {
            match Self::open(&activate, name.clone(), device, config) {
                Ok(encoder) => return Ok(encoder),
                Err(error) => failures.push(format!("{name}: {}", error.message())),
            }
        }
        let text = if failures.is_empty() {
            "no hardware H.264 encoder is registered".to_string()
        } else {
            failures.join("; ")
        };
        Err(Error::new(E_FAIL, text))
    }

    fn open(
        activate: &IMFActivate,
        name: String,
        device: &ID3D11Device,
        config: &EncoderConfig,
    ) -> Result<Self> {
        let transform: IMFTransform =
            unsafe { activate.ActivateObject() }.map_err(at("activate"))?;
        let attributes = unsafe { transform.GetAttributes() }?;
        let is_async = unsafe { attributes.GetUINT32(&MF_TRANSFORM_ASYNC) }.unwrap_or(0) != 0;
        if !is_async {
            return Err(Error::new(E_FAIL, "not an asynchronous (hardware) encoder"));
        }
        unsafe {
            attributes.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1)?;
            let _ = attributes.SetUINT32(&MF_LOW_LATENCY, 1);
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
                .map_err(at("set the D3D11 device"))?;
        }

        // Encoder settings go before the media types: some encoders fix rate control with the type.
        let codec: ICodecAPI = transform.cast()?;
        let mut refused = Vec::new();
        let settings: [(&'static str, &GUID, VARIANT); 5] = [
            (
                "low latency",
                &CODECAPI_AVLowLatencyMode,
                VARIANT::from(true),
            ),
            (
                "constant bitrate",
                &CODECAPI_AVEncCommonRateControlMode,
                VARIANT::from(eAVEncCommonRateControlMode_CBR.0 as u32),
            ),
            (
                "bitrate",
                &CODECAPI_AVEncCommonMeanBitRate,
                VARIANT::from(config.bitrate),
            ),
            (
                "no B-frames",
                &CODECAPI_AVEncMPVDefaultBPictureCount,
                VARIANT::from(0u32),
            ),
            // An IDR every 10 s; the viewer asks for one sooner when it loses a frame.
            (
                "keyframe interval",
                &CODECAPI_AVEncMPVGOPSize,
                VARIANT::from(config.fps * 10),
            ),
        ];
        for (label, api, value) in &settings {
            if unsafe { codec.SetValue(*api, value) }.is_err() {
                refused.push(*label);
            }
        }

        let output_type = video_type(&MFVideoFormat_H264, config)?;
        unsafe {
            output_type.SetUINT32(&MF_MT_AVG_BITRATE, config.bitrate)?;
            output_type.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_High.0 as u32)?;
            transform
                .SetOutputType(0, &output_type, 0)
                .map_err(at("H.264 output type"))?;
        }
        let input_type = video_type(&MFVideoFormat_NV12, config)?;
        unsafe { transform.SetInputType(0, &input_type, 0) }.map_err(at("NV12 input type"))?;

        unsafe {
            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
        }
        let events: IMFMediaEventGenerator = transform.cast()?;
        let mut encoder = Self {
            transform,
            events,
            codec,
            _manager: manager,
            name,
            refused,
            input_requests: 0,
            frame_duration: 10_000_000 / i64::from(config.fps.max(1)),
            provides_samples: false,
            output_size: 0,
            drained: false,
        };
        encoder.read_output_info()?;
        Ok(encoder)
    }

    /// The encoder's name, e.g. "NVIDIA H.264 Encoder MFT".
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Settings the encoder did not accept (it then uses its own default for them).
    pub fn refused_settings(&self) -> &[&'static str] {
        &self.refused
    }

    /// True if the encoder has asked for another frame.
    pub fn wants_input(&self) -> bool {
        self.input_requests > 0
    }

    /// True once all frames have come out after [`H264Encoder::drain`].
    pub fn drained(&self) -> bool {
        self.drained
    }

    /// Handles all pending encoder events without waiting: counts input requests and appends finished
    /// frames to `out`.
    pub fn poll(&mut self, out: &mut Vec<EncodedFrame>) -> Result<()> {
        loop {
            let event = match unsafe { self.events.GetEvent(MF_EVENT_FLAG_NO_WAIT) } {
                Ok(event) => event,
                Err(error) if error.code() == MF_E_NO_EVENTS_AVAILABLE => return Ok(()),
                Err(error) => return Err(error),
            };
            let kind = unsafe { event.GetType() }?;
            if kind == METransformNeedInput.0 as u32 {
                self.input_requests += 1;
            } else if kind == METransformHaveOutput.0 as u32 {
                if let Some(frame) = self.take_output()? {
                    out.push(frame);
                }
            } else if kind == METransformDrainComplete.0 as u32 {
                self.drained = true;
            }
        }
    }

    /// Sends one NV12 frame (call only when [`H264Encoder::wants_input`]). `time` is in 100 ns units;
    /// `keyframe` forces an IDR frame.
    pub fn encode(&mut self, nv12: &ID3D11Texture2D, time: i64, keyframe: bool) -> Result<()> {
        if keyframe {
            let value = VARIANT::from(1u32);
            let _ = unsafe {
                self.codec
                    .SetValue(&CODECAPI_AVEncVideoForceKeyFrame, &value)
            };
        }
        let buffer = unsafe { MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID, nv12, 0, false) }?;
        let length = unsafe { buffer.cast::<IMF2DBuffer>()?.GetContiguousLength() }?;
        let sample = unsafe { MFCreateSample() }?;
        unsafe {
            buffer.SetCurrentLength(length)?;
            sample.AddBuffer(&buffer)?;
            sample.SetSampleTime(time)?;
            sample.SetSampleDuration(self.frame_duration)?;
            self.transform.ProcessInput(0, &sample, 0)?;
        }
        self.input_requests = self.input_requests.saturating_sub(1);
        Ok(())
    }

    /// Asks the encoder to emit every frame it still holds; keep calling `poll` until `drained`.
    pub fn drain(&mut self) -> Result<()> {
        unsafe { self.transform.ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0) }
    }

    fn read_output_info(&mut self) -> Result<()> {
        let info = unsafe { self.transform.GetOutputStreamInfo(0) }?;
        self.provides_samples = (info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32) != 0;
        self.output_size = info.cbSize;
        Ok(())
    }

    fn take_output(&mut self) -> Result<Option<EncodedFrame>> {
        let sample = if self.provides_samples {
            None
        } else {
            let sample = unsafe { MFCreateSample() }?;
            let size = self.output_size.max(8 << 20);
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
                let new_type = unsafe { self.transform.GetOutputAvailableType(0, 0) }?;
                unsafe { self.transform.SetOutputType(0, &new_type, 0) }?;
                self.read_output_info()?;
                return Ok(None);
            }
            Err(error) if error.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(None),
            Err(error) => return Err(error),
        }
        let Some(sample) = sample else {
            return Ok(None);
        };
        let time = unsafe { sample.GetSampleTime() }.unwrap_or(0);
        let clean = unsafe { sample.GetUINT32(&MFSampleExtension_CleanPoint) };
        let buffer = unsafe { sample.ConvertToContiguousBuffer() }?;
        let mut data: *mut u8 = null_mut();
        let mut length = 0;
        unsafe { buffer.Lock(&mut data, None, Some(&mut length)) }?;
        let bytes = unsafe { std::slice::from_raw_parts(data, length as usize) }.to_vec();
        unsafe { buffer.Unlock() }?;
        Ok(Some(EncodedFrame {
            data: bytes,
            time,
            keyframe: clean.unwrap_or(0) != 0,
        }))
    }
}

impl Drop for H264Encoder {
    fn drop(&mut self) {
        // Asynchronous transforms must be shut down before release.
        if let Ok(shutdown) = self.transform.cast::<IMFShutdown>() {
            let _ = unsafe { shutdown.Shutdown() };
        }
    }
}

/// Hardware H.264 encoders, best first (Media Foundation's own order), with their names.
fn hardware_encoders() -> Result<Vec<(IMFActivate, String)>> {
    let output = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: MFVideoFormat_H264,
    };
    let flags = MFT_ENUM_FLAG(MFT_ENUM_FLAG_HARDWARE.0 | MFT_ENUM_FLAG_SORTANDFILTER.0);
    let mut list: *mut Option<IMFActivate> = null_mut();
    let mut count = 0;
    let category = MFT_CATEGORY_VIDEO_ENCODER;
    unsafe { MFTEnumEx(category, flags, None, Some(&output), &mut list, &mut count) }
        .map_err(at("MFTEnumEx"))?;
    let mut found = Vec::new();
    if !list.is_null() {
        let slots = unsafe { std::slice::from_raw_parts_mut(list, count as usize) };
        for slot in slots.iter_mut() {
            if let Some(activate) = slot.take() {
                let name = friendly_name(&activate);
                found.push((activate, name));
            }
        }
        unsafe { CoTaskMemFree(Some(list as *const _)) };
    }
    Ok(found)
}

fn friendly_name(activate: &IMFActivate) -> String {
    let mut text = PWSTR::null();
    let mut length = 0;
    let key = &MFT_FRIENDLY_NAME_Attribute;
    if unsafe { activate.GetAllocatedString(key, &mut text, &mut length) }.is_err() {
        return "unnamed encoder".into();
    }
    let name = unsafe { text.to_string() }.unwrap_or_default();
    unsafe { CoTaskMemFree(Some(text.0 as *const _)) };
    name
}

/// A progressive video media type of the config's size and rate, BT.709 limited range.
fn video_type(subtype: &GUID, config: &EncoderConfig) -> Result<IMFMediaType> {
    let media_type = unsafe { MFCreateMediaType() }?;
    unsafe {
        media_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        media_type.SetGUID(&MF_MT_SUBTYPE, subtype)?;
        media_type.SetUINT64(&MF_MT_FRAME_SIZE, pack(config.width, config.height))?;
        media_type.SetUINT64(&MF_MT_FRAME_RATE, pack(config.fps, 1))?;
        media_type.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1))?;
        let progressive = MFVideoInterlace_Progressive.0 as u32;
        media_type.SetUINT32(&MF_MT_INTERLACE_MODE, progressive)?;
        let matrix = MFVideoTransferMatrix_BT709.0 as u32;
        media_type.SetUINT32(&MF_MT_YUV_MATRIX, matrix)?;
        let range = MFNominalRange_16_235.0 as u32;
        media_type.SetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE, range)?;
        let primaries = MFVideoPrimaries_BT709.0 as u32;
        media_type.SetUINT32(&MF_MT_VIDEO_PRIMARIES, primaries)?;
        let transfer = MFVideoTransFunc_709.0 as u32;
        media_type.SetUINT32(&MF_MT_TRANSFER_FUNCTION, transfer)?;
    }
    Ok(media_type)
}

/// Two 32-bit values in one 64-bit Media Foundation attribute (frame size, frame rate, aspect ratio).
fn pack(high: u32, low: u32) -> u64 {
    (u64::from(high) << 32) | u64::from(low)
}

/// Adds which step failed to an error message.
fn at(step: &'static str) -> impl Fn(Error) -> Error {
    move |error| Error::new(error.code(), format!("{step}: {}", error.message()))
}
