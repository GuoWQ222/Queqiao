//! The screen as the display hardware shows it (KMS), for the Queqiao service's desktop helper (docs/LINUX.md): the
//! framebuffer each lit CRTC scans out, whoever draws it (the sign-in screen, a lock screen, a text console, a
//! desktop). Only root with CAP_SYS_ADMIN is given a framebuffer's buffer handles (GETFB2), so no program of a person
//! can use this. The pointer, on its own plane, is drawn into the picture.
//!
//! A buffer in linear memory is read where it lies: exported as a DMA-BUF and mapped (the usual way), else mapped as
//! a dumb buffer (what compositors without GPU drawing scan out). A buffer in a GPU's own tiled layout (a modifier
//! other than LINEAR) is imported by the GPU itself and drawn into a plain texture (kms_egl.rs).

use std::cell::RefCell;
use std::fs::{File, OpenOptions};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::fs::OpenOptionsExt;

use crate::BgraImage;
use crate::kms_egl::{EglReader, Plane};

/// Tests: the GPU's reading of a picture of its own layout (see kms_egl).
pub use crate::kms_egl::self_test as gpu_self_test;

const fn iowr(nr: u64, size: usize) -> u64 {
    (3 << 30) | ((size as u64) << 16) | (0x64 << 8) | nr
}

const fn iow(nr: u64, size: usize) -> u64 {
    (1 << 30) | ((size as u64) << 16) | (0x64 << 8) | nr
}

#[repr(C)]
#[derive(Default)]
struct ClientCap {
    capability: u64,
    value: u64,
}

#[repr(C)]
#[derive(Default)]
struct CardRes {
    fb_id_ptr: u64,
    crtc_id_ptr: u64,
    connector_id_ptr: u64,
    encoder_id_ptr: u64,
    count_fbs: u32,
    count_crtcs: u32,
    count_connectors: u32,
    count_encoders: u32,
    min_width: u32,
    max_width: u32,
    min_height: u32,
    max_height: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct ModeInfo {
    clock: u32,
    hdisplay: u16,
    hsync_start: u16,
    hsync_end: u16,
    htotal: u16,
    hskew: u16,
    vdisplay: u16,
    vsync_start: u16,
    vsync_end: u16,
    vtotal: u16,
    vscan: u16,
    vrefresh: u32,
    flags: u32,
    kind: u32,
    name: [u8; 32],
}

#[repr(C)]
#[derive(Default)]
struct Crtc {
    set_connectors_ptr: u64,
    count_connectors: u32,
    crtc_id: u32,
    fb_id: u32,
    x: u32,
    y: u32,
    gamma_size: u32,
    mode_valid: u32,
    mode: ModeInfo,
}

#[repr(C)]
#[derive(Default)]
struct PlaneRes {
    plane_id_ptr: u64,
    count_planes: u32,
}

#[repr(C)]
#[derive(Default)]
struct GetPlane {
    plane_id: u32,
    crtc_id: u32,
    fb_id: u32,
    possible_crtcs: u32,
    gamma_size: u32,
    count_format_types: u32,
    format_type_ptr: u64,
}

#[repr(C)]
#[derive(Default)]
struct ObjProps {
    props_ptr: u64,
    prop_values_ptr: u64,
    count_props: u32,
    obj_id: u32,
    obj_type: u32,
}

#[repr(C)]
#[derive(Default)]
struct GetProperty {
    values_ptr: u64,
    enum_blob_ptr: u64,
    prop_id: u32,
    flags: u32,
    name: [u8; 32],
    count_values: u32,
    count_enum_blobs: u32,
}

#[repr(C)]
#[derive(Default)]
struct Fb2 {
    fb_id: u32,
    width: u32,
    height: u32,
    pixel_format: u32,
    flags: u32,
    handles: [u32; 4],
    pitches: [u32; 4],
    offsets: [u32; 4],
    modifier: [u64; 4],
}

#[repr(C)]
#[derive(Default)]
struct Fb {
    fb_id: u32,
    width: u32,
    height: u32,
    pitch: u32,
    bpp: u32,
    depth: u32,
    handle: u32,
}

#[repr(C)]
#[derive(Default)]
struct MapDumb {
    handle: u32,
    pad: u32,
    offset: u64,
}

#[repr(C)]
#[derive(Default)]
struct PrimeHandle {
    handle: u32,
    flags: u32,
    fd: i32,
}

#[repr(C)]
#[derive(Default)]
struct GemClose {
    handle: u32,
    pad: u32,
}

const SET_CLIENT_CAP: u64 = iow(0x0d, size_of::<ClientCap>());
const GET_RESOURCES: u64 = iowr(0xA0, size_of::<CardRes>());
const GET_CRTC: u64 = iowr(0xA1, size_of::<Crtc>());
const GET_PROPERTY: u64 = iowr(0xAA, size_of::<GetProperty>());
const GET_FB: u64 = iowr(0xAD, size_of::<Fb>());
const MAP_DUMB: u64 = iowr(0xB3, size_of::<MapDumb>());
const GET_PLANE_RESOURCES: u64 = iowr(0xB5, size_of::<PlaneRes>());
const GET_PLANE: u64 = iowr(0xB6, size_of::<GetPlane>());
const OBJ_GET_PROPERTIES: u64 = iowr(0xB9, size_of::<ObjProps>());
const GET_FB2: u64 = iowr(0xCE, size_of::<Fb2>());
const PRIME_HANDLE_TO_FD: u64 = iowr(0x2d, size_of::<PrimeHandle>());
const GEM_CLOSE: u64 = iow(0x09, size_of::<GemClose>());
/// DMA_BUF_IOCTL_SYNC (`_IOW('b', 0, u64)`): reads of a mapped DMA-BUF between START and END see the GPU's writes.
const DMA_BUF_SYNC: u64 = (1 << 30) | (8 << 16) | (0x62 << 8);
const DMA_BUF_SYNC_READ: u64 = 1;
const DMA_BUF_SYNC_START: u64 = 0;
const DMA_BUF_SYNC_END: u64 = 4;

const CLIENT_CAP_UNIVERSAL_PLANES: u64 = 2;
/// Asked for only to be shown the planes' atomic properties (where the pointer is: CRTC_X, CRTC_Y); this process
/// never commits anything.
const CLIENT_CAP_ATOMIC: u64 = 3;
/// Virtual machines' GPUs (virtio-gpu, vmwgfx, qxl) hide the pointer's plane from atomic clients that do not take
/// its hotspot into account (Linux 6.6 and later); unknown to older kernels, which list it anyway.
const CLIENT_CAP_CURSOR_PLANE_HOTSPOT: u64 = 6;
const MODE_FB_MODIFIERS: u32 = 2;
const OBJECT_PLANE: u32 = 0xeeee_eeee;
const PLANE_TYPE_CURSOR: u64 = 2;
const MOD_LINEAR: u64 = 0;

const fn fourcc(code: &[u8; 4]) -> u32 {
    u32::from_le_bytes(*code)
}

const XRGB8888: u32 = fourcc(b"XR24");
const ARGB8888: u32 = fourcc(b"AR24");
const XBGR8888: u32 = fourcc(b"XB24");
const ABGR8888: u32 = fourcc(b"AB24");
const XRGB2101010: u32 = fourcc(b"XR30");
const ARGB2101010: u32 = fourcc(b"AR30");
const RGB565: u32 = fourcc(b"RG16");

fn ioctl<T>(fd: RawFd, request: u64, argument: &mut T) -> std::io::Result<()> {
    loop {
        let result = unsafe { libc::ioctl(fd, request as _, argument as *mut T) };
        if result == 0 {
            return Ok(());
        }
        let error = std::io::Error::last_os_error();
        if !matches!(error.raw_os_error(), Some(libc::EINTR | libc::EAGAIN)) {
            return Err(error);
        }
    }
}

/// What changes when a screen does (Kms::state): its framebuffer, and the pointer plane's framebuffer and place.
pub type KmsState = (u32, Option<(u32, i32, i32)>);

/// A lit screen: one CRTC with a mode and a framebuffer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KmsScreen {
    /// The card (`/dev/dri/cardN`) and the CRTC on it.
    pub card: usize,
    pub crtc: u32,
    pub width: u32,
    pub height: u32,
    /// Where its picture starts in the framebuffer (X11 puts all screens in one framebuffer side by side).
    pub left: i32,
    pub top: i32,
}

/// The display cards with something lit.
pub struct Kms {
    cards: Vec<(usize, File)>,
    /// The GPU's reader of each card, made the first time a tiled buffer is read (on the thread that reads).
    readers: RefCell<Vec<(usize, Result<EglReader, String>)>>,
    /// Tests: every buffer through the GPU, linear ones too.
    force_gpu: bool,
}

impl Kms {
    /// Every `/dev/dri/cardN` that opens, with universal planes (the pointer's own plane) where the card has them.
    pub fn open() -> Result<Kms, String> {
        let mut cards = Vec::new();
        let mut last_error = String::from("no /dev/dri/card*");
        for index in 0..16 {
            let path = format!("/dev/dri/card{index}");
            match OpenOptions::new()
                .read(true)
                .write(true)
                .custom_flags(libc::O_CLOEXEC)
                .open(&path)
            {
                Ok(card) => {
                    let mut cap = ClientCap {
                        capability: CLIENT_CAP_UNIVERSAL_PLANES,
                        value: 1,
                    };
                    let _ = ioctl(card.as_raw_fd(), SET_CLIENT_CAP, &mut cap);
                    for capability in [CLIENT_CAP_ATOMIC, CLIENT_CAP_CURSOR_PLANE_HOTSPOT] {
                        let mut cap = ClientCap {
                            capability,
                            value: 1,
                        };
                        let _ = ioctl(card.as_raw_fd(), SET_CLIENT_CAP, &mut cap);
                    }
                    cards.push((index, card));
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => last_error = format!("{path}: {error}"),
            }
        }
        if cards.is_empty() {
            return Err(last_error);
        }
        Ok(Kms {
            cards,
            readers: RefCell::new(Vec::new()),
            force_gpu: false,
        })
    }

    /// Tests: reads every buffer through the GPU (EGL), as a tiled one is read.
    pub fn force_gpu(&mut self, on: bool) {
        self.force_gpu = on;
    }

    /// How the GPU reads this card's buffers (made now if not yet), for reports.
    pub fn gpu_reader(&self, card: usize) -> Result<String, String> {
        self.with_reader(card, |reader| Ok(reader.description.clone()))
    }

    fn with_reader<T>(
        &self,
        card: usize,
        work: impl FnOnce(&mut EglReader) -> Result<T, String>,
    ) -> Result<T, String> {
        let mut readers = self.readers.borrow_mut();
        if !readers.iter().any(|(number, _)| *number == card) {
            readers.push((card, EglReader::new(Some(&format!("/dev/dri/card{card}")))));
        }
        match readers.iter_mut().find(|(number, _)| *number == card) {
            Some((_, Ok(reader))) => work(reader),
            Some((_, Err(error))) => Err(format!("the GPU cannot read it: {error}")),
            None => Err("no reader".into()),
        }
    }

    /// The area of a buffer read by the GPU: each plane exported as a DMA-BUF, imported with the buffer's layout.
    fn read_with_gpu(
        &self,
        card: usize,
        fd: RawFd,
        buffer: &Buffer,
        area: (u32, u32, u32, u32),
    ) -> Result<Vec<u8>, String> {
        let mut exported: Vec<(u32, OwnedFd)> = Vec::new();
        let mut planes = Vec::new();
        for &(handle, pitch, offset) in &buffer.planes {
            if !exported.iter().any(|(h, _)| *h == handle) {
                let mut prime = PrimeHandle {
                    handle,
                    flags: libc::O_CLOEXEC as u32,
                    fd: -1,
                };
                ioctl(fd, PRIME_HANDLE_TO_FD, &mut prime)
                    .map_err(|e| format!("DMA-BUF export: {e}"))?;
                exported.push((handle, unsafe { OwnedFd::from_raw_fd(prime.fd) }));
            }
            let plane_fd = exported
                .iter()
                .find(|(h, _)| *h == handle)
                .map(|(_, f)| f.as_raw_fd())
                .unwrap_or(-1);
            planes.push(Plane {
                fd: plane_fd,
                offset,
                pitch,
            });
        }
        self.with_reader(card, |reader| {
            reader.read(
                &planes,
                (buffer.width, buffer.height),
                buffer.format,
                buffer.modifier,
                area,
            )
        })
    }

    fn card(&self, index: usize) -> Result<RawFd, String> {
        self.cards
            .iter()
            .find(|(number, _)| *number == index)
            .map(|(_, card)| card.as_raw_fd())
            .ok_or_else(|| format!("card {index} is gone"))
    }

    /// The lit screens, card by card, CRTC by CRTC.
    pub fn screens(&self) -> Result<Vec<KmsScreen>, String> {
        let mut screens = Vec::new();
        for (index, card) in &self.cards {
            let fd = card.as_raw_fd();
            let mut res = CardRes::default();
            if ioctl(fd, GET_RESOURCES, &mut res).is_err() {
                // Not a display card (a render-only GPU's card node).
                continue;
            }
            let mut crtcs = vec![0u32; res.count_crtcs as usize];
            let mut res = CardRes {
                crtc_id_ptr: crtcs.as_mut_ptr() as u64,
                count_crtcs: crtcs.len() as u32,
                ..CardRes::default()
            };
            ioctl(fd, GET_RESOURCES, &mut res)
                .map_err(|e| format!("card{index}: resources: {e}"))?;
            crtcs.truncate(res.count_crtcs as usize);
            for crtc_id in crtcs {
                let crtc = get_crtc(fd, crtc_id)?;
                if crtc.mode_valid != 0 && crtc.fb_id != 0 {
                    screens.push(KmsScreen {
                        card: *index,
                        crtc: crtc_id,
                        width: u32::from(crtc.mode.hdisplay),
                        height: u32::from(crtc.mode.vdisplay),
                        left: crtc.x as i32,
                        top: crtc.y as i32,
                    });
                }
            }
        }
        Ok(screens)
    }

    /// The framebuffer the screen shows now: a new number with every page flip, the same while nothing changes.
    pub fn frame_id(&self, screen: &KmsScreen) -> Result<u32, String> {
        let crtc = get_crtc(self.card(screen.card)?, screen.crtc)?;
        if crtc.mode_valid == 0 || crtc.fb_id == 0 {
            return Err("the screen went dark".into());
        }
        Ok(crtc.fb_id)
    }

    /// What the screen's framebuffer is (its format, layout and size), for reports.
    pub fn describe(&self, screen: &KmsScreen) -> Result<String, String> {
        let fd = self.card(screen.card)?;
        let crtc = get_crtc(fd, screen.crtc)?;
        let buffer = Buffer::get(fd, crtc.fb_id)?;
        Ok(format!(
            "card{} CRTC {}: {}x{} at {},{} of a {}x{} {} framebuffer (pitch {}, modifier {})",
            screen.card,
            screen.crtc,
            screen.width,
            screen.height,
            screen.left,
            screen.top,
            buffer.width,
            buffer.height,
            String::from_utf8_lossy(&buffer.format.to_le_bytes()),
            buffer.pitch,
            buffer
                .modifier
                .map_or("none given".to_string(), |m| format!("{m:#x}"))
        ))
    }

    /// Where the pointer's plane is and what it holds, for reports.
    pub fn describe_pointer(&self, screen: &KmsScreen) -> String {
        let Ok(fd) = self.card(screen.card) else {
            return "no card".into();
        };
        match cursor_plane(fd, screen.crtc) {
            None => "no pointer plane in use".into(),
            Some((fb, x, y)) => match Buffer::get(fd, fb) {
                Ok(buffer) => format!(
                    "pointer plane at {x},{y}: {}x{} {} (modifier {:?})",
                    buffer.width,
                    buffer.height,
                    String::from_utf8_lossy(&buffer.format.to_le_bytes()),
                    buffer.modifier
                ),
                Err(error) => format!("pointer plane at {x},{y}: {error}"),
            },
        }
    }

    /// What changes when the screen does: its framebuffer (a new one with each page flip) and the pointer's plane
    /// (its framebuffer and place). An X server draws into one framebuffer in place, so a picture that changed can
    /// have the same state: compare the pictures too.
    pub fn state(&self, screen: &KmsScreen) -> Result<KmsState, String> {
        let fd = self.card(screen.card)?;
        Ok((self.frame_id(screen)?, cursor_plane(fd, screen.crtc)))
    }

    /// The screen's picture as it is shown now, with the pointer drawn in.
    pub fn capture(&self, screen: &KmsScreen) -> Result<BgraImage, String> {
        // A compositor removes the framebuffer it flipped away from: read the CRTC again if it went meanwhile.
        let mut tries = 0;
        loop {
            match self.capture_once(screen) {
                Err(error) if error.contains("(os error 2)") && tries < 3 => tries += 1,
                result => return result,
            }
        }
    }

    fn capture_once(&self, screen: &KmsScreen) -> Result<BgraImage, String> {
        let fd = self.card(screen.card)?;
        let crtc = get_crtc(fd, screen.crtc)?;
        if crtc.mode_valid == 0 || crtc.fb_id == 0 {
            return Err("the screen went dark".into());
        }
        let (width, height) = (u32::from(crtc.mode.hdisplay), u32::from(crtc.mode.vdisplay));
        let mut image = BgraImage {
            width,
            height,
            pixels: vec![0; width as usize * height as usize * 4],
        };
        let buffer = Buffer::get(fd, crtc.fb_id)?;
        let tiled = buffer.modifier.is_some_and(|m| m != MOD_LINEAR);
        if tiled || self.force_gpu {
            let area = (
                crtc.x,
                crtc.y,
                width.min(buffer.width.saturating_sub(crtc.x)),
                height.min(buffer.height.saturating_sub(crtc.y)),
            );
            let pixels = self.read_with_gpu(screen.card, fd, &buffer, area)?;
            let row = area.2 as usize * 4;
            for (y, source) in pixels.chunks_exact(row.max(1)).enumerate() {
                image.pixels[y * width as usize * 4..][..row].copy_from_slice(source);
            }
        } else if let Err(error) = buffer.copy_into(fd, crtc.x, crtc.y, &mut image) {
            // A linear buffer the driver would not map: the GPU reads it.
            let area = (
                crtc.x,
                crtc.y,
                width.min(buffer.width),
                height.min(buffer.height),
            );
            let pixels = self
                .read_with_gpu(screen.card, fd, &buffer, area)
                .map_err(|gpu| format!("{error}; {gpu}"))?;
            let row = area.2 as usize * 4;
            for (y, source) in pixels.chunks_exact(row.max(1)).enumerate() {
                image.pixels[y * width as usize * 4..][..row].copy_from_slice(source);
            }
        }
        if let Some((cursor_fb, x, y)) = cursor_plane(fd, screen.crtc)
            && let Ok(cursor) = Buffer::get(fd, cursor_fb)
        {
            let mut pointer = BgraImage {
                width: cursor.width,
                height: cursor.height,
                pixels: vec![0; cursor.width as usize * cursor.height as usize * 4],
            };
            if cursor.copy_into(fd, 0, 0, &mut pointer).is_ok() {
                blend(&mut image, &pointer, x, y);
            }
        }
        Ok(image)
    }
}

fn get_crtc(fd: RawFd, crtc_id: u32) -> Result<Crtc, String> {
    let mut crtc = Crtc {
        crtc_id,
        ..Crtc::default()
    };
    ioctl(fd, GET_CRTC, &mut crtc).map_err(|e| format!("CRTC {crtc_id}: {e}"))?;
    Ok(crtc)
}

/// A framebuffer's first plane: its buffer handle (ours until closed), layout and format.
struct Buffer {
    fd: RawFd,
    handles: Vec<u32>,
    /// Each plane: its handle, pitch and offset (a tiled layout may have a second plane: compression data).
    planes: Vec<(u32, u32, u32)>,
    handle: u32,
    width: u32,
    height: u32,
    pitch: u32,
    offset: u32,
    format: u32,
    modifier: Option<u64>,
}

impl Buffer {
    fn get(fd: RawFd, fb_id: u32) -> Result<Buffer, String> {
        let mut fb2 = Fb2 {
            fb_id,
            ..Fb2::default()
        };
        match ioctl(fd, GET_FB2, &mut fb2) {
            Ok(()) => {
                let mut handles: Vec<u32> =
                    fb2.handles.iter().copied().filter(|h| *h != 0).collect();
                handles.sort_unstable();
                handles.dedup();
                if fb2.handles[0] == 0 {
                    close_handles(fd, &handles);
                    return Err(
                        "the framebuffer's buffer was not handed over (CAP_SYS_ADMIN needed)"
                            .into(),
                    );
                }
                let planes = (0..4)
                    .filter(|&i| fb2.handles[i] != 0)
                    .map(|i| (fb2.handles[i], fb2.pitches[i], fb2.offsets[i]))
                    .collect();
                Ok(Buffer {
                    fd,
                    handles,
                    planes,
                    handle: fb2.handles[0],
                    width: fb2.width,
                    height: fb2.height,
                    pitch: fb2.pitches[0],
                    offset: fb2.offsets[0],
                    format: fb2.pixel_format,
                    modifier: (fb2.flags & MODE_FB_MODIFIERS != 0).then_some(fb2.modifier[0]),
                })
            }
            // Kernels before 5.7: GETFB (one plane, bits per pixel instead of a format).
            Err(error)
                if error.raw_os_error() == Some(libc::EINVAL)
                    || error.raw_os_error() == Some(libc::ENOTTY) =>
            {
                let mut fb = Fb {
                    fb_id,
                    ..Fb::default()
                };
                ioctl(fd, GET_FB, &mut fb).map_err(|e| format!("framebuffer {fb_id}: {e}"))?;
                if fb.handle == 0 {
                    return Err(
                        "the framebuffer's buffer was not handed over (CAP_SYS_ADMIN needed)"
                            .into(),
                    );
                }
                let format = match (fb.bpp, fb.depth) {
                    (32, 24) => XRGB8888,
                    (32, 32) => ARGB8888,
                    (32, 30) => XRGB2101010,
                    (16, 16) => RGB565,
                    (bpp, depth) => {
                        close_handles(fd, &[fb.handle]);
                        return Err(format!(
                            "framebuffer of {bpp} bits ({depth} deep): not read"
                        ));
                    }
                };
                Ok(Buffer {
                    fd,
                    handles: vec![fb.handle],
                    planes: vec![(fb.handle, fb.pitch, 0)],
                    handle: fb.handle,
                    width: fb.width,
                    height: fb.height,
                    pitch: fb.pitch,
                    offset: 0,
                    format,
                    modifier: None,
                })
            }
            Err(error) => Err(format!("framebuffer {fb_id}: {error}")),
        }
    }

    /// Copies the area `(left, top)` + the image's size (clipped to the buffer) into `image`, as B, G, R, A.
    fn copy_into(
        &self,
        fd: RawFd,
        left: u32,
        top: u32,
        image: &mut BgraImage,
    ) -> Result<(), String> {
        if let Some(modifier) = self.modifier.filter(|m| *m != MOD_LINEAR) {
            return Err(format!(
                "the screen's buffer is in the GPU's tiled layout (modifier {modifier:#x}): not readable here"
            ));
        }
        let bytes_per_pixel = match self.format {
            XRGB8888 | ARGB8888 | XBGR8888 | ABGR8888 | XRGB2101010 | ARGB2101010 => 4,
            RGB565 => 2,
            other => {
                return Err(format!(
                    "screen format {:?} not read",
                    String::from_utf8_lossy(&other.to_le_bytes())
                ));
            }
        };
        let size = self.offset as usize + self.pitch as usize * self.height as usize;
        let mapping = Mapping::new(fd, self.handle, size)?;
        mapping.sync(DMA_BUF_SYNC_START | DMA_BUF_SYNC_READ);
        let width = image.width.min(self.width.saturating_sub(left)) as usize;
        let rows = image.height.min(self.height.saturating_sub(top)) as usize;
        let source = mapping.bytes();
        for row in 0..rows {
            let start = self.offset as usize
                + (top as usize + row) * self.pitch as usize
                + left as usize * bytes_per_pixel;
            let from = &source[start..start + width * bytes_per_pixel];
            let target = &mut image.pixels[row * image.width as usize * 4..][..width * 4];
            convert_row(self.format, from, target);
        }
        mapping.sync(DMA_BUF_SYNC_END | DMA_BUF_SYNC_READ);
        Ok(())
    }
}

impl Drop for Buffer {
    /// GETFB2 gave this process handles of its own to the buffer: closed here (the buffer stays the compositor's).
    fn drop(&mut self) {
        close_handles(self.fd, &self.handles);
    }
}

fn close_handles(fd: RawFd, handles: &[u32]) {
    for &handle in handles {
        let mut close = GemClose { handle, pad: 0 };
        let _ = ioctl(fd, GEM_CLOSE, &mut close);
    }
}

/// One row into B, G, R, A.
fn convert_row(format: u32, from: &[u8], to: &mut [u8]) {
    match format {
        XRGB8888 | ARGB8888 => {
            to.copy_from_slice(from);
            if format == XRGB8888 {
                to.as_chunks_mut::<4>()
                    .0
                    .iter_mut()
                    .for_each(|p| p[3] = 255);
            }
        }
        XBGR8888 | ABGR8888 => {
            for (p, q) in from
                .as_chunks::<4>()
                .0
                .iter()
                .zip(to.as_chunks_mut::<4>().0.iter_mut())
            {
                q.copy_from_slice(&[
                    p[2],
                    p[1],
                    p[0],
                    if format == XBGR8888 { 255 } else { p[3] },
                ]);
            }
        }
        XRGB2101010 | ARGB2101010 => {
            for (p, q) in from
                .as_chunks::<4>()
                .0
                .iter()
                .zip(to.as_chunks_mut::<4>().0.iter_mut())
            {
                let v = u32::from_le_bytes([p[0], p[1], p[2], p[3]]);
                let channel = |shift: u32| ((v >> shift) & 0x3ff) as u16 >> 2;
                q.copy_from_slice(&[channel(0) as u8, channel(10) as u8, channel(20) as u8, 255]);
            }
        }
        RGB565 => {
            for (p, q) in from
                .as_chunks::<2>()
                .0
                .iter()
                .zip(to.as_chunks_mut::<4>().0.iter_mut())
            {
                let v = u16::from_le_bytes([p[0], p[1]]);
                let (r, g, b) = ((v >> 11) & 0x1f, (v >> 5) & 0x3f, v & 0x1f);
                q.copy_from_slice(&[
                    ((b << 3) | (b >> 2)) as u8,
                    ((g << 2) | (g >> 4)) as u8,
                    ((r << 3) | (r >> 2)) as u8,
                    255,
                ]);
            }
        }
        _ => {}
    }
}

/// A buffer mapped for reading: through a DMA-BUF of it where the driver exports one, else as a dumb buffer.
struct Mapping {
    pointer: *mut libc::c_void,
    length: usize,
    dmabuf: Option<OwnedFd>,
}

impl Mapping {
    fn new(fd: RawFd, handle: u32, length: usize) -> Result<Mapping, String> {
        let mut prime = PrimeHandle {
            handle,
            flags: libc::O_CLOEXEC as u32,
            fd: -1,
        };
        let mut errors = Vec::new();
        match ioctl(fd, PRIME_HANDLE_TO_FD, &mut prime) {
            Ok(()) => {
                let dmabuf = unsafe { OwnedFd::from_raw_fd(prime.fd) };
                let pointer = unsafe {
                    libc::mmap(
                        std::ptr::null_mut(),
                        length,
                        libc::PROT_READ,
                        libc::MAP_SHARED,
                        dmabuf.as_raw_fd(),
                        0,
                    )
                };
                if pointer != libc::MAP_FAILED {
                    return Ok(Mapping {
                        pointer,
                        length,
                        dmabuf: Some(dmabuf),
                    });
                }
                errors.push(format!("DMA-BUF mmap: {}", std::io::Error::last_os_error()));
            }
            Err(error) => errors.push(format!("DMA-BUF export: {error}")),
        }
        let mut map = MapDumb {
            handle,
            ..MapDumb::default()
        };
        match ioctl(fd, MAP_DUMB, &mut map) {
            Ok(()) => {
                let pointer = unsafe {
                    libc::mmap(
                        std::ptr::null_mut(),
                        length,
                        libc::PROT_READ,
                        libc::MAP_SHARED,
                        fd,
                        map.offset as libc::off_t,
                    )
                };
                if pointer != libc::MAP_FAILED {
                    return Ok(Mapping {
                        pointer,
                        length,
                        dmabuf: None,
                    });
                }
                errors.push(format!("dumb mmap: {}", std::io::Error::last_os_error()));
            }
            Err(error) => errors.push(format!("dumb map: {error}")),
        }
        Err(format!(
            "the screen's buffer cannot be mapped ({})",
            errors.join("; ")
        ))
    }

    fn bytes(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.pointer.cast::<u8>(), self.length) }
    }

    fn sync(&self, flags: u64) {
        if let Some(dmabuf) = &self.dmabuf {
            let mut flags = flags;
            let _ = ioctl(dmabuf.as_raw_fd(), DMA_BUF_SYNC, &mut flags);
        }
    }
}

impl Drop for Mapping {
    fn drop(&mut self) {
        unsafe { libc::munmap(self.pointer, self.length) };
    }
}

/// The pointer's plane on this CRTC, if it shows one: its framebuffer and where it is (CRTC_X, CRTC_Y).
fn cursor_plane(fd: RawFd, crtc_id: u32) -> Option<(u32, i32, i32)> {
    let mut res = PlaneRes::default();
    ioctl(fd, GET_PLANE_RESOURCES, &mut res).ok()?;
    let mut planes = vec![0u32; res.count_planes as usize];
    let mut res = PlaneRes {
        plane_id_ptr: planes.as_mut_ptr() as u64,
        count_planes: planes.len() as u32,
    };
    ioctl(fd, GET_PLANE_RESOURCES, &mut res).ok()?;
    planes.truncate(res.count_planes as usize);
    for plane_id in planes {
        let mut plane = GetPlane {
            plane_id,
            ..GetPlane::default()
        };
        if ioctl(fd, GET_PLANE, &mut plane).is_err() || plane.crtc_id != crtc_id || plane.fb_id == 0
        {
            continue;
        }
        let properties = plane_properties(fd, plane_id)?;
        let value = |name: &str| properties.iter().find(|(n, _)| n == name).map(|(_, v)| *v);
        if value("type") != Some(PLANE_TYPE_CURSOR) {
            continue;
        }
        // Signed values in a 64-bit property.
        let x = value("CRTC_X").unwrap_or(0) as i64 as i32;
        let y = value("CRTC_Y").unwrap_or(0) as i64 as i32;
        return Some((plane.fb_id, x, y));
    }
    None
}

fn plane_properties(fd: RawFd, plane_id: u32) -> Option<Vec<(String, u64)>> {
    let mut props = ObjProps {
        obj_id: plane_id,
        obj_type: OBJECT_PLANE,
        ..ObjProps::default()
    };
    ioctl(fd, OBJ_GET_PROPERTIES, &mut props).ok()?;
    let mut ids = vec![0u32; props.count_props as usize];
    let mut values = vec![0u64; props.count_props as usize];
    let mut props = ObjProps {
        props_ptr: ids.as_mut_ptr() as u64,
        prop_values_ptr: values.as_mut_ptr() as u64,
        count_props: ids.len() as u32,
        obj_id: plane_id,
        obj_type: OBJECT_PLANE,
    };
    ioctl(fd, OBJ_GET_PROPERTIES, &mut props).ok()?;
    let count = (props.count_props as usize).min(ids.len());
    Some(
        ids[..count]
            .iter()
            .zip(&values[..count])
            .filter_map(|(&prop_id, &value)| {
                let mut property = GetProperty {
                    prop_id,
                    ..GetProperty::default()
                };
                ioctl(fd, GET_PROPERTY, &mut property).ok()?;
                let end = property.name.iter().position(|&b| b == 0).unwrap_or(32);
                Some((
                    String::from_utf8_lossy(&property.name[..end]).into_owned(),
                    value,
                ))
            })
            .collect(),
    )
}

/// The pointer over the picture (premultiplied alpha, as cursor planes are).
fn blend(image: &mut BgraImage, pointer: &BgraImage, left: i32, top: i32) {
    for row in 0..pointer.height as i32 {
        let y = top + row;
        if y < 0 || y >= image.height as i32 {
            continue;
        }
        for column in 0..pointer.width as i32 {
            let x = left + column;
            if x < 0 || x >= image.width as i32 {
                continue;
            }
            let from = &pointer.pixels
                [(row as usize * pointer.width as usize + column as usize) * 4..][..4];
            let alpha = u32::from(from[3]);
            if alpha == 0 {
                continue;
            }
            let to = &mut image.pixels[(y as usize * image.width as usize + x as usize) * 4..][..4];
            for channel in 0..3 {
                to[channel] = (u32::from(from[channel])
                    + u32::from(to[channel]) * (255 - alpha) / 255)
                    .min(255) as u8;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_kernel_structures_have_the_kernels_sizes() {
        // From drm.h / drm_mode.h (x86_64): any difference would make each ioctl number wrong.
        assert_eq!(size_of::<CardRes>(), 64);
        assert_eq!(size_of::<ModeInfo>(), 68);
        assert_eq!(size_of::<Crtc>(), 104);
        assert_eq!(size_of::<PlaneRes>(), 16);
        assert_eq!(size_of::<GetPlane>(), 32);
        assert_eq!(size_of::<ObjProps>(), 32);
        assert_eq!(size_of::<GetProperty>(), 64);
        assert_eq!(size_of::<Fb2>(), 104);
        assert_eq!(size_of::<Fb>(), 28);
        assert_eq!(size_of::<MapDumb>(), 16);
        assert_eq!(size_of::<PrimeHandle>(), 12);
        // DRM_IOCTL_MODE_GETFB2 and DRM_IOCTL_MODE_GETCRTC as libdrm's headers give them.
        assert_eq!(GET_FB2, 0xC06864CE);
        assert_eq!(GET_CRTC, 0xC06864A1);
        assert_eq!(MAP_DUMB, 0xC01064B3);
        assert_eq!(PRIME_HANDLE_TO_FD, 0xC00C642D);
    }

    #[test]
    fn formats_come_out_as_bgra() {
        let mut out = [0u8; 4];
        convert_row(XBGR8888, &[10, 20, 30, 0], &mut out);
        assert_eq!(out, [30, 20, 10, 255]);
        convert_row(XRGB8888, &[10, 20, 30, 0], &mut out);
        assert_eq!(out, [10, 20, 30, 255]);
        let white_565 = 0xffffu16.to_le_bytes();
        convert_row(RGB565, &white_565, &mut out);
        assert_eq!(out, [255, 255, 255, 255]);
        let red_30 = (0x3ffu32 << 20).to_le_bytes();
        convert_row(XRGB2101010, &red_30, &mut out);
        assert_eq!(out, [0, 0, 255, 255]);
    }

    #[test]
    fn the_pointer_is_drawn_over_the_picture() {
        let mut image = BgraImage {
            width: 4,
            height: 4,
            pixels: vec![100; 64],
        };
        let mut pointer = BgraImage {
            width: 2,
            height: 2,
            pixels: vec![0; 16],
        };
        pointer.pixels[..4].copy_from_slice(&[255, 255, 255, 255]);
        blend(&mut image, &pointer, -1, 1);
        // Only its top right pixel lands (column 1 at x 0): transparent there, so nothing changes; move it in.
        blend(&mut image, &pointer, 1, 1);
        assert_eq!(&image.pixels[(4 + 1) * 4..][..3], &[255, 255, 255]);
        assert_eq!(&image.pixels[0..3], &[100, 100, 100]);
    }
}
