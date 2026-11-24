//! Software H.264 encoder: OpenH264 (Cisco, BSD licence), compiled into Queqiao, as UU Remote does. Runs on the CPU,
//! so it works on every PC: no GPU encoder, virtual machines, blocked drivers, and Windows "N" editions without Media
//! Foundation. The same on Windows and Linux (docs/LINUX.md): this takes NV12 planes in memory; Windows' wrapper
//! (software.rs) reads them from a texture, Linux converts the captured frame on the CPU (cpu_convert.rs).

use std::time::Instant;

use openh264::OpenH264API;
use openh264::encoder::{
    BitRate, Encoder, EncoderConfig as OpenH264Config, FrameRate, FrameType, IntraFramePeriod,
    RateControlMode, UsageType, VuiConfig,
};
use openh264::formats::YUVSlices;

use crate::config::{EncodedFrame, EncoderConfig, SoftwareTiming};

pub(crate) struct SoftwareCore {
    encoder: Encoder,
    u: Vec<u8>,
    v: Vec<u8>,
    ready: Vec<EncodedFrame>,
    pub(crate) name: String,
    pub(crate) timing: SoftwareTiming,
}

impl SoftwareCore {
    pub(crate) fn open(config: &EncoderConfig) -> Result<Self, String> {
        // Up to 4 threads: more slices cost compression, and the agent shares the CPU with the user.
        let threads = std::thread::available_parallelism().map_or(1, |n| n.get().clamp(1, 4));
        let settings = OpenH264Config::new()
            .usage_type(UsageType::ScreenContentRealTime)
            .rate_control_mode(RateControlMode::Bitrate)
            .bitrate(BitRate::from_bps(config.bitrate))
            .max_frame_rate(FrameRate::from_hz(config.fps as f32))
            .skip_frames(false)
            // Not available for screen content: OpenH264 turns them off and warns otherwise.
            .adaptive_quantization(false)
            .background_detection(false)
            .intra_frame_period(IntraFramePeriod::from_num_frames(config.fps * 10))
            .num_threads(threads as u16)
            .vui(VuiConfig::bt709());
        let encoder = Encoder::with_api_config(OpenH264API::from_source(), settings)
            .map_err(|e| format!("OpenH264: {e}"))?;
        Ok(Self {
            encoder,
            u: Vec::new(),
            v: Vec::new(),
            ready: Vec::new(),
            name: format!("OpenH264 (software, {threads} threads)"),
            timing: SoftwareTiming::default(),
        })
    }

    /// Changes the target bitrate (all layers) for the next frames.
    pub(crate) fn set_bitrate(&mut self, bitrate: u32) -> Result<(), String> {
        let mut info = openh264_sys2::SBitrateInfo {
            iLayer: openh264_sys2::SPATIAL_LAYER_ALL,
            iBitrate: i32::try_from(bitrate).unwrap_or(i32::MAX),
        };
        let code = unsafe {
            self.encoder.raw_api().set_option(
                openh264_sys2::ENCODER_OPTION_BITRATE,
                (&raw mut info).cast(),
            )
        };
        if code == 0 {
            Ok(())
        } else {
            Err(format!("OpenH264 refused the bitrate ({code})"))
        }
    }

    pub(crate) fn poll(&mut self, out: &mut Vec<EncodedFrame>) {
        out.append(&mut self.ready);
    }

    /// Encodes one NV12 frame now (synchronously): `y` is `height` rows of `pitch` bytes, `uv` the `height / 2` rows
    /// of interleaved U, V after it. `time` is in 100 ns units. `copy_ms` (how long getting the planes took) goes
    /// into the timing.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn encode_nv12(
        &mut self,
        y: &[u8],
        uv: &[u8],
        pitch: usize,
        (width, height): (usize, usize),
        time: i64,
        keyframe: bool,
        copy_ms: f64,
    ) -> Result<(), String> {
        if keyframe {
            self.encoder.force_intra_frame();
        }
        let started = Instant::now();
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
        let split = Instant::now();
        let source = YUVSlices::new(
            (y, &self.u, &self.v),
            (width, height),
            (pitch, chroma_width, chroma_width),
        );
        let encoded = self
            .encoder
            .encode(&source)
            .map(|stream| (stream.to_vec(), stream.frame_type()));
        let done = Instant::now();
        self.timing.frames += 1;
        self.timing.copy_ms += copy_ms + (split - started).as_secs_f64() * 1e3;
        self.timing.encode_ms += (done - split).as_secs_f64() * 1e3;
        let (data, frame_type) = encoded.map_err(|e| format!("OpenH264: {e}"))?;
        if !data.is_empty() {
            self.ready.push(EncodedFrame {
                data,
                time,
                keyframe: matches!(frame_type, FrameType::IDR | FrameType::I),
            });
        }
        Ok(())
    }
}
