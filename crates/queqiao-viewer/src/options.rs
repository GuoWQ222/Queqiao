//! The viewer's command line, on every system (docs/LINUX.md step 3); moved from win.rs.

use std::path::PathBuf;

use queqiao_core::protocol::{Command, Quality};

/// What the main window asks the session to do as soon as it is up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Then {
    /// The file picker: the files chosen go to that PC.
    Files,
    /// A command line on that PC.
    Terminal,
}

#[derive(Default, Clone)]
pub(crate) struct Options {
    /// A device whose new key the person confirmed (its old pin is dropped).
    pub(crate) trust_new_key: Option<String>,
    pub(crate) connect: Option<String>,
    pub(crate) listen: Option<String>,
    pub(crate) server: Option<String>,
    pub(crate) pin: Option<String>,
    pub(crate) user: Option<String>,
    pub(crate) password: Option<String>,
    pub(crate) device: Option<String>,
    /// With --open: what to do as soon as the session is up (the main window's "Send files" and "Command line").
    pub(crate) then: Option<Then>,
    /// With --device: a second device of the account at the same time (tests: several sessions at once).
    pub(crate) also_device: Option<String>,
    pub(crate) list: bool,
    pub(crate) logout: bool,
    pub(crate) ui_test: Option<String>,
    pub(crate) seconds: Option<u64>,
    pub(crate) width: Option<i32>,
    pub(crate) report: Option<PathBuf>,
    pub(crate) input_test: bool,
    /// Tests: the card of figures on and the bar kept out from the start.
    pub(crate) figures: bool,
    /// Tests: the session picture (--ui-pictures) this many seconds after the first picture (default 3).
    pub(crate) picture_at: Option<u64>,
    /// Tests: a picture of the session window with its bar and card (viewer_session.bmp) into this folder.
    pub(crate) ui_pictures: Option<PathBuf>,
    /// For tests: send a clipboard text once the data channel is open.
    pub(crate) clipboard_test: bool,
    /// Files to send once the session's files channel is open (tests; people drop files on the window).
    pub(crate) send_files: Vec<PathBuf>,
    /// Commands to send once the session's control channel is open (tests; people use the window menu).
    pub(crate) commands: Vec<Command>,
    /// Play the other PC's sound on this render device (part of its name) instead of the default one.
    pub(crate) sound_device: Option<String>,
    /// Check the received sound for the agent's test tone and report it.
    pub(crate) sound_test: bool,
    /// Start with the other PC's sound off.
    pub(crate) mute: bool,
    /// Start in full screen.
    pub(crate) full_screen: bool,
    /// Darken that PC's screen from the start.
    pub(crate) privacy: bool,
    /// Open a command line on that PC and check it (its window closes by itself).
    pub(crate) terminal_test: bool,
    /// After the input test, relative moves (game mouse) that the agent checks.
    pub(crate) game_mouse_test: bool,
    /// Mute that PC's speakers from the start.
    pub(crate) mute_speakers: bool,
    /// Cancel a file this side sends once its first piece went out (tests).
    pub(crate) cancel_test: bool,
    /// Tests of the sign-in screens through the service: Ctrl+Alt+Del 3 s after the data channel opens, Escape
    /// (which closes that screen) 2 s later; the pictures shown meanwhile and after are reported.
    pub(crate) secure_test: bool,
    /// Send this file as a clipboard copy (tests: the other side must put it on its clipboard).
    pub(crate) clipboard_files_test: Option<PathBuf>,
    /// The picture's short side at most this (720, 1080) from the start; 0 or none: the screen's own.
    pub(crate) resolution: Option<u16>,
    /// Show that PC's screen n (from 0) from the start.
    pub(crate) screen: Option<u8>,
    /// A bitrate ceiling (kbit/s) to send at the start (tests; people use "Custom bitrate...").
    pub(crate) max_bitrate: Option<u32>,
    /// Picture settings to send at the start: frames per second at most (0: the screen's) and quality.
    pub(crate) picture: Option<(u8, Quality)>,
    /// Help someone of another account: their help ID and code (tests; people use "Help someone...").
    pub(crate) help_id: Option<String>,
    pub(crate) help_code: Option<String>,
    /// A help invitation link (queqiao://help/<help ID>/<code>): the help form, filled in from it.
    pub(crate) invite: Option<String>,
    /// --help-connect: the help form, filled in from the link, connects at once (the main window's Help page).
    pub(crate) help_go: bool,
    /// Tests: take this device (name or id) out of the account, then leave.
    pub(crate) remove_device: Option<String>,
    /// Tests: the passwords given (in turn) when the PC asks for its Windows password.
    pub(crate) test_passwords: Vec<String>,
    /// Press Connect in the help form once it is drawn (tests).
    pub(crate) test_submit_help: bool,
    /// With --user and --password: a new account (on a server that takes them) instead of a login.
    pub(crate) signup: bool,
    /// A throwaway device with this name instead of this PC's own (tests); its login is not kept.
    pub(crate) test_device: Option<String>,
    /// The picture's codec to ask for (as the Picture codec menu does): auto, h264, av1 or h265.
    pub(crate) codec: Option<queqiao_core::protocol::CodecPreference>,
    /// H.265 in 4:4:4 colour, and in 10 bits (as the menu's toggles).
    pub(crate) chroma444: bool,
    pub(crate) ten_bit: bool,
    /// Send a test tone as this PC's microphone from the start (tests).
    pub(crate) mic_test: bool,
    /// The session window's opacity in percent (as the Opacity menu sets it).
    pub(crate) opacity: Option<u8>,
    /// Boss key test: Ctrl+Alt+F11 for this run, pressed (as Windows would report it) to hide and show the window.
    pub(crate) boss_key_test: bool,
}

impl Options {
    pub(crate) fn direct(&self) -> bool {
        self.connect.is_some() || self.listen.is_some()
    }

    /// Started by a person (no test or script options): errors go into a message box.
    pub(crate) fn interactive(&self) -> bool {
        !self.direct()
            && self.device.is_none()
            && !self.list
            && !self.logout
            && self.ui_test.is_none()
            && self.report.is_none()
    }
}

/// The command line. A shipping build reads only `--force`, which only chooses among the media paths (the device
/// list needs no options, a queqiao: link goes through `invitation_options`). Every other option is for the tests
/// and exists only in builds with the `test-switches` feature (`cargo build-test`).
pub(crate) fn parse(mut args: impl Iterator<Item = String>) -> Option<Options> {
    // Only the test options change `options`.
    #[cfg_attr(not(feature = "test-switches"), allow(unused_mut))]
    let mut options = Options::default();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--force" => queqiao_core::set_forced(&args.next()?),
            // The main window (the agent's): a device of the account, or helping someone with their help ID and
            // code (as a link). Each does what a click there does.
            "--open" => options.device = Some(args.next()?),
            "--then" => {
                options.then = Some(match args.next()?.as_str() {
                    "files" => Then::Files,
                    "terminal" => Then::Terminal,
                    _ => return None,
                })
            }
            "--help-connect" => {
                options.invite = Some(args.next()?);
                options.help_go = true;
            }
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
        "--device" => options.device = args.next(),
        "--also-device" => options.also_device = args.next(),
        "--list" => options.list = true,
        "--logout" => options.logout = true,
        "--ui-test" => options.ui_test = args.next(),
        "--seconds" => options.seconds = args.next().and_then(|v| v.parse().ok()),
        "--width" => options.width = args.next().and_then(|v| v.parse().ok()),
        "--report" => options.report = args.next().map(PathBuf::from),
        "--input-test" => options.input_test = true,
        "--figures" => options.figures = true,
        "--picture-at" => options.picture_at = Some(args.next()?.parse().ok()?),
        "--ui-pictures" => options.ui_pictures = Some(PathBuf::from(args.next()?)),
        "--clipboard-test" => options.clipboard_test = true,
        "--send-file" => options.send_files.push(PathBuf::from(args.next()?)),
        "--sound-device" => options.sound_device = Some(args.next()?),
        "--sound-test" => options.sound_test = true,
        "--mic-test" => options.mic_test = true,
        "--mute" => options.mute = true,
        "--codec" => {
            use queqiao_core::protocol::CodecPreference;
            options.codec = Some(match args.next()?.as_str() {
                "h264" => CodecPreference::H264,
                "av1" => CodecPreference::Av1,
                "h265" => CodecPreference::H265,
                _ => CodecPreference::Automatic,
            });
        }
        "--chroma444" => options.chroma444 = true,
        "--ten-bit" => options.ten_bit = true,
        "--full-screen" => options.full_screen = true,
        "--privacy" => options.privacy = true,
        "--terminal-test" => options.terminal_test = true,
        "--game-mouse-test" => options.game_mouse_test = true,
        "--mute-speakers" => options.mute_speakers = true,
        "--cancel-test" => options.cancel_test = true,
        "--secure-test" => options.secure_test = true,
        "--clipboard-files-test" => {
            options.clipboard_files_test = Some(PathBuf::from(args.next()?))
        }
        "--resolution" => options.resolution = Some(args.next()?.parse().ok()?),
        "--screen" => options.screen = Some(args.next()?.parse().ok()?),
        "--max-bitrate" => options.max_bitrate = Some(args.next()?.parse().ok()?),
        "--help-id" => options.help_id = Some(args.next()?),
        "--help-code" => options.help_code = Some(normalize_code(&args.next()?)),
        "--invite" => options.invite = Some(args.next()?),
        "--test-submit-help" => options.test_submit_help = true,
        "--signup" => options.signup = true,
        "--test-device" => options.test_device = Some(args.next()?),
        "--opacity" => options.opacity = args.next().and_then(|v| v.parse().ok()),
        "--boss-key-test" => options.boss_key_test = true,
        "--picture" => {
            let value = args.next()?;
            let (fps, quality) = value.split_once(',')?;
            let quality = match quality {
                "smooth" => Quality::Smooth,
                "balanced" => Quality::Balanced,
                "sharp" => Quality::Sharp,
                _ => return None,
            };
            options.picture = Some((fps.parse().ok()?, quality));
        }
        "--command" => options.commands.push(match args.next()?.as_str() {
            "lock" => Command::Lock,
            "restart" => Command::Restart,
            "ctrl-alt-del" => Command::CtrlAltDel,
            "restart-queqiao" => Command::RestartQueqiao,
            _ => return None,
        }),
        "--trust-new-key" => options.trust_new_key = args.next(),
        "--remove-device" => options.remove_device = Some(args.next()?),
        "--test-password" => {
            options.test_passwords = args.next()?.split(',').map(str::to_string).collect()
        }
        _ => return Some(false),
    }
    Some(true)
}

/// A queqiao: link: Windows starts the viewer with `--invite "<link>"` (the handler the installer registers). Only
/// the one value after `--invite` is read, nothing else: a link with a `"` in it cannot add options of its own (the
/// security review of 2026-11-22 found links that could otherwise log in to another server, send files or write
/// files). Anything else is kept as an invalid link, so the person is told it is not a Queqiao help link.
pub(crate) fn invitation_options(rest: &[String]) -> Options {
    let link = match rest {
        [link] => link.clone(),
        _ => rest.join(" "),
    };
    Options {
        invite: Some(link),
        ..Options::default()
    }
}

#[cfg(test)]
mod option_tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_link_cannot_add_options() {
        // As Windows passes a link with a quote in it: "%1" ends early and the rest become options.
        let injected = args(&[
            "queqiao://help/123456789/ABCDEFGH\"",
            "--connect",
            "198.51.100.7:9",
            "--send-file",
            "C:\\Users",
            "--report",
            "x.cmd",
        ]);
        let options = invitation_options(&injected);
        assert!(
            options.connect.is_none() && options.report.is_none() && options.send_files.is_empty()
        );
        // The text after the quote is no option; as with a pasted invitation, only the help ID and code are read
        // from it, to fill in the help form.
        let link = options.invite.unwrap();
        assert_eq!(
            queqiao_core::invitation::parse(&link),
            Some(("123456789".to_string(), "ABCDEFGH".to_string()))
        );
        // A plain link is taken as it is.
        let plain = invitation_options(&args(&["queqiao://help/123456789/ABCDEFGH"]));
        assert_eq!(
            plain.invite.as_deref(),
            Some("queqiao://help/123456789/ABCDEFGH")
        );
        assert!(
            invitation_options(&[])
                .invite
                .is_some_and(|link| link.is_empty())
        );
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
            "--report",
            "--send-file",
            "--invite",
            "--test-submit-help",
            "--trust-new-key",
            "--device",
            "--command",
            "--signup",
        ] {
            assert!(
                parse(args(&[option, "x"]).into_iter()).is_none(),
                "{option}"
            );
        }
        assert!(parse(std::iter::empty()).is_some());
    }

    #[cfg(feature = "test-switches")]
    #[test]
    fn a_test_build_reads_the_test_options() {
        let options =
            parse(args(&["--connect", "127.0.0.1:1", "--report", "r.txt"]).into_iter()).unwrap();
        assert_eq!(options.connect.as_deref(), Some("127.0.0.1:1"));
        assert!(options.report.is_some());
    }
}
