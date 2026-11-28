//! The local channels between `queqiao-service`, its desktop helper and the agent it starts.
//!
//! The service makes each channel from two anonymous pipes, one per direction (a synchronous pipe handle runs
//! one read or write at a time, so a handle each way lets both go at once), and hands the ends to the processes
//! it starts as inherited handles, named on their command line. Nothing else can open them: an anonymous pipe
//! has no name, and the service passes each end to exactly one process (`PROC_THREAD_ATTRIBUTE_HANDLE_LIST`).
//! On Linux the pipes are made close-on-exec and the launcher moves the two ends to the child's own descriptors
//! after fork, so again each end reaches exactly one process (`pipe_unix.rs`).
//!
//! On the pipe each message is a 4-byte little-endian length and then the message: a kind byte and its fields.

mod messages;
#[cfg(unix)]
#[path = "pipe_unix.rs"]
mod pipe;
#[cfg(windows)]
#[path = "pipe_windows.rs"]
mod pipe;

pub use messages::{
    FromHelper, FromService, LogonIdentity, MAX_MESSAGE, MAX_TEXT, MAX_UPDATE_INSTALLER,
    MAX_UPDATE_MANIFEST, ToHelper, ToService,
};
pub use pipe::{Ends, channel};

/// The service's own control code (Windows: a user-defined service control, 128-255): "start Queqiao at the screen
/// now". A Queqiao started by hand sends it (its shortcut, also after Quit), and the service's permissions let
/// signed-in users send it (Interactive users have SERVICE_USER_DEFINED_CONTROL by default); it only starts the
/// service's own Queqiao where the service would start it anyway. 2026-11-27: a click on the shortcut did nothing.
///
/// Any interactive user may send a control, and a control does not say who sent it: the service honours it only while
/// [`START_REQUEST`] exists in the namespace of the session at the screen (`Session\<id>\<name>`), which only that
/// session's programs (and SYSTEM) can make objects in: an administrator in another session is refused. Someone else
/// signed in (Remote Desktop) cannot bring back the Queqiao the person at the screen quit (the security check of
/// 2026-11-28).
pub const CONTROL_START: u32 = 200;

/// The event a Queqiao started by hand holds in its own session (`Local\` + this) while it asks for
/// [`CONTROL_START`].
pub const START_REQUEST: &str = "QueqiaoStartRequest";

use pipe::Pipe;

/// The pipes' buffer size.
const BUFFER: u32 = 1 << 20;

/// Reads messages.
pub struct Reader(Pipe);
/// Writes messages.
pub struct Writer(Pipe);

impl Reader {
    /// The next message; an error once the other side is gone or sends more than `MAX_MESSAGE`.
    pub fn read(&mut self) -> Result<Vec<u8>, String> {
        let mut length = [0u8; 4];
        self.fill(&mut length)?;
        let length = u32::from_le_bytes(length) as usize;
        if length > MAX_MESSAGE {
            return Err(format!("a message of {length} bytes is too long"));
        }
        let mut message = vec![0u8; length];
        self.fill(&mut message)?;
        Ok(message)
    }

    fn fill(&mut self, buffer: &mut [u8]) -> Result<(), String> {
        let mut done = 0;
        while done < buffer.len() {
            let end = buffer.len().min(done + BUFFER as usize);
            let read = self.0.read_some(&mut buffer[done..end])?;
            if read == 0 {
                return Err("the channel closed".into());
            }
            done += read;
        }
        Ok(())
    }
}

impl Writer {
    pub fn write(&mut self, message: &[u8]) -> Result<(), String> {
        if message.len() > MAX_MESSAGE {
            return Err(format!("a message of {} bytes is too long", message.len()));
        }
        self.all(&(message.len() as u32).to_le_bytes())?;
        self.all(message)
    }

    fn all(&mut self, mut data: &[u8]) -> Result<(), String> {
        while !data.is_empty() {
            let end = data.len().min(BUFFER as usize);
            let written = self.0.write_some(&data[..end])?;
            if written == 0 {
                return Err("the channel closed".into());
            }
            data = &data[written..];
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_cross_a_channel_both_ways_at_once() {
        let (ours, theirs) = channel().unwrap();
        let (mut our_reader, mut our_writer) = ours.split();
        let (mut their_reader, mut their_writer) = theirs.split();
        // A big message one way while a small one goes the other way.
        let big = vec![7u8; 9 << 20];
        let sent = big.clone();
        let writer = std::thread::spawn(move || our_writer.write(&sent));
        their_writer.write(b"hello").unwrap();
        assert_eq!(our_reader.read().unwrap(), b"hello");
        assert_eq!(their_reader.read().unwrap(), big);
        writer.join().unwrap().unwrap();
        // The other side gone: reading ends with an error.
        drop(their_writer);
        assert!(our_reader.read().is_err());
    }

    #[test]
    fn too_long_is_refused_both_ways() {
        let (ours, theirs) = channel().unwrap();
        let (_, mut our_writer) = ours.split();
        let (mut their_reader, _their_writer) = theirs.split();
        assert!(our_writer.write(&vec![0u8; MAX_MESSAGE + 1]).is_err());
        // A length above the limit: refused before anything is allocated.
        our_writer
            .all(&(MAX_MESSAGE as u32 + 1).to_le_bytes())
            .unwrap();
        assert!(their_reader.read().is_err());
    }

    #[test]
    fn parse_wants_two_pipes() {
        let (ours, _theirs) = channel().unwrap();
        let text = ours.argument();
        let parsed = Ends::parse(&text).expect("pipes");
        std::mem::forget(parsed); // the same handles as `ours`
        assert!(Ends::parse("0,0").is_none());
        assert!(Ends::parse("12").is_none());
        assert!(Ends::parse("a,b").is_none());
        let first = text.split(',').next().unwrap();
        assert!(Ends::parse(&format!("{first},{first}")).is_none());
    }
}
