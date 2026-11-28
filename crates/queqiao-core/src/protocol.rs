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

/// Why the controlled PC asks for its Windows password before it shows its screen (as UU Remote).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PasswordReason {
    /// The PC is locked.
    Locked,
    /// Nobody is signed in on it.
    SignedOut,
    /// Locked, and this viewer is not asked (someone helping from another account): the person there unlocks it.
    OnlyTheOwner,
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
    /// Viewer -> agent: text typed on the viewer (a phone's keyboard, in any language), typed on the PC as characters,
    /// whatever its keyboard layout (at most [`MAX_TYPE_TEXT`] bytes).
    TypeText(String),
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
    /// Agent -> viewer: the picture and input wait for that PC's Windows password; `tries_left` more tries (0: this
    /// viewer is not asked, or has no tries left).
    PasswordNeeded {
        reason: PasswordReason,
        tries_left: u8,
    },
    /// Viewer -> agent: the password the person typed (only in the encrypted session; Windows checks it there).
    Password(String),
    /// Agent -> viewer: right (picture and input go on) or wrong, `tries_left` more.
    PasswordResult {
        ok: bool,
        tries_left: u8,
    },
    /// Agent -> viewer: the picture's encoder now, for the session card (the user, 2026-11-27: encoding and decoding
    /// on lines of their own): what it makes ("H.265 4:2:0") and whether that PC's GPU makes it. Sent when the encoder
    /// starts and whenever it changes; a viewer before it leaves it out.
    Encoder {
        picture: String,
        gpu: bool,
    },
    /// Agent -> viewer: the resolutions the shown screen offers (largest first, at most 24), the one it has, and the
    /// one it had before this session changed it (`own`; the same as `current` while unchanged). `note`: why its
    /// resolution cannot change (then `sizes` is empty). A viewer before it leaves it out.
    DisplayModes {
        current: (u16, u16),
        own: (u16, u16),
        sizes: Vec<(u16, u16)>,
        note: String,
    },
    /// Agent -> viewer: Super Screen is on (that PC shows a virtual screen of `size`) or off; `note`: why it did not
    /// change (empty when it did). A viewer before it leaves it out.
    SuperScreenState {
        on: bool,
        size: (u16, u16),
        note: String,
    },
    // Drawing on that PC's screen (annotation, 2026-11-28).
    /// Viewer -> agent: drawing on the shown screen of that PC for the person there (help sessions): pen strokes on top
    /// of everything, which clicks and keys pass through. An agent before it drops these, and then never answers with
    /// `AnnotateState`.
    Annotate(Annotation),
    /// Agent -> viewer: the drawing on its screen now: shown with `strokes` strokes (the answer to `On`, to each stroke's
    /// `End` and to `Clear`), or not shown (the answer to `Off`; or it cannot be shown: `note` says why, e.g. Wayland).
    AnnotateState {
        shown: bool,
        strokes: u16,
        note: String,
    },
}

/// One step of drawing on that PC's screen (`Message::Annotate`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Annotation {
    /// Drawing mode is on: the drawing's window goes up over that screen (empty).
    On,
    /// A new stroke in `colour` (0xRRGGBB), `width` thick in pixels of a 1080-line screen (the agent scales it to its
    /// screen's short side, so a stroke looks as thick on any screen; 1 to [`MAX_STROKE_WIDTH`]).
    Begin { colour: u32, width: u8 },
    /// The stroke goes on to here: 0..=65535 across the shown screen, as `InputEvent::MouseMove`.
    Point { x: u16, y: u16 },
    /// The stroke is finished.
    End,
    /// Every stroke taken away (drawing mode stays on).
    Clear,
    /// Drawing mode is off: the strokes and their window go.
    Off,
}

/// The thickest stroke, in pixels of a 1080-line screen.
pub const MAX_STROKE_WIDTH: u8 = 64;

/// The longest password sent (UTF-8 bytes).
pub const MAX_PASSWORD: usize = 256;

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
    /// Through the GPU makers' encoders (NVENC, oneVPL, AMF), also 4:4:4 and 10-bit; or Windows' H.265 encoder.
    H265,
}

impl VideoCodec {
    pub fn name(self) -> &'static str {
        match self {
            VideoCodec::H264 => "H.264",
            VideoCodec::Av1 => "AV1",
            VideoCodec::H265 => "H.265",
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
    H265,
}

/// What H.265 a viewer decodes: the bits of `Command::VideoCodec`'s `can_decode_hevc`.
pub mod hevc {
    /// Main (4:2:0, 8 bits).
    pub const MAIN: u8 = 1;
    /// Main 10 (4:2:0, 10 bits).
    pub const MAIN10: u8 = 2;
    /// 4:4:4, 8 bits (range extensions).
    pub const CHROMA444: u8 = 4;
    /// 4:4:4, 10 bits.
    pub const CHROMA444_10: u8 = 8;

    /// The bit for a format.
    pub fn bit(chroma444: bool, ten_bit: bool) -> u8 {
        match (chroma444, ten_bit) {
            (false, false) => MAIN,
            (false, true) => MAIN10,
            (true, false) => CHROMA444,
            (true, true) => CHROMA444_10,
        }
    }

    /// "Main, Main 10, 4:4:4, 4:4:4 10-bit" (or "none").
    pub fn describe(caps: u8) -> String {
        let names: Vec<&str> = [
            (MAIN, "Main"),
            (MAIN10, "Main 10"),
            (CHROMA444, "4:4:4"),
            (CHROMA444_10, "4:4:4 10-bit"),
        ]
        .iter()
        .filter(|(bit, _)| caps & bit != 0)
        .map(|(_, name)| *name)
        .collect();
        if names.is_empty() {
            "none".into()
        } else {
            names.join(", ")
        }
    }
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
    /// The shown screen of that PC to this resolution for the session (one of `Message::DisplayModes`' sizes);
    /// 0 x 0: back to its own. Put back when the session ends.
    DisplayMode {
        width: u16,
        height: u16,
    },
    /// Super Screen (Windows): a virtual screen on that PC of this size and refresh rate (0: 60 Hz) and Windows'
    /// scale in percent (0: as Windows recommends), shown instead of that PC's own screens (they go dark); 0 x 0:
    /// back to its own screens. Undone when the session ends.
    SuperScreen {
        width: u16,
        height: u16,
        refresh: u8,
        scale: u16,
    },
    /// Show screen `n` (counted as in `Message::Screens`).
    Screen(u8),
    /// The highest bitrate, kbit/s (instead of a quality preset); 0 goes back to the preset.
    MaxBitrate(u32),
    /// End Queqiao on that PC and start it again (through its service).
    RestartQueqiao,
    /// The viewer's microphone comes (true) or stops: the controlled PC plays it into a virtual microphone.
    Microphone(bool),
    /// Lock that PC when this session ends (true), as its own setting may also say.
    LockAfter(bool),
    /// The picture's codec as the viewer wants it, and whether it can decode AV1 (H.264 it always can), what H.265 it
    /// decodes (`hevc` bits; 0 from viewers before H.265), and whether it wants 4:4:4 colour and 10 bits (H.265).
    VideoCodec {
        prefer: CodecPreference,
        can_decode_av1: bool,
        can_decode_hevc: u8,
        chroma444: bool,
        ten_bit: bool,
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
/// Typed text longer than this (UTF-8 bytes) is not sent in one message.
pub const MAX_TYPE_TEXT: usize = 4096;

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
const TYPE_TEXT: u8 = 51;
const COMMAND: u8 = 60;
const NOTICE: u8 = 61;
const SCREENS: u8 = 62;
const POINTER_SHAPE: u8 = 63;
const POINTER_VISIBLE: u8 = 64;
const PASSWORD_NEEDED: u8 = 65;
const PASSWORD: u8 = 66;
const PASSWORD_RESULT: u8 = 67;
const ENCODER: u8 = 68;
const DISPLAY_MODES: u8 = 69;
const ANNOTATE: u8 = 70;
const ANNOTATE_STATE: u8 = 71;
const SUPER_SCREEN_STATE: u8 = 72;

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
            Message::TypeText(ref text) => {
                out.push(TYPE_TEXT);
                out.extend_from_slice(text.as_bytes());
            }
            Message::Command(command) => {
                out.push(COMMAND);
                match command {
                    Command::CtrlAltDel => out.push(1),
                    Command::Lock => out.push(2),
                    Command::RestartQueqiao => out.push(13),
                    Command::Microphone(on) => out.extend([15, u8::from(on)]),
                    Command::LockAfter(on) => out.extend([16, u8::from(on)]),
                    Command::VideoCodec {
                        prefer,
                        can_decode_av1,
                        can_decode_hevc,
                        chroma444,
                        ten_bit,
                    } => out.extend([
                        14,
                        match prefer {
                            CodecPreference::Automatic => 0,
                            CodecPreference::H264 => 1,
                            CodecPreference::Av1 => 2,
                            CodecPreference::H265 => 3,
                        },
                        u8::from(can_decode_av1),
                        // Earlier programs read the first three bytes only.
                        can_decode_hevc & 0x0F,
                        u8::from(chroma444) | (u8::from(ten_bit) << 1),
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
                    Command::DisplayMode { width, height } => {
                        out.push(17);
                        out.extend_from_slice(&width.to_le_bytes());
                        out.extend_from_slice(&height.to_le_bytes());
                    }
                    Command::SuperScreen {
                        width,
                        height,
                        refresh,
                        scale,
                    } => {
                        out.push(18);
                        out.extend_from_slice(&width.to_le_bytes());
                        out.extend_from_slice(&height.to_le_bytes());
                        out.push(refresh);
                        out.extend_from_slice(&scale.to_le_bytes());
                    }
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
            Message::PasswordNeeded { reason, tries_left } => out.extend([
                PASSWORD_NEEDED,
                match reason {
                    PasswordReason::Locked => 1,
                    PasswordReason::SignedOut => 2,
                    PasswordReason::OnlyTheOwner => 3,
                },
                tries_left,
            ]),
            Message::Password(ref password) => {
                out.push(PASSWORD);
                out.extend_from_slice(password.as_bytes());
            }
            Message::PasswordResult { ok, tries_left } => {
                out.extend([PASSWORD_RESULT, u8::from(ok), tries_left])
            }
            Message::Encoder { ref picture, gpu } => {
                out.extend([ENCODER, u8::from(gpu)]);
                // At most 64 bytes, cut between characters.
                let mut end = picture.len().min(64);
                while !picture.is_char_boundary(end) {
                    end -= 1;
                }
                out.extend_from_slice(&picture.as_bytes()[..end]);
            }
            Message::SuperScreenState { on, size, ref note } => {
                out.extend([SUPER_SCREEN_STATE, u8::from(on)]);
                out.extend_from_slice(&size.0.to_le_bytes());
                out.extend_from_slice(&size.1.to_le_bytes());
                let mut end = note.len().min(200);
                while !note.is_char_boundary(end) {
                    end -= 1;
                }
                out.extend_from_slice(&note.as_bytes()[..end]);
            }
            Message::DisplayModes {
                current,
                own,
                ref sizes,
                ref note,
            } => {
                out.push(DISPLAY_MODES);
                for value in [current.0, current.1, own.0, own.1] {
                    out.extend_from_slice(&value.to_le_bytes());
                }
                out.push(sizes.len().min(24) as u8);
                for (width, height) in sizes.iter().take(24) {
                    out.extend_from_slice(&width.to_le_bytes());
                    out.extend_from_slice(&height.to_le_bytes());
                }
                // At most 200 bytes, cut between characters.
                let mut end = note.len().min(200);
                while !note.is_char_boundary(end) {
                    end -= 1;
                }
                out.extend_from_slice(&note.as_bytes()[..end]);
            }
            Message::Annotate(annotation) => {
                out.push(ANNOTATE);
                match annotation {
                    Annotation::On => out.push(1),
                    Annotation::Begin { colour, width } => {
                        let [blue, green, red, _] = colour.to_le_bytes();
                        out.extend([2, red, green, blue, width.clamp(1, MAX_STROKE_WIDTH)]);
                    }
                    Annotation::Point { x, y } => {
                        out.push(3);
                        out.extend_from_slice(&x.to_le_bytes());
                        out.extend_from_slice(&y.to_le_bytes());
                    }
                    Annotation::End => out.push(4),
                    Annotation::Clear => out.push(5),
                    Annotation::Off => out.push(6),
                }
            }
            Message::AnnotateState {
                shown,
                strokes,
                ref note,
            } => {
                out.extend([ANNOTATE_STATE, u8::from(shown)]);
                out.extend_from_slice(&strokes.to_le_bytes());
                // At most 200 bytes, cut between characters.
                let mut end = note.len().min(200);
                while !note.is_char_boundary(end) {
                    end -= 1;
                }
                out.extend_from_slice(&note.as_bytes()[..end]);
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
            TYPE_TEXT if rest.len() <= MAX_TYPE_TEXT => {
                Message::TypeText(String::from_utf8(rest.to_vec()).ok()?)
            }
            COMMAND => Message::Command(match rest.first()? {
                1 => Command::CtrlAltDel,
                2 => Command::Lock,
                13 => Command::RestartQueqiao,
                15 => Command::Microphone(match rest.get(1)? {
                    0 => false,
                    1 => true,
                    _ => return None,
                }),
                16 => Command::LockAfter(match rest.get(1)? {
                    0 => false,
                    1 => true,
                    _ => return None,
                }),
                14 => Command::VideoCodec {
                    prefer: match rest.get(1)? {
                        0 => CodecPreference::Automatic,
                        1 => CodecPreference::H264,
                        2 => CodecPreference::Av1,
                        3 => CodecPreference::H265,
                        _ => return None,
                    },
                    can_decode_av1: match rest.get(2)? {
                        0 => false,
                        1 => true,
                        _ => return None,
                    },
                    // Not there from viewers before H.265.
                    can_decode_hevc: rest.get(3).copied().unwrap_or(0) & 0x0F,
                    chroma444: rest.get(4).is_some_and(|flags| flags & 1 != 0),
                    ten_bit: rest.get(4).is_some_and(|flags| flags & 2 != 0),
                },
                3 => Command::Restart,
                4 => Command::SoundOn,
                5 => Command::SoundOff,
                6 => Command::PrivacyOn,
                7 => Command::PrivacyOff,
                10 => Command::Resolution(u16::from_le_bytes(rest.get(1..3)?.try_into().ok()?)),
                11 => Command::Screen(*rest.get(1)?),
                17 => Command::DisplayMode {
                    width: u16::from_le_bytes(rest.get(1..3)?.try_into().ok()?),
                    height: u16::from_le_bytes(rest.get(3..5)?.try_into().ok()?),
                },
                18 => Command::SuperScreen {
                    width: u16::from_le_bytes(rest.get(1..3)?.try_into().ok()?),
                    height: u16::from_le_bytes(rest.get(3..5)?.try_into().ok()?),
                    refresh: *rest.get(5)?,
                    scale: u16::from_le_bytes(rest.get(6..8)?.try_into().ok()?),
                },
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
            PASSWORD_NEEDED => match rest {
                [reason, tries_left] => Message::PasswordNeeded {
                    reason: match reason {
                        1 => PasswordReason::Locked,
                        2 => PasswordReason::SignedOut,
                        3 => PasswordReason::OnlyTheOwner,
                        _ => return None,
                    },
                    tries_left: *tries_left,
                },
                _ => return None,
            },
            PASSWORD if rest.len() <= MAX_PASSWORD => {
                Message::Password(String::from_utf8(rest.to_vec()).ok()?)
            }
            PASSWORD_RESULT => match rest {
                [ok @ (0 | 1), tries_left] => Message::PasswordResult {
                    ok: *ok == 1,
                    tries_left: *tries_left,
                },
                _ => return None,
            },
            ENCODER => match rest {
                [gpu @ (0 | 1), picture @ ..] if picture.len() <= 64 => Message::Encoder {
                    picture: String::from_utf8(picture.to_vec()).ok()?,
                    gpu: *gpu == 1,
                },
                _ => return None,
            },
            SUPER_SCREEN_STATE => {
                let on = match rest.first()? {
                    0 => false,
                    1 => true,
                    _ => return None,
                };
                let size = (u16_at(1)?, u16_at(3)?);
                let note = rest.get(5..)?;
                if note.len() > 200 {
                    return None;
                }
                Message::SuperScreenState {
                    on,
                    size,
                    note: String::from_utf8(note.to_vec()).ok()?,
                }
            }
            DISPLAY_MODES => {
                let current = (u16_at(0)?, u16_at(2)?);
                let own = (u16_at(4)?, u16_at(6)?);
                let count = usize::from(*rest.get(8)?);
                let sizes_end = 9 + count * 4;
                if count > 24 || rest.len() < sizes_end || rest.len() - sizes_end > 200 {
                    return None;
                }
                let sizes = rest[9..sizes_end]
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
                Message::DisplayModes {
                    current,
                    own,
                    sizes,
                    note: String::from_utf8(rest[sizes_end..].to_vec()).ok()?,
                }
            }
            ANNOTATE => Message::Annotate(match rest {
                [1] => Annotation::On,
                [2, red, green, blue, width @ 1..=MAX_STROKE_WIDTH] => Annotation::Begin {
                    colour: u32::from_le_bytes([*blue, *green, *red, 0]),
                    width: *width,
                },
                [3, x0, x1, y0, y1] => Annotation::Point {
                    x: u16::from_le_bytes([*x0, *x1]),
                    y: u16::from_le_bytes([*y0, *y1]),
                },
                [4] => Annotation::End,
                [5] => Annotation::Clear,
                [6] => Annotation::Off,
                _ => return None,
            }),
            ANNOTATE_STATE => match rest {
                [shown @ (0 | 1), low, high, note @ ..] if note.len() <= 200 => {
                    Message::AnnotateState {
                        shown: *shown == 1,
                        strokes: u16::from_le_bytes([*low, *high]),
                        note: String::from_utf8(note.to_vec()).ok()?,
                    }
                }
                _ => return None,
            },
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_messages_round_trip_and_long_ones_are_refused() {
        for message in [
            Message::PasswordNeeded {
                reason: PasswordReason::Locked,
                tries_left: 5,
            },
            Message::PasswordNeeded {
                reason: PasswordReason::OnlyTheOwner,
                tries_left: 0,
            },
            Message::Password("pässwörd 1".into()),
            Message::PasswordResult {
                ok: false,
                tries_left: 3,
            },
            Message::Encoder {
                picture: "H.265 4:2:0".into(),
                gpu: true,
            },
        ] {
            assert_eq!(Message::decode(&message.encode()), Some(message));
        }
        let long = Message::Password("x".repeat(MAX_PASSWORD + 1)).encode();
        assert_eq!(Message::decode(&long), None);
        assert_eq!(Message::decode(&[PASSWORD_NEEDED, 9, 1]), None);
        assert_eq!(Message::decode(&[PASSWORD_RESULT, 2, 1]), None);
    }

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
            Message::TypeText("typed 中文, émoji 🙂\n".into()),
            Message::Command(Command::Lock),
            Message::Command(Command::RestartQueqiao),
            Message::Command(Command::Microphone(true)),
            Message::Command(Command::Microphone(false)),
            Message::Command(Command::LockAfter(true)),
            Message::Command(Command::DisplayMode {
                width: 2560,
                height: 1440,
            }),
            Message::Command(Command::DisplayMode {
                width: 0,
                height: 0,
            }),
            Message::Command(Command::SuperScreen {
                width: 3840,
                height: 2160,
                refresh: 60,
                scale: 200,
            }),
            Message::Command(Command::SuperScreen {
                width: 0,
                height: 0,
                refresh: 0,
                scale: 0,
            }),
            Message::SuperScreenState {
                on: true,
                size: (3840, 2160),
                note: String::new(),
            },
            Message::SuperScreenState {
                on: false,
                size: (0, 0),
                note: "Queqiao's display driver is not installed".into(),
            },
            Message::DisplayModes {
                current: (1920, 1080),
                own: (2560, 1440),
                sizes: vec![(2560, 1440), (1920, 1080), (1280, 720)],
                note: String::new(),
            },
            Message::DisplayModes {
                current: (1920, 1080),
                own: (1920, 1080),
                sizes: Vec::new(),
                note: "not on Wayland yet".into(),
            },
            Message::Command(Command::LockAfter(false)),
            Message::Command(Command::VideoCodec {
                prefer: CodecPreference::Av1,
                can_decode_av1: true,
                can_decode_hevc: 0,
                chroma444: false,
                ten_bit: false,
            }),
            Message::Command(Command::VideoCodec {
                prefer: CodecPreference::Automatic,
                can_decode_av1: false,
                can_decode_hevc: hevc::MAIN | hevc::MAIN10,
                chroma444: false,
                ten_bit: true,
            }),
            Message::Command(Command::VideoCodec {
                prefer: CodecPreference::H265,
                can_decode_av1: false,
                can_decode_hevc: hevc::MAIN | hevc::CHROMA444 | hevc::CHROMA444_10,
                chroma444: true,
                ten_bit: true,
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

    #[test]
    fn annotations_round_trip_and_bad_ones_are_refused() {
        for message in [
            Message::Annotate(Annotation::On),
            Message::Annotate(Annotation::Begin {
                colour: 0xE5_3935,
                width: 5,
            }),
            Message::Annotate(Annotation::Begin {
                colour: 0xFF_FFFF,
                width: MAX_STROKE_WIDTH,
            }),
            Message::Annotate(Annotation::Point { x: 0, y: 65535 }),
            Message::Annotate(Annotation::Point { x: 12345, y: 1 }),
            Message::Annotate(Annotation::End),
            Message::Annotate(Annotation::Clear),
            Message::Annotate(Annotation::Off),
            Message::AnnotateState {
                shown: true,
                strokes: 3,
                note: String::new(),
            },
            Message::AnnotateState {
                shown: false,
                strokes: 0,
                note: "not on Wayland yet".into(),
            },
        ] {
            assert_eq!(Message::decode(&message.encode()), Some(message));
        }
        // The tags others use stay theirs: drawing is 70 and 71.
        assert_eq!(Message::Annotate(Annotation::On).encode(), [70, 1]);
        assert_eq!(
            Message::AnnotateState {
                shown: true,
                strokes: 2,
                note: String::new()
            }
            .encode(),
            [71, 1, 2, 0]
        );
        // The colour goes as red, green, blue.
        assert_eq!(
            Message::Annotate(Annotation::Begin {
                colour: 0x11_2233,
                width: 7
            })
            .encode(),
            [70, 2, 0x11, 0x22, 0x33, 7]
        );
        // A width of 0 or above the most, an unknown step, a byte short or long, a long note: refused.
        assert_eq!(Message::decode(&[ANNOTATE, 2, 1, 2, 3, 0]), None);
        assert_eq!(
            Message::decode(&[ANNOTATE, 2, 1, 2, 3, MAX_STROKE_WIDTH + 1]),
            None
        );
        assert_eq!(Message::decode(&[ANNOTATE, 7]), None);
        assert_eq!(Message::decode(&[ANNOTATE]), None);
        assert_eq!(Message::decode(&[ANNOTATE, 3, 1, 2, 3]), None);
        assert_eq!(Message::decode(&[ANNOTATE, 4, 0]), None);
        assert_eq!(Message::decode(&[ANNOTATE_STATE, 2, 0, 0]), None);
        assert_eq!(Message::decode(&[ANNOTATE_STATE, 1, 0]), None);
        let mut long = vec![ANNOTATE_STATE, 0, 0, 0];
        long.extend([b'x'; 201]);
        assert_eq!(Message::decode(&long), None);
        // A width above the most is sent as the most, not dropped on the way.
        let wide = Message::Annotate(Annotation::Begin {
            colour: 0,
            width: 200,
        });
        assert_eq!(
            Message::decode(&wide.encode()),
            Some(Message::Annotate(Annotation::Begin {
                colour: 0,
                width: MAX_STROKE_WIDTH
            }))
        );
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
            Message::Annotate(Annotation::Begin {
                colour: 0xFF_00FF,
                width: 5,
            }),
            Message::Annotate(Annotation::Point { x: 300, y: 400 }),
            Message::AnnotateState {
                shown: true,
                strokes: 1,
                note: "note".into(),
            },
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
