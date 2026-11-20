//! STUN (RFC 8489) and TURN (RFC 8656) messages, shared by the TURN client in queqiao-transport and the
//! STUN/TURN server in queqiao-server. Only what Queqiao uses: Binding, Allocate, Refresh, CreatePermission,
//! ChannelBind, Send and Data, long-term credentials (MESSAGE-INTEGRITY with HMAC-SHA1), FINGERPRINT, and
//! ChannelData framing.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use hmac::{Hmac, Mac};
use md5::{Digest, Md5};
use sha1::Sha1;

pub const MAGIC: u32 = 0x2112_A442;
const HEADER: usize = 20;
const FINGERPRINT_XOR: u32 = 0x5354_554E;

pub mod method {
    pub const BINDING: u16 = 0x001;
    pub const ALLOCATE: u16 = 0x003;
    pub const REFRESH: u16 = 0x004;
    pub const SEND: u16 = 0x006;
    pub const DATA: u16 = 0x007;
    pub const CREATE_PERMISSION: u16 = 0x008;
    pub const CHANNEL_BIND: u16 = 0x009;
}

pub mod attr {
    pub const USERNAME: u16 = 0x0006;
    pub const MESSAGE_INTEGRITY: u16 = 0x0008;
    pub const ERROR_CODE: u16 = 0x0009;
    pub const CHANNEL_NUMBER: u16 = 0x000C;
    pub const LIFETIME: u16 = 0x000D;
    pub const XOR_PEER_ADDRESS: u16 = 0x0012;
    pub const DATA: u16 = 0x0013;
    pub const REALM: u16 = 0x0014;
    pub const NONCE: u16 = 0x0015;
    pub const XOR_RELAYED_ADDRESS: u16 = 0x0016;
    pub const REQUESTED_TRANSPORT: u16 = 0x0019;
    pub const XOR_MAPPED_ADDRESS: u16 = 0x0020;
    pub const SOFTWARE: u16 = 0x8022;
    pub const FINGERPRINT: u16 = 0x8028;
}

/// REQUESTED-TRANSPORT value for UDP (protocol 17).
pub const TRANSPORT_UDP: [u8; 4] = [17, 0, 0, 0];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    Request,
    Indication,
    Success,
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Message {
    pub class: Class,
    pub method: u16,
    pub transaction: [u8; 12],
    /// Attributes in order, without MESSAGE-INTEGRITY and FINGERPRINT (`encode` adds those).
    pub attributes: Vec<(u16, Vec<u8>)>,
    /// Whether the decoded message carried MESSAGE-INTEGRITY (check it with [`check_integrity`]).
    pub has_integrity: bool,
}

impl Message {
    pub fn new(class: Class, method: u16, transaction: [u8; 12]) -> Message {
        Message {
            class,
            method,
            transaction,
            attributes: Vec::new(),
            has_integrity: false,
        }
    }

    /// A response to `self` (same method and transaction).
    pub fn reply(&self, class: Class) -> Message {
        Message::new(class, self.method, self.transaction)
    }

    pub fn with(mut self, kind: u16, value: impl Into<Vec<u8>>) -> Message {
        self.attributes.push((kind, value.into()));
        self
    }

    pub fn with_address(self, kind: u16, address: SocketAddr) -> Message {
        let value = xor_address(address, &self.transaction);
        self.with(kind, value)
    }

    pub fn with_error(self, code: u16, reason: &str) -> Message {
        let mut value = vec![0, 0, (code / 100) as u8, (code % 100) as u8];
        value.extend_from_slice(reason.as_bytes());
        self.with(attr::ERROR_CODE, value)
    }

    pub fn get(&self, kind: u16) -> Option<&[u8]> {
        self.attributes
            .iter()
            .find(|(k, _)| *k == kind)
            .map(|(_, v)| v.as_slice())
    }

    pub fn text(&self, kind: u16) -> Option<String> {
        self.get(kind)
            .map(|v| String::from_utf8_lossy(v).into_owned())
    }

    pub fn address(&self, kind: u16) -> Option<SocketAddr> {
        unxor_address(self.get(kind)?, &self.transaction)
    }

    pub fn u32(&self, kind: u16) -> Option<u32> {
        Some(u32::from_be_bytes(
            self.get(kind)?.get(..4)?.try_into().ok()?,
        ))
    }

    pub fn error_code(&self) -> Option<u16> {
        let value = self.get(attr::ERROR_CODE)?;
        Some(u16::from(*value.get(2)? & 7) * 100 + u16::from(*value.get(3)?))
    }

    /// The message as bytes, with MESSAGE-INTEGRITY when `integrity_key` is given, and FINGERPRINT when
    /// asked.
    pub fn encode(&self, integrity_key: Option<&[u8]>, fingerprint: bool) -> Vec<u8> {
        let mut out = Vec::with_capacity(HEADER + 64);
        out.extend_from_slice(&message_type(self.class, self.method).to_be_bytes());
        out.extend_from_slice(&[0, 0]);
        out.extend_from_slice(&MAGIC.to_be_bytes());
        out.extend_from_slice(&self.transaction);
        for (kind, value) in &self.attributes {
            push_attribute(&mut out, *kind, value);
        }
        if let Some(key) = integrity_key {
            let length = out.len() - HEADER + 24;
            set_length(&mut out, length);
            let mac = hmac_sha1(key, &out);
            push_attribute(&mut out, attr::MESSAGE_INTEGRITY, &mac);
        }
        if fingerprint {
            let length = out.len() - HEADER + 8;
            set_length(&mut out, length);
            let crc = crc32fast::hash(&out) ^ FINGERPRINT_XOR;
            push_attribute(&mut out, attr::FINGERPRINT, &crc.to_be_bytes());
        }
        let length = out.len() - HEADER;
        set_length(&mut out, length);
        out
    }

    /// Parses a STUN message. `None` for anything else, or when its FINGERPRINT is wrong.
    pub fn decode(bytes: &[u8]) -> Option<Message> {
        if !is_stun(bytes) {
            return None;
        }
        let length = usize::from(u16::from_be_bytes([bytes[2], bytes[3]]));
        if HEADER + length != bytes.len() || !length.is_multiple_of(4) {
            return None;
        }
        let (class, method) = split_type(u16::from_be_bytes([bytes[0], bytes[1]]));
        let mut message = Message::new(class, method, bytes[8..20].try_into().ok()?);
        let mut at = HEADER;
        while at + 4 <= bytes.len() {
            let kind = u16::from_be_bytes([bytes[at], bytes[at + 1]]);
            let size = usize::from(u16::from_be_bytes([bytes[at + 2], bytes[at + 3]]));
            let value = bytes.get(at + 4..at + 4 + size)?;
            match kind {
                attr::MESSAGE_INTEGRITY => message.has_integrity = true,
                attr::FINGERPRINT => {
                    let expected = crc32fast::hash(&bytes[..at]) ^ FINGERPRINT_XOR;
                    if value != expected.to_be_bytes() {
                        return None;
                    }
                }
                // Only FINGERPRINT may follow MESSAGE-INTEGRITY.
                _ if message.has_integrity => {}
                _ => message.attributes.push((kind, value.to_vec())),
            }
            at += 4 + size.div_ceil(4) * 4;
        }
        Some(message)
    }
}

/// Checks the MESSAGE-INTEGRITY of a received message with `key` (constant time).
pub fn check_integrity(bytes: &[u8], key: &[u8]) -> bool {
    let mut at = HEADER;
    while at + 4 <= bytes.len() {
        let kind = u16::from_be_bytes([bytes[at], bytes[at + 1]]);
        let size = usize::from(u16::from_be_bytes([bytes[at + 2], bytes[at + 3]]));
        if kind == attr::MESSAGE_INTEGRITY {
            let Some(received) = bytes.get(at + 4..at + 24) else {
                return false;
            };
            let mut covered = bytes[..at].to_vec();
            set_length(&mut covered, at - HEADER + 24);
            let mut mac = Hmac::<Sha1>::new_from_slice(key).expect("HMAC takes any key length");
            mac.update(&covered);
            return mac.verify_slice(received).is_ok();
        }
        at += 4 + size.div_ceil(4) * 4;
    }
    false
}

/// A STUN message (rather than ChannelData, RTP, DTLS...): first two bits 0 and the magic cookie.
pub fn is_stun(bytes: &[u8]) -> bool {
    bytes.len() >= HEADER && bytes[0] & 0xC0 == 0 && bytes[4..8] == MAGIC.to_be_bytes()
}

/// ChannelData framing: channel number, length, payload (not padded: allowed over UDP).
pub fn channel_data(channel: u16, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + payload.len());
    out.extend_from_slice(&channel.to_be_bytes());
    out.extend_from_slice(&(payload.len() as u16).to_be_bytes());
    out.extend_from_slice(payload);
    out
}

/// The channel number and payload of a ChannelData message.
pub fn parse_channel_data(bytes: &[u8]) -> Option<(u16, &[u8])> {
    if bytes.len() < 4 || !(0x40..=0x7F).contains(&bytes[0]) {
        return None;
    }
    let channel = u16::from_be_bytes([bytes[0], bytes[1]]);
    let length = usize::from(u16::from_be_bytes([bytes[2], bytes[3]]));
    Some((channel, bytes.get(4..4 + length)?))
}

/// The long-term credential key: MD5("username:realm:password").
pub fn long_term_key(username: &str, realm: &str, password: &str) -> [u8; 16] {
    Md5::digest(format!("{username}:{realm}:{password}").as_bytes()).into()
}

pub fn hmac_sha1(key: &[u8], data: &[u8]) -> [u8; 20] {
    let mut mac = Hmac::<Sha1>::new_from_slice(key).expect("HMAC takes any key length");
    mac.update(data);
    mac.finalize().into_bytes().into()
}

fn message_type(class: Class, method: u16) -> u16 {
    let c: u16 = match class {
        Class::Request => 0,
        Class::Indication => 1,
        Class::Success => 2,
        Class::Error => 3,
    };
    (method & 0x000F)
        | ((method & 0x0070) << 1)
        | ((method & 0x0F80) << 2)
        | ((c & 1) << 4)
        | ((c & 2) << 7)
}

fn split_type(value: u16) -> (Class, u16) {
    let method = (value & 0x000F) | ((value >> 1) & 0x0070) | ((value >> 2) & 0x0F80);
    let class = match ((value >> 4) & 1) | ((value >> 7) & 2) {
        0 => Class::Request,
        1 => Class::Indication,
        2 => Class::Success,
        _ => Class::Error,
    };
    (class, method)
}

fn push_attribute(out: &mut Vec<u8>, kind: u16, value: &[u8]) {
    out.extend_from_slice(&kind.to_be_bytes());
    out.extend_from_slice(&(value.len() as u16).to_be_bytes());
    out.extend_from_slice(value);
    out.resize(out.len() + (4 - value.len() % 4) % 4, 0);
}

fn set_length(out: &mut [u8], length: usize) {
    out[2..4].copy_from_slice(&(length as u16).to_be_bytes());
}

fn xor_address(address: SocketAddr, transaction: &[u8; 12]) -> Vec<u8> {
    let port = address.port() ^ (MAGIC >> 16) as u16;
    let mut value = vec![0];
    match address.ip() {
        IpAddr::V4(ip) => {
            value.push(1);
            value.extend_from_slice(&port.to_be_bytes());
            value.extend_from_slice(&(u32::from(ip) ^ MAGIC).to_be_bytes());
        }
        IpAddr::V6(ip) => {
            value.push(2);
            value.extend_from_slice(&port.to_be_bytes());
            let mask = xor_mask_v6(transaction);
            value.extend(ip.octets().iter().zip(mask).map(|(a, b)| a ^ b));
        }
    }
    value
}

fn unxor_address(value: &[u8], transaction: &[u8; 12]) -> Option<SocketAddr> {
    let port = u16::from_be_bytes([*value.get(2)?, *value.get(3)?]) ^ (MAGIC >> 16) as u16;
    let ip = match value.get(1)? {
        1 => {
            let raw = u32::from_be_bytes(value.get(4..8)?.try_into().ok()?);
            IpAddr::V4(Ipv4Addr::from(raw ^ MAGIC))
        }
        2 => {
            let raw: [u8; 16] = value.get(4..20)?.try_into().ok()?;
            let mask = xor_mask_v6(transaction);
            let mut octets = [0u8; 16];
            for (i, octet) in octets.iter_mut().enumerate() {
                *octet = raw[i] ^ mask[i];
            }
            IpAddr::V6(Ipv6Addr::from(octets))
        }
        _ => return None,
    };
    Some(SocketAddr::new(ip, port))
}

fn xor_mask_v6(transaction: &[u8; 12]) -> [u8; 16] {
    let mut mask = [0u8; 16];
    mask[..4].copy_from_slice(&MAGIC.to_be_bytes());
    mask[4..].copy_from_slice(transaction);
    mask
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unhex(text: &str) -> Vec<u8> {
        let text: String = text.split_whitespace().collect();
        (0..text.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
            .collect()
    }

    const PASSWORD: &[u8] = b"VOkJxbRl1RmTxUk/WvJxBt";

    #[test]
    fn rfc5769_sample_request() {
        let bytes = unhex(
            "00010058 2112a442 b7e7a701 bc34d686 fa87dfae
             80220010 5354554e 20746573 7420636c 69656e74
             00240004 6e0001ff
             80290008 932ff9b1 51263b36
             00060009 6576746a 3a683676 59202020
             00080014 9aeaa70c bfd8cb56 781ef2b5 b2d3f249 c1b571a2
             80280004 e57a3bcf",
        );
        let message = Message::decode(&bytes).expect("valid, with a correct fingerprint");
        assert_eq!(
            (message.class, message.method),
            (Class::Request, method::BINDING)
        );
        assert_eq!(message.text(attr::USERNAME).as_deref(), Some("evtj:h6vY"));
        assert!(message.has_integrity);
        assert!(check_integrity(&bytes, PASSWORD));
        assert!(!check_integrity(&bytes, b"wrong password"));
        let mut corrupted = bytes.clone();
        corrupted[30] ^= 1;
        assert!(
            Message::decode(&corrupted).is_none(),
            "fingerprint must catch it"
        );
    }

    #[test]
    fn rfc5769_sample_ipv4_response() {
        let bytes = unhex(
            "0101003c 2112a442 b7e7a701 bc34d686 fa87dfae
             8022000b 74657374 20766563 746f7220
             00200008 0001a147 e112a643
             00080014 2b91f599 fd9e90c3 8c7489f9 2af9ba53 f06be7d7
             80280004 c07d4c96",
        );
        let message = Message::decode(&bytes).unwrap();
        assert_eq!(
            (message.class, message.method),
            (Class::Success, method::BINDING)
        );
        assert_eq!(
            message.address(attr::XOR_MAPPED_ADDRESS),
            Some("192.0.2.1:32853".parse().unwrap())
        );
        assert!(check_integrity(&bytes, PASSWORD));
    }

    #[test]
    fn own_messages_round_trip_with_long_term_key() {
        let key = long_term_key("user", "queqiao", "secret");
        let transaction = [7u8; 12];
        let v6: SocketAddr = "[2001:db8::1]:4000".parse().unwrap();
        let request = Message::new(Class::Request, method::CHANNEL_BIND, transaction)
            .with(attr::CHANNEL_NUMBER, [0x40, 0x01, 0, 0])
            .with_address(attr::XOR_PEER_ADDRESS, "203.0.113.9:5000".parse().unwrap())
            .with_address(attr::XOR_RELAYED_ADDRESS, v6)
            .with(attr::USERNAME, "user");
        let bytes = request.encode(Some(&key), true);
        let decoded = Message::decode(&bytes).unwrap();
        assert_eq!(decoded.attributes, request.attributes);
        assert_eq!(decoded.address(attr::XOR_RELAYED_ADDRESS), Some(v6));
        assert!(check_integrity(&bytes, &key));
        let error = request.reply(Class::Error).with_error(438, "Stale Nonce");
        assert_eq!(
            Message::decode(&error.encode(None, false))
                .unwrap()
                .error_code(),
            Some(438)
        );
        let framed = channel_data(0x4001, b"payload");
        assert_eq!(parse_channel_data(&framed), Some((0x4001, &b"payload"[..])));
        assert!(!is_stun(&framed));
    }
}

/// Robustness: STUN and ChannelData come from anyone on the network. Random messages with well-formed and
/// broken attributes must never panic the decoder, the integrity check or the attribute readers.
#[cfg(test)]
mod robustness {
    use super::*;

    struct Random(u64);

    impl Random {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn below(&mut self, n: usize) -> usize {
            (self.next() % n.max(1) as u64) as usize
        }
        fn bytes(&mut self, n: usize) -> Vec<u8> {
            (0..n).map(|_| self.next() as u8).collect()
        }
    }

    /// A STUN-looking message: right header and cookie, random attributes (known kinds and others), and
    /// sometimes a length that does not fit.
    fn random_message(random: &mut Random) -> Vec<u8> {
        let kinds = [
            attr::USERNAME,
            attr::MESSAGE_INTEGRITY,
            attr::ERROR_CODE,
            attr::CHANNEL_NUMBER,
            attr::LIFETIME,
            attr::XOR_PEER_ADDRESS,
            attr::DATA,
            attr::REALM,
            attr::NONCE,
            attr::XOR_RELAYED_ADDRESS,
            attr::REQUESTED_TRANSPORT,
            attr::XOR_MAPPED_ADDRESS,
            attr::SOFTWARE,
            attr::FINGERPRINT,
        ];
        let mut attributes = Vec::new();
        for _ in 0..random.below(6) {
            let kind = if random.below(4) == 0 {
                random.next() as u16
            } else {
                kinds[random.below(kinds.len())]
            };
            let length = random.below(40);
            attributes.extend_from_slice(&kind.to_be_bytes());
            let claimed = if random.below(8) == 0 {
                random.next() as u16
            } else {
                length as u16
            };
            attributes.extend_from_slice(&claimed.to_be_bytes());
            attributes.extend(random.bytes(length));
            attributes.extend(vec![0u8; (4 - length % 4) % 4]);
        }
        let mut out = Vec::new();
        out.extend_from_slice(&((random.next() as u16) & 0x3FFF).to_be_bytes());
        let length = if random.below(8) == 0 {
            random.next() as u16
        } else {
            attributes.len() as u16
        };
        out.extend_from_slice(&length.to_be_bytes());
        out.extend_from_slice(&0x2112_A442u32.to_be_bytes());
        out.extend(random.bytes(12));
        out.extend(attributes);
        out
    }

    #[test]
    fn random_messages_never_panic() {
        let mut random = Random(0xD1B5_4A32_D192_ED03);
        for round in 0..200_000 {
            let bytes = if round % 4 == 0 {
                let length = random.below(64);
                random.bytes(length)
            } else {
                random_message(&mut random)
            };
            let _ = check_integrity(&bytes, b"key");
            let _ = parse_channel_data(&bytes);
            if let Some(message) = Message::decode(&bytes) {
                for kind in [
                    attr::USERNAME,
                    attr::XOR_PEER_ADDRESS,
                    attr::XOR_MAPPED_ADDRESS,
                    attr::XOR_RELAYED_ADDRESS,
                    attr::LIFETIME,
                    attr::CHANNEL_NUMBER,
                    attr::REALM,
                ] {
                    let _ = (
                        message.get(kind),
                        message.text(kind),
                        message.address(kind),
                        message.u32(kind),
                    );
                }
                let _ = message.error_code();
                let _ = message.encode(Some(b"key"), true);
            }
        }
    }
}
