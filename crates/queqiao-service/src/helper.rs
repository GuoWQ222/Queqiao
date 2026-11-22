//! The desktop helper: SYSTEM, in the console session, because only SYSTEM may see and use the "Winlogon" desktop
//! (the sign-in, lock, Ctrl+Alt+Del and UAC screens). It talks only to the agent, over the channel the service
//! handed to both, and does three things:
//! - tells the agent which desktop takes input, whenever that changes;
//! - while asked to, captures a screen of that desktop and sends its pictures (at most 30 a second);
//! - during a session, injects the agent's input there, but never on the person's own desktop ("Default"):
//!   there the agent injects as the person, with the person's rights;
//! - during a session, shows the session banner (with End) on any other desktop: the person's own banner is on
//!   their desktop, out of sight on the sign-in, lock and UAC screens.
//!
//! It ends when the agent's channel closes, or at the first message it does not know.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use queqiao_capture::{Duplicator, needs_restart};
use queqiao_display::SessionBanner;
use queqiao_input::{Injector, ScreenRect};
use queqiao_local::{Ends, FromHelper, ToHelper, Writer};
use windows::Win32::Foundation::{GENERIC_ALL, HANDLE};
use windows::Win32::System::StationsAndDesktops::{
    CloseDesktop, DESKTOP_ACCESS_FLAGS, DESKTOP_CONTROL_FLAGS, GetUserObjectInformationW, HDESK,
    OpenInputDesktop, SetThreadDesktop, UOI_NAME,
};

use crate::log;

/// The signed-in person's own desktop.
const OWN_DESKTOP: &str = "Default";
const FRAME_GAP: Duration = Duration::from_millis(33);

/// A desktop handle, closed when dropped (which fails while a thread is on it: drop it after).
struct Desktop(HDESK);

unsafe impl Send for Desktop {}

impl Drop for Desktop {
    fn drop(&mut self) {
        let _ = unsafe { CloseDesktop(self.0) };
    }
}

/// The desktop that takes input now, and its name.
fn input_desktop() -> Result<(Desktop, String), String> {
    let desktop = unsafe {
        OpenInputDesktop(
            DESKTOP_CONTROL_FLAGS(0),
            false,
            DESKTOP_ACCESS_FLAGS(GENERIC_ALL.0),
        )
    }
    .map_err(|e| format!("OpenInputDesktop: {e}"))?;
    let desktop = Desktop(desktop);
    let mut name = [0u16; 128];
    let mut needed = 0u32;
    unsafe {
        GetUserObjectInformationW(
            HANDLE(desktop.0.0),
            UOI_NAME,
            Some(name.as_mut_ptr().cast()),
            (name.len() * 2) as u32,
            Some(&mut needed),
        )
    }
    .map_err(|e| format!("GetUserObjectInformationW: {e}"))?;
    let length = name.iter().position(|&c| c == 0).unwrap_or(name.len());
    Ok((desktop, String::from_utf16_lossy(&name[..length])))
}

struct Shared {
    /// The screen to capture, if asked.
    capture: Mutex<Option<u8>>,
    /// Changes whenever capture must start again (asked anew, stopped, or the desktop changed).
    generation: AtomicU64,
    session: AtomicBool,
    /// The viewer the banner names (shown during a session, off the person's own desktop).
    banner: Mutex<Option<String>>,
    /// Changes whenever the banner must be shown anew (named, removed, the session or the desktop changed).
    banner_generation: AtomicU64,
    writer: Mutex<Writer>,
}

impl Shared {
    fn send(&self, message: &FromHelper) -> bool {
        self.writer
            .lock()
            .is_ok_and(|mut writer| writer.write(&message.encode()).is_ok())
    }
}

pub fn run(args: &[String]) -> Result<(), String> {
    let ends = match args {
        [flag, value] if flag == "--channel" => Ends::parse(value),
        _ => None,
    }
    .ok_or("usage: queqiao-service desktop --channel <read>,<write> (the service starts it)")?;
    let _ = queqiao_core::platform::prepare_process();
    let (mut reader, writer) = ends.split();
    let shared = Arc::new(Shared {
        capture: Mutex::new(None),
        generation: AtomicU64::new(0),
        session: AtomicBool::new(false),
        banner: Mutex::new(None),
        banner_generation: AtomicU64::new(0),
        writer: Mutex::new(writer),
    });
    log::line(&format!(
        "desktop helper: started (privileges: {})",
        queqiao_core::platform::own_privileges().join(", ")
    ));
    {
        let shared = shared.clone();
        std::thread::spawn(move || watch(&shared));
    }
    {
        let shared = shared.clone();
        std::thread::spawn(move || capture_loop(&shared));
    }
    {
        let shared = shared.clone();
        std::thread::spawn(move || banner_loop(&shared));
    }
    let mut injecting = Injecting::default();
    let ending = loop {
        let message = match reader.read() {
            Ok(message) => message,
            Err(error) => break format!("the agent's channel: {error}"),
        };
        match ToHelper::decode(&message) {
            Some(ToHelper::Capture { screen }) => {
                if let Ok(mut capture) = shared.capture.lock() {
                    *capture = Some(screen);
                }
                shared.generation.fetch_add(1, Ordering::SeqCst);
            }
            Some(ToHelper::StopCapture) => {
                if let Ok(mut capture) = shared.capture.lock() {
                    *capture = None;
                }
                shared.generation.fetch_add(1, Ordering::SeqCst);
            }
            Some(ToHelper::Session(on)) => {
                shared.session.store(on, Ordering::SeqCst);
                shared.banner_generation.fetch_add(1, Ordering::SeqCst);
                log::line(&format!(
                    "desktop helper: a session {}",
                    if on { "started" } else { "ended" }
                ));
            }
            Some(ToHelper::Input {
                event,
                left,
                top,
                width,
                height,
            }) => {
                if shared.session.load(Ordering::SeqCst) {
                    injecting.inject(
                        &event,
                        ScreenRect {
                            left,
                            top,
                            width,
                            height,
                        },
                    );
                } else {
                    injecting.refused += 1;
                }
            }
            Some(ToHelper::Banner(viewer)) => {
                if let Ok(mut banner) = shared.banner.lock() {
                    *banner = viewer;
                }
                shared.banner_generation.fetch_add(1, Ordering::SeqCst);
            }
            None => break "a message it does not know".to_string(),
        }
    };
    log::line(&format!(
        "desktop helper: ending ({ending}); injected {}, refused {}{}",
        injecting.injected,
        injecting.refused,
        if injecting.last_error.is_empty() {
            String::new()
        } else {
            format!(", last error: {}", injecting.last_error)
        }
    ));
    Ok(())
}

/// Input on the desktop that takes input: this thread follows it.
#[derive(Default)]
struct Injecting {
    attached: Option<(Desktop, String)>,
    injected: u64,
    refused: u64,
    last_error: String,
}

impl Injecting {
    fn inject(&mut self, event: &queqiao_core::protocol::InputEvent, screen: ScreenRect) {
        match self.attach() {
            Ok(name) if name == OWN_DESKTOP => {
                self.refused += 1;
                return;
            }
            Ok(_) => {}
            Err(error) => {
                self.refused += 1;
                self.last_error = error;
                return;
            }
        }
        match Injector::new(screen).inject(event) {
            Ok(()) => self.injected += 1,
            Err(error) => {
                self.refused += 1;
                self.last_error = error;
            }
        }
    }

    /// Moves this thread to the input desktop if it changed; its name.
    fn attach(&mut self) -> Result<String, String> {
        let (desktop, name) = input_desktop()?;
        if self
            .attached
            .as_ref()
            .is_some_and(|(_, current)| *current == name)
        {
            return Ok(name);
        }
        unsafe { SetThreadDesktop(desktop.0) }.map_err(|e| format!("SetThreadDesktop: {e}"))?;
        self.attached = Some((desktop, name.clone()));
        Ok(name)
    }
}

/// Tells the agent which desktop takes input, at once and whenever it changes.
fn watch(shared: &Shared) {
    let mut last = String::new();
    loop {
        if let Ok((_, name)) = input_desktop()
            && name != last
        {
            if !shared.send(&FromHelper::Desktop(name.clone())) {
                return;
            }
            log::line(&format!("desktop helper: input desktop {name}"));
            last = name;
            shared.generation.fetch_add(1, Ordering::SeqCst);
            shared.banner_generation.fetch_add(1, Ordering::SeqCst);
        }
        std::thread::sleep(Duration::from_millis(150));
    }
}

/// Captures while asked: each round on a new thread, which attaches to the input desktop of that moment (a thread
/// can change desktops only while it has no windows).
fn capture_loop(shared: &Arc<Shared>) {
    let mut last_error = String::new();
    loop {
        let screen = shared.capture.lock().ok().and_then(|capture| *capture);
        let Some(screen) = screen else {
            std::thread::sleep(Duration::from_millis(30));
            continue;
        };
        let generation = shared.generation.load(Ordering::SeqCst);
        let worker = {
            let shared = shared.clone();
            std::thread::spawn(move || {
                let mut attached = None;
                let result = capture_round(&shared, screen, generation, &mut attached);
                (attached, result)
            })
        };
        // The round's desktop is closed once its thread is gone.
        match worker.join() {
            Ok((_, Ok(()))) => last_error.clear(),
            Ok((_, Err(error))) => {
                if error != last_error {
                    log::line(&format!("desktop helper: capture: {error}"));
                    if !shared.send(&FromHelper::Failed(error.clone())) {
                        return;
                    }
                    last_error = error;
                }
                std::thread::sleep(Duration::from_millis(300));
            }
            Err(_) => std::thread::sleep(Duration::from_millis(300)),
        }
    }
}

fn capture_round(
    shared: &Shared,
    screen: u8,
    generation: u64,
    attached: &mut Option<Desktop>,
) -> Result<(), String> {
    let (desktop, name) = input_desktop()?;
    unsafe { SetThreadDesktop(desktop.0) }.map_err(|e| format!("SetThreadDesktop: {e}"))?;
    *attached = Some(desktop);
    let mut duplicator =
        Duplicator::new(usize::from(screen)).map_err(|e| format!("capture on {name}: {e}"))?;
    // Where the screen is, before its first picture (the agent for the sign-in screen cannot see the screens).
    let info = duplicator.info().clone();
    if !shared.send(&FromHelper::Screen {
        left: info.left,
        top: info.top,
        width: info.width,
        height: info.height,
    }) {
        return Ok(());
    }
    let send = |width: u32, height: u32, pixels: Vec<u8>| {
        shared.send(&FromHelper::Frame {
            width,
            height,
            pixels,
        })
    };
    // The first picture at once, whether or not the screen changes.
    let first = duplicator
        .snapshot_image()
        .map_err(|e| format!("snapshot on {name}: {e}"))?;
    if !send(first.width, first.height, first.pixels) {
        return Ok(());
    }
    let mut last_sent = Instant::now();
    while shared.generation.load(Ordering::SeqCst) == generation {
        match duplicator.acquire(100) {
            Ok(Some(frame)) if frame.present_qpc != 0 => {
                std::thread::sleep(FRAME_GAP.saturating_sub(last_sent.elapsed()));
                let image = duplicator
                    .copy_to_cpu(&frame)
                    .map_err(|e| format!("copy on {name}: {e}"))?;
                if !send(image.width, image.height, image.pixels) {
                    return Ok(());
                }
                last_sent = Instant::now();
            }
            Ok(_) => {}
            // The desktop switched or the display changed: the next round opens it again.
            Err(error) if needs_restart(&error) => return Ok(()),
            Err(error) => return Err(format!("capture on {name}: {error}")),
        }
    }
    Ok(())
}

/// How long the banner may fail on the desktop taking input before the session ends.
const BANNER_GRACE: Duration = Duration::from_secs(3);

/// Shows the session banner while a session runs and a desktop other than the person's own takes input; each
/// round on a fresh thread attached to that desktop (a thread with a window cannot change desktops). If it cannot be
/// shown for BANNER_GRACE, the agent is told and ends the session (FromHelper::NoBanner).
fn banner_loop(shared: &Arc<Shared>) {
    let mut last_error = String::new();
    // Since when the banner has failed in a row, and the showing (generation) the agent was told about.
    let mut failing: Option<std::time::Instant> = None;
    let mut told: Option<u64> = None;
    loop {
        let generation = shared.banner_generation.load(Ordering::SeqCst);
        let viewer = shared
            .banner
            .lock()
            .ok()
            .and_then(|banner| banner.clone())
            .filter(|_| shared.session.load(Ordering::SeqCst));
        let mut retry = false;
        if let Some(viewer) = viewer {
            let worker = {
                let shared = shared.clone();
                std::thread::spawn(move || {
                    let mut attached = None;
                    let result = banner_round(&shared, &viewer, generation, &mut attached);
                    (attached, result)
                })
            };
            match worker.join() {
                Ok((_, Err(error))) => {
                    let since = *failing.get_or_insert_with(std::time::Instant::now);
                    if since.elapsed() >= BANNER_GRACE && told != Some(generation) {
                        log::line(&format!(
                            "desktop helper: no banner for {} s ({error}): the session ends",
                            BANNER_GRACE.as_secs()
                        ));
                        let _ = shared.send(&FromHelper::NoBanner(error.clone()));
                        told = Some(generation);
                    }
                    if error != last_error {
                        log::line(&format!("desktop helper: banner: {error}"));
                        last_error = error;
                    }
                    retry = true;
                }
                Ok((_, Ok(()))) => failing = None,
                Err(_) => {}
            }
        } else {
            failing = None;
        }
        if retry {
            std::thread::sleep(Duration::from_millis(500));
            continue;
        }
        while shared.banner_generation.load(Ordering::SeqCst) == generation {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

fn banner_round(
    shared: &Shared,
    viewer: &str,
    generation: u64,
    attached: &mut Option<Desktop>,
) -> Result<(), String> {
    let (desktop, name) = input_desktop()?;
    // On the person's own desktop their agent's banner shows.
    if name == OWN_DESKTOP {
        return Ok(());
    }
    unsafe { SetThreadDesktop(desktop.0) }.map_err(|e| format!("SetThreadDesktop: {e}"))?;
    *attached = Some(desktop);
    let banner = SessionBanner::show(viewer).map_err(|e| format!("on {name}: {e}"))?;
    log::line(&format!("desktop helper: banner shown on {name}"));
    while shared.banner_generation.load(Ordering::SeqCst) == generation {
        if banner.end_pressed(50) {
            log::line(&format!(
                "desktop helper: End pressed on the banner on {name}"
            ));
            if !shared.send(&FromHelper::EndPressed) {
                break;
            }
        }
    }
    // The window goes on this thread, before it ends.
    drop(banner);
    Ok(())
}
