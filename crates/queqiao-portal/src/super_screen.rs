//! Super Screen on GNOME (Wayland): a virtual monitor of mutter's at the size the viewer asks for, shown instead of
//! the PC's own screens for the session. Only through GNOME's own interfaces (mutter.rs), where the person at the PC
//! turned on "Control GNOME without asking": the portal would ask the person there first, and only some desktops'
//! portals offer a virtual source at all (a later step).
//!
//! A mutter session of its own records a virtual monitor (ScreenCast's RecordVirtual, "is-platform" where the API has
//! it: GNOME treats it as a real monitor, not as a screen being shared). mutter makes the monitor once the capture's
//! PipeWire stream agrees on its size (asked for exactly: pipewire.rs) and sets it beside the PC's own; once it is
//! there ([`Virtual::settle`]), it becomes the only monitor for the session (DisplayConfig's ApplyMonitorsConfig,
//! "temporary": never saved), so windows move to it and the PC's own screens go dark. The session that recorded the
//! PC's own monitor stops before that (mutter 42 crashed when a monitor still recorded went off). Where mutter refuses
//! the layout, the virtual monitor stays beside the PC's own and the session still shows and controls it (the report
//! says so). While it is on, the capture and the input open its session ([`crate::session`]); input goes through that
//! session's remote desktop, on the virtual stream. Off, or at the session's end ([`stop`]): the session stops, mutter removes the monitor and falls
//! back to the PC's own layout (its saved one, else its default), and the layout seen before is put back where that
//! differs from it.

use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use crate::{PortalSession, SHARED, USE_MUTTER, display_config, mutter};

/// Super Screen's session while it is on.
static ON: Mutex<Option<Arc<PortalSession>>> = Mutex::new(None);
/// What Super Screen did, for the session's report (taken by [`take_notes`]).
static NOTES: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// The largest virtual monitor asked for (the capture's and the encoders' limit; mutter's is 16384).
pub const MAX_SIDE: u32 = 8192;

fn note(text: String) {
    if let Ok(mut notes) = NOTES.lock() {
        notes.push(text);
    }
}

/// What Super Screen did since this was last asked, for the report.
pub fn take_notes() -> Vec<String> {
    NOTES
        .lock()
        .map(|mut notes| std::mem::take(&mut *notes))
        .unwrap_or_default()
}

/// Super Screen's session while it is on.
pub(crate) fn current() -> Option<Arc<PortalSession>> {
    ON.lock().ok().and_then(|on| on.clone())
}

/// Whether Super Screen is on.
pub fn is_on() -> bool {
    current().is_some()
}

/// Whether GNOME's own interfaces can make the virtual monitor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mutter {
    /// Not turned on ("Control GNOME without asking" off, or someone helping from another account).
    NotAllowed,
    /// Turned on, but the session uses the portal: GNOME's own interfaces were not there (why).
    Unavailable(String),
    /// Turned on, and the session uses them (or has none open yet: they are tried).
    Ready,
}

pub fn mutter() -> Mutter {
    if !USE_MUTTER.load(std::sync::atomic::Ordering::Relaxed) {
        return Mutter::NotAllowed;
    }
    let shared = SHARED.lock().ok().and_then(|shared| shared.upgrade());
    match shared {
        Some(session) if !matches!(session.kind, crate::Kind::Mutter(_)) => {
            Mutter::Unavailable(session.mutter_note.clone())
        }
        _ => Mutter::Ready,
    }
}

/// Super Screen on: a virtual monitor of `width` x `height` at `refresh` Hz (one on already goes first, its layout
/// back). Sessions opened from now on show it; it is made once the capture's stream opens.
pub fn start(width: u32, height: u32, refresh: u32) -> Result<(), String> {
    stop();
    if width == 0 || height == 0 || width > MAX_SIDE || height > MAX_SIDE {
        return Err(format!(
            "a virtual monitor of {width}x{height} (up to {MAX_SIDE}x{MAX_SIDE})"
        ));
    }
    let refresh = refresh.clamp(1, 240);
    // The monitors there now and their layout: the new one is told apart from them, and the layout put back later.
    let (before, own) = match display_config::state() {
        Ok((_, state)) => (
            display_config::connectors(&state),
            display_config::layout_of(&state),
        ),
        Err(error) => {
            note(format!(
                "Super Screen: the layout before is not known ({error}): mutter's own comes back afterwards"
            ));
            (Vec::new(), None)
        }
    };
    let session = mutter::MutterSession::open(mutter::Source::Virtual(width, height))?;
    note(format!(
        "Super Screen: GNOME's ScreenCast (API {}) records a virtual monitor of {width}x{height} at {refresh} Hz \
         (RecordVirtual{}); this PC's layout before: {}",
        session.version,
        if session.version >= 3 {
            ", as a monitor of the platform"
        } else {
            ""
        },
        own.as_ref()
            .map_or_else(|| "not known".to_string(), display_config::Layout::describe)
    ));
    let virtual_monitor = Virtual {
        refresh,
        before,
        own,
        settled: Mutex::new(Settled::default()),
    };
    let session = Arc::new(PortalSession::of_mutter(session, Some(virtual_monitor)));
    let mut on = ON
        .lock()
        .map_err(|_| "Super Screen's lock broke".to_string())?;
    *on = Some(session);
    // The session recording the PC's own monitor stops now (the capture opens the virtual monitor's next): mutter 42
    // crashed when a monitor a stream still recorded went off for the virtual one. Off again, a new one opens.
    let shared = SHARED.lock().ok().and_then(|mut shared| {
        let session = shared.upgrade();
        *shared = Weak::new();
        session
    });
    if let Some(shared) = shared {
        shared.stop();
    }
    Ok(())
}

/// Super Screen off (nothing if it is not on): its session stops (the capture and the input that still hold it get
/// nothing more from it), the virtual monitor goes, and this PC's own layout comes back.
pub fn stop() {
    let Some(session) = ON.lock().ok().and_then(|mut on| on.take()) else {
        return;
    };
    let Some(virtual_monitor) = &session.virtual_monitor else {
        return;
    };
    session.stop();
    let connector = virtual_monitor
        .settled
        .lock()
        .ok()
        .and_then(|settled| settled.connector.clone());
    // mutter removes the monitor as the session stops: waited for, so that what records the primary monitor next
    // finds the PC's own.
    let started = Instant::now();
    let mut state = None;
    while started.elapsed() < Duration::from_secs(3) {
        match display_config::state() {
            Ok((proxy, now)) => {
                let connectors = display_config::connectors(&now);
                let gone = match &connector {
                    Some(connector) => !connectors.contains(connector),
                    None => connectors
                        .iter()
                        .all(|name| virtual_monitor.before.contains(name)),
                };
                if gone {
                    state = Some((proxy, now));
                    break;
                }
            }
            Err(error) => {
                note(format!("Super Screen off: {error}"));
                return;
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let Some((proxy, now)) = state else {
        note("Super Screen off: mutter still lists the virtual monitor after 3 s".into());
        return;
    };
    let gone_after = started.elapsed().as_millis();
    let Some(own) = &virtual_monitor.own else {
        note(format!(
            "Super Screen off: the virtual monitor gone after {gone_after} ms (mutter's layout)"
        ));
        return;
    };
    if display_config::layout_of(&now).as_ref() == Some(own) {
        note(format!(
            "Super Screen off: the virtual monitor gone after {gone_after} ms, this PC's own layout back ({})",
            own.describe()
        ));
        return;
    }
    // mutter fell back to another layout (its default where the one before was set for a while only).
    note(match display_config::apply_layout(&proxy, now.0, own) {
        Ok(()) => format!(
            "Super Screen off: the virtual monitor gone after {gone_after} ms, this PC's layout before put back ({})",
            own.describe()
        ),
        Err(error) => format!(
            "Super Screen off: the virtual monitor gone after {gone_after} ms, the layout before not put back: {error}"
        ),
    });
}

/// What a session's virtual monitor needs besides the session: its refresh rate, what was there before it, and what
/// was done once it came.
pub(crate) struct Virtual {
    refresh: u32,
    /// The monitors' connectors before it came, and their layout (put back afterwards).
    before: Vec<String>,
    own: Option<display_config::Layout>,
    settled: Mutex<Settled>,
}

/// Once the virtual monitor came: its connector, whether it was made the only one (tried once: a capture opened again
/// meanwhile finds it done).
#[derive(Default)]
struct Settled {
    tried: bool,
    connector: Option<String>,
    only: bool,
}

impl Virtual {
    pub(crate) fn refresh(&self) -> u32 {
        self.refresh
    }

    /// For the session's description.
    pub(crate) fn describe(&self) -> String {
        let Ok(settled) = self.settled.lock() else {
            return String::new();
        };
        match (&settled.connector, settled.only) {
            (Some(connector), true) => format!(" ({connector}, the only monitor)"),
            (Some(connector), false) => format!(" ({connector}, beside this PC's own)"),
            (None, _) => " (not listed by mutter)".into(),
        }
    }

    /// The capture's stream agreed on `size`: mutter makes the monitor then. It is looked for (up to 3 s) and made
    /// the only one.
    pub(crate) fn settle(&self, size: (u32, u32)) {
        let Ok(mut settled) = self.settled.lock() else {
            return;
        };
        if settled.tried {
            return;
        }
        settled.tried = true;
        let started = Instant::now();
        let found = loop {
            match display_config::state() {
                Ok((proxy, state)) => {
                    if let Some(connector) = display_config::new_monitor(&state, &self.before, size)
                    {
                        break Some((proxy, state, connector));
                    }
                }
                Err(error) => {
                    note(format!(
                        "Super Screen: the virtual monitor not found: {error}"
                    ));
                    break None;
                }
            }
            if started.elapsed() > Duration::from_secs(3) {
                note("Super Screen: mutter did not list the virtual monitor within 3 s".into());
                break None;
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        let Some((proxy, state, connector)) = found else {
            return;
        };
        settled.connector = Some(connector.clone());
        let listed_after = started.elapsed().as_millis();
        let made = display_config::only_layout(&state, &connector).and_then(|layout| {
            if display_config::layout_of(&state).as_ref() == Some(&layout) {
                return Ok(());
            }
            display_config::apply_layout(&proxy, state.0, &layout)
        });
        note(match made {
            Ok(()) => {
                settled.only = true;
                format!(
                    "Super Screen: the virtual monitor is {connector} ({}x{}, listed after {listed_after} ms), \
                     the only monitor now: this PC's own are off for the session",
                    size.0, size.1
                )
            }
            Err(error) => format!(
                "Super Screen: the virtual monitor is {connector} ({}x{}, listed after {listed_after} ms), beside \
                 this PC's own monitors: mutter did not make it the only one ({error})",
                size.0, size.1
            ),
        });
    }
}
