//! Remote command lines over the session's "terminal" stream.
//!
//! - The controlled PC runs each command line in a pseudo console ([`pty::Pty`], ConPTY): PowerShell, or cmd
//!   where there is no PowerShell; [`Terminals`] keeps them by number and turns their output into messages.
//! - The controlling PC shows each in a console window of its own ([`window::TerminalWindow`]): a helper
//!   process (the viewer started again with `--terminal-pipe`) whose console renders the VT output and turns
//!   keys into VT input, connected to the viewer by two one-way named pipes. On Linux the window is the desktop's
//!   terminal emulator running that helper, connected by one Unix socket (window_unix.rs, helper_unix.rs).
//!
//! [`TerminalMessage`] is what goes between them; several command lines can be open at once.

#[cfg(windows)]
mod helper;
#[cfg(target_os = "linux")]
#[path = "helper_unix.rs"]
mod helper;
#[cfg(windows)]
mod pty;
#[cfg(target_os = "linux")]
#[path = "pty_unix.rs"]
mod pty;
mod terminals;
#[cfg(windows)]
mod window;
#[cfg(target_os = "linux")]
#[path = "window_unix.rs"]
mod window;

pub use helper::run_helper;
pub use pty::Pty;
pub use terminals::Terminals;
pub use window::{TerminalWindow, WindowEvent};

/// Output read from a command line goes out in pieces of at most this many bytes.
pub const CHUNK: usize = 16 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TerminalMessage {
    /// Viewer -> agent: start a command line with this window size (characters).
    Open {
        id: u32,
        cols: u16,
        rows: u16,
    },
    /// Viewer -> agent: keys (VT input, UTF-8).
    Input {
        id: u32,
        data: Vec<u8>,
    },
    Resize {
        id: u32,
        cols: u16,
        rows: u16,
    },
    /// Viewer -> agent: the window was closed; end the command line.
    Close {
        id: u32,
    },
    /// Agent -> viewer: the command line runs (`shell` names it).
    Opened {
        id: u32,
        shell: String,
    },
    /// Agent -> viewer: output (VT, UTF-8; a piece may end inside a character).
    Output {
        id: u32,
        data: Vec<u8>,
    },
    /// Agent -> viewer: the command line ended with this exit code.
    Exited {
        id: u32,
        code: u32,
    },
    /// Agent -> viewer: it could not be started, or terminals are not allowed there.
    Failed {
        id: u32,
        reason: String,
    },
}

const OPEN: u8 = 1;
const INPUT: u8 = 2;
const RESIZE: u8 = 3;
const CLOSE: u8 = 4;
const OPENED: u8 = 11;
const OUTPUT: u8 = 12;
const EXITED: u8 = 13;
const FAILED: u8 = 14;

impl TerminalMessage {
    pub fn encode(&self) -> Vec<u8> {
        let (tag, id) = match self {
            TerminalMessage::Open { id, .. } => (OPEN, id),
            TerminalMessage::Input { id, .. } => (INPUT, id),
            TerminalMessage::Resize { id, .. } => (RESIZE, id),
            TerminalMessage::Close { id } => (CLOSE, id),
            TerminalMessage::Opened { id, .. } => (OPENED, id),
            TerminalMessage::Output { id, .. } => (OUTPUT, id),
            TerminalMessage::Exited { id, .. } => (EXITED, id),
            TerminalMessage::Failed { id, .. } => (FAILED, id),
        };
        let mut out = vec![tag];
        out.extend_from_slice(&id.to_le_bytes());
        match self {
            TerminalMessage::Open { cols, rows, .. }
            | TerminalMessage::Resize { cols, rows, .. } => {
                out.extend_from_slice(&cols.to_le_bytes());
                out.extend_from_slice(&rows.to_le_bytes());
            }
            TerminalMessage::Input { data, .. } | TerminalMessage::Output { data, .. } => {
                out.extend_from_slice(data)
            }
            TerminalMessage::Opened { shell: text, .. }
            | TerminalMessage::Failed { reason: text, .. } => {
                out.extend_from_slice(text.as_bytes())
            }
            TerminalMessage::Exited { code, .. } => out.extend_from_slice(&code.to_le_bytes()),
            TerminalMessage::Close { .. } => {}
        }
        out
    }

    pub fn decode(bytes: &[u8]) -> Option<TerminalMessage> {
        let (&tag, rest) = bytes.split_first()?;
        let id = u32::from_le_bytes(rest.get(..4)?.try_into().ok()?);
        let body = &rest[4..];
        let size = || -> Option<(u16, u16)> {
            Some((
                u16::from_le_bytes(body.get(..2)?.try_into().ok()?),
                u16::from_le_bytes(body.get(2..4)?.try_into().ok()?),
            ))
        };
        let text = || String::from_utf8(body.to_vec()).ok();
        Some(match tag {
            OPEN => {
                let (cols, rows) = size()?;
                TerminalMessage::Open { id, cols, rows }
            }
            INPUT => TerminalMessage::Input {
                id,
                data: body.to_vec(),
            },
            RESIZE => {
                let (cols, rows) = size()?;
                TerminalMessage::Resize { id, cols, rows }
            }
            CLOSE => TerminalMessage::Close { id },
            OPENED => TerminalMessage::Opened { id, shell: text()? },
            OUTPUT => TerminalMessage::Output {
                id,
                data: body.to_vec(),
            },
            EXITED => TerminalMessage::Exited {
                id,
                code: u32::from_le_bytes(body.get(..4)?.try_into().ok()?),
            },
            FAILED => TerminalMessage::Failed {
                id,
                reason: text()?,
            },
            _ => return None,
        })
    }
}

/// Window sizes are kept to what a console can be.
pub fn clamp_size(cols: u16, rows: u16) -> (i16, i16) {
    (cols.clamp(20, 500) as i16, rows.clamp(5, 300) as i16)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_round_trip() {
        let messages = [
            TerminalMessage::Open {
                id: 1,
                cols: 120,
                rows: 30,
            },
            TerminalMessage::Input {
                id: 2,
                data: b"dir\r".to_vec(),
            },
            TerminalMessage::Resize {
                id: 3,
                cols: 80,
                rows: 25,
            },
            TerminalMessage::Close { id: 4 },
            TerminalMessage::Opened {
                id: 5,
                shell: "PowerShell".into(),
            },
            TerminalMessage::Output {
                id: 6,
                data: "ä 中文\x1b[0m".as_bytes().to_vec(),
            },
            TerminalMessage::Exited { id: 7, code: 259 },
            TerminalMessage::Failed {
                id: 8,
                reason: "not allowed".into(),
            },
        ];
        for message in messages {
            assert_eq!(TerminalMessage::decode(&message.encode()), Some(message));
        }
        assert_eq!(TerminalMessage::decode(&[]), None);
        assert_eq!(TerminalMessage::decode(&[OPEN, 1, 0, 0, 0, 80]), None);
        assert_eq!(TerminalMessage::decode(&[99, 1, 0, 0, 0]), None);
    }

    #[test]
    fn a_command_line_runs_and_ends() {
        // A real pseudo console: no window is involved, so this runs anywhere (also over SSH).
        let mut terminals = Terminals::new(true);
        let replies = terminals.handle(TerminalMessage::Open {
            id: 1,
            cols: 100,
            rows: 30,
        });
        let Some(TerminalMessage::Opened { shell, .. }) = replies.first() else {
            panic!("not opened: {replies:?}");
        };
        // The command as typed must not show the expected text itself (the console echoes it).
        let input = if shell == "PowerShell" {
            "Write-Output ('queqiao' + '-terminal-ok')\r"
        } else if shell == "cmd" {
            "echo queqiao^-terminal-ok\r"
        } else {
            // A POSIX shell (Linux).
            "echo queqiao-''terminal-ok\r"
        };
        terminals.handle(TerminalMessage::Input {
            id: 1,
            data: input.as_bytes().to_vec(),
        });
        let mut output = Vec::new();
        let mut exited = None;
        let until = std::time::Instant::now() + std::time::Duration::from_secs(30);
        let mut sent_exit = false;
        while exited.is_none() && std::time::Instant::now() < until {
            for message in terminals.poll(usize::MAX) {
                match message {
                    TerminalMessage::Output { data, .. } => output.extend_from_slice(&data),
                    TerminalMessage::Exited { code, .. } => exited = Some(code),
                    _ => {}
                }
            }
            let text = String::from_utf8_lossy(&output);
            if !sent_exit && text.contains("queqiao-terminal-ok") {
                sent_exit = true;
                terminals.handle(TerminalMessage::Input {
                    id: 1,
                    data: b"exit\r".to_vec(),
                });
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let text = String::from_utf8_lossy(&output);
        assert!(text.contains("queqiao-terminal-ok"), "output: {text}");
        assert_eq!(exited, Some(0), "output: {text}");
        assert!(terminals.is_empty());
    }

    #[test]
    fn terminals_can_be_refused() {
        let mut terminals = Terminals::new(false);
        let replies = terminals.handle(TerminalMessage::Open {
            id: 1,
            cols: 80,
            rows: 25,
        });
        assert!(matches!(
            replies.as_slice(),
            [TerminalMessage::Failed { id: 1, .. }]
        ));
        assert!(terminals.is_empty());
    }
}

/// Robustness: messages from the other side never panic the decoder, and what is accepted encodes back.
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
    }

    #[test]
    fn random_messages_never_panic() {
        let mut random = Random(0x0123_4567_89AB_CDEF);
        for _ in 0..200_000 {
            let length = random.below(24);
            let mut bytes: Vec<u8> = (0..length).map(|_| random.next() as u8).collect();
            // Often a known tag, so the bodies get exercised too.
            if !bytes.is_empty() && random.below(2) == 0 {
                bytes[0] =
                    [OPEN, INPUT, RESIZE, CLOSE, OPENED, OUTPUT, EXITED, FAILED][random.below(8)];
            }
            if let Some(message) = TerminalMessage::decode(&bytes) {
                assert_eq!(TerminalMessage::decode(&message.encode()), Some(message));
            }
        }
    }
}
