//! The session window's control center (the bar's "Control center" button), on every system: what it shows, apart
//! from the windows that show it (Windows: control_center.rs; Linux: linux/control_center.rs). As UU Remote's, a column
//! of groups (Screen, Picture, Window, Sound, Security, Input, Tools, More), each opening its choices beside it, then
//! Full screen and End session. Choosing an item gives the window's menu command with its id (as the system menu's
//! items do); a slider gives `WindowEvent::Value`. Check marks follow the window's `check_menu_item`.

/// An icon of the control center (Windows: a Segoe MDL2 Assets glyph; Linux: a symbol egui's fonts have).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PanelIcon {
    None,
    Screen,
    NextScreen,
    Picture,
    Window,
    Sound,
    Security,
    Input,
    Tools,
    More,
    FullScreen,
    End,
}

impl PanelIcon {
    /// The Segoe MDL2 Assets glyph.
    pub fn glyph(self) -> &'static str {
        match self {
            PanelIcon::None => "",
            PanelIcon::Screen => "\u{E7F4}",
            PanelIcon::NextScreen => "\u{E8AB}",
            PanelIcon::Picture => "\u{E91B}",
            PanelIcon::Window => "\u{E737}",
            PanelIcon::Sound => "\u{E767}",
            PanelIcon::Security => "\u{E72E}",
            PanelIcon::Input => "\u{E765}",
            PanelIcon::Tools => "\u{E90F}",
            PanelIcon::More => "\u{E712}",
            PanelIcon::FullScreen => "\u{E740}",
            PanelIcon::End => "\u{E7E8}",
        }
    }

    /// A symbol for fonts without Segoe's glyphs (egui's emoji and icon fonts have these).
    pub fn symbol(self) -> &'static str {
        match self {
            PanelIcon::None => "",
            PanelIcon::Screen => "🖵",
            PanelIcon::NextScreen => "⇄",
            PanelIcon::Picture => "🖼",
            PanelIcon::Window => "□",
            PanelIcon::Sound => "🔊",
            PanelIcon::Security => "🛡",
            PanelIcon::Input => "⌨",
            PanelIcon::Tools => "🔧",
            PanelIcon::More => "☰",
            PanelIcon::FullScreen => "⛶",
            PanelIcon::End => "✖",
        }
    }
}

/// How an item shows that it is on: a check mark (on or off by itself) or a dot (one of a group).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mark {
    None,
    Check,
    Radio,
}

#[derive(Clone, Debug, PartialEq)]
pub enum PanelItem {
    /// A choice: its `id` comes back as the window's menu command.
    Action {
        id: u16,
        icon: PanelIcon,
        label: String,
        /// A line or two under the label, in small grey text (what it does).
        detail: String,
        /// A key that does the same ("Ctrl+Alt+Enter"), at the right.
        shortcut: String,
        mark: Mark,
        /// Ends something (End session): in red.
        danger: bool,
        /// Shown, but not to be chosen now.
        enabled: bool,
    },
    /// A heading inside a group ("Keyboard", "Mouse").
    Heading(String),
    Separator,
    /// A level from 0 to 100 set by dragging (the sound's volume); each change comes as `WindowEvent::Value`.
    Slider {
        id: u16,
        label: String,
        value: u8,
    },
    /// A group: its items open beside the column.
    Group {
        icon: PanelIcon,
        label: String,
        items: Vec<PanelItem>,
    },
}

impl PanelItem {
    /// A plain choice.
    pub fn action(id: u16, label: &str) -> PanelItem {
        PanelItem::Action {
            id,
            icon: PanelIcon::None,
            label: label.to_string(),
            detail: String::new(),
            shortcut: String::new(),
            mark: Mark::None,
            danger: false,
            enabled: true,
        }
    }

    /// A choice that is on or off by itself (a check mark when on).
    pub fn check(id: u16, label: &str) -> PanelItem {
        PanelItem::action(id, label).marked(Mark::Check)
    }

    /// One choice of a group (a dot on the one that is on).
    pub fn radio(id: u16, label: &str) -> PanelItem {
        PanelItem::action(id, label).marked(Mark::Radio)
    }

    pub fn group(icon: PanelIcon, label: &str, items: Vec<PanelItem>) -> PanelItem {
        PanelItem::Group {
            icon,
            label: label.to_string(),
            items,
        }
    }

    pub fn heading(label: &str) -> PanelItem {
        PanelItem::Heading(label.to_string())
    }

    fn marked(mut self, new: Mark) -> PanelItem {
        if let PanelItem::Action { mark, .. } = &mut self {
            *mark = new;
        }
        self
    }

    /// With a line or two under the label.
    pub fn detail(mut self, text: &str) -> PanelItem {
        if let PanelItem::Action { detail, .. } = &mut self {
            *detail = text.to_string();
        }
        self
    }

    /// With the key that does the same, at the right.
    pub fn shortcut(mut self, text: &str) -> PanelItem {
        if let PanelItem::Action { shortcut, .. } = &mut self {
            *shortcut = text.to_string();
        }
        self
    }

    pub fn icon(mut self, new: PanelIcon) -> PanelItem {
        if let PanelItem::Action { icon, .. } | PanelItem::Group { icon, .. } = &mut self {
            *icon = new;
        }
        self
    }

    /// Shown greyed: not to be chosen now.
    pub fn disabled(mut self) -> PanelItem {
        if let PanelItem::Action { enabled, .. } = &mut self {
            *enabled = false;
        }
        self
    }

    /// In red: it ends something.
    pub fn danger(mut self) -> PanelItem {
        if let PanelItem::Action { danger, .. } = &mut self {
            *danger = true;
        }
        self
    }

    /// The command id of a choice or slider.
    pub fn id(&self) -> Option<u16> {
        match self {
            PanelItem::Action { id, .. } | PanelItem::Slider { id, .. } => Some(*id),
            _ => None,
        }
    }

    /// Whether it can be pointed at (not a heading or a separator).
    pub fn selectable(&self) -> bool {
        match self {
            PanelItem::Action { enabled, .. } => *enabled,
            PanelItem::Slider { .. } | PanelItem::Group { .. } => true,
            PanelItem::Heading(_) | PanelItem::Separator => false,
        }
    }
}

/// Every item of `items` and its groups, for `f` to change (labels renamed, sliders set, items enabled).
pub fn for_each_item(items: &mut [PanelItem], f: &mut impl FnMut(&mut PanelItem)) {
    for item in items {
        f(item);
        if let PanelItem::Group { items, .. } = item {
            for_each_item(items, f);
        }
    }
}

/// New text for the item `id` (as `rename_menu_item` does for the system menu).
pub fn rename(items: &mut [PanelItem], id: u16, text: &str) {
    for_each_item(items, &mut |item| {
        if let PanelItem::Action { id: own, label, .. } = item
            && *own == id
        {
            *label = text.to_string();
        }
    });
}

/// The slider `id` at `value`.
pub fn set_value(items: &mut [PanelItem], id: u16, new: u8) {
    for_each_item(items, &mut |item| {
        if let PanelItem::Slider { id: own, value, .. } = item
            && *own == id
        {
            *value = new.min(100);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn items_are_found_inside_groups() {
        let mut items = vec![
            PanelItem::group(
                PanelIcon::Sound,
                "Sound",
                vec![
                    PanelItem::check(0x1030, "Play that PC's sound"),
                    PanelItem::Slider {
                        id: 0x1730,
                        label: "Volume".into(),
                        value: 100,
                    },
                ],
            ),
            PanelItem::action(0x1040, "Full screen")
                .icon(PanelIcon::FullScreen)
                .shortcut("Ctrl+Alt+Enter")
                .disabled(),
            PanelItem::heading("Keyboard"),
        ];
        rename(&mut items, 0x1030, "Sound here");
        set_value(&mut items, 0x1730, 140);
        let PanelItem::Group { items: inside, .. } = &items[0] else {
            panic!()
        };
        assert!(
            matches!(&inside[0], PanelItem::Action { label, mark: Mark::Check, .. } if label == "Sound here")
        );
        assert!(matches!(inside[1], PanelItem::Slider { value: 100, .. }));
        assert!(!items[1].selectable());
        assert_eq!(items[1].id(), Some(0x1040));
        assert!(!items[2].selectable());
        assert!(items[0].selectable());
    }
}
