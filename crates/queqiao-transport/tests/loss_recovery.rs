//! Video over a constrained link, in memory: the agent sends 30 frames a second, as the real agent does: one
//! keyframe at the start (8 frames' worth), frames sized to the bitrate after that, and every 3 s a screen change
//! of 3 frames' worth. Without the tuning the bitrate is a fixed 12 Mbit/s; with it, 85 % of the bandwidth
//! estimate (adaptive bitrate). The frames go towards a 20 Mbit/s bottleneck with a 50 ms queue (what overflows is dropped, as a Wi-Fi
//! uplink or a home router does), 0.5 % random loss on top (seeded: every run is the same) and 15 ms delay. Counts
//! the frames the viewer gets with gaps (each one costs a keyframe request) and how late frames arrive. Compares
//! str0m's defaults with the settings the network thread uses.

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use str0m::bwe::Bitrate;
use str0m::change::{SdpAnswer, SdpOffer};
use str0m::media::{Direction, MediaKind, MediaTime};
use str0m::net::{Protocol, Receive};
use str0m::{Candidate, Event, Input, Output, RtcConfig};

const DELAY: Duration = Duration::from_millis(15);
const LOSS_PER_MILLE: u64 = 5;
const LINK_BITS_PER_SECOND: f64 = 20e6;
const QUEUE: Duration = Duration::from_millis(50);

/// A small deterministic random generator (xorshift), so every run loses the same packets.
struct Random(u64);

impl Random {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

/// The bottleneck towards the viewer: packets leave one after another at the link rate; a packet that would
/// wait longer than the queue allows is dropped.
struct Bottleneck {
    free_at: Instant,
}

impl Bottleneck {
    /// When the packet arrives at the far end, or None if the queue drops it.
    fn send(&mut self, now: Instant, size: usize) -> Option<Instant> {
        let start = self.free_at.max(now);
        if start - now > QUEUE {
            return None;
        }
        let airtime = Duration::from_secs_f64(size as f64 * 8.0 / LINK_BITS_PER_SECOND);
        self.free_at = start + airtime;
        Some(self.free_at + DELAY)
    }
}

/// An Annex-B frame of `size` bytes with no start code inside the payload.
fn frame(size: usize, seed: u8, key: bool) -> Vec<u8> {
    let mut data = vec![0, 0, 0, 1, if key { 0x65 } else { 0x41 }];
    data.extend((0..size).map(|i| ((i as u8).wrapping_mul(31).wrapping_add(seed)) | 1));
    data
}

struct Outcome {
    frames: u32,
    broken: u32,
    late_ms: Vec<f64>,
    estimate: Option<u64>,
}

fn run(tuned: bool, configure: impl Fn(RtcConfig) -> RtcConfig) -> Outcome {
    str0m::crypto::from_feature_flags().install_process_default();
    let start = Instant::now();
    let build = |start| configure(RtcConfig::new().clear_codecs().enable_h264(true)).build(start);
    let viewer_addr: SocketAddr = "10.0.0.1:5000".parse().unwrap();
    let agent_addr: SocketAddr = "10.0.0.2:5000".parse().unwrap();
    let mut viewer = build(start);
    let mut agent = build(start);
    viewer.add_local_candidate(Candidate::host(viewer_addr, "udp").unwrap());
    agent.add_local_candidate(Candidate::host(agent_addr, "udp").unwrap());
    let mut api = viewer.sdp_api();
    api.add_media(MediaKind::Video, Direction::RecvOnly, None, None, None);
    let (offer, pending) = api.apply().unwrap();
    let answer = agent
        .sdp_api()
        .accept_offer(SdpOffer::from_sdp_string(&offer.to_sdp_string()).unwrap())
        .unwrap();
    viewer
        .sdp_api()
        .accept_answer(
            pending,
            SdpAnswer::from_sdp_string(&answer.to_sdp_string()).unwrap(),
        )
        .unwrap();

    let mut random = Random(0x9E37_79B9_7F4A_7C15);
    let mut bottleneck = Bottleneck { free_at: start };
    // (delivery time, to the viewer?, source, bytes)
    let mut link: Vec<(Instant, bool, SocketAddr, Vec<u8>)> = Vec::new();
    let mut written: VecDeque<(u64, Instant)> = VecDeque::new();
    let mut mid = None;
    let mut connected = (false, false);
    let mut outcome = Outcome {
        frames: 0,
        broken: 0,
        late_ms: Vec::new(),
        estimate: None,
    };
    let mut now = start;
    let mut next_frame = start;
    let mut sent = 0u64;
    let end = start + Duration::from_secs(10);
    // Each side's latest time: inputs never go back (deadlines inside the millisecond are served as their own
    // time, as the network thread does).
    let mut clocks = [start, start];
    while now < end {
        clocks = [clocks[0].max(now), clocks[1].max(now)];
        link.sort_by_key(|(at, ..)| *at);
        while link.first().is_some_and(|(at, ..)| *at <= now) {
            let (_, to_viewer, source, bytes) = link.remove(0);
            let (target, destination, clock) = if to_viewer {
                (&mut viewer, viewer_addr, clocks[0])
            } else {
                (&mut agent, agent_addr, clocks[1])
            };
            let input = Input::Receive(
                clock,
                Receive {
                    proto: Protocol::Udp,
                    source,
                    destination,
                    contents: bytes.as_slice().try_into().unwrap(),
                },
            );
            target.handle_input(input).unwrap();
        }
        for to_viewer in [false, true] {
            let (rtc, clock) = if to_viewer {
                (&mut agent, &mut clocks[1])
            } else {
                (&mut viewer, &mut clocks[0])
            };
            rtc.handle_input(Input::Timeout(*clock)).unwrap();
            let mut immediate = 0;
            loop {
                match rtc.poll_output().unwrap() {
                    // As the network thread does: a deadline inside this millisecond is served at once, as
                    // its own time (the pacer asks again a fraction of a millisecond after each packet).
                    Output::Timeout(at)
                        if at < now + Duration::from_millis(1) && immediate < 10_000 =>
                    {
                        immediate += 1;
                        *clock = (*clock).max(at);
                        rtc.handle_input(Input::Timeout(*clock)).unwrap();
                    }
                    Output::Timeout(_) => break,
                    Output::Transmit(t) => {
                        let source = t.source;
                        let bytes = t.contents.to_vec();
                        if to_viewer {
                            if random.next() % 1000 < LOSS_PER_MILLE {
                                continue;
                            }
                            if let Some(at) = bottleneck.send(now, bytes.len() + 28) {
                                link.push((at, true, source, bytes));
                            }
                        } else {
                            link.push((now + DELAY, false, source, bytes));
                        }
                    }
                    Output::Event(Event::Connected) => {
                        if to_viewer {
                            connected.1 = true;
                        } else {
                            connected.0 = true;
                        }
                    }
                    Output::Event(Event::MediaAdded(added)) => mid = Some(added.mid),
                    Output::Event(Event::EgressBitrateEstimate(estimate)) => {
                        if let str0m::bwe::BweKind::Twcc { estimate, .. }
                        | str0m::bwe::BweKind::Remb { estimate, .. } = estimate
                        {
                            outcome.estimate = Some(estimate.as_u64());
                        }
                    }
                    Output::Event(Event::MediaData(data)) => {
                        outcome.frames += 1;
                        outcome.broken += u32::from(!data.contiguous);
                        let index = data.time.numer() / 3000;
                        if let Some((_, at)) = written.iter().find(|(i, _)| *i == index) {
                            outcome.late_ms.push((now - *at).as_secs_f64() * 1e3);
                        }
                    }
                    Output::Event(_) => {}
                }
            }
        }
        if connected.0
            && connected.1
            && now >= next_frame
            && let Some(mid) = mid
            && let Some(writer) = agent.writer(mid)
        {
            let pt = writer.payload_params().next().map(|p| p.pt()).unwrap();
            let key = sent == 0;
            let bits_per_second = match (tuned, outcome.estimate) {
                (true, Some(estimate)) => estimate as f64 * 0.85,
                _ => 12e6,
            };
            let budget = (bits_per_second / 8.0 / 30.0) as usize;
            let size = if key {
                budget * 8
            } else if sent % 90 == 45 {
                budget * 3
            } else {
                budget
            };
            writer
                .write(
                    pt,
                    clocks[1],
                    MediaTime::from_90khz(sent * 3000),
                    frame(size, sent as u8, key),
                )
                .unwrap();
            written.push_back((sent, now));
            if written.len() > 120 {
                written.pop_front();
            }
            sent += 1;
            next_frame = now + Duration::from_millis(33);
        }
        now += Duration::from_millis(1);
    }
    outcome
}

fn summary(mut values: Vec<f64>) -> String {
    values.sort_by(f64::total_cmp);
    let at = |q: f64| {
        values
            .get(((values.len().max(1) - 1) as f64 * q) as usize)
            .copied()
            .unwrap_or(f64::NAN)
    };
    format!("median {:.0} ms, p95 {:.0} ms", at(0.5), at(0.95))
}

/// A label, whether the frames follow the bandwidth estimate, and the str0m settings.
type Variant = (&'static str, bool, fn(RtcConfig) -> RtcConfig);

#[test]
fn tuned_settings_survive_keyframe_bursts() {
    let variants: [Variant; 5] = [
        ("str0m defaults", false, |c| c),
        ("reordering 2048, send buffer 4096", false, |c| {
            c.set_reordering_size_video(2048)
                .set_send_buffer_video(4096)
        }),
        ("bandwidth estimation, fixed bitrate", false, |c| {
            c.enable_bwe(Some(Bitrate::mbps(12)))
        }),
        ("bandwidth estimation, adaptive bitrate", true, |c| {
            c.enable_bwe(Some(Bitrate::mbps(12)))
        }),
        ("queqiao_transport::tune, adaptive", true, |c| {
            queqiao_transport::tune(c, Bitrate::mbps(12))
        }),
    ];
    let mut results = Vec::new();
    for (label, adaptive, configure) in variants {
        let outcome = run(adaptive, configure);
        println!(
            "{label}: {} frames, {} with gaps; write -> received {}; estimate {:?}",
            outcome.frames,
            outcome.broken,
            summary(outcome.late_ms.clone()),
            outcome.estimate
        );
        results.push(outcome);
    }
    // Measured 2026-11-21: defaults 2 frames with gaps (unpaced bursts overflow the queue), median 38 ms;
    // tuned 0 gaps, median 34 ms, p95 81 ms.
    let defaults = &results[0];
    let tuned = results.last().unwrap();
    let mut late = tuned.late_ms.clone();
    late.sort_by(f64::total_cmp);
    let p95 = late[(late.len() - 1) * 95 / 100];
    assert!(tuned.frames > 280, "frames must flow");
    assert!(
        tuned.broken == 0 && defaults.broken > 0,
        "pacing must prevent the losses the bursts cause"
    );
    assert!(
        p95 < 150.0,
        "pacing with adaptive bitrate must not add a backlog (p95 {p95:.0} ms)"
    );
}
