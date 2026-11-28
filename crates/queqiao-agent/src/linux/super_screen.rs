//! Super Screen on Linux (the session's Command::SuperScreen; Windows' is in session.rs): GNOME on Wayland through
//! GNOME's own interfaces, where the person at this PC turned on "Control GNOME without asking" (queqiao_portal's
//! super_screen.rs: a virtual monitor of mutter's, the only one for the session). Elsewhere a note says why not:
//! - GNOME through the portal: it would ask the person at the PC first (a portal's "virtual" source type exists on
//!   some desktops: a later step);
//! - KDE (KWin's virtual outputs for screen casts) and sway (wlroots' headless outputs): not built yet;
//! - X11: no general way (it needs the X server's dummy driver or a kernel module).

use queqiao_core::text::{tr, trf};
use queqiao_portal::super_screen::{self, MAX_SIDE, Mutter};

/// What the session runs on, for [`refusal`].
struct Desktop {
    wayland: bool,
    gnome: bool,
}

impl Desktop {
    fn now() -> Desktop {
        let set = |name: &str| std::env::var_os(name).is_some_and(|value| !value.is_empty());
        // As the capture tells a Wayland session (its programs talk to the compositor; X11 reaches only XWayland).
        let wayland = set("WAYLAND_DISPLAY")
            && (std::env::var("XDG_SESSION_TYPE").is_ok_and(|kind| kind == "wayland")
                || !set("DISPLAY"));
        // "GNOME", "ubuntu:GNOME", "GNOME-Classic:GNOME".
        let gnome = std::env::var("XDG_CURRENT_DESKTOP").is_ok_and(|names| {
            names
                .split(':')
                .any(|name| name.eq_ignore_ascii_case("GNOME"))
        });
        Desktop { wayland, gnome }
    }
}

/// Why Super Screen cannot be on here (the note for the viewer), or None.
fn refusal(desktop: &Desktop, mutter: Mutter) -> Option<String> {
    if !desktop.wayland {
        return Some(tr("Super Screen is not available on X11: a virtual screen there needs the X server's dummy driver or a kernel module.").into());
    }
    if !desktop.gnome {
        return Some(
            tr("Super Screen on Linux works with GNOME on Wayland for now, not on this desktop.")
                .into(),
        );
    }
    match mutter {
        Mutter::Ready => None,
        Mutter::NotAllowed => Some(tr("Super Screen on GNOME needs \"Control GNOME without asking\" turned on at that PC (through the desktop portal, GNOME would ask the person there first).").into()),
        Mutter::Unavailable(why) => Some(trf(
            "GNOME's own remote desktop is not available on that PC, so Super Screen is not either: {why}",
            &[("why", &why)],
        )),
    }
}

/// Super Screen on at `mode` (width, height, refresh rate): checked first, then `before` (the session's own resolution
/// change put back, as on Windows), then GNOME's virtual monitor started; the capture opened next shows it. The screen
/// index to open: on Wayland the session shows one screen, 0.
pub(crate) fn enter(
    mode: (u32, u32, u32),
    scale: u16,
    lines: &mut Vec<String>,
    before: impl FnOnce() -> Option<String>,
) -> Result<usize, String> {
    if let Some(note) = refusal(&Desktop::now(), super_screen::mutter()) {
        return Err(note);
    }
    let (width, height, refresh) = mode;
    if width > MAX_SIDE || height > MAX_SIDE {
        return Err(trf(
            "Super Screen goes up to {max} x {max}.",
            &[("max", &MAX_SIDE.to_string())],
        ));
    }
    if let Some(line) = before() {
        lines.push(line);
    }
    // GNOME picks the scale of a virtual monitor (as of a monitor whose size in millimetres it does not know).
    if scale > 0 {
        lines.push(format!(
            "Super Screen: the scale asked for ({scale} %) is left to GNOME"
        ));
    }
    let started = super_screen::start(width, height, refresh);
    lines.extend(super_screen::take_notes());
    started.map_err(|error| {
        trf(
            "GNOME did not make the virtual screen: {why}",
            &[("why", &error)],
        )
    })?;
    Ok(0)
}

/// Super Screen off (or the session's end; nothing if it is not on): the virtual monitor goes and this PC's own
/// layout comes back. What was done goes into the report.
pub(crate) fn leave(lines: &mut Vec<String>) {
    super_screen::stop();
    lines.extend(super_screen::take_notes());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn super_screen_is_refused_with_a_reason_where_it_cannot_be() {
        let gnome = Desktop {
            wayland: true,
            gnome: true,
        };
        assert_eq!(refusal(&gnome, Mutter::Ready), None);
        // Through the portal GNOME would ask the person first: the setting is named (in either language).
        assert!(
            refusal(&gnome, Mutter::NotAllowed)
                .is_some_and(|note| note.contains(tr("Control GNOME without asking")))
        );
        assert!(
            refusal(&gnome, Mutter::Unavailable("no such interface".into()))
                .is_some_and(|note| note.ends_with("no such interface"))
        );
        let x11 = Desktop {
            wayland: false,
            gnome: true,
        };
        assert!(refusal(&x11, Mutter::Ready).is_some_and(|note| note.contains("X11")));
        let kde = Desktop {
            wayland: true,
            gnome: false,
        };
        let other = refusal(&kde, Mutter::Ready);
        assert!(other.is_some() && other != refusal(&x11, Mutter::Ready));
    }
}
