//! The controlled PC's side on Linux (docs/LINUX.md): command lines in pseudo terminals, with the API of Windows'
//! ConPTY one (pty.rs). The person's own shell ($SHELL, else bash, else sh) starts in their home folder as the leader
//! of a session of its own, with the pseudo terminal as its controlling terminal; TERM=xterm-256color, which the
//! viewer's console renders.

use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};

use crate::{CHUNK, clamp_size};

/// Pieces of output waiting to be sent, per command line; beyond this the reader waits, and so does the
/// program writing (no unbounded memory when the network is slow).
const WAITING_PIECES: usize = 64;

/// One command line in a pseudo terminal.
pub struct Pty {
    master: File,
    child: Mutex<Child>,
    output: Receiver<Vec<u8>>,
    pub shell: &'static str,
}

fn window_size(cols: u16, rows: u16) -> libc::winsize {
    let (cols, rows) = clamp_size(cols, rows);
    libc::winsize {
        ws_row: rows as u16,
        ws_col: cols as u16,
        ws_xpixel: 0,
        ws_ypixel: 0,
    }
}

/// The person's shell: $SHELL when it is an absolute path that exists, else bash, else sh.
fn shell_path() -> String {
    std::env::var("SHELL")
        .ok()
        .filter(|path| path.starts_with('/') && std::path::Path::new(path).exists())
        .or_else(|| {
            ["/bin/bash", "/usr/bin/bash"]
                .into_iter()
                .find(|path| std::path::Path::new(path).exists())
                .map(str::to_string)
        })
        .unwrap_or_else(|| "/bin/sh".to_string())
}

fn shell_name(path: &str) -> &'static str {
    match path.rsplit('/').next().unwrap_or("") {
        "bash" => "bash",
        "zsh" => "zsh",
        "fish" => "fish",
        "dash" => "dash",
        "ksh" => "ksh",
        "tcsh" => "tcsh",
        "sh" => "sh",
        _ => "shell",
    }
}

impl Pty {
    /// The person's shell in their home folder.
    pub fn spawn(cols: u16, rows: u16) -> Result<Pty, String> {
        let size = window_size(cols, rows);
        let (mut master, mut slave) = (-1, -1);
        let opened = unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null(),
                &size,
            )
        };
        if opened != 0 {
            return Err(format!(
                "no pseudo terminal: {}",
                std::io::Error::last_os_error()
            ));
        }
        let (master, slave) =
            unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
        // The program's ends must not leak into other programs this one starts.
        for fd in [master.as_raw_fd(), slave.as_raw_fd()] {
            unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
        }
        let io = |fd: &OwnedFd| fd.try_clone().map(Stdio::from).map_err(|e| e.to_string());
        let path = shell_path();
        let home = std::env::var_os("HOME").unwrap_or_else(|| "/".into());
        let mut command = Command::new(&path);
        command
            .current_dir(&home)
            .env("TERM", "xterm-256color")
            .stdin(io(&slave)?)
            .stdout(io(&slave)?)
            .stderr(io(&slave)?);
        // A session of its own, with the pseudo terminal as its controlling terminal (job control, Ctrl+C).
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = command
            .spawn()
            .map_err(|error| format!("{path}: {error}"))?;
        drop(slave);
        let master = File::from(master);
        let reader = master.try_clone().map_err(|e| e.to_string())?;
        let (sender, output) = mpsc::sync_channel(WAITING_PIECES);
        std::thread::Builder::new()
            .name("queqiao-pty".into())
            .spawn(move || read_output(reader, sender))
            .map_err(|e| e.to_string())?;
        Ok(Pty {
            master,
            child: Mutex::new(child),
            output,
            shell: shell_name(&path),
        })
    }

    pub fn write(&self, data: &[u8]) -> Result<(), String> {
        (&self.master).write_all(data).map_err(|e| e.to_string())
    }

    pub fn resize(&self, cols: u16, rows: u16) -> Result<(), String> {
        let size = window_size(cols, rows);
        if unsafe { libc::ioctl(self.master.as_raw_fd(), libc::TIOCSWINSZ, &size) } < 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        Ok(())
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

    /// The exit code once the shell has ended (killed by a signal: 128 + the signal, as shells report it).
    pub fn exit_code(&self) -> Option<u32> {
        let mut child = self.child.lock().ok()?;
        let status = child.try_wait().ok()??;
        Some(match (status.code(), status.signal()) {
            (Some(code), _) => code as u32,
            (None, Some(signal)) => 128 + signal as u32,
            (None, None) => 1,
        })
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        // Hang up the shell's session (as closing a terminal window does), then end it if it stays.
        if let Ok(mut child) = self.child.lock()
            && child.try_wait().ok().flatten().is_none()
        {
            let group = child.id() as libc::pid_t;
            unsafe { libc::kill(-group, libc::SIGHUP) };
            std::thread::sleep(std::time::Duration::from_millis(50));
            if child.try_wait().ok().flatten().is_none() {
                let _ = child.kill();
            }
            let _ = child.wait();
        }
    }
}

fn read_output(mut pipe: File, sender: SyncSender<Vec<u8>>) {
    let mut buffer = vec![0u8; CHUNK];
    loop {
        // EIO once the shell and everything it started have closed the terminal.
        let read = match pipe.read(&mut buffer) {
            Ok(0) | Err(_) => return,
            Ok(read) => read,
        };
        let mut piece = buffer[..read].to_vec();
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
