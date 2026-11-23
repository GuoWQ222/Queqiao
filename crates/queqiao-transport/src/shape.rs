//! Tests only (QUEQIAO_FORCE=shape:<kbit/s>/<queue ms>/<loss per mille>): what this side sends to the other side
//! passes a bottleneck like a slow uplink's: at most kbit/s, a queue of at most queue ms (a datagram that would wait
//! longer is dropped, as a full router queue drops it), and random loss. M4 measures the video's parity (FEC)
//! behind it: whether parity sent beside str0m's pacer and bandwidth estimate costs delay or bitrate.

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

pub struct Shaper {
    /// Bytes a second.
    rate: f64,
    queue_max: Duration,
    loss: f64,
    queue: VecDeque<(Instant, SocketAddr, SocketAddr, Vec<u8>)>,
    /// When the bottleneck is free again (the last datagram queued has gone out).
    free_at: Option<Instant>,
    random: u64,
    pub passed: u64,
    pub dropped_full: u64,
    pub dropped_lost: u64,
}

impl Shaper {
    /// From "<kbit/s>/<queue ms>/<loss per mille>" (the loss may be left out).
    pub fn from_spec(spec: &str) -> Option<Shaper> {
        let mut parts = spec.split('/');
        let kbit: f64 = parts.next()?.parse().ok()?;
        let queue_ms: u64 = parts.next()?.parse().ok()?;
        let loss: f64 = parts.next().map_or(Some(0.0), |p| p.parse().ok())?;
        (kbit > 0.0).then(|| Shaper {
            rate: kbit * 1000.0 / 8.0,
            queue_max: Duration::from_millis(queue_ms),
            loss: (loss / 1000.0).clamp(0.0, 1.0),
            queue: VecDeque::new(),
            free_at: None,
            random: 0x9E37_79B9_7F4A_7C15,
            passed: 0,
            dropped_full: 0,
            dropped_lost: 0,
        })
    }

    fn next_random(&mut self) -> f64 {
        self.random ^= self.random << 13;
        self.random ^= self.random >> 7;
        self.random ^= self.random << 17;
        (self.random >> 11) as f64 / (1u64 << 53) as f64
    }

    /// A datagram to send: queued to go out when the bottleneck has carried it, or dropped.
    pub fn offer(
        &mut self,
        now: Instant,
        source: SocketAddr,
        destination: SocketAddr,
        datagram: &[u8],
    ) {
        if self.next_random() < self.loss {
            self.dropped_lost += 1;
            return;
        }
        let start = self.free_at.map_or(now, |free| free.max(now));
        if start.saturating_duration_since(now) > self.queue_max {
            self.dropped_full += 1;
            return;
        }
        let out = start + Duration::from_secs_f64(datagram.len() as f64 / self.rate);
        self.free_at = Some(out);
        self.queue
            .push_back((out, source, destination, datagram.to_vec()));
    }

    /// The datagrams whose time has come.
    pub fn due(&mut self, now: Instant) -> Vec<(SocketAddr, SocketAddr, Vec<u8>)> {
        let mut out = Vec::new();
        while self.queue.front().is_some_and(|(at, ..)| *at <= now) {
            if let Some((_, source, destination, datagram)) = self.queue.pop_front() {
                self.passed += 1;
                out.push((source, destination, datagram));
            }
        }
        out
    }

    pub fn next_release(&self) -> Option<Instant> {
        self.queue.front().map(|(at, ..)| *at)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bottleneck_holds_its_rate_and_drops_what_waits_too_long() {
        // 8 Mbit/s = 1 MB/s; a queue of 100 ms = 100 kB.
        let mut shaper = Shaper::from_spec("8000/100/0").unwrap();
        let start = Instant::now();
        let address: SocketAddr = "127.0.0.1:1".parse().unwrap();
        for _ in 0..200 {
            shaper.offer(start, address, address, &[0u8; 1000]);
        }
        // About 100 kB fit in the queue (101 datagrams of 1 kB: the one that starts at 100 ms still goes).
        assert!(
            (100..=102).contains(&shaper.queue.len()),
            "{}",
            shaper.queue.len()
        );
        assert_eq!(shaper.dropped_full as usize + shaper.queue.len(), 200);
        // After 50 ms half of them are out.
        let out = shaper.due(start + Duration::from_millis(50));
        assert!((49..=51).contains(&out.len()), "{}", out.len());
    }

    #[test]
    fn random_loss_takes_about_its_share() {
        let mut shaper = Shaper::from_spec("1000000/1000/100").unwrap();
        let start = Instant::now();
        let address: SocketAddr = "127.0.0.1:1".parse().unwrap();
        for _ in 0..10_000 {
            shaper.offer(start, address, address, &[0u8; 10]);
        }
        assert!(
            (800..1200).contains(&shaper.dropped_lost),
            "{}",
            shaper.dropped_lost
        );
        assert!(Shaper::from_spec("x").is_none());
    }
}
