//! The channel's pipes on Windows: anonymous pipes, their ends handed to a child as inherited handles that only it
//! gets (`PROC_THREAD_ATTRIBUTE_HANDLE_LIST`, set by the service's launcher).

use windows::Win32::Foundation::{
    CloseHandle, HANDLE, HANDLE_FLAG_INHERIT, HANDLE_FLAGS, SetHandleInformation,
};
use windows::Win32::Storage::FileSystem::{FILE_TYPE_PIPE, GetFileType, ReadFile, WriteFile};
use windows::Win32::System::Pipes::CreatePipe;

use super::{BUFFER, Reader, Writer};

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
        (Reader(Pipe(ends.read)), Writer(Pipe(ends.write)))
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

/// One end of a pipe, closed when dropped.
pub(crate) struct Pipe(HANDLE);

// A plain kernel handle: usable from any thread.
unsafe impl Send for Pipe {}

impl Pipe {
    pub(crate) fn read_some(&mut self, buffer: &mut [u8]) -> Result<usize, String> {
        let mut read = 0u32;
        unsafe { ReadFile(self.0, Some(buffer), Some(&mut read), None) }
            .map_err(|e| format!("the channel closed ({e})"))?;
        Ok(read as usize)
    }

    pub(crate) fn write_some(&mut self, data: &[u8]) -> Result<usize, String> {
        let mut written = 0u32;
        unsafe { WriteFile(self.0, Some(data), Some(&mut written), None) }
            .map_err(|e| format!("the channel closed ({e})"))?;
        Ok(written as usize)
    }
}

impl Drop for Pipe {
    fn drop(&mut self) {
        let _ = unsafe { CloseHandle(self.0) };
    }
}
