//! Tests (`scripts\run_m5_service.ps1`, started at Medium integrity by `scripts\window_message_probe.ps1`): sends the
//! agent's tray window two messages and reports what Windows let through: one the agent does not let through
//! (refused while it runs at a higher level) and its tray icon's own (let through on purpose: Explorer's clicks).
//!
//! Usage: `message_probe <agent process id> <report file>`

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowExW, GetWindowThreadProcessId, PostMessageW, WM_APP, WM_MOUSEMOVE,
};
use windows::core::{PCWSTR, w};

fn main() {
    let mut args = std::env::args().skip(1);
    let (Some(agent), Some(report)) =
        (args.next().and_then(|a| a.parse::<u32>().ok()), args.next())
    else {
        eprintln!("usage: message_probe <agent process id> <report file>");
        std::process::exit(2);
    };
    let level = queqiao_core::platform::own_integrity_level()
        .map_or("unknown", queqiao_core::platform::integrity_name);
    let mut lines = vec![format!("level {level}")];
    let mut after: Option<HWND> = None;
    let mut found = None;
    while let Ok(window) =
        unsafe { FindWindowExW(None, after, w!("QueqiaoPresence"), PCWSTR::null()) }
    {
        let mut owner = 0u32;
        unsafe { GetWindowThreadProcessId(window, Some(&mut owner)) };
        if owner == agent {
            found = Some(window);
            break;
        }
        after = Some(window);
    }
    match found {
        None => lines.push("no window of the agent".into()),
        Some(window) => {
            // WM_APP + 1 is the tray icon's message; with WM_MOUSEMOVE the agent does nothing.
            for (name, message, lparam) in [
                ("other", WM_APP + 77, 0),
                ("tray", WM_APP + 1, WM_MOUSEMOVE as isize),
            ] {
                let posted =
                    unsafe { PostMessageW(Some(window), message, WPARAM(0), LPARAM(lparam)) };
                lines.push(match posted {
                    Ok(()) => format!("{name} ok"),
                    Err(error) => format!("{name} refused {}", error.code().0 & 0xFFFF),
                });
            }
        }
    }
    let _ = std::fs::write(report, lines.join("\r\n") + "\r\n");
}
