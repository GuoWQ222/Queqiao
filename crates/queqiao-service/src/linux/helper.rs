//! The desktop helper (Linux): root, reduced to what the screen and input need, because the sign-in and lock screens
//! belong to no program a person runs. It talks only to the agent, over the channel the service handed to both:
//! - tells the agent which screen takes input, whenever that changes: "Default" (the person's own desktop, unlocked),
//!   "Lock" (their session, locked), "Greeter" (the sign-in screen), "Other" (someone else's session);
//! - while asked to, sends pictures of the screen as the display hardware shows it (KMS, at most 30 a second, when it
//!   changes), waking a screen the desktop switched off; never of the person's own desktop: the agent captures that
//!   itself, through the desktop's own consent (GNOME's portal), which KMS would go round;
//! - during a session, injects the agent's input through the kernel (uinput), but never on the person's own desktop:
//!   there the agent injects as the person;
//! - pictures and input only while its own session is on the screen: never another session that took the screen
//!   (another user switched in) in the moment before the service ends this pair;
//! - during a session off the person's own desktop, shows the session banner where it can (an X11 screen); where it
//!   cannot (GNOME shows no other program on its sign-in and lock screens) it tells the agent, which then tells the
//!   person afterwards (the person's choice, 2026-11-25).
//!
//! It ends when the agent's channel closes, or at the first message it does not know.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use queqiao_capture::kms::{Kms, KmsScreen};
use queqiao_input::UinputInjector;
use queqiao_local::{Ends, FromHelper, ToHelper, Writer};

use super::log;
use super::logind::Logind;

const OWN_DESKTOP: &str = "Default";
const FRAME_GAP: Duration = Duration::from_millis(33);
/// How often a screen whose framebuffer stayed the same is read and compared (an X server draws in place).
const LOOK_GAP: Duration = Duration::from_millis(200);

struct Shared {
    capture: Mutex<Option<u8>>,
    generation: AtomicU64,
    session: AtomicBool,
    banner: Mutex<Option<String>>,
    banner_generation: AtomicU64,
    desktop: Mutex<String>,
    /// The session on the screen is this helper's own (logind's active session).
    mine: AtomicBool,
    injector: Mutex<Option<UinputInjector>>,
    writer: Mutex<Writer>,
}

impl Shared {
    fn send(&self, message: &FromHelper) -> bool {
        self.writer
            .lock()
            .is_ok_and(|mut writer| writer.write(&message.encode()).is_ok())
    }

    fn on_own_desktop(&self) -> bool {
        self.desktop
            .lock()
            .is_ok_and(|desktop| *desktop == OWN_DESKTOP)
    }

    /// Whether this helper may show and operate the screen: only its own session's lock or sign-in screen. Never the
    /// person's own desktop (the agent works there as the person), and never another session that took the screen
    /// (another user switched in; the sign-in screen of a user switch) in the moment before the service ends this
    /// pair: the security review of 2026-11-26 found that window open for input and pictures.
    fn may_operate(&self) -> bool {
        self.mine.load(Ordering::SeqCst) && !self.on_own_desktop()
    }
}

struct Options {
    ends: Ends,
    session: String,
    sign_in: bool,
    x11_display: Option<String>,
    x11_authority: Option<String>,
}

fn parse(args: &[String]) -> Option<Options> {
    let (mut ends, mut session, mut sign_in, mut x11_display, mut x11_authority) =
        (None, None, false, None, None);
    let mut words = args.iter();
    while let Some(word) = words.next() {
        match word.as_str() {
            "--channel" => ends = Ends::parse(words.next()?),
            "--session" => session = Some(words.next()?.clone()),
            "--sign-in" => sign_in = true,
            "--x11-display" => x11_display = Some(words.next()?.clone()),
            "--x11-authority" => x11_authority = Some(words.next()?.clone()),
            _ => return None,
        }
    }
    Some(Options {
        ends: ends?,
        session: session?,
        sign_in,
        x11_display,
        x11_authority,
    })
}

pub fn run(args: &[String]) -> Result<(), String> {
    let options = parse(args).ok_or(
        "usage: queqiao-service desktop --channel <read>,<write> --session <id> [--sign-in] (the service starts it)",
    )?;
    if let Some(display) = &options.x11_display {
        // Before any thread: the banner's window goes to that X server.
        unsafe { std::env::set_var("DISPLAY", display) };
        if let Some(authority) = &options.x11_authority {
            unsafe { std::env::set_var("XAUTHORITY", authority) };
        }
    }
    let (mut reader, writer) = options.ends.split();
    let shared = Arc::new(Shared {
        capture: Mutex::new(None),
        generation: AtomicU64::new(0),
        session: AtomicBool::new(false),
        banner: Mutex::new(None),
        banner_generation: AtomicU64::new(0),
        desktop: Mutex::new(String::new()),
        mine: AtomicBool::new(false),
        injector: Mutex::new(None),
        writer: Mutex::new(writer),
    });
    log::line(&format!(
        "desktop helper: started for session {}{}",
        options.session,
        if options.sign_in {
            " (the sign-in screen)"
        } else {
            ""
        }
    ));
    {
        let shared = shared.clone();
        let (session, sign_in) = (options.session.clone(), options.sign_in);
        std::thread::spawn(move || watch(&shared, &session, sign_in));
    }
    {
        let shared = shared.clone();
        std::thread::spawn(move || capture_loop(&shared));
    }
    {
        let shared = shared.clone();
        let x11 = options.x11_display.is_some();
        std::thread::spawn(move || banner_loop(&shared, x11));
    }
    let (mut injected, mut refused, mut last_error) = (0u64, 0u64, String::new());
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
                if let Ok(mut injector) = shared.injector.lock() {
                    if on && injector.is_none() {
                        match UinputInjector::new() {
                            Ok(made) => {
                                *injector = Some(made);
                                // The compositor takes a new device in before its first events count.
                                std::thread::sleep(Duration::from_millis(800));
                            }
                            Err(error) => log::line(&format!("desktop helper: input: {error}")),
                        }
                    } else if !on {
                        *injector = None;
                    }
                }
                log::line(&format!(
                    "desktop helper: a session {}",
                    if on { "started" } else { "ended" }
                ));
            }
            Some(ToHelper::Input { event, .. }) => {
                if !shared.session.load(Ordering::SeqCst) || !shared.may_operate() {
                    refused += 1;
                    continue;
                }
                match shared
                    .injector
                    .lock()
                    .ok()
                    .as_mut()
                    .and_then(|i| i.as_mut())
                {
                    Some(injector) => match injector.inject(&event) {
                        Ok(()) => injected += 1,
                        Err(error) => {
                            refused += 1;
                            last_error = error;
                        }
                    },
                    None => refused += 1,
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
        "desktop helper: ending ({ending}); injected {injected}, refused {refused}{}",
        if last_error.is_empty() {
            String::new()
        } else {
            format!(", last error: {last_error}")
        }
    ));
    Ok(())
}

/// Tells the agent which screen takes input, at once and whenever it changes.
fn watch(shared: &Shared, session: &str, sign_in: bool) {
    let logind = loop {
        match Logind::connect() {
            Ok(logind) => break logind,
            Err(error) => {
                log::line(&format!("desktop helper: {error}"));
                std::thread::sleep(Duration::from_secs(1));
            }
        }
    };
    let mut last = String::new();
    loop {
        let active = logind.active();
        // Set before the name changes, so input and pictures stop at once when another session takes the screen.
        let mine = matches!(&active, Ok(Some(active)) if active.id == session);
        shared.mine.store(mine, Ordering::SeqCst);
        let name = match active {
            Ok(Some(active)) if active.id == session => {
                if sign_in || active.is_sign_in() {
                    "Greeter"
                } else if logind.locked(&active.path).unwrap_or(false) {
                    "Lock"
                } else {
                    OWN_DESKTOP
                }
            }
            Ok(Some(active)) if active.is_sign_in() => "Greeter",
            Ok(_) => "Other",
            Err(_) => {
                std::thread::sleep(Duration::from_millis(500));
                continue;
            }
        };
        if name != last {
            if let Ok(mut desktop) = shared.desktop.lock() {
                *desktop = name.to_string();
            }
            if !shared.send(&FromHelper::Desktop(name.to_string())) {
                return;
            }
            log::line(&format!("desktop helper: input desktop {name}"));
            last = name.to_string();
            shared.generation.fetch_add(1, Ordering::SeqCst);
            shared.banner_generation.fetch_add(1, Ordering::SeqCst);
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Presses and lets go of Shift on the helper's keyboard: the desktop turns a screen it switched off on again.
fn wake(shared: &Shared) {
    if let Ok(mut injector) = shared.injector.lock()
        && let Some(injector) = injector.as_mut()
    {
        for down in [true, false] {
            let _ = injector.inject(&queqiao_core::protocol::InputEvent::Key {
                scancode: 0x2A,
                extended: false,
                down,
            });
        }
    }
}

fn capture_loop(shared: &Shared) {
    let mut kms: Option<Kms> = None;
    let mut last_error = String::new();
    loop {
        let screen = shared.capture.lock().ok().and_then(|capture| *capture);
        let Some(screen) = screen else {
            std::thread::sleep(Duration::from_millis(30));
            continue;
        };
        let generation = shared.generation.load(Ordering::SeqCst);
        if !shared.may_operate() {
            report(
                shared,
                &mut last_error,
                if shared.on_own_desktop() {
                    "the person's own desktop takes input: the agent captures it itself there"
                        .into()
                } else {
                    "another session is on the screen: not captured".into()
                },
            );
            while shared.generation.load(Ordering::SeqCst) == generation && !shared.may_operate() {
                std::thread::sleep(Duration::from_millis(30));
            }
            continue;
        }
        if kms.is_none() {
            match Kms::open() {
                Ok(opened) => kms = Some(opened),
                Err(error) => {
                    report(shared, &mut last_error, format!("KMS: {error}"));
                    std::thread::sleep(Duration::from_millis(1000));
                    continue;
                }
            }
        }
        let Some(kms_ref) = kms.as_ref() else {
            continue;
        };
        match capture_round(shared, kms_ref, usize::from(screen), generation) {
            Ok(()) => last_error.clear(),
            Err(error) => {
                report(shared, &mut last_error, error);
                // A card may have come or gone (another GPU, a dock): opened again next time.
                kms = None;
                std::thread::sleep(Duration::from_millis(300));
            }
        }
    }
}

fn report(shared: &Shared, last_error: &mut String, error: String) {
    if error != *last_error {
        log::line(&format!("desktop helper: capture: {error}"));
        shared.send(&FromHelper::Failed(error.clone()));
        *last_error = error;
    }
}

/// The lit screen the agent asked for (by its number; the last one if there are fewer), waking the screens first if
/// none is lit.
fn lit_screen(shared: &Shared, kms: &Kms, index: usize) -> Result<KmsScreen, String> {
    let until = Instant::now() + Duration::from_secs(3);
    let mut woken = false;
    loop {
        let screens = kms.screens()?;
        if let Some(screen) = screens.get(index.min(screens.len().saturating_sub(1))) {
            return Ok(screen.clone());
        }
        if Instant::now() >= until {
            return Err("no screen is lit (switched off, and it did not wake)".into());
        }
        if !woken {
            wake(shared);
            woken = true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn capture_round(shared: &Shared, kms: &Kms, index: usize, generation: u64) -> Result<(), String> {
    let screen = lit_screen(shared, kms, index)?;
    if !shared.send(&FromHelper::Screen {
        left: screen.left,
        top: screen.top,
        width: screen.width,
        height: screen.height,
    }) {
        return Ok(());
    }
    let mut last_state = None;
    let mut last_hash = None;
    let mut last_look = Instant::now() - LOOK_GAP;
    while shared.generation.load(Ordering::SeqCst) == generation && shared.may_operate() {
        let state = kms.state(&screen)?;
        if Some(state) != last_state || last_look.elapsed() >= LOOK_GAP {
            last_look = Instant::now();
            let image = kms.capture(&screen)?;
            if (image.width, image.height) != (screen.width, screen.height) {
                // The mode changed: started again with the new size.
                return Ok(());
            }
            let hash = {
                use std::hash::{Hash, Hasher};
                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                image.pixels.hash(&mut hasher);
                hasher.finish()
            };
            if Some(hash) != last_hash {
                if !shared.send(&FromHelper::Frame {
                    width: image.width,
                    height: image.height,
                    pixels: image.pixels,
                }) {
                    return Ok(());
                }
                last_hash = Some(hash);
            }
            last_state = Some(state);
        }
        std::thread::sleep(FRAME_GAP);
    }
    Ok(())
}

/// Shows the session banner while a session runs off the person's own desktop, where an X11 screen lets it (its
/// window on the X server of that screen); otherwise says so to the agent once per showing.
fn banner_loop(shared: &Shared, x11: bool) {
    let mut told: Option<u64> = None;
    loop {
        let generation = shared.banner_generation.load(Ordering::SeqCst);
        let viewer = shared
            .banner
            .lock()
            .ok()
            .and_then(|banner| banner.clone())
            .filter(|_| shared.session.load(Ordering::SeqCst) && !shared.on_own_desktop());
        if let Some(viewer) = viewer {
            let shown = if x11 {
                match queqiao_display::SessionBanner::show(&viewer) {
                    Ok(banner) => {
                        log::line("desktop helper: banner shown on the X11 screen");
                        while shared.banner_generation.load(Ordering::SeqCst) == generation {
                            if banner.end_pressed(50) {
                                log::line("desktop helper: End pressed on the banner");
                                shared.send(&FromHelper::EndPressed);
                            }
                        }
                        drop(banner);
                        Ok(())
                    }
                    Err(error) => Err(error.to_string()),
                }
            } else {
                Err("this screen shows no other program's windows (a Wayland sign-in or lock screen)".to_string())
            };
            if let Err(reason) = shown
                && told != Some(generation)
            {
                log::line(&format!("desktop helper: no banner: {reason}"));
                shared.send(&FromHelper::NoBanner(reason));
                told = Some(generation);
            }
        }
        while shared.banner_generation.load(Ordering::SeqCst) == generation {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}
