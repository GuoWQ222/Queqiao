//! The local channels between `queqiao-service`, its desktop helper and the agent it starts.
//!
//! The service makes each channel from two anonymous pipes, one per direction (a synchronous pipe handle runs
//! one read or write at a time, so a handle each way lets both go at once), and hands the ends to the processes
//! it starts as inherited handles, named on their command line. Nothing else can open them: an anonymous pipe
//! has no name, and the service passes each end to exactly one process (`PROC_THREAD_ATTRIBUTE_HANDLE_LIST`).
//!
//! On the pipe each message is a 4-byte little-endian length and then the message: a kind byte and its fields.

mod messages;

pub use messages::{
    FromHelper, FromService, LogonIdentity, MAX_MESSAGE, MAX_TEXT, ToHelper, ToService,
};

use windows::Win32::Foundation::{
    CloseHandle, HANDLE, HANDLE_FLAG_INHERIT, HANDLE_FLAGS, SetHandleInformation,
};
use windows::Win32::Storage::FileSystem::{FILE_TYPE_PIPE, GetFileType, ReadFile, WriteFile};
use windows::Win32::System::Pipes::CreatePipe;

/// The pipes' buffer size.
const BUFFER: u32 = 1 << 20;

/// Where one process reads and writes its side of a channel.
pub struct Ends {
    pub read: HANDLE,
    pub write: HANDLE,
}

// Plain kernel handles: usable from any thread.
unsafe impl Send for Ends {}

impl Ends {
    /// From the command-line form `<read>,<write>` (the inherited handle values). Both must be pipes: a wrong
    /// number would name some other handle of this process.
    pub fn parse(text: &str) -> Option<Ends> {
        let (read, write) = text.split_once(',')?;
        let handle = |value: &str| -> Option<HANDLE> {
            let value = value.trim().parse::<usize>().ok().filter(|v| *v != 0)?;
            let handle = HANDLE(value as *mut core::ffi::c_void);
            (unsafe { GetFileType(handle) } == FILE_TYPE_PIPE).then_some(handle)
        };
        let (read, write) = (handle(read)?, handle(write)?);
        // Not `then_some(Ends { .. })`: that builds (and drops, closing the handles) even when refused.
        if read == write {
            return None;
        }
        Some(Ends { read, write })
    }

    /// The command-line form, for the process these ends are handed to.
    pub fn argument(&self) -> String {
        format!("{},{}", self.read.0 as usize, self.write.0 as usize)
    }

    /// Both handles, to list in the child's `PROC_THREAD_ATTRIBUTE_HANDLE_LIST`; marked inheritable (the list
    /// decides which inheritable handles a child gets).
    pub fn for_child(&self) -> Result<[HANDLE; 2], String> {
        for handle in [self.read, self.write] {
            unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT.0, HANDLE_FLAG_INHERIT) }
                .map_err(|e| format!("SetHandleInformation: {e}"))?;
        }
        Ok([self.read, self.write])
    }

    pub fn split(self) -> (Reader, Writer) {
        let ends = std::mem::ManuallyDrop::new(self);
        // Ours alone from now on: not to be inherited by anything this process starts.
        for handle in [ends.read, ends.write] {
            let _ = unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT.0, HANDLE_FLAGS(0)) };
        }
        (Reader(ends.read), Writer(ends.write))
    }
}

impl Drop for Ends {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.read);
            let _ = CloseHandle(self.write);
        }
    }
}

/// A channel: the ends of its two sides (this process and a child, or two children).
pub fn channel() -> Result<(Ends, Ends), String> {
    let (child_reads, we_write) = pipe()?;
    let (we_read, child_writes) = pipe()?;
    Ok((
        Ends {
            read: we_read,
            write: we_write,
        },
        Ends {
            read: child_reads,
            write: child_writes,
        },
    ))
}

fn pipe() -> Result<(HANDLE, HANDLE), String> {
    let (mut read, mut write) = (HANDLE::default(), HANDLE::default());
    unsafe { CreatePipe(&mut read, &mut write, None, BUFFER) }
        .map_err(|e| format!("CreatePipe: {e}"))?;
    Ok((read, write))
}

/// Reads messages.
pub struct Reader(HANDLE);
/// Writes messages.
pub struct Writer(HANDLE);

unsafe impl Send for Reader {}
unsafe impl Send for Writer {}

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
            let mut read = 0u32;
            unsafe { ReadFile(self.0, Some(&mut buffer[done..end]), Some(&mut read), None) }
                .map_err(|e| format!("the channel closed ({e})"))?;
            if read == 0 {
                return Err("the channel closed".into());
            }
            done += read as usize;
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
            let mut written = 0u32;
            unsafe { WriteFile(self.0, Some(&data[..end]), Some(&mut written), None) }
                .map_err(|e| format!("the channel closed ({e})"))?;
            if written == 0 {
                return Err("the channel closed".into());
            }
            data = &data[written as usize..];
        }
        Ok(())
    }
}

impl Drop for Reader {
    fn drop(&mut self) {
        let _ = unsafe { CloseHandle(self.0) };
    }
}

impl Drop for Writer {
    fn drop(&mut self) {
        let _ = unsafe { CloseHandle(self.0) };
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
        let same = format!("{0},{0}", ours.read.0 as usize);
        assert!(Ends::parse(&same).is_none());
    }
}
