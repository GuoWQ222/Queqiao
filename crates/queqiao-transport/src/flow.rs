//! How much bulk data (files, clipboard images) may be on its way in the SCTP association at once.
//!
//! str0m's SCTP (sctp-proto) sends the data of every channel from one queue, in the order it was written: a control
//! message (a key, a mouse move, a ping) written after a megabyte of file data waits until that megabyte is sent.
//! Measured on one PC (M4, two 21 MB files at once, 2026-11-23): with 1 MB in flight per stream the round trip
//! was 160-780 ms while they moved; with 128 kB it was 2-17 ms, and the files arrived 2.5 times sooner.
//!
//! So the streams keep about as much on its way as the path delivers in one round trip and a quarter (so that a
//! faster path is found within a few round trips), plus [`TARGET`]: enough to keep the path busy, and control
//! messages wait about [`TARGET`] (and a quarter of the round trip) behind it. The path's round trip is the
//! shortest STUN round trip seen (ICE's own checks: plain UDP, not behind the SCTP queue); how much it delivers is
//! measured from what the other side acknowledges ([`Flow::update`]). Never less than [`MIN_CAP`] (what the
//! measurement above used): where the shortest round trip is well below the usual one (Wi-Fi), the cap could
//! otherwise shrink below what the path holds and slow the files down to nothing.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// How long a control message may wait behind bulk data, about.
pub const TARGET: Duration = Duration::from_millis(25);
/// The round trip assumed until a STUN check has measured one (on the long side: too short would slow files).
const ASSUMED_RTT: Duration = Duration::from_millis(100);
/// Delivery is measured over windows this long; the fastest of the last [`RATE_MEMORY`] counts (a window in which
/// the streams had little to send says nothing about the path).
const WINDOW: Duration = Duration::from_millis(100);
const RATE_MEMORY: Duration = Duration::from_secs(2);
/// The least and the most all streams together may have on their way, and where they start.
pub const MIN_CAP: usize = 128 * 1024;
pub const MAX_CAP: usize = 2 * 1024 * 1024;
const START_CAP: usize = 256 * 1024;

pub struct Flow {
    /// The shortest round trip of the path seen so far.
    path_rtt: Option<Duration>,
    /// When the current window began, and how much had been acknowledged then.
    window: Option<(Instant, u64)>,
    /// Delivery rates (bytes a second) of the recent windows.
    rates: VecDeque<(Instant, f64)>,
    cap: usize,
}

impl Default for Flow {
    fn default() -> Flow {
        Flow::new()
    }
}

impl Flow {
    pub fn new() -> Flow {
        Flow {
            path_rtt: None,
            window: None,
            rates: VecDeque::new(),
            cap: START_CAP,
        }
    }

    /// A round trip of the path measured by STUN; true when it is the shortest yet (the one used).
    pub fn path_rtt(&mut self, rtt: Duration) -> bool {
        if self.path_rtt.is_some_and(|shortest| shortest <= rtt) {
            return false;
        }
        self.path_rtt = Some(rtt);
        true
    }

    #[cfg(test)]
    pub fn shortest_rtt(&self) -> Option<Duration> {
        self.path_rtt
    }

    /// `acked`: all bytes of the streams the other side has acknowledged so far (it only grows).
    pub fn update(&mut self, now: Instant, acked: u64) {
        let Some((began, acked_then)) = self.window else {
            self.window = Some((now, acked));
            return;
        };
        let length = now.saturating_duration_since(began);
        if length < WINDOW {
            return;
        }
        let rate = acked.saturating_sub(acked_then) as f64 / length.as_secs_f64();
        self.window = Some((now, acked));
        self.rates.push_back((now, rate));
        while self
            .rates
            .front()
            .is_some_and(|(at, _)| now.saturating_duration_since(*at) > RATE_MEMORY)
        {
            self.rates.pop_front();
        }
        let fastest = self.rates.iter().map(|(_, rate)| *rate).fold(0.0, f64::max);
        let rtt = self.path_rtt.unwrap_or(ASSUMED_RTT);
        let wanted = fastest * (rtt.as_secs_f64() * 1.25 + TARGET.as_secs_f64());
        self.cap = (wanted as usize).clamp(MIN_CAP, MAX_CAP);
    }

    /// How much all streams together may have on their way now.
    pub fn cap(&self) -> usize {
        self.cap
    }
}

/// How much more `mine` (bytes on its way on one stream) may grow, when `others` streams also have some on their way
/// and all together may have `cap`: each busy stream gets an equal share, at least [`SMALLEST_SHARE`] (files and
/// clipboard images go in 16 kB pieces), at most `most`.
pub fn room(cap: usize, mine: usize, others: usize, most: usize) -> usize {
    let share = (cap / (1 + others)).max(SMALLEST_SHARE).min(most);
    share.saturating_sub(mine)
}

const SMALLEST_SHARE: usize = 64 * 1024;

#[cfg(test)]
mod tests {
    use super::*;

    /// A path with a bottleneck of `rate` bytes a second and a round trip of `rtt` without queueing, a sender that
    /// always has data and keeps what `Flow` allows on its way: what the path delivers in the last 3 of `seconds`,
    /// and the longest a control message written in that time would have waited behind the bulk data.
    fn simulate(rate: f64, rtt: Duration, tell_rtt: bool, seconds: u64) -> (f64, Duration) {
        let start = Instant::now();
        let step = Duration::from_millis(1);
        let mut flow = Flow::new();
        if tell_rtt {
            flow.path_rtt(rtt);
        }
        let (mut written, mut acked) = (0u64, 0u64);
        // Bytes waiting at the bottleneck (SCTP's queue and the network's), and acknowledgements on their way back.
        let mut waiting = 0.0f64;
        let mut acks: VecDeque<(Duration, u64)> = VecDeque::new();
        let mut carry = 0.0f64;
        let mut worst = Duration::ZERO;
        let mut acked_at_measure = 0u64;
        let measure_from = Duration::from_secs(seconds - 3);
        let mut t = Duration::ZERO;
        while t < Duration::from_secs(seconds) {
            let now = start + t;
            while acks.front().is_some_and(|(at, _)| *at <= t) {
                acked += acks.pop_front().map(|(_, bytes)| bytes).unwrap_or(0);
            }
            flow.update(now, acked);
            let on_its_way = written - acked;
            let more = flow.cap().saturating_sub(on_its_way as usize) as u64;
            written += more;
            waiting += more as f64;
            // The bottleneck sends rate * step; what it sends is acknowledged a round trip later.
            carry += (rate * step.as_secs_f64()).min(waiting);
            let sent = carry.floor();
            carry -= sent;
            waiting -= sent;
            if sent > 0.0 {
                acks.push_back((t + rtt, sent as u64));
            }
            if t >= measure_from {
                if t == measure_from {
                    acked_at_measure = acked;
                }
                worst = worst.max(Duration::from_secs_f64(waiting / rate));
            }
            t += step;
        }
        let delivered = (acked - acked_at_measure) as f64 / 3.0;
        (delivered / rate, worst)
    }

    #[test]
    fn the_path_stays_busy_and_control_messages_wait_about_the_target() {
        // (bytes a second, round trip): a slow home uplink, a fast one, a LAN, a long path, a satellite.
        let paths = [
            (5e6 / 8.0, 30),
            (100e6 / 8.0, 30),
            (1e9 / 8.0, 1),
            (50e6 / 8.0, 150),
            (10e6 / 8.0, 600),
        ];
        for (rate, rtt) in paths {
            let rtt = Duration::from_millis(rtt);
            let (used, waited) = simulate(rate, rtt, true, 10);
            // The most there may be on its way limits a path that delivers more than MAX_CAP a round trip.
            let possible = (MAX_CAP as f64 / rtt.as_secs_f64() / rate).min(1.0);
            assert!(
                used >= 0.9 * possible,
                "{rate} B/s, {rtt:?}: the path carried {used:.2} of what it can"
            );
            // Waiting behind the bulk data: about TARGET and a quarter of the round trip (the smallest cap may
            // queue more on slow paths).
            let floor = Duration::from_secs_f64(MIN_CAP as f64 / rate).saturating_sub(rtt);
            let expected = TARGET + rtt / 4 + Duration::from_millis(10);
            assert!(
                waited <= expected.max(floor + Duration::from_millis(5)),
                "{rate} B/s, {rtt:?}: a control message waited up to {waited:?}"
            );
        }
    }

    #[test]
    fn without_a_measured_round_trip_it_still_fills_the_path() {
        let (used, waited) = simulate(20e6 / 8.0, Duration::from_millis(80), false, 10);
        assert!(used >= 0.9, "the path carried {used:.2} of what it can");
        assert!(
            waited <= Duration::from_millis(80),
            "waited up to {waited:?}"
        );
    }

    #[test]
    fn a_shortest_round_trip_below_the_usual_one_keeps_the_smallest_cap() {
        // Wi-Fi: one lucky 5 ms check, 60 ms as a rule. The cap shrinks, but not below MIN_CAP.
        let rate = 50e6 / 8.0;
        let start = Instant::now();
        let mut flow = Flow::new();
        flow.path_rtt(Duration::from_millis(5));
        let rtt = 0.060;
        for window in 0..100u32 {
            let now = start + Duration::from_millis(100) * window;
            // What a cap-limited sender delivers: the cap each round trip.
            let acked = (f64::from(window) * 0.1 * (flow.cap() as f64 / rtt).min(rate)) as u64;
            flow.update(now, acked);
        }
        assert!(flow.cap() >= MIN_CAP);
    }

    #[test]
    fn the_shortest_round_trip_is_kept() {
        let mut flow = Flow::new();
        assert!(flow.path_rtt(Duration::from_millis(30)));
        assert!(!flow.path_rtt(Duration::from_millis(80)));
        assert!(flow.path_rtt(Duration::from_millis(20)));
        assert_eq!(flow.shortest_rtt(), Some(Duration::from_millis(20)));
    }

    #[test]
    fn busy_streams_share_the_cap() {
        // Alone: all of it (up to the most one stream may have).
        assert_eq!(room(512 * 1024, 0, 0, 1 << 20), 512 * 1024);
        assert_eq!(room(4 << 20, 0, 0, 1 << 20), 1 << 20);
        // With another busy stream: half, less what is already on its way.
        assert_eq!(room(512 * 1024, 100 * 1024, 1, 1 << 20), 156 * 1024);
        // Never less than SMALLEST_SHARE.
        assert_eq!(room(MIN_CAP, 0, 2, 1 << 20), SMALLEST_SHARE);
        assert_eq!(room(MIN_CAP, SMALLEST_SHARE, 2, 1 << 20), 0);
    }
}
