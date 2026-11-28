//! GNOME on Wayland: the screen's resolution through mutter's display configuration (org.gnome.Mutter.DisplayConfig on
//! the session bus, the interface GNOME's own Settings use; its description is mutter's data/dbus-interfaces): the
//! monitor's modes, one applied for the session ("temporary": never saved into monitors.xml, so the person's own
//! settings stay), and the mode it had applied back.
//!
//! One screen at a time, as on X11: with several monitors their layout stays as it is (refused for now).

use std::collections::HashMap;

use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::OwnedValue;

const NAME: &str = "org.gnome.Mutter.DisplayConfig";
const PATH: &str = "/org/gnome/Mutter/DisplayConfig";

/// ApplyMonitorsConfig's methods: verify 0, temporary 1, persistent 2.
const TEMPORARY: u32 = 1;

type Properties = HashMap<String, OwnedValue>;
/// A monitor's connector, vendor, product, serial.
type Spec = (String, String, String, String);
/// id, width, height, refresh rate, preferred scale, supported scales, properties.
type Mode = (String, i32, i32, f64, f64, Vec<f64>, Properties);
type Monitor = (Spec, Vec<Mode>, Properties);
/// x, y, scale, transform, primary, monitors, properties.
type Logical = (i32, i32, f64, u32, bool, Vec<Spec>, Properties);
type State = (u32, Vec<Monitor>, Vec<Logical>, Properties);
/// A monitor's size now, and its sizes.
pub type Sizes = ((u32, u32), Vec<(u32, u32)>);

fn error(what: &str) -> impl Fn(zbus::Error) -> String + '_ {
    move |e| format!("GNOME's display settings ({what}): {e}")
}

fn flag(properties: &Properties, key: &str) -> bool {
    properties
        .get(key)
        .and_then(|value| bool::try_from(value).ok())
        .unwrap_or(false)
}

struct Current {
    proxy: Proxy<'static>,
    serial: u32,
    logical: Logical,
    monitor: Monitor,
    mode: Mode,
}

fn current() -> Result<Current, String> {
    let connection = Connection::session().map_err(error("the session bus"))?;
    let proxy = Proxy::new(&connection, NAME, PATH, NAME).map_err(error("connect"))?;
    let (serial, monitors, logical, _): State = proxy
        .call("GetCurrentState", &())
        .map_err(error("GetCurrentState"))?;
    if logical.len() != 1 {
        return Err("this PC shows several screens: their layout stays as it is".into());
    }
    let logical = logical.into_iter().next().ok_or("no screen")?;
    let spec = logical
        .5
        .first()
        .cloned()
        .ok_or("a screen without a monitor")?;
    let monitor = monitors
        .into_iter()
        .find(|monitor| monitor.0 == spec)
        .ok_or("the screen's monitor is not listed")?;
    let mode = monitor
        .1
        .iter()
        .find(|mode| flag(&mode.6, "is-current"))
        .map(|mode| {
            (
                mode.0.clone(),
                mode.1,
                mode.2,
                mode.3,
                mode.4,
                mode.5.clone(),
                Properties::new(),
            )
        })
        .ok_or("the monitor's current mode is not known")?;
    Ok(Current {
        proxy,
        serial,
        logical,
        monitor,
        mode,
    })
}

/// The monitor's size now and its sizes (each once, any order: the caller sorts).
pub fn modes() -> Result<Sizes, String> {
    let current = current()?;
    let sizes = current
        .monitor
        .1
        .iter()
        .map(|mode| (mode.1.max(0) as u32, mode.2.max(0) as u32))
        .collect();
    Ok((
        (current.mode.1.max(0) as u32, current.mode.2.max(0) as u32),
        sizes,
    ))
}

/// The monitor's connector ("Virtual-1", "eDP-1") and its size now.
pub fn name_and_size() -> Result<(String, (u32, u32)), String> {
    let current = current()?;
    Ok((
        current.monitor.0.0.clone(),
        (current.mode.1.max(0) as u32, current.mode.2.max(0) as u32),
    ))
}

/// The monitor to its `width` x `height` mode (at the refresh rate it has where it can, else its highest), applied
/// for the session only.
pub fn set(width: u32, height: u32) -> Result<(), String> {
    let current = current()?;
    let mode = current
        .monitor
        .1
        .iter()
        .filter(|mode| mode.1.max(0) as u32 == width && mode.2.max(0) as u32 == height)
        .max_by(|a, b| {
            let same = |mode: &Mode| (mode.3 - current.mode.3).abs() < 0.5;
            (same(a), a.3)
                .partial_cmp(&(same(b), b.3))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .ok_or_else(|| format!("that screen has no {width}x{height} mode"))?;
    apply(&current, mode)
}

/// The monitor back to a mode of `size` (its own, as it was before the session changed it).
pub fn restore(size: (u32, u32)) -> Result<(), String> {
    let current = current()?;
    if (current.mode.1.max(0) as u32, current.mode.2.max(0) as u32) == size {
        return Ok(());
    }
    set(size.0, size.1)
}

fn apply(current: &Current, mode: &Mode) -> Result<(), String> {
    let (x, y, scale, transform, primary, _, _) = &current.logical;
    // The scale stays where the new mode allows it, else the mode's own.
    let scale = if mode.5.iter().any(|s| (s - scale).abs() < 0.01) {
        *scale
    } else {
        mode.4
    };
    let monitors = vec![(
        current.monitor.0.0.clone(),
        mode.0.clone(),
        HashMap::<String, OwnedValue>::new(),
    )];
    let logical = vec![(*x, *y, scale, *transform, *primary, monitors)];
    current
        .proxy
        .call_method(
            "ApplyMonitorsConfig",
            &(
                current.serial,
                TEMPORARY,
                logical,
                HashMap::<String, OwnedValue>::new(),
            ),
        )
        .map(|_| ())
        .map_err(error("ApplyMonitorsConfig"))
}
