//! The controlled PC's own keyboard and mouse off while its screen is dark (the privacy screen): low-level keyboard
//! and mouse hooks on a thread of their own swallow every event that does not carry Windows' "injected" mark, so the
//! person at the PC cannot type or click unseen, while what the agent injects for the viewer goes through.
//! Ctrl+Alt+Del cannot be hooked: it brings up Windows' own screen, which ends the privacy screen (the agent sees the
//! desktop change) and with it this block. The hooks go when the block is dropped (and with the process).

use std::sync::atomic::{AtomicU64, Ordering};
use std::thread::JoinHandle;

use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetMessageW, KBDLLHOOKSTRUCT, LLKHF_INJECTED, LLMHF_INJECTED, MSG,
    MSLLHOOKSTRUCT, PostThreadMessageW, SetWindowsHookExW, UnhookWindowsHookEx, WH_KEYBOARD_LL,
    WH_MOUSE_LL, WM_QUIT,
};

/// Events swallowed by the hooks of this process (the hook procedures have no other way to count).
static SWALLOWED: AtomicU64 = AtomicU64::new(0);

/// Whether a low-level event stays (true) or is swallowed: only injected ones stay.
pub fn keeps(injected: bool) -> bool {
    injected
}

pub struct LocalInputBlock {
    thread_id: u32,
    thread: Option<JoinHandle<()>>,
    start: u64,
}

impl LocalInputBlock {
    /// The hooks set (an error if Windows refused them).
    pub fn start() -> Result<LocalInputBlock, String> {
        let (ready_sender, ready) = std::sync::mpsc::channel::<Result<u32, String>>();
        let thread = std::thread::Builder::new()
            .name("queqiao-block-input".into())
            .spawn(move || {
                let instance = unsafe { GetModuleHandleW(None) }.ok().map(Into::into);
                let keyboard =
                    unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_proc), instance, 0) };
                let mouse =
                    unsafe { SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), instance, 0) };
                let (keyboard, mouse) = match (keyboard, mouse) {
                    (Ok(keyboard), Ok(mouse)) => (keyboard, mouse),
                    (keyboard, mouse) => {
                        let error = [keyboard.as_ref().err(), mouse.as_ref().err()]
                            .into_iter()
                            .flatten()
                            .map(|e| e.message())
                            .next()
                            .unwrap_or_default();
                        for hook in [keyboard.ok(), mouse.ok()].into_iter().flatten() {
                            let _ = unsafe { UnhookWindowsHookEx(hook) };
                        }
                        let _ =
                            ready_sender.send(Err(format!("blocking the local input: {error}")));
                        return;
                    }
                };
                let _ = ready_sender.send(Ok(unsafe { GetCurrentThreadId() }));
                // The hooks are called while this thread waits here; WM_QUIT ends it.
                let mut message = MSG::default();
                while unsafe { GetMessageW(&mut message, None, 0, 0) }.as_bool() {}
                let _ = unsafe { UnhookWindowsHookEx(keyboard) };
                let _ = unsafe { UnhookWindowsHookEx(mouse) };
            })
            .map_err(|e| e.to_string())?;
        let thread_id = ready
            .recv()
            .map_err(|_| "the input block's thread ended".to_string())??;
        Ok(LocalInputBlock {
            thread_id,
            thread: Some(thread),
            start: SWALLOWED.load(Ordering::Relaxed),
        })
    }

    /// Local events swallowed since this block started.
    pub fn swallowed(&self) -> u64 {
        SWALLOWED.load(Ordering::Relaxed) - self.start
    }
}

impl Drop for LocalInputBlock {
    fn drop(&mut self) {
        let _ = unsafe { PostThreadMessageW(self.thread_id, WM_QUIT, WPARAM(0), LPARAM(0)) };
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

extern "system" fn keyboard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 {
        let key = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        if !keeps(key.flags.0 & LLKHF_INJECTED.0 != 0) {
            SWALLOWED.fetch_add(1, Ordering::Relaxed);
            return LRESULT(1);
        }
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

extern "system" fn mouse_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 {
        let mouse = unsafe { &*(lparam.0 as *const MSLLHOOKSTRUCT) };
        if !keeps(mouse.flags & LLMHF_INJECTED != 0) {
            SWALLOWED.fetch_add(1, Ordering::Relaxed);
            return LRESULT(1);
        }
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The hooks themselves are not set here: on a developer's console they would take the keyboard and mouse for
    // the test's length. The agent's privacy session test (run_m4_local.ps1) sets them, with input going through.
    #[test]
    fn only_injected_input_goes_through() {
        assert!(keeps(true));
        assert!(!keeps(false));
    }
}
