//! Puts decoded pictures into the session window on X11 (Windows: present.rs): converted and scaled on the CPU
//! (yuv.rs: BT.709 limited range in, the aspect ratio kept, black bars around) into an image the X server shares with
//! this process (MIT-SHM), else sent in the request (a display over the network). Works without a GPU (Xvfb, virtual
//! machines); drawing on the GPU comes with the GUI step.

use queqiao_capture::{Context, Device};
use queqiao_codec::Picture;
use x11rb::connection::{Connection, RequestConnection};
use x11rb::protocol::shm::ConnectionExt as _;
use x11rb::protocol::xproto::{ConnectionExt as _, ImageFormat};
use x11rb::rust_connection::RustConnection;

use super::window::{Rect, Window};
use super::{Result, x11_error};
use crate::yuv::{Layout, picture_to_bgra};

/// A System V shared memory segment attached here and to the X server.
struct Segment {
    address: *mut u8,
    size: usize,
    xid: u32,
}

impl Segment {
    fn new(connection: &RustConnection, size: usize) -> Result<Segment> {
        let id = unsafe { libc::shmget(libc::IPC_PRIVATE, size, libc::IPC_CREAT | 0o600) };
        if id < 0 {
            return Err(format!("shmget: {}", std::io::Error::last_os_error()));
        }
        let address = unsafe { libc::shmat(id, std::ptr::null(), 0) };
        if address as isize == -1 {
            unsafe { libc::shmctl(id, libc::IPC_RMID, std::ptr::null_mut()) };
            return Err(format!("shmat: {}", std::io::Error::last_os_error()));
        }
        let segment = Segment {
            address: address.cast(),
            size,
            xid: connection.generate_id().map_err(x11_error)?,
        };
        let attached = connection
            .shm_attach(segment.xid, id as u32, false)
            .map_err(x11_error)
            .and_then(|cookie| cookie.check().map_err(x11_error));
        // Marked for removal now: it goes once both sides detached, also if this process ends abruptly.
        unsafe { libc::shmctl(id, libc::IPC_RMID, std::ptr::null_mut()) };
        attached.map(|()| segment)
    }

    fn bytes(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.address, self.size) }
    }
}

impl Drop for Segment {
    fn drop(&mut self) {
        unsafe { libc::shmdt(self.address.cast()) };
    }
}

pub struct Presenter {
    connection: RustConnection,
    window: u32,
    gc: u32,
    depth: u8,
    /// The shared image and the window size it is for; None without MIT-SHM.
    shared: Option<(Segment, (u32, u32))>,
    shm: bool,
    /// The image when it goes in the request.
    image: Vec<u8>,
}

impl Presenter {
    pub fn new(_device: &Device, _context: &Context, window: &Window) -> Result<Presenter> {
        let (connection, _) = x11rb::connect(None).map_err(x11_error)?;
        let gc = connection.generate_id().map_err(x11_error)?;
        connection
            .create_gc(gc, window.window, &Default::default())
            .map_err(x11_error)?;
        let shm = connection
            .shm_query_version()
            .ok()
            .and_then(|cookie| cookie.reply().ok())
            .is_some();
        Ok(Presenter {
            connection,
            window: window.window,
            gc,
            depth: window.depth,
            shared: None,
            shm,
            image: Vec::new(),
        })
    }

    pub fn description(&self) -> String {
        format!(
            "CPU (BT.709, bilinear, aspect kept) into {}",
            if self.shm {
                "an X11 shared-memory image"
            } else {
                "X11 requests (no shared memory)"
            }
        )
    }

    /// Shows a decoded picture of `size` (width, height) in the window, scaled to its drawing area.
    pub fn show(&mut self, picture: &Picture, size: (u32, u32), window: &Window) -> Result<()> {
        let (width, height) = window.client_size();
        let bytes = width as usize * height as usize * 4;
        let need = Layout::Nv12.bytes(picture.pitch, picture.rows);
        if picture.data.len() < need || size.0 as usize > picture.pitch {
            return Err(format!(
                "a picture of {} bytes for {}x{} (pitch {})",
                picture.data.len(),
                size.0,
                size.1,
                picture.pitch
            ));
        }
        let picture_size = (size.0 as usize, size.1 as usize);
        let window_size = (width as usize, height as usize);
        if self.shm {
            if self
                .shared
                .as_ref()
                .is_none_or(|(_, at)| *at != (width, height))
            {
                self.shared = None;
                match Segment::new(&self.connection, bytes) {
                    Ok(segment) => self.shared = Some((segment, (width, height))),
                    // Shared memory refused (another host): the requests from now on.
                    Err(_) => self.shm = false,
                }
            }
            if let Some((segment, _)) = self.shared.as_mut() {
                picture_to_bgra(
                    &picture.data,
                    picture.pitch,
                    picture.rows,
                    Layout::Nv12,
                    picture_size,
                    segment.bytes(),
                    window_size,
                );
                self.connection
                    .shm_put_image(
                        self.window,
                        self.gc,
                        width as u16,
                        height as u16,
                        0,
                        0,
                        width as u16,
                        height as u16,
                        0,
                        0,
                        self.depth,
                        ImageFormat::Z_PIXMAP.into(),
                        false,
                        segment.xid,
                        0,
                    )
                    .map_err(x11_error)?;
                // The image is drawn before it is written again.
                self.connection
                    .get_input_focus()
                    .map_err(x11_error)?
                    .reply()
                    .map_err(x11_error)?;
                return Ok(());
            }
        }
        self.image.resize(bytes, 0);
        picture_to_bgra(
            &picture.data,
            picture.pitch,
            picture.rows,
            Layout::Nv12,
            picture_size,
            &mut self.image,
            window_size,
        );
        // In bands that fit a request.
        let row = width as usize * 4;
        let band = ((self.connection.maximum_request_bytes() - 64) / row.max(1)).max(1);
        for (index, rows) in self.image.chunks(band * row).enumerate() {
            self.connection
                .put_image(
                    ImageFormat::Z_PIXMAP,
                    self.window,
                    self.gc,
                    width as u16,
                    (rows.len() / row) as u16,
                    0,
                    (index * band) as i16,
                    0,
                    self.depth,
                    rows,
                )
                .map_err(x11_error)?;
        }
        self.connection.flush().map_err(x11_error)
    }
}

/// The aspect-preserving area of `output` that an `input`-sized picture fills, centred.
pub fn fit(input: (u32, u32), output: (u32, u32)) -> Rect {
    let scale =
        (f64::from(output.0) / f64::from(input.0)).min(f64::from(output.1) / f64::from(input.1));
    let width = ((f64::from(input.0) * scale).round() as i32).clamp(1, output.0 as i32);
    let height = ((f64::from(input.1) * scale).round() as i32).clamp(1, output.1 as i32);
    let left = (output.0 as i32 - width) / 2;
    let top = (output.1 as i32 - height) / 2;
    Rect {
        left,
        top,
        right: left + width,
        bottom: top + height,
    }
}
