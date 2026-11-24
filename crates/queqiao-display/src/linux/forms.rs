//! The forms a viewer's session opens, on Linux (Windows: forms.rs); the dialogs are in dialogs.rs. Picking files uses
//! zenity's file chooser where it is installed (Queqiao has none of its own yet), else a typed path. The session bar's
//! lists and the password form come with the rest of the GUI. The session picture for tests is a bitmap file of the window, as on Windows.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use x11rb::connection::Connection;
use x11rb::protocol::xproto::{ConnectionExt as _, ImageFormat};

use super::Result;
use super::window::Hwnd;

/// Runs zenity with `args`; its output when the person answered (OK, Yes), None otherwise or without zenity.
pub(super) fn zenity(args: &[&str]) -> Option<String> {
    let output = Command::new("zenity")
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    output.status.success().then(|| {
        String::from_utf8_lossy(&output.stdout)
            .trim_end_matches('\n')
            .to_string()
    })
}

/// The window a dialog belongs to, from its number as it was sent to another thread.
pub fn owner_from_raw(raw: isize) -> Hwnd {
    Hwnd(raw as usize)
}

/// Files chosen to send: zenity's file chooser where it is installed, else paths typed (separated by ";") in
/// Queqiao's own dialog. None chosen when cancelled.
pub fn pick_files(_owner: Option<Hwnd>, title: &str) -> Vec<PathBuf> {
    let has_zenity = Command::new("zenity")
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success());
    let chosen = if has_zenity {
        zenity(&[
            "--file-selection",
            "--multiple",
            "--separator=\n",
            "--title",
            title,
        ])
    } else {
        super::dialogs::ask_text(
            title,
            queqiao_core::text::tr("Files to send (paths, separated by ;):"),
            "",
        )
        .map(|typed| typed.replace(';', "\n"))
    };
    chosen
        .map(|chosen| {
            chosen
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(PathBuf::from)
                .collect()
        })
        .unwrap_or_default()
}

/// A menu (Windows: at the pointer, owned by `owner`): the chosen item's id.
pub fn popup_menu(_owner: Hwnd, items: &[(u16, &str, bool)]) -> Option<u16> {
    super::dialogs::menu(queqiao_core::text::tr("Queqiao"), items)
}

/// A list with buttons (the session's transfers): Queqiao's own forms come in step 5; until then it is refused.
pub struct PickList {
    never: std::convert::Infallible,
}

/// The id `PickList::next` gives when the list was closed.
pub const PICK_CLOSED: u16 = 0xFFFF;

impl PickList {
    pub fn new(_title: &str, _heading: &str, _buttons: &[(u16, &str)]) -> Result<PickList> {
        Err("lists: not on Linux yet".into())
    }

    pub fn set_items(&mut self, _items: Vec<String>) {
        match self.never {}
    }

    pub fn selected(&self) -> Option<usize> {
        match self.never {}
    }

    pub fn next(&mut self, _wait_ms: u32) -> Option<(u16, Option<usize>)> {
        match self.never {}
    }
}

/// What the person did in the password form.
pub enum PasswordAction {
    Submit(String),
    Cancel,
}

/// The form asking for that PC's password (its lock screen): Queqiao's own forms come in step 5; until then it is
/// refused, and the session says so.
pub struct PasswordForm {
    _never: std::convert::Infallible,
}

impl PasswordForm {
    pub fn new(_title: &str, _text: &str) -> Result<PasswordForm> {
        Err("the password form: not on Linux yet".into())
    }

    pub fn set_status(&self, _text: &str) {
        match self._never {}
    }

    pub fn next(&mut self, _wait_ms: u32) -> Option<PasswordAction> {
        match self._never {}
    }
}

/// The window's picture as it is on the screen, as a bitmap file (32-bit, bottom-up).
/// The window's picture (Windows: without what lies on top; X11 gives the window's own contents either way).
pub fn save_window_picture(window: Hwnd, path: &Path) -> std::io::Result<()> {
    save_screen_picture(window, path)
}

pub fn save_screen_picture(window: Hwnd, path: &Path) -> std::io::Result<()> {
    let failed = |e: &dyn std::fmt::Display| std::io::Error::other(format!("X11: {e}"));
    let (connection, _) = x11rb::connect(None).map_err(|e| failed(&e))?;
    let geometry = connection
        .get_geometry(window.0 as u32)
        .map_err(|e| failed(&e))?
        .reply()
        .map_err(|e| failed(&e))?;
    let image = connection
        .get_image(
            ImageFormat::Z_PIXMAP,
            window.0 as u32,
            0,
            0,
            geometry.width,
            geometry.height,
            !0,
        )
        .map_err(|e| failed(&e))?
        .reply()
        .map_err(|e| failed(&e))?;
    let (width, height) = (usize::from(geometry.width), usize::from(geometry.height));
    if image.data.len() < width * height * 4 {
        return Err(std::io::Error::other(
            "the window's picture is not 32 bits a pixel",
        ));
    }
    let size = width * height * 4;
    let mut file = Vec::with_capacity(54 + size);
    file.extend_from_slice(b"BM");
    file.extend_from_slice(&((54 + size) as u32).to_le_bytes());
    file.extend_from_slice(&[0; 4]);
    file.extend_from_slice(&54u32.to_le_bytes());
    for value in [40u32, width as u32, height as u32] {
        file.extend_from_slice(&value.to_le_bytes());
    }
    file.extend_from_slice(&1u16.to_le_bytes());
    file.extend_from_slice(&32u16.to_le_bytes());
    for value in [0u32, size as u32, 0, 0, 0, 0] {
        file.extend_from_slice(&value.to_le_bytes());
    }
    for row in (0..height).rev() {
        for pixel in image.data[row * width * 4..(row + 1) * width * 4]
            .as_chunks::<4>()
            .0
            .iter()
        {
            file.extend_from_slice(&[pixel[0], pixel[1], pixel[2], 255]);
        }
    }
    drop(connection.flush());
    std::fs::write(path, file)
}
