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
const PING: u8 = 20;
const PONG: u8 = 21;
const FRAME_INFO: u8 = 30;
const LEAVE: u8 = 40;
const CLIPBOARD_TEXT: u8 = 50;
const COMMAND: u8 = 60;
const NOTICE: u8 = 61;

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
                    Command::Restart => out.push(3),
                    Command::SoundOn => out.push(4),
                    Command::SoundOff => out.push(5),
                    Command::PrivacyOn => out.push(6),
                    Command::PrivacyOff => out.push(7),
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
            FRAME_INFO => Message::FrameInfo {
                time_90khz: u64_at(0)?,
                agent_ms: f32::from_bits(u32_at(8)?),
            },
            LEAVE => Message::Leave,
            CLIPBOARD_TEXT => Message::ClipboardText(String::from_utf8(rest.to_vec()).ok()?),
            COMMAND => Message::Command(match rest.first()? {
                1 => Command::CtrlAltDel,
                2 => Command::Lock,
                3 => Command::Restart,
                4 => Command::SoundOn,
                5 => Command::SoundOff,
                6 => Command::PrivacyOn,
                7 => Command::PrivacyOff,
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
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_message_round_trips() {
        let messages = [
            Message::Input(InputEvent::MouseMove { x: 0, y: 65535 }),
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
            Message::Command(Command::Restart),
            Message::Command(Command::CtrlAltDel),
            Message::Command(Command::SoundOn),
            Message::Command(Command::SoundOff),
            Message::Command(Command::PrivacyOn),
            Message::Command(Command::PrivacyOff),
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
}
