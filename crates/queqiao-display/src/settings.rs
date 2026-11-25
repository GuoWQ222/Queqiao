//! The Settings page's contents, the same on Windows and Linux: tabs (as UU Remote's Settings: General, Security,
//! Keyboard; Queqiao's own "Sessions" for what this PC allows while it is controlled; no proxy), each a column of
//! cards. A card has an icon, a title, a line on what it does, and on the right a switch (with "On" / "Off"), a choice,
//! buttons, or the keys of a shortcut with a pencil. A click gives a tray menu id, handled by `Presence::choose` (the
//! settings password and the lock while someone helps apply as in the tray menu).

use queqiao_core::hotkeys::Keys;
use queqiao_core::text::{LanguageChoice, tr, trf};

use crate::model::HelpMode;
use crate::tray::{
    MENU_ALLOW, MENU_ASK, MENU_AUTO_UPDATE, MENU_AUTOSTART, MENU_CLIPBOARD, MENU_FILES,
    MENU_FIXED_CODE, MENU_HELP_ASK_ONLY, MENU_HELP_BOTH, MENU_HELP_CODE_ONLY, MENU_HELP_FILES,
    MENU_KEY_BOSS, MENU_KEY_FULL_SCREEN, MENU_KEY_GAME_MOUSE, MENU_KEYS_DEFAULT, MENU_LANGUAGE_EN,
    MENU_LANGUAGE_SYSTEM, MENU_LANGUAGE_ZH, MENU_LOCK_AFTER, MENU_PASSWORD, MENU_TERMINAL,
    MENU_UPDATE_CHECK, MENU_WOL_CHECK, autostart_text,
};

/// What the Settings page shows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Shown {
    pub help_mode: HelpMode,
    pub help_files: bool,
    pub allow: bool,
    pub ask: bool,
    pub clipboard: bool,
    pub files: bool,
    pub terminal: bool,
    pub autostart: bool,
    pub lock_after: bool,
    pub auto_update: bool,
    pub password: bool,
    pub fixed_code: bool,
    pub language: LanguageChoice,
    pub keys: Keys,
    pub version: String,
}

/// The Settings page's tabs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Tab {
    #[default]
    General,
    Security,
    Sessions,
    Keyboard,
}

impl Tab {
    pub(crate) const ALL: [Tab; 4] = [Tab::General, Tab::Security, Tab::Sessions, Tab::Keyboard];

    pub(crate) fn title(self) -> &'static str {
        match self {
            Tab::General => tr("General"),
            Tab::Security => tr("Security"),
            Tab::Sessions => tr("When controlled"),
            Tab::Keyboard => tr("Keyboard"),
        }
    }
}

/// What a card has on its right.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Control {
    /// A switch: clicking it sends `id` (the setting turns over).
    Switch { id: u16, on: bool },
    /// One of several: the current one's name; choosing sends that option's id.
    Choice {
        value: String,
        options: Vec<(u16, String, bool)>,
    },
    /// Buttons, left to right.
    Buttons(Vec<(u16, String)>),
    /// A shortcut's keys ("Ctrl", "Alt", "Enter"; none: not set) and the pencil that changes it (`id`).
    Keys { id: u16, keys: Vec<String> },
}

/// One card: its icon (a Segoe MDL2 glyph on Windows), title, the line under it, and its control. A `sub` card belongs
/// to the one above it (drawn joined to it, indented).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Row {
    pub glyph: &'static str,
    pub title: String,
    pub detail: String,
    pub control: Control,
    pub sub: bool,
}

fn row(glyph: &'static str, title: &str, detail: &str, control: Control) -> Row {
    Row {
        glyph,
        title: title.to_string(),
        detail: detail.to_string(),
        control,
        sub: false,
    }
}

fn switch(id: u16, on: bool) -> Control {
    Control::Switch { id, on }
}

/// The cards of `tab`.
pub(crate) fn rows(tab: Tab, shown: &Shown) -> Vec<Row> {
    match tab {
        Tab::General => vec![
            row(
                "\u{E7E8}",
                autostart_text(),
                tr("Queqiao starts with this PC, so your devices can reach it."),
                switch(MENU_AUTOSTART, shown.autostart),
            ),
            row(
                "\u{E895}",
                tr("Install updates automatically"),
                tr("New versions are installed while no session runs."),
                switch(MENU_AUTO_UPDATE, shown.auto_update),
            ),
            row(
                "\u{E946}",
                tr("Version"),
                &trf("Queqiao {version}", &[("version", &shown.version)]),
                Control::Buttons(vec![(
                    MENU_UPDATE_CHECK,
                    tr("Check for updates").to_string(),
                )]),
            ),
            row(
                "\u{E774}",
                tr("Display language"),
                tr("The language of Queqiao's windows on this PC."),
                Control::Choice {
                    value: shown.language.label().to_string(),
                    options: [
                        (MENU_LANGUAGE_SYSTEM, LanguageChoice::System),
                        (MENU_LANGUAGE_ZH, LanguageChoice::Chinese),
                        (MENU_LANGUAGE_EN, LanguageChoice::English),
                    ]
                    .into_iter()
                    .map(|(id, choice)| (id, choice.label().to_string(), choice == shown.language))
                    .collect(),
                },
            ),
            row(
                "\u{E839}",
                tr("Wake-on-LAN"),
                tr("Whether your other devices can wake this PC when it sleeps or is off."),
                Control::Buttons(vec![(MENU_WOL_CHECK, tr("Check...").to_string())]),
            ),
        ],
        Tab::Security => {
            let mode = |mode: HelpMode| match mode {
                HelpMode::CodeAndAsk => tr("The code, then I allow it"),
                HelpMode::CodeOnly => tr("The code alone"),
                HelpMode::AskOnly => tr("No code: I allow each request"),
            };
            let fixed = Row {
                sub: true,
                ..row(
                    "",
                    tr("Fixed help code"),
                    tr("A code of your own that stays the same, instead of a new one each time."),
                    Control::Buttons(vec![(
                        MENU_FIXED_CODE,
                        if shown.fixed_code {
                            tr("Remove")
                        } else {
                            tr("Set...")
                        }
                        .to_string(),
                    )]),
                )
            };
            vec![
                row(
                    "\u{E7F4}",
                    tr("This account's devices may control this PC"),
                    tr("Your other devices, logged in to the same account, can connect to it."),
                    switch(MENU_ALLOW, shown.allow),
                ),
                row(
                    "\u{E8BD}",
                    tr("Ask before each session"),
                    tr("Someone at this PC allows or declines each session first."),
                    switch(MENU_ASK, shown.ask),
                ),
                row(
                    "\u{E716}",
                    tr("How someone of another account starts helping"),
                    tr("With this PC's help ID and the code it shows."),
                    Control::Choice {
                        value: mode(shown.help_mode).to_string(),
                        options: [
                            (MENU_HELP_BOTH, HelpMode::CodeAndAsk),
                            (MENU_HELP_CODE_ONLY, HelpMode::CodeOnly),
                            (MENU_HELP_ASK_ONLY, HelpMode::AskOnly),
                        ]
                        .into_iter()
                        .map(|(id, value)| (id, mode(value).to_string(), value == shown.help_mode))
                        .collect(),
                    },
                ),
                fixed,
                row(
                    "\u{E8B7}",
                    tr("Allow files while someone helps"),
                    tr("Files can go both ways while someone of another account helps."),
                    switch(MENU_HELP_FILES, shown.help_files),
                ),
                row(
                    "\u{E72E}",
                    tr("Settings password"),
                    tr("These settings, logging out and quitting ask for it."),
                    Control::Buttons(vec![(
                        MENU_PASSWORD,
                        if shown.password {
                            tr("Remove")
                        } else {
                            tr("Set...")
                        }
                        .to_string(),
                    )]),
                ),
            ]
        }
        Tab::Sessions => vec![
            row(
                "\u{E77F}",
                tr("Share the clipboard"),
                tr("Copy on one PC and paste on the other, both ways."),
                switch(MENU_CLIPBOARD, shown.clipboard),
            ),
            row(
                "\u{E8B7}",
                tr("Receive files"),
                tr("Files can be sent to this PC, and taken from it, in a session."),
                switch(MENU_FILES, shown.files),
            ),
            row(
                "\u{E756}",
                tr("Allow command lines"),
                tr("Your devices can open a command line on this PC."),
                switch(MENU_TERMINAL, shown.terminal),
            ),
            row(
                "\u{E72E}",
                tr("Lock this PC after a session"),
                tr("This PC locks as soon as a session ends."),
                switch(MENU_LOCK_AFTER, shown.lock_after),
            ),
        ],
        Tab::Keyboard => {
            let keys = |text: &str| -> Vec<String> {
                text.split('+')
                    .map(str::trim)
                    .filter(|part| !part.is_empty())
                    .map(|part| match part {
                        "Ctrl" | "Alt" | "Shift" => part.to_string(),
                        other => tr_key(other),
                    })
                    .collect()
            };
            vec![
                row(
                    "\u{E765}",
                    tr("Shortcuts of the session window"),
                    tr("Pressed in a session, these work on this PC and never go to the other PC."),
                    Control::Buttons(vec![(
                        MENU_KEYS_DEFAULT,
                        tr("Restore the default keys").to_string(),
                    )]),
                ),
                Row {
                    sub: true,
                    ..row(
                        "",
                        tr("Full screen or back to a window"),
                        "",
                        Control::Keys {
                            id: MENU_KEY_FULL_SCREEN,
                            keys: keys(&shown.keys.full_screen().text()),
                        },
                    )
                },
                Row {
                    sub: true,
                    ..row(
                        "",
                        tr("Game mouse on or off"),
                        "",
                        Control::Keys {
                            id: MENU_KEY_GAME_MOUSE,
                            keys: keys(&shown.keys.game_mouse().text()),
                        },
                    )
                },
                Row {
                    sub: true,
                    ..row(
                        "",
                        tr("Boss key: hide the session window and its sound at once"),
                        "",
                        Control::Keys {
                            id: MENU_KEY_BOSS,
                            keys: shown
                                .keys
                                .boss_key()
                                .map(|key| keys(&key.text()))
                                .unwrap_or_default(),
                        },
                    )
                },
            ]
        }
    }
}

/// A key's name as the person reads it (arrows as arrows).
fn tr_key(name: &str) -> String {
    match name {
        "Left" => "←".into(),
        "Right" => "→".into(),
        "Up" => "↑".into(),
        "Down" => "↓".into(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tab_has_cards_and_every_control_an_id() {
        let shown = Shown {
            version: "0.1.0".into(),
            ..Shown::default()
        };
        for tab in Tab::ALL {
            let rows = rows(tab, &shown);
            assert!(!rows.is_empty(), "{tab:?}");
            for row in rows {
                assert!(!row.title.is_empty());
                match row.control {
                    Control::Switch { id, .. } | Control::Keys { id, .. } => assert!(id > 0),
                    Control::Choice { options, .. } => {
                        assert!(options.iter().filter(|(_, _, chosen)| *chosen).count() == 1);
                    }
                    Control::Buttons(buttons) => assert!(!buttons.is_empty()),
                }
            }
        }
    }

    #[test]
    fn the_keys_show_as_their_parts() {
        let shown = Shown::default();
        let rows = rows(Tab::Keyboard, &shown);
        let full_screen = rows
            .iter()
            .find_map(|row| match &row.control {
                Control::Keys {
                    id: MENU_KEY_FULL_SCREEN,
                    keys,
                } => Some(keys.clone()),
                _ => None,
            })
            .unwrap();
        assert_eq!(full_screen, ["Ctrl", "Alt", "Enter"]);
    }
}
