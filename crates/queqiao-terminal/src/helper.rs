//! The terminal window itself: the viewer started with `--terminal-pipe <name>` in a console of its own. The
//! console renders the command line's VT output and turns keys into VT input; this only passes bytes along.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use windows::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE, HANDLE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows::Win32::System::Console::{
    AllocConsole, CONSOLE_SCREEN_BUFFER_INFO, DISABLE_NEWLINE_AUTO_RETURN, ENABLE_PROCESSED_OUTPUT,
    ENABLE_VIRTUAL_TERMINAL_INPUT, ENABLE_VIRTUAL_TERMINAL_PROCESSING, ENABLE_WINDOW_INPUT,
    GetConsoleScreenBufferInfo, ReadConsoleW, SetConsoleCP, SetConsoleMode, SetConsoleOutputCP,
    SetConsoleTitleW, WriteConsoleW,
};
use windows::core::{PCWSTR, w};

use crate::window::{
    FRAME_END, FRAME_INPUT, FRAME_OUTPUT, FRAME_RESIZE, frame, open_pipe, read_frame, write_all,
};

const UTF8: u32 = 65001;

/// Runs the terminal window for the pipes named `pipe` until the command line or the session ends; returns the
/// process exit code. With `test`, 0 means output and the end both arrived (and the window closes at once).
pub fn run_helper(pipe: &str, title: &str, test: bool) -> i32 {
    let _ = unsafe { AllocConsole() };
    let title: Vec<u16> = format!("Queqiao - {title}")
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let _ = unsafe { SetConsoleTitleW(PCWSTR(title.as_ptr())) };
    let (Some(input), Some(output)) = (console(w!("CONIN$")), console(w!("CONOUT$"))) else {
        return 2;
    };
    unsafe {
        let _ = SetConsoleCP(UTF8);
        let _ = SetConsoleOutputCP(UTF8);
        // Raw keys as VT sequences: no line editing, echo or Ctrl+C handling here (the far side does that).
        let _ = SetConsoleMode(input, ENABLE_VIRTUAL_TERMINAL_INPUT | ENABLE_WINDOW_INPUT);
        let _ = SetConsoleMode(
            output,
            ENABLE_PROCESSED_OUTPUT
                | ENABLE_VIRTUAL_TERMINAL_PROCESSING
                | DISABLE_NEWLINE_AUTO_RETURN,
        );
    }
    let (Ok(to_viewer), Ok(from_viewer)) = (
        open_pipe(&format!("{pipe}-in"), GENERIC_WRITE.0),
        open_pipe(&format!("{pipe}-out"), GENERIC_READ.0),
    ) else {
        write_text(output, "Queqiao: the viewer is not there.\r\n");
        std::thread::sleep(Duration::from_secs(5));
        return 3;
    };
    let to_viewer = Arc::new(Mutex::new(to_viewer));
    let console_handles = ConsoleHandles { input, output };

    // Keys -> viewer.
    {
        let to_viewer = to_viewer.clone();
        let handles = console_handles;
        std::thread::spawn(move || {
            // The whole Send wrapper moves in (a closure would otherwise take just the handle field).
            let handles = handles;
            let mut buffer = [0u16; 512];
            loop {
                let mut read = 0u32;
                let result = unsafe {
                    ReadConsoleW(
                        handles.input,
                        buffer.as_mut_ptr().cast(),
                        buffer.len() as u32,
                        &mut read,
                        None,
                    )
                };
                if result.is_err() {
                    return;
                }
                if read == 0 {
                    continue;
                }
                let text = String::from_utf16_lossy(&buffer[..read as usize]);
                let sent = to_viewer
                    .lock()
                    .map(|pipe| write_all(&pipe, &frame(FRAME_INPUT, text.as_bytes())));
                if !matches!(sent, Ok(Ok(()))) {
                    return;
                }
            }
        });
    }
    // The window's size -> viewer, at the start and whenever it changes.
    {
        let to_viewer = to_viewer.clone();
        let handles = console_handles;
        std::thread::spawn(move || {
            let handles = handles;
            let mut last = (0u16, 0u16);
            loop {
                let mut info = CONSOLE_SCREEN_BUFFER_INFO::default();
                if unsafe { GetConsoleScreenBufferInfo(handles.output, &mut info) }.is_ok() {
                    let window = info.srWindow;
                    let size = (
                        (window.Right - window.Left + 1).max(1) as u16,
                        (window.Bottom - window.Top + 1).max(1) as u16,
                    );
                    if size != last {
                        last = size;
                        let mut body = size.0.to_le_bytes().to_vec();
                        body.extend_from_slice(&size.1.to_le_bytes());
                        let sent = to_viewer
                            .lock()
                            .map(|pipe| write_all(&pipe, &frame(FRAME_RESIZE, &body)));
                        if !matches!(sent, Ok(Ok(()))) {
                            return;
                        }
                    }
                }
                std::thread::sleep(Duration::from_millis(150));
            }
        });
    }
    // Viewer -> screen.
    let mut pending: Vec<u8> = Vec::new();
    let mut saw_output = false;
    loop {
        match read_frame(&from_viewer) {
            Ok((FRAME_OUTPUT, body)) => {
                saw_output |= !body.is_empty();
                pending.extend_from_slice(&body);
                let text = take_utf8(&mut pending);
                write_text(output, &text);
            }
            Ok((FRAME_END, body)) => {
                write_text(
                    output,
                    &format!("\r\n\x1b[0m[{}]\r\n", String::from_utf8_lossy(&body)),
                );
                if test {
                    return if saw_output { 0 } else { 4 };
                }
                write_text(output, "This window closes in 10 seconds.\r\n");
                std::thread::sleep(Duration::from_secs(10));
                return 0;
            }
            Ok(_) => {}
            Err(()) => {
                write_text(
                    output,
                    "\r\n\x1b[0m[Queqiao: the connection to the session ended.]\r\n",
                );
                if !test {
                    std::thread::sleep(Duration::from_secs(10));
                }
                return 5;
            }
        }
    }
}

#[derive(Clone, Copy)]
struct ConsoleHandles {
    input: HANDLE,
    output: HANDLE,
}

unsafe impl Send for ConsoleHandles {}

fn console(name: PCWSTR) -> Option<HANDLE> {
    unsafe {
        CreateFileW(
            name,
            GENERIC_READ.0 | GENERIC_WRITE.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_FLAGS_AND_ATTRIBUTES(0),
            None,
        )
    }
    .ok()
}

fn write_text(output: HANDLE, text: &str) {
    let wide: Vec<u16> = text.encode_utf16().collect();
    let _ = unsafe { WriteConsoleW(output, &wide, None, None) };
}

/// The complete UTF-8 text at the front of `pending` (a piece may end inside a character: that tail stays);
/// bytes that are not UTF-8 at all become U+FFFD.
fn take_utf8(pending: &mut Vec<u8>) -> String {
    let mut text = String::new();
    loop {
        match std::str::from_utf8(pending) {
            Ok(all) => {
                text.push_str(all);
                pending.clear();
                return text;
            }
            Err(error) => {
                let good = error.valid_up_to();
                text.push_str(std::str::from_utf8(&pending[..good]).unwrap_or_default());
                match error.error_len() {
                    // Cut off inside a character: keep the tail for the next piece.
                    None => {
                        pending.drain(..good);
                        return text;
                    }
                    Some(bad) => {
                        text.push('\u{FFFD}');
                        pending.drain(..good + bad);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::take_utf8;

    #[test]
    fn utf8_split_across_pieces_is_kept_whole() {
        let bytes = "a中b".as_bytes();
        let mut pending = bytes[..2].to_vec();
        assert_eq!(take_utf8(&mut pending), "a");
        pending.extend_from_slice(&bytes[2..]);
        assert_eq!(take_utf8(&mut pending), "中b");
        let mut bad = vec![b'x', 0xFF, b'y'];
        assert_eq!(take_utf8(&mut bad), "x\u{FFFD}y");
    }
}
