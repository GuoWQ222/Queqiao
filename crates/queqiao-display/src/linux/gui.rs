//! Queqiao's own windows on Linux (docs/LINUX.md step 5): one GUI thread per process ("queqiao-gui") runs egui
//! (eframe, OpenGL; Mesa's software renderer where there is no GPU) and draws every form, prompt, banner and main
//! window of the process, each a window of its own (an egui viewport). The code that opens one (the agent's UI
//! thread, a viewer's session) holds a handle and polls it, as Windows' forms are polled; the handles and the
//! windows share their state. The thread starts with the first window and lives as long as the process.
//!
//! Chinese and Japanese texts (Queqiao's languages) need a font with those characters: the system's own, found
//! through fontconfig, is added after egui's; none is bundled.

use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Mutex, OnceLock};
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
}

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
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        for request in self.incoming.try_iter() {
            match request {
                Request::Open(id, view) => self.views.push((id, view)),
                Request::Close(id) => self.views.retain(|(open, _)| *open != id),
            }
        }
        self.views.retain_mut(|(id, view)| {
            let mut keep = true;
            // The same builder each time: egui applies only what changed.
            ctx.show_viewport_immediate(
                egui::ViewportId::from_hash_of(("queqiao", *id)),
                view.builder(),
                |ctx, _| {
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
        // Requests from the handles wake the thread; a countdown or a status line asks for its own repaints.
        ctx.request_repaint_after(Duration::from_millis(250));
    }
}

/// The system's fonts for Chinese and Japanese, after egui's own (found through fontconfig's fc-match).
fn add_system_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    for (name, pattern) in [
        ("cjk-sc", "sans-serif:lang=zh-cn"),
        ("cjk-ja", "sans-serif:lang=ja"),
    ] {
        let Some(path) = std::process::Command::new("fc-match")
            .args(["-f", "%{file}", pattern])
            .output()
            .ok()
            .filter(|out| out.status.success())
            .map(|out| String::from_utf8_lossy(&out.stdout).into_owned())
        else {
            continue;
        };
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        fonts.font_data.insert(
            name.into(),
            std::sync::Arc::new(egui::FontData::from_owned(bytes)),
        );
        for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
            fonts.families.entry(family).or_default().push(name.into());
        }
    }
    ctx.set_fonts(fonts);
}
