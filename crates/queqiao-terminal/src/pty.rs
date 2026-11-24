//! The controlled PC's side: command lines in pseudo consoles (ConPTY, Windows 10 version 1809 and later).

use std::ffi::c_void;
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};

use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows::Win32::Storage::FileSystem::{ReadFile, WriteFile};
use windows::Win32::System::Console::{
    COORD, ClosePseudoConsole, CreatePseudoConsole, HPCON, ResizePseudoConsole,
};
use windows::Win32::System::Pipes::CreatePipe;
use windows::Win32::System::Threading::{
    DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT, GetExitCodeProcess,
    InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST, PROCESS_INFORMATION,
    STARTF_USESTDHANDLES, STARTUPINFOEXW, TerminateProcess, UpdateProcThreadAttribute,
    WaitForSingleObject,
};
use windows::core::{PCWSTR, PWSTR};

use crate::{CHUNK, clamp_size};

/// PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE.
const PSEUDOCONSOLE_ATTRIBUTE: usize = 0x0002_0016;
/// Pieces of output waiting to be sent, per command line; beyond this the reader waits, and so does the
/// program writing (no unbounded memory when the network is slow).
const WAITING_PIECES: usize = 64;

/// A handle that may move to another thread.
struct Owned(HANDLE);

unsafe impl Send for Owned {}

impl Drop for Owned {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            let _ = unsafe { CloseHandle(self.0) };
        }
    }
}

/// One command line in a pseudo console.
pub struct Pty {
    console: HPCON,
    input: Owned,
    process: Owned,
    output: Receiver<Vec<u8>>,
    pub shell: &'static str,
}

impl Pty {
    /// PowerShell (or cmd where there is none) in the user's profile folder.
    pub fn spawn(cols: u16, rows: u16) -> Result<Pty, String> {
        let windows_folder = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
        let powershell =
            format!(r"{windows_folder}\System32\WindowsPowerShell\v1.0\powershell.exe");
        let (shell, command) = if std::path::Path::new(&powershell).exists() {
            ("PowerShell", format!("\"{powershell}\" -NoLogo"))
        } else {
            ("cmd", format!(r"{windows_folder}\System32\cmd.exe"))
        };
        let (cols, rows) = clamp_size(cols, rows);
        // Pipes: we write keys into one and read output from the other; the pseudo console holds the far ends.
        let (mut input_read, mut input_write) = (HANDLE::default(), HANDLE::default());
        let (mut output_read, mut output_write) = (HANDLE::default(), HANDLE::default());
        unsafe { CreatePipe(&mut input_read, &mut input_write, None, 0) }
            .map_err(|e| e.message())?;
        let (input_read, input_write) = (Owned(input_read), Owned(input_write));
        unsafe { CreatePipe(&mut output_read, &mut output_write, None, 0) }
            .map_err(|e| e.message())?;
        let (output_read, output_write) = (Owned(output_read), Owned(output_write));
        let console = unsafe {
            CreatePseudoConsole(COORD { X: cols, Y: rows }, input_read.0, output_write.0, 0)
        }
        .map_err(|e| {
            format!(
                "CreatePseudoConsole: {} (needs Windows 10 version 1809 or later)",
                e.message()
            )
        })?;
        // The pseudo console has its own copies now.
        drop((input_read, output_write));
        let process = match start_in(console, &command) {
            Ok(process) => process,
            Err(error) => {
                unsafe { ClosePseudoConsole(console) };
                return Err(error);
            }
        };
        let (sender, output) = mpsc::sync_channel(WAITING_PIECES);
        std::thread::Builder::new()
            .name("queqiao-terminal-read".into())
            .spawn(move || read_output(output_read, sender))
            .map_err(|e| e.to_string())?;
        Ok(Pty {
            console,
            input: input_write,
            process,
            output,
            shell,
        })
    }

    pub fn write(&self, data: &[u8]) -> Result<(), String> {
        let mut written = 0u32;
        unsafe { WriteFile(self.input.0, Some(data), Some(&mut written), None) }
            .map_err(|e| e.message())
    }

    pub fn resize(&self, cols: u16, rows: u16) -> Result<(), String> {
        let (cols, rows) = clamp_size(cols, rows);
        unsafe { ResizePseudoConsole(self.console, COORD { X: cols, Y: rows }) }
            .map_err(|e| e.message())
    }

    /// Output read since the last call, at most about `limit` bytes.
    pub fn take_output(&self, limit: usize) -> Vec<Vec<u8>> {
        let mut pieces = Vec::new();
        let mut taken = 0;
        while taken < limit {
            match self.output.try_recv() {
                Ok(piece) => {
                    taken += piece.len();
                    pieces.push(piece);
                }
                Err(_) => break,
            }
        }
        pieces
    }

    /// The exit code once the program has ended.
    pub fn exit_code(&self) -> Option<u32> {
        if unsafe { WaitForSingleObject(self.process.0, 0) } != WAIT_OBJECT_0 {
            return None;
        }
        let mut code = 0u32;
        unsafe { GetExitCodeProcess(self.process.0, &mut code) }.ok()?;
        Some(code)
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        // Closing the pseudo console ends the program in it; the reader thread keeps draining the output
        // meanwhile (ClosePseudoConsole can wait for that) and ends at the end of the pipe.
        unsafe { ClosePseudoConsole(self.console) };
        if self.exit_code().is_none() {
            let _ = unsafe { TerminateProcess(self.process.0, 1) };
        }
    }
}

fn start_in(console: HPCON, command: &str) -> Result<Owned, String> {
    let mut size = 0usize;
    // The first call only says how much room the attribute list needs.
    let _ = unsafe { InitializeProcThreadAttributeList(None, 1, None, &mut size) };
    let mut list_memory = vec![0u8; size];
    let list = LPPROC_THREAD_ATTRIBUTE_LIST(list_memory.as_mut_ptr().cast());
    unsafe { InitializeProcThreadAttributeList(Some(list), 1, None, &mut size) }
        .map_err(|e| e.message())?;
    let result = (|| {
        unsafe {
            UpdateProcThreadAttribute(
                list,
                0,
                PSEUDOCONSOLE_ATTRIBUTE,
                Some(console.0 as *const c_void),
                size_of::<HPCON>(),
                None,
                None,
            )
        }
        .map_err(|e| e.message())?;
        let mut startup = STARTUPINFOEXW::default();
        startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
        // No standard handles of ours: the program must use the pseudo console only.
        startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        startup.lpAttributeList = list;
        let mut command_line: Vec<u16> = command.encode_utf16().chain(Some(0)).collect();
        let folder = std::env::var("USERPROFILE").unwrap_or_else(|_| r"C:\".into());
        let folder: Vec<u16> = folder.encode_utf16().chain(Some(0)).collect();
        let mut information = PROCESS_INFORMATION::default();
        unsafe {
            // At Medium integrity when the agent runs raised (the person's programs' level).
            queqiao_core::platform::create_process(
                PWSTR(command_line.as_mut_ptr()),
                false,
                EXTENDED_STARTUPINFO_PRESENT,
                PCWSTR(folder.as_ptr()),
                &startup.StartupInfo,
                &mut information,
            )
        }
        .map_err(|e| format!("starting {command}: {}", e.message()))?;
        let _ = unsafe { CloseHandle(information.hThread) };
        Ok(Owned(information.hProcess))
    })();
    unsafe { DeleteProcThreadAttributeList(list) };
    result
}

fn read_output(pipe: Owned, sender: SyncSender<Vec<u8>>) {
    let mut buffer = vec![0u8; CHUNK];
    loop {
        let mut read = 0u32;
        let result = unsafe { ReadFile(pipe.0, Some(&mut buffer), Some(&mut read), None) };
        if result.is_err() || read == 0 {
            return;
        }
        let mut piece = buffer[..read as usize].to_vec();
        // Waits while the session has not sent the earlier pieces.
        loop {
            match sender.try_send(piece) {
                Ok(()) => break,
                Err(TrySendError::Full(back)) => {
                    piece = back;
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                Err(TrySendError::Disconnected(_)) => return,
            }
        }
    }
}
