//! libopus through its C interface (opusic-sys): 48 kHz stereo float, 10 ms frames.

use std::os::raw::c_int;

use opusic_sys::{
    OpusDecoder, OpusEncoder, opus_decode_float, opus_decoder_create, opus_decoder_destroy,
    opus_encode_float, opus_encoder_create, opus_encoder_ctl, opus_encoder_destroy,
};

use crate::{CHANNELS, FRAME, SAMPLE_RATE};

/// From opus_defines.h.
const APPLICATION_RESTRICTED_LOWDELAY: c_int = 2051;
const SET_BITRATE: c_int = 4002;

pub struct Encoder(*mut OpusEncoder);

// The encoder is used by one thread at a time (moved into the capture thread).
unsafe impl Send for Encoder {}

impl Encoder {
    /// The low-delay mode (CELT only): 2.5 ms look-ahead instead of 6.5 ms, good for any sound.
    pub fn new(bitrate: i32) -> Result<Encoder, String> {
        let mut error: c_int = 0;
        let encoder = unsafe {
            opus_encoder_create(
                SAMPLE_RATE as i32,
                CHANNELS as c_int,
                APPLICATION_RESTRICTED_LOWDELAY,
                &mut error,
            )
        };
        if encoder.is_null() || error != 0 {
            return Err(format!("opus_encoder_create: error {error}"));
        }
        let encoder = Encoder(encoder);
        let result = unsafe { opus_encoder_ctl(encoder.0, SET_BITRATE, bitrate) };
        if result != 0 {
            return Err(format!("setting the bitrate: error {result}"));
        }
        Ok(encoder)
    }

    /// One frame of `FRAME` interleaved stereo samples -> one packet.
    pub fn encode(&mut self, pcm: &[f32]) -> Result<Vec<u8>, String> {
        if pcm.len() != FRAME * CHANNELS {
            return Err(format!(
                "a frame has {} samples, not {}",
                pcm.len(),
                FRAME * CHANNELS
            ));
        }
        let mut packet = vec![0u8; 1275];
        let length = unsafe {
            opus_encode_float(
                self.0,
                pcm.as_ptr(),
                FRAME as c_int,
                packet.as_mut_ptr(),
                packet.len() as i32,
            )
        };
        if length < 0 {
            return Err(format!("opus_encode_float: error {length}"));
        }
        packet.truncate(length as usize);
        Ok(packet)
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        unsafe { opus_encoder_destroy(self.0) };
    }
}

pub struct Decoder(*mut OpusDecoder);

unsafe impl Send for Decoder {}

impl Decoder {
    pub fn new() -> Result<Decoder, String> {
        let mut error: c_int = 0;
        let decoder =
            unsafe { opus_decoder_create(SAMPLE_RATE as i32, CHANNELS as c_int, &mut error) };
        if decoder.is_null() || error != 0 {
            return Err(format!("opus_decoder_create: error {error}"));
        }
        Ok(Decoder(decoder))
    }

    /// Decodes one packet into `pcm` (`FRAME` interleaved stereo samples); None conceals a lost packet.
    pub fn decode(&mut self, packet: Option<&[u8]>, pcm: &mut [f32]) -> Result<(), String> {
        if pcm.len() != FRAME * CHANNELS {
            return Err("wrong frame size".into());
        }
        let (data, length) = match packet {
            Some(packet) => (packet.as_ptr(), packet.len() as i32),
            None => (std::ptr::null(), 0),
        };
        let samples =
            unsafe { opus_decode_float(self.0, data, length, pcm.as_mut_ptr(), FRAME as c_int, 0) };
        if samples < 0 {
            return Err(format!("opus_decode_float: error {samples}"));
        }
        // A packet of another duration (the sender's choice) would not fit the 10 ms frame.
        if samples as usize != FRAME {
            pcm.fill(0.0);
            return Err(format!("a packet of {samples} samples, not {FRAME}"));
        }
        Ok(())
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        unsafe { opus_decoder_destroy(self.0) };
    }
}
