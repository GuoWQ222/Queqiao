//! Messages between viewer and agent on the WebRTC data channel (video itself travels as RTP).
//! A compact binary encoding: one tag byte, then little-endian fields. Unknown tags are ignored by the
//! receiver, so newer peers can add messages.

/// A mouse button.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    /// The "back" side button.
    X1,
    /// The "forward" side button.
    X2,
}

/// One input event from the viewer, for the agent to inject.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputEvent {
    /// Absolute pointer position on the remote screen: 0 = left/top edge, 65535 = right/bottom edge.
    MouseMove {
        x: u16,
        y: u16,
    },
    MouseButton {
        button: MouseButton,
        down: bool,
    },
    /// Wheel movement in Windows units (120 = one notch); positive = away from the user / to the right.
    Wheel {
        delta: i16,
        horizontal: bool,
    },
    /// A key by its hardware scancode (set 1), so the remote keyboard layout decides the character.
    Key {
        scancode: u16,
        extended: bool,
        down: bool,
    },
    /// Relative pointer movement in mouse counts (game mouse: games read movement, not positions).
    MouseDelta {
        dx: i16,
        dy: i16,
    },
}

/// Everything sent on the data channel.
#[derive(Clone, Debug, PartialEq)]
pub enum Message {
    Input(InputEvent),
    /// Round-trip measurement: the receiver answers with `Pong` carrying the same values.
    Ping {
        id: u32,
        sent_us: u64,
    },
    Pong {
        id: u32,
        sent_us: u64,
    },
    /// Agent -> viewer, one per video frame: how long the agent took from the screen change to sending.
    FrameInfo {
        time_90khz: u64,
        agent_ms: f32,
    },
    /// The sender ends the session (the other side stops at once instead of waiting for a timeout).
    Leave,
    /// The sender's clipboard now holds this text (both directions; at most [`MAX_CLIPBOARD_TEXT`] bytes).
    ClipboardText(String),
    /// Viewer -> agent: do something on the controlled PC.
    Command(Command),
    /// Agent -> viewer: something to show the person controlling (e.g. why a command cannot run).
    Notice(String),
    /// Agent -> viewer: the PC's screens (width, height) and which one is shown (at most 16).
    Screens {
        current: u8,
        sizes: Vec<(u16, u16)>,
    },
    /// Agent -> viewer: the pointer's shape on that PC (the session window's cursor takes it).
    PointerShape(PointerShape),
    /// Agent -> viewer: that PC's pointer is hidden (false; games, videos) or shown again.
    PointerVisible(bool),
}

/// A pointer shape as Windows gives it, rows tightly packed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PointerShape {
    pub kind: PointerKind,
    /// The pointer's size in pixels (at most [`MAX_POINTER`] each way).
    pub width: u16,
    pub height: u16,
    /// Where the pointer points, inside it.
    pub hot_x: u16,
    pub hot_y: u16,
    /// Monochrome: the AND mask's rows, then the XOR mask's (1 bit per pixel, `(width + 7) / 8` bytes per row);
    /// colour and masked colour: `width * 4` bytes per row, B, G, R, A.
    pub data: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PointerKind {
    Monochrome,
    /// Alpha blended.
    Color,
    /// Alpha 0xFF: the pixel is XORed with the screen; 0: it replaces it.
    MaskedColor,
}

/// The largest pointer shape sent, each way.
pub const MAX_POINTER: u16 = 256;

impl PointerShape {
    /// How many bytes `data` holds for this kind and size.
    pub fn expected_len(kind: PointerKind, width: u16, height: u16) -> usize {
        let (width, height) = (usize::from(width), usize::from(height));
        match kind {
            PointerKind::Monochrome => width.div_ceil(8) * height * 2,
            PointerKind::Color | PointerKind::MaskedColor => width * height * 4,
        }
    }

    /// Within the limits, the hot spot inside, `data` the right length.
    pub fn is_valid(&self) -> bool {
        (1..=MAX_POINTER).contains(&self.width)
            && (1..=MAX_POINTER).contains(&self.height)
            && self.hot_x < self.width
            && self.hot_y < self.height
            && self.data.len() == Self::expected_len(self.kind, self.width, self.height)
    }
}

/// The video codec of a stream.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum VideoCodec {
    #[default]
    H264,
    Av1,
}

impl VideoCodec {
    pub fn name(self) -> &'static str {
        match self {
            VideoCodec::H264 => "H.264",
            VideoCodec::Av1 => "AV1",
        }
    }
}

/// The codec the viewer asks for: Automatic (the controlled PC picks), or one of them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CodecPreference {
    #[default]
    Automatic,
    H264,
    Av1,
}

/// What the viewer can ask the controlled PC to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    /// Ctrl+Alt+Del (the secure attention sequence: needs the Queqiao service).
    CtrlAltDel,
    /// Lock the PC (Windows' lock screen).
    Lock,
    /// Restart the PC.
    Restart,
    /// Send this PC's sound (the default) or stop sending it.
    SoundOn,
    SoundOff,
    /// Darken that PC's monitors while controlled (the person there sees a dark screen; capture does not).
    PrivacyOn,
    PrivacyOff,
    /// Picture settings: frames per second at most (0: as many as the screen shows) and quality.
    Picture {
        fps: u8,
        quality: Quality,
    },
    /// Mute that PC's speakers for the session (its sound still comes to the viewer), or not.
    MuteSpeakers(bool),
    /// The picture's size: its short side at most this many pixels (e.g. 720, 1080), or 0 for the screen's own.
    Resolution(u16),
    /// Show screen `n` (counted as in `Message::Screens`).
    Screen(u8),
    /// The highest bitrate, kbit/s (instead of a quality preset); 0 goes back to the preset.
    MaxBitrate(u32),
    /// End Queqiao on that PC and start it again (through its service).
    RestartQueqiao,
    /// The picture's codec as the viewer wants it, and whether it can decode AV1 (H.264 it always can).
    VideoCodec {
        prefer: CodecPreference,
        can_decode_av1: bool,
    },
}

/// How the bitrate is spent: fewer bits (slow networks), the size-based amount, or twice that (sharp text).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Quality {
    Smooth,
    Balanced,
    Sharp,
}

impl Quality {
    /// The share of the size-based bitrate the encoder may use at most.
    pub fn bitrate_factor(self) -> f64 {
        match self {
            Quality::Smooth => 0.5,
            Quality::Balanced => 1.0,
            Quality::Sharp => 2.0,
        }
    }
}

/// Clipboard text larger than this (UTF-8 bytes) is not sent in one message.
pub const MAX_CLIPBOARD_TEXT: usize = 256 * 1024;

const INPUT_MOVE: u8 = 1;
const INPUT_BUTTON: u8 = 2;
const INPUT_WHEEL: u8 = 3;
const INPUT_KEY: u8 = 4;
const INPUT_DELTA: u8 = 5;
const PING: u8 = 20;
const PONG: u8 = 21;
const FRAME_INFO: u8 = 30;
const LEAVE: u8 = 40;
const CLIPBOARD_TEXT: u8 = 50;
const COMMAND: u8 = 60;
const NOTICE: u8 = 61;
const SCREENS: u8 = 62;
const POINTER_SHAPE: u8 = 63;
const POINTER_VISIBLE: u8 = 64;

impl Message {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(16);
        match *self {
            Message::Input(InputEvent::MouseMove { x, y }) => {
                out.push(INPUT_MOVE);
                out.extend_from_slice(&x.to_le_bytes());
                out.extend_from_slice(&y.to_le_bytes());
            }
            Message::Input(InputEvent::MouseButton { button, down }) => {
                out.extend([INPUT_BUTTON, button as u8, u8::from(down)]);
            }
            Message::Input(InputEvent::MouseDelta { dx, dy }) => {
                out.push(INPUT_DELTA);
                out.extend_from_slice(&dx.to_le_bytes());
                out.extend_from_slice(&dy.to_le_bytes());
            }
            Message::Input(InputEvent::Wheel { delta, horizontal }) => {
                out.push(INPUT_WHEEL);
                out.extend_from_slice(&delta.to_le_bytes());
                out.push(u8::from(horizontal));
            }
            Message::Input(InputEvent::Key {
                scancode,
                extended,
                down,
            }) => {
                out.push(INPUT_KEY);
                out.extend_from_slice(&scancode.to_le_bytes());
                out.extend([u8::from(extended), u8::from(down)]);
            }
            Message::Ping { id, sent_us } | Message::Pong { id, sent_us } => {
                out.push(if matches!(self, Message::Ping { .. }) {
                    PING
                } else {
                    PONG
                });
                out.extend_from_slice(&id.to_le_bytes());
                out.extend_from_slice(&sent_us.to_le_bytes());
            }
            Message::FrameInfo {
                time_90khz,
                agent_ms,
            } => {
                out.push(FRAME_INFO);
                out.extend_from_slice(&time_90khz.to_le_bytes());
                out.extend_from_slice(&agent_ms.to_le_bytes());
            }
            Message::Leave => out.push(LEAVE),
            Message::ClipboardText(ref text) => {
                out.push(CLIPBOARD_TEXT);
                out.extend_from_slice(text.as_bytes());
            }
            Message::Command(command) => {
                out.push(COMMAND);
                match command {
                    Command::CtrlAltDel => out.push(1),
                    Command::Lock => out.push(2),
                    Command::RestartQueqiao => out.push(13),
                    Command::VideoCodec {
                        prefer,
                        can_decode_av1,
                    } => out.extend([
                        14,
                        match prefer {
                            CodecPreference::Automatic => 0,
                            CodecPreference::H264 => 1,
                            CodecPreference::Av1 => 2,
                        },
                        u8::from(can_decode_av1),
                    ]),
                    Command::Restart => out.push(3),
                    Command::SoundOn => out.push(4),
                    Command::SoundOff => out.push(5),
                    Command::PrivacyOn => out.push(6),
                    Command::PrivacyOff => out.push(7),
                    Command::MuteSpeakers(on) => out.extend([9, u8::from(on)]),
                    Command::Resolution(short) => {
                        out.push(10);
                        out.extend_from_slice(&short.to_le_bytes());
                    }
                    Command::Screen(index) => out.extend([11, index]),
                    Command::MaxBitrate(kbit) => {
                        out.push(12);
                        out.extend_from_slice(&kbit.to_le_bytes());
                    }
                    Command::Picture { fps, quality } => out.extend([
                        8,
                        fps,
                        match quality {
                            Quality::Smooth => 0,
                            Quality::Balanced => 1,
                            Quality::Sharp => 2,
                        },
                    ]),
                }
            }
            Message::Notice(ref text) => {
                out.push(NOTICE);
                out.extend_from_slice(text.as_bytes());
            }
            Message::Screens { current, ref sizes } => {
                out.extend([SCREENS, current, sizes.len().min(16) as u8]);
                for (width, height) in sizes.iter().take(16) {
                    out.extend_from_slice(&width.to_le_bytes());
                    out.extend_from_slice(&height.to_le_bytes());
                }
            }
            Message::PointerShape(ref shape) => {
                out.push(POINTER_SHAPE);
                out.push(match shape.kind {
                    PointerKind::Monochrome => 1,
                    PointerKind::Color => 2,
                    PointerKind::MaskedColor => 4,
                });
                for value in [shape.width, shape.height, shape.hot_x, shape.hot_y] {
                    out.extend_from_slice(&value.to_le_bytes());
                }
                out.extend_from_slice(&shape.data);
            }
            Message::PointerVisible(visible) => out.extend([POINTER_VISIBLE, u8::from(visible)]),
        }
        out
    }

    /// `None` for unknown or truncated messages.
    pub fn decode(bytes: &[u8]) -> Option<Message> {
        let (&tag, rest) = bytes.split_first()?;
        let u16_at = |i: usize| Some(u16::from_le_bytes(rest.get(i..i + 2)?.try_into().ok()?));
        let u32_at = |i: usize| Some(u32::from_le_bytes(rest.get(i..i + 4)?.try_into().ok()?));
        let u64_at = |i: usize| Some(u64::from_le_bytes(rest.get(i..i + 8)?.try_into().ok()?));
        let flag = |i: usize| rest.get(i).map(|&b| b != 0);
        Some(match tag {
            INPUT_MOVE => Message::Input(InputEvent::MouseMove {
                x: u16_at(0)?,
                y: u16_at(2)?,
            }),
            INPUT_DELTA => Message::Input(InputEvent::MouseDelta {
                dx: u16_at(0)? as i16,
                dy: u16_at(2)? as i16,
            }),
            INPUT_BUTTON => {
                let button = match rest.first()? {
                    0 => MouseButton::Left,
                    1 => MouseButton::Right,
                    2 => MouseButton::Middle,
                    3 => MouseButton::X1,
                    4 => MouseButton::X2,
                    _ => return None,
                };
                Message::Input(InputEvent::MouseButton {
                    button,
                    down: flag(1)?,
                })
            }
            INPUT_WHEEL => Message::Input(InputEvent::Wheel {
                delta: u16_at(0)? as i16,
                horizontal: flag(2)?,
            }),
            INPUT_KEY => Message::Input(InputEvent::Key {
                scancode: u16_at(0)?,
                extended: flag(2)?,
                down: flag(3)?,
            }),
            PING => Message::Ping {
                id: u32_at(0)?,
                sent_us: u64_at(4)?,
            },
            PONG => Message::Pong {
                id: u32_at(0)?,
                sent_us: u64_at(4)?,
            },
            FRAME_INFO => {
                let agent_ms = f32::from_bits(u32_at(8)?);
                // A delay is a finite, non-negative number of milliseconds; anything else (NaN, infinity) would
                // spoil the delay statistics.
                if !agent_ms.is_finite() || agent_ms < 0.0 {
                    return None;
                }
                Message::FrameInfo {
                    time_90khz: u64_at(0)?,
                    agent_ms,
                }
            }
            LEAVE => Message::Leave,
            CLIPBOARD_TEXT => Message::ClipboardText(String::from_utf8(rest.to_vec()).ok()?),
            COMMAND => Message::Command(match rest.first()? {
                1 => Command::CtrlAltDel,
                2 => Command::Lock,
                13 => Command::RestartQueqiao,
                14 => Command::VideoCodec {
                    prefer: match rest.get(1)? {
                        0 => CodecPreference::Automatic,
                        1 => CodecPreference::H264,
                        2 => CodecPreference::Av1,
                        _ => return None,
                    },
                    can_decode_av1: match rest.get(2)? {
                        0 => false,
                        1 => true,
                        _ => return None,
                    },
                },
                3 => Command::Restart,
                4 => Command::SoundOn,
                5 => Command::SoundOff,
                6 => Command::PrivacyOn,
                7 => Command::PrivacyOff,
                10 => Command::Resolution(u16::from_le_bytes(rest.get(1..3)?.try_into().ok()?)),
                11 => Command::Screen(*rest.get(1)?),
                12 => Command::MaxBitrate(u32::from_le_bytes(rest.get(1..5)?.try_into().ok()?)),
                9 => Command::MuteSpeakers(match rest.get(1)? {
                    0 => false,
                    1 => true,
                    _ => return None,
                }),
                8 => Command::Picture {
                    fps: *rest.get(1)?,
                    quality: match rest.get(2)? {
                        0 => Quality::Smooth,
                        1 => Quality::Balanced,
                        2 => Quality::Sharp,
                        _ => return None,
                    },
                },
                _ => return None,
            }),
            NOTICE => Message::Notice(String::from_utf8(rest.to_vec()).ok()?),
            SCREENS => {
                let (&current, rest) = rest.split_first()?;
                let (&count, sizes) = rest.split_first()?;
                if count > 16 || sizes.len() != usize::from(count) * 4 {
                    return None;
                }
                let sizes = sizes
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|pair| {
                        (
                            u16::from_le_bytes([pair[0], pair[1]]),
                            u16::from_le_bytes([pair[2], pair[3]]),
                        )
                    })
                    .collect();
                Message::Screens { current, sizes }
            }
            POINTER_SHAPE => {
                let kind = match rest.first()? {
                    1 => PointerKind::Monochrome,
                    2 => PointerKind::Color,
                    4 => PointerKind::MaskedColor,
                    _ => return None,
                };
                let at =
                    |i: usize| Some(u16::from_le_bytes(rest.get(1 + i..3 + i)?.try_into().ok()?));
                let shape = PointerShape {
                    kind,
                    width: at(0)?,
                    height: at(2)?,
                    hot_x: at(4)?,
                    hot_y: at(6)?,
                    data: rest.get(9..)?.to_vec(),
                };
                if !shape.is_valid() {
                    return None;
                }
                Message::PointerShape(shape)
            }
            POINTER_VISIBLE => Message::PointerVisible(match rest {
                [0] => false,
                [1] => true,
                _ => return None,
            }),
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pointer_shapes_round_trip_and_bad_ones_are_refused() {
        let shapes = [
            PointerShape {
                kind: PointerKind::Monochrome,
                width: 32,
                height: 32,
                hot_x: 0,
                hot_y: 0,
                data: vec![0xAA; 4 * 32 * 2],
            },
            PointerShape {
                kind: PointerKind::Color,
                width: 48,
                height: 48,
                hot_x: 10,
                hot_y: 20,
                data: vec![7; 48 * 48 * 4],
            },
            PointerShape {
                kind: PointerKind::MaskedColor,
                width: 17,
                height: 5,
                hot_x: 16,
                hot_y: 4,
                data: vec![1; 17 * 5 * 4],
            },
        ];
        for shape in shapes {
            let message = Message::PointerShape(shape.clone());
            assert_eq!(Message::decode(&message.encode()), Some(message.clone()));
            // One byte short or long, the hot spot outside, too large: refused.
            let bytes = message.encode();
            assert_eq!(Message::decode(&bytes[..bytes.len() - 1]), None);
            let mut longer = bytes.clone();
            longer.push(0);
            assert_eq!(Message::decode(&longer), None);
            let outside = PointerShape {
                hot_x: shape.width,
                ..shape.clone()
            };
            assert_eq!(
                Message::decode(&Message::PointerShape(outside).encode()),
                None
            );
        }
        let huge = PointerShape {
            kind: PointerKind::Color,
            width: MAX_POINTER + 1,
            height: 1,
            hot_x: 0,
            hot_y: 0,
            data: vec![0; (usize::from(MAX_POINTER) + 1) * 4],
        };
        assert_eq!(Message::decode(&Message::PointerShape(huge).encode()), None);
        for visible in [false, true] {
            let message = Message::PointerVisible(visible);
            assert_eq!(Message::decode(&message.encode()), Some(message));
        }
        assert_eq!(Message::decode(&[POINTER_VISIBLE, 2]), None);
    }

    #[test]
    fn every_message_round_trips() {
        let messages = [
            Message::Input(InputEvent::MouseMove { x: 0, y: 65535 }),
            Message::Input(InputEvent::MouseDelta { dx: -40, dy: 32767 }),
            Message::Input(InputEvent::MouseButton {
                button: MouseButton::X2,
                down: true,
            }),
            Message::Input(InputEvent::Wheel {
                delta: -240,
                horizontal: true,
            }),
            Message::Input(InputEvent::Key {
                scancode: 0x1D,
                extended: true,
                down: false,
            }),
            Message::Ping {
                id: 7,
                sent_us: u64::MAX,
            },
            Message::Pong { id: 7, sent_us: 1 },
            Message::FrameInfo {
                time_90khz: 1 << 40,
                agent_ms: 3.25,
            },
            Message::Leave,
            Message::ClipboardText("copied text, ä 中文".into()),
            Message::Command(Command::Lock),
            Message::Command(Command::RestartQueqiao),
            Message::Command(Command::VideoCodec {
                prefer: CodecPreference::Av1,
                can_decode_av1: true,
            }),
            Message::Command(Command::VideoCodec {
                prefer: CodecPreference::Automatic,
                can_decode_av1: false,
            }),
            Message::Command(Command::Restart),
            Message::Command(Command::CtrlAltDel),
            Message::Command(Command::SoundOn),
            Message::Command(Command::SoundOff),
            Message::Command(Command::PrivacyOn),
            Message::Command(Command::PrivacyOff),
            Message::Command(Command::MuteSpeakers(true)),
            Message::Command(Command::MuteSpeakers(false)),
            Message::Command(Command::Resolution(720)),
            Message::Command(Command::Resolution(0)),
            Message::Command(Command::Screen(1)),
            Message::Command(Command::MaxBitrate(50_000)),
            Message::Screens {
                current: 1,
                sizes: vec![(2560, 1440), (1920, 1080)],
            },
            Message::Command(Command::Picture {
                fps: 30,
                quality: Quality::Sharp,
            }),
            Message::Command(Command::Picture {
                fps: 0,
                quality: Quality::Smooth,
            }),
            Message::Notice("the PC is locked".into()),
        ];
        for message in messages {
            assert_eq!(Message::decode(&message.encode()), Some(message));
        }
    }

    #[test]
    fn unknown_and_truncated_messages_are_ignored() {
        assert_eq!(Message::decode(&[]), None);
        assert_eq!(Message::decode(&[200, 1, 2]), None);
        assert_eq!(Message::decode(&[INPUT_MOVE, 1, 2, 3]), None);
        assert_eq!(Message::decode(&[INPUT_BUTTON, 9, 1]), None);
    }

    #[test]
    fn frame_delays_must_be_numbers() {
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, -1.0] {
            let mut bytes = vec![FRAME_INFO];
            bytes.extend_from_slice(&7u64.to_le_bytes());
            bytes.extend_from_slice(&bad.to_bits().to_le_bytes());
            assert_eq!(Message::decode(&bytes), None, "{bad}");
        }
    }
}

/// Robustness: the other side's bytes are not trusted. Random and mutated inputs must never panic, and whatever
/// is accepted must encode back to the same message.
#[cfg(test)]
mod robustness {
    use super::*;

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
        fn bytes(&mut self, n: usize) -> Vec<u8> {
            (0..n).map(|_| self.next() as u8).collect()
        }
        fn mutate(&mut self, valid: &[u8]) -> Vec<u8> {
            let mut out = valid.to_vec();
            match self.below(4) {
                0 if !out.is_empty() => {
                    let at = self.below(out.len());
                    out[at] ^= 1 << self.below(8);
                }
                1 => out.truncate(self.below(out.len() + 1)),
                2 => {
                    let extra = self.below(16);
                    out.extend(self.bytes(extra));
                }
                _ if !out.is_empty() => {
                    let at = self.below(out.len());
                    out[at] = self.next() as u8;
                }
                _ => {}
            }
            out
        }
    }

    #[test]
    fn random_and_mutated_messages_never_panic() {
        let samples: Vec<Vec<u8>> = [
            Message::Input(InputEvent::MouseMove { x: 10, y: 20 }),
            Message::Input(InputEvent::Key {
                scancode: 0x1C,
                extended: false,
                down: true,
            }),
            Message::Ping { id: 1, sent_us: 2 },
            Message::FrameInfo {
                time_90khz: 3,
                agent_ms: 4.0,
            },
            Message::ClipboardText("text".into()),
            Message::Command(Command::Picture {
                fps: 60,
                quality: Quality::Balanced,
            }),
            Message::Notice("notice".into()),
        ]
        .iter()
        .map(Message::encode)
        .collect();
        let mut random = Random(0x9E37_79B9_7F4A_7C15);
        for round in 0..200_000 {
            let bytes = if round % 2 == 0 {
                let length = random.below(48);
                random.bytes(length)
            } else {
                let sample = &samples[random.below(samples.len())];
                random.mutate(sample)
            };
            if let Some(message) = Message::decode(&bytes) {
                assert_eq!(
                    Message::decode(&message.encode()),
                    Some(message),
                    "{bytes:?}"
                );
            }
        }
    }
}
