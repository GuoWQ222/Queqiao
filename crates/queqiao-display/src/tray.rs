//! The tray menu, on every system (docs/LINUX.md): its items and what choosing one does: the settings it changes, the
//! settings password asked for first, settings locked while someone of another account controls the PC. Windows draws
//! it as a popup menu (presence.rs), Linux as a StatusNotifierItem's menu; the main window's settings go through it
//! too. Moved from presence.rs.

use queqiao_core::text::{tr, trf};

use crate::model::{AccountState, HelpMode, PasswordCheck, PresenceEvent, normalize_code};

pub(crate) const MENU_END: u16 = 200;
pub(crate) const MENU_ALLOW: u16 = 201;
pub(crate) const MENU_ASK: u16 = 202;
pub(crate) const MENU_QUIT: u16 = 203;
pub(crate) const MENU_CLIPBOARD: u16 = 204;
pub(crate) const MENU_FILES: u16 = 205;
pub(crate) const MENU_TERMINAL: u16 = 206;
pub(crate) const MENU_AUTOSTART: u16 = 207;
pub(crate) const MENU_HELP: u16 = 208;
pub(crate) const MENU_LOCK_AFTER: u16 = 209;
pub(crate) const MENU_LOGIN: u16 = 215;
pub(crate) const MENU_UPDATE_CHECK: u16 = 216;
pub(crate) const MENU_AUTO_UPDATE: u16 = 217;
pub(crate) const MENU_FIXED_CODE: u16 = 219;
pub(crate) const MENU_PASSWORD: u16 = 220;
pub(crate) const MENU_WOL_CHECK: u16 = 222;
pub(crate) const MENU_OPEN: u16 = 223;
pub(crate) const MENU_HELP_BOTH: u16 = 224;
pub(crate) const MENU_HELP_CODE_ONLY: u16 = 225;
pub(crate) const MENU_HELP_ASK_ONLY: u16 = 226;
pub(crate) const MENU_HELP_FILES: u16 = 227;
// Settings kept by this PC's Queqiao for this person alone (no event for the agent): the display language and the
// session window's keys.
pub(crate) const MENU_LANGUAGE_SYSTEM: u16 = 230;
pub(crate) const MENU_LANGUAGE_ZH: u16 = 231;
pub(crate) const MENU_LANGUAGE_EN: u16 = 232;
pub(crate) const MENU_KEY_FULL_SCREEN: u16 = 233;
pub(crate) const MENU_KEY_GAME_MOUSE: u16 = 234;
pub(crate) const MENU_KEY_BOSS: u16 = 235;
pub(crate) const MENU_KEYS_DEFAULT: u16 = 236;

/// What a local setting's choice did (see [`choose_local`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Local {
    /// Not one of them: the tray's model handles it.
    Other,
    /// Handled, nothing to draw again.
    Unchanged,
    /// The session keys changed (sessions opened from now on use them).
    Keys,
    /// The display language changed: the windows are made again in it.
    Language,
}

/// The display language and the session keys: chosen in Settings, kept in this person's Queqiao folder.
pub(crate) fn choose_local(item: u16, dialogs: &dyn Dialogs) -> Local {
    use queqiao_core::hotkeys::{Hotkey, Keys};
    use queqiao_core::text::{LanguageChoice, choose_language, saved_choice};
    let language = match item {
        MENU_LANGUAGE_SYSTEM => Some(LanguageChoice::System),
        MENU_LANGUAGE_ZH => Some(LanguageChoice::Chinese),
        MENU_LANGUAGE_EN => Some(LanguageChoice::English),
        _ => None,
    };
    if let Some(choice) = language {
        if choice == saved_choice() {
            return Local::Unchanged;
        }
        return match choose_language(choice) {
            Ok(()) => Local::Language,
            Err(error) => {
                dialogs.show_error(tr("Queqiao"), &error.to_string());
                Local::Unchanged
            }
        };
    }
    let mut keys = Keys::load();
    let (value, prompt, may_be_empty) = match item {
        MENU_KEYS_DEFAULT => {
            if keys == Keys::default() {
                return Local::Unchanged;
            }
            return match Keys::default().save() {
                Ok(()) => Local::Keys,
                Err(error) => {
                    dialogs.show_error(tr("Queqiao"), &error);
                    Local::Unchanged
                }
            };
        }
        MENU_KEY_FULL_SCREEN => (
            &mut keys.full_screen,
            tr("Full screen key (for example Ctrl+Alt+Enter):"),
            false,
        ),
        MENU_KEY_GAME_MOUSE => (
            &mut keys.game_mouse,
            tr("Game mouse key (for example Ctrl+Alt+M):"),
            false,
        ),
        MENU_KEY_BOSS => (
            &mut keys.boss_key,
            tr("Boss key, hides this window at once (for example Ctrl+Alt+H; empty: none):"),
            true,
        ),
        _ => return Local::Other,
    };
    let Some(typed) = dialogs.ask_text(tr("Queqiao"), prompt, value) else {
        return Local::Unchanged;
    };
    let typed = typed.trim().to_string();
    match Hotkey::parse(&typed) {
        Some(key) => *value = key.text(),
        None if typed.is_empty() && may_be_empty => value.clear(),
        None => {
            dialogs.show_error(
                tr("Queqiao"),
                &trf(
                    "Not a key Queqiao can use: {key}. A key is Ctrl and/or Alt (Shift may be added) with one letter, digit, F1 to F12 or Enter, Space, Home and the like.",
                    &[("key", &typed)],
                ),
            );
            return Local::Unchanged;
        }
    }
    match keys.save() {
        Ok(()) => Local::Keys,
        Err(error) => {
            dialogs.show_error(tr("Queqiao"), &error);
            Local::Unchanged
        }
    }
}

/// For tests: menu items to choose as a click would.
pub const TEST_MENU_TERMINAL: u16 = MENU_TERMINAL;
pub const TEST_MENU_LOGIN: u16 = MENU_LOGIN;
pub const TEST_MENU_UPDATE_CHECK: u16 = MENU_UPDATE_CHECK;

/// The shortest fixed help code.
pub const MIN_FIXED_CODE: usize = 8;

/// One line of the menu.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Entry {
    Item {
        id: u16,
        text: String,
        checked: bool,
        /// Not while the settings are locked.
        enabled: bool,
        /// The one a double click (Windows) or a click (Linux) on the icon does.
        default: bool,
    },
    /// A line that only says how things stand.
    Status(String),
    Separator,
}

/// The forms choosing an item may need (Windows' own or Linux's).
pub(crate) trait Dialogs {
    fn ask_secret(&self, title: &str, prompt: &str) -> Option<String>;
    fn ask_text(&self, title: &str, prompt: &str, initial: &str) -> Option<String>;
    fn show_error(&self, title: &str, text: &str);
}

/// What the menu shows and changes.
pub(crate) struct Tray {
    pub(crate) allow: bool,
    pub(crate) ask: bool,
    pub(crate) clipboard: bool,
    pub(crate) files: bool,
    pub(crate) terminal: bool,
    pub(crate) autostart: bool,
    pub(crate) lock_after: bool,
    /// While someone of another account controls the PC, the settings (and Quit) cannot be changed from here: they
    /// hold the mouse too.
    pub(crate) settings_locked: bool,
    /// Checks the settings password, when one is set: settings and Quit ask for it.
    pub(crate) password_check: Option<PasswordCheck>,
    pub(crate) viewer: Option<String>,
    pub(crate) account: AccountState,
    pub(crate) auto_update: bool,
    pub(crate) fixed_code: bool,
    pub(crate) help_mode: HelpMode,
    pub(crate) help_files: bool,
}

impl Tray {
    pub(crate) fn new(
        allow: bool,
        ask: bool,
        clipboard: bool,
        files: bool,
        terminal: bool,
        autostart: bool,
        lock_after: bool,
    ) -> Tray {
        Tray {
            allow,
            ask,
            clipboard,
            files,
            terminal,
            autostart,
            lock_after,
            settings_locked: false,
            password_check: None,
            viewer: None,
            account: AccountState::Online,
            auto_update: true,
            fixed_code: false,
            help_mode: HelpMode::default(),
            help_files: false,
        }
    }

    /// The first line: how things stand.
    pub(crate) fn status(&self) -> String {
        match (&self.viewer, self.account) {
            (Some(viewer), _) => trf(
                "Queqiao: {viewer} is controlling this PC",
                &[("viewer", viewer)],
            ),
            (None, AccountState::Online) => tr("Queqiao: ready").into(),
            (None, AccountState::Offline) => {
                tr("Queqiao: cannot reach the server, trying again").into()
            }
            (None, AccountState::LoggedOut) => tr("Queqiao: not logged in").into(),
        }
    }

    /// The menu's lines, in order.
    pub(crate) fn entries(&self) -> Vec<Entry> {
        let item = |id: u16, text: &str, checked: bool, setting: bool| Entry::Item {
            id,
            text: text.to_string(),
            checked,
            enabled: !(setting && self.settings_locked),
            default: false,
        };
        let mut entries = vec![
            Entry::Item {
                id: MENU_OPEN,
                text: tr("Open Queqiao").into(),
                checked: false,
                enabled: true,
                default: true,
            },
            Entry::Status(self.status()),
            Entry::Separator,
        ];
        if self.viewer.is_some() {
            entries.push(item(MENU_END, tr("End the session"), false, false));
        }
        if self.account != AccountState::Online {
            entries.push(item(MENU_LOGIN, tr("Log in..."), false, true));
        }
        entries.push(item(MENU_HELP, tr("Let someone help me..."), false, false));
        entries.push(item(
            MENU_FIXED_CODE,
            if self.fixed_code {
                tr("Remove the fixed help code")
            } else {
                tr("A fixed help code for this PC...")
            },
            false,
            true,
        ));
        entries.push(item(
            MENU_ALLOW,
            tr("Let my account's devices control this PC"),
            self.allow,
            true,
        ));
        entries.push(item(
            MENU_ASK,
            tr("Ask me before each session"),
            self.ask,
            true,
        ));
        entries.push(item(
            MENU_CLIPBOARD,
            tr("Share the clipboard"),
            self.clipboard,
            true,
        ));
        entries.push(item(MENU_FILES, tr("Receive files"), self.files, true));
        entries.push(item(
            MENU_TERMINAL,
            tr("Allow remote command lines"),
            self.terminal,
            true,
        ));
        entries.push(item(MENU_AUTOSTART, autostart_text(), self.autostart, true));
        entries.push(item(
            MENU_LOCK_AFTER,
            tr("Lock this PC when a session ends"),
            self.lock_after,
            true,
        ));
        entries.push(item(
            MENU_PASSWORD,
            if self.password_check.is_some() {
                tr("Remove the settings password...")
            } else {
                tr("Lock these settings with a password...")
            },
            false,
            true,
        ));
        entries.push(Entry::Separator);
        entries.push(item(
            MENU_AUTO_UPDATE,
            tr("Install updates automatically"),
            self.auto_update,
            true,
        ));
        entries.push(item(
            MENU_UPDATE_CHECK,
            tr("Check for updates..."),
            false,
            false,
        ));
        entries.push(item(
            MENU_WOL_CHECK,
            tr("Wake-on-LAN check..."),
            false,
            false,
        ));
        entries.push(Entry::Separator);
        entries.push(item(MENU_QUIT, tr("Quit Queqiao on this PC"), false, false));
        entries
    }

    /// What choosing menu item `item` does (also the path tests take). Opening the main window (MENU_OPEN) is the
    /// caller's: None here.
    pub(crate) fn choose(&mut self, item: u16, dialogs: &dyn Dialogs) -> Option<PresenceEvent> {
        let setting = matches!(
            item,
            MENU_ALLOW
                | MENU_ASK
                | MENU_CLIPBOARD
                | MENU_FILES
                | MENU_TERMINAL
                | MENU_AUTOSTART
                | MENU_LOCK_AFTER
                | MENU_AUTO_UPDATE
                | MENU_QUIT
                | MENU_HELP_BOTH
                | MENU_HELP_CODE_ONLY
                | MENU_HELP_ASK_ONLY
                | MENU_HELP_FILES
        );
        let protected =
            setting || item == MENU_PASSWORD || item == MENU_LOGIN || item == MENU_FIXED_CODE;
        if self.settings_locked && protected {
            return Some(PresenceEvent::SettingsLocked);
        }
        if protected && let Some(check) = &self.password_check {
            let typed = dialogs.ask_secret(tr("Queqiao"), tr("The settings password:"));
            if !typed.is_some_and(|typed| check(&typed)) {
                return Some(PresenceEvent::SettingsLocked);
            }
        }
        if item == MENU_FIXED_CODE {
            if self.fixed_code {
                return Some(PresenceEvent::SetFixedHelpCode(None));
            }
            let typed = dialogs.ask_text(
                tr("Queqiao"),
                tr("A fixed help code (8 or more letters or digits):"),
                "",
            )?;
            let code = normalize_code(&typed);
            if code.len() < MIN_FIXED_CODE {
                dialogs.show_error(
                    tr("Queqiao"),
                    tr("A fixed help code needs 8 or more letters or digits."),
                );
                return None;
            }
            return Some(PresenceEvent::SetFixedHelpCode(Some(code)));
        }
        if item == MENU_PASSWORD {
            if self.password_check.is_some() {
                return Some(PresenceEvent::SetPassword(None));
            }
            let first = dialogs.ask_secret(tr("Queqiao"), tr("A password for these settings:"))?;
            let again = dialogs.ask_secret(tr("Queqiao"), tr("The same password again:"))?;
            return (!first.is_empty() && first == again)
                .then_some(PresenceEvent::SetPassword(Some(first)));
        }
        match item {
            MENU_END => Some(PresenceEvent::EndSession),
            MENU_LOGIN => Some(PresenceEvent::LogIn),
            MENU_HELP => Some(PresenceEvent::OpenHelp),
            MENU_ALLOW => {
                self.allow = !self.allow;
                Some(PresenceEvent::SetAllow(self.allow))
            }
            MENU_ASK => {
                self.ask = !self.ask;
                Some(PresenceEvent::SetAsk(self.ask))
            }
            MENU_CLIPBOARD => {
                self.clipboard = !self.clipboard;
                Some(PresenceEvent::SetClipboard(self.clipboard))
            }
            MENU_FILES => {
                self.files = !self.files;
                Some(PresenceEvent::SetFiles(self.files))
            }
            MENU_TERMINAL => {
                self.terminal = !self.terminal;
                Some(PresenceEvent::SetTerminal(self.terminal))
            }
            MENU_AUTOSTART => {
                self.autostart = !self.autostart;
                Some(PresenceEvent::SetAutostart(self.autostart))
            }
            MENU_LOCK_AFTER => {
                self.lock_after = !self.lock_after;
                Some(PresenceEvent::SetLockAfter(self.lock_after))
            }
            MENU_AUTO_UPDATE => {
                self.auto_update = !self.auto_update;
                Some(PresenceEvent::SetAutoUpdate(self.auto_update))
            }
            MENU_UPDATE_CHECK => Some(PresenceEvent::CheckUpdates),
            MENU_WOL_CHECK => Some(PresenceEvent::CheckWakeOnLan),
            MENU_QUIT => Some(PresenceEvent::Quit),
            MENU_HELP_BOTH | MENU_HELP_CODE_ONLY | MENU_HELP_ASK_ONLY => {
                self.help_mode = match item {
                    MENU_HELP_CODE_ONLY => HelpMode::CodeOnly,
                    MENU_HELP_ASK_ONLY => HelpMode::AskOnly,
                    _ => HelpMode::CodeAndAsk,
                };
                Some(PresenceEvent::SetHelpMode(self.help_mode))
            }
            MENU_HELP_FILES => {
                self.help_files = !self.help_files;
                Some(PresenceEvent::SetHelpFiles(self.help_files))
            }
            _ => None,
        }
    }
}

/// "Start with Windows", or on Linux with the desktop session.
pub(crate) fn autostart_text() -> &'static str {
    if cfg!(windows) {
        tr("Start with Windows")
    } else {
        tr("Start when I sign in")
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    /// Answers the forms from a list, and notes the errors shown.
    #[derive(Default)]
    struct Answers {
        typed: RefCell<Vec<Option<String>>>,
        errors: RefCell<Vec<String>>,
    }

    impl Dialogs for Answers {
        fn ask_secret(&self, _title: &str, _prompt: &str) -> Option<String> {
            self.typed.borrow_mut().remove(0)
        }
        fn ask_text(&self, _title: &str, _prompt: &str, _initial: &str) -> Option<String> {
            self.typed.borrow_mut().remove(0)
        }
        fn show_error(&self, _title: &str, text: &str) {
            self.errors.borrow_mut().push(text.to_string());
        }
    }

    fn tray() -> Tray {
        Tray::new(true, false, true, true, true, false, false)
    }

    #[test]
    fn settings_toggle_and_lock() {
        let mut tray = tray();
        let none = Answers::default();
        assert_eq!(
            tray.choose(MENU_ASK, &none),
            Some(PresenceEvent::SetAsk(true))
        );
        assert!(tray.ask);
        tray.settings_locked = true;
        assert_eq!(
            tray.choose(MENU_ALLOW, &none),
            Some(PresenceEvent::SettingsLocked)
        );
        assert!(tray.allow, "unchanged while locked");
        // Not a setting: still there while locked.
        assert_eq!(tray.choose(MENU_HELP, &none), Some(PresenceEvent::OpenHelp));
        let disabled = tray
            .entries()
            .iter()
            .filter(|e| matches!(e, Entry::Item { enabled: false, .. }))
            .count();
        assert!(disabled >= 9, "{disabled} settings greyed");
    }

    #[test]
    fn the_password_guards_the_settings() {
        let mut tray = tray();
        tray.password_check = Some(Box::new(|typed: &str| typed == "secret"));
        let wrong = Answers {
            typed: RefCell::new(vec![Some("guess".into())]),
            ..Default::default()
        };
        assert_eq!(
            tray.choose(MENU_QUIT, &wrong),
            Some(PresenceEvent::SettingsLocked)
        );
        let right = Answers {
            typed: RefCell::new(vec![Some("secret".into())]),
            ..Default::default()
        };
        assert_eq!(tray.choose(MENU_QUIT, &right), Some(PresenceEvent::Quit));
        // Removing it needs it too.
        let right = Answers {
            typed: RefCell::new(vec![Some("secret".into())]),
            ..Default::default()
        };
        assert_eq!(
            tray.choose(MENU_PASSWORD, &right),
            Some(PresenceEvent::SetPassword(None))
        );
    }

    #[test]
    fn a_fixed_code_is_long_enough_and_normalized() {
        let mut tray = tray();
        let short = Answers {
            typed: RefCell::new(vec![Some("ab-12".into())]),
            ..Default::default()
        };
        assert_eq!(tray.choose(MENU_FIXED_CODE, &short), None);
        assert_eq!(short.errors.borrow().len(), 1);
        let good = Answers {
            typed: RefCell::new(vec![Some("abcd-1234 x".into())]),
            ..Default::default()
        };
        assert_eq!(
            tray.choose(MENU_FIXED_CODE, &good),
            Some(PresenceEvent::SetFixedHelpCode(Some("ABCD1234X".into())))
        );
        // A new password must be typed the same twice.
        let differ = Answers {
            typed: RefCell::new(vec![Some("one".into()), Some("two".into())]),
            ..Default::default()
        };
        assert_eq!(tray.choose(MENU_PASSWORD, &differ), None);
    }
}
