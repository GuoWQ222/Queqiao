//! The mobile core as the apps use it, on a desktop (scripts/run_mobile_core_probe.sh): logs in, waits for the device
//! in the list, connects, counts frames and sound packets, sends a click and typing, disconnects. The events it got
//! are printed as they come.
//! Usage: core_probe <folder> <server> <pin> <user> <password> <device name> [seconds, default 10]
//! Prints CORE_PROBE_PASS frames=N keyframes=K audio=A or CORE_PROBE_FAIL <why>.

use queqiao_mobile::Core;
use serde_json::{Value, json};
use std::time::{Duration, Instant};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 7 {
        println!(
            "usage: core_probe <folder> <server> <pin> <user> <password> <device name> [seconds]"
        );
        return;
    }
    let seconds: u64 = args.get(7).and_then(|s| s.parse().ok()).unwrap_or(10);
    let core = Core::start(&args[1], "core probe");
    let events = |core: &Core, until: Instant, done: &mut dyn FnMut(&Value) -> bool| -> bool {
        while Instant::now() < until {
            if let Some(event) = core.next_event(Duration::from_millis(100)) {
                println!("event: {event}");
                if let Ok(value) = serde_json::from_str::<Value>(&event)
                    && done(&value)
                {
                    return true;
                }
            }
        }
        false
    };
    core.command(&json!({"login": {"server": args[2], "pin": args[3], "user": args[4], "password": args[5]}}).to_string());
    let mut device_id = String::new();
    let listed = events(
        &core,
        Instant::now() + Duration::from_secs(20),
        &mut |value| {
            if let Some(Value::Array(list)) = value.get("devices") {
                for device in list {
                    if device["name"] == args[6] && device["online"] == true {
                        device_id = device["id"].as_str().unwrap_or("").to_string();
                        return true;
                    }
                }
            }
            false
        },
    );
    if !listed {
        println!("CORE_PROBE_FAIL the device was not listed online");
        return;
    }
    core.command(&json!({"connect": {"device": device_id}}).to_string());
    let started = Instant::now();
    let (mut frames, mut keyframes, mut audio, mut bytes) = (0u64, 0u64, 0u64, 0usize);
    let mut sent_input = false;
    while started.elapsed() < Duration::from_secs(seconds) {
        while let Some(event) = core.next_event(Duration::ZERO) {
            println!("event: {event}");
            if event.contains("\"failed\"") {
                println!("CORE_PROBE_FAIL the session failed");
                return;
            }
        }
        if let Some(frame) = core.next_frame(Duration::from_millis(20)) {
            frames += 1;
            bytes += frame.data.len();
            if keyframe(&frame.data) {
                keyframes += 1;
            }
        }
        while core.next_audio(Duration::ZERO).is_some() {
            audio += 1;
        }
        if frames > 10 && !sent_input {
            sent_input = true;
            for command in [
                json!({"move": {"x": 0.5, "y": 0.5}}),
                json!({"button": {"button": "left", "down": true}}),
                json!({"button": {"button": "left", "down": false}}),
                json!({"wheel": {"delta": -120}}),
                json!({"key": {"scancode": 0x10, "extended": false, "down": true}}),
                json!({"key": {"scancode": 0x10, "extended": false, "down": false}}),
            ] {
                core.command(&command.to_string());
            }
        }
    }
    core.command(&json!({"disconnect": {}}).to_string());
    events(
        &core,
        Instant::now() + Duration::from_secs(5),
        &mut |value| value.get("session").and_then(|s| s.get("state")) == Some(&json!("ended")),
    );
    println!(
        "{frames} frames ({:.2} MB), {keyframes} keyframes, {audio} sound packets",
        bytes as f64 / 1e6
    );
    if frames >= 30 && keyframes >= 1 {
        println!("CORE_PROBE_PASS frames={frames} keyframes={keyframes} audio={audio}");
    } else {
        println!("CORE_PROBE_FAIL frames={frames} keyframes={keyframes}");
    }
}

/// Whether an Annex B frame holds an IDR slice (H.264 type 5, H.265 types 16-21) or parameter sets.
fn keyframe(data: &[u8]) -> bool {
    data.windows(4).any(|w| {
        w[0] == 0 && w[1] == 0 && w[2] == 1 && {
            let h264 = w[3] & 0x1F;
            let h265 = (w[3] >> 1) & 0x3F;
            h264 == 5 || h264 == 7 || (16..=21).contains(&h265) || h265 == 32
        }
    })
}
