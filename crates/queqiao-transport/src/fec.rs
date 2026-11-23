//! Forward error correction for the video, as UU Remote has beside WebRTC's retransmissions (Reed-Solomon parity,
//! 5 % to 35 % by the loss measured).
//!
//! Measured over the PC -> sunshine path (2026-11-23): 77 of 37,000 video packets lost in 40 s; NACK repaired most,
//! but 5 frames still came incomplete, and the viewer showed them broken (blocks out of place, doubled text) until
//! a keyframe came. Parity repairs a loss as soon as the group's parity arrives, without a round trip.
//!
//! The parity is made from the video's SRTP datagrams as they leave (encrypted, the RTP header in clear), in groups
//! of up to [`MAX_GROUP`] consecutive packets of one stream, closed at the end of each frame (the marker bit). It
//! travels in datagrams of its own on the same path (first byte [`MAGIC`]: no STUN, DTLS, TURN channel, RTP or RTCP
//! starts so, and str0m never sees them). The receiver keeps the protected stream's recent datagrams; when some are
//! missing and enough parity came, it rebuilds them byte for byte and hands them to str0m as if they had arrived:
//! str0m decrypts and checks them as any other (a forged parity datagram can only make something that fails SRTP's
//! authentication). Parity of ciphertext tells nothing more than the ciphertext.
//!
//! The code: a systematic Reed-Solomon code over GF(256) with a Cauchy matrix (any K of the K + M shards give the
//! K data shards back). Each shard is a datagram with its length in front (2 bytes), padded to the group's longest.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

/// The first byte of a parity datagram (RFC 7983's demultiplexing leaves 192-255 unused).
pub const MAGIC: u8 = 0xFB;
const VERSION: u8 = 1;
/// MAGIC, VERSION, SSRC (4), first sequence number (2), K, M, the parity's index, shard size (2).
const HEADER: usize = 13;
/// Packets in a group at most (str0m's replay window is 128 packets: a rebuilt packet stays well inside it).
pub const MAX_GROUP: usize = 48;
/// How much parity: at least, at most (of the packets protected), and how fast it follows the loss.
pub const MIN_RATE: f64 = 0.05;
pub const MAX_RATE: f64 = 0.35;
/// The receiver keeps this many recent datagrams of the protected stream at most, for this long.
const KEEP_PACKETS: usize = 4096;
const KEEP_FOR: Duration = Duration::from_secs(1);
/// A group open this long on the sender is closed before the next packet joins it.
const GROUP_AGE: Duration = Duration::from_millis(100);

/// What the RTP header of a datagram says (version 2, not RTCP).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rtp {
    pub ssrc: u32,
    pub seq: u16,
    pub pt: u8,
    pub marker: bool,
}

/// The RTP header of `datagram`, if it is an RTP packet (not RTCP: RFC 5761, packet types 64-95 with the marker
/// bit are RTCP's 192-223).
pub fn rtp(datagram: &[u8]) -> Option<Rtp> {
    if datagram.len() < 12 || datagram[0] >> 6 != 2 {
        return None;
    }
    let pt = datagram[1] & 0x7F;
    if (64..96).contains(&pt) {
        return None;
    }
    Some(Rtp {
        ssrc: u32::from_be_bytes([datagram[8], datagram[9], datagram[10], datagram[11]]),
        seq: u16::from_be_bytes([datagram[2], datagram[3]]),
        pt,
        marker: datagram[1] & 0x80 != 0,
    })
}

/// How much parity for a loss (fraction of packets lost, the worst of the last seconds): MIN_RATE when nothing is
/// lost, four times the loss above it, at most MAX_RATE.
pub fn rate_for_loss(loss: f64) -> f64 {
    (MIN_RATE + 4.0 * loss).clamp(MIN_RATE, MAX_RATE)
}

// GF(256) with the polynomial x^8 + x^4 + x^3 + x^2 + 1 (0x11D), generator 2.
const fn tables() -> ([u8; 512], [u8; 256]) {
    let mut exp = [0u8; 512];
    let mut log = [0u8; 256];
    let mut x: u16 = 1;
    let mut i = 0;
    while i < 255 {
        exp[i] = x as u8;
        log[x as usize] = i as u8;
        x <<= 1;
        if x & 0x100 != 0 {
            x ^= 0x11D;
        }
        i += 1;
    }
    while i < 512 {
        exp[i] = exp[i - 255];
        i += 1;
    }
    (exp, log)
}
static EXP_LOG: ([u8; 512], [u8; 256]) = tables();

fn mul(a: u8, b: u8) -> u8 {
    if a == 0 || b == 0 {
        return 0;
    }
    let (exp, log) = &EXP_LOG;
    exp[usize::from(log[usize::from(a)]) + usize::from(log[usize::from(b)])]
}

fn inv(a: u8) -> u8 {
    debug_assert!(a != 0);
    let (exp, log) = &EXP_LOG;
    exp[255 - usize::from(log[usize::from(a)])]
}

/// dst ^= c * src, byte by byte.
fn mul_add(dst: &mut [u8], src: &[u8], c: u8) {
    if c == 0 {
        return;
    }
    let mut row = [0u8; 256];
    for (x, value) in row.iter_mut().enumerate() {
        *value = mul(c, x as u8);
    }
    for (d, s) in dst.iter_mut().zip(src) {
        *d ^= row[usize::from(*s)];
    }
}

/// The coefficient of data shard `j` in parity shard `i` of a group of `k` (Cauchy: 1 / (x_i + y_j), x_i = k + i,
/// y_j = j; every square part of it can be inverted, so any k shards give the data back).
fn coefficient(k: usize, i: usize, j: usize) -> u8 {
    inv(((k + i) as u8) ^ (j as u8))
}

/// `m` parity shards of the equally long data shards.
fn encode(data: &[Vec<u8>], m: usize) -> Vec<Vec<u8>> {
    let k = data.len();
    let size = data.first().map_or(0, Vec::len);
    (0..m)
        .map(|i| {
            let mut parity = vec![0u8; size];
            for (j, shard) in data.iter().enumerate() {
                mul_add(&mut parity, shard, coefficient(k, i, j));
            }
            parity
        })
        .collect()
}

/// Inverts the n x n matrix `a` (rows) in GF(256); None if it cannot be.
fn invert(mut a: Vec<Vec<u8>>) -> Option<Vec<Vec<u8>>> {
    let n = a.len();
    let mut b: Vec<Vec<u8>> = (0..n)
        .map(|r| (0..n).map(|c| u8::from(r == c)).collect())
        .collect();
    for col in 0..n {
        let pivot = (col..n).find(|&r| a[r][col] != 0)?;
        a.swap(col, pivot);
        b.swap(col, pivot);
        let scale = inv(a[col][col]);
        for c in 0..n {
            a[col][c] = mul(a[col][c], scale);
            b[col][c] = mul(b[col][c], scale);
        }
        for r in 0..n {
            if r != col && a[r][col] != 0 {
                let factor = a[r][col];
                for c in 0..n {
                    let (ac, bc) = (mul(factor, a[col][c]), mul(factor, b[col][c]));
                    a[r][c] ^= ac;
                    b[r][c] ^= bc;
                }
            }
        }
    }
    Some(b)
}

/// The missing data shards of a group of `k`: `data[j]` is shard j if it came, `parity` the parity shards that came
/// (index, shard), all `size` long. None if too few came.
fn recover(
    k: usize,
    data: &[Option<&[u8]>],
    parity: &[(usize, &[u8])],
    size: usize,
) -> Option<Vec<(usize, Vec<u8>)>> {
    let missing: Vec<usize> = (0..k).filter(|&j| data[j].is_none()).collect();
    if missing.is_empty() {
        return Some(Vec::new());
    }
    if parity.len() < missing.len() {
        return None;
    }
    let used = &parity[..missing.len()];
    // For each parity shard used: what is left of it once the data that came is taken out, and the coefficients
    // of the missing shards in it.
    let mut rest: Vec<Vec<u8>> = Vec::with_capacity(used.len());
    let mut matrix: Vec<Vec<u8>> = Vec::with_capacity(used.len());
    for (i, shard) in used {
        let mut left = shard.to_vec();
        for (j, came) in data.iter().enumerate() {
            if let Some(came) = came {
                mul_add(&mut left, came, coefficient(k, *i, j));
            }
        }
        rest.push(left);
        matrix.push(missing.iter().map(|&j| coefficient(k, *i, j)).collect());
    }
    let inverse = invert(matrix)?;
    Some(
        missing
            .iter()
            .enumerate()
            .map(|(row, &j)| {
                let mut shard = vec![0u8; size];
                for (col, left) in rest.iter().enumerate() {
                    mul_add(&mut shard, left, inverse[row][col]);
                }
                (j, shard)
            })
            .collect(),
    )
}

/// A datagram as a shard: its length (2 bytes), then itself, padded to `size`.
fn shard(datagram: &[u8], size: usize) -> Vec<u8> {
    let mut shard = Vec::with_capacity(size);
    shard.extend_from_slice(&(datagram.len() as u16).to_be_bytes());
    shard.extend_from_slice(datagram);
    shard.resize(size, 0);
    shard
}

/// The datagram in a shard (None if its length does not fit).
fn unshard(shard: &[u8]) -> Option<Vec<u8>> {
    let length = usize::from(u16::from_be_bytes([*shard.first()?, *shard.get(1)?]));
    shard.get(2..2 + length).map(<[u8]>::to_vec)
}

/// The sending side: groups the video's packets as they leave and makes their parity.
pub struct Sender {
    rate: f64,
    group: Vec<Vec<u8>>,
    ssrc: u32,
    first_seq: u16,
    opened: Option<Instant>,
    /// Parity datagrams and protected packets so far (the report).
    pub parity_sent: u64,
    pub protected: u64,
}

impl Default for Sender {
    fn default() -> Sender {
        Sender::new()
    }
}

impl Sender {
    pub fn new() -> Sender {
        Sender {
            rate: MIN_RATE,
            group: Vec::new(),
            ssrc: 0,
            first_seq: 0,
            opened: None,
            parity_sent: 0,
            protected: 0,
        }
    }

    pub fn set_rate(&mut self, rate: f64) {
        self.rate = rate.clamp(MIN_RATE, MAX_RATE);
    }

    pub fn rate(&self) -> f64 {
        self.rate
    }

    /// A video packet (SRTP datagram) as it leaves: the parity datagrams to send after it, once its group closes
    /// (at the end of a frame, or MAX_GROUP packets).
    pub fn sent(&mut self, datagram: &[u8], rtp: &Rtp, now: Instant) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        let next = self.first_seq.wrapping_add(self.group.len() as u16);
        let stale = self
            .opened
            .is_some_and(|opened| now.saturating_duration_since(opened) > GROUP_AGE);
        if !self.group.is_empty() && (rtp.ssrc != self.ssrc || rtp.seq != next || stale) {
            out.extend(self.close());
        }
        if self.group.is_empty() {
            self.ssrc = rtp.ssrc;
            self.first_seq = rtp.seq;
            self.opened = Some(now);
        }
        self.group.push(datagram.to_vec());
        if rtp.marker || self.group.len() >= MAX_GROUP {
            out.extend(self.close());
        }
        out
    }

    fn close(&mut self) -> Vec<Vec<u8>> {
        let group = std::mem::take(&mut self.group);
        self.opened = None;
        let k = group.len();
        let longest = group.iter().map(Vec::len).max().unwrap_or(0);
        if k == 0 || longest + 2 > usize::from(u16::MAX) {
            return Vec::new();
        }
        let m = ((k as f64 * self.rate).ceil() as usize).clamp(1, 255 - k);
        let size = longest + 2;
        let data: Vec<Vec<u8>> = group.iter().map(|datagram| shard(datagram, size)).collect();
        self.protected += k as u64;
        self.parity_sent += m as u64;
        encode(&data, m)
            .into_iter()
            .enumerate()
            .map(|(index, parity)| {
                let mut datagram = Vec::with_capacity(HEADER + size);
                datagram.push(MAGIC);
                datagram.push(VERSION);
                datagram.extend_from_slice(&self.ssrc.to_be_bytes());
                datagram.extend_from_slice(&self.first_seq.to_be_bytes());
                datagram.push(k as u8);
                datagram.push(m as u8);
                datagram.push(index as u8);
                datagram.extend_from_slice(&(size as u16).to_be_bytes());
                datagram.extend_from_slice(&parity);
                datagram
            })
            .collect()
    }
}

struct Group {
    k: usize,
    size: usize,
    parity: Vec<Option<Vec<u8>>>,
    at: Instant,
}

/// The receiving side: keeps the protected streams' recent packets and rebuilds the missing ones from parity.
#[derive(Default)]
pub struct Receiver {
    packets: HashMap<(u32, u16), Vec<u8>>,
    order: VecDeque<((u32, u16), Instant)>,
    groups: HashMap<(u32, u16), Group>,
    /// The highest sequence number seen per stream (a packet too far behind is not rebuilt: str0m would refuse it).
    newest: HashMap<u32, u16>,
    /// Parity datagrams that came, packets rebuilt, groups not completed from parity (the report; retransmission
    /// may have repaired them: it comes on a stream of its own, not seen here).
    pub parity_received: u64,
    pub rebuilt: u64,
    pub unrecoverable: u64,
}

impl Receiver {
    pub fn new() -> Receiver {
        Receiver::default()
    }

    /// An RTP datagram that came: kept for a while (from the first one: the session's first keyframe is
    /// protected too). Returns the datagrams it let be rebuilt.
    pub fn media(&mut self, datagram: &[u8], rtp: &Rtp, now: Instant) -> Vec<Vec<u8>> {
        self.keep((rtp.ssrc, rtp.seq), datagram.to_vec(), now);
        let newest = self.newest.entry(rtp.ssrc).or_insert(rtp.seq);
        if (rtp.seq.wrapping_sub(*newest) as i16) > 0 {
            *newest = rtp.seq;
        }
        let key = self
            .groups
            .iter()
            .find(|((ssrc, first), group)| {
                *ssrc == rtp.ssrc && usize::from(rtp.seq.wrapping_sub(*first)) < group.k
            })
            .map(|(key, _)| *key);
        match key {
            Some(key) => self.try_recover(key),
            None => Vec::new(),
        }
    }

    /// A parity datagram (first byte MAGIC). Returns the datagrams it let be rebuilt.
    pub fn parity(&mut self, datagram: &[u8], now: Instant) -> Vec<Vec<u8>> {
        self.expire(now);
        if datagram.len() < HEADER || datagram[0] != MAGIC || datagram[1] != VERSION {
            return Vec::new();
        }
        let ssrc = u32::from_be_bytes([datagram[2], datagram[3], datagram[4], datagram[5]]);
        let first = u16::from_be_bytes([datagram[6], datagram[7]]);
        let (k, m, index) = (
            usize::from(datagram[8]),
            usize::from(datagram[9]),
            usize::from(datagram[10]),
        );
        let size = usize::from(u16::from_be_bytes([datagram[11], datagram[12]]));
        if k == 0 || k > MAX_GROUP || m == 0 || k + m > 255 || index >= m || size < 3 {
            return Vec::new();
        }
        if datagram.len() != HEADER + size {
            return Vec::new();
        }
        self.parity_received += 1;
        let group = self.groups.entry((ssrc, first)).or_insert_with(|| Group {
            k,
            size,
            parity: vec![None; m],
            at: now,
        });
        // Parity that does not match its group's (another group with the same start, or forged): left out.
        if group.k != k || group.size != size || group.parity.len() != m {
            return Vec::new();
        }
        group.parity[index] = Some(datagram[HEADER..].to_vec());
        self.try_recover((ssrc, first))
    }

    fn keep(&mut self, key: (u32, u16), datagram: Vec<u8>, now: Instant) {
        if self.packets.insert(key, datagram).is_none() {
            self.order.push_back((key, now));
        }
        while self.order.len() > KEEP_PACKETS {
            if let Some((old, _)) = self.order.pop_front() {
                self.packets.remove(&old);
            }
        }
    }

    fn expire(&mut self, now: Instant) {
        while self
            .order
            .front()
            .is_some_and(|(_, at)| now.saturating_duration_since(*at) > KEEP_FOR)
        {
            if let Some((old, _)) = self.order.pop_front() {
                self.packets.remove(&old);
            }
        }
        let before = self.groups.len();
        self.groups
            .retain(|_, group| now.saturating_duration_since(group.at) <= KEEP_FOR);
        self.unrecoverable += (before - self.groups.len()) as u64;
    }

    /// Rebuilds what group `key` lacks if enough came; the group goes once whole.
    fn try_recover(&mut self, key: (u32, u16)) -> Vec<Vec<u8>> {
        let (ssrc, first) = key;
        let Some(group) = self.groups.get(&key) else {
            return Vec::new();
        };
        let size = group.size;
        let data: Vec<Option<&[u8]>> = (0..group.k)
            .map(|j| {
                self.packets
                    .get(&(ssrc, first.wrapping_add(j as u16)))
                    .filter(|datagram| datagram.len() + 2 <= size)
                    .map(Vec::as_slice)
            })
            .collect();
        let missing = data.iter().filter(|shard| shard.is_none()).count();
        if missing == 0 {
            self.groups.remove(&key);
            return Vec::new();
        }
        let parity: Vec<(usize, &[u8])> = group
            .parity
            .iter()
            .enumerate()
            .filter_map(|(index, shard)| shard.as_deref().map(|shard| (index, shard)))
            .collect();
        if parity.len() < missing {
            return Vec::new();
        }
        let shards: Vec<Vec<u8>> = data
            .iter()
            .map(|came| {
                came.map(|datagram| shard(datagram, size))
                    .unwrap_or_default()
            })
            .collect();
        let present: Vec<Option<&[u8]>> = data
            .iter()
            .zip(&shards)
            .map(|(came, shard)| came.map(|_| shard.as_slice()))
            .collect();
        let rebuilt = recover(group.k, &present, &parity, size);
        let newest = self.newest.get(&ssrc).copied().unwrap_or(first);
        self.groups.remove(&key);
        let Some(rebuilt) = rebuilt else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for (j, shard) in rebuilt {
            let seq = first.wrapping_add(j as u16);
            // Too far behind for str0m's replay window (128): it would refuse it.
            if newest.wrapping_sub(seq) as i16 >= 120 {
                continue;
            }
            if let Some(datagram) = unshard(&shard)
                && rtp(&datagram).is_some_and(|header| header.ssrc == ssrc && header.seq == seq)
            {
                self.rebuilt += 1;
                self.keep((ssrc, seq), datagram.clone(), Instant::now());
                out.push(datagram);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fake video packet (RTP header with ssrc, seq and marker; then `length` bytes of payload).
    fn packet(ssrc: u32, seq: u16, marker: bool, length: usize) -> Vec<u8> {
        let mut datagram = vec![0x80, 96 | if marker { 0x80 } else { 0 }];
        datagram.extend_from_slice(&seq.to_be_bytes());
        datagram.extend_from_slice(&12345u32.to_be_bytes());
        datagram.extend_from_slice(&ssrc.to_be_bytes());
        let mut x = u32::from(seq).wrapping_mul(2_654_435_761) ^ length as u32;
        for _ in 0..length {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            datagram.push(x as u8);
        }
        datagram
    }

    #[test]
    fn the_field_has_inverses() {
        for a in 1..=255u8 {
            assert_eq!(mul(a, inv(a)), 1, "{a}");
        }
        assert_eq!(mul(0, 7), 0);
        assert_eq!(mul(2, 0x80), 0x1D);
    }

    #[test]
    fn any_k_shards_give_the_data_back() {
        for (k, m) in [(1, 1), (5, 2), (30, 3), (48, 17)] {
            let data: Vec<Vec<u8>> = (0..k)
                .map(|j| shard(&packet(7, j as u16, false, 100 + j * 13), 1000))
                .collect();
            let parity = encode(&data, m);
            // Drop m data shards: the first ones, the last ones, every other one.
            for pattern in 0..3 {
                let lost: Vec<usize> = match pattern {
                    0 => (0..m.min(k)).collect(),
                    1 => (k.saturating_sub(m)..k).collect(),
                    _ => (0..k).step_by(2).take(m).collect(),
                };
                let present: Vec<Option<&[u8]>> = (0..k)
                    .map(|j| (!lost.contains(&j)).then(|| data[j].as_slice()))
                    .collect();
                let given: Vec<(usize, &[u8])> = parity
                    .iter()
                    .enumerate()
                    .map(|(i, p)| (i, p.as_slice()))
                    .collect();
                let rebuilt = recover(k, &present, &given, 1000).expect("recovered");
                assert_eq!(rebuilt.len(), lost.len());
                for (j, shard) in rebuilt {
                    assert_eq!(shard, data[j], "k {k} m {m} shard {j}");
                }
            }
        }
    }

    #[test]
    fn too_few_shards_rebuild_nothing() {
        let data: Vec<Vec<u8>> = (0..6)
            .map(|j| shard(&packet(7, j, false, 50), 80))
            .collect();
        let parity = encode(&data, 2);
        let present: Vec<Option<&[u8]>> = (0..6)
            .map(|j| (j > 2).then(|| data[j].as_slice()))
            .collect();
        let given: Vec<(usize, &[u8])> = parity
            .iter()
            .enumerate()
            .map(|(i, p)| (i, p.as_slice()))
            .collect();
        assert!(recover(6, &present, &given, 80).is_none());
    }

    #[test]
    fn lost_video_packets_come_back_byte_for_byte() {
        let start = Instant::now();
        let mut sender = Sender::new();
        sender.set_rate(0.2);
        let mut receiver = Receiver::new();
        // A first frame of one packet, lost: its parity alone brings it back.
        let first = packet(0xABCD, 65_519, true, 300);
        for datagram in sender.sent(&first, &rtp(&first).unwrap(), start) {
            receiver.parity(&datagram, start);
        }
        // Three frames: 20 packets, 3 packets, 30 packets (sequence numbers crossing 65535), each followed by its
        // parity as the sender sends it; some packets of each lost on the way.
        let lost: Vec<usize> = vec![0, 7, 19, 21, 30, 31, 52];
        let (mut index, mut seq) = (0usize, 65_520u16);
        let (mut rebuilt, mut expected) = (Vec::new(), Vec::new());
        let mut parity_count = 0;
        for frame in [20usize, 3, 30] {
            let mut parity = Vec::new();
            for i in 0..frame {
                let datagram = packet(0xABCD, seq, i + 1 == frame, 900 + (i * 37) % 300);
                parity.extend(sender.sent(&datagram, &rtp(&datagram).unwrap(), start));
                if lost.contains(&index) {
                    expected.push(datagram);
                } else {
                    rebuilt.extend(receiver.media(&datagram, &rtp(&datagram).unwrap(), start));
                }
                index += 1;
                seq = seq.wrapping_add(1);
            }
            parity_count += parity.len();
            for datagram in &parity {
                rebuilt.extend(receiver.parity(datagram, start));
            }
        }
        assert_eq!(sender.protected, 54);
        assert_eq!(parity_count, 4 + 1 + 6);
        rebuilt.sort();
        expected.sort();
        assert_eq!(rebuilt, expected);
        // And the first packet: it never came as media, its parity rebuilt it.
        assert_eq!(receiver.rebuilt, 7 + 1);
    }

    #[test]
    fn packets_before_their_parity_are_rebuilt_when_it_comes() {
        let start = Instant::now();
        let mut sender = Sender::new();
        sender.set_rate(0.1);
        let mut receiver = Receiver::new();
        let frame: Vec<Vec<u8>> = (2..12u16)
            .map(|seq| packet(9, seq, seq == 11, 500))
            .collect();
        let mut parity = Vec::new();
        for datagram in &frame {
            parity.extend(sender.sent(datagram, &rtp(datagram).unwrap(), start));
        }
        for (index, datagram) in frame.iter().enumerate() {
            if index != 4 {
                assert!(
                    receiver
                        .media(datagram, &rtp(datagram).unwrap(), start)
                        .is_empty()
                );
            }
        }
        let rebuilt: Vec<Vec<u8>> = parity
            .iter()
            .flat_map(|p| receiver.parity(p, start))
            .collect();
        assert_eq!(rebuilt, vec![frame[4].clone()]);
    }

    #[test]
    fn malformed_parity_is_left_out() {
        let start = Instant::now();
        let mut receiver = Receiver::new();
        for datagram in [
            vec![MAGIC],
            vec![MAGIC, VERSION, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 10],
            vec![MAGIC, VERSION, 0, 0, 0, 1, 0, 0, 200, 1, 0, 0, 3, 1, 2, 3],
            vec![MAGIC, VERSION, 0, 0, 0, 1, 0, 0, 2, 2, 5, 0, 3, 1, 2, 3],
        ] {
            assert!(receiver.parity(&datagram, start).is_empty());
        }
        assert_eq!(receiver.parity_received, 0);
    }

    #[test]
    fn rtcp_and_other_datagrams_are_not_rtp() {
        assert!(rtp(&packet(1, 1, false, 10)).is_some());
        // RTCP sender report: 0x80, 200.
        assert!(rtp(&[0x80, 200, 0, 6, 0, 0, 0, 1, 0, 0, 0, 0]).is_none());
        assert!(rtp(&[22, 3, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0]).is_none());
        assert!(rtp(&[MAGIC; 20]).is_none());
    }

    #[test]
    fn the_rate_follows_the_loss() {
        assert_eq!(rate_for_loss(0.0), MIN_RATE);
        assert!((rate_for_loss(0.02) - 0.13).abs() < 1e-9);
        assert_eq!(rate_for_loss(0.5), MAX_RATE);
    }
}
