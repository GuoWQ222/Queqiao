//! What goes over the local channels. The desktop helper runs as SYSTEM, so what it reads is checked as strictly
//! as anything from the network: fixed sizes, a frame's pixels exactly width x height x 4, short texts.

use queqiao_core::protocol::{InputEvent, Message};

/// The longest message: a frame of 8192 x 4800 (about 157 MB), with room for its header.
pub const MAX_MESSAGE: usize = 8192 * 4800 * 4 + 64;
/// The longest text (a desktop's name, an error, a reply), in bytes.
pub const MAX_TEXT: usize = 1024;
/// The largest frame side.
const MAX_SIDE: u32 = 16384;

/// From the agent to the desktop helper.
#[derive(Clone, Debug, PartialEq)]
pub enum ToHelper {
    /// Capture this screen (numbered as `queqiao_capture::screens` counts them) of whichever desktop takes input
    /// now, and send its pictures as it changes.
    Capture {
        screen: u8,
    },
    StopCapture,
    /// Inject this on the desktop that takes input, mapped to this screen area (desktop coordinates). The helper
    /// does so only while that desktop is not the signed-in person's own: there the agent injects itself.
    Input {
        event: InputEvent,
        left: i32,
        top: i32,
        width: u32,
        height: u32,
    },
    /// A remote session started (true) or ended (false): input is taken only during one.
    Session(bool),
    /// The session banner for this viewer (None: none): the helper shows it, with End, whenever the desktop that
    /// takes input is not the person's own (the person's own banner is on their desktop, out of sight there).
    Banner(Option<String>),
}

/// From the desktop helper to the agent.
#[derive(Clone, Debug, PartialEq)]
pub enum FromHelper {
    /// The desktop that takes input now: "Default" (the person's own), "Winlogon" (sign-in, lock, Ctrl+Alt+Del
    /// and UAC screens), "Screen-saver".
    Desktop(String),
    /// A picture of the captured screen: rows of width x 4 bytes, B, G, R, A.
    Frame {
        width: u32,
        height: u32,
        pixels: Vec<u8>,
    },
    /// Capture failed (it is tried again).
    Failed(String),
    /// The person at the PC pressed End on the helper's banner.
    EndPressed,
    /// The captured screen's place on the desktop (sent as each capture starts, before its first picture).
    Screen {
        left: i32,
        top: i32,
        width: u32,
        height: u32,
    },
}

/// From the agent to the service.
#[derive(Clone, Debug, PartialEq)]
pub enum ToService {
    /// Ctrl+Alt+Del on the PC's screen (SendSAS: only a service may).
    SendSas,
    /// End this agent and its helper and start both again.
    Restart,
    /// What the agent for the sign-in screen needs to be this device (the person allows control there).
    LogonIdentity(LogonIdentity),
    /// The person does not allow control at the sign-in screen (any more): the service forgets the identity.
    ForgetLogon,
}

/// From the service to the agent.
#[derive(Clone, Debug, PartialEq)]
pub enum FromService {
    /// The outcome of a request, for the viewer.
    Reply(String),
    /// To the agent for the sign-in screen, right after it starts: who to be.
    Identity(LogonIdentity),
}

/// This device as the agent for the sign-in screen is to be: its id, name and key, the account login, what the
/// person allows, and the keys of the account's devices as the person's agent pinned them. Kept by the service
/// (DPAPI for SYSTEM, a file only SYSTEM may read) and handed to that agent in memory only.
#[derive(Clone, PartialEq, Eq)]
pub struct LogonIdentity {
    pub device_id: String,
    pub device_name: String,
    /// The device's private key (PKCS#8).
    pub key_pkcs8: Vec<u8>,
    pub server: String,
    pub pin: String,
    pub refresh_token: String,
    /// The account's own devices may control this PC without asking (the person's setting).
    pub allow_account_control: bool,
    /// Device id -> public key (hex).
    pub pins: Vec<(String, String)>,
}

// Never printed: it holds the key and the login.
impl std::fmt::Debug for LogonIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "LogonIdentity({} {})", self.device_id, self.device_name)
    }
}

/// The most pins a logon identity carries.
const MAX_PINS: usize = 1000;

impl LogonIdentity {
    fn encode_into(&self, out: &mut Vec<u8>) {
        for value in [&self.device_id, &self.device_name] {
            text(out, value);
        }
        out.extend_from_slice(&(self.key_pkcs8.len().min(MAX_TEXT) as u16).to_le_bytes());
        out.extend_from_slice(&self.key_pkcs8[..self.key_pkcs8.len().min(MAX_TEXT)]);
        for value in [&self.server, &self.pin, &self.refresh_token] {
            text(out, value);
        }
        out.push(u8::from(self.allow_account_control));
        let count = self.pins.len().min(MAX_PINS);
        out.extend_from_slice(&(count as u16).to_le_bytes());
        for (device, key) in &self.pins[..count] {
            text(out, device);
            text(out, key);
        }
    }

    fn decode_from(bytes: &[u8]) -> Option<LogonIdentity> {
        let mut reader = Fields(bytes);
        let device_id = reader.text()?;
        let device_name = reader.text()?;
        let key_pkcs8 = reader.bytes()?;
        let server = reader.text()?;
        let pin = reader.text()?;
        let refresh_token = reader.text()?;
        let allow_account_control = match reader.take(1)? {
            [0] => false,
            [1] => true,
            _ => return None,
        };
        let count = usize::from(u16::from_le_bytes(reader.take(2)?.try_into().ok()?));
        if count > MAX_PINS {
            return None;
        }
        let mut pins = Vec::with_capacity(count);
        for _ in 0..count {
            pins.push((reader.text()?, reader.text()?));
        }
        reader.0.is_empty().then_some(LogonIdentity {
            device_id,
            device_name,
            key_pkcs8,
            server,
            pin,
            refresh_token,
            allow_account_control,
            pins,
        })
    }
}

/// Length-prefixed fields one after another.
struct Fields<'a>(&'a [u8]);

impl<'a> Fields<'a> {
    fn take(&mut self, count: usize) -> Option<&'a [u8]> {
        let (head, rest) = (self.0.get(..count)?, self.0.get(count..)?);
        self.0 = rest;
        Some(head)
    }

    fn bytes(&mut self) -> Option<Vec<u8>> {
        let length = usize::from(u16::from_le_bytes(self.take(2)?.try_into().ok()?));
        if length > MAX_TEXT {
            return None;
        }
        Some(self.take(length)?.to_vec())
    }

    fn text(&mut self) -> Option<String> {
        String::from_utf8(self.bytes()?).ok()
    }
}

fn text(out: &mut Vec<u8>, value: &str) {
    let mut end = value.len().min(MAX_TEXT);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    out.extend_from_slice(&(end as u16).to_le_bytes());
    out.extend_from_slice(&value.as_bytes()[..end]);
}

/// A text field that is all of the rest.
fn read_text(rest: &[u8]) -> Option<String> {
    let length = usize::from(u16::from_le_bytes(rest.get(..2)?.try_into().ok()?));
    if length > MAX_TEXT || rest.len() != 2 + length {
        return None;
    }
    String::from_utf8(rest[2..].to_vec()).ok()
}

fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

fn i32_at(bytes: &[u8], at: usize) -> Option<i32> {
    Some(i32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

impl ToHelper {
    pub fn encode(&self) -> Vec<u8> {
        match self {
            ToHelper::Capture { screen } => vec![1, *screen],
            ToHelper::StopCapture => vec![2],
            ToHelper::Input {
                event,
                left,
                top,
                width,
                height,
            } => {
                let mut out = vec![3];
                out.extend_from_slice(&left.to_le_bytes());
                out.extend_from_slice(&top.to_le_bytes());
                out.extend_from_slice(&width.to_le_bytes());
                out.extend_from_slice(&height.to_le_bytes());
                out.extend(Message::Input(*event).encode());
                out
            }
            ToHelper::Session(on) => vec![4, u8::from(*on)],
            ToHelper::Banner(None) => vec![5, 0],
            ToHelper::Banner(Some(viewer)) => {
                let mut out = vec![5, 1];
                text(&mut out, viewer);
                out
            }
        }
    }

    pub fn decode(bytes: &[u8]) -> Option<ToHelper> {
        let (&kind, rest) = bytes.split_first()?;
        match kind {
            1 if rest.len() == 1 => Some(ToHelper::Capture { screen: rest[0] }),
            2 if rest.is_empty() => Some(ToHelper::StopCapture),
            3 => {
                let (left, top) = (i32_at(rest, 0)?, i32_at(rest, 4)?);
                let (width, height) = (u32_at(rest, 8)?, u32_at(rest, 12)?);
                if width == 0 || height == 0 || width > MAX_SIDE || height > MAX_SIDE {
                    return None;
                }
                match Message::decode(rest.get(16..)?)? {
                    Message::Input(event) => Some(ToHelper::Input {
                        event,
                        left,
                        top,
                        width,
                        height,
                    }),
                    _ => None,
                }
            }
            4 if rest.len() == 1 && rest[0] <= 1 => Some(ToHelper::Session(rest[0] == 1)),
            5 => match rest.split_first()? {
                (0, []) => Some(ToHelper::Banner(None)),
                (1, viewer) => read_text(viewer).map(|viewer| ToHelper::Banner(Some(viewer))),
                _ => None,
            },
            _ => None,
        }
    }
}

impl FromHelper {
    pub fn encode(&self) -> Vec<u8> {
        match self {
            FromHelper::Desktop(name) => {
                let mut out = vec![1];
                text(&mut out, name);
                out
            }
            FromHelper::Frame {
                width,
                height,
                pixels,
            } => {
                let mut out = Vec::with_capacity(9 + pixels.len());
                out.push(2);
                out.extend_from_slice(&width.to_le_bytes());
                out.extend_from_slice(&height.to_le_bytes());
                out.extend_from_slice(pixels);
                out
            }
            FromHelper::Failed(error) => {
                let mut out = vec![3];
                text(&mut out, error);
                out
            }
            FromHelper::EndPressed => vec![4],
            FromHelper::Screen {
                left,
                top,
                width,
                height,
            } => {
                let mut out = vec![5];
                out.extend_from_slice(&left.to_le_bytes());
                out.extend_from_slice(&top.to_le_bytes());
                out.extend_from_slice(&width.to_le_bytes());
                out.extend_from_slice(&height.to_le_bytes());
                out
            }
        }
    }

    pub fn decode(bytes: &[u8]) -> Option<FromHelper> {
        let (&kind, rest) = bytes.split_first()?;
        match kind {
            1 => read_text(rest).map(FromHelper::Desktop),
            2 => {
                let (width, height) = (u32_at(rest, 0)?, u32_at(rest, 4)?);
                if width == 0 || height == 0 || width > MAX_SIDE || height > MAX_SIDE {
                    return None;
                }
                let size = (width as usize)
                    .checked_mul(height as usize)?
                    .checked_mul(4)?;
                let pixels = rest.get(8..)?;
                (pixels.len() == size).then(|| FromHelper::Frame {
                    width,
                    height,
                    pixels: pixels.to_vec(),
                })
            }
            3 => read_text(rest).map(FromHelper::Failed),
            4 if rest.is_empty() => Some(FromHelper::EndPressed),
            5 if rest.len() == 16 => {
                let (left, top) = (i32_at(rest, 0)?, i32_at(rest, 4)?);
                let (width, height) = (u32_at(rest, 8)?, u32_at(rest, 12)?);
                if width == 0 || height == 0 || width > MAX_SIDE || height > MAX_SIDE {
                    return None;
                }
                Some(FromHelper::Screen {
                    left,
                    top,
                    width,
                    height,
                })
            }
            _ => None,
        }
    }
}

impl ToService {
    pub fn encode(&self) -> Vec<u8> {
        match self {
            ToService::SendSas => vec![1],
            ToService::Restart => vec![2],
            ToService::LogonIdentity(identity) => {
                let mut out = vec![3];
                identity.encode_into(&mut out);
                out
            }
            ToService::ForgetLogon => vec![4],
        }
    }

    pub fn decode(bytes: &[u8]) -> Option<ToService> {
        match bytes {
            [1] => Some(ToService::SendSas),
            [2] => Some(ToService::Restart),
            [3, rest @ ..] => LogonIdentity::decode_from(rest).map(ToService::LogonIdentity),
            [4] => Some(ToService::ForgetLogon),
            _ => None,
        }
    }
}

impl FromService {
    pub fn encode(&self) -> Vec<u8> {
        match self {
            FromService::Reply(reply) => {
                let mut out = vec![1];
                text(&mut out, reply);
                out
            }
            FromService::Identity(identity) => {
                let mut out = vec![2];
                identity.encode_into(&mut out);
                out
            }
        }
    }

    pub fn decode(bytes: &[u8]) -> Option<FromService> {
        let (&kind, rest) = bytes.split_first()?;
        match kind {
            1 => read_text(rest).map(FromService::Reply),
            2 => LogonIdentity::decode_from(rest).map(FromService::Identity),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use queqiao_core::protocol::MouseButton;

    fn samples() -> (Vec<ToHelper>, Vec<FromHelper>) {
        let to = vec![
            ToHelper::Capture { screen: 2 },
            ToHelper::StopCapture,
            ToHelper::Input {
                event: InputEvent::MouseMove { x: 100, y: 65535 },
                left: -1920,
                top: 0,
                width: 1920,
                height: 1080,
            },
            ToHelper::Input {
                event: InputEvent::Key {
                    scancode: 0x01,
                    extended: false,
                    down: true,
                },
                left: 0,
                top: 0,
                width: 3840,
                height: 2160,
            },
            ToHelper::Input {
                event: InputEvent::MouseButton {
                    button: MouseButton::Left,
                    down: false,
                },
                left: 0,
                top: 0,
                width: 800,
                height: 600,
            },
            ToHelper::Session(true),
            ToHelper::Session(false),
            ToHelper::Banner(Some("DESKTOP-1 (alice)".into())),
            ToHelper::Banner(None),
        ];
        let from = vec![
            FromHelper::Desktop("Winlogon".into()),
            FromHelper::Desktop("Default".into()),
            FromHelper::Frame {
                width: 3,
                height: 2,
                pixels: (0..24).collect(),
            },
            FromHelper::Failed("DuplicateOutput: access denied".into()),
            FromHelper::EndPressed,
            FromHelper::Screen {
                left: -2560,
                top: 0,
                width: 2560,
                height: 1440,
            },
        ];
        (to, from)
    }

    fn identity() -> LogonIdentity {
        LogonIdentity {
            device_id: "0123456789abcdef".into(),
            device_name: "鹊桥-PC".into(),
            key_pkcs8: (0..83).collect(),
            server: "203.0.113.5:47100".into(),
            pin: "ab".repeat(32),
            refresh_token: "cd".repeat(32),
            allow_account_control: true,
            pins: vec![
                ("dev1".into(), "11".repeat(32)),
                ("dev2".into(), "22".repeat(32)),
            ],
        }
    }

    #[test]
    fn identity_round_trip_and_refusals() {
        let message = ToService::LogonIdentity(identity());
        let bytes = message.encode();
        assert_eq!(ToService::decode(&bytes), Some(message));
        let handed = FromService::Identity(identity());
        assert_eq!(FromService::decode(&handed.encode()), Some(handed));
        // Cut anywhere, or with anything after it: refused.
        for cut in 1..bytes.len() {
            assert_eq!(ToService::decode(&bytes[..cut]), None, "cut at {cut}");
        }
        let mut longer = bytes.clone();
        longer.push(0);
        assert_eq!(ToService::decode(&longer), None);
        // The key never shows in a debug print.
        assert!(!format!("{:?}", identity()).contains("cdcd"));
    }

    #[test]
    fn round_trip() {
        let (to, from) = samples();
        for message in to {
            assert_eq!(ToHelper::decode(&message.encode()), Some(message));
        }
        for message in from {
            assert_eq!(FromHelper::decode(&message.encode()), Some(message));
        }
        for message in [
            ToService::SendSas,
            ToService::Restart,
            ToService::ForgetLogon,
        ] {
            assert_eq!(ToService::decode(&message.encode()), Some(message));
        }
        let reply = FromService::Reply("Ctrl+Alt+Del sent".into());
        assert_eq!(FromService::decode(&reply.encode()), Some(reply));
    }

    #[test]
    fn wrong_sizes_are_refused() {
        // Pixels one byte short or long, a zero or huge side.
        let mut frame = FromHelper::Frame {
            width: 3,
            height: 2,
            pixels: vec![0; 24],
        }
        .encode();
        frame.pop();
        assert_eq!(FromHelper::decode(&frame), None);
        frame.extend([0, 0]);
        assert_eq!(FromHelper::decode(&frame), None);
        let mut huge = vec![2];
        huge.extend(u32::MAX.to_le_bytes());
        huge.extend(u32::MAX.to_le_bytes());
        assert_eq!(FromHelper::decode(&huge), None);
        let mut zero = vec![2];
        zero.extend(0u32.to_le_bytes());
        zero.extend(5u32.to_le_bytes());
        assert_eq!(FromHelper::decode(&zero), None);
        // A long text is cut on a character boundary, and a text longer than allowed is refused.
        let long = "鹊".repeat(600);
        match FromHelper::decode(&FromHelper::Failed(long).encode()) {
            Some(FromHelper::Failed(text)) => {
                assert!(text.len() <= MAX_TEXT && text.len() % 3 == 0)
            }
            other => panic!("{other:?}"),
        }
        let mut over = vec![1];
        over.extend(((MAX_TEXT + 1) as u16).to_le_bytes());
        over.extend(vec![b'a'; MAX_TEXT + 1]);
        assert_eq!(FromHelper::decode(&over), None);
        // Input with an empty screen area, or a message other than input inside.
        let mut empty = vec![3];
        empty.extend([0u8; 16]);
        empty.extend(Message::Input(InputEvent::MouseMove { x: 1, y: 1 }).encode());
        assert_eq!(ToHelper::decode(&empty), None);
        let mut other = vec![3, 0, 0, 0, 0, 0, 0, 0, 0];
        other.extend(100u32.to_le_bytes());
        other.extend(100u32.to_le_bytes());
        other.extend(Message::Leave.encode());
        assert_eq!(ToHelper::decode(&other), None);
        assert_eq!(ToHelper::decode(&[4, 2]), None);
    }

    /// xorshift64: a fixed seed, so a failure repeats.
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
    }

    /// Random and mutated inputs never panic, and what is accepted encodes back to the same bytes.
    #[test]
    fn robustness() {
        let mut random = Random(0x5eed_1a7e_0c41_0001);
        let (to, from) = samples();
        let valid: Vec<Vec<u8>> = to
            .iter()
            .map(ToHelper::encode)
            .chain(from.iter().map(FromHelper::encode))
            .chain([vec![1], vec![2], vec![4]])
            .chain([ToService::LogonIdentity(identity()).encode()])
            .collect();
        for round in 0..200_000 {
            let bytes = if round % 2 == 0 {
                let length = random.below(40);
                (0..length)
                    .map(|_| random.next() as u8)
                    .collect::<Vec<u8>>()
            } else {
                let mut bytes = valid[random.below(valid.len())].clone();
                match random.below(3) {
                    0 if !bytes.is_empty() => {
                        let at = random.below(bytes.len());
                        bytes[at] ^= 1 << random.below(8);
                    }
                    1 => bytes.truncate(random.below(bytes.len() + 1)),
                    _ => bytes.push(random.next() as u8),
                }
                bytes
            };
            if let Some(message) = ToHelper::decode(&bytes) {
                assert_eq!(ToHelper::decode(&message.encode()), Some(message));
            }
            if let Some(message) = FromHelper::decode(&bytes) {
                assert_eq!(message.encode(), bytes);
            }
            if let Some(message) = ToService::decode(&bytes) {
                assert_eq!(message.encode(), bytes);
            }
            if let Some(message) = FromService::decode(&bytes) {
                assert_eq!(message.encode(), bytes);
            }
        }
    }
}
