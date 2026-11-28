//! The resolution of the controlled PC's screens: the sizes a screen offers, a change to one of them for the session
//! (the controlling side's choice: a sharper picture, or the size of its own window), and the screen's own size back
//! afterwards.
//!
//! - Windows: the display's modes (EnumDisplaySettingsEx) and a change for the session only (ChangeDisplaySettingsEx
//!   with CDS_FULLSCREEN: nothing written to the registry, so a restart or a new sign-in has the person's own size
//!   whatever happens); back with the registry's mode.
//! - Linux, X11: RandR (the output's modes, the CRTC set to one, the screen's size made to fit); back to the mode it
//!   had. GNOME on Wayland: mutter's display settings (queqiao_portal::display_config), applied "temporarily" (never
//!   saved). One screen at a time; a PC with several screens side by side keeps its layout (refused there for now).
//!
//! A change made is remembered in a file ([`pending_restore`]): a Queqiao that ended without putting the size back
//! (a crash) puts it back when it starts again.

/// A screen's sizes: what it shows now, and those it can be switched to (largest first, each once).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DisplayModes {
    pub current: (u32, u32),
    pub sizes: Vec<(u32, u32)>,
}

/// Sizes at most this many (the menu's length).
pub const MAX_SIZES: usize = 24;

/// The sizes offered: no smaller than 800 x 600, each once, largest first.
pub fn tidy(mut sizes: Vec<(u32, u32)>) -> Vec<(u32, u32)> {
    sizes.retain(|(w, h)| *w >= 800 && *h >= 600 && *w <= 16384 && *h <= 16384);
    sizes.sort_by_key(|&(w, h)| std::cmp::Reverse((w * h, w)));
    sizes.dedup();
    sizes.truncate(MAX_SIZES);
    sizes
}

/// Where a change waiting to be undone is noted (the screen's name per line).
fn marker() -> Option<std::path::PathBuf> {
    #[cfg(windows)]
    let base = std::env::var_os("LOCALAPPDATA").map(std::path::PathBuf::from);
    #[cfg(not(windows))]
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(std::path::PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME").map(|home| std::path::PathBuf::from(home).join(".local/state"))
        });
    Some(base?.join("Queqiao").join("display_mode_changed.txt"))
}

fn note_changed(screen: &str, original: (u32, u32)) {
    let Some(path) = marker() else {
        return;
    };
    if let Some(folder) = path.parent() {
        let _ = std::fs::create_dir_all(folder);
    }
    let mut text = std::fs::read_to_string(&path).unwrap_or_default();
    if !text
        .lines()
        .any(|line| line.split('\t').next() == Some(screen))
    {
        text.push_str(&format!("{screen}\t{}\t{}\n", original.0, original.1));
        let _ = std::fs::write(&path, text);
    }
}

fn note_restored(screen: &str) {
    let Some(path) = marker() else {
        return;
    };
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let rest: String = text
        .lines()
        .filter(|line| line.split('\t').next() != Some(screen))
        .map(|line| format!("{line}\n"))
        .collect();
    if rest.is_empty() {
        let _ = std::fs::remove_file(&path);
    } else {
        let _ = std::fs::write(&path, rest);
    }
}

/// Changes a crashed Queqiao left: each put back (its screen's name and size before, as noted).
pub fn pending_restore() -> Vec<String> {
    let Some(path) = marker() else {
        return Vec::new();
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    let mut done = Vec::new();
    for line in text.lines() {
        let mut parts = line.split('\t');
        let (Some(screen), Some(w), Some(h)) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        let original = (w.parse().unwrap_or(0), h.parse().unwrap_or(0));
        let outcome = platform::restore_named(screen, original);
        done.push(match outcome {
            Ok(()) => format!("the size of screen {screen} put back after an earlier session"),
            Err(error) => format!("the size of screen {screen} not put back: {error}"),
        });
        note_restored(screen);
    }
    done
}

/// Screen `index`'s sizes (as `screens` counts them).
pub fn display_modes(index: usize) -> Result<DisplayModes, String> {
    platform::modes(index)
}

/// Screen `index` to `width` x `height` (one of its sizes) for the session; what it had is noted to come back.
pub fn set_display_mode(index: usize, width: u32, height: u32) -> Result<(), String> {
    let (name, original) = platform::name_and_size(index)?;
    platform::set(index, width, height)?;
    note_changed(&name, original);
    Ok(())
}

/// Screen `index` back to the size it had before `set_display_mode`.
pub fn restore_display_mode(index: usize, original: (u32, u32)) -> Result<(), String> {
    let (name, _) = platform::name_and_size(index)?;
    let result = platform::restore_named(&name, original);
    note_restored(&name);
    result
}

#[cfg(windows)]
mod platform {
    use windows::Win32::Graphics::Gdi::{
        CDS_FULLSCREEN, CDS_TYPE, ChangeDisplaySettingsExW, DEVMODEW, DISP_CHANGE_SUCCESSFUL,
        DM_BITSPERPEL, DM_DISPLAYFREQUENCY, DM_PELSHEIGHT, DM_PELSWIDTH, ENUM_CURRENT_SETTINGS,
        ENUM_DISPLAY_SETTINGS_FLAGS, ENUM_DISPLAY_SETTINGS_MODE, EnumDisplaySettingsExW,
    };
    use windows::core::PCWSTR;

    use super::{DisplayModes, tidy};

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(Some(0)).collect()
    }

    /// The GDI name of screen `index` ("\\.\DISPLAY1"), from the DXGI output the capture opens.
    fn device(index: usize) -> Result<String, String> {
        let screens = crate::win::screen_names().map_err(|e| e.to_string())?;
        screens
            .get(index)
            .cloned()
            .ok_or_else(|| format!("screen {index} not found ({} attached)", screens.len()))
    }

    fn mode(name: &[u16], number: u32) -> Option<DEVMODEW> {
        let mut mode = DEVMODEW {
            dmSize: size_of::<DEVMODEW>() as u16,
            ..Default::default()
        };
        unsafe {
            EnumDisplaySettingsExW(
                PCWSTR(name.as_ptr()),
                ENUM_DISPLAY_SETTINGS_MODE(number),
                &mut mode,
                ENUM_DISPLAY_SETTINGS_FLAGS(0),
            )
        }
        .as_bool()
        .then_some(mode)
    }

    fn current(name: &[u16]) -> Result<DEVMODEW, String> {
        mode(name, ENUM_CURRENT_SETTINGS.0)
            .ok_or_else(|| "the screen's mode cannot be read".to_string())
    }

    /// The modes at the screen's colour depth and orientation (its sizes at any refresh rate).
    fn all(name: &[u16], now: &DEVMODEW) -> Vec<DEVMODEW> {
        let orientation = unsafe { now.Anonymous1.Anonymous2.dmDisplayOrientation };
        (0..4096)
            .map_while(|number| mode(name, number))
            .filter(|m| m.dmBitsPerPel == now.dmBitsPerPel)
            .filter(|m| unsafe { m.Anonymous1.Anonymous2.dmDisplayOrientation } == orientation)
            .collect()
    }

    pub(super) fn name_and_size(index: usize) -> Result<(String, (u32, u32)), String> {
        let name = device(index)?;
        let now = current(&wide(&name))?;
        Ok((name, (now.dmPelsWidth, now.dmPelsHeight)))
    }

    pub(super) fn modes(index: usize) -> Result<DisplayModes, String> {
        let name = wide(&device(index)?);
        let now = current(&name)?;
        let sizes = all(&name, &now)
            .iter()
            .map(|m| (m.dmPelsWidth, m.dmPelsHeight))
            .collect();
        Ok(DisplayModes {
            current: (now.dmPelsWidth, now.dmPelsHeight),
            sizes: tidy(sizes),
        })
    }

    pub(super) fn set(index: usize, width: u32, height: u32) -> Result<(), String> {
        let name = wide(&device(index)?);
        let now = current(&name)?;
        // The size asked for, at the refresh rate it has now where it can, else its highest.
        let mut chosen = all(&name, &now)
            .into_iter()
            .filter(|m| m.dmPelsWidth == width && m.dmPelsHeight == height)
            .max_by_key(|m| {
                (
                    m.dmDisplayFrequency == now.dmDisplayFrequency,
                    m.dmDisplayFrequency,
                )
            })
            .ok_or_else(|| format!("that screen has no {width}x{height} mode"))?;
        chosen.dmFields = DM_PELSWIDTH | DM_PELSHEIGHT | DM_BITSPERPEL | DM_DISPLAYFREQUENCY;
        let result = unsafe {
            ChangeDisplaySettingsExW(
                PCWSTR(name.as_ptr()),
                Some(&chosen),
                None,
                CDS_FULLSCREEN,
                None,
            )
        };
        if result == DISP_CHANGE_SUCCESSFUL {
            Ok(())
        } else {
            Err(format!("Windows did not change the mode ({})", result.0))
        }
    }

    /// Back to the registry's mode (the person's own): a session's change never went there.
    pub(super) fn restore_named(name: &str, _original: (u32, u32)) -> Result<(), String> {
        let name = wide(name);
        let result = unsafe {
            ChangeDisplaySettingsExW(PCWSTR(name.as_ptr()), None, None, CDS_TYPE(0), None)
        };
        if result == DISP_CHANGE_SUCCESSFUL {
            Ok(())
        } else {
            Err(format!("Windows did not put the mode back ({})", result.0))
        }
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use x11rb::connection::Connection;
    use x11rb::protocol::randr::{self, ConnectionExt as _};
    use x11rb::rust_connection::RustConnection;

    use super::{DisplayModes, tidy};

    /// A Wayland session: its X server is XWayland's, which cannot change the real screen; GNOME's display settings
    /// do it there (queqiao_portal::display_config), for the one screen the session shows.
    fn wayland() -> bool {
        std::env::var_os("WAYLAND_DISPLAY").is_some()
            || std::env::var("XDG_SESSION_TYPE").is_ok_and(|kind| kind == "wayland")
    }

    fn wayland_screen(index: usize) -> Result<(), String> {
        if index == 0 {
            Ok(())
        } else {
            Err(format!("screen {} is not the one shown", index + 1))
        }
    }

    fn connect() -> Result<(RustConnection, u32), String> {
        let (connection, number) = x11rb::connect(None).map_err(|e| format!("X11: {e}"))?;
        let root = connection.setup().roots[number].root;
        Ok((connection, root))
    }

    /// The monitor's output, its CRTC and the CRTC's state; and the screen's resources.
    struct Target {
        output_name: String,
        crtc: u32,
        info: randr::GetCrtcInfoReply,
        output_modes: Vec<u32>,
        resources: randr::GetScreenResourcesCurrentReply,
    }

    fn x(error: impl std::fmt::Display) -> String {
        format!("X11 RandR: {error}")
    }

    fn target(connection: &RustConnection, root: u32, index: usize) -> Result<Target, String> {
        let monitors = connection
            .randr_get_monitors(root, true)
            .map_err(x)?
            .reply()
            .map_err(x)?
            .monitors;
        let monitor = monitors
            .get(index)
            .ok_or_else(|| format!("screen {index} not found"))?;
        let output = *monitor.outputs.first().ok_or("that screen has no output")?;
        let resources = connection
            .randr_get_screen_resources_current(root)
            .map_err(x)?
            .reply()
            .map_err(x)?;
        let output_info = connection
            .randr_get_output_info(output, resources.config_timestamp)
            .map_err(x)?
            .reply()
            .map_err(x)?;
        if output_info.crtc == 0 {
            return Err("that screen is off".into());
        }
        let active = resources
            .crtcs
            .iter()
            .filter_map(|crtc| {
                connection
                    .randr_get_crtc_info(*crtc, resources.config_timestamp)
                    .ok()?
                    .reply()
                    .ok()
            })
            .filter(|info| info.mode != 0)
            .count();
        if active > 1 {
            return Err("this PC shows several screens: their layout stays as it is".into());
        }
        let info = connection
            .randr_get_crtc_info(output_info.crtc, resources.config_timestamp)
            .map_err(x)?
            .reply()
            .map_err(x)?;
        Ok(Target {
            output_name: String::from_utf8_lossy(&output_info.name).into_owned(),
            crtc: output_info.crtc,
            info,
            output_modes: output_info.modes,
            resources,
        })
    }

    fn size_of_mode(
        resources: &randr::GetScreenResourcesCurrentReply,
        mode: u32,
    ) -> Option<(u32, u32)> {
        resources
            .modes
            .iter()
            .find(|m| m.id == mode)
            .map(|m| (u32::from(m.width), u32::from(m.height)))
    }

    pub(super) fn name_and_size(index: usize) -> Result<(String, (u32, u32)), String> {
        if wayland() {
            wayland_screen(index)?;
            return queqiao_portal::display_config::name_and_size();
        }
        let (connection, root) = connect()?;
        let target = target(&connection, root, index)?;
        Ok((
            target.output_name,
            (u32::from(target.info.width), u32::from(target.info.height)),
        ))
    }

    pub(super) fn modes(index: usize) -> Result<DisplayModes, String> {
        if wayland() {
            wayland_screen(index)?;
            let (current, sizes) = queqiao_portal::display_config::modes()?;
            return Ok(DisplayModes {
                current,
                sizes: tidy(sizes),
            });
        }
        let (connection, root) = connect()?;
        let target = target(&connection, root, index)?;
        let sizes = target
            .output_modes
            .iter()
            .filter_map(|mode| size_of_mode(&target.resources, *mode))
            .collect();
        Ok(DisplayModes {
            current: (u32::from(target.info.width), u32::from(target.info.height)),
            sizes: tidy(sizes),
        })
    }

    /// The CRTC to the output's `width` x `height` mode (its highest refresh rate), the screen sized to fit it.
    fn apply(
        connection: &RustConnection,
        root: u32,
        target: &Target,
        width: u32,
        height: u32,
    ) -> Result<(), String> {
        let rate = |m: &randr::ModeInfo| {
            let total = u64::from(m.htotal) * u64::from(m.vtotal);
            (u64::from(m.dot_clock) * 1000)
                .checked_div(total)
                .unwrap_or(0)
        };
        let mode = target
            .resources
            .modes
            .iter()
            .filter(|m| target.output_modes.contains(&m.id))
            .filter(|m| u32::from(m.width) == width && u32::from(m.height) == height)
            .max_by_key(|m| rate(m))
            .ok_or_else(|| format!("that screen has no {width}x{height} mode"))?;
        let outputs = target.info.outputs.clone();
        let (w, h) = (mode.width, mode.height);
        // The screen's size in millimetres at 96 dots per inch (what desktops assume).
        let mm = |pixels: u16| ((f64::from(pixels) * 25.4 / 96.0).round() as u32).max(1);
        let grow = u32::from(w) > u32::from(target.info.width)
            || u32::from(h) > u32::from(target.info.height);
        let screen = |connection: &RustConnection| {
            connection
                .randr_set_screen_size(root, w, h, mm(w), mm(h))
                .map_err(x)?
                .check()
                .map_err(x)
        };
        // Larger: the screen first, then the CRTC (it must fit); smaller: the other way round.
        if grow {
            screen(connection)?;
        }
        let reply = connection
            .randr_set_crtc_config(
                target.crtc,
                x11rb::CURRENT_TIME,
                target.resources.config_timestamp,
                target.info.x,
                target.info.y,
                mode.id,
                target.info.rotation,
                &outputs,
            )
            .map_err(x)?
            .reply()
            .map_err(x)?;
        if reply.status != randr::SetConfig::SUCCESS {
            return Err(format!(
                "the X server refused the mode ({:?})",
                reply.status
            ));
        }
        if !grow {
            screen(connection)?;
        }
        connection.flush().map_err(x)?;
        Ok(())
    }

    pub(super) fn set(index: usize, width: u32, height: u32) -> Result<(), String> {
        if wayland() {
            wayland_screen(index)?;
            return queqiao_portal::display_config::set(width, height);
        }
        let (connection, root) = connect()?;
        let target = target(&connection, root, index)?;
        apply(&connection, root, &target, width, height)
    }

    /// The output named `name` back to `original`.
    pub(super) fn restore_named(name: &str, original: (u32, u32)) -> Result<(), String> {
        if wayland() {
            let _ = name;
            return queqiao_portal::display_config::restore(original);
        }
        let (connection, root) = connect()?;
        let monitors = connection
            .randr_get_monitors(root, true)
            .map_err(x)?
            .reply()
            .map_err(x)?
            .monitors;
        for index in 0..monitors.len() {
            if let Ok(target) = target(&connection, root, index)
                && target.output_name == name
            {
                if (u32::from(target.info.width), u32::from(target.info.height)) == original {
                    return Ok(());
                }
                return apply(&connection, root, &target, original.0, original.1);
            }
        }
        Err(format!("output {name} not found"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_are_offered_once_and_largest_first() {
        let sizes = tidy(vec![
            (1920, 1080),
            (640, 480),
            (2560, 1440),
            (1920, 1080),
            (1280, 720),
            (1600, 900),
            (1280, 1024),
        ]);
        assert_eq!(
            sizes,
            // 640 x 480 is below the smallest offered.
            [
                (2560, 1440),
                (1920, 1080),
                (1600, 900),
                (1280, 1024),
                (1280, 720)
            ]
        );
    }
}
