//! The agent's command line, on every system (docs/LINUX.md step 3); moved from win.rs.

use std::path::PathBuf;

#[derive(Default)]
pub(crate) struct Options {
    /// Tests: this PC counts as locked for the whole session (nothing is really locked).
    pub(crate) test_locked: bool,
    /// Tests: start a program at once and report its integrity level.
    pub(crate) test_start_program: bool,
    /// Tests: the Windows password the lock gate takes (instead of Windows' check of the real one).
    pub(crate) test_password: Option<String>,
    /// Tests: the help mode for this run, never saved (the person's settings file is shared with the tests).
    pub(crate) help_mode: Option<queqiao_display::HelpMode>,
    /// Show the main window (the Start menu's shortcut): this one, or the one of the Queqiao already running.
    pub(crate) show: bool,
    /// Tests: pictures of the main window's pages (into --ui-pictures) once logged in.
    pub(crate) test_main_window: bool,
    /// A device whose new key the person confirmed (its old pin is dropped).
    pub(crate) trust_new_key: Option<String>,
    /// Ask the person at the PC before each session in this run (whatever the setting says).
    pub(crate) ask: bool,
    /// For tests: answer the consent prompt (true = Allow) as a click would, after 1 s.
    pub(crate) answer: Option<bool>,
    /// For tests: press the banner's End button this many seconds into a session.
    pub(crate) end_after: Option<u64>,
    /// For tests: save pictures of the banner and the consent prompt in this folder.
    pub(crate) ui_pictures: Option<PathBuf>,
    /// For tests: send a clipboard text once the session's data channel is open.
    pub(crate) clipboard_test: bool,
    /// Files to send to the viewer once the session's files channel is open (tests).
    pub(crate) send_files: Vec<PathBuf>,
    /// Report the viewer's commands (lock, restart) instead of carrying them out (tests).
    pub(crate) dry_run_commands: bool,
    /// Capture the sound of this render device (part of its name) instead of the default one.
    pub(crate) sound_device: Option<String>,
    /// Play a test tone into the sound device during the session (tests: use a virtual device).
    pub(crate) sound_test: bool,
    /// Listen to the virtual microphone for the viewer's test tone once its microphone comes (tests).
    pub(crate) mic_check: bool,
    /// The privacy screen as a small square of a test colour, checked against what capture sees (tests).
    pub(crate) privacy_test: bool,
    /// Take help requests from the start with this code (tests; people use the tray menu).
    pub(crate) help_code: Option<String>,
    /// Write the help ID into this file once the server gave it (tests).
    pub(crate) help_id_file: Option<PathBuf>,
    /// A fixed help code for this run (tests; people set it in the tray).
    pub(crate) fixed_help_code: Option<String>,
    /// The Wake-on-LAN set-up check into the report, then end.
    pub(crate) wol_check: bool,
    /// A throwaway device with this name instead of this PC's own (tests with two accounts on one PC).
    pub(crate) test_device: Option<String>,
    /// Lock this PC when a session ends, for this run only (tests: with --dry-run-commands it is only reported).
    pub(crate) lock_after_session: bool,
    /// In a helper's session, choose a tray setting as a click would (tests: it must be refused).
    pub(crate) test_press_setting: bool,
    /// Start not logged in and log in through the tray's "Log in..." and its form, filled with --server, --pin,
    /// --user and --password (tests).
    pub(crate) test_tray_login: bool,
    pub(crate) connect: Option<String>,
    pub(crate) listen: Option<String>,
    pub(crate) server: Option<String>,
    pub(crate) pin: Option<String>,
    pub(crate) user: Option<String>,
    pub(crate) password: Option<String>,
    pub(crate) sessions: Option<u32>,
    pub(crate) seconds: Option<u64>,
    pub(crate) fps: Option<u32>,
    pub(crate) report: Option<PathBuf>,
    pub(crate) verify_input: bool,
    /// The channels the Queqiao service hands over when it starts the agent (`<read>,<write>` handles).
    pub(crate) service_channel: Option<String>,
    /// The agent for the sign-in screen (the service starts it while nobody is signed in): who it is comes from the
    /// service, in memory; it shows nothing itself and keeps nothing.
    pub(crate) logon: bool,
    pub(crate) desktop_channel: Option<String>,
}

/// The command line. A shipping build reads only what the Queqiao service passes (`--service-channel`,
/// `--desktop-channel`, `--logon`) and `--force`, which only chooses among the media paths. Every other option is
/// for the tests and exists only in builds with the `test-switches` feature (`cargo build-test`): found by the
/// security review of 2026-11-22, a program running as the person could otherwise start the signed agent as a
/// remote-control tool of its own (`--connect` without any account, `--answer allow`, another account's login).
pub(crate) fn parse(mut args: impl Iterator<Item = String>) -> Option<Options> {
    let mut options = Options::default();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--service-channel" => options.service_channel = Some(args.next()?),
            "--logon" => options.logon = true,
            "--show" => options.show = true,
            "--desktop-channel" => options.desktop_channel = Some(args.next()?),
            "--force" => queqiao_core::set_forced(&args.next()?),
            _ => {
                #[cfg(feature = "test-switches")]
                if test_switch(&mut options, &arg, &mut args)? {
                    continue;
                }
                return None;
            }
        }
    }
    Some(options)
}

/// The options for the tests (builds with the `test-switches` feature only): whether `arg` is one.
#[cfg(feature = "test-switches")]
pub(crate) fn test_switch(
    options: &mut Options,
    arg: &str,
    args: &mut impl Iterator<Item = String>,
) -> Option<bool> {
    use queqiao_display::normalize_code;
    match arg {
        "--connect" => options.connect = args.next(),
        "--listen" => options.listen = args.next(),
        "--server" => options.server = args.next(),
        "--pin" => options.pin = args.next(),
        "--user" => options.user = args.next(),
        "--password" => options.password = args.next(),
        "--sessions" => options.sessions = args.next().and_then(|v| v.parse().ok()),
        "--seconds" => options.seconds = args.next().and_then(|v| v.parse().ok()),
        "--fps" => options.fps = args.next().and_then(|v| v.parse().ok()),
        "--report" => options.report = args.next().map(PathBuf::from),
        "--verify-input" => options.verify_input = true,
        "--trust-new-key" => options.trust_new_key = args.next(),
        "--ask" => options.ask = true,
        "--answer" => options.answer = Some(args.next()? == "allow"),
        "--end-after" => options.end_after = args.next().and_then(|v| v.parse().ok()),
        "--ui-pictures" => options.ui_pictures = args.next().map(PathBuf::from),
        "--clipboard-test" => options.clipboard_test = true,
        "--send-file" => options.send_files.push(PathBuf::from(args.next()?)),
        "--dry-run-commands" => options.dry_run_commands = true,
        "--sound-device" => options.sound_device = Some(args.next()?),
        "--sound-test" => options.sound_test = true,
        "--mic-check" => options.mic_check = true,
        "--privacy-test" => options.privacy_test = true,
        "--help-code" => options.help_code = Some(normalize_code(&args.next()?)),
        "--help-id-file" => options.help_id_file = Some(PathBuf::from(args.next()?)),
        "--fixed-help-code" => options.fixed_help_code = Some(normalize_code(&args.next()?)),
        "--wol-check" => options.wol_check = true,
        "--test-device" => options.test_device = Some(args.next()?),
        "--lock-after-session" => options.lock_after_session = true,
        "--test-press-setting" => options.test_press_setting = true,
        "--test-tray-login" => options.test_tray_login = true,
        "--test-main-window" => options.test_main_window = true,
        "--test-locked" => options.test_locked = true,
        "--test-start-program" => options.test_start_program = true,
        "--test-password" => options.test_password = Some(args.next()?),
        "--help-mode" => {
            options.help_mode = Some(match args.next()?.as_str() {
                "code" => queqiao_display::HelpMode::CodeOnly,
                "ask" => queqiao_display::HelpMode::AskOnly,
                "both" => queqiao_display::HelpMode::CodeAndAsk,
                _ => return None,
            })
        }
        _ => return Some(false),
    }
    Some(true)
}

#[cfg(test)]
mod option_tests {
    use super::*;
    use crate::session::clean_name;

    fn args(list: &[&str]) -> std::vec::IntoIter<String> {
        list.iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>()
            .into_iter()
    }

    #[test]
    fn the_service_options_are_read() {
        let options = parse(args(&[
            "--logon",
            "--service-channel",
            "1:2",
            "--desktop-channel",
            "3:4",
        ]))
        .unwrap();
        assert!(options.logon);
        assert_eq!(options.service_channel.as_deref(), Some("1:2"));
        assert_eq!(options.desktop_channel.as_deref(), Some("3:4"));
        assert!(parse(args(&["--service-channel"])).is_none());
    }

    #[cfg(not(feature = "test-switches"))]
    #[test]
    fn a_shipping_build_refuses_the_test_options() {
        for option in [
            "--connect",
            "--listen",
            "--server",
            "--pin",
            "--user",
            "--password",
            "--answer",
            "--report",
            "--send-file",
            "--fixed-help-code",
            "--help-code",
            "--trust-new-key",
            "--ui-pictures",
            "--sessions",
        ] {
            assert!(parse(args(&[option, "x"])).is_none(), "{option}");
        }
    }

    #[cfg(feature = "test-switches")]
    #[test]
    fn a_test_build_reads_the_test_options() {
        let options = parse(args(&["--listen", "127.0.0.1:1", "--answer", "allow"])).unwrap();
        assert_eq!(options.listen.as_deref(), Some("127.0.0.1:1"));
        assert_eq!(options.answer, Some(true));
    }

    #[test]
    fn names_in_prompts_are_cleaned() {
        assert_eq!(
            clean_name("Anna's laptop\r\n\r\n(account x)"),
            "Anna's laptop(account x)"
        );
        assert_eq!(clean_name("evil\u{202E}gnp.exe"), "evilgnp.exe");
        assert_eq!(clean_name(&"a".repeat(60)).chars().count(), 40);
    }

    #[cfg(windows)]
    #[test]
    fn a_test_process_is_not_local_service() {
        assert!(!queqiao_core::platform::runs_as_local_service());
    }
}
