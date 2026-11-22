//! Is a speaker / microphone pair a virtual cable? Listens to the microphone for 2 s with nothing played, then for 2 s
//! while the test tone plays into the speaker; prints how many 10 ms frames had sound and how many were the tone, and
//! what the microphone heard: its level and its strongest frequency on each channel.
//! Usage: cable_probe <part of the speaker's name, or - to play nothing> <part of the microphone's name>
//!        cable_probe auto (the virtual microphone Queqiao would use)

use std::time::Duration;

/// Goertzel power of `samples` at `frequency` (48 kHz).
fn power(samples: &[f32], frequency: f32) -> f32 {
    let omega = 2.0 * std::f32::consts::PI * frequency / 48_000.0;
    let coefficient = 2.0 * omega.cos();
    let (mut previous, mut before) = (0f32, 0f32);
    for sample in samples {
        let current = sample + coefficient * previous - before;
        before = previous;
        previous = current;
    }
    previous * previous + before * before - coefficient * previous * before
}

fn describe(samples: &[f32]) -> String {
    let mut parts = Vec::new();
    for channel in 0..2 {
        let mono: Vec<f32> = samples.iter().skip(channel).step_by(2).copied().collect();
        if mono.is_empty() {
            return "nothing recorded".into();
        }
        let rms = (mono.iter().map(|s| s * s).sum::<f32>() / mono.len() as f32).sqrt();
        // The strongest frequency, 50 Hz steps up to 12 kHz, on the middle second.
        let middle = &mono[mono.len() / 4..mono.len() * 3 / 4];
        let (best, best_power) = (1..=240)
            .map(|step| step as f32 * 50.0)
            .map(|frequency| (frequency, power(middle, frequency)))
            .fold(
                (0.0, 0.0),
                |best, item| if item.1 > best.1 { item } else { best },
            );
        let at_tone = power(middle, 1000.0);
        parts.push(format!(
            "channel {channel}: rms {rms:.4}, strongest {best} Hz, the tone's share of that {:.2}",
            if best_power > 0.0 {
                at_tone / best_power
            } else {
                0.0
            }
        ));
    }
    parts.join("; ")
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("auto") {
        match queqiao_audio::virtual_microphone() {
            Some(cable) => {
                println!(
                    "virtual microphone: speaker \"{}\", microphone \"{}\"",
                    cable.speaker, cable.microphone
                );
                args = vec![cable.speaker, cable.microphone];
            }
            None => {
                println!("no virtual microphone on this PC");
                std::process::exit(1);
            }
        }
    }
    let [speaker, microphone] = &args[..] else {
        println!("usage: cable_probe <speaker> <microphone>");
        std::process::exit(2);
    };
    let quiet = queqiao_audio::listen_for_tone(microphone, Duration::from_secs(2));
    println!("nothing played: {quiet:?} (frames with sound, frames of the tone)");
    if speaker == "-" {
        match queqiao_audio::record(microphone, Duration::from_secs(2)) {
            Ok(samples) => {
                let odd = samples
                    .iter()
                    .filter(|s| !s.is_finite() || s.abs() > 1.0)
                    .count();
                println!(
                    "heard: {} samples ({odd} outside -1..1 or not numbers), first {:?}; {}",
                    samples.len(),
                    &samples[..samples.len().min(8)],
                    describe(&samples)
                );
            }
            Err(error) => println!("recording failed: {error}"),
        }
        return;
    }
    let tone = match queqiao_audio::Tone::start(Some(speaker.clone()), queqiao_audio::TEST_TONE_HZ)
    {
        Ok(tone) => tone,
        Err(error) => {
            println!("the tone did not start: {error}");
            std::process::exit(1);
        }
    };
    std::thread::sleep(Duration::from_millis(300));
    let playing = queqiao_audio::listen_for_tone(microphone, Duration::from_secs(2));
    println!("the tone playing into the speaker: {playing:?}");
    match queqiao_audio::record(microphone, Duration::from_secs(2)) {
        Ok(samples) => println!("heard: {} samples; {}", samples.len(), describe(&samples)),
        Err(error) => println!("recording failed: {error}"),
    }
    drop(tone);
    let cable = matches!(playing, Ok((_, tone)) if tone >= 50);
    println!("a virtual cable: {cable}");
}
