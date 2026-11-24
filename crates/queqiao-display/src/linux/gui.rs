//! Queqiao's own windows on Linux (docs/LINUX.md step 5): one GUI thread per process ("queqiao-gui") runs egui
//! (eframe, OpenGL; Mesa's software renderer where there is no GPU) and draws every form, prompt, banner and main
//! window of the process, each a window of its own (an egui viewport). The code that opens one (the agent's UI
//! thread, a viewer's session) holds a handle and polls it, as Windows' forms are polled; the handles and the
//! windows share their state. The thread starts with the first window and lives as long as the process.
//!
//! Chinese and Japanese texts (Queqiao's languages) need a font with those characters: the system's own, found
//! through fontconfig, is added after egui's; none is bundled.

use std::path::Path;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, OnceLock};

use eframe::glow::{self, HasContext};
use std::time::Duration;

use eframe::egui;
use winit::platform::x11::EventLoopBuilderExtX11;

/// One window drawn by the GUI thread.
pub(crate) trait View: Send {
    /// How the window is made (title, size, on top, frame); asked once, when it opens.
    fn builder(&self) -> egui::ViewportBuilder;
    /// Draws the window; false when it closes itself (a button that ends it).
    fn draw(&mut self, ctx: &egui::Context) -> bool;
    /// The person closed the window (its frame's close button).
    fn closed(&mut self);
}

enum Request {
    Open(u64, Box<dyn View>),
    Close(u64),
    /// A picture of the window, as egui draws it.
    Picture(u64, Sender<Picture>),
}

/// A window's picture: width, height, and its pixels top row first, 4 bytes each (blue, green, red, unused).
type Picture = (usize, usize, Vec<u8>);

struct Gui {
    requests: Sender<Request>,
    context: egui::Context,
}

static GUI: OnceLock<Result<Gui, String>> = OnceLock::new();
static NEXT_ID: Mutex<u64> = Mutex::new(1);

/// The GUI thread, started on first use; an error when this desktop cannot show windows (no display, no OpenGL).
fn gui() -> Result<&'static Gui, String> {
    GUI.get_or_init(start).as_ref().map_err(Clone::clone)
}

fn start() -> Result<Gui, String> {
    let (requests, incoming) = mpsc::channel();
    let (ready, started) = mpsc::channel::<Result<egui::Context, String>>();
    let ready_on_error = ready.clone();
    std::thread::Builder::new()
        .name("queqiao-gui".into())
        .spawn(move || {
            let options = eframe::NativeOptions {
                // The root window is not shown: every window is a viewport of its own.
                viewport: egui::ViewportBuilder::default()
                    .with_title("Queqiao")
                    .with_visible(false)
                    .with_inner_size([1.0, 1.0]),
                event_loop_builder: Some(Box::new(|builder| {
                    builder.with_any_thread(true);
                })),
                run_and_return: false,
                ..Default::default()
            };
            let result = eframe::run_native(
                "queqiao",
                options,
                Box::new(move |creation| {
                    add_system_fonts(&creation.egui_ctx);
                    let _ = ready.send(Ok(creation.egui_ctx.clone()));
                    Ok(Box::new(App {
                        incoming,
                        views: Vec::new(),
                        pictures: Vec::new(),
                    }))
                }),
            );
            if let Err(error) = result {
                let _ = ready_on_error.send(Err(format!("the GUI did not start: {error}")));
            }
        })
        .map_err(|e| e.to_string())?;
    let context = started
        .recv_timeout(Duration::from_secs(20))
        .map_err(|_| "the GUI did not start in time".to_string())??;
    Ok(Gui { requests, context })
}

/// A window on screen while this lives (closed when dropped).
pub(crate) struct Opened {
    id: u64,
}

impl Drop for Opened {
    fn drop(&mut self) {
        if let Ok(gui) = gui() {
            let _ = gui.requests.send(Request::Close(self.id));
            gui.context.request_repaint();
        }
    }
}

/// The bit that marks a handle (Hwnd) as one of these windows rather than an X window (whose ids stay below 2^32).
pub(crate) const GUI_WINDOW: usize = 1 << 40;

impl Opened {
    /// A handle for this window, for what takes an X window's (save_window_picture).
    pub(crate) fn hwnd(&self) -> super::window::Hwnd {
        super::window::Hwnd(GUI_WINDOW | self.id as usize)
    }

    /// A picture of the window as egui draws it (its contents, whatever lies on top), saved as a bitmap file (Windows:
    /// the forms' save_picture).
    pub(crate) fn save_picture(&self, path: &Path) -> std::io::Result<()> {
        save_picture_of(self.id, path)
    }
}

/// The picture of window `id` (see `Opened::save_picture`).
pub(crate) fn save_picture_of(id: u64, path: &Path) -> std::io::Result<()> {
    let gui = gui().map_err(std::io::Error::other)?;
    let (sender, picture) = mpsc::channel();
    gui.requests
        .send(Request::Picture(id, sender))
        .map_err(|_| std::io::Error::other("the GUI thread ended"))?;
    gui.context.request_repaint();
    let (width, height, bgrx) = picture
        .recv_timeout(Duration::from_secs(5))
        .map_err(|_| std::io::Error::other("no picture of the window within 5 s"))?;
    std::fs::write(path, super::forms::bitmap_file(width, height, &bgrx))
}

/// Opens `view` as a window of its own.
pub(crate) fn open(view: Box<dyn View>) -> Result<Opened, String> {
    let gui = gui()?;
    let id = {
        let mut next = NEXT_ID.lock().map_err(|_| "poisoned".to_string())?;
        *next += 1;
        *next
    };
    gui.requests
        .send(Request::Open(id, view))
        .map_err(|_| "the GUI thread ended".to_string())?;
    gui.context.request_repaint();
    Ok(Opened { id })
}

/// Asks the GUI thread to draw again (a handle changed what a window shows).
pub(crate) fn repaint() {
    if let Ok(gui) = gui() {
        gui.context.request_repaint();
    }
}

struct App {
    incoming: Receiver<Request>,
    views: Vec<(u64, Box<dyn View>)>,
    /// Pictures asked for: the window and who waits.
    pictures: Vec<(u64, Sender<Picture>)>,
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        for request in self.incoming.try_iter() {
            match request {
                Request::Open(id, view) => self.views.push((id, view)),
                Request::Close(id) => self.views.retain(|(open, _)| *open != id),
                Request::Picture(id, sender) => self.pictures.push((id, sender)),
            }
        }
        let pictures = &mut self.pictures;
        self.views.retain_mut(|(id, view)| {
            let id = *id;
            let mut keep = true;
            // The same builder each time: egui applies only what changed.
            ctx.show_viewport_immediate(
                egui::ViewportId::from_hash_of(("queqiao", id)),
                view.builder(),
                |ctx, _| {
                    // A picture asked for: read once this frame is drawn (the callback comes last, on the top
                    // layer).
                    if let Some(at) = pictures.iter().position(|picture| picture.0 == id) {
                        let (_, sender) = pictures.remove(at);
                        take_picture(ctx, sender);
                    }
                    if ctx.input(|i| i.viewport().close_requested()) {
                        view.closed();
                        keep = false;
                        return;
                    }
                    keep = view.draw(ctx);
                },
            );
            keep
        });
        // Pictures of windows that closed meanwhile: nobody waits for them any more.
        let open: Vec<u64> = self.views.iter().map(|(id, _)| *id).collect();
        self.pictures.retain(|(id, _)| open.contains(id));
        // Requests from the handles wake the thread; a countdown or a status line asks for its own repaints.
        ctx.request_repaint_after(Duration::from_millis(250));
    }
}

/// Reads the window's picture at the end of this frame: a paint callback on the top layer, so everything else is
/// drawn by then, reads the frame being drawn (OpenGL's back buffer) and sends it.
fn take_picture(ctx: &egui::Context, sender: Sender<Picture>) {
    let callback = eframe::egui_glow::CallbackFn::new(move |info, painter| {
        let [width, height] = info.screen_size_px.map(|side| side as usize);
        let mut rgba = vec![0u8; width * height * 4];
        let gl = painter.gl();
        unsafe {
            gl.pixel_store_i32(glow::PACK_ALIGNMENT, 1);
            gl.read_pixels(
                0,
                0,
                width as i32,
                height as i32,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelPackData::Slice(Some(&mut rgba)),
            );
        }
        // OpenGL's rows go bottom up; the picture's top down, blue first.
        let mut bgrx = Vec::with_capacity(rgba.len());
        for row in rgba.chunks_exact(width * 4).rev() {
            for pixel in row.as_chunks::<4>().0 {
                bgrx.extend_from_slice(&[pixel[2], pixel[1], pixel[0], 255]);
            }
        }
        let _ = sender.send((width, height, bgrx));
    });
    ctx.layer_painter(egui::LayerId::new(
        egui::Order::Debug,
        egui::Id::new("queqiao-picture"),
    ))
    .add(egui::PaintCallback {
        rect: ctx.screen_rect(),
        callback: Arc::new(callback),
    });
}

/// The system's font files for Chinese and Japanese (found through fontconfig's fc-match).
pub(crate) fn system_font_files() -> Vec<String> {
    ["sans-serif:lang=zh-cn", "sans-serif:lang=ja"]
        .into_iter()
        .filter_map(|pattern| {
            std::process::Command::new("fc-match")
                .args(["-f", "%{file}", pattern])
                .output()
                .ok()
                .filter(|out| out.status.success())
                .map(|out| String::from_utf8_lossy(&out.stdout).into_owned())
                .filter(|path| !path.is_empty())
        })
        .collect()
}

/// The system's fonts for Chinese and Japanese, after egui's own.
fn add_system_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    for (index, path) in system_font_files().into_iter().enumerate() {
        let name = format!("system-{index}");
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        fonts.font_data.insert(
            name.clone(),
            std::sync::Arc::new(egui::FontData::from_owned(bytes)),
        );
        for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
            fonts.families.entry(family).or_default().push(name.clone());
        }
    }
    ctx.set_fonts(fonts);
}
