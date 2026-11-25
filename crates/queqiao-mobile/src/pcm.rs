//! The PC's sound as samples, for apps whose system has no Opus decoder of its own to rely on (iOS): libopus
//! (opusic-sys, built from its source) decodes each packet into 48 kHz stereo float samples, interleaved. Android
//! decodes Opus with MediaCodec and does not build this.

use std::os::raw::c_int;

use opusic_sys::{OpusDecoder, opus_decode_float, opus_decoder_create, opus_decoder_destroy};

pub const SAMPLE_RATE: i32 = 48_000;
pub const CHANNELS: usize = 2;
/// The longest Opus packet: 120 ms.
pub const MOST_SAMPLES: usize = 5760;

pub struct Decoder(*mut OpusDecoder);

// Used by one thread at a time (behind the core's lock).
unsafe impl Send for Decoder {}

impl Decoder {
    pub fn new() -> Result<Decoder, String> {
        let mut error: c_int = 0;
        let decoder = unsafe { opus_decoder_create(SAMPLE_RATE, CHANNELS as c_int, &mut error) };
        if decoder.is_null() || error != 0 {
            return Err(format!("opus_decoder_create: error {error}"));
        }
        Ok(Decoder(decoder))
    }

    /// Decodes one packet into `pcm` (interleaved stereo); the number of samples per channel.
    pub fn decode(&mut self, packet: &[u8], pcm: &mut [f32]) -> Result<usize, String> {
        let room = (pcm.len() / CHANNELS).min(MOST_SAMPLES);
        let samples = unsafe {
            opus_decode_float(
                self.0,
                packet.as_ptr(),
                packet.len() as i32,
                pcm.as_mut_ptr(),
                room as c_int,
                0,
            )
        };
        if samples < 0 {
            return Err(format!("opus_decode_float: error {samples}"));
        }
        Ok(samples as usize)
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        unsafe { opus_decoder_destroy(self.0) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opusic_sys::{opus_encode_float, opus_encoder_create, opus_encoder_destroy};

    #[test]
    fn a_tone_comes_back() {
        let mut error: c_int = 0;
        // OPUS_APPLICATION_AUDIO = 2049.
        let encoder =
            unsafe { opus_encoder_create(SAMPLE_RATE, CHANNELS as c_int, 2049, &mut error) };
        assert!(!encoder.is_null() && error == 0);
        let mut decoder = Decoder::new().unwrap();
        let mut peak = 0f32;
        for frame in 0..20 {
            // 10 ms of a 440 Hz tone at half scale.
            let input: Vec<f32> = (0..480 * CHANNELS)
                .map(|i| {
                    let t = (frame * 480 + i / CHANNELS) as f32 / SAMPLE_RATE as f32;
                    0.5 * (2.0 * std::f32::consts::PI * 440.0 * t).sin()
                })
                .collect();
            let mut packet = [0u8; 1500];
            let length = unsafe {
                opus_encode_float(
                    encoder,
                    input.as_ptr(),
                    480,
                    packet.as_mut_ptr(),
                    packet.len() as i32,
                )
            };
            assert!(length > 0);
            let mut pcm = vec![0f32; MOST_SAMPLES * CHANNELS];
            assert_eq!(
                decoder.decode(&packet[..length as usize], &mut pcm),
                Ok(480)
            );
            if frame > 5 {
                peak = pcm[..480 * CHANNELS]
                    .iter()
                    .fold(peak, |m, s| m.max(s.abs()));
            }
        }
        unsafe { opus_encoder_destroy(encoder) };
        assert!(peak > 0.35 && peak < 0.65, "peak {peak}");
    }
}
