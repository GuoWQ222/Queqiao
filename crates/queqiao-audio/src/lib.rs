//! Sound for a session.
//!
//! - [`Capture`] (controlled PC): what its speakers play, through WASAPI loopback, as Opus packets of 10 ms. The
//!   Windows audio engine converts to 48 kHz stereo float; digital silence is not sent, and the packets' times
//!   follow the wall clock across silences, so the player knows a gap from a loss.
//! - [`Player`] (controlling PC): decodes and plays through WASAPI with about 30 ms queued at the device; a lost
//!   packet is concealed by Opus, a late one dropped, and the queue is cut when it grows past 200 ms.
//! - [`Tone`]: a test tone into a render device (tests capture it again from a virtual device, so nothing is
//!   heard on real speakers).
//!
//! Each runs on its own thread with its own COM apartment.
// Windows only for now: the Linux layer comes in docs/LINUX.md step 2 (the platform API); until then this crate
// is empty on Linux.
#![cfg(windows)]

mod opus;
mod wasapi;

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

pub use wasapi::{render_devices, set_mute};

pub const SAMPLE_RATE: u32 = 48_000;
pub const CHANNELS: usize = 2;
/// Samples per channel in one packet: 10 ms.
pub const FRAME: usize = 480;
/// Opus bitrate for desktop sound (music, games, calls): clear stereo.
const BITRATE: i32 = 128_000;
/// The frequency of the test tone: exactly bin 10 of a 480-sample frame, so the check is exact.
pub const TEST_TONE_HZ: f32 = 1000.0;

/// Captures the controlled PC's sound and hands each Opus packet to `send(packet, time in 48 kHz samples)`.
pub struct Capture {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    stats: Arc<Mutex<CaptureStats>>,
    notes: Receiver<String>,
}

#[derive(Clone, Debug, Default)]
pub struct CaptureStats {
    pub device: String,
    pub packets: u64,
    pub bytes: u64,
    /// 10 ms frames left out because they were digital silence.
    pub silent: u64,
}

/// Where captured sound comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// What a render device plays (part of its name; None: the default speakers, followed when that changes).
    Loopback(Option<String>),
    /// A microphone (part of its name; None: the default one, followed).
    Microphone(Option<String>),
    /// The test tone (TEST_TONE_HZ), for tests.
    Tone,
}

impl Capture {
    /// `device`: part of a render device's name, or None for the default speakers or headphones (followed when
    /// the default changes).
    pub fn start(device: Option<String>, send: Box<dyn FnMut(Vec<u8>, u64) + Send>) -> Capture {
        Self::from_source(Source::Loopback(device), send)
    }

    /// Sound from `source`, as Opus packets to `send`.
    pub fn from_source(source: Source, send: Box<dyn FnMut(Vec<u8>, u64) + Send>) -> Capture {
        let stop = Arc::new(AtomicBool::new(false));
        let stats = Arc::new(Mutex::new(CaptureStats::default()));
        let (note_sender, notes) = mpsc::channel();
        let thread = {
            let (stop, stats) = (stop.clone(), stats.clone());
            std::thread::Builder::new()
                .name("queqiao-sound-capture".into())
                .spawn(move || capture_thread(source, send, &stop, &stats, &note_sender))
                .ok()
        };
        Capture {
            stop,
            thread,
            stats,
            notes,
        }
    }

    pub fn stats(&self) -> CaptureStats {
        self.stats
            .lock()
            .map(|stats| stats.clone())
            .unwrap_or_default()
    }

    /// Things worth a report line (device opened, lost, errors).
    pub fn notes(&self) -> Vec<String> {
        self.notes.try_iter().collect()
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn capture_thread(
    source: Source,
    mut send: Box<dyn FnMut(Vec<u8>, u64) + Send>,
    stop: &AtomicBool,
    stats: &Mutex<CaptureStats>,
    notes: &Sender<String>,
) {
    let device = match &source {
        Source::Loopback(device) | Source::Microphone(device) => device.clone(),
        Source::Tone => None,
    };
    let _com = wasapi::Com::init();
    let mut encoder = match opus::Encoder::new(BITRATE) {
        Ok(encoder) => encoder,
        Err(error) => {
            let _ = notes.send(format!("sound: no encoder ({error})"));
            return;
        }
    };
    let started = Instant::now();
    // Stream time in 48 kHz samples, kept on the wall clock across silences.
    let mut position = 0u64;
    let mut pending: Vec<f32> = Vec::new();
    let mut fresh: Vec<f32> = Vec::new();
    if source == Source::Tone {
        tone_source(&mut encoder, &mut send, stop, stats, notes);
        return;
    }
    while !stop.load(Ordering::Relaxed) {
        let opened = match &source {
            Source::Microphone(_) => wasapi::Stream::microphone(device.as_deref()),
            _ => wasapi::Stream::loopback(device.as_deref()),
        };
        let stream = match opened {
            Ok(stream) => stream,
            Err(error) => {
                let _ = notes.send(format!("sound: not captured ({error}); trying again"));
                sleep_unless(stop, Duration::from_secs(2));
                continue;
            }
        };
        let _ = notes.send(format!("sound from: {}", stream.name));
        if let Ok(mut stats) = stats.lock() {
            stats.device = stream.name.clone();
        }
        let mut default_checked = Instant::now();
        pending.clear();
        loop {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            if let Err(error) = stream.read(&mut fresh) {
                let _ = notes.send(format!("sound: device lost ({error}); opening again"));
                break;
            }
            if fresh.is_empty() {
                // Follow the default device (headphones plugged in, another output chosen).
                if device.is_none() && default_checked.elapsed() > Duration::from_secs(2) {
                    default_checked = Instant::now();
                    if !stream.is_default() {
                        let _ = notes.send("sound: the default device changed".to_string());
                        break;
                    }
                }
                std::thread::sleep(Duration::from_millis(3));
                continue;
            }
            // WASAPI delivers nothing while nothing plays: after such a gap, the stream time jumps to now.
            let wall = started.elapsed().as_micros() as u64 * u64::from(SAMPLE_RATE) / 1_000_000;
            if pending.is_empty() && wall > position + 4 * FRAME as u64 {
                position = wall - wall % FRAME as u64;
            }
            pending.extend_from_slice(&fresh);
            while pending.len() >= FRAME * CHANNELS {
                let frame: Vec<f32> = pending.drain(..FRAME * CHANNELS).collect();
                if frame.iter().all(|sample| *sample == 0.0) {
                    if let Ok(mut stats) = stats.lock() {
                        stats.silent += 1;
                    }
                } else {
                    match encoder.encode(&frame) {
                        Ok(packet) => {
                            if let Ok(mut stats) = stats.lock() {
                                stats.packets += 1;
                                stats.bytes += packet.len() as u64;
                            }
                            send(packet, position);
                        }
                        Err(error) => {
                            let _ = notes.send(format!("sound: encoding failed ({error})"));
                        }
                    }
                }
                position += FRAME as u64;
            }
        }
    }
}

/// The test tone as the sound source: 10 ms frames on the wall clock, encoded and sent.
fn tone_source(
    encoder: &mut opus::Encoder,
    send: &mut Box<dyn FnMut(Vec<u8>, u64) + Send>,
    stop: &AtomicBool,
    stats: &Mutex<CaptureStats>,
    notes: &Sender<String>,
) {
    let _ = notes.send(format!("sound from: a {TEST_TONE_HZ} Hz test tone"));
    if let Ok(mut stats) = stats.lock() {
        stats.device = "test tone".into();
    }
    let step = 2.0 * std::f32::consts::PI * TEST_TONE_HZ / SAMPLE_RATE as f32;
    let started = Instant::now();
    let mut position = 0u64;
    while !stop.load(Ordering::Relaxed) {
        let due = Duration::from_micros(position * 1_000_000 / u64::from(SAMPLE_RATE));
        if started.elapsed() < due {
            std::thread::sleep(Duration::from_millis(2));
            continue;
        }
        let mut frame = Vec::with_capacity(FRAME * CHANNELS);
        for at in 0..FRAME as u64 {
            let value = 0.25 * ((position + at) as f32 * step).sin();
            frame.extend([value; CHANNELS]);
        }
        if let Ok(packet) = encoder.encode(&frame) {
            if let Ok(mut stats) = stats.lock() {
                stats.packets += 1;
                stats.bytes += packet.len() as u64;
            }
            send(packet, position);
        }
        position += FRAME as u64;
    }
}

/// A virtual cable on this PC: what is played into `speaker` comes out of `microphone`, which programs can use as
/// their microphone. VB-Audio's VB-CABLE (its speaker "CABLE Input", or "Speakers (VB-Audio Virtual Cable)" in the
/// display language, its microphone "CABLE Output"), VoiceMeeter's, or Virtual Audio Cable's. Not Steam Streaming
/// Microphone: measured 2026-11-22, what is played into its speaker comes out of its microphone at another rate and in
/// another sample format (a 1 kHz tone as 450 Hz noise), as Steam feeds it its own way. Queqiao never installs one
/// (VB-CABLE's licence does not allow bundling it): it says where to get one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VirtualMicrophone {
    pub speaker: String,
    pub microphone: String,
}

pub fn virtual_microphone() -> Option<VirtualMicrophone> {
    pair_virtual_microphone(&wasapi::render_devices(), &wasapi::capture_devices())
}

fn pair_virtual_microphone(
    speakers: &[String],
    microphones: &[String],
) -> Option<VirtualMicrophone> {
    let has = |name: &str, part: &str| name.to_lowercase().contains(&part.to_lowercase());
    // (speaker names, microphone name), first match wins; among speakers, an ordinary one before VB-CABLE's
    // 16-channel one.
    let pairs: [(&[&str], &str); 3] = [
        (&["CABLE Input", "VB-Audio Virtual Cable"], "CABLE Output"),
        (&["VoiceMeeter Input"], "VoiceMeeter Output"),
        (&["Virtual Audio Cable"], "Virtual Audio Cable"),
    ];
    pairs.iter().find_map(|(speaker_parts, microphone_part)| {
        let speaker = speakers
            .iter()
            .filter(|name| speaker_parts.iter().any(|part| has(name, part)))
            // "CABLE In 16 Ch" in the device list, "CABLE In 16ch" as an endpoint.
            .min_by_key(|name| name.to_lowercase().replace(' ', "").contains("16ch"))?;
        let microphone = microphones.iter().find(|name| has(name, microphone_part))?;
        Some(VirtualMicrophone {
            speaker: speaker.clone(),
            microphone: microphone.clone(),
        })
    })
}

/// What a microphone hears for `duration` (48 kHz, interleaved stereo), for checks.
pub fn record(microphone: &str, duration: Duration) -> Result<Vec<f32>, String> {
    let _com = wasapi::Com::init();
    let stream = wasapi::Stream::microphone(Some(microphone))?;
    let until = Instant::now() + duration;
    let (mut all, mut fresh) = (Vec::new(), Vec::new());
    while Instant::now() < until {
        stream.read(&mut fresh)?;
        all.extend_from_slice(&fresh);
        std::thread::sleep(Duration::from_millis(5));
    }
    Ok(all)
}

/// Listens to a microphone for `duration`: (10 ms frames with sound, frames that were mostly the test tone).
pub fn listen_for_tone(microphone: &str, duration: Duration) -> Result<(u32, u32), String> {
    let _com = wasapi::Com::init();
    let stream = wasapi::Stream::microphone(Some(microphone))?;
    let until = Instant::now() + duration;
    let (mut heard, mut tone) = (0u32, 0u32);
    let (mut pending, mut fresh) = (Vec::new(), Vec::new());
    while Instant::now() < until {
        stream.read(&mut fresh)?;
        pending.extend_from_slice(&fresh);
        while pending.len() >= FRAME * CHANNELS {
            let frame: Vec<f32> = pending.drain(..FRAME * CHANNELS).collect();
            let (sound, is_tone) = tone_check(&frame);
            heard += u32::from(sound);
            tone += u32::from(is_tone);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    Ok((heard, tone))
}

fn sleep_unless(stop: &AtomicBool, duration: Duration) {
    let until = Instant::now() + duration;
    while !stop.load(Ordering::Relaxed) && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Plays the other PC's sound.
pub struct Player {
    packets: Sender<(Arc<[u8]>, u64)>,
    stop: Arc<AtomicBool>,
    muted: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    stats: Arc<Mutex<PlayerStats>>,
    notes: Receiver<String>,
}

#[derive(Clone, Debug, Default)]
pub struct PlayerStats {
    pub device: String,
    pub received: u64,
    pub bytes: u64,
    pub played: u64,
    /// Frames Opus made up for packets that did not arrive in time.
    pub concealed: u64,
    /// Packets that came after their time, or were cut to keep the delay short.
    pub dropped: u64,
    /// With the tone check on: frames with sound, and how many of them were mostly the test tone.
    pub loud_frames: u64,
    pub tone_frames: u64,
}

/// Frames kept queued at the device: the margin for network jitter.
const DEVICE_TARGET: u32 = 3 * FRAME as u32;
/// Silence written before sound starts (after a gap), so the device queue starts at its target.
const PREROLL: usize = 2 * FRAME;
/// A gap longer than this in the packet times is silence on the other side: playing resumes from the new time.
const RESYNC_GAP: u64 = 10 * FRAME as u64;
/// More than this waiting (200 ms): the oldest packets are dropped down to `CATCH_UP`.
const MAX_WAITING: u64 = 20 * FRAME as u64;
const CATCH_UP: u64 = 6 * FRAME as u64;

impl Player {
    /// `device`: part of a render device's name, or None for the default. With `check_tone`, each decoded
    /// frame is checked for the test tone (see [`PlayerStats`]).
    pub fn start(device: Option<String>, check_tone: bool) -> Player {
        let (packets, incoming) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let muted = Arc::new(AtomicBool::new(false));
        let stats = Arc::new(Mutex::new(PlayerStats::default()));
        let (note_sender, notes) = mpsc::channel();
        let thread = {
            let (stop, muted, stats) = (stop.clone(), muted.clone(), stats.clone());
            std::thread::Builder::new()
                .name("queqiao-sound-play".into())
                .spawn(move || {
                    play_thread(
                        device,
                        check_tone,
                        &incoming,
                        &stop,
                        &muted,
                        &stats,
                        &note_sender,
                    )
                })
                .ok()
        };
        Player {
            packets,
            stop,
            muted,
            thread,
            stats,
            notes,
        }
    }

    pub fn push(&self, packet: Arc<[u8]>, time_48khz: u64) {
        let _ = self.packets.send((packet, time_48khz));
    }

    pub fn set_muted(&self, muted: bool) {
        self.muted.store(muted, Ordering::Relaxed);
    }

    pub fn stats(&self) -> PlayerStats {
        self.stats
            .lock()
            .map(|stats| stats.clone())
            .unwrap_or_default()
    }

    pub fn notes(&self) -> Vec<String> {
        self.notes.try_iter().collect()
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn play_thread(
    device: Option<String>,
    check_tone: bool,
    incoming: &Receiver<(Arc<[u8]>, u64)>,
    stop: &AtomicBool,
    muted: &AtomicBool,
    stats: &Mutex<PlayerStats>,
    notes: &Sender<String>,
) {
    let _com = wasapi::Com::init();
    let mut decoder = match opus::Decoder::new() {
        Ok(decoder) => decoder,
        Err(error) => {
            let _ = notes.send(format!("sound: no decoder ({error})"));
            return;
        }
    };
    let mut waiting: BTreeMap<u64, Arc<[u8]>> = BTreeMap::new();
    let mut pcm = vec![0f32; FRAME * CHANNELS];
    while !stop.load(Ordering::Relaxed) {
        let stream = match wasapi::Stream::render(device.as_deref()) {
            Ok(stream) => stream,
            Err(error) => {
                let _ = notes.send(format!("sound: not played ({error}); trying again"));
                sleep_unless(stop, Duration::from_secs(2));
                continue;
            }
        };
        let _ = notes.send(format!("sound to: {}", stream.name));
        if let Ok(mut stats) = stats.lock() {
            stats.device = stream.name.clone();
        }
        // Sound starts again from the first packet on a new device.
        let mut next: Option<u64> = None;
        'device: loop {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            for (packet, time) in incoming.try_iter() {
                if let Ok(mut stats) = stats.lock() {
                    stats.received += 1;
                    stats.bytes += packet.len() as u64;
                    if next.is_some_and(|next| time < next) {
                        stats.dropped += 1;
                        continue;
                    }
                }
                waiting.insert(time, packet);
            }
            // Keep the delay short: cut the oldest when too much waits.
            if let (Some(&first), Some(&last)) = (waiting.keys().next(), waiting.keys().next_back())
                && last - first > MAX_WAITING
            {
                let keep_from = last - CATCH_UP;
                let before = waiting.len();
                waiting.retain(|time, _| *time >= keep_from);
                if let Ok(mut stats) = stats.lock() {
                    stats.dropped += (before - waiting.len()) as u64;
                }
                next = Some(keep_from.max(next.unwrap_or(0)));
            }
            let queued = match stream.padding() {
                Ok(queued) => queued,
                Err(error) => {
                    let _ = notes.send(format!("sound: device lost ({error}); opening again"));
                    break 'device;
                }
            };
            let mut room = DEVICE_TARGET.saturating_sub(queued);
            while room >= FRAME as u32 {
                let Some(&first) = waiting.keys().next() else {
                    break;
                };
                let current = match next {
                    Some(current) if first <= current + RESYNC_GAP => current,
                    // The first sound, or sound again after a silence: start a little behind.
                    _ => {
                        if stream.write(&vec![0f32; PREROLL * CHANNELS]).is_err() {
                            break 'device;
                        }
                        room = room.saturating_sub(PREROLL as u32);
                        first
                    }
                };
                let decoded = match waiting.remove(&current) {
                    Some(packet) => decoder.decode(Some(&packet), &mut pcm),
                    None => {
                        if let Ok(mut stats) = stats.lock() {
                            stats.concealed += 1;
                        }
                        decoder.decode(None, &mut pcm)
                    }
                };
                if decoded.is_err() {
                    pcm.fill(0.0);
                }
                if check_tone {
                    let (loud, tone) = tone_check(&pcm);
                    if let Ok(mut stats) = stats.lock() {
                        stats.loud_frames += u64::from(loud);
                        stats.tone_frames += u64::from(tone);
                    }
                }
                if muted.load(Ordering::Relaxed) {
                    pcm.fill(0.0);
                }
                if stream.write(&pcm).is_err() {
                    break 'device;
                }
                if let Ok(mut stats) = stats.lock() {
                    stats.played += 1;
                }
                next = Some(current + FRAME as u64);
                room = room.saturating_sub(FRAME as u32);
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}

/// (the frame has sound, most of its energy is the test tone), on the left channel.
fn tone_check(pcm: &[f32]) -> (bool, bool) {
    let left: Vec<f32> = pcm.iter().step_by(CHANNELS).copied().collect();
    let energy: f32 = left.iter().map(|sample| sample * sample).sum();
    if energy / (left.len() as f32) < 1e-5 {
        return (false, false);
    }
    // Goertzel at the tone's frequency: for a pure tone, power = (N/2) * energy.
    let omega = 2.0 * std::f32::consts::PI * TEST_TONE_HZ / SAMPLE_RATE as f32;
    let coefficient = 2.0 * omega.cos();
    let (mut previous, mut before) = (0f32, 0f32);
    for sample in &left {
        let current = sample + coefficient * previous - before;
        before = previous;
        previous = current;
    }
    let power = previous * previous + before * before - coefficient * previous * before;
    let share = power / (left.len() as f32 / 2.0 * energy);
    (true, share > 0.5)
}

/// Plays a sine tone into a render device until dropped (tests).
pub struct Tone {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Tone {
    pub fn start(device: Option<String>, frequency: f32) -> Result<Tone, String> {
        let stop = Arc::new(AtomicBool::new(false));
        let (ready_sender, ready) = mpsc::channel();
        let thread = {
            let stop = stop.clone();
            std::thread::Builder::new()
                .name("queqiao-test-tone".into())
                .spawn(move || {
                    let _com = wasapi::Com::init();
                    let stream = match wasapi::Stream::render(device.as_deref()) {
                        Ok(stream) => {
                            let _ = ready_sender.send(Ok(stream.name.clone()));
                            stream
                        }
                        Err(error) => {
                            let _ = ready_sender.send(Err(error));
                            return;
                        }
                    };
                    let step = 2.0 * std::f32::consts::PI * frequency / SAMPLE_RATE as f32;
                    let mut phase = 0f32;
                    let mut buffer = vec![0f32; FRAME * CHANNELS];
                    while !stop.load(Ordering::Relaxed) {
                        while stream.padding().is_ok_and(|queued| queued < DEVICE_TARGET) {
                            for pair in buffer.chunks_mut(CHANNELS) {
                                pair.fill(0.25 * phase.sin());
                                phase = (phase + step) % (2.0 * std::f32::consts::PI);
                            }
                            if stream.write(&buffer).is_err() {
                                return;
                            }
                        }
                        std::thread::sleep(Duration::from_millis(3));
                    }
                })
                .map_err(|e| e.to_string())?
        };
        ready
            .recv()
            .map_err(|_| "the tone thread ended".to_string())?
            .map(|_| Tone {
                stop,
                thread: Some(thread),
            })
    }
}

impl Drop for Tone {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod microphone_tests {
    use super::*;

    #[test]
    fn virtual_cables_are_paired_by_their_names() {
        let names = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        // As on the test PC (Chinese Windows): Steam's pair, a headset.
        let speakers = names(&[
            "\u{626c}\u{58f0}\u{5668} (Steam Streaming Speakers)",
            "\u{626c}\u{58f0}\u{5668} (Steam Streaming Microphone)",
            "Headphones (Feaulle SHINE)",
        ]);
        let microphones = names(&[
            "\u{9ea6}\u{514b}\u{98ce} (Steam Streaming Microphone)",
            "Microphone (Feaulle SHINE)",
        ]);
        // Steam's pair is not a cable (it changes the sound): not taken.
        assert_eq!(pair_virtual_microphone(&speakers, &microphones), None);
        // VB-CABLE as installed on the test PC (2026-11-22, pack 46): its speaker in the display language and a
        // 16-channel one; the ordinary one is taken.
        let mut with_cable = speakers.clone();
        with_cable.push("CABLE In 16ch (VB-Audio Virtual Cable)".into());
        with_cable.push("CABLE In 16 Ch (VB-Audio Virtual Cable)".into());
        with_cable.push("\u{626c}\u{58f0}\u{5668} (VB-Audio Virtual Cable)".into());
        let mut cable_mics = microphones.clone();
        cable_mics.push("CABLE Output (VB-Audio Virtual Cable)".into());
        let found = pair_virtual_microphone(&with_cable, &cable_mics).unwrap();
        assert!(
            found.speaker.starts_with('\u{626c}') && found.microphone.starts_with("CABLE Output")
        );
        // An older VB-CABLE: "CABLE Input".
        let older = names(&["CABLE Input (VB-Audio Virtual Cable)"]);
        let found = pair_virtual_microphone(&older, &cable_mics).unwrap();
        assert!(found.speaker.starts_with("CABLE Input"));
        // A speaker without its microphone side: none.
        let headset = names(&["Microphone (Feaulle SHINE)"]);
        assert_eq!(pair_virtual_microphone(&with_cable, &headset), None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opus_round_trip_keeps_the_tone() {
        let mut encoder = opus::Encoder::new(BITRATE).unwrap();
        let mut decoder = opus::Decoder::new().unwrap();
        let step = 2.0 * std::f32::consts::PI * TEST_TONE_HZ / SAMPLE_RATE as f32;
        let mut pcm = vec![0f32; FRAME * CHANNELS];
        let (mut loud, mut tone) = (0, 0);
        for index in 0..50 {
            let frame: Vec<f32> = (0..FRAME * CHANNELS)
                .map(|sample| 0.25 * (((index * FRAME + sample / CHANNELS) as f32) * step).sin())
                .collect();
            let packet = encoder.encode(&frame).unwrap();
            assert!(
                packet.len() < 400,
                "a 10 ms packet at 128 kbit/s is about 160 bytes"
            );
            decoder.decode(Some(&packet), &mut pcm).unwrap();
            let (is_loud, is_tone) = tone_check(&pcm);
            loud += u32::from(is_loud);
            tone += u32::from(is_tone);
        }
        // The codec's first frames ramp up; nearly all the rest must be the tone.
        assert!(loud >= 45 && tone >= loud - 3, "loud {loud}, tone {tone}");
        // Concealment gives a frame of the right size too.
        decoder.decode(None, &mut pcm).unwrap();
    }

    #[test]
    fn the_tone_check_tells_a_tone_from_noise() {
        let step = 2.0 * std::f32::consts::PI * TEST_TONE_HZ / SAMPLE_RATE as f32;
        let tone: Vec<f32> = (0..FRAME * CHANNELS)
            .map(|sample| 0.25 * ((sample / CHANNELS) as f32 * step).sin())
            .collect();
        assert_eq!(tone_check(&tone), (true, true));
        // A different pitch (1.5 kHz) is sound, but not the tone.
        let other: Vec<f32> = (0..FRAME * CHANNELS)
            .map(|sample| 0.25 * ((sample / CHANNELS) as f32 * step * 1.5).sin())
            .collect();
        assert_eq!(tone_check(&other), (true, false));
        assert_eq!(tone_check(&vec![0.0; FRAME * CHANNELS]), (false, false));
    }
}

/// Measurement (run by hand: `cargo test -p queqiao-audio -- --ignored --nocapture loopback_while_muted`):
/// does loopback still capture a device's sound while its endpoint is muted? Uses the virtual device named in
/// QUEQIAO_TEST_DEVICE (nothing is heard).
#[cfg(test)]
mod mute_measurement {
    use super::*;

    fn loudness(device: &str, seconds: f32) -> f32 {
        let stream = wasapi::Stream::loopback(Some(device)).expect("loopback");
        let (mut sum, mut count) = (0f64, 0usize);
        let mut fresh = Vec::new();
        let until = Instant::now() + Duration::from_secs_f32(seconds);
        while Instant::now() < until {
            stream.read(&mut fresh).expect("read");
            for sample in &fresh {
                sum += f64::from(*sample) * f64::from(*sample);
            }
            count += fresh.len();
            std::thread::sleep(Duration::from_millis(5));
        }
        if count == 0 {
            0.0
        } else {
            (sum / count as f64).sqrt() as f32
        }
    }

    #[test]
    #[ignore]
    fn loopback_while_muted() {
        let device = std::env::var("QUEQIAO_TEST_DEVICE")
            .unwrap_or_else(|_| "Steam Streaming Speakers".into());
        let _com = wasapi::Com::init();
        let _tone = Tone::start(Some(device.clone()), TEST_TONE_HZ).expect("tone");
        std::thread::sleep(Duration::from_millis(300));
        let before = loudness(&device, 1.0);
        wasapi::set_mute(Some(&device), true).expect("mute");
        let muted = loudness(&device, 1.0);
        wasapi::set_mute(Some(&device), false).expect("unmute");
        let after = loudness(&device, 1.0);
        println!("MUTE_MEASUREMENT before {before:.4} muted {muted:.4} after {after:.4}");
    }
}
