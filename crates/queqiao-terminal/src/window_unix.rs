//! The controlling PC's side on Linux (docs/LINUX.md), with the API of Windows' window.rs: a window per command line
//! is the desktop's terminal emulator (x-terminal-emulator, which Debian and Ubuntu point at the person's choice; else
//! GNOME Terminal, Konsole, Xfce's, xterm) running the viewer again as its helper (`--terminal-pipe <socket>`,
//! helper_unix.rs), as Windows starts a console of its own. The terminal renders the command line's VT output and
//! turns keys into VT input; the helper passes bytes along.
//!
//! Helper and viewer talk over one Unix socket in the private runtime folder (0700, this user's only). The viewer takes
//! one client, of this user (SO_PEERCRED), with the one-time token it gave the helper in its environment (not on its
//! command line, which other users can read in /proc), and removes the socket once it is connected. The helper may
//! not be the viewer's child (GNOME Terminal's launcher hands it to a server and ends): it tells its exit code over
//! the socket.

use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Socket frames: [kind][length, u32 little-endian][bytes], as Windows' pipes carry them.
pub(crate) const FRAME_INPUT: u8 = 1;
pub(crate) const FRAME_RESIZE: u8 = 2;
pub(crate) const FRAME_OUTPUT: u8 = 3;
pub(crate) const FRAME_END: u8 = 4;
pub(crate) const FRAME_TITLE: u8 = 5;
pub(crate) const FRAME_DETACH: u8 = 6;
/// The helper's first frame: the token from its environment.
pub(crate) const FRAME_TOKEN: u8 = 7;
/// The helper's last: its exit code (u32 little-endian).
pub(crate) const FRAME_EXIT: u8 = 8;
/// Where the helper finds its token.
pub(crate) const TOKEN_VARIABLE: &str = "QUEQIAO_TERMINAL_TOKEN";

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

pub struct TerminalWindow {
    events: Receiver<WindowEvent>,
    frames: Option<Sender<Vec<u8>>>,
    exit_code: Arc<Mutex<Option<u32>>>,
}

impl TerminalWindow {
    /// Opens a terminal window for one command line: `program` (this viewer) is started in it with `--terminal-pipe`.
    /// With `test`, the helper checks itself and ends with 0 when output and the end both reached it.
    pub fn open(program: &Path, title: &str, test: bool) -> Result<TerminalWindow, String> {
        let folder = queqiao_core::platform::runtime_folder()
            .ok_or("no private folder for the terminal window's socket")?;
        let token = random_hex()?;
        let socket = folder.join(format!(
            "terminal-{}-{}.sock",
            std::process::id(),
            &token[..12]
        ));
        let _ = std::fs::remove_file(&socket);
        let listener = UnixListener::bind(&socket)
            .map_err(|e| format!("the terminal window's socket: {e}"))?;
        listener.set_nonblocking(true).map_err(|e| e.to_string())?;
        let title: String = title
            .chars()
            .filter(|c| !c.is_control())
            .take(100)
            .collect();
        let mut helper = vec![
            program.to_string_lossy().into_owned(),
            "--terminal-pipe".into(),
            socket.to_string_lossy().into_owned(),
            "--terminal-title".into(),
            title.clone(),
        ];
        if test {
            helper.push("--terminal-test".into());
        }
        if let Err(error) = start_in_terminal(&helper, &title, &token) {
            let _ = std::fs::remove_file(&socket);
            return Err(error);
        }
        let (event_sender, events) = mpsc::channel();
        let (frames, frame_receiver) = mpsc::channel::<Vec<u8>>();
        let exit_code = Arc::new(Mutex::new(None));
        let code = exit_code.clone();
        std::thread::Builder::new()
            .name("queqiao-terminal-window".into())
            .spawn(move || {
                let stream = accept(&listener, &token);
                // Nobody else connects any more.
                let _ = std::fs::remove_file(&socket);
                let Some(stream) = stream else {
                    let _ = event_sender.send(WindowEvent::Closed);
                    return;
                };
                if let Ok(mut writer) = stream.try_clone() {
                    std::thread::spawn(move || {
                        for frame in frame_receiver {
                            if writer.write_all(&frame).is_err() {
                                return;
                            }
                        }
                    });
                }
                read_frames(stream, &event_sender, &code);
                let _ = event_sender.send(WindowEvent::Closed);
            })
            .map_err(|e| e.to_string())?;
        Ok(TerminalWindow {
            events,
            frames: Some(frames),
            exit_code,
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

    /// The helper's exit code once it has ended (as it told it).
    pub fn helper_exit_code(&self) -> Option<u32> {
        self.exit_code.lock().ok().and_then(|code| *code)
    }

    fn send(&self, kind: u8, data: &[u8]) {
        if let Some(frames) = &self.frames {
            let _ = frames.send(frame(kind, data));
        }
    }
}

impl Drop for TerminalWindow {
    /// The window shows that the session ended and closes by itself.
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

/// One frame; frames over 1 MiB are refused (the socket is ours, but a size is never trusted blindly).
pub(crate) fn read_frame(stream: &mut impl Read) -> Result<(u8, Vec<u8>), ()> {
    let mut head = [0u8; 5];
    stream.read_exact(&mut head).map_err(|_| ())?;
    let length = u32::from_le_bytes([head[1], head[2], head[3], head[4]]) as usize;
    if length > 1 << 20 {
        return Err(());
    }
    let mut body = vec![0u8; length];
    stream.read_exact(&mut body).map_err(|_| ())?;
    Ok((head[0], body))
}

/// 16 random bytes as hex (the helper's token).
fn random_hex() -> Result<String, String> {
    let mut bytes = [0u8; 16];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut random| random.read_exact(&mut bytes))
        .map_err(|e| format!("no random numbers: {e}"))?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

/// The program `name` on the PATH.
fn on_path(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH")?
        .to_string_lossy()
        .split(':')
        .map(|folder| Path::new(folder).join(name))
        .find(|path| path.is_file())
}

/// Starts `helper` (the program and its arguments) in the first terminal emulator found, with the token in its
/// environment. Each takes the command its own way.
fn start_in_terminal(helper: &[String], title: &str, token: &str) -> Result<(), String> {
    let terminals: [(&str, Vec<&str>); 5] = [
        // Debian's policy: -T title, -e command and its arguments.
        ("x-terminal-emulator", vec!["-T", title, "-e"]),
        ("gnome-terminal", vec!["--title", title, "--"]),
        ("konsole", vec!["-e"]),
        ("xfce4-terminal", vec!["-T", title, "-x"]),
        ("xterm", vec!["-T", title, "-e"]),
    ];
    let Some((program, arguments)) = terminals
        .iter()
        .find_map(|(name, arguments)| Some((on_path(name)?, arguments)))
    else {
        return Err(
            "no terminal emulator (x-terminal-emulator, gnome-terminal, konsole, xfce4-terminal or xterm) on this PC"
                .into(),
        );
    };
    let mut child = Command::new(&program)
        .args(arguments)
        .args(helper)
        .env(TOKEN_VARIABLE, token)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("starting {}: {e}", program.display()))?;
    // Waited for on a thread of its own, so it leaves no zombie behind.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// Waits (up to CONNECT_LIMIT) for the helper: a client of this user with the token.
fn accept(listener: &UnixListener, token: &str) -> Option<UnixStream> {
    let until = Instant::now() + CONNECT_LIMIT;
    while Instant::now() < until {
        match listener.accept() {
            Ok((mut stream, _)) => {
                let _ = stream.set_nonblocking(false);
                let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                let ours = same_user(&stream)
                    && matches!(read_frame(&mut stream), Ok((FRAME_TOKEN, body)) if body == token.as_bytes());
                if ours {
                    let _ = stream.set_read_timeout(None);
                    return Some(stream);
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(_) => return None,
        }
    }
    None
}

/// Whether the socket's other end runs as this user (SO_PEERCRED).
fn same_user(stream: &UnixStream) -> bool {
    use std::os::fd::AsRawFd;
    let mut credentials = libc::ucred {
        pid: 0,
        uid: u32::MAX,
        gid: u32::MAX,
    };
    let mut length = size_of::<libc::ucred>() as libc::socklen_t;
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&raw mut credentials).cast(),
            &mut length,
        )
    };
    result == 0 && credentials.uid == unsafe { libc::geteuid() }
}

fn read_frames(
    mut stream: UnixStream,
    events: &Sender<WindowEvent>,
    exit_code: &Mutex<Option<u32>>,
) {
    while let Ok((kind, body)) = read_frame(&mut stream) {
        let event = match kind {
            FRAME_INPUT => WindowEvent::Input(body),
            FRAME_RESIZE if body.len() == 4 => WindowEvent::Resize(
                u16::from_le_bytes([body[0], body[1]]),
                u16::from_le_bytes([body[2], body[3]]),
            ),
            FRAME_EXIT if body.len() == 4 => {
                if let Ok(mut code) = exit_code.lock() {
                    *code = Some(u32::from_le_bytes([body[0], body[1], body[2], body[3]]));
                }
                continue;
            }
            _ => continue,
        };
        if events.send(event).is_err() {
            return;
        }
    }
}
