//! The terminal window itself on Linux (Windows: helper.rs): the viewer started with `--terminal-pipe <socket>` inside a
//! terminal emulator. The terminal renders the command line's VT output and turns keys into VT input; this sets it raw
//! (no line editing, echo or Ctrl+C here: the far side does that) and passes bytes along.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use queqiao_core::text::tr;

use crate::window::{
    FRAME_DETACH, FRAME_END, FRAME_EXIT, FRAME_INPUT, FRAME_OUTPUT, FRAME_RESIZE, FRAME_TITLE,
    FRAME_TOKEN, TOKEN_VARIABLE, frame, read_frame,
};

/// The terminal set raw while this lives; as it was again afterwards.
struct Raw(Option<libc::termios>);

impl Raw {
    fn set() -> Raw {
        let mut original: libc::termios = unsafe { std::mem::zeroed() };
        if unsafe { libc::isatty(0) } != 1 || unsafe { libc::tcgetattr(0, &mut original) } != 0 {
            return Raw(None);
        }
        let mut raw = original;
        unsafe {
            libc::cfmakeraw(&mut raw);
            libc::tcsetattr(0, libc::TCSANOW, &raw);
        }
        Raw(Some(original))
    }
}

impl Drop for Raw {
    fn drop(&mut self) {
        if let Some(original) = &self.0 {
            unsafe { libc::tcsetattr(0, libc::TCSANOW, original) };
        }
    }
}

fn write_text(text: &str) {
    let mut out = std::io::stdout();
    let _ = out.write_all(text.as_bytes());
    let _ = out.flush();
}

fn set_title(title: &str) {
    let clean: String = title.chars().filter(|c| !c.is_control()).collect();
    write_text(&format!("\x1b]0;{} - {clean}\x07", tr("Queqiao")));
}

/// The terminal's size in characters.
fn size() -> Option<(u16, u16)> {
    let mut size: libc::winsize = unsafe { std::mem::zeroed() };
    (unsafe { libc::ioctl(1, libc::TIOCGWINSZ, &mut size) } == 0 && size.ws_col > 0)
        .then_some((size.ws_col, size.ws_row.max(1)))
}

/// Runs the terminal window for the socket `socket` until the command line or the session ends; returns the process
/// exit code (also told to the viewer). With `test`, 0 means output and the end both arrived (and the window closes at
/// once).
pub fn run_helper(socket: &str, title: &str, test: bool) -> i32 {
    set_title(title);
    let token = std::env::var(TOKEN_VARIABLE).unwrap_or_default();
    let Ok(mut stream) = UnixStream::connect(socket) else {
        write_text(&format!("{}\r\n", tr("Queqiao: the viewer is not there.")));
        std::thread::sleep(Duration::from_secs(5));
        return 3;
    };
    if stream
        .write_all(&frame(FRAME_TOKEN, token.as_bytes()))
        .is_err()
    {
        return 3;
    }
    let Ok(writer) = stream.try_clone() else {
        return 3;
    };
    let writer = Arc::new(Mutex::new(writer));
    let send = {
        let writer = writer.clone();
        move |kind: u8, body: &[u8]| -> bool {
            writer
                .lock()
                .is_ok_and(|mut writer| writer.write_all(&frame(kind, body)).is_ok())
        }
    };
    let raw = Raw::set();
    // Keys -> viewer.
    {
        let send = send.clone();
        std::thread::spawn(move || {
            let mut stdin = std::io::stdin().lock();
            let mut buffer = [0u8; 1024];
            loop {
                match stdin.read(&mut buffer) {
                    Ok(0) | Err(_) => return,
                    Ok(read) => {
                        if !send(FRAME_INPUT, &buffer[..read]) {
                            return;
                        }
                    }
                }
            }
        });
    }
    // The window's size -> viewer, at the start and whenever it changes.
    {
        let send = send.clone();
        std::thread::spawn(move || {
            let mut last = (0u16, 0u16);
            loop {
                if let Some(now) = size()
                    && now != last
                {
                    last = now;
                    let mut body = now.0.to_le_bytes().to_vec();
                    body.extend_from_slice(&now.1.to_le_bytes());
                    if !send(FRAME_RESIZE, &body) {
                        return;
                    }
                }
                std::thread::sleep(Duration::from_millis(150));
            }
        });
    }
    // Viewer -> screen.
    let mut saw_output = false;
    let code = loop {
        match read_frame(&mut stream) {
            Ok((FRAME_OUTPUT, body)) => {
                saw_output |= !body.is_empty();
                let mut out = std::io::stdout();
                let _ = out.write_all(&body);
                let _ = out.flush();
            }
            Ok((FRAME_TITLE, body)) => set_title(&String::from_utf8_lossy(&body)),
            // Detached: the command line keeps running there; the viewer shows it again on request.
            Ok((FRAME_DETACH, _)) => {
                write_text(&format!(
                    "\r\n\x1b[0m[{}]\r\n",
                    tr(
                        "Detached: the command line keeps running on that PC (Command lines... in the session menu shows it again)."
                    )
                ));
                if !test {
                    std::thread::sleep(Duration::from_secs(1));
                }
                break 0;
            }
            Ok((FRAME_END, body)) => {
                write_text(&format!(
                    "\r\n\x1b[0m[{}]\r\n",
                    String::from_utf8_lossy(&body)
                ));
                if test {
                    break if saw_output { 0 } else { 4 };
                }
                write_text(&format!("{}\r\n", tr("This window closes in 10 seconds.")));
                std::thread::sleep(Duration::from_secs(10));
                break 0;
            }
            Ok(_) => {}
            Err(()) => {
                write_text(&format!(
                    "\r\n\x1b[0m[{}]\r\n",
                    tr("Queqiao: the connection to the session ended.")
                ));
                if !test {
                    std::thread::sleep(Duration::from_secs(10));
                }
                break 5;
            }
        }
    };
    drop(raw);
    send(FRAME_EXIT, &(code as u32).to_le_bytes());
    code
}
