//! The clipboard, shared between the viewer and the agent: text and images (CF_DIB). A watcher thread
//! ("queqiao-clipboard") owns a hidden message-only window registered for clipboard updates: it reports what is
//! copied on this PC and puts what comes from the other side on the clipboard, remembering (by fingerprint) what
//! it put there so that it is not sent back.

use std::cell::RefCell;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::JoinHandle;
use std::time::Duration;

use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::DataExchange::{
    AddClipboardFormatListener, CloseClipboard, EmptyClipboard, GetClipboardData,
    IsClipboardFormatAvailable, OpenClipboard, RemoveClipboardFormatListener, SetClipboardData,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Memory::{
    GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock,
};
use windows::Win32::System::Ole::{CF_DIB, CF_HDROP, CF_UNICODETEXT};
use windows::Win32::UI::Shell::{DROPFILES, DragQueryFileW, HDROP};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::w;

/// Wakes the watcher thread to put something on the clipboard.
const SET_TEXT: u32 = WM_APP + 2;
/// Images larger than this are left alone (64 MB).
const MAX_IMAGE: usize = 64 << 20;

/// What the clipboard holds: text, or an image as a device-independent bitmap (CF_DIB bytes).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClipContent {
    Text(String),
    Image(Vec<u8>),
    /// Files and folders copied in Explorer (CF_HDROP).
    Files(Vec<std::path::PathBuf>),
}

impl ClipContent {
    fn fingerprint(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.hash(&mut hasher);
        hasher.finish()
    }
}

impl std::hash::Hash for ClipContent {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        match self {
            ClipContent::Text(text) => (0u8, text).hash(state),
            ClipContent::Image(bytes) => (1u8, bytes).hash(state),
            ClipContent::Files(paths) => (2u8, paths).hash(state),
        }
    }
}

struct State {
    changes: Sender<ClipContent>,
    pending: Receiver<ClipContent>,
    /// What this watcher last put on the clipboard (its own update is not reported).
    last_set: Option<u64>,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

pub struct Clipboard {
    window: usize,
    texts: Sender<ClipContent>,
    changes: Receiver<ClipContent>,
    thread: Option<JoinHandle<()>>,
}

impl Clipboard {
    pub fn start() -> Result<Clipboard, String> {
        let (text_sender, pending) = mpsc::channel();
        let (change_sender, changes) = mpsc::channel();
        let (ready_sender, ready) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("queqiao-clipboard".into())
            .spawn(move || watch(change_sender, pending, &ready_sender))
            .map_err(|e| e.to_string())?;
        let window = ready
            .recv()
            .map_err(|_| "the clipboard thread ended".to_string())??;
        Ok(Clipboard {
            window,
            texts: text_sender,
            changes,
            thread: Some(thread),
        })
    }

    /// Puts `text` on this PC's clipboard (from the other side).
    pub fn set_text(&self, text: String) {
        self.set(ClipContent::Text(text));
    }

    /// Puts text or an image on this PC's clipboard (from the other side).
    pub fn set(&self, content: ClipContent) {
        if self.texts.send(content).is_ok() {
            let _ = unsafe {
                PostMessageW(
                    Some(HWND(self.window as *mut _)),
                    SET_TEXT,
                    WPARAM(0),
                    LPARAM(0),
                )
            };
        }
    }

    /// What was copied on this PC since the last call, if anything (the latest only).
    pub fn changed(&self) -> Option<ClipContent> {
        self.changes.try_iter().last()
    }
}

impl Drop for Clipboard {
    fn drop(&mut self) {
        let _ = unsafe {
            PostMessageW(
                Some(HWND(self.window as *mut _)),
                WM_CLOSE,
                WPARAM(0),
                LPARAM(0),
            )
        };
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn watch(
    changes: Sender<ClipContent>,
    pending: Receiver<ClipContent>,
    ready: &Sender<Result<usize, String>>,
) {
    let window = match create_window() {
        Ok(window) => window,
        Err(error) => {
            let _ = ready.send(Err(error));
            return;
        }
    };
    STATE.set(Some(State {
        changes,
        pending,
        last_set: None,
    }));
    if let Err(error) = unsafe { AddClipboardFormatListener(window) } {
        let _ = ready.send(Err(format!("clipboard listener: {}", error.message())));
        let _ = unsafe { DestroyWindow(window) };
        return;
    }
    let _ = ready.send(Ok(window.0 as usize));
    let mut message = MSG::default();
    while unsafe { GetMessageW(&mut message, None, 0, 0) }.as_bool() {
        unsafe {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
    STATE.set(None);
}

fn create_window() -> Result<HWND, String> {
    let instance = unsafe { GetModuleHandleW(None) }.map_err(|e| e.message())?;
    static REGISTER: std::sync::Once = std::sync::Once::new();
    REGISTER.call_once(|| unsafe {
        let class = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(window_proc),
            hInstance: instance.into(),
            lpszClassName: w!("QueqiaoClipboard"),
            ..Default::default()
        };
        RegisterClassExW(&class);
    });
    unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            w!("QueqiaoClipboard"),
            w!("Queqiao clipboard"),
            WINDOW_STYLE::default(),
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            Some(instance.into()),
            None,
        )
    }
    .map_err(|e| e.message())
}

extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_CLIPBOARDUPDATE => {
            // Files first: Explorer's copy may carry other formats too.
            let content = read_files(Some(hwnd))
                .map(ClipContent::Files)
                .or_else(|| read_text(Some(hwnd)).map(ClipContent::Text))
                .or_else(|| read_image(Some(hwnd)).map(ClipContent::Image));
            if let Some(content) = content {
                let print = content.fingerprint();
                STATE.with_borrow(|state| {
                    if let Some(state) = state
                        && state.last_set != Some(print)
                    {
                        let _ = state.changes.send(content);
                    }
                });
            }
            LRESULT(0)
        }
        SET_TEXT => {
            let contents: Vec<ClipContent> = STATE.with_borrow(|state| {
                state
                    .as_ref()
                    .map_or_else(Vec::new, |state| state.pending.try_iter().collect())
            });
            if let Some(content) = contents.into_iter().last() {
                let print = content.fingerprint();
                let placed = match &content {
                    ClipContent::Text(text) => write_text(hwnd, text),
                    ClipContent::Image(bytes) => write_global(hwnd, u32::from(CF_DIB.0), bytes),
                    ClipContent::Files(paths) => {
                        write_global(hwnd, u32::from(CF_HDROP.0), &drop_files(paths))
                    }
                };
                if placed {
                    STATE.with_borrow_mut(|state| {
                        if let Some(state) = state {
                            state.last_set = Some(print);
                        }
                    });
                }
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            unsafe {
                let _ = RemoveClipboardFormatListener(hwnd);
                let _ = DestroyWindow(hwnd);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

/// Opens the clipboard for `owner`, waiting briefly while another program holds it.
fn open(owner: Option<HWND>) -> bool {
    for _ in 0..10 {
        if unsafe { OpenClipboard(owner) }.is_ok() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    false
}

/// The clipboard's text, if it holds text (`owner`: the window opening it; None for the calling task).
pub fn read_text(owner: Option<HWND>) -> Option<String> {
    if unsafe { IsClipboardFormatAvailable(u32::from(CF_UNICODETEXT.0)) }.is_err() || !open(owner) {
        return None;
    }
    let text = (|| {
        let handle = unsafe { GetClipboardData(u32::from(CF_UNICODETEXT.0)) }.ok()?;
        let memory = HGLOBAL(handle.0);
        let pointer = unsafe { GlobalLock(memory) } as *const u16;
        if pointer.is_null() {
            return None;
        }
        let units = unsafe { GlobalSize(memory) } / 2;
        let slice = unsafe { std::slice::from_raw_parts(pointer, units) };
        let length = slice.iter().position(|&unit| unit == 0).unwrap_or(units);
        let text = String::from_utf16_lossy(&slice[..length]);
        let _ = unsafe { GlobalUnlock(memory) };
        Some(text)
    })();
    let _ = unsafe { CloseClipboard() };
    text
}

/// The clipboard's image as a device-independent bitmap (CF_DIB: header and pixels), if it holds one.
pub fn read_image(owner: Option<HWND>) -> Option<Vec<u8>> {
    if unsafe { IsClipboardFormatAvailable(u32::from(CF_DIB.0)) }.is_err() || !open(owner) {
        return None;
    }
    let image = (|| {
        let handle = unsafe { GetClipboardData(u32::from(CF_DIB.0)) }.ok()?;
        let memory = HGLOBAL(handle.0);
        let size = unsafe { GlobalSize(memory) };
        if size == 0 || size > MAX_IMAGE {
            return None;
        }
        let pointer = unsafe { GlobalLock(memory) } as *const u8;
        if pointer.is_null() {
            return None;
        }
        let bytes = unsafe { std::slice::from_raw_parts(pointer, size) }.to_vec();
        let _ = unsafe { GlobalUnlock(memory) };
        Some(bytes)
    })();
    let _ = unsafe { CloseClipboard() };
    image
}

/// The files and folders on the clipboard (copied in Explorer), if any.
pub fn read_files(owner: Option<HWND>) -> Option<Vec<std::path::PathBuf>> {
    if unsafe { IsClipboardFormatAvailable(u32::from(CF_HDROP.0)) }.is_err() || !open(owner) {
        return None;
    }
    let files = (|| {
        let handle = unsafe { GetClipboardData(u32::from(CF_HDROP.0)) }.ok()?;
        let drop = HDROP(handle.0);
        let count = unsafe { DragQueryFileW(drop, u32::MAX, None) };
        let mut paths = Vec::new();
        for index in 0..count.min(10_000) {
            let length = unsafe { DragQueryFileW(drop, index, None) } as usize;
            let mut buffer = vec![0u16; length + 1];
            let copied = unsafe { DragQueryFileW(drop, index, Some(&mut buffer)) } as usize;
            paths.push(std::path::PathBuf::from(String::from_utf16_lossy(
                &buffer[..copied.min(length)],
            )));
        }
        (!paths.is_empty()).then_some(paths)
    })();
    let _ = unsafe { CloseClipboard() };
    files
}

/// CF_HDROP's memory: a DROPFILES header, then each path (UTF-16) ending in a zero, then one more zero.
fn drop_files(paths: &[std::path::PathBuf]) -> Vec<u8> {
    let header = size_of::<DROPFILES>();
    let mut bytes = vec![0u8; header];
    let dropfiles = DROPFILES {
        pFiles: header as u32,
        fWide: true.into(),
        ..Default::default()
    };
    unsafe {
        std::ptr::copy_nonoverlapping(
            (&raw const dropfiles).cast::<u8>(),
            bytes.as_mut_ptr(),
            header,
        )
    };
    for path in paths {
        for unit in path
            .as_os_str()
            .to_string_lossy()
            .encode_utf16()
            .chain(Some(0))
        {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
    }
    bytes.extend_from_slice(&[0, 0]);
    bytes
}

fn write_text(owner: HWND, text: &str) -> bool {
    let wide: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
    let bytes = unsafe { std::slice::from_raw_parts(wide.as_ptr().cast::<u8>(), wide.len() * 2) };
    write_global(owner, u32::from(CF_UNICODETEXT.0), bytes)
}

/// Puts `bytes` on the clipboard as `format` (replacing what was there).
fn write_global(owner: HWND, format: u32, bytes: &[u8]) -> bool {
    let Ok(memory) = (unsafe { GlobalAlloc(GMEM_MOVEABLE, bytes.len().max(1)) }) else {
        return false;
    };
    let pointer = unsafe { GlobalLock(memory) } as *mut u8;
    if pointer.is_null() {
        let _ = unsafe { GlobalFree(Some(memory)) };
        return false;
    }
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), pointer, bytes.len());
        let _ = GlobalUnlock(memory);
    }
    if !open(Some(owner)) {
        let _ = unsafe { GlobalFree(Some(memory)) };
        return false;
    }
    let placed = unsafe {
        let _ = EmptyClipboard();
        SetClipboardData(format, Some(HANDLE(memory.0)))
    };
    let _ = unsafe { CloseClipboard() };
    if placed.is_err() {
        // Not taken over by the clipboard: still ours to free.
        let _ = unsafe { GlobalFree(Some(memory)) };
        return false;
    }
    true
}
