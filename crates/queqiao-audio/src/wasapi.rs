//! WASAPI in shared mode at 48 kHz stereo float: the audio engine converts from and to each device's own format
//! (AUTOCONVERTPCM), so no resampling of our own.

use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Media::Audio::{
    AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
    AUDCLNT_STREAMFLAGS_LOOPBACK, AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY, DEVICE_STATE_ACTIVE,
    IAudioCaptureClient, IAudioClient, IAudioRenderClient, IMMDevice, IMMDeviceEnumerator,
    MMDeviceEnumerator, WAVEFORMATEX, eConsole, eRender,
};
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
    CoUninitialize, STGM_READ,
};

use crate::{CHANNELS, SAMPLE_RATE};

const WAVE_FORMAT_IEEE_FLOAT: u16 = 3;
/// The device buffer asked for: 100 ms (in 100 ns units).
const BUFFER: i64 = 1_000_000;

/// COM for this thread while alive.
pub struct Com(bool);

impl Com {
    pub fn init() -> Com {
        Com(unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok())
    }
}

impl Drop for Com {
    fn drop(&mut self) {
        if self.0 {
            unsafe { CoUninitialize() };
        }
    }
}

enum Client {
    Capture(IAudioCaptureClient),
    Render(IAudioRenderClient),
}

pub struct Stream {
    pub name: String,
    id: String,
    client: IAudioClient,
    service: Client,
    buffer_frames: u32,
}

impl Stream {
    /// What a render device (speakers, headphones) plays.
    pub fn loopback(device: Option<&str>) -> Result<Stream, String> {
        Stream::open(device, true)
    }

    pub fn render(device: Option<&str>) -> Result<Stream, String> {
        Stream::open(device, false)
    }

    fn open(device: Option<&str>, loopback: bool) -> Result<Stream, String> {
        let (device, name) = find_device(device)?;
        let id = device_id(&device);
        let client: IAudioClient = unsafe { device.Activate(CLSCTX_ALL, None) }
            .map_err(|e| format!("{name}: {}", e.message()))?;
        let format = WAVEFORMATEX {
            wFormatTag: WAVE_FORMAT_IEEE_FLOAT,
            nChannels: CHANNELS as u16,
            nSamplesPerSec: SAMPLE_RATE,
            nAvgBytesPerSec: SAMPLE_RATE * 4 * CHANNELS as u32,
            nBlockAlign: 4 * CHANNELS as u16,
            wBitsPerSample: 32,
            cbSize: 0,
        };
        let mut flags =
            AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY;
        if loopback {
            flags |= AUDCLNT_STREAMFLAGS_LOOPBACK;
        }
        unsafe { client.Initialize(AUDCLNT_SHAREMODE_SHARED, flags, BUFFER, 0, &format, None) }
            .map_err(|e| format!("{name}: {}", e.message()))?;
        let buffer_frames =
            unsafe { client.GetBufferSize() }.map_err(|e| format!("{name}: {}", e.message()))?;
        let service = if loopback {
            Client::Capture(
                unsafe { client.GetService() }.map_err(|e| format!("{name}: {}", e.message()))?,
            )
        } else {
            Client::Render(
                unsafe { client.GetService() }.map_err(|e| format!("{name}: {}", e.message()))?,
            )
        };
        unsafe { client.Start() }.map_err(|e| format!("{name}: {}", e.message()))?;
        Ok(Stream {
            name,
            id,
            client,
            service,
            buffer_frames,
        })
    }

    /// Loopback: everything captured since the last call, interleaved (empty while nothing plays).
    pub fn read(&self, out: &mut Vec<f32>) -> Result<(), String> {
        out.clear();
        let Client::Capture(capture) = &self.service else {
            return Err("not a capture stream".into());
        };
        loop {
            let size = unsafe { capture.GetNextPacketSize() }.map_err(|e| e.message())?;
            if size == 0 {
                return Ok(());
            }
            let mut data = std::ptr::null_mut();
            let mut frames = 0u32;
            let mut flags = 0u32;
            unsafe { capture.GetBuffer(&mut data, &mut frames, &mut flags, None, None) }
                .map_err(|e| e.message())?;
            let count = frames as usize * CHANNELS;
            if flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 || data.is_null() {
                out.resize(out.len() + count, 0.0);
            } else {
                out.extend_from_slice(unsafe {
                    std::slice::from_raw_parts(data as *const f32, count)
                });
            }
            unsafe { capture.ReleaseBuffer(frames) }.map_err(|e| e.message())?;
        }
    }

    /// Frames queued at the device and not yet played.
    pub fn padding(&self) -> Result<u32, String> {
        unsafe { self.client.GetCurrentPadding() }.map_err(|e| e.message())
    }

    /// Render: queues interleaved samples (as many as fit).
    pub fn write(&self, samples: &[f32]) -> Result<(), String> {
        let Client::Render(render) = &self.service else {
            return Err("not a render stream".into());
        };
        let free = self.buffer_frames.saturating_sub(self.padding()?);
        let frames = ((samples.len() / CHANNELS) as u32).min(free);
        if frames == 0 {
            return Ok(());
        }
        let data = unsafe { render.GetBuffer(frames) }.map_err(|e| e.message())?;
        let count = frames as usize * CHANNELS;
        unsafe { std::ptr::copy_nonoverlapping(samples.as_ptr(), data as *mut f32, count) };
        unsafe { render.ReleaseBuffer(frames, 0) }.map_err(|e| e.message())
    }

    /// Whether this is still the default render device.
    pub fn is_default(&self) -> bool {
        enumerator()
            .and_then(|enumerator| {
                unsafe { enumerator.GetDefaultAudioEndpoint(eRender, eConsole) }
                    .map_err(|e| e.message())
            })
            .is_ok_and(|device| device_id(&device) == self.id)
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        let _ = unsafe { self.client.Stop() };
    }
}

fn enumerator() -> Result<IMMDeviceEnumerator, String> {
    unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }.map_err(|e| e.message())
}

/// The names of the active render devices (speakers, headphones, virtual devices).
pub fn render_devices() -> Vec<String> {
    let _com = Com::init();
    let Ok(enumerator) = enumerator() else {
        return Vec::new();
    };
    let Ok(devices) = (unsafe { enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE) })
    else {
        return Vec::new();
    };
    let count = unsafe { devices.GetCount() }.unwrap_or(0);
    (0..count)
        .filter_map(|index| unsafe { devices.Item(index) }.ok())
        .map(|device| friendly_name(&device))
        .collect()
}

/// The render device whose name contains `wanted` (any case), or the default one.
fn find_device(wanted: Option<&str>) -> Result<(IMMDevice, String), String> {
    let enumerator = enumerator()?;
    let Some(wanted) = wanted else {
        let device = unsafe { enumerator.GetDefaultAudioEndpoint(eRender, eConsole) }
            .map_err(|e| format!("no speakers or headphones ({})", e.message()))?;
        let name = friendly_name(&device);
        return Ok((device, name));
    };
    let devices = unsafe { enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE) }
        .map_err(|e| e.message())?;
    let count = unsafe { devices.GetCount() }.map_err(|e| e.message())?;
    let wanted_lower = wanted.to_lowercase();
    for index in 0..count {
        let device = unsafe { devices.Item(index) }.map_err(|e| e.message())?;
        let name = friendly_name(&device);
        if name.to_lowercase().contains(&wanted_lower) {
            return Ok((device, name));
        }
    }
    Err(format!("no active sound device named like \"{wanted}\""))
}

fn friendly_name(device: &IMMDevice) -> String {
    unsafe { device.OpenPropertyStore(STGM_READ) }
        .and_then(|store| unsafe { store.GetValue(&PKEY_Device_FriendlyName) })
        .map(|value| value.to_string())
        .unwrap_or_else(|_| "unnamed sound device".into())
}

fn device_id(device: &IMMDevice) -> String {
    match unsafe { device.GetId() } {
        Ok(id) => {
            let text = unsafe { id.to_string() }.unwrap_or_default();
            unsafe { CoTaskMemFree(Some(id.0 as *const _)) };
            text
        }
        Err(_) => String::new(),
    }
}
