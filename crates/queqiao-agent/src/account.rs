//! The agent with an account (M3), on every system (docs/LINUX.md step 5): logged in to queqiao-server, online, serving
//! the viewers of the account one after another and the help requests of others; the tray's and the main window's
//! choices, updates, Wake-on-LAN. What differs between the systems comes through `platform` (win.rs, linux.rs) and
//! the modules of the same name (autostart, power, update, wake, wol_check).

use std::time::{Duration, Instant};

use queqiao_account::{
    Account, AccountEvent, ClientMessage, DeviceIdentity, Role as AccountRole, SavedLogin,
};
use queqiao_core::text::{tr, trf};
use queqiao_display::{AccountState, LoginAction, LoginFields, LoginForm, PresenceEvent};
use queqiao_transport::{Signaling, check_hello, hello_body};

use crate::options::*;
use crate::platform;
use crate::service_link::Links;
use crate::session::*;
use crate::{autostart, power, ui, update, wake, wol_check};
/// How long the person at the PC has to allow a session (the viewer waits 30 s for the answer).
const CONSENT_SECONDS: u32 = 25;

/// Logged in to queqiao-server: waits for viewers of the account and serves them one after another.
/// When the server connection drops, it reconnects with the saved login (waiting 1 s, 2 s, ... 30 s).
pub(crate) fn run_account(
    options: &Options,
    end: Option<Instant>,
    ui: &ui::Ui,
    mut settings: ui::Settings,
    links: &mut Links,
    handed: Option<queqiao_local::LogonIdentity>,
    lines: &mut Vec<String>,
) -> Result<bool, String> {
    // Tests run two accounts on one PC: a throwaway device (own key, pins in memory) for the second. At the sign-in
    // screen the device and its login are the person's, handed over by the service (in memory only).
    let mut identity = match (&handed, &options.test_device) {
        (Some(handed), _) => DeviceIdentity::handed_over(
            &handed.device_id,
            &handed.device_name,
            &handed.key_pkcs8,
            handed.pins.clone(),
        ),
        (None, Some(label)) => DeviceIdentity::in_memory(label),
        (None, None) => DeviceIdentity::load_or_create(),
    }
    .map_err(|e| e.to_string())?;
    let mut login = match &handed {
        Some(handed) => SavedLogin {
            server: handed.server.clone(),
            pin: handed.pin.clone(),
            refresh_token: Some(handed.refresh_token.clone()),
        },
        None => SavedLogin::load(AccountRole::Agent).unwrap_or_default(),
    };
    if let Some(server) = &options.server {
        if login.server != *server {
            login.refresh_token = None;
        }
        login.server = server.clone();
    }
    if let Some(pin) = &options.pin {
        login.pin = pin.clone();
    }
    // The person's own agent (started at sign-in, by the installer or from the Start menu) stays in the tray when
    // it cannot go online: not logged in (the tray offers "Log in..."), or the server out of reach (it tries again
    // after 1 s, 2 s, ... 30 s, in the background). Started for a test or for the sign-in screen, it ends with the
    // error instead.
    let tolerant = handed.is_none()
        && (options.test_tray_login
            || (options.test_device.is_none()
                && options.server.is_none()
                && options.user.is_none()
                && options.password.is_none()
                && options.trust_new_key.is_none()));
    // A login is kept for this PC's own device only (not for a throwaway test device, nor the sign-in screen's).
    let keeps_login = handed.is_none() && options.test_device.is_none();
    let mut state = AccountState::Online;
    // No login kept (a fresh install: only the server Queqiao was built for) and none given: logged out, without
    // asking the server. It was asked before, and its "no saved login" counted as being offline, so a first start
    // showed an account that "cannot reach the server" with Log out (the user, 2026-11-27).
    let nothing_to_log_in_with = login.refresh_token.is_none() && options.password.is_none();
    let mut account = if options.test_tray_login
        || login.server.is_empty()
        || nothing_to_log_in_with
    {
        if !tolerant {
            return Err("not logged in: start once with --server <host:port> --pin <fingerprint> --user <name> --password <password>".into());
        }
        state = AccountState::LoggedOut;
        lines.push("not logged in: Log in... in the tray".into());
        None
    } else {
        let password = options.user.as_deref().zip(options.password.as_deref());
        match Account::connect(&mut login, &identity, AccountRole::Agent, password) {
            Ok(account) => Some(account),
            Err(error) if !tolerant => return Err(error.0),
            Err(error) => {
                state = if needs_new_login(&error.0) {
                    AccountState::LoggedOut
                } else {
                    AccountState::Offline
                };
                lines.push(format!("not online: {error}"));
                None
            }
        }
    };
    ui.set_account_state(state);

    // With the service: this device for its agent at the sign-in screen while the person lets the account's devices
    // in without asking (otherwise the service forgets it). Offered once online.
    let mut offer_logon = false;
    let (mut sessions, mut passed) = (0u32, true);
    let mut retry = Duration::from_secs(1);
    let mut next_try = Instant::now() + retry;
    // A connection attempt in the background while offline: (the result, the login as the attempt left it).
    type Attempt = std::thread::JoinHandle<(Result<Account, String>, SavedLogin)>;
    let mut attempt: Option<Attempt> = None;
    let mut deferred: Vec<PresenceEvent> = Vec::new();
    // This PC's network cards, so another device of the account can wake it later.
    let macs = wake::macs();
    // Help for someone of another account: the code while help requests are open (a fixed one keeps them open), and
    // whether the help window shows it (only when the person asked, or a test gave a code).
    let mut fixed_code: Option<String> = options
        .fixed_help_code
        .clone()
        .or_else(|| settings.fixed_help_code());
    let mut help_code: Option<String> = options.help_code.clone().or_else(|| fixed_code.clone());
    let mut help_window = options.help_code.is_some();
    // After five wrong codes the server closes help: a fixed code opens it again after a while (guessing stays slow).
    let mut fixed_reopen_at: Option<Instant> = None;
    // A helper the person allowed when it asked without a code (its next session request needs no prompt).
    // Only for HELPER_ALLOWED_FOR: an allowed ask does not stay open (the security review of 2026-11-22).
    let mut helper_allowed: Option<(String, Instant)> = None;
    let mut trust_new_key = options.trust_new_key.as_deref();
    if let Some(online) = account.as_mut() {
        went_online(
            ui,
            online,
            &login,
            &identity,
            keeps_login,
            trust_new_key.take(),
            &macs,
            help_code.as_deref(),
            lines,
        )?;
        offer_logon = keeps_login;
    }
    // Tests: the main window, each page saved as a picture a few seconds on (once help is open, with --help-code).
    let mut main_pictures_at = options.test_main_window.then(|| {
        ui.set_transfers(transfer_rows());
        ui.show_main(None);
        Instant::now() + Duration::from_secs(3)
    });
    if options.test_tray_login {
        ui.press_menu(queqiao_display::TEST_MENU_LOGIN);
    }
    // Updates, for the person's own agent of an installed Queqiao: put there by queqiao-setup, or started by the
    // Queqiao service (which updates its own copy).
    let updater = (tolerant
        && !options.test_tray_login
        && (update::installed_here() || links.service.is_some()))
    .then(|| update::Updater::start(lines, links.service.is_some()));
    loop {
        if end.is_some_and(|end| Instant::now() > end)
            || options.sessions.is_some_and(|n| sessions >= n)
        {
            break;
        }
        if main_pictures_at.is_some_and(|at| Instant::now() >= at) {
            main_pictures_at = None;
            if let Some(folder) = &options.ui_pictures {
                lines.push(match ui.save_main_pictures(folder) {
                    Ok(()) => "main window: pictures of its pages saved".into(),
                    Err(error) => format!("main window: pictures: {error}"),
                });
                lines.extend(ui.test_settings());
            }
        }
        // What the person did in the tray (also during the last session).
        deferred.extend(std::iter::from_fn(|| ui.try_event()));
        // What the service handed over by itself (Linux: the sign-in screen was used where no notice could be shown
        // there): told to the person now.
        if let Some(service) = links.service.as_mut() {
            for notice in service.notices() {
                ui.notify(&notice);
                lines.push(format!("notice from the Queqiao service: {notice}"));
            }
        }
        let mut quit = false;
        for event in deferred.drain(..) {
            match event {
                PresenceEvent::Quit => quit = true,
                PresenceEvent::SetAllow(on) => {
                    settings.allow_account_control = on;
                    let _ = settings.save();
                    offer_logon = handed.is_none() && options.test_device.is_none();
                    lines.push(format!(
                        "the person at this PC set: this account's devices may control it: {on}"
                    ));
                }
                PresenceEvent::SetAutostart(on) => match autostart::set(on) {
                    Ok(()) => lines.push(format!(
                        "the person at this PC set: start with Windows: {on}"
                    )),
                    Err(error) => {
                        lines.push(format!("start with Windows could not be set: {error}"))
                    }
                },
                PresenceEvent::SetLockAfter(on) => {
                    settings.lock_after_session = on;
                    let _ = settings.save();
                    lines.push(format!(
                        "the person at this PC set: lock this PC when a session ends: {on}"
                    ));
                }
                PresenceEvent::SetTerminal(on) => {
                    settings.allow_terminal = on;
                    let _ = settings.save();
                    lines.push(format!(
                        "the person at this PC set: allow remote command lines: {on}"
                    ));
                }
                PresenceEvent::SetFiles(on) => {
                    settings.receive_files = on;
                    let _ = settings.save();
                    lines.push(format!("the person at this PC set: receive files: {on}"));
                }
                PresenceEvent::SetClipboard(on) => {
                    settings.share_clipboard = on;
                    let _ = settings.save();
                    lines.push(format!(
                        "the person at this PC set: share the clipboard: {on}"
                    ));
                }
                PresenceEvent::SetAsk(on) => {
                    settings.ask_before_session = on;
                    let _ = settings.save();
                    offer_logon = handed.is_none() && options.test_device.is_none();
                    lines.push(format!(
                        "the person at this PC set: ask before each session: {on}"
                    ));
                }
                wanted @ (PresenceEvent::OpenHelp | PresenceEvent::HelpNewCode) => match account.as_mut() {
                    // The window shows the fixed code when there is one; New code: a one-time code instead.
                    Some(online) => {
                        let code = match (&fixed_code, matches!(wanted, PresenceEvent::OpenHelp)) {
                            (Some(fixed), true) => fixed.clone(),
                            _ => new_help_code(),
                        };
                        online.open_help(&code);
                        help_code = Some(code);
                        help_window = true;
                    }
                    None => {
                        lines.push("help: not online".into());
                        // Not on this thread: it keeps trying to go online meanwhile.
                        std::thread::spawn(|| {
                            queqiao_display::show_error(
                                queqiao_core::text::tr("Queqiao"),
                                queqiao_core::text::tr("This PC is not online, so nobody can be let in to help. Log in from the tray icon, or check the network."),
                            )
                        });
                    }
                },
                PresenceEvent::HelpClose => {
                    help_window = false;
                    match (account.as_mut(), &fixed_code) {
                        // Back to the fixed code, without the window.
                        (Some(online), Some(fixed)) => {
                            online.open_help(fixed);
                            help_code = Some(fixed.clone());
                            lines.push("help: the window closed; open again with the fixed code".into());
                        }
                        (online, _) => {
                            if let Some(online) = online {
                                online.close_help();
                            }
                            help_code = None;
                            lines.push("help: closed by the person at this PC".into());
                        }
                    }
                }
                PresenceEvent::SetFixedHelpCode(code) => {
                    match settings.set_fixed_help_code(code.as_deref()) {
                        Ok(()) => lines.push(format!(
                            "the person at this PC {} a fixed help code",
                            if code.is_some() { "set" } else { "removed" }
                        )),
                        Err(error) => lines.push(format!("the fixed help code was not kept: {error}")),
                    }
                    fixed_code = code;
                    ui.set_fixed_code(fixed_code.is_some());
                    if !help_window && let Some(online) = account.as_mut() {
                        match &fixed_code {
                            Some(fixed) => {
                                online.open_help(fixed);
                                help_code = Some(fixed.clone());
                            }
                            None => {
                                online.close_help();
                                help_code = None;
                            }
                        }
                    }
                }
                PresenceEvent::LogIn if account.is_none() && tolerant => {
                    if let Some((mut new, tried, renewed)) =
                        tray_log_in(options, &login, &identity, lines)
                    {
                        login = tried;
                        if let Some(renewed) = renewed {
                            identity = renewed;
                        }
                        went_online(ui, &mut new, &login, &identity, keeps_login, trust_new_key.take(), &macs, help_code.as_deref(), lines)?;
                        lines.push("logged in from the tray".into());
                        account = Some(new);
                        // An attempt with the old login still running is let go (its connection closes when it ends).
                        attempt = None;
                        retry = Duration::from_secs(1);
                        state = AccountState::Online;
                        ui.set_account_state(state);
                        offer_logon = keeps_login;
                    }
                }
                PresenceEvent::LogIn => {}
                PresenceEvent::CheckWakeOnLan => {
                    // powercfg takes a moment: not on this thread.
                    std::thread::spawn(|| {
                        let advice = wol_check::advice(&wol_check::check());
                        queqiao_display::show_info(queqiao_core::text::tr("Queqiao"), &advice);
                    });
                }
                PresenceEvent::CheckUpdates => match &updater {
                    Some(updater) => {
                        lines.push("updates: the person asked for a look".into());
                        updater.request();
                    }
                    None => notify(
                        false,
                        queqiao_core::text::tr(
                            "Updates come with an installed Queqiao (installed with queqiao-setup).",
                        )
                        .to_string(),
                    ),
                },
                PresenceEvent::SetAutoUpdate(on) => {
                    settings.auto_update = on;
                    let _ = settings.save();
                    lines.push(format!(
                        "the person at this PC set: install updates automatically: {on}"
                    ));
                }
                PresenceEvent::SettingsLocked => {
                    lines.push("a setting was chosen while someone of another account controls this PC: refused".into())
                }
                PresenceEvent::SetPassword(password) => {
                    settings.settings_password = password.as_deref().map(queqiao_account::hash_local_password).unwrap_or_default();
                    let _ = settings.save();
                    let hash = (!settings.settings_password.is_empty()).then(|| settings.settings_password.clone());
                    ui.set_password_hash(hash);
                    lines.push(format!(
                        "the person at this PC {} the settings password",
                        if password.is_some() { "set" } else { "removed" }
                    ));
                }
                PresenceEvent::Connect(device) => platform::open_viewer(&["--open", &device], lines),
                // The person said Take Control: the device controlling it now is disconnected.
                PresenceEvent::TakeOver(device) => {
                    platform::open_viewer(&["--open", &device, "--take-over"], lines)
                }
                PresenceEvent::EndControl(device) => match account.as_ref() {
                    Some(online) => match online.end_control(&device) {
                        Ok(()) => lines.push(format!("main window: the session on {device} ends")),
                        Err(error) => {
                            lines.push(format!("main window: end control: {error}"));
                            notify(true, error.0);
                        }
                    },
                    None => lines.push("main window: end control: not online".into()),
                },
                PresenceEvent::SendFiles(device) => {
                    platform::open_viewer(&["--open", &device, "--then", "files"], lines)
                }
                PresenceEvent::OpenCommandLine(device) => {
                    platform::open_viewer(&["--open", &device, "--then", "terminal"], lines)
                }
                PresenceEvent::HelpSomeone { help_id, code } => {
                    let link = queqiao_core::invitation::link(&help_id, &code);
                    platform::open_viewer(&["--help-connect", &link], lines);
                }
                PresenceEvent::Wake(device) => match account.as_ref() {
                    Some(online) => {
                        online.send(ClientMessage::Wake { device_id: device.clone() });
                        lines.push(format!("main window: wake {device}"));
                    }
                    None => lines.push("main window: wake: not online".into()),
                },
                PresenceEvent::RemoveDevice(device) => {
                    if let Some(online) = account.as_ref() {
                        online.remove_device(&device);
                        lines.push(format!("main window: {device} taken out of the account"));
                    }
                }
                PresenceEvent::LogOut => {
                    // Logged out only: the PC stays on the account's devices, shown offline (the user, 2026-11-27).
                    // Another account logs in on it with a new device (tray_log_in).
                    if let Some(online) = account.take() {
                        online.send(ClientMessage::Logout);
                        // A moment for it to go out before the connection closes.
                        std::thread::sleep(Duration::from_millis(300));
                    }
                    login.refresh_token = None;
                    let _ = login.save(AccountRole::Agent);
                    log_out_viewer(&identity);
                    // The service's agent for the sign-in screen was this device: no more.
                    if let Some(service) = links.service.as_mut() {
                        let _ = service.offer_logon(None);
                    }
                    state = AccountState::LoggedOut;
                    ui.set_account_state(state);
                    ui.set_user("");
                    ui.set_devices(Vec::new());
                    lines.push("main window: logged out (this PC's saved logins are gone)".into());
                }
                PresenceEvent::OpenReceivedFiles => {
                    platform::open_folder(&queqiao_files::downloads_folder());
                }
                PresenceEvent::ClearTransfers => {
                    queqiao_files::history::clear();
                    ui.set_transfers(Vec::new());
                }
                PresenceEvent::NewHelpId => match account.as_ref() {
                    Some(online) => {
                        online.refresh_help_id();
                        lines.push("help: a new help ID asked for".into());
                    }
                    None => lines.push("help: a new help ID needs the server".into()),
                },
                PresenceEvent::SetHelpMode(mode) => {
                    settings.set_help_mode(mode);
                    let _ = settings.save();
                    lines.push(format!("help mode: {}", settings.help_mode));
                }
                PresenceEvent::SetHelpFiles(on) => {
                    settings.help_files = on;
                    let _ = settings.save();
                    lines.push(format!("files while someone helps: {on}"));
                }
                PresenceEvent::MainShown => {
                    ui.set_transfers(transfer_rows());
                    ui.set_connections(connection_rows());
                    if let Some(online) = account.as_ref() {
                        ui.set_devices(main_devices(&online.devices, &online.device_id));
                    }
                }
                PresenceEvent::EndSession | PresenceEvent::Consent(_) => {}
            }
        }
        if quit {
            lines.push("quit from the tray icon".into());
            break;
        }
        // An update found: installed between sessions, by itself if the person lets it, or when they asked and agree.
        if let Some(found) = updater.as_ref().and_then(update::Updater::try_result) {
            match found {
                update::Found::UpToDate { by_hand } => {
                    lines.push(format!("updates: {} is the latest", queqiao_core::VERSION));
                    if by_hand {
                        notify(
                            false,
                            trf(
                                "Queqiao is up to date (version {version}).",
                                &[("version", queqiao_core::VERSION)],
                            ),
                        );
                    }
                }
                update::Found::Skipped { version, by_hand } => {
                    lines.push(format!(
                        "updates: {version} did not keep running here before: skipped"
                    ));
                    if by_hand {
                        notify(
                            false,
                            trf(
                                "Queqiao {version} did not start on this PC and was put back. Queqiao waits for a later version.",
                                &[("version", &version)],
                            ),
                        );
                    }
                }
                update::Found::Failed { error, by_hand } => {
                    lines.push(format!("updates: {error}"));
                    if by_hand {
                        notify(
                            true,
                            trf("The update check failed: {error}", &[("error", &error)]),
                        );
                    }
                }
                update::Found::Ready {
                    version,
                    notes,
                    installer,
                    manifest,
                    signature,
                    by_hand,
                } => {
                    lines.push(format!("updates: {version} downloaded and checked"));
                    let install = if by_hand {
                        queqiao_display::confirm(
                            platform::no_owner(),
                            queqiao_core::text::tr("Queqiao"),
                            &trf(
                                "Queqiao {version} is ready to install. Queqiao closes for a moment and starts again.\n\n{notes}\n\nInstall it now?",
                                &[("version", &version), ("notes", &notes)],
                            ),
                        )
                    } else {
                        settings.auto_update
                    };
                    if install {
                        match links.service.as_mut() {
                            // The service replaces its programs and starts this agent again.
                            Some(service) => {
                                if let Err(error) = update::apply_through_service(
                                    service, &installer, &version, &manifest, &signature, lines,
                                ) {
                                    lines.push(format!("updates: {error}"));
                                }
                            }
                            None => match update::apply(&installer, &version) {
                                Ok(()) => {
                                    lines.push(format!(
                                        "updates: installing {version}; this agent leaves"
                                    ));
                                    break;
                                }
                                Err(error) => lines
                                    .push(format!("updates: the installer did not start: {error}")),
                            },
                        }
                    }
                }
            }
        }
        // Offline or not logged in: the tray, and while offline an attempt in the background now and then.
        let Some(online) = account.as_mut() else {
            if let Some(worker) = attempt.take_if(|worker| worker.is_finished()) {
                let outcome = worker
                    .join()
                    .map_err(|_| "the connection attempt failed".to_string());
                match outcome {
                    Ok((Ok(mut new), tried)) => {
                        login = tried;
                        went_online(
                            ui,
                            &mut new,
                            &login,
                            &identity,
                            keeps_login,
                            trust_new_key.take(),
                            &macs,
                            help_code.as_deref(),
                            lines,
                        )?;
                        lines.push("reconnected".into());
                        account = Some(new);
                        retry = Duration::from_secs(1);
                        state = AccountState::Online;
                        ui.set_account_state(state);
                        offer_logon = keeps_login;
                    }
                    Ok((Err(error), _)) if needs_new_login(&error) => {
                        if !tolerant {
                            return Err(error);
                        }
                        lines.push(format!("not online: {error}"));
                        state = AccountState::LoggedOut;
                        ui.set_account_state(state);
                    }
                    Ok((Err(_), _)) | Err(_) => {
                        retry = (retry * 2).min(Duration::from_secs(30));
                        next_try = Instant::now() + retry;
                    }
                }
            } else if attempt.is_none()
                && state == AccountState::Offline
                && Instant::now() >= next_try
            {
                if !tolerant && end.is_some_and(|end| next_try + retry > end) {
                    return Err("the server connection was lost".into());
                }
                let (mut tried, identity) = (login.clone(), identity.clone());
                attempt = Some(std::thread::spawn(move || {
                    let result = Account::connect(&mut tried, &identity, AccountRole::Agent, None);
                    (result.map_err(|error| error.0), tried)
                }));
            }
            if let Some(event) = ui.wait_event(Duration::from_millis(100)) {
                deferred.push(event);
            }
            continue;
        };
        if fixed_reopen_at.is_some_and(|at| Instant::now() >= at)
            && let Some(fixed) = &fixed_code
        {
            fixed_reopen_at = None;
            online.open_help(fixed);
            help_code = Some(fixed.clone());
            lines.push("help: open again with the fixed code".into());
        }
        if offer_logon && let Some(service) = links.service.as_mut() {
            offer_logon = false;
            let allowed = settings.allow_account_control && !settings.ask_before_session;
            let offer = allowed
                .then(|| logon_identity(&identity, &login, online))
                .flatten();
            let handing = offer.is_some();
            match service.offer_logon(offer) {
                Ok(()) if handing => lines.push(
                    "the sign-in screen: this device handed to the Queqiao service for it".into(),
                ),
                Ok(()) => lines.push(
                    "the sign-in screen: not offered (control without asking is off here)".into(),
                ),
                Err(error) => lines.push(format!("the sign-in screen: {error}")),
            }
        }
        match online.next_event(Duration::from_millis(200)) {
            Some(AccountEvent::Signal {
                from,
                from_name,
                from_role: AccountRole::Viewer,
                session: id,
                kind,
                body,
                signature,
            }) if kind == "hello" => {
                // Only a device of this account whose key matches its pin, signing what it sends.
                if let Err(error) =
                    online.check_signal(&from, AccountRole::Viewer, &id, &kind, &body, &signature)
                {
                    lines.push(format!("refused a session request: {error}"));
                    continue;
                }
                // A device of another account, joined by this PC's help code.
                let helper = online.helper_account(&from).map(str::to_string);
                let shown_name = match &helper {
                    Some(user) => trf(
                        "{viewer} (account {user}) is helping on this PC",
                        &[("viewer", &from_name), ("user", user)],
                    ),
                    None => from_name.clone(),
                };
                let mut signal = online.session(&from, AccountRole::Viewer, &id);
                let hello = match check_hello(&body, "agent") {
                    Ok(hello) => hello,
                    Err(error) => {
                        let _ = signal.send("error", &error.0);
                        lines.push(format!("refused viewer {from_name}: {error}"));
                        continue;
                    }
                };
                if helper.is_none() && !settings.allow_account_control {
                    let _ = signal.send(
                        "error",
                        "control by this account's devices is turned off on that PC",
                    );
                    lines.push(format!(
                        "refused viewer {from_name}: control by this account's devices is off"
                    ));
                    continue;
                }
                // Someone of another account is asked about, unless this PC lets the help code alone in (as UU) or the
                // person allowed their request a moment ago.
                let asked_already = helper.is_some()
                    && helper_allowed
                        .as_ref()
                        .is_some_and(|(id, at)| *id == from && at.elapsed() < HELPER_ALLOWED_FOR);
                let ask_helper = helper.is_some()
                    && help_mode(options, &settings) != queqiao_display::HelpMode::CodeOnly;
                if asked_already {
                    helper_allowed = None;
                } else if ask_helper || options.ask || settings.ask_before_session {
                    let question = match &helper {
                        Some(user) => trf(
                            "{viewer} (account {user}) wants to control this PC to help you.",
                            &[
                                ("viewer", &clean_name(&from_name)),
                                ("user", &clean_name(user)),
                            ],
                        ),
                        None => trf(
                            "{viewer}, a device of your account, wants to control this PC.",
                            &[("viewer", &clean_name(&from_name))],
                        ),
                    };
                    match ask_consent(ui, &question, options.answer) {
                        Ok(true) => {
                            lines.push(format!("the person at this PC allowed {from_name}"))
                        }
                        Ok(false) => {
                            let _ = signal.send("error", "the person at that PC declined");
                            lines.push(format!("the person at this PC declined {from_name}"));
                            continue;
                        }
                        Err(error) => {
                            let _ = signal
                                .send("error", "the consent prompt could not be shown on that PC");
                            lines.push(format!(
                                "refused viewer {from_name}: consent prompt: {error}"
                            ));
                            continue;
                        }
                    }
                }
                signal
                    .send("hello", &hello_body("agent"))
                    .map_err(|e| e.to_string())?;
                sessions += 1;
                lines.push(format!("session {sessions}: viewer {from_name} ({from})"));
                if let Some(user) = &helper {
                    lines.push(format!(
                        "helper session: {from_name} of account {user}; file transfer, command lines, the privacy screen and restarting are off"
                    ));
                    // The helper holds the mouse: this PC's settings are locked until the session ends.
                    ui.lock_settings(true);
                    if options.test_press_setting {
                        ui.press_menu(queqiao_display::TEST_MENU_TERMINAL);
                    }
                }
                let started_at = std::time::SystemTime::now();
                let outcome = session(
                    options,
                    &mut signal,
                    &shown_name,
                    &from,
                    end,
                    ui,
                    Permissions {
                        clipboard: settings.share_clipboard,
                        files: settings.receive_files && (helper.is_none() || settings.help_files),
                        terminal: settings.allow_terminal && helper.is_none(),
                        helper: helper.is_some(),
                        fec: hello.fec,
                        controller: match &helper {
                            Some(user) => format!("{from_name} ({user})"),
                            None => from_name.clone(),
                        },
                    },
                    links,
                    &mut deferred,
                    lines,
                );
                // However it ended (a failure before it was under way included): nobody controls this PC now.
                drop(signal);
                online.set_control_state(None);
                match outcome {
                    Ok(outcome) => {
                        passed &= outcome.passed;
                        ui::record_session(&from_name, &from, started_at, &outcome.ended);
                        lock_after(options, &settings, outcome.lock_after, lines);
                        ui.lock_settings(false);
                    }
                    Err(error) => {
                        ui.lock_settings(false);
                        lock_after(options, &settings, false, lines);
                        lines.push(format!("session {sessions} failed: {error}"));
                        ui::record_session(
                            &from_name,
                            &from,
                            started_at,
                            &format!("failed: {error}"),
                        );
                        passed = false;
                    }
                }
            }
            Some(AccountEvent::Closed(reason)) => {
                lines.push(format!("server connection lost: {reason}"));
                if !tolerant && end.is_some_and(|end| Instant::now() + retry > end) {
                    return Err("the server connection was lost".into());
                }
                // Tried again in the background (above), the tray responding meanwhile.
                account = None;
                retry = Duration::from_secs(1);
                next_try = Instant::now() + retry;
                state = AccountState::Offline;
                ui.set_account_state(state);
            }
            Some(AccountEvent::Error(message)) => lines.push(format!("server: {message}")),
            Some(AccountEvent::Notice(message)) => {
                lines.push(format!("notice: {message}"));
                ui.notify(&message);
            }
            // A device joined the account, or one came back with a new key: the person is told (if it was not them,
            // they change the password and take the device out).
            Some(AccountEvent::DeviceJoined(name)) => {
                let text = trf(
                    "A new device, {name}, logged in to your account. If that was not you, change your password and take it out of the account.",
                    &[("name", &clean_name(&name))],
                );
                lines.push(format!("notification: a new device, {name}"));
                ui.notify(&text);
            }
            Some(AccountEvent::DeviceKeyChanged(name)) => {
                let text = trf(
                    "{name} logged in again with a new device key. If that was not you, change your password and take it out of the account.",
                    &[("name", &clean_name(&name))],
                );
                lines.push(format!("notification: {name} has a new key"));
                ui.notify(&text);
            }
            Some(AccountEvent::HelpOpen(help_id)) => {
                lines.push(format!("help: open, help ID {help_id}"));
                if let Some(path) = &options.help_id_file {
                    let _ = std::fs::write(path, &help_id);
                }
                if help_window && let Some(code) = &help_code {
                    ui.show_help(Some((&help_id, code)));
                }
            }
            Some(AccountEvent::WakeRequest { name, macs }) => match wake::send(&macs) {
                Ok(sent) => lines.push(format!("wake: sent {sent} wake-up packets for {name}")),
                Err(error) => lines.push(format!("wake: could not wake {name}: {error}")),
            },
            Some(AccountEvent::HelpAsked {
                request,
                device,
                user,
            }) => {
                // A request without a code: only while this PC takes help requests, only when it lets them in without a
                // code (as UU's "no code, this PC agrees"), and the person decides.
                let takes_asks =
                    help_mode(options, &settings) == queqiao_display::HelpMode::AskOnly;
                let allowed = help_code.is_some() && takes_asks && {
                    let question = trf(
                        "{viewer} (account {user}) asks to help you on this PC. Allow it?",
                        &[
                            ("viewer", &clean_name(&device.name)),
                            ("user", &clean_name(&user)),
                        ],
                    );
                    matches!(ask_consent(ui, &question, options.answer), Ok(true))
                };
                if help_code.is_some() && !takes_asks {
                    lines.push("help: this PC takes help requests with the help code only".into());
                }
                online.answer_help(request, allowed);
                lines.push(format!(
                    "help: {} of account {user} asked without a code: {}",
                    device.name,
                    if allowed { "allowed" } else { "declined" }
                ));
                if allowed {
                    lines.push(format!("the person at this PC allowed {}", device.name));
                    // Its session request that follows (soon) is not asked about again.
                    helper_allowed = Some((device.id.clone(), Instant::now()));
                }
            }
            Some(AccountEvent::HelpClosed(reason)) => {
                lines.push(format!("help: closed: {reason}"));
                help_code = None;
                helper_allowed = None;
                help_window = false;
                ui.show_help(None);
                if fixed_code.is_some() {
                    fixed_reopen_at = Some(Instant::now() + FIXED_CODE_PAUSE);
                    lines.push(format!(
                        "help: the fixed code takes requests again in {} minutes",
                        FIXED_CODE_PAUSE.as_secs() / 60
                    ));
                }
            }
            Some(AccountEvent::HelpRequested { device, user }) => {
                lines.push(format!(
                    "help: {} of account {user} was let in (code given or ask allowed)",
                    device.name
                ));
                // The code is used up: the fixed one again, or a new one while the window is open.
                if help_code.is_some() {
                    let code = match &fixed_code {
                        Some(fixed) if !help_window => fixed.clone(),
                        _ => new_help_code(),
                    };
                    online.open_help(&code);
                    if fixed_code.as_ref() == Some(&code) {
                        lines.push("help: open again with the fixed code".into());
                    }
                    help_code = Some(code);
                }
            }
            Some(AccountEvent::Devices(devices)) => {
                ui.set_devices(main_devices(&devices, &online.device_id));
            }
            Some(
                AccountEvent::Signal { .. }
                | AccountEvent::IceServers { .. }
                | AccountEvent::PasswordChanged(_)
                | AccountEvent::HelpGranted(_),
            )
            | None => {}
            // No session runs (one that does takes it itself): nothing to end.
            Some(AccountEvent::EndControl(by)) => {
                lines.push(format!("{by} asked to end the session here: none runs"));
            }
        }
    }
    lines.push(format!("{sessions} sessions"));
    Ok(passed && options.sessions.is_none_or(|n| sessions >= n))
}

/// A message for the person at this PC, on a thread of its own (the agent goes on meanwhile).
fn notify(error: bool, text: String) {
    std::thread::spawn(move || {
        let title = queqiao_core::text::tr("Queqiao");
        if error {
            queqiao_display::show_error(title, &text);
        } else {
            queqiao_display::show_info(title, &text);
        }
    });
}

/// Whether the server wants the password again (the saved login expired, was revoked, or the password changed), or
/// there is no saved login at all.
fn needs_new_login(error: &str) -> bool {
    error.contains("log in again") || error.contains("no saved login")
}

/// Just online: the login kept (for this PC's own device), a new key pinned if asked, this PC's network cards sent
/// (so another device of the account can wake it), and help requests open again if they were.
#[allow(clippy::too_many_arguments)]
fn went_online(
    ui: &ui::Ui,
    account: &mut Account,
    login: &SavedLogin,
    identity: &DeviceIdentity,
    keep_login: bool,
    trust_new_key: Option<&str>,
    macs: &[String],
    help_code: Option<&str>,
    lines: &mut Vec<String>,
) -> Result<(), String> {
    if keep_login {
        login.save(AccountRole::Agent).map_err(|e| e.to_string())?;
    }
    if let Some(device) = trust_new_key {
        account.trust_new_key(device).map_err(|e| e.to_string())?;
        lines.push(format!("the key of device {device} will be pinned anew"));
    }
    // The main window's account line and device list.
    ui.set_user(&account.user);
    ui.set_devices(main_devices(&account.devices, &account.device_id));
    lines.push(format!(
        "logged in as {} at {} (route: {}); this device: {} ({}); {} devices on the account",
        account.user,
        login.server,
        account.route(),
        identity.name,
        identity.id,
        account.devices.len()
    ));
    if !macs.is_empty() {
        account.send(ClientMessage::DeviceInfo {
            macs: macs.to_vec(),
        });
    }
    if let Some(code) = help_code {
        account.open_help(code);
    }
    Ok(())
}

/// "Log in..." in the tray: the login form (logging in, or the sign-up page), the login itself in the background so
/// the form keeps responding. The account, the login as it now is, and this PC's new device when its earlier one is
/// another account's (DeviceIdentity::renew); None when the person closed the form.
fn tray_log_in(
    options: &Options,
    login: &SavedLogin,
    identity: &DeviceIdentity,
    lines: &mut Vec<String>,
) -> Option<(Account, SavedLogin, Option<DeviceIdentity>)> {
    let defaults = LoginFields {
        server: login.server.clone(),
        pin: login.pin.clone(),
        user: String::new(),
        password: String::new(),
    };
    let mut form = match LoginForm::new(
        queqiao_core::text::tr("Queqiao - log in this PC"),
        &defaults,
    ) {
        Ok(form) => form,
        Err(error) => {
            lines.push(format!("login form: {}", error.message()));
            return None;
        }
    };
    if options.test_tray_login {
        form.set_fields(&LoginFields {
            server: options.server.clone().unwrap_or_default(),
            pin: options.pin.clone().unwrap_or_default(),
            user: options.user.clone().unwrap_or_default(),
            password: options.password.clone().unwrap_or_default(),
        });
        let drawn = Instant::now() + Duration::from_millis(500);
        while Instant::now() < drawn {
            let _ = form.next(20);
        }
        if let Some(folder) = &options.ui_pictures {
            let _ = form.save_picture(&folder.join("agent_login.bmp"));
        }
        form.submit();
    }
    loop {
        match form.next(15) {
            Some(LoginAction::Cancel) => {
                lines.push("login: the form was closed".into());
                return None;
            }
            Some(action @ (LoginAction::Submit(_) | LoginAction::SignUp(_))) => {
                // Two pages (the user, 2026-11-27): "Log in" logs in only; "Create account" makes one.
                let (fields, sign_up) = match action {
                    LoginAction::SignUp(fields) => (fields, true),
                    LoginAction::Submit(fields) => (fields, false),
                    LoginAction::Cancel => continue,
                };
                if let Some(missing) = queqiao_display::missing_login_field(&fields) {
                    form.set_status(missing);
                    continue;
                }
                let mut tried = login.clone();
                if tried.server != fields.server {
                    tried.refresh_token = None;
                }
                tried.server = fields.server.clone();
                tried.pin = fields.pin.clone();
                form.set_busy(
                    true,
                    queqiao_core::text::tr(if sign_up {
                        "Creating the account..."
                    } else {
                        "Logging in..."
                    }),
                );
                let mut identity = identity.clone();
                let worker = std::thread::spawn(move || {
                    let attempt = |tried: &mut SavedLogin, identity: &DeviceIdentity| {
                        if sign_up {
                            Account::register(
                                tried,
                                identity,
                                AccountRole::Agent,
                                &fields.user,
                                &fields.password,
                            )
                        } else {
                            Account::connect(
                                tried,
                                identity,
                                AccountRole::Agent,
                                Some((&fields.user, &fields.password)),
                            )
                        }
                    };
                    let mut result = attempt(&mut tried, &identity);
                    // This PC's device is another account's (logged out of it, it stays there, offline): a new
                    // device for this account.
                    let mut renewed = None;
                    if let Err(error) = &result
                        && queqiao_account::belongs_elsewhere(&error.0)
                        && let Ok(new) = identity.renew()
                    {
                        identity = new.clone();
                        renewed = Some(new);
                        result = attempt(&mut tried, &identity);
                    }
                    // One login for both: the viewer (Connect in the main window) gets its saved login as well.
                    if result.is_ok() {
                        let mut viewer = tried.clone();
                        viewer.refresh_token = None;
                        if Account::connect(
                            &mut viewer,
                            &identity,
                            AccountRole::Viewer,
                            Some((&fields.user, &fields.password)),
                        )
                        .is_ok()
                        {
                            let _ = viewer.save(AccountRole::Viewer);
                        }
                    }
                    (result.map_err(|error| error.0), tried, renewed)
                });
                while !worker.is_finished() {
                    if let Some(LoginAction::Cancel) = form.next(15) {
                        lines.push("login: the form was closed".into());
                        return None;
                    }
                }
                match worker.join() {
                    Ok((Ok(account), tried, renewed)) => {
                        if sign_up {
                            lines.push(format!("login: the account {} was made", account.user));
                        }
                        if let Some(renewed) = &renewed {
                            lines.push(format!(
                                "login: this PC's earlier device is another account's: a new device {}",
                                renewed.id
                            ));
                        }
                        return Some((account, tried, renewed));
                    }
                    Ok((Err(error), _, _)) => {
                        lines.push(format!("login: {error}"));
                        if options.test_tray_login {
                            return None;
                        }
                        form.set_busy(false, &error);
                    }
                    Err(_) => return None,
                }
            }
            None => {}
        }
    }
}

/// Asks the person at the PC whether `viewer` may control it; false when denied or unanswered.
fn ask_consent(ui: &ui::Ui, question: &str, answer: Option<bool>) -> Result<bool, String> {
    ui.ask(question, CONSENT_SECONDS)?;
    if let Some(answer) = answer {
        std::thread::sleep(Duration::from_secs(1));
        ui.press_consent(answer);
    }
    let deadline = Instant::now() + Duration::from_secs(u64::from(CONSENT_SECONDS) + 2);
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Ok(false);
        }
        if let Some(PresenceEvent::Consent(allowed)) = ui.wait_event(left) {
            return Ok(allowed);
        }
    }
}

/// Locks the PC after a session when the person chose so, the viewer asked for it (`asked`), or a test did.
fn lock_after(options: &Options, settings: &ui::Settings, asked: bool, lines: &mut Vec<String>) {
    if !(settings.lock_after_session || options.lock_after_session || asked) {
        return;
    }
    if options.dry_run_commands {
        lines.push("after the session: the PC would be locked now (test run)".into());
        return;
    }
    match power::lock() {
        Ok(()) => lines.push("after the session: the PC is locked".into()),
        Err(error) => lines.push(format!("after the session: locking failed: {error}")),
    }
}

/// A help code: 8 of 32 letters and digits without look-alikes (no 0, O, 1 or I), 40 bits.
/// How long help stays closed after five wrong codes before a fixed code takes requests again.
const FIXED_CODE_PAUSE: Duration = Duration::from_secs(15 * 60);

fn new_help_code() -> String {
    const ALPHABET: &[u8; 32] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    let random = queqiao_account::new_session_id();
    random
        .as_bytes()
        .chunks(2)
        .filter_map(|pair| u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok())
        .map(|byte| char::from(ALPHABET[usize::from(byte % 32)]))
        .collect()
}

/// This device as the service's agent at the sign-in screen is to be (None without a key or a saved login).
fn logon_identity(
    identity: &DeviceIdentity,
    login: &SavedLogin,
    account: &Account,
) -> Option<queqiao_local::LogonIdentity> {
    Some(queqiao_local::LogonIdentity {
        device_id: identity.id.clone(),
        device_name: identity.name.clone(),
        key_pkcs8: identity.key.as_ref()?.pkcs8().to_vec(),
        server: login.server.clone(),
        pin: login.pin.clone(),
        refresh_token: login.refresh_token.clone()?,
        allow_account_control: true,
        pins: account.pins(),
        owner_sid: String::new(),
        owner_account: String::new(),
    })
}

/// The account's devices as the main window lists them: this PC first, then those online, by name.
fn main_devices(
    devices: &[queqiao_core::account::Device],
    this: &str,
) -> Vec<queqiao_display::MainDevice> {
    let mut rows: Vec<queqiao_display::MainDevice> = devices
        .iter()
        .map(|device| queqiao_display::MainDevice {
            id: device.id.clone(),
            name: device.name.clone(),
            online: device.controllable || device.online,
            this_pc: device.id == this,
            controlled_by: device.controlled_by.clone(),
        })
        .collect();
    rows.sort_by_key(|row| (!row.this_pc, !row.online, row.name.to_lowercase()));
    rows
}

/// The sessions this PC started (queqiao_account::connections), as the main window lists them: "Today 15:08" for
/// today, the date otherwise; "12 min", "1 h 05 min".
fn connection_rows() -> Vec<queqiao_display::ConnectionRow> {
    let now = queqiao_account::connections::local_now();
    let today = now.get(..10).unwrap_or_default().to_string();
    queqiao_account::connections::load()
        .into_iter()
        .map(|connection| {
            let when = match connection.when.split_once(' ') {
                Some((date, time)) if date == today => trf("Today {time}", &[("time", time)]),
                _ => connection.when.clone(),
            };
            let minutes = connection.seconds.div_ceil(60);
            let length = if connection.seconds < 60 {
                tr("under a minute").to_string()
            } else if minutes < 60 {
                trf("{minutes} min", &[("minutes", &minutes.to_string())])
            } else {
                trf(
                    "{hours} h {minutes} min",
                    &[
                        ("hours", &(minutes / 60).to_string()),
                        ("minutes", &format!("{:02}", minutes % 60)),
                    ],
                )
            };
            queqiao_display::ConnectionRow {
                device_id: connection.device_id,
                when,
                length,
            }
        })
        .collect()
}

/// The files sent and received (queqiao_files::history), as the main window lists them.
fn transfer_rows() -> Vec<queqiao_display::TransferRow> {
    queqiao_files::history::load()
        .into_iter()
        .map(|entry| {
            let pattern = if entry.received {
                "from {peer} · {size} · {when}"
            } else {
                "to {peer} · {size} · {when}"
            };
            let size = queqiao_files::history::size_text(entry.size);
            queqiao_display::TransferRow {
                received: entry.received,
                detail: trf(
                    pattern,
                    &[
                        ("peer", &entry.peer),
                        ("size", &size),
                        ("when", &entry.when),
                    ],
                ),
                name: entry.name,
            }
        })
        .collect()
}

/// The viewer logged out too: its saved login revoked on the server (as far as it can be reached) and gone here.
fn log_out_viewer(identity: &DeviceIdentity) {
    let Some(mut saved) = SavedLogin::load(AccountRole::Viewer) else {
        return;
    };
    if saved.refresh_token.is_some() {
        let mut revoking = saved.clone();
        let identity = identity.clone();
        std::thread::spawn(move || {
            if let Ok(account) =
                Account::connect(&mut revoking, &identity, AccountRole::Viewer, None)
            {
                account.send(ClientMessage::Logout);
                std::thread::sleep(Duration::from_millis(300));
            }
        });
    }
    saved.refresh_token = None;
    let _ = saved.save(AccountRole::Viewer);
}

/// How someone of another account may start helping: the setting, or for tests --help-mode.
fn help_mode(options: &Options, settings: &ui::Settings) -> queqiao_display::HelpMode {
    options.help_mode.unwrap_or_else(|| settings.help_mode())
}

/// How long an allowed request without a code lets its session in without a second prompt.
const HELPER_ALLOWED_FOR: Duration = Duration::from_secs(60);
