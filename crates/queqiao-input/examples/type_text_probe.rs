//! Types text through `Injector::type_text` (what the agent does with a phone's Message::TypeText: KEYEVENTF_UNICODE)
//! into an edit window of its own and reads it back: the check that typing arrives as the same characters on Windows,
//! whatever the keyboard layout and input method. It types only while its own window is in front, never into another
//! program; when Windows does not let it come to the front it types nothing and reports TYPE_TEXT_SKIP. Run it in the
//! desktop session of a test machine (not over SSH: that session has no desktop).
//! Usage: type_text_probe [report file]. Last line: TYPE_TEXT_PASS, TYPE_TEXT_FAIL <why> or TYPE_TEXT_SKIP <why>.

#[cfg(not(windows))]
fn main() {
    println!("TYPE_TEXT_SKIP Windows only");
}

#[cfg(windows)]
fn main() {
    let mut report = String::new();
    let verdict = probe::run(&mut report);
    report.push_str(&verdict);
    report.push('\n');
    if let Some(path) = std::env::args().nth(1) {
        let _ = std::fs::write(path, &report);
    }
    print!("{report}");
}

#[cfg(windows)]
mod probe {
    use std::fmt::Write as _;
    use std::time::{Duration, Instant};

    use queqiao_input::{Injector, ScreenRect};
    use windows::Win32::Foundation::HWND;
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyboardLayout, SetFocus};
    use windows::Win32::UI::WindowsAndMessaging::*;
    use windows::core::w;

    /// A line break, a tab, Chinese, an accented letter and a character outside the BMP (two UTF-16 units).
    const TEXT: &str = "typed 中文 é\n\tend 😀";
    /// The edit control's styles: several lines, scrolled to the caret, Enter makes a new line.
    const ES_MULTILINE: u32 = 0x0004;
    const ES_AUTOVSCROLL: u32 = 0x0040;
    const ES_WANTRETURN: u32 = 0x1000;

    pub fn run(report: &mut String) -> String {
        let instance = match unsafe { GetModuleHandleW(None) } {
            Ok(instance) => instance,
            Err(error) => return format!("TYPE_TEXT_FAIL module: {error}"),
        };
        let style = WS_OVERLAPPEDWINDOW
            | WS_VISIBLE
            | WINDOW_STYLE(ES_MULTILINE | ES_AUTOVSCROLL | ES_WANTRETURN);
        let window = match unsafe {
            CreateWindowExW(
                WS_EX_TOPMOST,
                w!("EDIT"),
                // An edit control's window text is what it holds: empty (its title bar shows the typed text).
                w!(""),
                style,
                120,
                120,
                560,
                240,
                None,
                None,
                Some(instance.into()),
                None,
            )
        } {
            Ok(window) => window,
            Err(error) => return format!("TYPE_TEXT_FAIL window: {error}"),
        };
        pump(Duration::from_millis(300));
        let front = bring_to_front(window);
        pump(Duration::from_millis(300));
        let _ = unsafe { SetFocus(Some(window)) };
        let layout = unsafe { GetKeyboardLayout(0) };
        let _ = writeln!(report, "keyboard layout: {:08x}", layout.0 as usize);
        let _ = writeln!(report, "in front: {}", if front { "yes" } else { "no" });
        // Checked again just before typing: the keys go to whatever window is in front.
        if unsafe { GetForegroundWindow() } != window {
            let _ = unsafe { DestroyWindow(window) };
            return "TYPE_TEXT_SKIP the test window could not come to the front (nothing typed)"
                .into();
        }
        let typed = Injector::new(ScreenRect {
            left: 0,
            top: 0,
            width: 1,
            height: 1,
        })
        .type_text(TEXT);
        pump(Duration::from_millis(1500));
        let mut buffer = vec![0u16; 1024];
        let length = unsafe { GetWindowTextW(window, &mut buffer) }.max(0) as usize;
        let got = String::from_utf16_lossy(&buffer[..length]);
        let _ = unsafe { DestroyWindow(window) };
        pump(Duration::from_millis(100));
        let expected = TEXT.replace('\n', "\r\n");
        let _ = writeln!(report, "typed: {:?}", TEXT);
        let _ = writeln!(report, "read back: {:?}", got);
        match typed {
            Err(error) => format!("TYPE_TEXT_FAIL type_text: {error}"),
            Ok(()) if got == expected => "TYPE_TEXT_PASS".into(),
            Ok(()) => "TYPE_TEXT_FAIL the window holds other text".into(),
        }
    }

    /// Brings the window to the front. Windows lets a program take the front only in some cases (the person's last
    /// input went to it, or nothing has had input for a while); joined for a moment to the input of the window in front,
    /// it may, as a program the person just started would.
    fn bring_to_front(window: HWND) -> bool {
        unsafe {
            let _ = ShowWindow(window, SW_SHOWNORMAL);
            if SetForegroundWindow(window).as_bool() && GetForegroundWindow() == window {
                return true;
            }
            let front = GetForegroundWindow();
            let front_thread = GetWindowThreadProcessId(front, None);
            let own = GetCurrentThreadId();
            if front_thread != 0 && front_thread != own {
                let _ = AttachThreadInput(own, front_thread, true);
                let _ = BringWindowToTop(window);
                let _ = SetForegroundWindow(window);
                let _ = AttachThreadInput(own, front_thread, false);
            }
            GetForegroundWindow() == window
        }
    }

    fn pump(duration: Duration) {
        let end = Instant::now() + duration;
        let mut message = MSG::default();
        while Instant::now() < end {
            while unsafe { PeekMessageW(&mut message, None, 0, 0, PM_REMOVE) }.as_bool() {
                unsafe {
                    let _ = TranslateMessage(&message);
                    DispatchMessageW(&message);
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
