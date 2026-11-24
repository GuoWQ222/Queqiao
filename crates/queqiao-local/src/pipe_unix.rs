//! The channel's pipes on Linux: made close-on-exec, so nothing this process starts inherits them by chance. The
//! launcher hands a child its two ends by moving them, after fork and in the child alone, to the descriptor numbers
//! named on its command line (`dup2` there leaves the copies open across exec): the counterpart of Windows' handle
//! list.

use std::fs::File;
use std::io::{ErrorKind, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};

use super::{BUFFER, Reader, Writer};

/// Where one process reads and writes its side of a channel.
pub struct Ends {
    pub read: OwnedFd,
    pub write: OwnedFd,
}

impl Ends {
    /// From the command-line form `<read>,<write>` (descriptors this process inherited). Both must be pipes and
    /// neither standard input, output nor error: a wrong number would name some other file of this process.
    pub fn parse(text: &str) -> Option<Ends> {
        let (read, write) = text.split_once(',')?;
        let descriptor = |value: &str| -> Option<RawFd> {
            let value = value.trim().parse::<RawFd>().ok().filter(|v| *v > 2)?;
            is_pipe(value).then_some(value)
        };
        let (read, write) = (descriptor(read)?, descriptor(write)?);
        if read == write {
            return None;
        }
        // Ours from here on: closed when the ends are dropped.
        Some(unsafe {
            Ends {
                read: OwnedFd::from_raw_fd(read),
                write: OwnedFd::from_raw_fd(write),
            }
        })
    }

    /// The command-line form, for the process these ends are handed to (after the launcher moved them there, the
    /// numbers the child sees).
    pub fn argument(&self) -> String {
        format!("{},{}", self.read.as_raw_fd(), self.write.as_raw_fd())
    }

    /// Both descriptors, for the launcher to move to the child's numbers after fork (`dup2` in the child alone).
    pub fn for_child(&self) -> Result<[RawFd; 2], String> {
        Ok([self.read.as_raw_fd(), self.write.as_raw_fd()])
    }

    pub fn split(self) -> (Reader, Writer) {
        // Ours alone from now on: an end this process inherited came without close-on-exec.
        for descriptor in [self.read.as_raw_fd(), self.write.as_raw_fd()] {
            unsafe { libc::fcntl(descriptor, libc::F_SETFD, libc::FD_CLOEXEC) };
        }
        (
            Reader(Pipe(File::from(self.read))),
            Writer(Pipe(File::from(self.write))),
        )
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

fn pipe() -> Result<(OwnedFd, OwnedFd), String> {
    let mut descriptors = [0 as libc::c_int; 2];
    if unsafe { libc::pipe2(descriptors.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(format!("pipe2: {}", std::io::Error::last_os_error()));
    }
    // 1 MiB as on Windows, where the system allows it (an unprivileged process may be held to pipe-max-size).
    unsafe { libc::fcntl(descriptors[1], libc::F_SETPIPE_SZ, BUFFER as libc::c_int) };
    Ok(unsafe {
        (
            OwnedFd::from_raw_fd(descriptors[0]),
            OwnedFd::from_raw_fd(descriptors[1]),
        )
    })
}

fn is_pipe(descriptor: RawFd) -> bool {
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    let found = unsafe { libc::fstat(descriptor, &mut stat) } == 0;
    found && stat.st_mode & libc::S_IFMT == libc::S_IFIFO
}

/// One end of a pipe, closed when dropped.
pub(crate) struct Pipe(File);

impl Pipe {
    pub(crate) fn read_some(&mut self, buffer: &mut [u8]) -> Result<usize, String> {
        loop {
            match self.0.read(buffer) {
                Ok(read) => return Ok(read),
                Err(error) if error.kind() == ErrorKind::Interrupted => {}
                Err(error) => return Err(format!("the channel closed ({error})")),
            }
        }
    }

    pub(crate) fn write_some(&mut self, data: &[u8]) -> Result<usize, String> {
        loop {
            match self.0.write(data) {
                Ok(written) => return Ok(written),
                Err(error) if error.kind() == ErrorKind::Interrupted => {}
                // The reader gone: EPIPE (Rust programs ignore SIGPIPE, so the write fails instead).
                Err(error) => return Err(format!("the channel closed ({error})")),
            }
        }
    }
}
