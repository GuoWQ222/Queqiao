//! The controlling PC's side on Linux (docs/LINUX.md), with the API of Windows' window.rs: a window per command line
//! will be a terminal emulator running the viewer's helper (as Windows starts a console of its own). Not written yet:
//! opening one is refused with a message, which the session shows.

use std::path::Path;

pub enum WindowEvent {
    /// Keys typed in the window (VT input, UTF-8).
    Input(Vec<u8>),
    /// The window's size in characters (also sent once at the start).
    Resize(u16, u16),
    /// The window was closed (or never came up).
    Closed,
}

pub struct TerminalWindow {
    never: std::convert::Infallible,
}

impl TerminalWindow {
    pub fn open(_program: &Path, _title: &str, _test: bool) -> Result<TerminalWindow, String> {
        Err("command line windows: not on Linux yet (docs/LINUX.md)".into())
    }

    pub fn try_event(&self) -> Option<WindowEvent> {
        match self.never {}
    }

    pub fn output(&self, _data: &[u8]) {
        match self.never {}
    }

    pub fn set_title(&self, _title: &str) {
        match self.never {}
    }

    pub fn detach(self) {
        match self.never {}
    }

    pub fn end(&mut self, _text: &str) {
        match self.never {}
    }

    pub fn helper_exit_code(&self) -> Option<u32> {
        match self.never {}
    }
}
