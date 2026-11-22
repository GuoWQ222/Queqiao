//! The controlling PC's side: a console window per command line. The window is a helper process (the viewer
//! started again with `--terminal-pipe`) in a console of its own; it talks to the viewer over two one-way named
//! pipes (a blocking read and a blocking write on one synchronous pipe handle would wait for each other).
//!
//! The pipes take one client each, never from another PC, and the viewer checks that the client is the helper
//! it started (by process ID) before it writes anything into them.

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

use windows::Win32::Foundation::{
    CloseHandle, ERROR_PIPE_CONNECTED, GENERIC_READ, GENERIC_WRITE, HANDLE,
};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_NONE,
    OPEN_EXISTING, PIPE_ACCESS_INBOUND, PIPE_ACCESS_OUTBOUND, ReadFile, WriteFile,
};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, GetNamedPipeClientProcessId, PIPE_READMODE_BYTE,
    PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT,
};
use windows::Win32::System::Threading::{
    CREATE_NEW_CONSOLE, GetExitCodeProcess, PROCESS_INFORMATION, STARTUPINFOW, WaitForSingleObject,
};
use windows::core::{PCWSTR, PWSTR};

use windows::Win32::Foundation::WAIT_OBJECT_0;

/// Pipe frames: [kind][length, u32 little-endian][bytes].
pub(crate) const FRAME_INPUT: u8 = 1;
pub(crate) const FRAME_RESIZE: u8 = 2;
pub(crate) const FRAME_OUTPUT: u8 = 3;
pub(crate) const FRAME_END: u8 = 4;
pub(crate) const FRAME_TITLE: u8 = 5;
pub(crate) const FRAME_DETACH: u8 = 6;
/// The helper must connect within this time.
const CONNECT_LIMIT: Duration = Duration::from_secs(15);

pub enum WindowEvent {
    /// Keys typed in the window (VT input, UTF-8).
    Input(Vec<u8>),
    /// The window's size in characters (also sent once at the start).
    Resize(u16, u16),
    /// The window was closed (or never came up).
    Closed,
}

pub(crate) struct Owned(HANDLE);

unsafe impl Send for Owned {}
unsafe impl Sync for Owned {}

impl Drop for Owned {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            let _ = unsafe { CloseHandle(self.0) };
        }
    }
}

pub struct TerminalWindow {
    events: Receiver<WindowEvent>,
    frames: Option<Sender<Vec<u8>>>,
    helper: Owned,
}

impl TerminalWindow {
    /// Opens a console window for one command line: `program` (this viewer) is started with `--terminal-pipe`.
    /// With `test`, the helper checks itself and exits 0 when output and the end both reached it.
    pub fn open(program: &Path, title: &str, test: bool) -> Result<TerminalWindow, String> {
        let random = RandomState::new().build_hasher().finish();
        let name = format!(
            r"\\.\pipe\queqiao-terminal-{}-{random:016x}",
            std::process::id()
        );
        let to_viewer = create_pipe(&format!("{name}-in"), PIPE_ACCESS_INBOUND)?;
        let to_helper = create_pipe(&format!("{name}-out"), PIPE_ACCESS_OUTBOUND)?;
        let title: String = title
            .chars()
            .filter(|c| *c != '"' && !c.is_control())
            .take(100)
            .collect();
        let mut command = format!(
            "\"{}\" --terminal-pipe {name} --terminal-title \"{title}\"",
            program.display()
        );
        if test {
            command.push_str(" --terminal-test");
        }
        let mut command_line: Vec<u16> = command.encode_utf16().chain(Some(0)).collect();
        let startup = STARTUPINFOW {
            cb: size_of::<STARTUPINFOW>() as u32,
            ..Default::default()
        };
        let mut information = PROCESS_INFORMATION::default();
        unsafe {
            // At Medium integrity when the agent runs raised (the person's programs' level).
            queqiao_core::platform::create_process(
                PWSTR(command_line.as_mut_ptr()),
                false,
                CREATE_NEW_CONSOLE,
                PCWSTR::null(),
                &startup,
                &mut information,
            )
        }
        .map_err(|e| format!("starting the terminal window: {}", e.message()))?;
        let _ = unsafe { CloseHandle(information.hThread) };
        let helper = Owned(information.hProcess);
        let helper_id = information.dwProcessId;

        let (event_sender, events) = mpsc::channel();
        let (frames, frame_receiver) = mpsc::channel::<Vec<u8>>();
        let connected = Arc::new(AtomicBool::new(false));
        // Without a client, ConnectNamedPipe waits for ever: after the limit, connect to the pipes ourselves so
        // it returns (and the process check then fails).
        {
            let (connected, name) = (connected.clone(), name.clone());
            std::thread::spawn(move || {
                std::thread::sleep(CONNECT_LIMIT);
                if !connected.load(Ordering::Relaxed) {
                    for (suffix, access) in [("-in", GENERIC_WRITE.0), ("-out", GENERIC_READ.0)] {
                        let _ = open_pipe(&format!("{name}{suffix}"), access);
                    }
                }
            });
        }
        std::thread::Builder::new()
            .name("queqiao-terminal-window".into())
            .spawn(move || {
                let ready = accept(&to_viewer, helper_id) && accept(&to_helper, helper_id);
                connected.store(true, Ordering::Relaxed);
                if !ready {
                    let _ = event_sender.send(WindowEvent::Closed);
                    return;
                }
                std::thread::spawn(move || {
                    for frame in frame_receiver {
                        if write_all(&to_helper, &frame).is_err() {
                            return;
                        }
                    }
                });
                read_frames(&to_viewer, &event_sender);
                let _ = event_sender.send(WindowEvent::Closed);
            })
            .map_err(|e| e.to_string())?;
        Ok(TerminalWindow {
            events,
            frames: Some(frames),
            helper,
        })
    }

    pub fn try_event(&self) -> Option<WindowEvent> {
        self.events.try_recv().ok()
    }

    /// Output of the command line, to show.
    pub fn output(&self, data: &[u8]) {
        self.send(FRAME_OUTPUT, data);
    }

    /// A new title for the window.
    pub fn set_title(&self, title: &str) {
        self.send(FRAME_TITLE, title.as_bytes());
    }

    /// Closes the window without ending the command line (the viewer keeps its output until it is shown again).
    pub fn detach(mut self) {
        self.send(FRAME_DETACH, &[]);
        self.frames = None;
    }

    /// The command line ended (or the session did): the window shows `text` and closes by itself.
    pub fn end(&mut self, text: &str) {
        self.send(FRAME_END, text.as_bytes());
        self.frames = None;
    }

    /// The helper's exit code once it has ended.
    pub fn helper_exit_code(&self) -> Option<u32> {
        if unsafe { WaitForSingleObject(self.helper.0, 0) } != WAIT_OBJECT_0 {
            return None;
        }
        let mut code = 0u32;
        unsafe { GetExitCodeProcess(self.helper.0, &mut code) }.ok()?;
        Some(code)
    }

    fn send(&self, kind: u8, data: &[u8]) {
        if let Some(frames) = &self.frames {
            let _ = frames.send(frame(kind, data));
        }
    }
}

impl Drop for TerminalWindow {
    /// The window shows that the session ended and closes by itself (one whose helper never connected is told
    /// by the pipes and closes too).
    fn drop(&mut self) {
        if self.frames.is_some() {
            self.end(queqiao_core::text::tr("The session has ended."));
        }
    }
}

pub(crate) fn frame(kind: u8, data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(5 + data.len());
    out.push(kind);
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(data);
    out
}

fn create_pipe(name: &str, direction: FILE_FLAGS_AND_ATTRIBUTES) -> Result<Owned, String> {
    let wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
    let pipe = unsafe {
        CreateNamedPipeW(
            PCWSTR(wide.as_ptr()),
            direction | FILE_FLAG_FIRST_PIPE_INSTANCE,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            1,
            64 * 1024,
            64 * 1024,
            0,
            None,
        )
    };
    if pipe.is_invalid() {
        return Err(format!(
            "creating a pipe: {}",
            windows::core::Error::from_thread().message()
        ));
    }
    Ok(Owned(pipe))
}

pub(crate) fn open_pipe(name: &str, access: u32) -> Result<Owned, String> {
    let wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
    unsafe {
        CreateFileW(
            PCWSTR(wide.as_ptr()),
            access,
            FILE_SHARE_NONE,
            None,
            OPEN_EXISTING,
            FILE_FLAGS_AND_ATTRIBUTES(0),
            None,
        )
    }
    .map(Owned)
    .map_err(|e| e.message())
}

/// Waits for the pipe's client and checks it is the helper we started.
fn accept(pipe: &Owned, helper_id: u32) -> bool {
    if let Err(error) = unsafe { ConnectNamedPipe(pipe.0, None) }
        && error.code() != ERROR_PIPE_CONNECTED.to_hresult()
    {
        return false;
    }
    let mut client = 0u32;
    unsafe { GetNamedPipeClientProcessId(pipe.0, &mut client) }.is_ok() && client == helper_id
}

pub(crate) fn write_all(pipe: &Owned, mut data: &[u8]) -> Result<(), ()> {
    while !data.is_empty() {
        let mut written = 0u32;
        unsafe { WriteFile(pipe.0, Some(data), Some(&mut written), None) }.map_err(|_| ())?;
        if written == 0 {
            return Err(());
        }
        data = &data[written as usize..];
    }
    Ok(())
}

pub(crate) fn read_exact(pipe: &Owned, out: &mut [u8]) -> Result<(), ()> {
    let mut filled = 0;
    while filled < out.len() {
        let mut read = 0u32;
        unsafe { ReadFile(pipe.0, Some(&mut out[filled..]), Some(&mut read), None) }
            .map_err(|_| ())?;
        if read == 0 {
            return Err(());
        }
        filled += read as usize;
    }
    Ok(())
}

/// One frame; frames over 1 MiB are refused (the pipe is ours, but a size is never trusted blindly).
pub(crate) fn read_frame(pipe: &Owned) -> Result<(u8, Vec<u8>), ()> {
    let mut head = [0u8; 5];
    read_exact(pipe, &mut head)?;
    let length = u32::from_le_bytes([head[1], head[2], head[3], head[4]]) as usize;
    if length > 1 << 20 {
        return Err(());
    }
    let mut body = vec![0u8; length];
    read_exact(pipe, &mut body)?;
    Ok((head[0], body))
}

fn read_frames(pipe: &Owned, events: &Sender<WindowEvent>) {
    while let Ok((kind, body)) = read_frame(pipe) {
        let event = match kind {
            FRAME_INPUT => WindowEvent::Input(body),
            FRAME_RESIZE if body.len() == 4 => WindowEvent::Resize(
                u16::from_le_bytes([body[0], body[1]]),
                u16::from_le_bytes([body[2], body[3]]),
            ),
            _ => continue,
        };
        if events.send(event).is_err() {
            return;
        }
    }
}
