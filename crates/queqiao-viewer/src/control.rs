//! The session window's control center (the bar's "Control center"), as UU Remote groups its own: Screen, Picture,
//! Window, Sound, Security, Input, Tools and More, each opening its choices beside the column, then Full screen and
//! End session. UU's choices and Queqiao's own, grouped anew: what Queqiao has that UU has not (codecs and their
//! colour, game mouse, command lines, opacity, hotkeys and the boss key) sits where a person would look for it.
//! The ids are the session window's menu ids (session.rs), so each choice works as its system-menu item does.

use queqiao_core::text::tr;
use queqiao_display::{PanelIcon, PanelItem};

use crate::session::{
    MAX_DISPLAY_MODES, MENU_ACTUAL_SIZE, MENU_CHROMA444, MENU_CODEC, MENU_COMMAND_LINES,
    MENU_CTRL_ALT_DEL, MENU_CUSTOM_BITRATE, MENU_DISPLAY_FIT, MENU_DISPLAY_MODES, MENU_DISPLAY_OWN,
    MENU_END, MENU_FIGURES, MENU_FILE_TRANSFER, MENU_FPS, MENU_FULL_SCREEN, MENU_GAME_MOUSE,
    MENU_HOTKEYS, MENU_LOCK, MENU_LOCK_AFTER, MENU_MICROPHONE, MENU_MOUSE_SPEED,
    MENU_MUTE_SPEAKERS, MENU_NEXT_SCREEN, MENU_ON_TOP, MENU_OPACITY, MENU_POINTER_SHAPE,
    MENU_PRIVACY, MENU_QUALITY, MENU_RESOLUTION, MENU_RESTART, MENU_RESTART_QUEQIAO, MENU_SCREENS,
    MENU_SEND_FILES, MENU_SHORTCUTS, MENU_SOUND, MENU_TEN_BIT, MENU_TERMINAL, MENU_VOLUME,
};

/// The groups' places in the column (tests open one by its place).
pub(crate) const GROUP_PICTURE: usize = 2;

/// The control center's items. `screens`: that PC's screens once it told them (names); `full_screen_key`,
/// `game_mouse_key`: the keys as the person set them; `volume`: the sound's volume now.
/// That PC's resolution as it last told it (`Message::DisplayModes`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ThatResolution {
    pub(crate) current: (u16, u16),
    pub(crate) own: (u16, u16),
    pub(crate) sizes: Vec<(u16, u16)>,
    pub(crate) note: String,
}

/// The size of `sizes` nearest to `wanted` that fits in it (this window); else the smallest.
pub(crate) fn fitting(sizes: &[(u32, u32)], wanted: (u32, u32)) -> Option<(u32, u32)> {
    sizes
        .iter()
        .copied()
        .filter(|(w, h)| *w <= wanted.0 && *h <= wanted.1)
        .max_by_key(|(w, h)| (w * h, *w))
        .or_else(|| sizes.iter().copied().min_by_key(|(w, h)| w * h))
}

pub(crate) fn items(
    screens: &[String],
    display: Option<&ThatResolution>,
    full_screen_key: &str,
    game_mouse_key: &str,
    volume: u8,
) -> Vec<PanelItem> {
    vec![
        PanelItem::group(
            PanelIcon::Screen,
            tr("Screen"),
            screen_items(screens, display),
        ),
        PanelItem::action(MENU_NEXT_SCREEN, tr("Next screen")).icon(PanelIcon::NextScreen),
        PanelItem::group(PanelIcon::Picture, tr("Picture"), picture_items()),
        PanelItem::group(PanelIcon::Window, tr("Window"), window_items()),
        PanelItem::group(PanelIcon::Sound, tr("Sound"), sound_items(volume)),
        PanelItem::group(PanelIcon::Security, tr("Security"), security_items()),
        PanelItem::group(PanelIcon::Input, tr("Input"), input_items(game_mouse_key)),
        PanelItem::group(PanelIcon::Tools, tr("Tools"), tool_items()),
        PanelItem::group(PanelIcon::More, tr("More"), more_items()),
        PanelItem::Separator,
        PanelItem::action(MENU_FULL_SCREEN, tr("Full screen"))
            .icon(PanelIcon::FullScreen)
            .shortcut(full_screen_key),
        PanelItem::action(MENU_END, tr("End session"))
            .icon(PanelIcon::End)
            .danger(),
    ]
}

/// The Screen group: that PC's screens (one choice each), its resolution (its own, one fitting this window, its
/// sizes), then the picture's size.
pub(crate) fn screen_items(screens: &[String], display: Option<&ThatResolution>) -> Vec<PanelItem> {
    let mut items = vec![PanelItem::heading(tr("That PC's screens"))];
    if screens.is_empty() {
        items.push(PanelItem::action(0, tr("One screen")).disabled());
    }
    items.extend(
        screens
            .iter()
            .enumerate()
            .map(|(index, name)| PanelItem::radio(MENU_SCREENS + 16 * index as u16, name)),
    );
    items.push(PanelItem::Separator);
    items.push(PanelItem::heading(tr("That PC's resolution")));
    match display {
        None => items.push(PanelItem::action(0, tr("Not offered by that PC's Queqiao")).disabled()),
        Some(display) if display.sizes.is_empty() => items.push(
            PanelItem::action(0, tr("Cannot be changed"))
                .detail(&display.note)
                .disabled(),
        ),
        Some(display) => {
            let own = format!("{} ({}x{})", tr("Its own"), display.own.0, display.own.1);
            items.push(PanelItem::radio(MENU_DISPLAY_OWN, &own));
            items.push(
                PanelItem::action(MENU_DISPLAY_FIT, tr("Fit this window")).detail(tr(
                    "The size of that PC's screen nearest to this window's, for a sharp picture.",
                )),
            );
            items.extend(
                display
                    .sizes
                    .iter()
                    .take(usize::from(MAX_DISPLAY_MODES))
                    .enumerate()
                    .map(|(index, (w, h))| {
                        PanelItem::radio(
                            MENU_DISPLAY_MODES + 16 * index as u16,
                            &format!("{w}x{h}"),
                        )
                    }),
            );
        }
    }
    items.push(PanelItem::Separator);
    items.push(PanelItem::heading(tr("Picture size")));
    items.extend(
        MENU_RESOLUTION
            .iter()
            .map(|(id, _, text)| PanelItem::radio(*id, tr(text))),
    );
    items
}

fn picture_items() -> Vec<PanelItem> {
    let details = [
        tr("Fewer bits: for slow networks."),
        tr("As many bits as the picture's size wants."),
        tr("Twice the bits: sharp text."),
    ];
    let mut items = vec![PanelItem::heading(tr("Quality"))];
    items.extend(
        MENU_QUALITY
            .iter()
            .zip(details)
            .map(|((id, _, text), detail)| PanelItem::radio(*id, tr(text)).detail(detail)),
    );
    items.push(PanelItem::action(
        MENU_CUSTOM_BITRATE,
        tr("Custom bitrate..."),
    ));
    items.push(PanelItem::Separator);
    items.push(PanelItem::heading(tr("Frame rate")));
    items.extend(
        MENU_FPS
            .iter()
            .map(|(id, _, text)| PanelItem::radio(*id, tr(text))),
    );
    items.push(PanelItem::Separator);
    items.push(PanelItem::heading(tr("Codec")));
    items.extend(
        MENU_CODEC
            .iter()
            .map(|(id, _, text)| PanelItem::radio(*id, tr(text))),
    );
    items.push(
        PanelItem::check(MENU_CHROMA444, tr("4:4:4 colour (H.265)"))
            .detail(tr("Sharp coloured text; both PCs must handle it.")),
    );
    items.push(
        PanelItem::check(MENU_TEN_BIT, tr("10-bit colour (H.265)"))
            .detail(tr("Smooth gradients; both PCs must handle it.")),
    );
    items
}

fn window_items() -> Vec<PanelItem> {
    let mut items = vec![
        PanelItem::action(MENU_ACTUAL_SIZE, tr("Actual size (1:1)")),
        PanelItem::check(MENU_ON_TOP, tr("Always on top")),
        PanelItem::Separator,
        PanelItem::heading(tr("Opacity")),
    ];
    items.extend(
        MENU_OPACITY
            .iter()
            .map(|(id, _, text)| PanelItem::radio(*id, tr(text))),
    );
    items
}

fn sound_items(volume: u8) -> Vec<PanelItem> {
    vec![
        PanelItem::check(MENU_SOUND, tr("Play that PC's sound"))
            .detail(tr("Off: that PC sends no sound at all.")),
        PanelItem::Slider {
            id: MENU_VOLUME,
            label: tr("Volume here").to_string(),
            value: volume,
        },
    ]
}

fn security_items() -> Vec<PanelItem> {
    vec![
        PanelItem::check(MENU_LOCK_AFTER, tr("Lock that PC when the session ends"))
            .detail(tr("Its screen locks as soon as you leave.")),
        PanelItem::check(MENU_PRIVACY, tr("Darken that PC's screen"))
            .detail(tr("People at that PC see a dark screen, and their keyboard and mouse do nothing; you still see everything.")),
        PanelItem::check(MENU_MUTE_SPEAKERS, tr("Mute that PC's speakers"))
            .detail(tr("Its sound still comes to you; the room there stays quiet.")),
        PanelItem::Separator,
        PanelItem::action(MENU_LOCK, tr("Lock that PC now")),
        PanelItem::action(MENU_CTRL_ALT_DEL, tr("Send Ctrl+Alt+Del")),
    ]
}

fn input_items(game_mouse_key: &str) -> Vec<PanelItem> {
    let mut items = vec![
        PanelItem::heading(tr("Keyboard")),
        PanelItem::check(MENU_SHORTCUTS, tr("Send system shortcuts to that PC")).detail(tr(
            "Win, Alt+Tab and the like go to that PC while this window is in front.",
        )),
        PanelItem::action(MENU_HOTKEYS, tr("Hotkeys...")),
        PanelItem::Separator,
        PanelItem::heading(tr("Mouse")),
        PanelItem::check(MENU_POINTER_SHAPE, tr("That PC's pointer shape"))
            .detail(tr("The pointer here takes the shape it has there.")),
        PanelItem::check(MENU_GAME_MOUSE, tr("Game mouse"))
            .shortcut(game_mouse_key)
            .detail(tr(
                "The pointer stays in the window and sends movement: for games that turn the view.",
            )),
    ];
    items.extend(
        MENU_MOUSE_SPEED
            .iter()
            .map(|(id, _, text)| PanelItem::radio(*id, tr(text))),
    );
    items.push(PanelItem::Separator);
    items.push(PanelItem::heading(tr("Microphone")));
    items.push(
        PanelItem::check(MENU_MICROPHONE, tr("Send my microphone"))
            .detail(tr("Programs there hear this PC's microphone.")),
    );
    items
}

fn tool_items() -> Vec<PanelItem> {
    // Drawing on that PC's screen first (annotate.rs): what someone helping reaches for.
    let mut items = crate::annotate::panel_items();
    items.extend([
        PanelItem::action(MENU_FILE_TRANSFER, tr("File transfer")),
        PanelItem::action(MENU_SEND_FILES, tr("Send files...")),
        PanelItem::Separator,
        PanelItem::action(MENU_TERMINAL, tr("Open a command line")),
        PanelItem::action(MENU_COMMAND_LINES, tr("Command lines...")),
        PanelItem::Separator,
        PanelItem::action(MENU_RESTART_QUEQIAO, tr("Restart Queqiao on that PC")),
        PanelItem::action(MENU_RESTART, tr("Restart that PC...")).danger(),
    ]);
    items
}

fn more_items() -> Vec<PanelItem> {
    vec![
        PanelItem::check(MENU_FIGURES, tr("Show the session's figures")).detail(tr(
            "Direct or relayed, delay, frames, data and codec, at the bottom right.",
        )),
    ]
}

#[cfg(test)]
mod tests {
    use queqiao_display::PanelItem;

    #[test]
    fn the_size_fitting_this_window_is_the_largest_inside_it() {
        let sizes = [(2560, 1440), (1920, 1080), (1600, 900), (1280, 1024)];
        assert_eq!(super::fitting(&sizes, (1920, 1050)), Some((1600, 900)));
        assert_eq!(super::fitting(&sizes, (3840, 2160)), Some((2560, 1440)));
        assert_eq!(super::fitting(&sizes, (1000, 700)), Some((1280, 1024)));
        assert_eq!(super::fitting(&[], (1000, 700)), None);
    }

    #[test]
    fn every_choice_has_a_menu_id_of_its_own() {
        let display = super::ThatResolution {
            current: (1920, 1080),
            own: (2560, 1440),
            sizes: vec![(2560, 1440), (1920, 1080), (1280, 720)],
            note: String::new(),
        };
        let items = super::items(
            &["Screen 1".into(), "Screen 2".into()],
            Some(&display),
            "Ctrl+Alt+Enter",
            "Ctrl+Alt+M",
            100,
        );
        let mut ids = Vec::new();
        fn collect(items: &[PanelItem], ids: &mut Vec<u16>) {
            for item in items {
                match item {
                    PanelItem::Group { items, .. } => collect(items, ids),
                    other => ids.extend(other.id().filter(|id| *id != 0)),
                }
            }
        }
        collect(&items, &mut ids);
        let unique: std::collections::HashSet<u16> = ids.iter().copied().collect();
        assert_eq!(unique.len(), ids.len(), "an id twice: {ids:x?}");
        for id in &ids {
            assert_eq!(id & 0xF, 0, "{id:#x}");
            assert!(*id < 0xF000, "{id:#x}");
        }
        assert!(matches!(items[0], PanelItem::Group { .. }));
        assert!(matches!(
            items[super::GROUP_PICTURE],
            PanelItem::Group { .. }
        ));
    }
}
