//! The clipboard on Linux under X11 (docs/LINUX.md), with the API of Windows' clipboard.rs: text, images and files,
//! shared between the viewer and the agent. A watcher thread ("queqiao-clipboard") with a window of its own learns of
//! each new owner of the CLIPBOARD selection (XFixes), asks it for what it holds and reports that; what comes from the
//! other side it holds itself, as the CLIPBOARD's owner, and gives to the programs that paste (large data in INCR
//! pieces). What it put there itself is not reported. Content a password manager marks as secret
//! (x-kde-passwordManagerHint, as KeePassXC and KDE set) is left alone, as Windows' clipboard-history exclusion is.
//!
//! Formats: text as UTF8_STRING (also STRING and text/plain); images as image/png or image/bmp (a bitmap file: the
//! device-independent bitmap the other side uses, after a 14-byte header); files as text/uri-list and
//! x-special/gnome-copied-files (file managers).

use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use x11rb::connection::{Connection, RequestConnection};
use x11rb::protocol::Event;
use x11rb::protocol::xfixes::{self, ConnectionExt as _};
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ChangeWindowAttributesAux, ConnectionExt as _, CreateWindowAux, EventMask,
    PropMode, Property, SELECTION_NOTIFY_EVENT, SelectionNotifyEvent, SelectionRequestEvent,
    Window, WindowClass,
};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

use crate::clip::{ClipContent, MAX_IMAGE, dib_ok};

x11rb::atom_manager! {
    Atoms: AtomsCookie {
        CLIPBOARD,
        TARGETS,
        TIMESTAMP,
        INCR,
        UTF8_STRING,
        TEXT,
        TEXT_PLAIN: b"text/plain",
        TEXT_PLAIN_UTF8: b"text/plain;charset=utf-8",
        IMAGE_PNG: b"image/png",
        IMAGE_BMP: b"image/bmp",
        URI_LIST: b"text/uri-list",
        GNOME_FILES: b"x-special/gnome-copied-files",
        PASSWORD_HINT: b"x-kde-passwordManagerHint",
        QUEQIAO_CLIP: b"QUEQIAO_CLIP",
    }
}

/// How long a transfer may take before it is given up (the other program stalled).
const TRANSFER_LIMIT: Duration = Duration::from_secs(10);

pub struct Clipboard {
    sets: Sender<ClipContent>,
    changes: Receiver<ClipContent>,
    thread: Option<JoinHandle<()>>,
}

impl Clipboard {
    pub fn start() -> Result<Clipboard, String> {
        let (sets, pending) = mpsc::channel();
        let (change_sender, changes) = mpsc::channel();
        let (ready_sender, ready) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("queqiao-clipboard".into())
            .spawn(move || match Watcher::new(change_sender, pending) {
                Ok(mut watcher) => {
                    let _ = ready_sender.send(Ok(()));
                    watcher.run();
                }
                Err(error) => {
                    let _ = ready_sender.send(Err(error));
                }
            })
            .map_err(|e| e.to_string())?;
        ready
            .recv()
            .map_err(|_| "the clipboard thread ended".to_string())??;
        Ok(Clipboard {
            sets,
            changes,
            thread: Some(thread),
        })
    }

    /// Puts `text` on this PC's clipboard (from the other side).
    pub fn set_text(&self, text: String) {
        self.set(ClipContent::Text(text));
    }

    /// Puts text, an image or files on this PC's clipboard (from the other side).
    pub fn set(&self, content: ClipContent) {
        // An image from the other side goes on the clipboard only as a well-formed bitmap.
        if let ClipContent::Image(bytes) = &content
            && !dib_ok(bytes)
        {
            return;
        }
        let _ = self.sets.send(content);
    }

    /// What was copied on this PC since the last call, if anything (the latest only).
    pub fn changed(&self) -> Option<ClipContent> {
        self.changes.try_iter().last()
    }
}

impl Drop for Clipboard {
    fn drop(&mut self) {
        // The watcher ends when its channel closes (within one wait).
        let (closed, _) = mpsc::channel();
        self.sets = closed;
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// A conversion asked of the clipboard's owner.
enum Asking {
    Targets,
    /// This format, then the others to try if it gives nothing usable.
    Data(Atom, Vec<Atom>),
}

/// Pieces of something large going to a program that pastes (INCR).
struct Sending {
    requestor: Window,
    property: Atom,
    kind: Atom,
    data: Vec<u8>,
    at: usize,
    done: bool,
    started: Instant,
}

struct Watcher {
    connection: RustConnection,
    window: Window,
    atoms: Atoms,
    changes: Sender<ClipContent>,
    pending: Receiver<ClipContent>,
    /// What this watcher holds as the CLIPBOARD's owner, and its PNG once made.
    owned: Option<(ClipContent, Option<Vec<u8>>)>,
    asking: Option<(Asking, Instant)>,
    /// Pieces coming in (INCR): the format and the bytes so far.
    receiving: Option<(Atom, Vec<Atom>, Vec<u8>)>,
    sending: Vec<Sending>,
    /// The largest piece sent in one request.
    piece: usize,
}

impl Watcher {
    fn new(
        changes: Sender<ClipContent>,
        pending: Receiver<ClipContent>,
    ) -> Result<Watcher, String> {
        let text = |e: &dyn std::fmt::Display| format!("clipboard (X11): {e}");
        let (connection, screen_number) = x11rb::connect(None).map_err(|e| text(&e))?;
        let root = connection.setup().roots[screen_number].root;
        let atoms = Atoms::new(&connection)
            .map_err(|e| text(&e))?
            .reply()
            .map_err(|e| text(&e))?;
        let window = connection.generate_id().map_err(|e| text(&e))?;
        connection
            .create_window(
                0,
                window,
                root,
                0,
                0,
                1,
                1,
                0,
                WindowClass::INPUT_ONLY,
                0,
                &CreateWindowAux::new().event_mask(EventMask::PROPERTY_CHANGE),
            )
            .map_err(|e| text(&e))?;
        connection
            .xfixes_query_version(5, 0)
            .map_err(|e| text(&e))?
            .reply()
            .map_err(|e| text(&e))?;
        connection
            .xfixes_select_selection_input(
                window,
                atoms.CLIPBOARD,
                xfixes::SelectionEventMask::SET_SELECTION_OWNER
                    | xfixes::SelectionEventMask::SELECTION_WINDOW_DESTROY
                    | xfixes::SelectionEventMask::SELECTION_CLIENT_CLOSE,
            )
            .map_err(|e| text(&e))?;
        connection.flush().map_err(|e| text(&e))?;
        let piece = (connection.maximum_request_bytes() - 1024).min(256 * 1024);
        Ok(Watcher {
            connection,
            window,
            atoms,
            changes,
            pending,
            owned: None,
            asking: None,
            receiving: None,
            sending: Vec::new(),
            piece,
        })
    }

    fn run(&mut self) {
        loop {
            loop {
                match self.pending.try_recv() {
                    Ok(content) => self.own(content),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => return,
                }
            }
            loop {
                match self.connection.poll_for_event() {
                    Ok(Some(event)) => self.handle(event),
                    Ok(None) => break,
                    // The display went away: nothing more to share.
                    Err(_) => return,
                }
            }
            self.sending
                .retain(|s| !s.done && s.started.elapsed() < TRANSFER_LIMIT);
            if self
                .asking
                .as_ref()
                .is_some_and(|(_, since)| since.elapsed() > TRANSFER_LIMIT)
            {
                self.asking = None;
                self.receiving = None;
            }
            let _ = self.connection.flush();
            let mut poll = libc::pollfd {
                fd: self.connection.stream().as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            unsafe { libc::poll(&mut poll, 1, 20) };
        }
    }

    /// Becomes the CLIPBOARD's owner with what came from the other side.
    fn own(&mut self, content: ClipContent) {
        let _ = self.connection.set_selection_owner(
            self.window,
            self.atoms.CLIPBOARD,
            x11rb::CURRENT_TIME,
        );
        self.owned = Some((content, None));
    }

    fn handle(&mut self, event: Event) {
        match event {
            Event::XfixesSelectionNotify(event) if event.selection == self.atoms.CLIPBOARD => {
                if event.owner == self.window || event.owner == x11rb::NONE {
                    return;
                }
                self.owned = None;
                self.ask(
                    self.atoms.TARGETS,
                    Asking::Targets,
                    event.selection_timestamp,
                );
            }
            Event::SelectionNotify(event) if event.requestor == self.window => self.answered(event),
            Event::PropertyNotify(event)
                if event.window == self.window
                    && event.atom == self.atoms.QUEQIAO_CLIP
                    && event.state == Property::NEW_VALUE =>
            {
                self.piece_came();
            }
            Event::PropertyNotify(event) if event.state == Property::DELETE => {
                self.next_piece(event.window, event.atom);
            }
            Event::SelectionRequest(event) => self.serve(&event),
            Event::SelectionClear(event) if event.selection == self.atoms.CLIPBOARD => {
                self.owned = None;
            }
            _ => {}
        }
    }

    fn ask(&mut self, target: Atom, asking: Asking, time: u32) {
        let _ = self.connection.convert_selection(
            self.window,
            self.atoms.CLIPBOARD,
            target,
            self.atoms.QUEQIAO_CLIP,
            time,
        );
        self.asking = Some((asking, Instant::now()));
        self.receiving = None;
    }

    /// The owner answered a conversion.
    fn answered(&mut self, event: SelectionNotifyEvent) {
        let Some((asking, _)) = self.asking.take() else {
            return;
        };
        if event.property == x11rb::NONE {
            if let Asking::Data(_, rest) = asking {
                self.try_next(rest);
            }
            return;
        }
        let Ok(Ok(reply)) = self
            .connection
            .get_property(
                true,
                self.window,
                self.atoms.QUEQIAO_CLIP,
                AtomEnum::ANY,
                0,
                (MAX_IMAGE / 4) as u32,
            )
            .map(|cookie| cookie.reply())
        else {
            return;
        };
        match asking {
            Asking::Targets => {
                let targets: Vec<Atom> = reply.value32().map(Iterator::collect).unwrap_or_default();
                self.choose(&targets);
            }
            Asking::Data(target, rest) => {
                if reply.type_ == self.atoms.INCR {
                    // Deleting the property (done by the read) starts the pieces.
                    self.receiving = Some((target, rest.clone(), Vec::new()));
                    self.asking = Some((Asking::Data(target, rest), Instant::now()));
                } else {
                    self.received(target, rest, reply.value);
                }
            }
        }
    }

    /// Picks what to ask for among the owner's formats: files, then images, then text.
    fn choose(&mut self, targets: &[Atom]) {
        let a = &self.atoms;
        if targets.contains(&a.PASSWORD_HINT) {
            return;
        }
        let order = [
            a.URI_LIST,
            a.GNOME_FILES,
            a.IMAGE_PNG,
            a.IMAGE_BMP,
            a.UTF8_STRING,
            a.TEXT_PLAIN_UTF8,
            u32::from(AtomEnum::STRING),
        ];
        let offered: Vec<Atom> = order.into_iter().filter(|t| targets.contains(t)).collect();
        self.try_next(offered);
    }

    fn try_next(&mut self, mut rest: Vec<Atom>) {
        if rest.is_empty() {
            return;
        }
        let target = rest.remove(0);
        self.ask(target, Asking::Data(target, rest), x11rb::CURRENT_TIME);
    }

    fn piece_came(&mut self) {
        let Some((target, _, bytes)) = self.receiving.as_mut() else {
            return;
        };
        let target = *target;
        let Ok(Ok(reply)) = self
            .connection
            .get_property(
                true,
                self.window,
                self.atoms.QUEQIAO_CLIP,
                AtomEnum::ANY,
                0,
                (MAX_IMAGE / 4) as u32,
            )
            .map(|cookie| cookie.reply())
        else {
            self.receiving = None;
            return;
        };
        if reply.value.is_empty() {
            if let Some((_, rest, bytes)) = self.receiving.take() {
                self.asking = None;
                self.received(target, rest, bytes);
            }
        } else if bytes.len() + reply.value.len() > MAX_IMAGE {
            self.receiving = None;
            self.asking = None;
        } else {
            bytes.extend_from_slice(&reply.value);
        }
    }

    /// What the owner gave in `target`: reported when usable, else the next format is asked for.
    fn received(&mut self, target: Atom, rest: Vec<Atom>, bytes: Vec<u8>) {
        let a = &self.atoms;
        let content = if target == a.URI_LIST {
            files_from_uris(&String::from_utf8_lossy(&bytes)).map(ClipContent::Files)
        } else if target == a.GNOME_FILES {
            let text = String::from_utf8_lossy(&bytes);
            let uris = text.split_once('\n').map_or("", |(_, uris)| uris);
            files_from_uris(uris).map(ClipContent::Files)
        } else if target == a.IMAGE_PNG {
            dib_from_png(&bytes).map(ClipContent::Image)
        } else if target == a.IMAGE_BMP {
            (bytes.len() > 14 && bytes.starts_with(b"BM") && dib_ok(&bytes[14..]))
                .then(|| ClipContent::Image(bytes[14..].to_vec()))
        } else if target == u32::from(AtomEnum::STRING) {
            Some(ClipContent::Text(
                bytes.iter().map(|&b| char::from(b)).collect(),
            ))
        } else {
            String::from_utf8(bytes).ok().map(ClipContent::Text)
        };
        match content {
            Some(content) => {
                let _ = self.changes.send(content);
            }
            // Links in a URI list, an image that does not decode: the next format.
            None => self.try_next(rest),
        }
    }

    /// A program asks for what this watcher holds.
    fn serve(&mut self, request: &SelectionRequestEvent) {
        let property = if request.property == x11rb::NONE {
            request.target
        } else {
            request.property
        };
        let served = request.selection == self.atoms.CLIPBOARD && self.answer(request, property);
        let reply = SelectionNotifyEvent {
            response_type: SELECTION_NOTIFY_EVENT,
            sequence: 0,
            time: request.time,
            requestor: request.requestor,
            selection: request.selection,
            target: request.target,
            property: if served { property } else { x11rb::NONE },
        };
        let _ = self
            .connection
            .send_event(false, request.requestor, EventMask::NO_EVENT, reply);
    }

    fn answer(&mut self, request: &SelectionRequestEvent, property: Atom) -> bool {
        let a = self.atoms;
        let (requestor, target) = (request.requestor, request.target);
        let Some((content, png)) = self.owned.as_mut() else {
            return false;
        };
        let text_targets = [a.UTF8_STRING, a.TEXT_PLAIN_UTF8, a.TEXT, a.TEXT_PLAIN];
        let mut formats = vec![a.TARGETS, a.TIMESTAMP];
        match content {
            ClipContent::Text(_) => {
                formats.extend(text_targets);
                formats.push(u32::from(AtomEnum::STRING));
            }
            ClipContent::Image(dib) => {
                if png.is_none() {
                    *png = Some(png_from_dib(dib).unwrap_or_default());
                }
                if png.as_ref().is_some_and(|p| !p.is_empty()) {
                    formats.push(a.IMAGE_PNG);
                }
                formats.push(a.IMAGE_BMP);
            }
            ClipContent::Files(_) => {
                formats.extend([a.URI_LIST, a.GNOME_FILES]);
                formats.extend(text_targets);
            }
        }
        if target == a.TARGETS {
            let _ = self.connection.change_property32(
                PropMode::REPLACE,
                requestor,
                property,
                AtomEnum::ATOM,
                &formats,
            );
            return true;
        }
        if target == a.TIMESTAMP {
            let _ = self.connection.change_property32(
                PropMode::REPLACE,
                requestor,
                property,
                AtomEnum::INTEGER,
                &[0],
            );
            return true;
        }
        if !formats.contains(&target) && target != u32::from(AtomEnum::STRING) {
            return false;
        }
        let (kind, data) = match content {
            ClipContent::Text(text) if target == u32::from(AtomEnum::STRING) => (
                target,
                text.chars()
                    .map(|c| u8::try_from(u32::from(c)).unwrap_or(b'?'))
                    .collect(),
            ),
            ClipContent::Text(text) => (a.UTF8_STRING, text.as_bytes().to_vec()),
            ClipContent::Image(dib) if target == a.IMAGE_BMP => (target, bmp_file(dib)),
            ClipContent::Image(_) => (target, png.clone().unwrap_or_default()),
            ClipContent::Files(paths) if target == a.URI_LIST => {
                (target, uri_list(paths).into_bytes())
            }
            ClipContent::Files(paths) if target == a.GNOME_FILES => (
                target,
                format!("copy\n{}", uri_list(paths).replace("\r\n", "\n").trim_end()).into_bytes(),
            ),
            ClipContent::Files(paths) => (
                a.UTF8_STRING,
                paths
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join("\n")
                    .into_bytes(),
            ),
        };
        if data.len() <= self.piece {
            let _ = self.connection.change_property8(
                PropMode::REPLACE,
                requestor,
                property,
                kind,
                &data,
            );
            return true;
        }
        // Large: in pieces as the requestor deletes each (INCR).
        let watch = ChangeWindowAttributesAux::new().event_mask(EventMask::PROPERTY_CHANGE);
        let _ = self.connection.change_window_attributes(requestor, &watch);
        let _ = self.connection.change_property32(
            PropMode::REPLACE,
            requestor,
            property,
            a.INCR,
            &[data.len() as u32],
        );
        self.sending.push(Sending {
            requestor,
            property,
            kind,
            data,
            at: 0,
            done: false,
            started: Instant::now(),
        });
        true
    }

    /// The requestor took a piece: the next one, and an empty one at the end.
    fn next_piece(&mut self, window: Window, property: Atom) {
        let piece = self.piece;
        let Some(sending) = self
            .sending
            .iter_mut()
            .find(|s| s.requestor == window && s.property == property && !s.done)
        else {
            return;
        };
        let end = (sending.at + piece).min(sending.data.len());
        let chunk = &sending.data[sending.at..end];
        let _ = self.connection.change_property8(
            PropMode::REPLACE,
            window,
            property,
            sending.kind,
            chunk,
        );
        if chunk.is_empty() {
            sending.done = true;
        }
        sending.at = end;
    }
}

/// The local paths in a text/uri-list ("file:///home/a/b%20c.txt" lines; "#" lines are comments).
fn files_from_uris(text: &str) -> Option<Vec<PathBuf>> {
    let paths: Vec<PathBuf> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(|uri| {
            let rest = uri.strip_prefix("file://")?;
            // file:///path or file://localhost/path; another host's files are not this PC's.
            let path = if rest.starts_with('/') {
                rest
            } else {
                rest.strip_prefix("localhost")?
            };
            percent_decode(path).map(PathBuf::from)
        })
        .collect();
    (!paths.is_empty()).then_some(paths)
}

fn percent_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'%' {
            let hex = std::str::from_utf8(bytes.get(at + 1..at + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            at += 3;
        } else {
            out.push(bytes[at]);
            at += 1;
        }
    }
    String::from_utf8(out).ok()
}

fn uri_list(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|path| {
            let encoded: String = path
                .to_string_lossy()
                .bytes()
                .map(|b| {
                    if b.is_ascii_alphanumeric() || b"/-._~".contains(&b) {
                        char::from(b).to_string()
                    } else {
                        format!("%{b:02X}")
                    }
                })
                .collect();
            format!("file://{encoded}\r\n")
        })
        .collect()
}

/// A bitmap file: the 14-byte BITMAPFILEHEADER, then the device-independent bitmap.
fn bmp_file(dib: &[u8]) -> Vec<u8> {
    let header = u32::from_le_bytes(dib[0..4].try_into().unwrap_or_default());
    let bits = u16::from_le_bytes(dib[14..16].try_into().unwrap_or_default());
    let compression = u32::from_le_bytes(dib[16..20].try_into().unwrap_or_default());
    let used = u32::from_le_bytes(dib[32..36].try_into().unwrap_or_default());
    let palette = if bits <= 8 {
        (if used == 0 { 1u32 << bits } else { used }) * 4
    } else {
        0
    };
    let masks = if compression == 3 && header == 40 {
        12
    } else {
        0
    };
    let offset = 14 + header + palette + masks;
    let mut file = Vec::with_capacity(14 + dib.len());
    file.extend_from_slice(b"BM");
    file.extend_from_slice(&((14 + dib.len()) as u32).to_le_bytes());
    file.extend_from_slice(&[0; 4]);
    file.extend_from_slice(&offset.to_le_bytes());
    file.extend_from_slice(dib);
    file
}

/// A PNG as a 32-bit device-independent bitmap (bottom-up, B, G, R, A).
fn dib_from_png(bytes: &[u8]) -> Option<Vec<u8>> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    decoder.set_limits(png::Limits { bytes: MAX_IMAGE });
    let mut reader = decoder.read_info().ok()?;
    let mut buffer = vec![0u8; reader.output_buffer_size()];
    let frame = reader.next_frame(&mut buffer).ok()?;
    let (width, height) = (frame.width as usize, frame.height as usize);
    if !(1..=16384).contains(&width) || !(1..=16384).contains(&height) {
        return None;
    }
    let channels = match frame.color_type {
        png::ColorType::Grayscale => 1,
        png::ColorType::GrayscaleAlpha => 2,
        png::ColorType::Rgb => 3,
        png::ColorType::Rgba => 4,
        png::ColorType::Indexed => return None,
    };
    let mut dib = Vec::with_capacity(40 + width * height * 4);
    for value in [40u32, width as u32, height as u32] {
        dib.extend_from_slice(&value.to_le_bytes());
    }
    dib.extend_from_slice(&1u16.to_le_bytes());
    dib.extend_from_slice(&32u16.to_le_bytes());
    for value in [0u32, (width * height * 4) as u32, 0, 0, 0, 0] {
        dib.extend_from_slice(&value.to_le_bytes());
    }
    for row in (0..height).rev() {
        let line = &buffer[row * frame.line_size..row * frame.line_size + width * channels];
        for pixel in line.chunks_exact(channels) {
            let (r, g, b, alpha) = match *pixel {
                [gray] => (gray, gray, gray, 255),
                [gray, alpha] => (gray, gray, gray, alpha),
                [r, g, b] => (r, g, b, 255),
                [r, g, b, alpha] => (r, g, b, alpha),
                _ => return None,
            };
            dib.extend_from_slice(&[b, g, r, alpha]);
        }
    }
    Some(dib)
}

/// A 24- or 32-bit device-independent bitmap as a PNG (other depths: none, image/bmp carries them). A 32-bit bitmap
/// whose alpha is 0 everywhere (as Windows often leaves it) is opaque.
fn png_from_dib(dib: &[u8]) -> Option<Vec<u8>> {
    let u32_at = |at: usize| Some(u32::from_le_bytes(dib.get(at..at + 4)?.try_into().ok()?));
    let header = u32_at(0)? as usize;
    let width = u32_at(4)? as i32;
    let height = u32_at(8)? as i32;
    let bits = u16::from_le_bytes(dib.get(14..16)?.try_into().ok()?);
    let compression = u32_at(16)?;
    if !matches!(bits, 24 | 32) || width <= 0 || height == 0 {
        return None;
    }
    if compression == 3 {
        // Bit fields: only the usual masks (red, green, blue in that order from the top).
        // Right after a 40-byte header, or inside a larger one: at the same place.
        let masks = (u32_at(40)?, u32_at(44)?, u32_at(48)?);
        if masks != (0x00FF_0000, 0x0000_FF00, 0x0000_00FF) || bits != 32 {
            return None;
        }
    } else if compression != 0 {
        return None;
    }
    let offset = header
        + if compression == 3 && header == 40 {
            12
        } else {
            0
        };
    let (width, rows) = (width as usize, height.unsigned_abs() as usize);
    let stride = (width * usize::from(bits)).div_ceil(32) * 4;
    let pixels = dib.get(offset..offset + stride * rows)?;
    let step = usize::from(bits) / 8;
    let opaque = bits == 24
        || pixels.chunks_exact(stride).all(|row| {
            row[..width * 4]
                .as_chunks::<4>()
                .0
                .iter()
                .all(|p| p[3] == 0)
        });
    let mut rgba = Vec::with_capacity(width * rows * 4);
    for y in 0..rows {
        // Bottom-up unless the height is negative.
        let row = if height > 0 { rows - 1 - y } else { y };
        let line = &pixels[row * stride..row * stride + width * step];
        for p in line.chunks_exact(step) {
            let alpha = if opaque { 255 } else { p[3] };
            rgba.extend_from_slice(&[p[2], p[1], p[0], alpha]);
        }
    }
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, width as u32, rows as u32);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_compression(png::Compression::Fast);
        let mut writer = encoder.write_header().ok()?;
        writer.write_image_data(&rgba).ok()?;
    }
    Some(out)
}

/// Asks the clipboard's owner for `target` once, from a connection of its own (for reports and tests): the bytes, if
/// it gives them within a second (not in pieces).
fn read_once(target_name: &[u8]) -> Option<(Vec<u8>, Atom)> {
    let (connection, screen_number) = x11rb::connect(None).ok()?;
    let root = connection.setup().roots[screen_number].root;
    let atoms = Atoms::new(&connection).ok()?.reply().ok()?;
    let target = connection
        .intern_atom(false, target_name)
        .ok()?
        .reply()
        .ok()?
        .atom;
    let window = connection.generate_id().ok()?;
    connection
        .create_window(
            0,
            window,
            root,
            0,
            0,
            1,
            1,
            0,
            WindowClass::INPUT_ONLY,
            0,
            &CreateWindowAux::new(),
        )
        .ok()?;
    connection
        .convert_selection(
            window,
            atoms.CLIPBOARD,
            target,
            atoms.QUEQIAO_CLIP,
            x11rb::CURRENT_TIME,
        )
        .ok()?;
    connection.flush().ok()?;
    let until = Instant::now() + Duration::from_secs(1);
    while Instant::now() < until {
        match connection.poll_for_event().ok()? {
            Some(Event::SelectionNotify(event)) if event.property != x11rb::NONE => {
                let reply = connection
                    .get_property(
                        true,
                        window,
                        atoms.QUEQIAO_CLIP,
                        AtomEnum::ANY,
                        0,
                        (MAX_IMAGE / 4) as u32,
                    )
                    .ok()?
                    .reply()
                    .ok()?;
                return (reply.type_ != atoms.INCR).then_some((reply.value, target));
            }
            Some(Event::SelectionNotify(_)) => return None,
            Some(_) => {}
            None => std::thread::sleep(Duration::from_millis(5)),
        }
    }
    None
}

/// The clipboard's text, if it holds any (`_owner`: Windows' window to open the clipboard with; none here).
pub fn read_text(_owner: Option<u32>) -> Option<String> {
    read_once(b"UTF8_STRING").and_then(|(bytes, _)| String::from_utf8(bytes).ok())
}

/// The files and folders on the clipboard (copied in a file manager), if any.
pub fn read_files(_owner: Option<u32>) -> Option<Vec<PathBuf>> {
    read_once(b"text/uri-list")
        .and_then(|(bytes, _)| files_from_uris(&String::from_utf8_lossy(&bytes)))
}

/// The controlled PC's own keyboard and mouse off while its screen is dark: it goes with the privacy screen, which
/// Linux does not have yet (docs/LINUX.md step 5), so it is refused (the viewer is told).
pub struct LocalInputBlock {
    swallowed: u64,
}

impl LocalInputBlock {
    pub fn start() -> Result<LocalInputBlock, String> {
        Err("blocking this PC's keyboard and mouse: not on Linux yet".into())
    }

    pub fn swallowed(&self) -> u64 {
        self.swallowed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uri_lists_give_local_paths_only() {
        let list = "# copied\r\nfile:///home/a/b%20c.txt\r\nfile://localhost/tmp/x\r\nhttps://example.com/\r\nfile://other/y\r\n";
        assert_eq!(
            files_from_uris(list),
            Some(vec![
                PathBuf::from("/home/a/b c.txt"),
                PathBuf::from("/tmp/x")
            ])
        );
        assert_eq!(files_from_uris("https://example.com/"), None);
        let back = uri_list(&[PathBuf::from("/home/a/b c.txt")]);
        assert_eq!(back, "file:///home/a/b%20c.txt\r\n");
        assert_eq!(
            files_from_uris(&back),
            Some(vec![PathBuf::from("/home/a/b c.txt")])
        );
    }

    #[test]
    fn images_go_from_png_to_a_bitmap_and_back() {
        // 2x2: red, green / blue, half-transparent white.
        let rgba = [
            255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 128,
        ];
        let mut png_bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut png_bytes, 2, 2);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            encoder
                .write_header()
                .unwrap()
                .write_image_data(&rgba)
                .unwrap();
        }
        let dib = dib_from_png(&png_bytes).unwrap();
        assert!(dib_ok(&dib));
        // Bottom-up: the first row stored is the picture's last (blue, white).
        assert_eq!(&dib[40..48], &[255, 0, 0, 255, 255, 255, 255, 128]);
        let file = bmp_file(&dib);
        assert_eq!(&file[0..2], b"BM");
        assert_eq!(u32::from_le_bytes(file[10..14].try_into().unwrap()), 54);
        let again = png_from_dib(&dib).unwrap();
        assert_eq!(dib_from_png(&again).unwrap(), dib);
    }
}
