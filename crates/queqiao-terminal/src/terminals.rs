//! The command lines of one session on the controlled PC, on every system (docs/LINUX.md): opened, written to,
//! resized and closed as the viewer says, their output sent in turn; each runs in a [`Pty`] (ConPTY on Windows, a
//! pseudo terminal on Linux). Moved from pty.rs.

use std::collections::BTreeMap;

use crate::TerminalMessage;
use crate::pty::Pty;

/// Command lines open at once, at most.
const MAX_OPEN: usize = 8;

/// The command lines of one session, by the viewer's numbers.
pub struct Terminals {
    allowed: bool,
    open: BTreeMap<u32, Pty>,
}

impl Terminals {
    /// With `allowed` false (the person at this PC turned terminals off) every Open is refused.
    pub fn new(allowed: bool) -> Terminals {
        Terminals {
            allowed,
            open: BTreeMap::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.open.is_empty()
    }

    pub fn len(&self) -> usize {
        self.open.len()
    }

    /// A message from the viewer; returns the replies.
    pub fn handle(&mut self, message: TerminalMessage) -> Vec<TerminalMessage> {
        match message {
            TerminalMessage::Open { id, cols, rows } => {
                let refused = if !self.allowed {
                    Some("the person at that PC does not allow remote command lines".to_string())
                } else if self.open.len() >= MAX_OPEN {
                    Some(format!("at most {MAX_OPEN} command lines at once"))
                } else if self.open.contains_key(&id) {
                    Some(format!("command line {id} is open already"))
                } else {
                    None
                };
                if let Some(reason) = refused {
                    return vec![TerminalMessage::Failed { id, reason }];
                }
                match Pty::spawn(cols, rows) {
                    Ok(pty) => {
                        let shell = pty.shell.to_string();
                        self.open.insert(id, pty);
                        vec![TerminalMessage::Opened { id, shell }]
                    }
                    Err(reason) => vec![TerminalMessage::Failed { id, reason }],
                }
            }
            TerminalMessage::Input { id, data } => {
                match self.open.get(&id).map(|pty| pty.write(&data)) {
                    Some(Err(reason)) => vec![TerminalMessage::Failed { id, reason }],
                    _ => Vec::new(),
                }
            }
            TerminalMessage::Resize { id, cols, rows } => {
                if let Some(pty) = self.open.get(&id) {
                    let _ = pty.resize(cols, rows);
                }
                Vec::new()
            }
            TerminalMessage::Close { id } => {
                self.open.remove(&id);
                Vec::new()
            }
            // Messages for the viewer are not ours.
            _ => Vec::new(),
        }
    }

    /// Output and endings to send, at most about `room` bytes of output.
    pub fn poll(&mut self, room: usize) -> Vec<TerminalMessage> {
        let mut messages = Vec::new();
        let mut left = room;
        let mut ended = Vec::new();
        for (&id, pty) in &self.open {
            for data in pty.take_output(left) {
                left = left.saturating_sub(data.len());
                messages.push(TerminalMessage::Output { id, data });
            }
            // Ended, and all its output taken: the last output goes before the ending.
            if let Some(code) = pty.exit_code()
                && left > 0
            {
                let rest = pty.take_output(usize::MAX);
                let drained = rest.is_empty();
                messages.extend(
                    rest.into_iter()
                        .map(|data| TerminalMessage::Output { id, data }),
                );
                if drained {
                    ended.push((id, code));
                }
            }
        }
        for (id, code) in ended {
            self.open.remove(&id);
            messages.push(TerminalMessage::Exited { id, code });
        }
        messages
    }
}
