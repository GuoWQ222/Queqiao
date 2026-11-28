//! GNOME on Wayland: the screen's resolution through mutter's display configuration (org.gnome.Mutter.DisplayConfig on
//! the session bus, the interface GNOME's own Settings use; its description is mutter's data/dbus-interfaces): the
//! monitor's modes, one applied for the session ("temporary": never saved into monitors.xml, so the person's own
//! settings stay), and the mode it had applied back.
//!
//! One screen at a time, as on X11: with several monitors their layout stays as it is (refused for now).
//!
//! Super Screen (super_screen.rs) also changes the layout here: its virtual monitor the only one, and the layout seen
//! before put back afterwards (both "temporary").

use std::collections::HashMap;

use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::{OwnedValue, Value};

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
/// The serial, the monitors, the logical monitors, the properties (GetCurrentState).
pub(crate) type State = (u32, Vec<Monitor>, Vec<Logical>, Properties);
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

/// One logical monitor as ApplyMonitorsConfig takes it: x, y, scale, transform, primary, and its monitors (connector,
/// mode id).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Place {
    pub(crate) x: i32,
    pub(crate) y: i32,
    pub(crate) scale: f64,
    pub(crate) transform: u32,
    pub(crate) primary: bool,
    pub(crate) monitors: Vec<(String, String)>,
}

/// The monitors' layout: the logical monitors (in order of place), and the layout mode where mutter lets it change.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Layout {
    pub(crate) places: Vec<Place>,
    pub(crate) layout_mode: Option<u32>,
}

impl Layout {
    /// Its monitors' connectors, for the report ("Meta-0", "eDP-1 + HDMI-1").
    pub(crate) fn describe(&self) -> String {
        let names: Vec<&str> = self
            .places
            .iter()
            .flat_map(|place| {
                place
                    .monitors
                    .iter()
                    .map(|(connector, _)| connector.as_str())
            })
            .collect();
        names.join(" + ")
    }
}

/// The state now (and the proxy that read it, to apply a layout on it).
pub(crate) fn state() -> Result<(Proxy<'static>, State), String> {
    let connection = Connection::session().map_err(error("the session bus"))?;
    let proxy = Proxy::new(&connection, NAME, PATH, NAME).map_err(error("connect"))?;
    let state: State = proxy
        .call("GetCurrentState", &())
        .map_err(error("GetCurrentState"))?;
    Ok((proxy, state))
}

/// The monitors' connectors.
pub(crate) fn connectors(state: &State) -> Vec<String> {
    state.1.iter().map(|monitor| monitor.0.0.clone()).collect()
}

/// The layout as mutter has it now; None where a monitor shown has no current mode (it is changing).
pub(crate) fn layout_of(state: &State) -> Option<Layout> {
    let mut places = Vec::new();
    for (x, y, scale, transform, primary, specs, _) in &state.2 {
        let mut monitors = Vec::new();
        for spec in specs {
            let monitor = state.1.iter().find(|monitor| &monitor.0 == spec)?;
            let mode = monitor.1.iter().find(|mode| flag(&mode.6, "is-current"))?;
            monitors.push((spec.0.clone(), mode.0.clone()));
        }
        places.push(Place {
            x: *x,
            y: *y,
            scale: *scale,
            transform: *transform,
            primary: *primary,
            monitors,
        });
    }
    places.sort_by_key(|place| (place.x, place.y));
    let layout_mode = flag(&state.3, "supports-changing-layout-mode")
        .then(|| {
            state
                .3
                .get("layout-mode")
                .and_then(|value| u32::try_from(value).ok())
        })
        .flatten();
    Some(Layout {
        places,
        layout_mode,
    })
}

/// A monitor that was not there before (`before`: the connectors then), preferring one that has a mode of `size`.
pub(crate) fn new_monitor(state: &State, before: &[String], size: (u32, u32)) -> Option<String> {
    let new: Vec<&Monitor> = state
        .1
        .iter()
        .filter(|monitor| !before.contains(&monitor.0.0))
        .collect();
    let has_size = |monitor: &&&Monitor| {
        monitor
            .1
            .iter()
            .any(|mode| (mode.1.max(0) as u32, mode.2.max(0) as u32) == size)
    };
    new.iter()
        .find(has_size)
        .or(new.first())
        .map(|monitor| monitor.0.0.clone())
}

/// The layout with only the monitor `connector`, at the top left, primary, at its current mode (else its preferred,
/// else its first) and that mode's preferred scale.
pub(crate) fn only_layout(state: &State, connector: &str) -> Result<Layout, String> {
    let monitor = state
        .1
        .iter()
        .find(|monitor| monitor.0.0 == connector)
        .ok_or_else(|| format!("no monitor {connector}"))?;
    let mode = monitor
        .1
        .iter()
        .find(|mode| flag(&mode.6, "is-current"))
        .or_else(|| monitor.1.iter().find(|mode| flag(&mode.6, "is-preferred")))
        .or(monitor.1.first())
        .ok_or_else(|| format!("the monitor {connector} has no mode"))?;
    let scale = if mode.4 > 0.0 { mode.4 } else { 1.0 };
    Ok(Layout {
        places: vec![Place {
            x: 0,
            y: 0,
            scale,
            transform: 0,
            primary: true,
            monitors: vec![(connector.to_string(), mode.0.clone())],
        }],
        layout_mode: layout_of(state).and_then(|layout| layout.layout_mode),
    })
}

/// `layout` applied for the session only ("temporary"), on the state `serial` read by `proxy`.
pub(crate) fn apply_layout(proxy: &Proxy, serial: u32, layout: &Layout) -> Result<(), String> {
    type Assigned = (String, String, HashMap<String, OwnedValue>);
    let logical: Vec<(i32, i32, f64, u32, bool, Vec<Assigned>)> = layout
        .places
        .iter()
        .map(|place| {
            (
                place.x,
                place.y,
                place.scale,
                place.transform,
                place.primary,
                place
                    .monitors
                    .iter()
                    .map(|(connector, mode)| (connector.clone(), mode.clone(), HashMap::new()))
                    .collect(),
            )
        })
        .collect();
    let mut properties: HashMap<&str, Value> = HashMap::new();
    if let Some(mode) = layout.layout_mode {
        properties.insert("layout-mode", Value::U32(mode));
    }
    proxy
        .call_method(
            "ApplyMonitorsConfig",
            &(serial, TEMPORARY, logical, properties),
        )
        .map(|_| ())
        .map_err(error("ApplyMonitorsConfig"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mode(id: &str, size: (i32, i32), current: bool) -> Mode {
        let mut properties = Properties::new();
        if current {
            properties.insert("is-current".into(), OwnedValue::from(true));
        }
        (
            id.into(),
            size.0,
            size.1,
            60.0,
            1.0,
            vec![1.0, 2.0],
            properties,
        )
    }

    fn spec(connector: &str) -> Spec {
        (
            connector.into(),
            "MetaVendor".into(),
            "Virtual remote monitor".into(),
            "0x000001".into(),
        )
    }

    /// The PC's own monitor (Meta-0, 1920x1080) and Super Screen's (Meta-1, 2560x1440, not shown yet).
    fn state_with_virtual() -> State {
        let own: Monitor = (
            spec("Meta-0"),
            vec![mode("1920x1080@60", (1920, 1080), true)],
            Properties::new(),
        );
        let virtual_monitor: Monitor = (
            spec("Meta-1"),
            vec![mode("2560x1440@60", (2560, 1440), true)],
            Properties::new(),
        );
        let logical: Logical = (0, 0, 1.0, 0, true, vec![spec("Meta-0")], Properties::new());
        let beside: Logical = (
            1920,
            0,
            1.0,
            0,
            false,
            vec![spec("Meta-1")],
            Properties::new(),
        );
        (
            7,
            vec![own, virtual_monitor],
            vec![beside, logical],
            Properties::new(),
        )
    }

    #[test]
    fn the_virtual_monitor_is_found_and_made_the_only_one() {
        let state = state_with_virtual();
        assert_eq!(connectors(&state), ["Meta-0", "Meta-1"]);
        assert_eq!(
            new_monitor(&state, &["Meta-0".into()], (2560, 1440)).as_deref(),
            Some("Meta-1")
        );
        assert_eq!(
            new_monitor(&state, &["Meta-0".into(), "Meta-1".into()], (2560, 1440)),
            None
        );
        let only = only_layout(&state, "Meta-1").unwrap();
        assert_eq!(
            only.places,
            [Place {
                x: 0,
                y: 0,
                scale: 1.0,
                transform: 0,
                primary: true,
                monitors: vec![("Meta-1".into(), "2560x1440@60".into())],
            }]
        );
        assert!(only_layout(&state, "HDMI-1").is_err());
    }

    #[test]
    fn layouts_compare_whatever_order_mutter_lists_them_in() {
        let state = state_with_virtual();
        let layout = layout_of(&state).unwrap();
        // Sorted by place: the PC's own first.
        assert_eq!(layout.describe(), "Meta-0 + Meta-1");
        let mut swapped = state_with_virtual();
        swapped.2.reverse();
        assert_eq!(layout_of(&swapped), Some(layout.clone()));
        // Without the layout mode where mutter does not let it change.
        assert_eq!(layout.layout_mode, None);
        // A monitor shown without a current mode (changing): no layout.
        let mut changing = state_with_virtual();
        changing.1[1].1[0].6 = Properties::new();
        assert_eq!(layout_of(&changing), None);
    }
}
